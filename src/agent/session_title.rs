//! Non-blocking first-prompt title refinement owned by one Agent.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

use futures_util::{FutureExt as _, StreamExt as _};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    model::{
        ContentBlock, ContentBlockKind, ContentBlockType, FinishReasonKind, LlmCallConfig, Message,
        MessageSource, NonNegativeSafeInteger, RequestPurpose, StreamChunkKind,
    },
    provider::{ModelProvider, ProviderRequest, ProviderRequestDraft, StreamValidator},
    session::{
        AppendReceipt, EventKind, EventSeq, NewEvent, PROVIDER_TITLE_MAX_BYTES, Session,
        SessionReservation, SessionTitleEvent, SessionTitleLlmRequestEvent, SessionTitleRoute,
        SessionTitleSource, TITLE_OUTPUT_MAX_TOKENS, fallback_title, normalize_title,
        title_prompt_text,
    },
};

use super::{AgentLoopError, ManualTitleRefreshOutcome};

const TITLE_PROVIDER: &str = "session-title-first-prompt-llm";
const TITLE_TIMEOUT: Duration = Duration::from_secs(60);
const TITLE_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);
const MAX_ASSEMBLED_TITLE_BYTES: usize = 16 * 1_024;
const TITLE_SYSTEM: &str = "Create a concise title for an AI coding-assistant session from the supplied human messages.\nReturn only the title on one line, in plain text of natural language, with no quotes, prefix, explanation, Markdown, XML, or terminal control codes. No code is allowed.\nUse the language of the messages.\nAim for about 5 words in non-CJK languages or 10 CJK characters.";

pub(super) struct SessionTitleRuntime {
    provider: Arc<dyn ModelProvider>,
    refinement_enabled: bool,
    state: TitleState,
}

enum TitleState {
    Armed,
    Pending {
        seq: EventSeq,
        text: String,
    },
    Running {
        cancellation: CancellationToken,
        task: JoinHandle<Option<ProviderTitle>>,
    },
    Done,
}

struct ProviderTitle {
    title: String,
    seq: EventSeq,
    provider: String,
    model: String,
}

enum TitleRequestOutcome {
    Accepted(String),
    Cancelled,
    Failed,
}

impl SessionTitleRuntime {
    pub(super) fn new(
        session: &Session,
        provider: Arc<dyn ModelProvider>,
        enabled: bool,
        refinement_enabled: bool,
    ) -> Self {
        let state = if !enabled
            || session.state().session_title().is_some()
            || session.state().session_title_llm_requested()
        {
            TitleState::Done
        } else {
            TitleState::Armed
        };
        Self {
            provider,
            refinement_enabled,
            state,
        }
    }

    pub(super) async fn record_fallback(
        &mut self,
        reservation: &mut SessionReservation<'_>,
        messages: &[Message],
        receipts: &[AppendReceipt],
    ) {
        if !matches!(self.state, TitleState::Armed) || messages.len() != receipts.len() {
            return;
        }
        let Some((seq, text)) = messages
            .iter()
            .zip(receipts)
            .find_map(|(message, receipt)| {
                title_prompt_text(message).map(|text| (receipt.seq(), text))
            })
        else {
            return;
        };
        let Some(fallback) = fallback_title(&text) else {
            return;
        };
        let Ok(event) = SessionTitleEvent::new(fallback, vec![seq], SessionTitleSource::Fallback)
        else {
            return;
        };
        if reservation
            .append_settled(NewEvent::log(EventKind::session_title(event)))
            .await
            .is_ok()
        {
            self.state = TitleState::Pending { seq, text };
        }
    }

