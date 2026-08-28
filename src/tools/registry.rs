use std::{path::Path, sync::Arc};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{
        ToolExecutionFuture, ToolExecutionRequest, ToolExecutionResult, ToolExecutor,
        ToolExecutorError, ToolShutdownFuture,
    },
    goal::{GoalBlockReason, GoalError, GoalRuntime, GoalUpdate, MAX_GOAL_OBJECTIVE_BYTES},
    model::{ContentBlock, JsonValue, ToolSchema},
    user_question::{
        MAX_QUESTION_HEADER_BYTES, MAX_QUESTION_ID_BYTES, MAX_QUESTION_OPTION_DESCRIPTION_BYTES,
        MAX_QUESTION_OPTION_LABEL_BYTES, MAX_QUESTION_OPTIONS, MAX_QUESTION_TEXT_BYTES,
        MAX_USER_QUESTIONS, MIN_QUESTION_OPTIONS, UserQuestionBroker, UserQuestionError,
        UserQuestionItem, UserQuestionOption, UserQuestionRequest,
    },
    workspace_authority::WorkspaceAuthority,
};

use super::{
    MAX_TOOL_CONTENT_BYTES,
    arguments::{parse_glob, parse_grep, parse_list, parse_read},
    error::{ToolCallError, ToolCallResult, ToolRegistryBuildError},
    workspace::Workspace,
    {glob, grep, list, read},
};

#[cfg(unix)]
use super::patch;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::plugin::{PluginConfig, PluginHost, approval_required_result, prepare_action};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::process::ProcessRunner;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::shell::{self, ShellEnvironment};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::agent::ToolClaimProfile;
#[cfg(unix)]
use crate::agent::{GoalToolCaller, ToolPreparation, ToolPreparationFuture};

/// Immutable catalogue and capability root for the four Phase 4 read-only tools.
pub struct ReadOnlyToolRegistry {
    workspace: Workspace,
    schemas: Arc<[ToolSchema]>,
}

impl std::fmt::Debug for ReadOnlyToolRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadOnlyToolRegistry")
            .field("workspace_configured", &true)
            .field("schema_count", &self.schemas.len())
            .finish()
    }
}

impl ReadOnlyToolRegistry {
    /// Open and permanently bind the registry to one existing workspace directory.
    pub fn open(workspace: impl AsRef<Path>) -> Result<Self, ToolRegistryBuildError> {
        let workspace = Workspace::open(workspace.as_ref())?;
        let schemas = build_schemas()?.into();
        Ok(Self { workspace, schemas })
    }

    /// Ordered tool declarations sent to the model by `AgentLoopConfig`.
    #[must_use]
    pub fn schemas(&self) -> &[ToolSchema] {
        &self.schemas
    }

    /// Normalized startup workspace used only for application assembly/display.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        self.workspace.display_root()
    }
}

impl ToolExecutor for ReadOnlyToolRegistry {
    fn execute(
        &self,
        request: ToolExecutionRequest,
        cancellation: CancellationToken,
    ) -> ToolExecutionFuture<'_> {
        let workspace = self.workspace.clone();
        Box::pin(async move {
            let outcome = dispatch(
                &workspace,
                request.name(),
                request.arguments().as_value(),
                &cancellation,
            )
            .await;
            match outcome {
                Ok(text) => normalize_success(text),
                Err(error) => error.into_execution_result(),
            }
        })
    }
}

/// Capability-bound catalogue containing the four read tools plus one
/// approval-gated, two-stage `apply_patch` mutation tool.
#[cfg(unix)]
pub struct WorkspaceToolRegistry {
    workspace: Workspace,
    schemas: Arc<[ToolSchema]>,
}

#[cfg(unix)]
impl std::fmt::Debug for WorkspaceToolRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceToolRegistry")
            .field("workspace_configured", &true)
            .field("schema_count", &self.schemas.len())
            .finish()
    }
}

#[cfg(unix)]
impl WorkspaceToolRegistry {
    pub fn open(workspace: impl AsRef<Path>) -> Result<Self, ToolRegistryBuildError> {
        let workspace = Workspace::open(workspace.as_ref())?;
        let schemas = build_workspace_schemas()?;
        Ok(Self {
            workspace,
            schemas: schemas.into(),
        })
    }

    #[must_use]
    pub fn schemas(&self) -> &[ToolSchema] {
        &self.schemas
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        self.workspace.display_root()
    }
}

