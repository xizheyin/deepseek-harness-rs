use std::{fs::OpenOptions, path::PathBuf, sync::Arc};

use futures_util::stream;
use tokio_util::sync::CancellationToken;

use super::{AgentLoop, AgentLoopConfig, AgentLoopError, ManualSessionExportOutcome, NoTools};
use crate::{
    model::LlmCallConfig,
    provider::{
        ModelProvider, PreparedProviderCall, PreparedRequestPreflight, ProviderPreflightError,
        ProviderPrepareError, ProviderRequest, ProviderRequestDraft, ProviderStream,
    },
    session::{EventKind, NewEvent, Session, TurnId},
};

struct NeverProvider;

impl ModelProvider for NeverProvider {
    fn prepare_call(
        &self,
        config: LlmCallConfig,
    ) -> Result<PreparedProviderCall, ProviderPrepareError> {
        Err(ProviderPrepareError::WrongProvider {
            expected: "never".to_owned(),
            actual: config.provider().to_owned(),
        })
    }

    fn preflight_request(
        &self,
        draft: ProviderRequestDraft<'_>,
    ) -> Result<PreparedRequestPreflight, ProviderPreflightError> {
        Err(ProviderPreflightError::Preparation(
            ProviderPrepareError::WrongProvider {
                expected: "never".to_owned(),
                actual: draft.config().provider().to_owned(),
            },
        ))
    }

    fn stream(
        &self,
        _request: ProviderRequest,
        _cancellation: CancellationToken,
    ) -> ProviderStream {
        Box::pin(stream::empty())
    }
}

#[tokio::test]
async fn export_is_idle_only_and_pre_cancelled_or_memory_sessions_have_no_success() {
    let mut cancelled_agent = memory_agent("session-export-cancelled");
    let (cancelled_path, cancelled_file) = destination("agent-export-cancelled");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        cancelled_agent
            .export_session_log(cancelled_file, cancellation)
            .await
            .unwrap(),
        ManualSessionExportOutcome::Cancelled
    );
    assert_eq!(std::fs::metadata(&cancelled_path).unwrap().len(), 0);

    let mut in_memory_agent = memory_agent("session-export-memory");
    let (memory_path, memory_file) = destination("agent-export-memory");
    assert_eq!(
        in_memory_agent
            .export_session_log(memory_file, CancellationToken::new())
            .await
            .unwrap(),
        ManualSessionExportOutcome::Failed
    );

    let mut busy_agent = memory_agent("session-export-busy");
    busy_agent
        .session
        .append(NewEvent::log(EventKind::turn_start(
            TurnId::new(1).unwrap(),
        )))
        .unwrap();
    let (busy_path, busy_file) = destination("agent-export-busy");
    assert!(matches!(
        busy_agent
            .export_session_log(busy_file, CancellationToken::new())
            .await,
        Err(AgentLoopError::SessionNotIdle)
    ));

    for path in [cancelled_path, memory_path, busy_path] {
        std::fs::remove_file(path).unwrap();
    }
}

fn memory_agent(id: &str) -> AgentLoop {
    AgentLoop::new(
        Session::new(id).unwrap(),
        Arc::new(NeverProvider),
        Arc::new(NoTools),
        AgentLoopConfig::new(LlmCallConfig::new("never", "model").unwrap()),
    )
    .unwrap()
}

fn destination(label: &str) -> (PathBuf, std::fs::File) {
    let path = std::env::temp_dir().join(format!("dsh-{label}-{}", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    (path, file)
}
