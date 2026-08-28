mod actor;
mod config;
mod framing;
mod protocol;
mod render;

pub(crate) use config::{LspConfig, LspConfigError};

use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{ToolExecutionResult, ToolExecutorError},
    model::{ContentBlock, JsonValue, ToolSchema},
    workspace_authority::WorkspaceAuthority,
};

use self::{
    actor::{LspActor, LspActorOutcome, LspActorQuery, LspStop},
    render::{file_uri, parse_arguments, render_result},
};
use super::{
    error::{ToolCallError, ToolCallResult, ToolRegistryBuildError},
    process::ProcessRunner,
    workspace::Workspace,
};

pub(crate) const LSP_TOOL_NAME: &str = "lsp";
pub(crate) const LSP_PROMPT_TEXT: &str = "Use search/read for ordinary navigation. Use lsp when textual matches are ambiguous or before a change requires precise definitions, implementations, or references. Positions are one-based line and character (UTF-16) at the cursor; an off-symbol position may return no results. findReferences always includes the declaration.";
const MAX_DOCUMENT_BYTES: usize = 4_000_000;

#[derive(Clone)]
struct Route {
    actor: Arc<LspActor>,
    language_id: String,
}

pub(crate) struct LspHost {
    actors: Box<[Arc<LspActor>]>,
    routes: BTreeMap<String, Route>,
    workspace: Arc<Workspace>,
    tool_timeout: Duration,
}

impl std::fmt::Debug for LspHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspHost")
            .field("server_count", &self.actors.len())
            .field("route_count", &self.routes.len())
            .field("workspace_capability", &true)
            .finish()
    }
}

impl Drop for LspHost {
    fn drop(&mut self) {
        for actor in &self.actors {
            actor.request_shutdown();
        }
    }
}

impl LspHost {
    pub(crate) fn start(
        config: LspConfig,
        runner: ProcessRunner,
        authority: WorkspaceAuthority,
        workspace: Arc<Workspace>,
        environment: &[(OsString, OsString)],
    ) -> Result<Self, ()> {
        let (servers, tool_timeout) = config.into_parts();
        let mut actors: Vec<Arc<LspActor>> = Vec::new();
        let mut routes = BTreeMap::new();
        for server in Vec::from(servers) {
            let extensions = server.extensions().to_vec();
            let actor =
                match LspActor::start(server, runner.clone(), authority.clone(), environment) {
                    Ok(actor) => actor,
                    Err(()) => {
                        for actor in &actors {
                            let _ = actor.shutdown_blocking();
                        }
                        return Err(());
                    }
                };
            for (extension, language_id) in extensions {
                routes.insert(
                    extension,
                    Route {
                        actor: Arc::clone(&actor),
                        language_id,
                    },
                );
            }
            actors.push(actor);
        }
        Ok(Self {
            actors: actors.into_boxed_slice(),
            routes,
            workspace,
            tool_timeout,
        })
    }

    pub(crate) async fn execute(
        &self,
        arguments: &Value,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionResult, ToolExecutorError> {
        let child = cancellation.child_token();
        let deadline = Instant::now() + self.tool_timeout;
        let work = self.execute_inner(arguments, child.clone(), deadline);
        tokio::pin!(work);
        let timeout = tokio::time::sleep(self.tool_timeout);
        tokio::pin!(timeout);
        tokio::select! {
            biased;
            result = &mut work => match result {
                Ok(text) => normalize_text_result(text),
                Err(error) => error.into_execution_result(),
            },
            _ = &mut timeout => {
                child.cancel();
                let _ = work.await;
                lsp_error("LspTimeoutError", "LSP_TIMEOUT", "language-server query exceeded its configured timeout")
            }
        }
    }

    async fn execute_inner(
        &self,
        arguments: &Value,
        cancellation: CancellationToken,
        deadline: Instant,
    ) -> ToolCallResult<String> {
        if cancellation.is_cancelled() {
            return Err(ToolCallError::aborted());
        }
        let parsed = parse_arguments(arguments).map_err(ToolCallError::invalid_args)?;
        let extension = final_extension(&parsed.file_path);
        let route = self.routes.get(&extension).ok_or_else(|| {
            ToolCallError::model(
                "LspError",
                "LSP_UNAVAILABLE",
                format!(
                    "no configured language server handles {:?}",
                    parsed.file_path
                ),
            )
        })?;
        let path = self.workspace.resolve(&parsed.file_path)?;
        let source = self
            .workspace
            .read_file_without_symlinks(&path, MAX_DOCUMENT_BYTES, &cancellation)
            .await?;
        let text =
            String::from_utf8(source.bytes).map_err(|_| ToolCallError::not_text(&path.display))?;
        if cancellation.is_cancelled() {
            return Err(ToolCallError::aborted());
        }
        let source_path = self.workspace.display_root().join(&path.relative);
        let uri = file_uri(&source_path).ok_or_else(|| {
            ToolCallError::model(
                "LspError",
                "LSP_UNAVAILABLE",
                "the workspace source could not be represented as a file URI",
            )
        })?;
        let outcome = route
            .actor
            .query(LspActorQuery {
                operation: parsed.operation,
                position: parsed.position,
                language_id: route.language_id.clone(),
                uri,
                text,
                cancellation,
                deadline,
            })
            .await;
        match outcome {
            LspActorOutcome::Success(result) => {
                Ok(render_result(&result, self.workspace.display_root()))
            }
            LspActorOutcome::Unsupported => Err(ToolCallError::model(
                "LspError",
                "LSP_UNSUPPORTED_OPERATION",
                format!(
                    "the configured language server does not support {} with transient document open/close and UTF-16 positions",
                    parsed.operation.as_str()
                ),
            )),
            LspActorOutcome::MalformedResponse => Err(ToolCallError::model(
                "LspError",
                "LSP_MALFORMED_RESPONSE",
                "the language server returned a malformed result",
            )),
            LspActorOutcome::Protocol => Err(ToolCallError::model(
                "LspError",
                "LSP_PROTOCOL_ERROR",
                "the language server rejected or violated the query protocol",
            )),
            LspActorOutcome::Process => Err(ToolCallError::model(
                "LspError",
                "LSP_PROCESS_FAILED",
                "the configured language-server process was unavailable",
            )),
            LspActorOutcome::Busy => Err(ToolCallError::model(
                "LspError",
                "LSP_BUSY",
                "the configured language server already has too many queued queries",
            )),
            LspActorOutcome::Stopped(LspStop::Cancelled) => Err(ToolCallError::aborted()),
            LspActorOutcome::Stopped(LspStop::Timeout) => Err(ToolCallError::model(
                "LspTimeoutError",
                "LSP_TIMEOUT",
                "language-server query exceeded its configured timeout",
            )),
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ()> {
        for actor in &self.actors {
            actor.request_shutdown();
        }
        let mut clean = true;
        for actor in &self.actors {
            clean &= actor.shutdown().await;
        }
        if clean { Ok(()) } else { Err(()) }
    }
}

pub(crate) fn schema() -> Result<ToolSchema, ToolRegistryBuildError> {
    let parameters = JsonValue::new(json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["goToDefinition", "findReferences", "goToImplementation", "hover"],
                "description": "goToDefinition, findReferences, goToImplementation, or hover"
            },
            "file_path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Source file relative to the workspace or an absolute path inside it"
            },
            "line": {
                "type": "integer",
                "minimum": 1,
                "maximum": 4294967295_u64,
                "description": "One-based source line"
            },
            "character": {
                "type": "integer",
                "minimum": 1,
                "maximum": 4294967295_u64,
                "description": "One-based UTF-16 column"
            }
        },
        "required": ["operation", "file_path", "line", "character"],
        "additionalProperties": false
    }))
    .map_err(|source| ToolRegistryBuildError::InvalidSchema {
        tool: LSP_TOOL_NAME,
        source: source.into(),
    })?;
    ToolSchema::new(
        LSP_TOOL_NAME,
        "Query a configured language server for precise definitions, references, implementations, or hover information.",
        parameters,
    )
    .map_err(|source| ToolRegistryBuildError::InvalidSchema {
        tool: LSP_TOOL_NAME,
        source,
    })
}