/// Explicit local authority containing read/search, file mutation, and one
/// approval-gated foreground Bash action. It retains exactly one workspace
/// capability shared by all six tools.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct LocalToolRegistry {
    workspace: Arc<Workspace>,
    schemas: Arc<[ToolSchema]>,
    environment: ShellEnvironment,
    runner: Arc<ProcessRunner>,
    plugins: Option<Arc<PluginHost>>,
    goal: Option<GoalRuntime>,
    user_questions: Option<UserQuestionBroker>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl std::fmt::Debug for LocalToolRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalToolRegistry")
            .field("workspace_configured", &true)
            .field("schema_count", &self.schemas.len())
            .field("environment", &self.environment)
            .field("process_runner", &self.runner)
            .field(
                "plugin_count",
                &self
                    .plugins
                    .as_ref()
                    .map_or(0, |plugins| plugins.schemas().len()),
            )
            .field("goal_enabled", &self.goal.is_some())
            .field("user_questions_enabled", &self.user_questions.is_some())
            .finish()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl LocalToolRegistry {
    pub fn open(workspace: impl AsRef<Path>) -> Result<Self, ToolRegistryBuildError> {
        let path = workspace.as_ref();
        let authority = WorkspaceAuthority::open(path).map_err(|source| {
            ToolRegistryBuildError::InvalidWorkspace {
                path: path.to_owned(),
                source,
            }
        })?;
        Self::from_authority(authority)
    }

    pub(crate) fn from_authority(
        authority: WorkspaceAuthority,
    ) -> Result<Self, ToolRegistryBuildError> {
        Self::from_authority_with_goal(authority, None)
    }

    pub(crate) fn from_authority_with_goal(
        authority: WorkspaceAuthority,
        goal: Option<GoalRuntime>,
    ) -> Result<Self, ToolRegistryBuildError> {
        Self::from_authority_with_interaction(authority, goal, None)
    }

    pub(crate) fn from_authority_with_interaction(
        authority: WorkspaceAuthority,
        goal: Option<GoalRuntime>,
        user_questions: Option<UserQuestionBroker>,
    ) -> Result<Self, ToolRegistryBuildError> {
        let workspace = Arc::new(Workspace::from_authority(authority));
        let environment = ShellEnvironment::capture()?;
        let runner = Arc::new(
            ProcessRunner::open()
                .map_err(|_| ToolRegistryBuildError::UnsupportedProcessObserver)?,
        );
        let mut schemas = build_workspace_schemas()?;
        schemas.push(shell::schema()?);
        if goal.is_some() {
            schemas.extend(build_goal_schemas()?);
        }
        if user_questions.is_some() {
            schemas.push(build_user_question_schema()?);
        }
        Ok(Self {
            workspace,
            schemas: schemas.into(),
            environment,
            runner,
            plugins: None,
            goal,
            user_questions,
        })
    }

    pub(crate) async fn from_authority_with_plugins(
        authority: WorkspaceAuthority,
        config: PluginConfig,
        cancellation: CancellationToken,
        goal: Option<GoalRuntime>,
        user_questions: Option<UserQuestionBroker>,
    ) -> Result<Self, ToolRegistryBuildError> {
        let mut registry = Self::from_authority_with_interaction(authority, goal, user_questions)?;
        let plugins = Arc::new(
            PluginHost::start(config, &registry.schemas, cancellation)
                .await
                .map_err(|error| match error {
                    super::plugin::PluginHostError::Startup { plugin_id } => {
                        ToolRegistryBuildError::PluginStartup { plugin_id }
                    }
                    super::plugin::PluginHostError::ToolCollision
                    | super::plugin::PluginHostError::TooManyTools
                    | super::plugin::PluginHostError::Shutdown => ToolRegistryBuildError::Plugin,
                })?,
        );
        let mut schemas = registry.schemas.to_vec();
        if schemas.try_reserve_exact(plugins.schemas().len()).is_err() {
            let _ = plugins.shutdown().await;
            return Err(ToolRegistryBuildError::UnsupportedProcessObserver);
        }
        schemas.extend_from_slice(plugins.schemas());
        registry.schemas = schemas.into();
        registry.plugins = Some(plugins);
        Ok(registry)
    }

    #[must_use]
    pub fn schemas(&self) -> &[ToolSchema] {
        &self.schemas
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        self.workspace.display_root()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ToolExecutor for LocalToolRegistry {
    fn claim_profile(&self, tool_name: &str) -> ToolClaimProfile {
        if tool_name == "bash" {
            ToolClaimProfile::shell_action()
        } else if tool_name == "ask_user_question" {
            ToolClaimProfile::user_question()
        } else if let Some(plugin_id) = self
            .plugins
            .as_ref()
            .and_then(|plugins| plugins.plugin_id(tool_name))
        {
            match ToolClaimProfile::plugin_action(plugin_id.to_owned()) {
                Ok(profile) => profile,
                Err(_) => ToolClaimProfile::standard(),
            }
        } else {
            ToolClaimProfile::standard()
        }
    }

    fn execute(
        &self,
        request: ToolExecutionRequest,
        cancellation: CancellationToken,
    ) -> ToolExecutionFuture<'_> {
        if is_goal_tool(request.name()) {
            let goal = self.goal.clone();
            return Box::pin(async move { dispatch_goal(goal, &request) });
        }
        if request.name() == "bash" {
            return Box::pin(async { shell::approval_required_result() });
        }
        if let Some(plugin_id) = self
            .plugins
            .as_ref()
            .and_then(|plugins| plugins.plugin_id(request.name()))
        {
            let result = approval_required_result(plugin_id);
            return Box::pin(async move { result });
        }
        if request.name() == "apply_patch" {
            return Box::pin(async {
                ToolCallError::model(
                    "ApprovalError",
                    "APPROVAL_REQUIRED",
                    "apply_patch must use the Agent approval preparation stage",
                )
                .into_execution_result()
            });
        }
        let workspace = Arc::clone(&self.workspace);
        Box::pin(async move {
            let outcome = dispatch(
                workspace.as_ref(),
                request.name(),
                request.arguments().as_value(),
                &cancellation,
            )
            .await;
            match outcome {
                Ok(text) => normalize_success(text),
                Err(error) => error.into_execution_result(),
            }
        })
    }

    fn prepare(
        &self,
        request: ToolExecutionRequest,
        cancellation: CancellationToken,
    ) -> ToolPreparationFuture<'_> {
        let workspace = Arc::clone(&self.workspace);
        let environment = self.environment.entries();
        let runner = Arc::clone(&self.runner);
        let plugins = self.plugins.clone();
        let goal = self.goal.clone();
        let user_questions = self.user_questions.clone();
        Box::pin(async move {
            if is_goal_tool(request.name()) {
                return prepare_goal(goal, &request);
            }
            if request.name() == "ask_user_question" {
                return prepare_user_question(user_questions, &request, cancellation).await;
            }
            if request.name() == "apply_patch" {
                return patch::prepare(
                    workspace.as_ref(),
                    request.arguments().as_value(),
                    &cancellation,
                )
                .await;
            }
            if request.name() == "bash" {
                return shell::prepare_action_setup(
                    request,
                    workspace,
                    Box::new(move |dispatch, invocation| {
                        shell::finish_action(dispatch, invocation, environment, runner)
                    }),
                );
            }
            if let Some(plugins) = plugins.filter(|plugins| plugins.contains(request.name())) {
                return prepare_action(request, plugins);
            }
            let outcome = dispatch(
                workspace.as_ref(),
                request.name(),
                request.arguments().as_value(),
                &cancellation,
            )
            .await;
            let result = match outcome {
                Ok(text) => normalize_success(text),
                Err(error) => error.into_execution_result(),
            }?;
            Ok(ToolPreparation::Complete(result))
        })
    }

    fn shutdown(&self) -> ToolShutdownFuture<'_> {
        let plugins = self.plugins.clone();
        Box::pin(async move {
            let Some(plugins) = plugins else {
                return Ok(());
            };
            plugins
                .shutdown()
                .await
                .map_err(|_| ToolExecutorError::new("plugin host shutdown failed"))
        })
    }
}

