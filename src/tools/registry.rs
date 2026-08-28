use std::{path::Path, sync::Arc};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{
        ToolExecutionFuture, ToolExecutionRequest, ToolExecutionResult, ToolExecutor,
        ToolExecutorError, ToolShutdownFuture,
    },
    goal::{GoalError, GoalRuntime, GoalUpdate, MAX_GOAL_OBJECTIVE_BYTES},
    model::{ContentBlock, JsonValue, ToolSchema},
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
use crate::agent::{ToolPreparation, ToolPreparationFuture};

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
        Ok(Self {
            workspace,
            schemas: schemas.into(),
            environment,
            runner,
            plugins: None,
            goal,
        })
    }

    pub(crate) async fn from_authority_with_plugins(
        authority: WorkspaceAuthority,
        config: PluginConfig,
        cancellation: CancellationToken,
        goal: Option<GoalRuntime>,
    ) -> Result<Self, ToolRegistryBuildError> {
        let mut registry = Self::from_authority_with_goal(authority, goal)?;
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
        Box::pin(async move {
            if is_goal_tool(request.name()) {
                return prepare_goal(goal, &request);
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
                serde_json::to_string(&json!({
                    "goal": snapshot.map(|goal| goal.to_value()),
                }))
                .map_err(|_| {
                    ToolCallError::model(
                        "GoalError",
                        "GOAL_UNAVAILABLE",
                        "Goal state could not be encoded",
                    )
                })
            }),
        "create_goal" => parse_create_goal_arguments(request.arguments().as_value())
            .and_then(|objective| goal.create(objective).map_err(goal_call_error))
            .and_then(|snapshot| {
                serde_json::to_string(&json!({ "goal": snapshot.to_value() })).map_err(|_| {
                    ToolCallError::model(
                        "GoalError",
                        "GOAL_UNAVAILABLE",
                        "Goal state could not be encoded",
                    )
                })
            }),
        "update_goal" => parse_update_goal_arguments(request.arguments().as_value())
            .and_then(|(revision, operation, objective)| {
                goal.update(revision, operation, objective)
                    .map_err(goal_call_error)
            })
            .and_then(|snapshot| {
                serde_json::to_string(&json!({ "goal": snapshot.to_value() })).map_err(|_| {
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
    let prepared = match request.name() {
        "create_goal" => parse_create_goal_arguments(request.arguments().as_value())
            .and_then(|objective| goal.prepare_create(objective).map_err(goal_call_error)),
        "update_goal" => parse_update_goal_arguments(request.arguments().as_value()).and_then(
            |(revision, operation, objective)| {
                goal.prepare_update(revision, operation, objective)
                    .map_err(goal_call_error)
            },
        ),
        _ => Err(ToolCallError::unknown_tool()),
    };
    match prepared {
        Ok(mutation) => Ok(ToolPreparation::Goal(mutation)),
        Err(error) => Ok(ToolPreparation::Complete(error.into_execution_result()?)),
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

fn parse_create_goal_arguments(arguments: &serde_json::Value) -> ToolCallResult<String> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("create_goal arguments must be an object"))?;
    if fields.len() != 1 {
        return Err(ToolCallError::invalid_args(
            "create_goal accepts exactly objective",
        ));
    }
    fields
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolCallError::invalid_args("objective must be a string"))
}

fn parse_update_goal_arguments(
    arguments: &serde_json::Value,
) -> ToolCallResult<(u64, GoalUpdate, Option<String>)> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| ToolCallError::invalid_args("update_goal arguments must be an object"))?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "expected_revision" | "operation" | "objective"
        )
    }) {
        return Err(ToolCallError::invalid_args(
            "update_goal received an unknown argument",
        ));
    }
    let revision = fields
        .get("expected_revision")
        .and_then(serde_json::Value::as_u64)
        .filter(|revision| *revision != 0)
        .ok_or_else(|| {
            ToolCallError::invalid_args("expected_revision must be a positive integer")
        })?;
    let operation = fields
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .and_then(GoalUpdate::parse)
        .ok_or_else(|| ToolCallError::invalid_args("operation is not supported"))?;
    let objective = match fields.get("objective") {
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| ToolCallError::invalid_args("objective must be a string"))?
                .to_owned(),
        ),
        None => None,
    };
    Ok((revision, operation, objective))
}

fn goal_call_error(error: GoalError) -> ToolCallError {
    let code = match &error {
        GoalError::Missing => "GOAL_MISSING",
        GoalError::Unfinished => "GOAL_UNFINISHED",
        GoalError::EmptyObjective | GoalError::ObjectiveTooLarge => "GOAL_INVALID_OBJECTIVE",
        GoalError::StaleRevision => "GOAL_STALE_REVISION",
        GoalError::InvalidTransition => "GOAL_INVALID_TRANSITION",
        GoalError::BlockThreshold => "GOAL_BLOCK_THRESHOLD",
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
            "Read the current process-local Goal, including its revision, phase, activation, and round limits.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )?,
        schema(
            "create_goal",
            "Create and arm one process-local Goal when no unfinished Goal exists.",
            json!({
                "type": "object",
                "properties": {
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_GOAL_OBJECTIVE_BYTES,
                        "description": "Concrete nonblank objective for bounded automatic continuation"
                    }
                },
                "required": ["objective"],
                "additionalProperties": false
            }),
        )?,
        schema(
            "update_goal",
            "Edit, pause, resume, complete, or report a repeated blocker for the current Goal using its exact revision.",
            json!({
                "type": "object",
                "properties": {
                    "expected_revision": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["edit", "pause", "resume", "complete", "blocked"]
                    },
                    "objective": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_GOAL_OBJECTIVE_BYTES,
                        "description": "Required only when operation is edit"
                    }
                },
                "required": ["expected_revision", "operation"],
                "additionalProperties": false
            }),
        )?,
    ])
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
    use super::{MAX_TOOL_CONTENT_BYTES, build_goal_schemas, dispatch_goal, normalize_success};
    use crate::{
        agent::{ToolDispatchBinding, ToolExecutionRequest},
        goal::GoalRuntime,
        model::{CallId, ContentBlockKind, JsonValue},
        tools::EMPTY_TEXT_BLOCK_JSON_BYTES,
    };

    fn goal_request(name: &str, arguments: serde_json::Value) -> ToolExecutionRequest {
        let raw_arguments = serde_json::to_string(&arguments).unwrap();
        ToolExecutionRequest::new(
            CallId::new(format!("call-{name}")),
            name.to_owned(),
            raw_arguments,
            JsonValue::new(arguments).unwrap(),
            ToolDispatchBinding::new(),
        )
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

        let update = goal_request(
            "update_goal",
            serde_json::json!({
                "expected_revision": 1,
                "operation": "complete"
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
}