    pub(super) async fn start_refinement(
        &mut self,
        reservation: &mut SessionReservation<'_>,
        conversation_config: &LlmCallConfig,
    ) {
        let TitleState::Pending { seq, text } =
            std::mem::replace(&mut self.state, TitleState::Done)
        else {
            return;
        };
        if !self.refinement_enabled {
            return;
        }
        let Ok(max_tokens) = NonNegativeSafeInteger::new(TITLE_OUTPUT_MAX_TOKENS) else {
            return;
        };
        let Ok(config) = conversation_config.with_max_tokens_preserving_extensions(max_tokens)
        else {
            return;
        };
        let Ok(messages) = title_messages(seq, &text) else {
            return;
        };
        let session_id = reservation.session().id().clone();
        let Ok(draft) = title_draft(&config, &messages, &session_id) else {
            return;
        };
        let preflight =
            match catch_unwind(AssertUnwindSafe(|| self.provider.preflight_request(draft))) {
                Ok(Ok(preflight)) => preflight,
                Ok(Err(_)) | Err(_) => return,
            };
        let route = match SessionTitleRoute::new(
            preflight.prepared_call().config().provider(),
            preflight.prepared_call().config().model(),
        ) {
            Ok(route) => route,
            Err(_) => return,
        };
        let request_event = match SessionTitleLlmRequestEvent::new(
            TITLE_PROVIDER,
            vec![seq],
            route.clone(),
            TITLE_SYSTEM,
            messages.clone(),
            max_tokens,
        ) {
            Ok(event) => event,
            Err(_) => return,
        };
        if reservation
            .append_settled(NewEvent::log(EventKind::session_title_llm_request(
                request_event,
            )))
            .await
            .is_err()
        {
            return;
        }
        let request = match title_draft(&config, &messages, &session_id)
            .and_then(|draft| draft.into_request(preflight).map_err(|_| ()))
        {
            Ok(request) => request,
            Err(()) => return,
        };
        let provider = self.provider.clone();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let provider_name = route.provider().to_owned();
        let model = route.model().to_owned();
        let task = tokio::spawn(async move {
            match run_title_request(provider, request, task_cancellation).await {
                TitleRequestOutcome::Accepted(title) => Some(ProviderTitle {
                    title,
                    seq,
                    provider: provider_name,
                    model,
                }),
                TitleRequestOutcome::Cancelled | TitleRequestOutcome::Failed => None,
            }
        });
        self.state = TitleState::Running { cancellation, task };
    }

    pub(super) async fn collect_ready(&mut self, session: &mut Session) {
        let ready = matches!(&self.state, TitleState::Running { task, .. } if task.is_finished());
        if ready {
            self.collect(session, false).await;
        }
    }

    pub(super) async fn shutdown(&mut self, session: &mut Session) {
        self.collect(session, true).await;
    }

    pub(super) async fn rename(
        &mut self,
        session: &mut Session,
        title: String,
    ) -> Result<String, AgentLoopError> {
        self.supersede().await;
        let event = SessionTitleEvent::new(title.clone(), Vec::new(), SessionTitleSource::User)?;
        session
            .append_settled(NewEvent::log(EventKind::session_title(event)))
            .await?;
        Ok(title)
    }