#[cfg(unix)]
impl ToolExecutor for WorkspaceToolRegistry {
    fn execute(
        &self,
        request: ToolExecutionRequest,
        cancellation: CancellationToken,
    ) -> ToolExecutionFuture<'_> {
        if request.name() == "apply_patch" {
            return Box::pin(async {
                ToolCallError::model(
                    "ApprovalError",
                    "APPROVAL_REQUIRED",
                    "apply_patch must use the Agent approval preparation stage",
                )
                .into_execution_result()
            });
        }
        let workspace = self.workspace.clone();
        Box::pin(async move {
            let outcome = dispatch(
                &workspace,
                request.name(),
                request.arguments().as_value(),
                &cancellation,
            )
            .await;
            match outcome {
                Ok(text) => normalize_success(text),
                Err(error) => error.into_execution_result(),
            }
        })
    }

    fn prepare(
        &self,
        request: ToolExecutionRequest,
        cancellation: CancellationToken,
    ) -> ToolPreparationFuture<'_> {
        let workspace = self.workspace.clone();
        Box::pin(async move {
            if request.name() == "apply_patch" {
                return patch::prepare(&workspace, request.arguments().as_value(), &cancellation)
                    .await;
            }
            let outcome = dispatch(
                &workspace,
                request.name(),
                request.arguments().as_value(),
                &cancellation,
            )
            .await;
            let result = match outcome {
                Ok(text) => normalize_success(text),
                Err(error) => error.into_execution_result(),
            }?;
            Ok(ToolPreparation::Complete(result))
        })
    }
}

async fn dispatch(
    workspace: &Workspace,
    name: &str,
    arguments: &serde_json::Value,
    cancellation: &CancellationToken,
) -> ToolCallResult<String> {
    if cancellation.is_cancelled() {
        return Err(ToolCallError::aborted());
    }
    match name {
        "list" => list::execute(workspace, parse_list(arguments)?, cancellation).await,
        "glob" => glob::execute(workspace, parse_glob(arguments)?, cancellation).await,
        "grep" => grep::execute(workspace, parse_grep(arguments)?, cancellation).await,
        "read" => read::execute(workspace, parse_read(arguments)?, cancellation).await,
        _ => Err(ToolCallError::unknown_tool()),
    }
}

fn normalize_success(text: String) -> Result<ToolExecutionResult, ToolExecutorError> {
    if text.len() > MAX_TOOL_CONTENT_BYTES {
        return ToolCallError::output_limit().into_execution_result();
    }
    let block = ContentBlock::text(text)
        .map_err(|_| ToolExecutorError::new("read-only tool output normalization failed"))?;
    if block.raw().encoded_len() > MAX_TOOL_CONTENT_BYTES {
        return ToolCallError::output_limit().into_execution_result();
    }
    ToolExecutionResult::success(vec![block])
        .map_err(|_| ToolExecutorError::new("read-only tool output normalization failed"))
}

fn is_goal_tool(name: &str) -> bool {
    matches!(name, "get_goal" | "create_goal" | "update_goal")
}

async fn prepare_user_question(
    broker: Option<UserQuestionBroker>,
    request: &ToolExecutionRequest,
    cancellation: CancellationToken,
) -> Result<ToolPreparation, ToolExecutorError> {
    let Some(broker) = broker else {
        return Ok(ToolPreparation::Complete(
            ToolCallError::unknown_tool().into_execution_result()?,
        ));
    };
    let question = match parse_user_question_arguments(request.arguments().as_value()) {
        Ok(question) => question,
        Err(error) => {
            return Ok(ToolPreparation::Complete(error.into_execution_result()?));
        }
    };
    let result = broker.ask(question, cancellation).await;
    let result = match result {
        Ok(answer) => serde_json::to_string(&json!({
            "answers": answer.answers().iter().map(|answer| json!({
                "id": answer.id(),
                "selected": [answer.selected()],
            })).collect::<Vec<_>>()
        }))
        .map_err(|_| ToolExecutorError::new("user-question result normalization failed"))
        .and_then(normalize_success),
        Err(error) => user_question_call_error(error).into_execution_result(),
    }?;
    Ok(ToolPreparation::Complete(result))
}

fn parse_user_question_arguments(
    arguments: &serde_json::Value,
) -> ToolCallResult<UserQuestionRequest> {
    let fields = arguments.as_object().ok_or_else(|| {
        ToolCallError::invalid_args("ask_user_question arguments must be an object")
    })?;
    if fields.keys().any(|key| key != "questions") {
        return Err(ToolCallError::invalid_args(
            "ask_user_question received an unknown argument",
        ));
    }
    let questions = fields
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ToolCallError::invalid_args("questions must be an array"))?;
    if questions.is_empty() || questions.len() > MAX_USER_QUESTIONS {
        return Err(ToolCallError::invalid_args(
            "questions must contain between one and three questions",
        ));
    }
    let mut parsed = Vec::new();
    parsed
        .try_reserve_exact(questions.len())
        .map_err(|_| ToolCallError::invalid_args("questions could not be retained"))?;
    for question in questions {
        let question = parse_user_question_item(question)?;
        if parsed
            .iter()
            .any(|existing: &UserQuestionItem| existing.id() == question.id())
        {
            return Err(ToolCallError::invalid_args("question ids must be unique"));
        }
        parsed.push(question);
    }
    Ok(UserQuestionRequest::new(parsed))
}

fn parse_user_question_item(value: &serde_json::Value) -> ToolCallResult<UserQuestionItem> {
    let question = value
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("question must be an object"))?;
    if question.keys().any(|key| {
        !matches!(
            key.as_str(),
            "id" | "question" | "header" | "options" | "multi_select"
        )
    }) {
        return Err(ToolCallError::invalid_args(
            "question received an unknown argument",
        ));
    }
    if question
        .get("multi_select")
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        return Err(ToolCallError::invalid_args(
            "multi_select must be false in this terminal version",
        ));
    }
    let id = bounded_question_text(question.get("id"), "id", MAX_QUESTION_ID_BYTES)?;
    let text = bounded_question_text(
        question.get("question"),
        "question",
        MAX_QUESTION_TEXT_BYTES,
    )?;
    let header = question
        .get("header")
        .map(|value| bounded_question_text(Some(value), "header", MAX_QUESTION_HEADER_BYTES))
        .transpose()?;
    let option_values = question
        .get("options")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ToolCallError::invalid_args("options must be an array"))?;
    if !(MIN_QUESTION_OPTIONS..=MAX_QUESTION_OPTIONS).contains(&option_values.len()) {
        return Err(ToolCallError::invalid_args(
            "options must contain between two and four choices",
        ));
    }
    let mut options = Vec::new();
    options
        .try_reserve_exact(option_values.len())
        .map_err(|_| ToolCallError::invalid_args("question options could not be retained"))?;
    for value in option_values {
        let fields = value
            .as_object()
            .ok_or_else(|| ToolCallError::invalid_args("option must be an object"))?;
        if fields
            .keys()
            .any(|key| !matches!(key.as_str(), "label" | "description"))
        {
            return Err(ToolCallError::invalid_args(
                "option received an unknown argument",
            ));
        }
        let label = bounded_question_text(
            fields.get("label"),
            "option label",
            MAX_QUESTION_OPTION_LABEL_BYTES,
        )?;
        if options
            .iter()
            .any(|option: &UserQuestionOption| option.label() == label)
        {
            return Err(ToolCallError::invalid_args("option labels must be unique"));
        }
        let description = fields
            .get("description")
            .map(|value| {
                bounded_question_text(
                    Some(value),
                    "option description",
                    MAX_QUESTION_OPTION_DESCRIPTION_BYTES,
                )
            })
            .transpose()?;
        options.push(UserQuestionOption::new(label, description));
    }
    Ok(UserQuestionItem::new(id, header, text, options))
}