fn final_extension(path: &str) -> String {
    let basename = path.rsplit(['/', '\\']).next().unwrap_or_default();
    let Some(dot) = basename.rfind('.') else {
        return String::new();
    };
    if dot == 0 {
        return String::new();
    }
    basename[dot..].to_ascii_lowercase()
}

fn normalize_text_result(text: String) -> Result<ToolExecutionResult, ToolExecutorError> {
    let block = ContentBlock::text(text)
        .map_err(|_| ToolExecutorError::new("LSP result normalization failed"))?;
    ToolExecutionResult::success(vec![block])
        .map_err(|_| ToolExecutorError::new("LSP result normalization failed"))
}

fn lsp_error(
    name: &'static str,
    code: &'static str,
    message: &'static str,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    ToolCallError::model(name, code, message).into_execution_result()
}

#[cfg(test)]
mod tests {
    use super::{final_extension, schema};
    use crate::tools::workspace::Workspace;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn extension_selection_matches_the_fixed_final_extension_rule() {
        assert_eq!(final_extension("src/Foo.TS"), ".ts");
        assert_eq!(final_extension("a/foo.d.ts"), ".ts");
        assert_eq!(final_extension(r"C:\\work\\Main.CS"), ".cs");
        assert_eq!(final_extension("Makefile"), "");
        assert_eq!(final_extension(".bashrc"), "");
    }

    #[test]
    fn schema_is_closed_and_matches_the_fixed_operation_catalogue() {
        let schema = schema().unwrap();
        let parameters = schema.parameters().as_value();
        assert_eq!(schema.name(), "lsp");
        assert_eq!(
            parameters["properties"]["operation"]["enum"],
            serde_json::json!([
                "goToDefinition",
                "findReferences",
                "goToImplementation",
                "hover"
            ])
        );
        assert_eq!(
            parameters["required"],
            serde_json::json!(["operation", "file_path", "line", "character"])
        );
        assert_eq!(parameters["additionalProperties"], false);
    }

    #[test]
    fn fixed_source_fixture_matches_the_rust_schema_and_limits() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/tools/upstream_phase37_lsp.json"
        ))
        .unwrap();
        let schema = schema().unwrap();
        assert_eq!(fixture["tool"]["name"], schema.name());
        assert_eq!(
            fixture["tool"]["operations"],
            schema.parameters().as_value()["properties"]["operation"]["enum"]
        );
        assert_eq!(
            fixture["tool"]["required"],
            schema.parameters().as_value()["required"]
        );
        assert_eq!(fixture["tool"]["maxLocations"], 100);
        assert_eq!(fixture["tool"]["maxResultCharacters"], 16_000);
    }

    #[tokio::test]
    async fn lsp_source_reads_reject_even_a_final_workspace_symlink() {
        let root = std::env::temp_dir().join(format!("dsh-lsp-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("real.rs"), "fn target() {}\n").unwrap();
        std::os::unix::fs::symlink("real.rs", root.join("link.rs")).unwrap();
        let workspace = Workspace::open(&root).unwrap();
        let path = workspace.resolve("link.rs").unwrap();
        let result = workspace
            .read_file_without_symlinks(&path, 4_000_000, &CancellationToken::new())
            .await;
        assert!(result.is_err_and(|error| error.has_code("WORKSPACE_PATH_DENIED")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