    pub(super) async fn refresh(
        &mut self,
        session: &mut Session,
        conversation_config: &LlmCallConfig,
        seq: EventSeq,
        text: String,
        cancellation: CancellationToken,
    ) -> Result<ManualTitleRefreshOutcome, AgentLoopError> {
        self.supersede().await;
        if cancellation.is_cancelled() {
            return Ok(ManualTitleRefreshOutcome::Cancelled);
        }
        if !self.refinement_enabled {
            return refresh_fallback(session, seq, &text).await;
        }
        if session.state().session_title().is_none() {
            let Some(fallback) = fallback_title(&text) else {
                return Ok(ManualTitleRefreshOutcome::NoHistory);
            };
            let event = SessionTitleEvent::new(fallback, vec![seq], SessionTitleSource::Fallback)?;
            session
                .append_settled(NewEvent::log(EventKind::session_title(event)))
                .await?;
        }
        let Ok(max_tokens) = NonNegativeSafeInteger::new(TITLE_OUTPUT_MAX_TOKENS) else {
            return Ok(ManualTitleRefreshOutcome::Failed);
        };
        let Ok(config) = conversation_config.with_max_tokens_preserving_extensions(max_tokens)
        else {
            return Ok(ManualTitleRefreshOutcome::Failed);
        };
        let Ok(messages) = title_messages(seq, &text) else {
            return Ok(ManualTitleRefreshOutcome::Failed);
        };
        let session_id = session.id().clone();
        let Ok(draft) = title_draft(&config, &messages, &session_id) else {
            return Ok(ManualTitleRefreshOutcome::Failed);
        };
        let preflight =
            match catch_unwind(AssertUnwindSafe(|| self.provider.preflight_request(draft))) {
                Ok(Ok(preflight)) => preflight,
                Ok(Err(_)) | Err(_) => return Ok(ManualTitleRefreshOutcome::Failed),
            };
        let route = SessionTitleRoute::new(
            preflight.prepared_call().config().provider(),
            preflight.prepared_call().config().model(),
        )?;
        let request_event = SessionTitleLlmRequestEvent::new(
            TITLE_PROVIDER,
            vec![seq],
            route.clone(),
            TITLE_SYSTEM,
            messages.clone(),
            max_tokens,
        )?;
        session
            .append_settled(NewEvent::log(EventKind::session_title_llm_request(
                request_event,
            )))
            .await?;
        let request = match title_draft(&config, &messages, &session_id)
            .and_then(|draft| draft.into_request(preflight).map_err(|_| ()))
        {
            Ok(request) => request,
            Err(()) => return Ok(ManualTitleRefreshOutcome::Failed),
        };
        match run_title_request(self.provider.clone(), request, cancellation).await {
            TitleRequestOutcome::Accepted(title) => {
                let event = SessionTitleEvent::new(
                    title.clone(),
                    vec![seq],
                    SessionTitleSource::Provider {
                        provider: route.provider().to_owned(),
                        model: Some(route.model().to_owned()),
                    },
                )?;
                session
                    .append_settled(NewEvent::log(EventKind::session_title(event)))
                    .await?;
                Ok(ManualTitleRefreshOutcome::Refreshed { title })
            }
            TitleRequestOutcome::Cancelled => Ok(ManualTitleRefreshOutcome::Cancelled),
            TitleRequestOutcome::Failed => Ok(ManualTitleRefreshOutcome::Failed),
        }
    }

    async fn supersede(&mut self) {
        let state = std::mem::replace(&mut self.state, TitleState::Done);
        let TitleState::Running {
            cancellation,
            mut task,
        } = state
        else {
            return;
        };
        cancellation.cancel();
        if tokio::time::timeout(TITLE_SHUTDOWN_GRACE, &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }

    async fn collect(&mut self, session: &mut Session, shutdown: bool) {
        let state = std::mem::replace(&mut self.state, TitleState::Done);
        let TitleState::Running {
            cancellation,
            mut task,
        } = state
        else {
            self.state = state;
            return;
        };
        if shutdown && !task.is_finished() {
            cancellation.cancel();
            if tokio::time::timeout(TITLE_SHUTDOWN_GRACE, &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
            return;
        }
        let Ok(Some(candidate)) = task.await else {
            return;
        };
        let Ok(event) = SessionTitleEvent::new(
            candidate.title,
            vec![candidate.seq],
            SessionTitleSource::Provider {
                provider: candidate.provider,
                model: Some(candidate.model),
            },
        ) else {
            return;
        };
        let _ = session
            .append_settled(NewEvent::log(EventKind::session_title(event)))
            .await;
    }
}

impl Drop for SessionTitleRuntime {
    fn drop(&mut self) {
        if let TitleState::Running { cancellation, task } = &self.state {
            cancellation.cancel();
            task.abort();
        }
    }
}

async fn refresh_fallback(
    session: &mut Session,
    seq: EventSeq,
    text: &str,
) -> Result<ManualTitleRefreshOutcome, AgentLoopError> {
    let current = session.state().session_title().cloned();
    if let Some(title) = &current {
        if !matches!(title.source(), SessionTitleSource::User) {
            return Ok(ManualTitleRefreshOutcome::Unchanged {
                title: title.title().to_owned(),
            });
        }
    }
    let Some(title) = fallback_title(text) else {
        return Ok(match current {
            Some(title) => ManualTitleRefreshOutcome::Unchanged {
                title: title.title().to_owned(),
            },
            None => ManualTitleRefreshOutcome::NoHistory,
        });
    };
    let event = SessionTitleEvent::new(title.clone(), vec![seq], SessionTitleSource::Fallback)?;
    session
        .append_settled(NewEvent::log(EventKind::session_title(event)))
        .await?;
    Ok(ManualTitleRefreshOutcome::Refreshed { title })
}

fn title_messages(seq: EventSeq, text: &str) -> Result<Vec<Message>, ()> {
    let framed = serde_json::json!([{ "seq": seq.get(), "text": text }]);
    let framed = serde_json::to_string(&framed).map_err(|_| ())?;
    let prompt =
        format!("Generate the session title from this JSON array of human messages:\n{framed}");
    let block = ContentBlock::text(prompt).map_err(|_| ())?;
    let source = MessageSource::user().map_err(|_| ())?;
    let message = Message::user(
        format!("dsh-session-title-{}", seq.get()),
        vec![block],
        source,
    )
    .map_err(|_| ())?;
    Ok(vec![message])
}

fn title_draft<'a>(
    config: &'a LlmCallConfig,
    messages: &'a [Message],
    session_id: &'a crate::session::SessionId,
) -> Result<ProviderRequestDraft<'a>, ()> {
    ProviderRequestDraft::new(config, messages)
        .and_then(|draft| draft.with_system(TITLE_SYSTEM))
        .map(|draft| draft.with_purpose(RequestPurpose::SessionTitle))
        .and_then(|draft| draft.with_session_id(session_id))
        .map_err(|_| ())
}

