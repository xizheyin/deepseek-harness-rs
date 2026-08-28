use crate::user_question::{
    MAX_CUSTOM_ANSWER_BYTES, UserQuestionEnvelope, UserQuestionItem, UserQuestionResponseItem,
    custom_answer_is_valid,
};

const MAX_SELECTION_RECORD_BYTES: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionPhase {
    Inactive,
    Received { retry: bool },
    Rendering,
    Selecting,
    Custom { retry: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QuestionAcceptingMode {
    Selection,
    Custom,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum QuestionInputUpdate {
    None,
    Selected(usize),
    CustomRequested,
    CustomSubmitted(String),
    Cancelled,
    Invalid,
    Eof,
}

#[derive(Debug)]
pub(super) struct UserQuestionUiState {
    active: Option<UserQuestionEnvelope>,
    phase: QuestionPhase,
    selection_record: [u8; MAX_SELECTION_RECORD_BYTES],
    selection_record_len: usize,
    custom_record: [u8; MAX_CUSTOM_ANSWER_BYTES],
    custom_record_len: usize,
    custom_record_oversized: bool,
    current_question: usize,
    answers: Vec<UserQuestionResponseItem>,
}

impl Default for UserQuestionUiState {
    fn default() -> Self {
        Self {
            active: None,
            phase: QuestionPhase::Inactive,
            selection_record: [0; MAX_SELECTION_RECORD_BYTES],
            selection_record_len: 0,
            custom_record: [0; MAX_CUSTOM_ANSWER_BYTES],
            custom_record_len: 0,
            custom_record_oversized: false,
            current_question: 0,
            answers: Vec::new(),
        }
    }
}

impl UserQuestionUiState {
    pub(super) fn receive(&mut self, envelope: UserQuestionEnvelope) -> Result<(), ()> {
        if self.active.is_some() || self.phase != QuestionPhase::Inactive {
            return Err(());
        }
        self.answers.clear();
        self.answers
            .try_reserve_exact(envelope.request().questions().len())
            .map_err(|_| ())?;
        self.active = Some(envelope);
        self.phase = QuestionPhase::Received { retry: false };
        self.clear_records();
        self.current_question = 0;
        Ok(())
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn is_accepting(&self) -> bool {
        matches!(
            self.phase,
            QuestionPhase::Selecting | QuestionPhase::Custom { .. }
        )
    }

    pub(super) fn is_custom(&self) -> bool {
        matches!(self.phase, QuestionPhase::Custom { .. })
    }

    pub(super) fn custom_retry(&self) -> bool {
        matches!(self.phase, QuestionPhase::Custom { retry: true })
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

    pub(super) fn begin_accepting(&mut self) -> Result<QuestionAcceptingMode, ()> {
        if self.phase != QuestionPhase::Rendering {
            return Err(());
        }
        self.clear_records();
        let has_options = self
            .active
            .as_ref()
            .and_then(|envelope| envelope.request().questions().get(self.current_question))
            .is_some_and(|question| !question.options().is_empty());
        if has_options {
            self.phase = QuestionPhase::Selecting;
            Ok(QuestionAcceptingMode::Selection)
        } else {
            self.phase = QuestionPhase::Custom { retry: false };
            Ok(QuestionAcceptingMode::Custom)
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8], enhanced: bool) -> QuestionInputUpdate {
        match self.phase {
            QuestionPhase::Selecting if enhanced => self.feed_enhanced_selection(bytes),
            QuestionPhase::Selecting => self.feed_linear_selection(bytes),
            QuestionPhase::Custom { .. } if !enhanced => self.feed_linear_custom(bytes),
            QuestionPhase::Custom { .. } => QuestionInputUpdate::Invalid,
            QuestionPhase::Inactive | QuestionPhase::Received { .. } | QuestionPhase::Rendering => {
                QuestionInputUpdate::Invalid
            }
        }
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
        if !valid || self.answers.len() != self.current_question {
            self.retry();
            return;
        }
        self.advance_or_finish(UserQuestionResponseItem::Selected(index), question_count);
    }

    pub(super) fn begin_custom(&mut self) -> Result<(), ()> {
        if self.phase != QuestionPhase::Selecting {
            return Err(());
        }
        self.clear_records();
        self.phase = QuestionPhase::Custom { retry: false };
        Ok(())
    }

    pub(super) fn submit_custom(&mut self, custom: String) -> bool {
        if !matches!(self.phase, QuestionPhase::Custom { .. })
            || self.answers.len() != self.current_question
        {
            self.retry_custom();
            return false;
        }
        let trimmed = custom.trim();
        if !custom_answer_is_valid(trimmed) {
            self.retry_custom();
            return false;
        }
        let mut retained = String::new();
        if retained.try_reserve_exact(trimmed.len()).is_err() {
            self.retry_custom();
            return false;
        }
        retained.push_str(trimmed);
        let question_count = self
            .active
            .as_ref()
            .map_or(0, |envelope| envelope.request().questions().len());
        self.advance_or_finish(UserQuestionResponseItem::Custom(retained), question_count);
        true
    }

    fn advance_or_finish(&mut self, answer: UserQuestionResponseItem, question_count: usize) {
        self.answers.push(answer);
        if self.answers.len() < question_count {
            self.current_question += 1;
            self.phase = QuestionPhase::Received { retry: false };
            self.clear_records();
            return;
        }
        let Some(envelope) = self.active.take() else {
            self.reset();
            return;
        };
        let answers = std::mem::take(&mut self.answers);
        let _ = envelope.answer(answers);
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
        if self.is_custom() {
            self.retry_custom();
        } else if self.active.is_some() {
            self.phase = QuestionPhase::Received { retry: true };
            self.clear_records();
        } else {
            self.reset();
        }
    }

    fn retry_custom(&mut self) {
        if self.active.is_some() {
            self.phase = QuestionPhase::Custom { retry: true };
            self.clear_records();
        } else {
            self.reset();
        }
    }

    fn feed_enhanced_selection(&self, bytes: &[u8]) -> QuestionInputUpdate {
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
            if (b'1'..=b'5').contains(byte) {
                let index = usize::from(*byte - b'1');
                return classify_selection(index, option_count);
            }
            if !matches!(*byte, b'\r' | b'\n' | b' ' | b'\t') {
                return QuestionInputUpdate::Invalid;
            }
        }
        QuestionInputUpdate::None
    }

    fn feed_linear_selection(&mut self, bytes: &[u8]) -> QuestionInputUpdate {
        for byte in bytes {
            if *byte == 0x04 {
                return QuestionInputUpdate::Eof;
            }
            if *byte == 0x1b {
                return QuestionInputUpdate::Cancelled;
            }
            if matches!(*byte, b'\r' | b'\n') {
                let record = &self.selection_record[..self.selection_record_len];
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
                self.selection_record_len = 0;
                let Some(digit) = digit.filter(|_| only_one) else {
                    return QuestionInputUpdate::Invalid;
                };
                let option_count = self
                    .active
                    .as_ref()
                    .and_then(|envelope| envelope.request().questions().get(self.current_question))
                    .map_or(0, |question| question.options().len());
                if (b'1'..=b'5').contains(&digit) {
                    let index = usize::from(digit - b'1');
                    return classify_selection(index, option_count);
                }
                return QuestionInputUpdate::Invalid;
            }
            if self.selection_record_len == self.selection_record.len() {
                self.selection_record_len = 0;
                return QuestionInputUpdate::Invalid;
            }
            self.selection_record[self.selection_record_len] = *byte;
            self.selection_record_len += 1;
        }
        QuestionInputUpdate::None
    }

    fn feed_linear_custom(&mut self, bytes: &[u8]) -> QuestionInputUpdate {
        for byte in bytes {
            if *byte == 0x04 {
                return QuestionInputUpdate::Eof;
            }
            if *byte == 0x1b {
                return QuestionInputUpdate::Cancelled;
            }
            if matches!(*byte, b'\r' | b'\n') {
                if self.custom_record_oversized {
                    self.clear_records();
                    return QuestionInputUpdate::Invalid;
                }
                let record = &self.custom_record[..self.custom_record_len];
                let text = match std::str::from_utf8(record) {
                    Ok(text) => text,
                    Err(_) => {
                        self.clear_records();
                        return QuestionInputUpdate::Invalid;
                    }
                };
                let mut retained = String::new();
                if retained.try_reserve_exact(text.len()).is_err() {
                    self.clear_records();
                    return QuestionInputUpdate::Invalid;
                }
                retained.push_str(text);
                self.clear_records();
                return QuestionInputUpdate::CustomSubmitted(retained);
            }
            if self.custom_record_len == self.custom_record.len() {
                self.custom_record_oversized = true;
                continue;
            }
            if !self.custom_record_oversized {
                self.custom_record[self.custom_record_len] = *byte;
                self.custom_record_len += 1;
            }
        }
        QuestionInputUpdate::None
    }

    fn clear_records(&mut self) {
        self.selection_record_len = 0;
        self.custom_record_len = 0;
        self.custom_record_oversized = false;
    }

    fn reset(&mut self) {
        self.active = None;
        self.phase = QuestionPhase::Inactive;
        self.clear_records();
        self.current_question = 0;
        self.answers.clear();
    }
}

fn classify_selection(index: usize, option_count: usize) -> QuestionInputUpdate {
    match index.cmp(&option_count) {
        std::cmp::Ordering::Less => QuestionInputUpdate::Selected(index),
        std::cmp::Ordering::Equal => QuestionInputUpdate::CustomRequested,
        std::cmp::Ordering::Greater => QuestionInputUpdate::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use crate::user_question::{
        UserQuestionBroker, UserQuestionItem, UserQuestionOption, UserQuestionRequest,
    };
    use futures_util::poll;
    use tokio_util::sync::CancellationToken;

    use super::{QuestionAcceptingMode, QuestionInputUpdate, UserQuestionUiState};

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
        assert_eq!(
            ui.begin_accepting().unwrap(),
            QuestionAcceptingMode::Selection
        );
        assert_eq!(ui.feed(b"2", true), QuestionInputUpdate::Selected(1));
        ui.select(1);
        assert_eq!(answer.await.unwrap().answers()[0].selected(), Some("Fast"));
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
        assert_eq!(answer.await.unwrap().answers()[0].selected(), Some("Safe"));

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
        assert_eq!(answer.answers()[0].selected(), Some("Fast"));
        assert_eq!(answer.answers()[1].id(), "tests");
        assert_eq!(answer.answers()[1].selected(), Some("Safe"));

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

    #[tokio::test]
    async fn optionless_and_other_paths_publish_trimmed_custom_text() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let answer = broker.ask(
            UserQuestionRequest::new(vec![UserQuestionItem::new(
                "detail".to_owned(),
                None,
                "What should I do?".to_owned(),
                Vec::new(),
            )]),
            CancellationToken::new(),
        );
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        assert_eq!(ui.begin_accepting().unwrap(), QuestionAcceptingMode::Custom);
        assert_eq!(
            ui.feed(" 只跑必要检查 \n".as_bytes(), false),
            QuestionInputUpdate::CustomSubmitted(" 只跑必要检查 ".to_owned())
        );
        assert!(ui.submit_custom(" 只跑必要检查 ".to_owned()));
        let answer = answer.await.unwrap();
        assert_eq!(answer.answers()[0].selected(), None);
        assert_eq!(answer.answers()[0].custom(), Some("只跑必要检查"));

        let answer = broker.ask(request(), CancellationToken::new());
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        assert_eq!(ui.feed(b"3", true), QuestionInputUpdate::CustomRequested);
        ui.begin_custom().unwrap();
        assert!(ui.submit_custom("manual".to_owned()));
        assert_eq!(answer.await.unwrap().answers()[0].custom(), Some("manual"));
    }

    #[tokio::test]
    async fn blank_and_oversized_custom_records_retry_without_settling_the_batch() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let answer = broker.ask(
            UserQuestionRequest::new(vec![UserQuestionItem::new(
                "detail".to_owned(),
                None,
                "What should I do?".to_owned(),
                Vec::new(),
            )]),
            CancellationToken::new(),
        );
        tokio::pin!(answer);
        assert!(poll!(&mut answer).is_pending());
        let mut ui = UserQuestionUiState::default();
        ui.receive(receiver.try_recv().unwrap()).unwrap();
        ui.mark_rendering().unwrap();
        ui.begin_accepting().unwrap();
        assert!(!ui.submit_custom(" \t ".to_owned()));
        assert!(ui.custom_retry());
        assert!(poll!(&mut answer).is_pending());

        let mut oversized = vec![b'x'; crate::user_question::MAX_CUSTOM_ANSWER_BYTES + 1];
        oversized.push(b'\n');
        assert_eq!(ui.feed(&oversized, false), QuestionInputUpdate::Invalid);
        ui.retry();
        assert!(ui.custom_retry());
        assert!(poll!(&mut answer).is_pending());
        ui.cancel();
        assert_eq!(
            answer.await,
            Err(crate::user_question::UserQuestionError::Cancelled)
        );
    }
}