fn bounded_question_text(
    value: Option<&serde_json::Value>,
    field: &'static str,
    maximum: usize,
) -> ToolCallResult<String> {
    let value = value
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ToolCallError::invalid_args(format!("{field} must be a string")))?;
    if value.is_empty()
        || value.len() > maximum
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(ToolCallError::invalid_args(format!(
            "{field} must be nonblank, trimmed, control-free, and at most {maximum} bytes"
        )));
    }
    Ok(value.to_owned())
}

fn user_question_call_error(error: UserQuestionError) -> ToolCallError {
    let (code, message) = match error {
        UserQuestionError::Cancelled => (
            "ASK_CANCELLED",
            "ask_user_question was cancelled before the user answered",
        ),
        UserQuestionError::Unavailable => (
            "NO_PROVIDER",
            "no terminal user-question answerer is available",
        ),
        UserQuestionError::InvalidResponse => (
            "INVALID_RESPONSE",
            "the terminal returned an invalid user-question answer",
        ),
    };
    ToolCallError::model("UserQuestionError", code, message)
}

fn dispatch_goal(
    goal: Option<GoalRuntime>,
    request: &ToolExecutionRequest,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    let Some(goal) = goal else {
        return ToolCallError::unknown_tool().into_execution_result();
    };
    let result = match request.name() {
        "get_goal" => parse_empty_goal_arguments(request.arguments().as_value())
            .and_then(|()| goal.snapshot().map_err(goal_call_error))
            .and_then(|snapshot| {
                let value =
                    snapshot.map_or_else(|| json!({ "goal": null }), |goal| goal.tool_value());
                serde_json::to_string(&value).map_err(|_| {
                    ToolCallError::model(
                        "GoalError",
                        "GOAL_UNAVAILABLE",
                        "Goal state could not be encoded",
                    )
                })
            }),
        "create_goal" => parse_create_goal_arguments(request.arguments().as_value())
            .and_then(|(objective, max_goal_rounds)| {
                goal.prepare_create_with_max(objective, max_goal_rounds)
                    .and_then(|mutation| mutation.commit_snapshot())
                    .map_err(goal_call_error)
            })
            .and_then(|snapshot| {
                serde_json::to_string(&snapshot.tool_value()).map_err(|_| {
                    ToolCallError::model(
                        "GoalError",
                        "GOAL_UNAVAILABLE",
                        "Goal state could not be encoded",
                    )
                })
            }),
        "update_goal" => parse_update_goal_arguments(request.arguments().as_value())
            .and_then(|arguments| {
                goal.prepare_update_exact(
                    Some(&arguments.goal_id),
                    arguments.revision,
                    arguments.operation,
                    arguments.objective,
                    arguments.max_goal_rounds,
                    arguments.blocked_reason,
                )
                .and_then(|mutation| mutation.commit_snapshot())
                .map_err(goal_call_error)
            })
            .and_then(|snapshot| {
                serde_json::to_string(&snapshot.tool_value()).map_err(|_| {
                    ToolCallError::model(
                        "GoalError",
                        "GOAL_UNAVAILABLE",
                        "Goal state could not be encoded",
                    )
                })
            }),
        _ => Err(ToolCallError::unknown_tool()),
    };
    match result {
        Ok(text) => normalize_success(text),
        Err(error) => error.into_execution_result(),
    }
}

fn prepare_goal(
    goal: Option<GoalRuntime>,
    request: &ToolExecutionRequest,
) -> Result<ToolPreparation, ToolExecutorError> {
    let Some(goal) = goal else {
        return Ok(ToolPreparation::Complete(
            ToolCallError::unknown_tool().into_execution_result()?,
        ));
    };
    if request.name() == "get_goal" {
        return dispatch_goal(Some(goal), request).map(ToolPreparation::Complete);
    }
    let caller = request.dispatch_binding().goal_caller();
    let prepared = match request.name() {
        "create_goal" => require_goal_authority(caller, GoalAuthorityNeed::DirectHuman)
            .and_then(|()| parse_create_goal_arguments(request.arguments().as_value()))
            .and_then(|(objective, max_goal_rounds)| {
                goal.prepare_create_with_max(objective, max_goal_rounds)
                    .map_err(goal_call_error)
            }),
        "update_goal" => {
            parse_update_goal_arguments(request.arguments().as_value()).and_then(|arguments| {
                let need = match arguments.operation {
                    GoalUpdate::Edit | GoalUpdate::Pause | GoalUpdate::Resume => {
                        GoalAuthorityNeed::DirectHuman
                    }
                    GoalUpdate::Complete | GoalUpdate::Blocked => GoalAuthorityNeed::Completion,
                };
                require_goal_authority(caller, need)?;
                if arguments.operation == GoalUpdate::Blocked
                    && caller == GoalToolCaller::GoalRound
                    && goal
                        .snapshot()
                        .map_err(goal_call_error)?
                        .is_none_or(|snapshot| snapshot.rounds_started() < 3)
                {
                    return Err(goal_call_error(GoalError::BlockThreshold));
                }
                goal.prepare_update_exact(
                    Some(&arguments.goal_id),
                    arguments.revision,
                    arguments.operation,
                    arguments.objective,
                    arguments.max_goal_rounds,
                    arguments.blocked_reason,
                )
                .map_err(goal_call_error)
            })
        }
        _ => Err(ToolCallError::unknown_tool()),
    };
    match prepared {
        Ok(mutation) => Ok(ToolPreparation::Goal(mutation)),
        Err(error) => Ok(ToolPreparation::Complete(error.into_execution_result()?)),
    }
}

