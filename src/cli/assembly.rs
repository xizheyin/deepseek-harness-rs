use std::{path::Path, sync::Arc};

use thiserror::Error;

use crate::{
    agent::{
        AgentLoop, AgentLoopConfig, FileChangePolicy, NoApprovalProvider, PluginPolicy,
        ShellPolicy, ToolExecutor,
    },
    entropy::EntropySource,
    goal::GoalRuntime,
    model::LlmCallConfig,
    plan_mode::PlanModeRuntime,
    provider::{
        ModelProvider,
        deepseek::{DEEPSEEK_PROVIDER, DeepSeekConfig, DeepSeekProvider},
    },
    session::{CommittedUiReceiver, Session, SessionStore, StoreError, SystemClock},
    tools::{LocalToolRegistry, PluginConfig, WorkspaceFileCatalogue},
    user_question::{UserQuestionBroker, UserQuestionReceiver},
    workspace_authority::WorkspaceAuthority,
    workspace_instructions::prepare_workspace_instructions,
};
use tokio_util::sync::CancellationToken;

use super::{
    approval::{ApprovalChallengePool, ApprovalEnvelopeReceiver, TerminalApprovalProvider},
    approval_join::ApprovalJoin,
    args::{ApprovalMode, DEFAULT_MODEL},
    identity::new_session_id,
};

const SYSTEM_PROMPT: &str = "You are dsh, a coding agent working only through the supplied workspace tools. Use tools when they are useful. Never claim a file change or command completed unless its correlated tool result says it completed. When a Goal exists, use get_goal and settle it truthfully with update_goal; leave it active while useful work remains. This session has no sandbox, Skills, Hooks, or background-task feature.";
const PLAN_MODE_POLICY: &str = "You are in Plan Mode. Explore and inspect the project, then produce a complete implementation plan before making changes. Do not modify files or run commands with side effects while planning. When the plan is ready, call exit_plan_mode with the complete markdown plan beginning with a # heading. Plan Mode is guidance, not a sandbox; all existing approval and safety rules still apply.";

pub(super) enum AgentAssembly {
    Script(AgentLoop),
    Interactive(InteractiveAssembly),
}

pub(super) struct InteractiveAssembly {
    pub(super) agent: AgentLoop,
    pub(super) events: CommittedUiReceiver,
    pub(super) approvals: ApprovalEnvelopeReceiver,
    pub(super) user_questions: UserQuestionReceiver,
    pub(super) joins: ApprovalJoin,
    pub(super) session_id: String,
    pub(super) resumed: bool,
    pub(super) file_suggestions: WorkspaceFileCatalogue,
    pub(super) goal: GoalRuntime,
    pub(super) plan_mode: PlanModeRuntime,
}

pub(super) struct AssemblySession {
    session: Session,
    authority: WorkspaceAuthority,
    resumed: bool,
}

impl AssemblySession {
    pub(super) fn resumed(session: Session, authority: WorkspaceAuthority) -> Self {
        Self {
            session,
            authority,
            resumed: true,
        }
    }
}

pub(super) struct AssemblyFailure {
    error: AssemblyError,
    session: Session,
}

impl AssemblyFailure {
    fn new(error: AssemblyError, session: Session) -> Self {
        Self { error, session }
    }