async fn run_title_request(
    provider: Arc<dyn ModelProvider>,
    request: ProviderRequest,
    cancellation: CancellationToken,
) -> TitleRequestOutcome {
    if cancellation.is_cancelled() {
        return TitleRequestOutcome::Cancelled;
    }
    let stream = match catch_unwind(AssertUnwindSafe(|| {
        provider.stream(request, cancellation.clone())
    })) {
        Ok(stream) => stream,
        Err(_) => return TitleRequestOutcome::Failed,
    };
    let future = AssertUnwindSafe(consume_title_stream(stream)).catch_unwind();
    tokio::select! {
        biased;
        () = cancellation.cancelled() => TitleRequestOutcome::Cancelled,
        result = tokio::time::timeout(TITLE_TIMEOUT, future) => match result {
            Ok(Ok(Some(title))) => TitleRequestOutcome::Accepted(title),
            Ok(Ok(None)) | Ok(Err(_)) => TitleRequestOutcome::Failed,
            Err(_) => {
            cancellation.cancel();
                TitleRequestOutcome::Failed
            }
        }
    }
}

async fn consume_title_stream(mut stream: crate::provider::ProviderStream) -> Option<String> {
    let mut validator = StreamValidator::default();
    let mut text = String::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let chunk = item.ok()?;
        validator.accept(&chunk).ok()?;
        match chunk.kind() {
            StreamChunkKind::BlockStart {
                block_type: ContentBlockType::Text,
                ..
            }
            | StreamChunkKind::TextDelta { .. }
            | StreamChunkKind::Usage { .. } => {}
            StreamChunkKind::BlockEnd { block, .. } => {
                let ContentBlockKind::Text { text: block_text } = block.kind() else {
                    return None;
                };
                if text.len().checked_add(block_text.len())? > MAX_ASSEMBLED_TITLE_BYTES {
                    return None;
                }
                text.push_str(block_text);
            }
            StreamChunkKind::Finish { reason, .. } => finish = Some(reason.clone()),
            StreamChunkKind::BlockStart { .. }
            | StreamChunkKind::ReasoningDelta { .. }
            | StreamChunkKind::ToolCallDelta { .. }
            | StreamChunkKind::Other { .. } => return None,
        }
    }
    validator.complete().ok()?;
    if !matches!(finish?.kind(), FinishReasonKind::Stop) {
        return None;
    }
    normalize_title(&text, PROVIDER_TITLE_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use futures_util::stream;

    use crate::{
        agent::{AgentLoop, AgentLoopConfig, ManualTitleRefreshOutcome, NoTools, TurnProposal},
        model::{
            ContentBlock, LlmCallConfig, LlmCallConfigAdapterDefaults, Message, MessageSource,
            NonNegativeSafeInteger, RequestPurpose, StreamChunk,
        },
        provider::{
            ModelProvider, PreparedProviderCall, PreparedRequestPreflight, ProviderPreflightError,
            ProviderPrepareError, ProviderRequest, ProviderRequestDraft, ProviderStream,
        },
        session::{EventKind, Session, SessionTitleSource},
    };
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct TitleProvider {
        purposes: Mutex<Vec<RequestPurpose>>,
        defer_title: bool,
        disable_titles: bool,
        invalid_title: bool,
        title_cancelled: Arc<AtomicBool>,
        cancellations: Mutex<Vec<CancellationToken>>,
    }

    impl ModelProvider for TitleProvider {
        fn supports_session_titles(&self) -> bool {
            !self.disable_titles
        }

        fn prepare_call(
            &self,
            config: LlmCallConfig,
        ) -> Result<PreparedProviderCall, ProviderPrepareError> {
            let max = NonNegativeSafeInteger::new(1_024).expect("test maximum is safe");
            let config = if config.max_tokens().is_none() {
                config
                    .with_max_tokens_preserving_extensions(max)
                    .expect("test config accepts a maximum")
            } else {
                config
            };
            Ok(PreparedProviderCall::new(
                config,
                LlmCallConfigAdapterDefaults::default(),
                Some(NonNegativeSafeInteger::new(8_192).expect("test context is safe")),
            ))
        }

        fn preflight_request(
            &self,
            draft: ProviderRequestDraft<'_>,
        ) -> Result<PreparedRequestPreflight, ProviderPreflightError> {
            let prepared = self.prepare_call(draft.config().clone())?;
            draft.finish(prepared, 1)
        }

        fn stream(
            &self,
            request: ProviderRequest,
            cancellation: CancellationToken,
        ) -> ProviderStream {
            self.purposes.lock().unwrap().push(request.purpose());
            self.cancellations
                .lock()
                .unwrap()
                .push(cancellation.clone());
            if request.purpose() == RequestPurpose::SessionTitle && self.defer_title {
                let title_cancelled = self.title_cancelled.clone();
                return Box::pin(stream::once(async move {
                    cancellation.cancelled().await;
                    title_cancelled.store(true, Ordering::SeqCst);
                    Ok(
                        StreamChunk::finish(crate::model::FinishReason::stop().unwrap(), None)
                            .unwrap(),
                    )
                }));
            }
            if request.purpose() == RequestPurpose::SessionTitle && self.invalid_title {
                return Box::pin(stream::iter([Ok(StreamChunk::finish(
                    crate::model::FinishReason::stop().unwrap(),
                    None,
                )
                .unwrap())]));
            }
            let text = if request.purpose() == RequestPurpose::SessionTitle {
                "修复解析器取消问题"
            } else {
                "done"
            };
            let chunks = vec![
                StreamChunk::block_start(0, crate::model::ContentBlockType::Text).unwrap(),
                StreamChunk::text_delta(0, text).unwrap(),
                StreamChunk::block_end(0, ContentBlock::text(text).unwrap()).unwrap(),
                StreamChunk::finish(crate::model::FinishReason::stop().unwrap(), None).unwrap(),
            ];
            Box::pin(stream::iter(chunks.into_iter().map(Ok)))
        }
    }

    #[tokio::test]
    async fn first_human_prompt_records_fallback_request_and_provider_replacement() {
        let provider = Arc::new(TitleProvider::default());
        let session = Session::new("session-title-test").unwrap();
        let config = AgentLoopConfig::new(LlmCallConfig::new("deepseek", "test-model").unwrap());
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();
        let prompt = Message::user(
            "human-1",
            vec![ContentBlock::text("请修复解析器的取消问题并补测试").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![prompt]), CancellationToken::new())
            .await
            .unwrap();
        for _ in 0..8 {
            if provider
                .purposes
                .lock()
                .unwrap()
                .contains(&RequestPurpose::SessionTitle)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let session = agent.shutdown_into_session().await.unwrap();

        let titles = session
            .events()
            .iter()
            .filter_map(|event| match event.kind() {
                EventKind::SessionTitle { title } => Some(title),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(titles.len(), 2);
        assert!(matches!(titles[0].source(), SessionTitleSource::Fallback));
        assert_eq!(titles[1].title(), "修复解析器取消问题");
        assert!(matches!(
            titles[1].source(),
            SessionTitleSource::Provider { .. }
        ));
        assert_eq!(
            session
                .events()
                .iter()
                .filter(|event| matches!(event.kind(), EventKind::SessionTitleLlmRequest { .. }))
                .count(),
            1
        );
        let purposes = provider.purposes.lock().unwrap();
        assert_eq!(
            purposes
                .iter()
                .filter(|purpose| **purpose == RequestPurpose::SessionTitle)
                .count(),
            1
        );
        assert_eq!(
            purposes
                .iter()
                .filter(|purpose| **purpose == RequestPurpose::Conversation)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn manual_rename_normalizes_supersedes_and_pins_the_user_title() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase48_manual_session_title.json"
        ))
        .unwrap();
        let provider = Arc::new(TitleProvider {
            defer_title: true,
            ..TitleProvider::default()
        });
        let session = Session::new("session-title-rename-test").unwrap();
        let config = AgentLoopConfig::new(LlmCallConfig::new("deepseek", "test-model").unwrap());
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();

        let first = Message::user(
            "human-1",
            vec![ContentBlock::text("Prompt that triggers generation").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![first]), CancellationToken::new())
            .await
            .unwrap();
        for _ in 0..16 {
            if provider
                .purposes
                .lock()
                .unwrap()
                .contains(&RequestPurpose::SessionTitle)
            {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert_eq!(
            agent
                .rename_session_title("  Hand\tpicked   name  ")
                .await
                .unwrap(),
            Some("Hand picked name".to_owned())
        );
        assert!(
            provider
                .purposes
                .lock()
                .unwrap()
                .iter()
                .zip(provider.cancellations.lock().unwrap().iter())
                .any(|(purpose, cancellation)| {
                    *purpose == RequestPurpose::SessionTitle && cancellation.is_cancelled()
                })
        );
        assert_eq!(
            agent
                .session()
                .state()
                .session_title()
                .map(|title| title.title()),
            Some("Hand picked name")
        );

        let second = Message::user(
            "human-2",
            vec![ContentBlock::text("A later eligible prompt").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![second]), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            agent.rename_session_title(" \u{1b}[31m ").await.unwrap(),
            None
        );
        let session = agent.shutdown_into_session().await.unwrap();

        let titles = session
            .events()
            .iter()
            .filter_map(|event| match event.kind() {
                EventKind::SessionTitle { title } => Some(title),
                _ => None,
            })
            .collect::<Vec<_>>();
        let latest = titles.last().unwrap();
        assert_eq!(latest.title(), fixture["accepted"]["title"]);
        assert!(latest.message_seqs().is_empty());
        assert!(matches!(latest.source(), SessionTitleSource::User));
        assert_eq!(
            provider
                .purposes
                .lock()
                .unwrap()
                .iter()
                .filter(|purpose| **purpose == RequestPurpose::SessionTitle)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn explicit_refresh_uses_first_prompt_and_replaces_the_user_pin() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase49_session_title_refresh.json"
        ))
        .unwrap();
        let provider = Arc::new(TitleProvider::default());
        let session = Session::new("session-title-refresh-test").unwrap();
        let config = AgentLoopConfig::new(
            LlmCallConfig::new(crate::provider::deepseek::DEEPSEEK_PROVIDER, "test-model").unwrap(),
        );
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();
        let first = Message::user(
            "human-refresh-1",
            vec![ContentBlock::text("First title prompt").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![first]), CancellationToken::new())
            .await
            .unwrap();
        agent.rename_session_title("Pinned by hand").await.unwrap();
        let second = Message::user(
            "human-refresh-2",
            vec![ContentBlock::text("Second prompt must be ignored").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![second]), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            agent
                .refresh_session_title(CancellationToken::new())
                .await
                .unwrap(),
            ManualTitleRefreshOutcome::Refreshed {
                title: "修复解析器取消问题".to_owned()
            }
        );
        let session = agent.shutdown_into_session().await.unwrap();
        let tail = session.events()[session.events().len() - 2..]
            .iter()
            .map(|event| event.kind().event_type())
            .collect::<Vec<_>>();
        assert_eq!(
            tail,
            fixture["providerRefresh"]["eventOrder"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect::<Vec<_>>()
        );
        let state = session.state();
        let latest = state.session_title().unwrap();
        assert!(matches!(
            latest.source(),
            SessionTitleSource::Provider { .. }
        ));
        assert_eq!(
            latest.message_seqs(),
            [crate::session::EventSeq::new(2).unwrap()]
        );
        assert_eq!(
            state.session_title_first_prompt(),
            Some((
                crate::session::EventSeq::new(2).unwrap(),
                "First title prompt"
            ))
        );
        assert!(!format!("{state:?}").contains("First title prompt"));
        assert_eq!(
            provider
                .purposes
                .lock()
                .unwrap()
                .iter()
                .filter(|purpose| **purpose == RequestPurpose::SessionTitle)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn explicit_refresh_cancellation_and_failure_keep_the_user_title() {
        for (cancel, invalid, expected) in [
            (true, false, ManualTitleRefreshOutcome::Cancelled),
            (false, true, ManualTitleRefreshOutcome::Failed),
        ] {
            let provider = Arc::new(TitleProvider {
                defer_title: cancel,
                invalid_title: invalid,
                ..TitleProvider::default()
            });
            let session =
                Session::new(format!("session-title-refresh-{cancel}-{invalid}")).unwrap();
            let config =
                AgentLoopConfig::new(LlmCallConfig::new("deepseek", "test-model").unwrap());
            let mut agent =
                AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();
            let first = Message::user(
                "human-refresh",
                vec![ContentBlock::text("Refresh cancellation source").unwrap()],
                MessageSource::user().unwrap(),
            )
            .unwrap();
            agent
                .run_turn(TurnProposal::Enter(vec![first]), CancellationToken::new())
                .await
                .unwrap();
            agent.rename_session_title("Pinned safely").await.unwrap();
            provider.title_cancelled.store(false, Ordering::SeqCst);
            let cancellation = CancellationToken::new();
            if cancel {
                let trigger = cancellation.clone();
                let observed = provider.clone();
                tokio::spawn(async move {
                    while observed
                        .purposes
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|purpose| **purpose == RequestPurpose::SessionTitle)
                        .count()
                        < 2
                    {
                        tokio::task::yield_now().await;
                    }
                    trigger.cancel();
                });
            }

            assert_eq!(
                agent.refresh_session_title(cancellation).await.unwrap(),
                expected
            );
            assert_eq!(
                agent
                    .session()
                    .state()
                    .session_title()
                    .map(|title| title.title()),
                Some("Pinned safely")
            );
            assert!(matches!(
                agent.session().events().last().map(|event| event.kind()),
                Some(EventKind::SessionTitleLlmRequest { .. })
            ));
            if cancel {
                assert!(
                    provider
                        .cancellations
                        .lock()
                        .unwrap()
                        .last()
                        .is_some_and(CancellationToken::is_cancelled)
                );
            }
        }
    }

    #[tokio::test]
    async fn fallback_only_refresh_unpins_without_a_title_request() {
        let provider = Arc::new(TitleProvider {
            disable_titles: true,
            ..TitleProvider::default()
        });
        let session = Session::new("session-title-fallback-refresh").unwrap();
        let config = AgentLoopConfig::new(
            LlmCallConfig::new(crate::provider::deepseek::DEEPSEEK_PROVIDER, "test-model").unwrap(),
        );
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();
        let first = Message::user(
            "human-fallback-refresh",
            vec![ContentBlock::text("Return to this fallback title please").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![first]), CancellationToken::new())
            .await
            .unwrap();
        let title_events = agent
            .session()
            .events()
            .iter()
            .filter(|event| matches!(event.kind(), EventKind::SessionTitle { .. }))
            .count();
        assert_eq!(
            agent
                .refresh_session_title(CancellationToken::new())
                .await
                .unwrap(),
            ManualTitleRefreshOutcome::Unchanged {
                title: "Return to this fallback title".to_owned()
            }
        );
        assert_eq!(
            agent
                .session()
                .events()
                .iter()
                .filter(|event| matches!(event.kind(), EventKind::SessionTitle { .. }))
                .count(),
            title_events
        );
        agent.rename_session_title("Manual title").await.unwrap();

        assert_eq!(
            agent
                .refresh_session_title(CancellationToken::new())
                .await
                .unwrap(),
            ManualTitleRefreshOutcome::Refreshed {
                title: "Return to this fallback title".to_owned()
            }
        );
        assert!(matches!(
            agent.session().state().session_title().unwrap().source(),
            SessionTitleSource::Fallback
        ));
        assert!(
            provider
                .purposes
                .lock()
                .unwrap()
                .iter()
                .all(|purpose| *purpose != RequestPurpose::SessionTitle)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_refresh_timeout_keeps_the_user_title_and_cancels_provider() {
        let provider = Arc::new(TitleProvider {
            defer_title: true,
            ..TitleProvider::default()
        });
        let session = Session::new("session-title-refresh-timeout").unwrap();
        let config = AgentLoopConfig::new(LlmCallConfig::new("deepseek", "test-model").unwrap());
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();
        let first = Message::user(
            "human-refresh-timeout",
            vec![ContentBlock::text("Refresh timeout source").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        agent
            .run_turn(TurnProposal::Enter(vec![first]), CancellationToken::new())
            .await
            .unwrap();
        agent
            .rename_session_title("Timeout-safe title")
            .await
            .unwrap();
        let observed = provider.clone();
        tokio::spawn(async move {
            while observed
                .purposes
                .lock()
                .unwrap()
                .iter()
                .filter(|purpose| **purpose == RequestPurpose::SessionTitle)
                .count()
                < 2
            {
                tokio::task::yield_now().await;
            }
            tokio::time::advance(super::TITLE_TIMEOUT + std::time::Duration::from_secs(1)).await;
        });

        assert_eq!(
            agent
                .refresh_session_title(CancellationToken::new())
                .await
                .unwrap(),
            ManualTitleRefreshOutcome::Failed
        );
        assert_eq!(
            agent
                .session()
                .state()
                .session_title()
                .map(|title| title.title()),
            Some("Timeout-safe title")
        );
        assert!(
            provider
                .cancellations
                .lock()
                .unwrap()
                .last()
                .is_some_and(CancellationToken::is_cancelled)
        );
    }

    #[tokio::test]
    async fn explicit_refresh_without_a_human_prompt_has_no_side_effect() {
        let provider = Arc::new(TitleProvider::default());
        let session = Session::new("session-title-refresh-empty").unwrap();
        let config = AgentLoopConfig::new(LlmCallConfig::new("deepseek", "test-model").unwrap());
        let mut agent =
            AgentLoop::new(session, provider.clone(), Arc::new(NoTools), config).unwrap();

        assert_eq!(
            agent
                .refresh_session_title(CancellationToken::new())
                .await
                .unwrap(),
            ManualTitleRefreshOutcome::NoHistory
        );
        assert!(agent.session().events().is_empty());
        assert!(provider.purposes.lock().unwrap().is_empty());

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            agent.refresh_session_title(cancelled).await.unwrap(),
            ManualTitleRefreshOutcome::Cancelled
        );
        assert!(agent.session().events().is_empty());
    }
}