#[derive(Clone, Copy)]
enum GoalAuthorityNeed {
    DirectHuman,
    Completion,
}

fn require_goal_authority(caller: GoalToolCaller, need: GoalAuthorityNeed) -> ToolCallResult<()> {
    let allowed = match need {
        GoalAuthorityNeed::DirectHuman => caller == GoalToolCaller::DirectHuman,
        GoalAuthorityNeed::Completion => matches!(
            caller,
            GoalToolCaller::DirectHuman | GoalToolCaller::GoalRound
        ),
    };
    if allowed {
        Ok(())
    } else {
        Err(ToolCallError::model(
            "GoalError",
            "GOAL_TOOL_AUTHORITY_REQUIRED",
            "this Goal operation is not allowed from the current turn source",
        ))
    }
}

fn parse_empty_goal_arguments(arguments: &serde_json::Value) -> ToolCallResult<()> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("goal tool arguments must be an object"))?;
    if fields.is_empty() {
        Ok(())
    } else {
        Err(ToolCallError::invalid_args(
            "get_goal does not accept arguments",
        ))
    }
}

fn parse_create_goal_arguments(
    arguments: &serde_json::Value,
) -> ToolCallResult<(String, Option<u32>)> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("create_goal arguments must be an object"))?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "objective" | "max_goal_rounds"))
    {
        return Err(ToolCallError::invalid_args(
            "create_goal received an unknown argument",
        ));
    }
    let objective = fields
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolCallError::invalid_args("objective must be a string"))?;
    let max_goal_rounds = fields
        .get("max_goal_rounds")
        .map(parse_positive_goal_cap)
        .transpose()?;
    Ok((objective, max_goal_rounds))
}

struct ParsedGoalUpdate {
    goal_id: String,
    revision: u64,
    operation: GoalUpdate,
    objective: Option<String>,
    max_goal_rounds: Option<u32>,
    blocked_reason: Option<GoalBlockReason>,
}

fn parse_update_goal_arguments(arguments: &serde_json::Value) -> ToolCallResult<ParsedGoalUpdate> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("update_goal arguments must be an object"))?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "goal_id" | "revision" | "action" | "objective" | "max_goal_rounds" | "blocked_reason"
        )
    }) {
        return Err(ToolCallError::invalid_args(
            "update_goal received an unknown argument",
        ));
    }
    let goal_id = fields
        .get("goal_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 256
                && *id == id.trim()
                && !id.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(goal_invalid_update)?;
    let revision = fields
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .filter(|revision| *revision != 0 && *revision <= crate::session::MAX_SAFE_INTEGER)
        .ok_or_else(goal_invalid_update)?;
    let operation = fields
        .get("action")
        .and_then(serde_json::Value::as_str)
        .and_then(GoalUpdate::parse)
        .ok_or_else(goal_invalid_update)?;
    let objective = optional_goal_text(fields.get("objective"), "objective")?;
    let max_goal_rounds = match fields.get("max_goal_rounds") {
        Some(value) if value.as_u64() == Some(0) => None,
        Some(value) => Some(parse_positive_goal_cap(value)?),
        None => None,
    };
    let blocked_reason = optional_goal_text(fields.get("blocked_reason"), "blocked_reason")?;
    let blocked_reason = match operation {
        GoalUpdate::Edit => {
            if blocked_reason.is_some() || (objective.is_none() && max_goal_rounds.is_none()) {
                return Err(goal_invalid_update());
            }
            None
        }
        GoalUpdate::Pause | GoalUpdate::Resume | GoalUpdate::Complete => {
            if objective.is_some() || max_goal_rounds.is_some() || blocked_reason.is_some() {
                return Err(goal_invalid_update());
            }
            None
        }
        GoalUpdate::Blocked => {
            if objective.is_some() || max_goal_rounds.is_some() {
                return Err(goal_invalid_update());
            }
            Some(
                GoalBlockReason::model_reported(
                    blocked_reason.as_deref().ok_or_else(goal_invalid_update)?,
                )
                .map_err(|_| goal_invalid_update())?,
            )
        }
    };
    Ok(ParsedGoalUpdate {
        goal_id,
        revision,
        operation,
        objective,
        max_goal_rounds,
        blocked_reason,
    })
}

fn optional_goal_text(
    value: Option<&serde_json::Value>,
    field: &'static str,
) -> ToolCallResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| ToolCallError::invalid_args(format!("{field} must be a string")))?;
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn parse_positive_goal_cap(value: &serde_json::Value) -> ToolCallResult<u32> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            ToolCallError::model(
                "GoalError",
                "GOAL_INVALID_MAX_ROUNDS",
                "max_goal_rounds must be a positive 32-bit integer",
            )
        })
}

fn goal_invalid_update() -> ToolCallError {
    ToolCallError::model(
        "GoalError",
        "GOAL_TOOL_INVALID_UPDATE",
        "update_goal arguments are invalid for the selected action",
    )
}

fn goal_call_error(error: GoalError) -> ToolCallError {
    let code = match &error {
        GoalError::Missing => "GOAL_NOT_FOUND",
        GoalError::Unfinished => "GOAL_ALREADY_EXISTS",
        GoalError::EmptyObjective | GoalError::ObjectiveTooLarge => "GOAL_INVALID_OBJECTIVE",
        GoalError::InvalidMaxRounds => "GOAL_INVALID_MAX_ROUNDS",
        GoalError::InvalidEdit => "GOAL_INVALID_EDIT",
        GoalError::InvalidBlockReason => "GOAL_INVALID_BLOCK_REASON",
        GoalError::StaleRevision => "GOAL_STALE_REVISION",
        GoalError::InvalidTransition => "GOAL_INVALID_TRANSITION",
        GoalError::BlockThreshold => "GOAL_TOOL_BLOCK_THRESHOLD",
        GoalError::Busy => "GOAL_BUSY",
        GoalError::Unavailable | GoalError::Commit(_) | GoalError::InvalidEvent => {
            "GOAL_UNAVAILABLE"
        }
    };
    ToolCallError::model("GoalError", code, error.to_string())
}

