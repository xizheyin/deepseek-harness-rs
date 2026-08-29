use std::{fs, os::unix::fs::PermissionsExt as _, sync::Arc};

use futures_util::stream;
use tokio_util::sync::CancellationToken;

use super::{AgentLoop, AgentLoopConfig, ManualSessionForkOutcome, NoTools};
use crate::{
    model::{ContentBlock, LlmCallConfig, Message, MessageSource},
    provider::{
        ModelProvider, PreparedProviderCall, PreparedRequestPreflight, ProviderPreflightError,
        ProviderPrepareError, ProviderRequest, ProviderRequestDraft, ProviderStream,
    },
    session::{
        EventKind, NewEvent, SessionForker, SessionId, SessionStore, SurfaceIntent, SystemClock,
        TurnEndReason, TurnId,
    },
    workspace_authority::WorkspaceAuthority,
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
async fn fork_creates_a_resumable_child_and_keeps_the_parent_usable() {
    let root = private_dir("agent-fork-store");
    let workspace = private_dir("agent-fork-workspace");
    let store = SessionStore::open_existing(&root).unwrap();
    let authority = WorkspaceAuthority::open(&workspace).unwrap();
    let parent = SessionId::new("session-550e8400-e29b-41d4-a716-446655440000");
    let child = SessionId::new("session-550e8400-e29b-41d4-a716-446655440001");
    let mut session = store
        .prepare_new(parent.clone(), &authority, SystemClock)
        .unwrap();
    session.materialize_if_needed().await.unwrap();
    session
        .append_settled(NewEvent::log(EventKind::turn_start(
            TurnId::new(1).unwrap(),
        )))
        .await
        .unwrap();
    let message = Message::user(
        "fork-user",
        vec![ContentBlock::text("try one direction").unwrap()],
        MessageSource::user().unwrap(),
    )
    .unwrap();
    session
        .append_settled(NewEvent::surface(
            EventKind::user_message(message),
            SurfaceIntent::append(),
        ))
        .await
        .unwrap();
    session
        .append_settled(NewEvent::log(EventKind::turn_end(
            TurnId::new(1).unwrap(),
            TurnEndReason::Completed,
        )))
        .await
        .unwrap();
    let mut agent = agent(session);
    assert_eq!(
        agent.rename_session_title("Try direction").await.unwrap(),
        Some("Try direction".to_owned())
    );
    let forker = SessionForker::new(&store, &authority).unwrap();

    let outcome = agent
        .fork_session(&forker, child.clone(), None, CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        ManualSessionForkOutcome::Forked {
            child: forked,
            seed_events: 4,
            bytes: _,
        } if forked == child
    ));
    assert_eq!(
        agent
            .rename_session_title("Parent continues")
            .await
            .unwrap(),
        Some("Parent continues".to_owned())
    );
    agent.shutdown().await.unwrap();

    let child_path = root.join(format!("{child}.jsonl"));
    assert_eq!(
        fs::metadata(&child_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut preparing = store
        .begin_resume(child.clone(), Some(workspace.clone()), SystemClock)
        .unwrap();
    preparing.wait_ready().await.unwrap();
    let mut prepared = preparing.finish().unwrap();
    prepared.begin_commit().unwrap();
    prepared.wait_commit().await.unwrap();
    let recovered = prepared.finish_commit().unwrap();
    let (mut child_session, _) = recovered.into_parts();
    assert_eq!(child_session.header().parent_session(), Some(&parent));
    assert_eq!(child_session.header().seed_length(), Some(4));
    assert_eq!(
        child_session
            .state()
            .session_title()
            .map(|title| title.title()),
        Some("Try direction (1)")
    );
    assert_eq!(child_session.visible_messages().len(), 1);
    child_session.shutdown().await.unwrap();

    fs::remove_file(root.join(format!("{parent}.jsonl"))).unwrap();
    fs::remove_file(child_path).unwrap();
    fs::remove_dir(root).unwrap();
    fs::remove_dir(workspace).unwrap();
}

#[tokio::test]
async fn fork_rejects_no_completed_turn_and_pre_cancelled_work_without_a_child() {
    let root = private_dir("agent-fork-empty-store");
    let workspace = private_dir("agent-fork-empty-workspace");
    let store = SessionStore::open_existing(&root).unwrap();
    let authority = WorkspaceAuthority::open(&workspace).unwrap();
    let parent = SessionId::new("session-550e8400-e29b-41d4-a716-446655440010");
    let mut empty = agent(
        store
            .prepare_new(parent.clone(), &authority, SystemClock)
            .unwrap(),
    );
    let forker = SessionForker::new(&store, &authority).unwrap();
    let unavailable_child = SessionId::new("session-550e8400-e29b-41d4-a716-446655440011");
    assert_eq!(
        empty
            .fork_session(
                &forker,
                unavailable_child.clone(),
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap(),
        ManualSessionForkOutcome::Unavailable
    );
    assert!(!root.join(format!("{unavailable_child}.jsonl")).exists());

    let cancelled_child = SessionId::new("session-550e8400-e29b-41d4-a716-446655440012");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        empty
            .fork_session(&forker, cancelled_child.clone(), None, cancellation)
            .await
            .unwrap(),
        ManualSessionForkOutcome::Cancelled
    );
    assert!(!root.join(format!("{cancelled_child}.jsonl")).exists());
    empty.shutdown().await.unwrap();

    fs::remove_file(root.join(format!("{parent}.jsonl"))).unwrap();
    fs::remove_dir(root).unwrap();
    fs::remove_dir(workspace).unwrap();
}

fn agent(session: crate::session::Session) -> AgentLoop {
    AgentLoop::new(
        session,
        Arc::new(NeverProvider),
        Arc::new(NoTools),
        AgentLoopConfig::new(LlmCallConfig::new("never", "model").unwrap()),
    )
    .unwrap()
}

fn private_dir(label: &str) -> std::path::PathBuf {
    let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
    let path = parent.join(format!("dsh-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