    pub(super) fn into_parts(self) -> (AssemblyError, Session) {
        (self.error, self.session)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(super) enum AssemblyError {
    #[error("CLI_WORKSPACE_UNAVAILABLE")]
    Workspace,
    #[error("CLI_PROVIDER_UNAVAILABLE")]
    Provider,
    #[error("CLI_ENTROPY_UNAVAILABLE")]
    Entropy,
    #[error("CLI_AGENT_UNAVAILABLE")]
    Agent,
    #[error("CLI_PLUGIN_UNAVAILABLE")]
    Plugin { plugin_id: Option<String> },
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub(super) fn prepare_new_session(workspace: &Path) -> Result<AssemblySession, AssemblyError> {
    // This synchronous open happens once, outside a Tokio worker. The exact
    // retained capability is shared by the durable header and all local tools.
    let authority = WorkspaceAuthority::open(workspace).map_err(|_| AssemblyError::Workspace)?;
    if authority.canonical_path().to_str().is_none() {
        return Err(AssemblyError::Workspace);
    }
    let session_id = new_session_id(EntropySource::system()).map_err(|_| AssemblyError::Entropy)?;
    let store = SessionStore::open_default()?;
    let session = store.prepare_new(session_id, &authority, SystemClock)?;
    Ok(AssemblySession {
        session,
        authority,
        resumed: false,
    })
}

pub(super) async fn assemble_session(
    prepared: AssemblySession,
    requested_model: Option<String>,
    interactive: bool,
    approval_mode: ApprovalMode,
    plugin_config: Option<PluginConfig>,
    cancellation: CancellationToken,
) -> Result<AgentAssembly, AssemblyFailure> {
    let AssemblySession {
        mut session,
        authority,
        resumed,
    } = prepared;
    let session_id = session.header().id().as_str().to_owned();
    let call = select_call(session.request_header(), requested_model);
    let call = match call {
        Ok(call) => call,
        Err(_) => return Err(AssemblyFailure::new(AssemblyError::Agent, session)),
    };

    // Attach a resumed observer at the post-marker sequence, and complete the
    // approval join's only fallible allocation, before consulting tool process
    // state or Provider credentials. Any error still returns the raw Session
    // so the CLI can explicitly finish its writer.
    let interactive_state = if interactive {
        let events = match session.attach_ui_observer() {
            Ok(events) => events,
            Err(_) => return Err(AssemblyFailure::new(AssemblyError::Agent, session)),
        };
        let challenges = match ApprovalChallengePool::from_entropy(EntropySource::system()) {
            Ok(challenges) => challenges,
            Err(_) => return Err(AssemblyFailure::new(AssemblyError::Entropy, session)),
        };
        let joins = match ApprovalJoin::new(challenges) {
            Ok(joins) => joins,
            Err(_) => return Err(AssemblyFailure::new(AssemblyError::Agent, session)),
        };
        Some((events, joins))
    } else {
        None
    };

    let provider_config = match DeepSeekConfig::from_process_environment() {
        Ok(config) => config,
        Err(_) => return Err(AssemblyFailure::new(AssemblyError::Provider, session)),
    };
    let provider = match DeepSeekProvider::from_environment(provider_config) {
        Ok(provider) => Arc::new(provider),
        Err(_) => return Err(AssemblyFailure::new(AssemblyError::Provider, session)),
    };

    // The suggestion scanner and local tools share the same retained
    // workspace capability. No later UI path reopens the ambient pathname.
    let file_suggestions =
        interactive.then(|| WorkspaceFileCatalogue::from_authority(authority.clone()));
    let goal = interactive.then(|| GoalRuntime::from_replay(session.state().goal_replay()));
    let plan_mode = PlanModeRuntime::new(session.state().plan_mode_active());
    let workspace_context =
        match prepare_workspace_instructions(&session, &authority, &cancellation).await {
            Ok(context) => context,
            Err(_) => return Err(AssemblyFailure::new(AssemblyError::Agent, session)),
        };
    let (user_questions, question_receiver) = if interactive {
        let (broker, receiver) = UserQuestionBroker::new();
        (Some(broker), Some(receiver))
    } else {
        (None, None)
    };

    let registry = match plugin_config {
        Some(plugin_config) => {
            LocalToolRegistry::from_authority_with_plugins(
                authority,
                plugin_config,
                cancellation,
                goal.clone(),
                Some(plan_mode.clone()),
                user_questions.clone(),
            )
            .await
        }
        None => LocalToolRegistry::from_authority_with_interaction(
            authority,
            goal.clone(),
            Some(plan_mode.clone()),
            user_questions,
        ),
    };
    let registry = match registry {
        Ok(registry) => Arc::new(registry),
        Err(error) => {
            let error = match error {
                crate::tools::ToolRegistryBuildError::InvalidWorkspace { .. } => {
                    AssemblyError::Workspace
                }
                crate::tools::ToolRegistryBuildError::PluginStartup { plugin_id } => {
                    AssemblyError::Plugin {
                        plugin_id: Some(plugin_id),
                    }
                }
                crate::tools::ToolRegistryBuildError::Plugin => {
                    AssemblyError::Plugin { plugin_id: None }
                }
                _ => AssemblyError::Agent,
            };
            return Err(AssemblyFailure::new(error, session));
        }
    };
    let schemas = registry.schemas().to_vec();

    let config = match AgentLoopConfig::new(call)
        .with_system(SYSTEM_PROMPT)
        .and_then(|config| config.with_tools(schemas))
        .and_then(|config| config.with_plan_mode(plan_mode.clone(), PLAN_MODE_POLICY))
    {
        Ok(config) => config,
        Err(_) => {
            let _ = registry.shutdown().await;
            return Err(AssemblyFailure::new(AssemblyError::Agent, session));
        }
    };
    let tools: Arc<dyn ToolExecutor> = registry.clone();
    let provider: Arc<dyn ModelProvider> = provider;

    if interactive {
        let Some((events, joins)) = interactive_state else {
            return Err(AssemblyFailure::new(AssemblyError::Agent, session));
        };
        let Some(file_suggestions) = file_suggestions else {
            return Err(AssemblyFailure::new(AssemblyError::Agent, session));
        };
        let Some(goal) = goal else {
            return Err(AssemblyFailure::new(AssemblyError::Agent, session));
        };
        let Some(user_questions) = question_receiver else {
            return Err(AssemblyFailure::new(AssemblyError::Agent, session));
        };
        let (approval, approvals) = TerminalApprovalProvider::new();
        let (file_change_policy, shell_policy, plugin_policy) = interactive_policies(approval_mode);
        let config = config
            .with_approval_provider(Arc::new(approval))
            .with_file_change_policy(file_change_policy)
            .with_shell_policy(shell_policy)
            .with_plugin_policy(plugin_policy);
        let mut agent = match AgentLoop::new_preserving_session(session, provider, tools, config) {
            Ok(agent) => agent,
            Err((_error, session)) => {
                let _ = registry.shutdown().await;
                return Err(AssemblyFailure::new(AssemblyError::Agent, session));
            }
        };
        agent.install_workspace_context(workspace_context);
        Ok(AgentAssembly::Interactive(InteractiveAssembly {
            agent,
            events,
            approvals,
            user_questions,
            joins,
            session_id,
            resumed,
            file_suggestions,
            goal,
            plan_mode,
        }))
    } else {
        let config = config
            .with_approval_provider(Arc::new(NoApprovalProvider))
            .with_file_change_policy(FileChangePolicy::Deny)
            .with_shell_policy(ShellPolicy::Deny)
            .with_plugin_policy(PluginPolicy::Deny);
        match AgentLoop::new_preserving_session(session, provider, tools, config) {
            Ok(mut agent) => {
                agent.install_workspace_context(workspace_context);
                Ok(AgentAssembly::Script(agent))
            }
            Err((_error, session)) => {
                let _ = registry.shutdown().await;
                Err(AssemblyFailure::new(AssemblyError::Agent, session))
            }
        }
    }
}

const fn interactive_policies(
    approval_mode: ApprovalMode,
) -> (FileChangePolicy, ShellPolicy, PluginPolicy) {
    let file = match approval_mode {
        ApprovalMode::Ask => FileChangePolicy::Ask,
        ApprovalMode::AutoEdit => FileChangePolicy::Allow,
    };
    (file, ShellPolicy::Ask, PluginPolicy::Ask)
}

fn select_call(
    previous: Option<&crate::session::EpochHeader>,
    requested_model: Option<String>,
) -> Result<LlmCallConfig, crate::model::ModelError> {
    let model = requested_model
        .as_deref()
        .or_else(|| previous.map(|header| header.config.model()))
        .unwrap_or(DEFAULT_MODEL);
    // Provider defaults and adapter-owned values are resolved afresh. Resume
    // reuses only the stored model name; it never freezes an old materialized
    // default or routes a hand-written journal to a non-DeepSeek Provider.
    LlmCallConfig::new(DEEPSEEK_PROVIDER, model)
}

#[cfg(test)]
mod tests {
    use crate::{
        agent::{FileChangePolicy, PluginPolicy, ShellPolicy},
        model::LlmCallConfig,
        session::EpochHeader,
    };

    use super::{
        ApprovalMode, DEEPSEEK_PROVIDER, DEFAULT_MODEL, SYSTEM_PROMPT, interactive_policies,
        select_call,
    };

    #[test]
    fn system_prompt_does_not_claim_later_phase_features() {
        assert!(!SYSTEM_PROMPT.contains("no persistence"));
        assert!(SYSTEM_PROMPT.contains("no sandbox, Skills, Hooks"));
        assert!(!SYSTEM_PROMPT.contains("MCP"));
    }

    #[test]
    fn auto_edit_changes_only_the_file_policy() {
        assert_eq!(
            interactive_policies(ApprovalMode::Ask),
            (FileChangePolicy::Ask, ShellPolicy::Ask, PluginPolicy::Ask,)
        );
        assert_eq!(
            interactive_policies(ApprovalMode::AutoEdit),
            (FileChangePolicy::Allow, ShellPolicy::Ask, PluginPolicy::Ask,)
        );
    }

    #[test]
    fn resumed_model_selection_does_not_freeze_old_provider_defaults() {
        let previous = EpochHeader {
            config: serde_json::from_value(serde_json::json!({
                "provider": "foreign-provider",
                "model": "stored-model",
                "temperature": 0.5,
                "maxTokens": 123,
                "extension": { "oldDefault": true }
            }))
            .unwrap(),
            adapter_defaults: None,
            system: None,
            tools: None,
        };

        let cases = [
            (None, None, DEFAULT_MODEL),
            (None, Some("override".to_owned()), "override"),
            (Some(&previous), None, "stored-model"),
            (Some(&previous), Some("override".to_owned()), "override"),
        ];
        for (header, requested, expected_model) in cases {
            let selected = select_call(header, requested).unwrap();
            assert_eq!(selected.provider(), DEEPSEEK_PROVIDER);
            assert_eq!(selected.model(), expected_model);
            assert_eq!(selected.temperature(), None);
            assert_eq!(selected.max_tokens(), None);
            assert_eq!(
                selected,
                LlmCallConfig::new(DEEPSEEK_PROVIDER, expected_model).unwrap()
            );
        }
    }
}