fn build_schemas() -> Result<Vec<ToolSchema>, ToolRegistryBuildError> {
    Ok(vec![
        schema(
            "list",
            "List one workspace directory without reading file contents.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Nonblank workspace-relative or inside-workspace absolute directory path (default: .); maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    }
                },
                "additionalProperties": false
            }),
        )?,
        schema(
            "glob",
            "Find workspace files whose relative path matches a glob pattern.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern; a basename pattern matches at any depth; runtime maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    },
                    "path": {
                        "type": "string",
                        "description": "Nonblank workspace-relative or inside-workspace absolute directory path (default: .); maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        )?,
        schema(
            "grep",
            "Search workspace file lines with a Rust regular expression.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression; runtime maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^[^\\u0000-\\u001F\\u007F-\\u009F]+$"
                    },
                    "path": {
                        "type": "string",
                        "description": "Nonblank workspace-relative or inside-workspace absolute file or directory path (default: .); maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    },
                    "include": {
                        "type": "string",
                        "description": "One positive file glob, for example *.{rs,toml}; runtime maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        )?,
        schema(
            "read",
            "Read a bounded UTF-8 page from one regular workspace file.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Nonblank workspace-relative or inside-workspace absolute regular file path; maximum is 4096 UTF-8 bytes",
                        "minLength": 1,
                        "maxLength": 4096,
                        "pattern": "^(?=.*\\S)[^\\u0000-\\u001F\\u007F-\\u009F]*$"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "One-based first line (default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 2000,
                        "description": "Maximum lines to return (default: 2000)"
                    }
                },
                "required": ["file_path"],
                "additionalProperties": false
            }),
        )?,
    ])
}

#[cfg(unix)]
fn build_workspace_schemas() -> Result<Vec<ToolSchema>, ToolRegistryBuildError> {
    let mut schemas = build_schemas()?;
    schemas.push(schema(
        "apply_patch",
        "Prepare one bounded single-file unified diff for approval and atomic publication.",
        json!({
            "type": "object",
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "One strict create/update unified diff; runtime maximum is 262144 UTF-8 bytes",
                    "minLength": 1,
                    "maxLength": 262144
                }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
    )?);
    Ok(schemas)
}

fn build_goal_schemas() -> Result<[ToolSchema; 3], ToolRegistryBuildError> {
    Ok([
        schema(
            "get_goal",
            "Read the current same-session Goal, including its exact id, revision, phase, activation, and round limit.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )?,
        schema(
            "create_goal",
            "Create and arm one persisted same-session Goal from a direct human request.",
            json!({
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_GOAL_OBJECTIVE_BYTES,
                        "description": "Concrete nonblank objective for bounded automatic continuation"
                    },
                    "max_goal_rounds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 4294967295_u32,
                        "description": "Optional positive automatic continuation round limit"
                    }
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
        )?,
        schema(
            "update_goal",
            "Update the exact current Goal. Edit, pause, and resume require a direct human turn; complete and blocked also allow the current Goal round.",
            json!({
                "type": "object",
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 256
                    },
                    "revision": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "action": {
                        "type": "string",
                        "enum": ["edit", "pause", "resume", "complete", "blocked"]
                    },
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_GOAL_OBJECTIVE_BYTES,
                        "description": "Replacement objective; valid only with action edit"
                    },
                    "max_goal_rounds": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 4294967295_u32,
                        "description": "Replacement round limit; valid only with action edit"
                    },
                    "blocked_reason": {
                        "type": "string",
                        "maxLength": 4096,
                        "description": "Concrete blocking condition; required only with action blocked"
                    }
                },
                "required": ["goal_id", "revision", "action"],
                "additionalProperties": false
            }),
        )?,
    ])
}

fn build_user_question_schema() -> Result<ToolSchema, ToolRegistryBuildError> {
    schema(
        "ask_user_question",
        "Ask the user one concise single-choice question when a decision or missing fact is required before continuing.",
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_USER_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_QUESTION_ID_BYTES
                            },
                            "question": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_QUESTION_TEXT_BYTES
                            },
                            "header": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_QUESTION_HEADER_BYTES
                            },
                            "options": {
                                "type": "array",
                                "minItems": MIN_QUESTION_OPTIONS,
                                "maxItems": MAX_QUESTION_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {
                                            "type": "string",
                                            "minLength": 1,
                                            "maxLength": MAX_QUESTION_OPTION_LABEL_BYTES
                                        },
                                        "description": {
                                            "type": "string",
                                            "minLength": 1,
                                            "maxLength": MAX_QUESTION_OPTION_DESCRIPTION_BYTES
                                        }
                                    },
                                    "required": ["label"],
                                    "additionalProperties": false
                                }
                            },
                            "multi_select": {
                                "type": "boolean",
                                "enum": [false],
                                "description": "This terminal version supports single-select only"
                            }
                        },
                        "required": ["id", "question", "options"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["questions"],
            "additionalProperties": false
        }),
    )
}

fn schema(
    name: &'static str,
    description: &'static str,
    parameters: serde_json::Value,
) -> Result<ToolSchema, ToolRegistryBuildError> {
    let parameters =
        JsonValue::new(parameters).map_err(|source| ToolRegistryBuildError::InvalidSchema {
            tool: name,
            source: source.into(),
        })?;
    ToolSchema::new(name, description, parameters)
        .map_err(|source| ToolRegistryBuildError::InvalidSchema { tool: name, source })
}

#[cfg(test)]
mod tests {
    use futures_util::poll;
    use tokio_util::sync::CancellationToken;

    use super::{
        MAX_TOOL_CONTENT_BYTES, build_goal_schemas, build_user_question_schema, dispatch_goal,
        normalize_success, parse_user_question_arguments, prepare_goal, prepare_user_question,
    };
    use crate::{
        agent::{GoalToolCaller, ToolDispatchBinding, ToolExecutionRequest, ToolPreparation},
        goal::GoalRuntime,
        model::{CallId, ContentBlockKind, JsonValue},
        tools::EMPTY_TEXT_BLOCK_JSON_BYTES,
        user_question::UserQuestionBroker,
    };

    fn goal_request(name: &str, arguments: serde_json::Value) -> ToolExecutionRequest {
        goal_request_as(name, arguments, GoalToolCaller::Untrusted)
    }

    fn goal_request_as(
        name: &str,
        arguments: serde_json::Value,
        caller: GoalToolCaller,
    ) -> ToolExecutionRequest {
        let raw_arguments = serde_json::to_string(&arguments).unwrap();
        ToolExecutionRequest::new(
            CallId::new(format!("call-{name}")),
            name.to_owned(),
            raw_arguments,
            JsonValue::new(arguments).unwrap(),
            ToolDispatchBinding::with_goal_caller(caller),
        )
    }

