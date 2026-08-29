use std::sync::Arc;

use futures_util::stream;
use tokio_util::sync::CancellationToken;

use super::{
    AgentLoop, AgentLoopConfig, AgentLoopError, FileChangePolicy, NoTools, PluginPolicy,
    ShellPolicy,
};
use crate::{
    model::LlmCallConfig,
    provider::{
        ModelProvider, PreparedProviderCall, PreparedRequestPreflight, ProviderPreflightError,
        ProviderPrepareError, ProviderRequest, ProviderRequestDraft, ProviderStream,
    },
    session::{EventKind, NewEvent, PermissionPreset, Session, TurnId},
};

struct UnusedProvider;

impl ModelProvider for UnusedProvider {
    fn prepare_call(
        &self,
        config: LlmCallConfig,
    ) -> Result<PreparedProviderCall, ProviderPrepareError> {
        Err(ProviderPrepareError::WrongProvider {
            expected: "unused".to_owned(),
            actual: config.provider().to_owned(),
        })
    }

    fn preflight_request(
        &self,
        draft: ProviderRequestDraft<'_>,
    ) -> Result<PreparedRequestPreflight, ProviderPreflightError> {
        Err(ProviderPrepareError::WrongProvider {
            expected: "unused".to_owned(),
            actual: draft.config().provider().to_owned(),
        }
        .into())
    }

    fn stream(
        &self,
        _request: ProviderRequest,
        _cancellation: CancellationToken,
    ) -> ProviderStream {
        Box::pin(stream::empty())
    }
}

fn agent(session: Session) -> AgentLoop {
    AgentLoop::new(
        session,
        Arc::new(UnusedProvider),
        Arc::new(NoTools),
        AgentLoopConfig::new(LlmCallConfig::new("unused", "unused").unwrap())
            .with_file_change_policy(FileChangePolicy::Ask)
            .with_shell_policy(ShellPolicy::Ask)
            .with_plugin_policy(PluginPolicy::Ask),
    )
    .unwrap()
}

#[test]
fn upstream_permission_fixture_names_the_fixed_contract_and_safe_difference() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/tools/upstream_phase53_permission_presets.json"
    ))
    .unwrap();
    assert_eq!(
        fixture["source"]["commit"],
        "47f943859bef60e4160492346772ded9b24f765a"
    );
    assert_eq!(fixture["contract"]["event"]["type"], "permission/preset");
    assert_eq!(
        fixture["rustIntentionalDifference"]["auto-edit"]["shell"],
        "ask"
    );
    assert_eq!(
        fixture["rustIntentionalDifference"]["auto-edit"]["plugins"],
        "ask"
    );
}

#[tokio::test]
async fn permission_selection_is_durable_model_invisible_and_narrow() {
    let mut agent = agent(Session::new("permission-selection").unwrap());
    assert_eq!(
        agent.current_permission_preset(),
        Some(PermissionPreset::Ask)
    );

    assert!(
        agent
            .select_permission_preset(PermissionPreset::AutoEdit)
            .await
            .unwrap()
    );
    assert_eq!(agent.config.file_change_policy, FileChangePolicy::Allow);
    assert_eq!(agent.config.shell_policy, ShellPolicy::Ask);
    assert_eq!(agent.config.plugin_policy, PluginPolicy::Ask);
    assert_eq!(
        agent.session().state().permission_preset(),
        Some(PermissionPreset::AutoEdit)
    );
    assert!(agent.session().state().surface_nodes().is_empty());
    assert!(agent.session().messages().is_empty());
    assert!(matches!(
        agent.session().events().last().map(|event| event.kind()),
        Some(EventKind::PermissionPreset {
            preset: PermissionPreset::AutoEdit
        })
    ));

    let count = agent.session().events().len();
    assert!(
        !agent
            .select_permission_preset(PermissionPreset::AutoEdit)
            .await
            .unwrap()
    );
    assert_eq!(agent.session().events().len(), count);
}

#[tokio::test]
async fn failed_permission_append_preserves_the_old_policy() {
    let mut session = Session::new("permission-full-session").unwrap();
    for _ in 0..crate::session::MAX_SESSION_EVENTS {
        session
            .append(NewEvent::log(EventKind::permission_preset(
                PermissionPreset::Ask,
            )))
            .unwrap();
    }
    let mut agent = agent(session);

    assert!(
        agent
            .select_permission_preset(PermissionPreset::AutoEdit)
            .await
            .is_err()
    );
    assert_eq!(agent.config.file_change_policy, FileChangePolicy::Ask);
    assert_eq!(
        agent.current_permission_preset(),
        Some(PermissionPreset::Ask)
    );
}

#[tokio::test]
async fn permission_selection_is_idle_only() {
    let mut agent = agent(Session::new("permission-busy").unwrap());
    agent
        .session
        .append(NewEvent::log(EventKind::turn_start(
            TurnId::new(1).unwrap(),
        )))
        .unwrap();

    assert!(matches!(
        agent
            .select_permission_preset(PermissionPreset::AutoEdit)
            .await,
        Err(AgentLoopError::SessionNotIdle)
    ));
}
