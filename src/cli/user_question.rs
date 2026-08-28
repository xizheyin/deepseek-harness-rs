use crate::user_question::{UserQuestionEnvelope, UserQuestionItem};

const MAX_SELECTION_RECORD_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionPhase {
    Inactive,
    Received { retry: bool },
    Rendering,
    Accepting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QuestionInputUpdate {
    None,
    Selected(usize),
    Cancelled,
    Invalid,
    Eof,
}

#[derive(Debug)]
pub(super) struct UserQuestionUiState {
    active: Option<UserQuestionEnvelope>,
    phase: QuestionPhase,
    record: [u8; MAX_SELECTION_RECORD_BYTES],
    record_len: usize,
    current_question: usize,
    selected_indices: Vec<usize>,
}

impl Default for UserQuestionUiState {
    fn default() -> Self {
        Self {
            active: None,
            phase: QuestionPhase::Inactive,
            record: [0; MAX_SELECTION_RECORD_BYTES],
            record_len: 0,
            current_question: 0,
            selected_indices: Vec::new(),
        }
    }
}

impl UserQuestionUiState {
    pub(super) fn receive(&mut self, envelope: UserQuestionEnvelope) -> Result<(), ()> {
        if self.active.is_some() || self.phase != QuestionPhase::Inactive {
            return Err(());
        }
        self.selected_indices.clear();
        self.selected_indices
            .try_reserve_exact(envelope.request().questions().len())
            .map_err(|_| ())?;
        self.active = Some(envelope);
        self.phase = QuestionPhase::Received { retry: false };
        self.record_len = 0;
        self.current_question = 0;
        Ok(())
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn is_accepting(&self) -> bool {
        self.phase == QuestionPhase::Accepting
    }

    pub(super) fn frame_request(&self) -> Option<(&UserQuestionItem, bool, usize, usize)> {
        let QuestionPhase::Received { retry } = self.phase else {
            return None;
        };
        let request = self.active.as_ref()?.request();
        request
            .questions()
            .get(self.current_question)
            .map(|question| {
                (
                    question,
                    retry,
                    self.current_question + 1,
                    request.questions().len(),
                )
            })
    }

    pub(super) fn mark_rendering(&mut self) -> Result<(), ()> {
        if !matches!(self.phase, QuestionPhase::Received { .. }) {
            return Err(());
        }
        self.phase = QuestionPhase::Rendering;
        Ok(())
    }

    pub(super) fn rendering_finished(&self) -> bool {
        self.phase == QuestionPhase::Rendering
    }

    pub(super) fn begin_accepting(&mut self) -> Result<(), ()> {
        if self.phase != QuestionPhase::Rendering {
            return Err(());
        }
        self.record_len = 0;
        self.phase = QuestionPhase::Accepting;
        Ok(())
    }

    pub(super) fn feed(&mut self, bytes: &[u8], enhanced: bool) -> QuestionInputUpdate {
        if self.phase != QuestionPhase::Accepting {
            return QuestionInputUpdate::Invalid;
        }
        if enhanced {
            return self.feed_enhanced(bytes);
        }
        self.feed_linear(bytes)
    }

    pub(super) fn select(&mut self, index: usize) {
        let Some(envelope) = self.active.as_ref() else {
            self.reset();
            return;
        };
        let question_count = envelope.request().questions().len();
        let valid = envelope
            .request()
            .questions()
            .get(self.current_question)
            .is_some_and(|question| index < question.options().len());
        if !valid || self.selected_indices.len() != self.current_question {
            self.retry();
            return;
        }
        self.selected_indices.push(index);
        if self.selected_indices.len() < question_count {
            self.current_question += 1;
            self.phase = QuestionPhase::Received { retry: false };
            self.record_len = 0;
            return;
        }
        let Some(envelope) = self.active.take() else {
            self.reset();
            return;
        };
        let selected_indices = std::mem::take(&mut self.selected_indices);
        let _ = envelope.answer(selected_indices);
        self.reset();
    }

    pub(super) fn cancel(&mut self) {
        let Some(envelope) = self.active.take() else {
            self.reset();
            return;
        };
        let _ = envelope.cancel();
        self.reset();
    }

    pub(super) fn retry(&mut self) {
        if self.active.is_some() {
            self.phase = QuestionPhase::Received { retry: true };
            self.record_len = 0;
        } else {
            self.reset();
        }
    }

    fn feed_enhanced(&self, bytes: &[u8]) -> QuestionInputUpdate {
        if bytes.contains(&0x04) {
            return QuestionInputUpdate::Eof;
        }
        if bytes.contains(&0x1b) {
            return QuestionInputUpdate::Cancelled;
        }
        let option_count = self
            .active
            .as_ref()
            .and_then(|envelope| envelope.request().questions().get(self.current_question))
            .map_or(0, |question| question.options().len());
        for byte in bytes {
            if (b'1'..=b'4').contains(byte) {
                let index = usize::from(*byte - b'1');
                return if index < option_count {
                    QuestionInputUpdate::Selected(index)
                } else {
                    QuestionInputUpdate::Invalid
                };
            }
            if !matches!(*byte, b'\r' | b'\n' | b' ' | b'\t') {
                return QuestionInputUpdate::Invalid;
            }
        }
        QuestionInputUpdate::None
    }

    fn feed_linear(&mut self, bytes: &[u8]) -> QuestionInputUpdate {
        for byte in bytes {
            if *byte == 0x04 {
                return QuestionInputUpdate::Eof;
            }
            if *byte == 0x1b {
                return QuestionInputUpdate::Cancelled;
            }
            if matches!(*byte, b'\r' | b'\n') {
                let record = &self.record[..self.record_len];
                let digit = record
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace());
                let only_one = digit.is_some()
                    && record
                        .iter()
                        .filter(|byte| !byte.is_ascii_whitespace())
                        .count()
                        == 1;
                self.record_len = 0;
                let Some(digit) = digit.filter(|_| only_one) else {
                    return QuestionInputUpdate::Invalid;
                };
                let option_count = self
                    .active
                    .as_ref()
                    .and_then(|envelope| envelope.request().questions().get(self.current_question))
                    .map_or(0, |question| question.options().len());
                if (b'1'..=b'4').contains(&digit) {
                    let index = usize::from(digit - b'1');
                    return if index < option_count {
                        QuestionInputUpdate::Selected(index)
                    } else {
                        QuestionInputUpdate::Invalid
                    };
                }
                return QuestionInputUpdate::Invalid;
            }
            if self.record_len == self.record.len() {
                self.record_len = 0;
                return QuestionInputUpdate::Invalid;
            }
            self.record[self.record_len] = *byte;
            self.record_len += 1;
        }
        QuestionInputUpdate::None
    }

    fn reset(&mut self) {
        self.active = None;
        self.phase = QuestionPhase::Inactive;
        self.record_len = 0;
        self.current_question = 0;
        self.selected_indices.clear();
    }
}

#[cfg(test)]
mod tests {
    use crate::user_question::{
        UserQuestionBroker, UserQuestionItem, UserQuestionOption, UserQuestionRequest,
    };
    use futures_util::poll;
    use tokio_util::sync::CancellationToken;

    use super::{QuestionInputUpdate, UserQuestionUiState};

    fn item(id: &str, question: &str) -> UserQuestionItem {
        UserQuestionItem::new(
            id.to_owned(),
            Some("Mode".to_owned()),
            question.to_owned(),
            vec![
                UserQuestionOption::new("Safe".to_owned(), None),
                UserQuestionOption::new("Fast".to_owned(), None),
            ],
        )
    }

    fn request() -> UserQuestionRequest {
        UserQuestionRequest::new(vec![item("mode", "Which mode?")])
    }

    #[tokio::test]
    async fn stale_input_is_not_accepted_before_the_render_fence() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let answer = broker.ask(request(), CancellationToken::new());
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();

        assert_eq!(ui.feed(b"2", true), QuestionInputUpdate::Invalid);
        assert!(ui.frame_request().is_some());
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        assert_eq!(ui.feed(b"2", true), QuestionInputUpdate::Selected(1));
        ui.select(1);
        assert_eq!(answer.await.unwrap().answers()[0].selected(), "Fast");
    }

    #[tokio::test]
    async fn linear_selection_waits_for_a_record_and_escape_cancels() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let answer = broker.ask(request(), CancellationToken::new());
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        assert_eq!(ui.feed(b"1", false), QuestionInputUpdate::None);
        assert_eq!(ui.feed(b"\n", false), QuestionInputUpdate::Selected(0));
        ui.select(0);
        assert_eq!(answer.await.unwrap().answers()[0].selected(), "Safe");

        let answer = broker.ask(request(), CancellationToken::new());
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        assert_eq!(ui.feed(&[0x1b], true), QuestionInputUpdate::Cancelled);
        ui.cancel();
        assert_eq!(
            answer.await,
            Err(crate::user_question::UserQuestionError::Cancelled)
        );
    }

    #[tokio::test]
    async fn selections_advance_in_order_and_publish_only_after_the_last_question() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let answer = broker.ask(
            UserQuestionRequest::new(vec![
                item("mode", "Which mode?"),
                item("tests", "Which tests?"),
            ]),
            CancellationToken::new(),
        );
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        assert_eq!(ui.frame_request().unwrap().0.id(), "mode");
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        ui.select(1);
        assert!(poll!(&mut answer).is_pending());
        assert_eq!(ui.frame_request().unwrap().0.id(), "tests");
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        ui.select(0);

        let answer = answer.await.unwrap();
        assert_eq!(answer.answers()[0].id(), "mode");
        assert_eq!(answer.answers()[0].selected(), "Fast");
        assert_eq!(answer.answers()[1].id(), "tests");
        assert_eq!(answer.answers()[1].selected(), "Safe");

        let answer = broker.ask(
            UserQuestionRequest::new(vec![
                item("mode", "Which mode?"),
                item("tests", "Which tests?"),
            ]),
            CancellationToken::new(),
        );
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        ui.select(1);
        assert!(poll!(&mut answer).is_pending());
        ui.cancel();
        assert_eq!(
            answer.await,
            Err(crate::user_question::UserQuestionError::Cancelled)
        );
    }
}