    fn goal_result_json(result: &crate::agent::ToolExecutionResult) -> serde_json::Value {
        let ContentBlockKind::Text { text } = result.content()[0].kind() else {
            panic!("Goal result should contain one text block")
        };
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn normalized_content_budget_accepts_the_exact_json_limit_and_rejects_one_more() {
        let exact = "x".repeat(MAX_TOOL_CONTENT_BYTES - EMPTY_TEXT_BLOCK_JSON_BYTES);
        let accepted = normalize_success(exact).unwrap();
        assert!(!accepted.is_error());
        assert_eq!(
            accepted.content()[0].raw().encoded_len(),
            MAX_TOOL_CONTENT_BYTES
        );

        let one_over = "x".repeat(MAX_TOOL_CONTENT_BYTES - EMPTY_TEXT_BLOCK_JSON_BYTES + 1);
        let rejected = normalize_success(one_over).unwrap();
        assert!(rejected.is_error());
        assert_eq!(
            rejected.error().map(|error| error.code.as_str()),
            Some("TOOL_OUTPUT_LIMIT")
        );
    }

    #[test]
    fn user_question_schema_and_arguments_are_closed_and_bounded() {
        let schema = build_user_question_schema().unwrap();
        assert_eq!(schema.name(), "ask_user_question");
        let parameters = schema.parameters().as_value();
        assert_eq!(parameters["additionalProperties"], false);
        assert_eq!(parameters["properties"]["questions"]["minItems"], 1);
        assert_eq!(parameters["properties"]["questions"]["maxItems"], 3);
        assert_eq!(
            parameters["properties"]["questions"]["items"]["properties"]["multi_select"]["enum"],
            serde_json::json!([false])
        );

        let valid = serde_json::json!({
            "questions": [{
                "id": "mode",
                "header": "Choose mode",
                "question": "Which mode should I use?",
                "options": [
                    { "label": "Safe (Recommended)", "description": "Keep every guard." },
                    { "label": "Fast" }
                ],
                "multi_select": false
            }]
        });
        let parsed = parse_user_question_arguments(&valid).unwrap();
        assert_eq!(parsed.questions()[0].header(), Some("Choose mode"));
        assert_eq!(parsed.questions()[0].options()[1].label(), "Fast");

        for invalid in [
            serde_json::json!({ "questions": [] }),
            serde_json::json!({ "questions": [
                valid["questions"][0].clone(),
                {"id":"two","question":"Two?","options":[{"label":"A"},{"label":"B"}]},
                {"id":"three","question":"Three?","options":[{"label":"A"},{"label":"B"}]},
                {"id":"four","question":"Four?","options":[{"label":"A"},{"label":"B"}]}
            ] }),
            serde_json::json!({ "questions": [
                {"id":"same","question":"One?","options":[{"label":"A"},{"label":"B"}]},
                {"id":"same","question":"Two?","options":[{"label":"A"},{"label":"B"}]}
            ] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?", "options": [{"label":"Only"}] }] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?", "options": [{"label":"Same"},{"label":"Same"}] }] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?", "options": [{"label":"A"},{"label":"B"}], "multi_select": true }] }),
            serde_json::json!({ "questions": [{ "id": " mode ", "question": "Choose?", "options": [{"label":"A"},{"label":"B"}] }] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?\n", "options": [{"label":"A"},{"label":"B"}] }] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?", "options": [{"label":"A"},{"label":"B","extra":true}] }] }),
            serde_json::json!({ "questions": [{ "id": "mode", "question": "Choose?", "options": [{"label":"A"},{"label":"B"}] }], "extra": true }),
        ] {
            assert!(
                parse_user_question_arguments(&invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[tokio::test]
    async fn user_question_returns_the_selected_label_as_compact_json() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let request = goal_request(
            "ask_user_question",
            serde_json::json!({
                "questions": [{
                    "id": "mode",
                    "question": "Which mode?",
                    "options": [{"label":"Safe"},{"label":"Fast"}]
                }]
            }),
        );
        let future = prepare_user_question(Some(broker), &request, CancellationToken::new());
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        receiver.try_recv().unwrap().answer(vec![1]).unwrap();
        let prepared = future.await.unwrap();
        let ToolPreparation::Complete(result) = prepared else {
            panic!("user question should settle as an ordinary result")
        };
        assert!(!result.is_error());
        let ContentBlockKind::Text { text } = result.content()[0].kind() else {
            panic!("user question should render compact JSON text")
        };
        assert_eq!(text, r#"{"answers":[{"id":"mode","selected":["Fast"]}]}"#);
    }

    #[test]
    fn goal_tool_schemas_and_dispatch_are_closed_and_share_one_runtime() {
        let schemas = build_goal_schemas().unwrap();
        assert_eq!(
            schemas
                .iter()
                .map(|schema| schema.name())
                .collect::<Vec<_>>(),
            ["get_goal", "create_goal", "update_goal"]
        );
        assert!(
            schemas
                .iter()
                .all(|schema| { schema.parameters().as_value()["additionalProperties"] == false })
        );

        let goal = GoalRuntime::new();
        let create = goal_request(
            "create_goal",
            serde_json::json!({ "objective": "finish the feature" }),
        );
        let created = dispatch_goal(Some(goal.clone()), &create).unwrap();
        assert!(!created.is_error());
        assert!(matches!(
            created.content()[0].kind(),
            ContentBlockKind::Text { text } if text.contains("\"revision\":1")
        ));
        let goal_id = goal.snapshot().unwrap().unwrap().id().to_owned();

        let update = goal_request(
            "update_goal",
            serde_json::json!({
                "goal_id": goal_id,
                "revision": 1,
                "action": "complete"
            }),
        );
        let completed = dispatch_goal(Some(goal.clone()), &update).unwrap();
        assert!(!completed.is_error());
        assert!(matches!(
            completed.content()[0].kind(),
            ContentBlockKind::Text { text } if text.contains("\"phase\":\"complete\"")
        ));

        let invalid = goal_request("get_goal", serde_json::json!({ "unexpected": true }));
        let rejected = dispatch_goal(Some(goal), &invalid).unwrap();
        assert!(rejected.is_error());
        assert_eq!(
            rejected.error().map(|error| error.code.as_str()),
            Some("INVALID_ARGS")
        );
    }

    #[test]
    fn goal_tool_authority_distinguishes_human_goal_round_and_untrusted_turns() {
        let goal = GoalRuntime::new();
        let rejected_create = prepare_goal(
            Some(goal.clone()),
            &goal_request_as(
                "create_goal",
                serde_json::json!({ "objective": "forged" }),
                GoalToolCaller::GoalRound,
            ),
        )
        .unwrap();
        let ToolPreparation::Complete(rejected_create) = rejected_create else {
            panic!("a Goal round must not create a Goal")
        };
        assert_eq!(
            rejected_create.error().map(|error| error.code.as_str()),
            Some("GOAL_TOOL_AUTHORITY_REQUIRED")
        );
        assert!(goal.snapshot().unwrap().is_none());

        let created = prepare_goal(
            Some(goal.clone()),
            &goal_request_as(
                "create_goal",
                serde_json::json!({ "objective": "authorized" }),
                GoalToolCaller::DirectHuman,
            ),
        )
        .unwrap();
        let ToolPreparation::Goal(created) = created else {
            panic!("a direct human turn may prepare Goal creation")
        };
        created.commit().unwrap();
        let goal_id = goal.snapshot().unwrap().unwrap().id().to_owned();

        let rejected_edit = prepare_goal(
            Some(goal.clone()),
            &goal_request_as(
                "update_goal",
                serde_json::json!({
                    "goal_id": goal_id,
                    "revision": 1,
                    "action": "edit",
                    "objective": "forged edit"
                }),
                GoalToolCaller::GoalRound,
            ),
        )
        .unwrap();
        let ToolPreparation::Complete(rejected_edit) = rejected_edit else {
            panic!("a Goal round must not edit its own objective")
        };
        assert_eq!(
            rejected_edit.error().map(|error| error.code.as_str()),
            Some("GOAL_TOOL_AUTHORITY_REQUIRED")
        );

        let completion = prepare_goal(
            Some(goal),
            &goal_request_as(
                "update_goal",
                serde_json::json!({
                    "goal_id": goal_id,
                    "revision": 1,
                    "action": "complete"
                }),
                GoalToolCaller::GoalRound,
            ),
        )
        .unwrap();
        assert!(matches!(completion, ToolPreparation::Goal(_)));
    }

    #[test]
    fn block_threshold_applies_to_goal_rounds_but_not_direct_human_turns() {
        let round_goal = GoalRuntime::new();
        round_goal.create("round blocker".to_owned()).unwrap();
        let round_goal_id = round_goal.snapshot().unwrap().unwrap().id().to_owned();
        let early = prepare_goal(
            Some(round_goal.clone()),
            &goal_request_as(
                "update_goal",
                serde_json::json!({
                    "goal_id": round_goal_id,
                    "revision": 1,
                    "action": "blocked",
                    "blocked_reason": "credential still unavailable"
                }),
                GoalToolCaller::GoalRound,
            ),
        )
        .unwrap();
        let ToolPreparation::Complete(early) = early else {
            panic!("an early Goal-round block must be rejected")
        };
        assert_eq!(
            early.error().map(|error| error.code.as_str()),
            Some("GOAL_TOOL_BLOCK_THRESHOLD")
        );
        assert_eq!(round_goal.snapshot().unwrap().unwrap().revision(), 1);

        let human_goal = GoalRuntime::new();
        human_goal.create("human blocker".to_owned()).unwrap();
        let human_goal_id = human_goal.snapshot().unwrap().unwrap().id().to_owned();
        let early_human = prepare_goal(
            Some(human_goal),
            &goal_request_as(
                "update_goal",
                serde_json::json!({
                    "goal_id": human_goal_id,
                    "revision": 1,
                    "action": "blocked",
                    "blocked_reason": "user requested a prerequisite pause"
                }),
                GoalToolCaller::DirectHuman,
            ),
        )
        .unwrap();
        assert!(matches!(early_human, ToolPreparation::Goal(_)));
    }

    #[test]
    fn official_goal_contract_checks_exact_ref_cap_and_blocker_shape() {
        let schemas = build_goal_schemas().unwrap();
        assert_eq!(
            schemas[1].parameters().as_value()["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["max_goal_rounds", "objective"]
        );
        assert_eq!(
            schemas[2].parameters().as_value()["required"],
            serde_json::json!(["goal_id", "revision", "action"])
        );

        let goal = GoalRuntime::new();
        let created = dispatch_goal(
            Some(goal.clone()),
            &goal_request(
                "create_goal",
                serde_json::json!({
                    "objective": "bounded contract",
                    "max_goal_rounds": 5
                }),
            ),
        )
        .unwrap();
        let created_json = goal_result_json(&created);
        let goal_id = created_json["goal"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created_json["activation"], "armed");
        assert_eq!(created_json["goal"]["maxGoalRounds"], 5);

        let wrong_id = dispatch_goal(
            Some(goal.clone()),
            &goal_request(
                "update_goal",
                serde_json::json!({
                    "goal_id": "goal-wrong",
                    "revision": 1,
                    "action": "edit",
                    "objective": "must not commit"
                }),
            ),
        )
        .unwrap();
        assert_eq!(
            wrong_id.error().map(|error| error.code.as_str()),
            Some("GOAL_STALE_REVISION")
        );
        assert_eq!(goal.snapshot().unwrap().unwrap().revision(), 1);

        let edited = dispatch_goal(
            Some(goal.clone()),
            &goal_request(
                "update_goal",
                serde_json::json!({
                    "goal_id": goal_id,
                    "revision": 1,
                    "action": "edit",
                    "objective": "",
                    "max_goal_rounds": 2,
                    "blocked_reason": ""
                }),
            ),
        )
        .unwrap();
        let edited_json = goal_result_json(&edited);
        assert_eq!(edited_json["goal"]["objective"], "bounded contract");
        assert_eq!(edited_json["goal"]["maxGoalRounds"], 2);

        let blocked = dispatch_goal(
            Some(goal),
            &goal_request(
                "update_goal",
                serde_json::json!({
                    "goal_id": goal_id,
                    "revision": 2,
                    "action": "blocked",
                    "blocked_reason": "  credential unavailable  "
                }),
            ),
        )
        .unwrap();
        let blocked_json = goal_result_json(&blocked);
        assert_eq!(blocked_json["goal"]["phase"], "blocked");
        assert_eq!(
            blocked_json["goal"]["blockedReason"],
            serde_json::json!({
                "code": "model-reported",
                "message": "credential unavailable"
            })
        );
    }
}
