use std::sync::{Arc, Mutex};

use futures_util::stream;
use tokio_util::sync::CancellationToken;

use super::{
    AgentLoop, AgentLoopConfig, AgentLoopError, ManualModelSelectionOutcome, NoTools, TurnProposal,
};
use crate::{
    model::{
        ContentBlock, ContentBlockType, FinishReason, LlmCallConfig, LlmCallConfigAdapterDefaults,
        Message, MessageSource, NonNegativeSafeInteger, ReasoningEffortId, StreamChunk, TrueMarker,
    },
    provider::{
        ModelProvider, PreparedProviderCall, PreparedRequestPreflight, ProviderPreflightError,
        ProviderPrepareError, ProviderRequest, ProviderRequestDraft, ProviderStream,
    },
    session::{EventKind, NewEvent, RequestHeaderReason, Session, TurnId},
};

#[derive(Default)]
struct SelectionProvider {
    dispatched: Mutex<Vec<LlmCallConfig>>,
}

impl ModelProvider for SelectionProvider {
    fn prepare_call(
        &self,
        config: LlmCallConfig,
    ) -> Result<PreparedProviderCall, ProviderPrepareError> {
        if config.provider() != "deepseek-official" {
            return Err(ProviderPrepareError::WrongProvider {
                expected: "deepseek-official".to_owned(),
                actual: config.provider().to_owned(),
            });
        }
        let requested = config.reasoning_effort().map(|effort| effort.as_str());
        let effort = match requested {
            None => "high",
            Some(value @ ("off" | "high" | "max")) => value,
            Some(value) => {
                return Err(ProviderPrepareError::UnsupportedReasoningEffort {
                    value: value.to_owned(),
                });
            }
        };
        let defaults = LlmCallConfigAdapterDefaults {
            reasoning_effort: requested.is_none().then_some(TrueMarker),
            max_tokens: Some(TrueMarker),
        };
        let effective = config.with_materialized_defaults(
            ReasoningEffortId::new(effort),
            NonNegativeSafeInteger::new(1_024).unwrap(),
        )?;
        Ok(PreparedProviderCall::new(
            effective,
            defaults,
            Some(NonNegativeSafeInteger::new(8_192).unwrap()),
        ))
    }

    fn preflight_request(
        &self,
        draft: ProviderRequestDraft<'_>,
    ) -> Result<PreparedRequestPreflight, ProviderPreflightError> {
        let prepared = self.prepare_call(draft.config().clone())?;
        draft.finish(prepared, 1)
    }

    fn stream(&self, request: ProviderRequest, _cancellation: CancellationToken) -> ProviderStream {
        self.dispatched
            .lock()
            .unwrap()
            .push(request.config().clone());
        Box::pin(stream::iter(
            [
                StreamChunk::block_start(0, ContentBlockType::Text).unwrap(),
                StreamChunk::block_end(0, ContentBlock::text("done").unwrap()).unwrap(),
                StreamChunk::finish(FinishReason::stop().unwrap(), None).unwrap(),
            ]
            .into_iter()
            .map(Ok),
        ))
    }
}

#[tokio::test]
async fn model_selection_is_pending_until_request_and_then_logs_initial_and_change() {
    let provider = Arc::new(SelectionProvider::default());
    let mut agent = AgentLoop::new(
        Session::new("session-model-selection").unwrap(),
        provider.clone(),
        Arc::new(NoTools),
        AgentLoopConfig::new(LlmCallConfig::new("deepseek-official", "deepseek-v4-flash").unwrap()),
    )
    .unwrap();

    assert_eq!(
        agent
            .current_model_selection()
            .unwrap()
            .reasoning_effort
            .as_deref(),
        Some("high")
    );
    assert!(agent.session().events().is_empty());
    assert_eq!(
        agent.select_model("private-preview", Some("max")).unwrap(),
        ManualModelSelectionOutcome::Selected {
            selection: super::SessionModelSelection {
                model: "private-preview".to_owned(),
                reasoning_effort: Some("max".to_owned()),
            },
            changed: true,
        }
    );
    assert!(agent.session().events().is_empty());
    assert_eq!(
        agent.select_model("rejected", Some("medium")).unwrap(),
        ManualModelSelectionOutcome::Unavailable
    );
    assert_eq!(
        agent.current_model_selection().unwrap().model,
        "private-preview"
    );

    agent
        .run_turn(user_turn("first-input", "first"), CancellationToken::new())
        .await
        .unwrap();
    let headers = request_headers(&agent);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].0.model(), "private-preview");
    assert_eq!(
        headers[0].0.reasoning_effort().map(|value| value.as_str()),
        Some("max")
    );
    assert_eq!(headers[0].1, &RequestHeaderReason::Initial);

    let event_count = agent.session().events().len();
    assert!(matches!(
        agent
            .select_model("deepseek-v4-flash", Some("off"))
            .unwrap(),
        ManualModelSelectionOutcome::Selected { changed: true, .. }
    ));
    assert!(matches!(
        agent.select_model("deepseek-v4-pro", None).unwrap(),
        ManualModelSelectionOutcome::Selected { changed: true, .. }
    ));
    assert_eq!(agent.session().events().len(), event_count);

    agent
        .run_turn(
            user_turn("second-input", "second"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let headers = request_headers(&agent);
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[1].0.model(), "deepseek-v4-pro");
    assert_eq!(
        headers[1].0.reasoning_effort().map(|value| value.as_str()),
        Some("high")
    );
    assert_eq!(headers[1].1, &RequestHeaderReason::Change);
    assert_eq!(
        headers[1]
            .0
            .raw()
            .as_value()
            .get("reasoningEffort")
            .and_then(serde_json::Value::as_str),
        Some("high")
    );

    let dispatched = provider.dispatched.lock().unwrap();
    assert_eq!(dispatched.len(), 2);
    assert_eq!(dispatched[0].model(), "private-preview");
    assert_eq!(dispatched[1].model(), "deepseek-v4-pro");
    drop(dispatched);

    assert!(matches!(
        agent.select_model("deepseek-v4-pro", Some("high")).unwrap(),
        ManualModelSelectionOutcome::Selected { changed: false, .. }
    ));
}

#[test]
fn model_selection_is_idle_only() {
    let provider = Arc::new(SelectionProvider::default());
    let mut agent = AgentLoop::new(
        Session::new("session-model-selection-busy").unwrap(),
        provider,
        Arc::new(NoTools),
        AgentLoopConfig::new(LlmCallConfig::new("deepseek-official", "deepseek-v4-flash").unwrap()),
    )
    .unwrap();
    agent
        .session
        .append(NewEvent::log(EventKind::turn_start(
            TurnId::new(1).unwrap(),
        )))
        .unwrap();
    assert!(matches!(
        agent.select_model("deepseek-v4-pro", None),
        Err(AgentLoopError::SessionNotIdle)
    ));
}

fn user_turn(id: &str, text: &str) -> TurnProposal {
    TurnProposal::Enter(vec![
        Message::user(
            id,
            vec![ContentBlock::text(text).unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap(),
    ])
}

fn request_headers(agent: &AgentLoop) -> Vec<(&LlmCallConfig, &RequestHeaderReason)> {
    agent
        .session()
        .events()
        .iter()
        .filter_map(|event| match event.kind() {
            EventKind::RequestHeader { header, reason } => Some((&header.config, reason)),
            _ => None,
        })
        .collect()
}
