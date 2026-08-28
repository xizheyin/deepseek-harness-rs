use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_QUESTION_ID_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_HEADER_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_TEXT_BYTES: usize = 512;
pub(crate) const MAX_QUESTION_OPTION_LABEL_BYTES: usize = 128;
pub(crate) const MAX_QUESTION_OPTION_DESCRIPTION_BYTES: usize = 256;
pub(crate) const MAX_CUSTOM_ANSWER_BYTES: usize = 4 * 1024;
pub(crate) const MAX_USER_QUESTIONS: usize = 3;
pub(crate) const MIN_QUESTION_OPTIONS: usize = 2;
pub(crate) const MAX_QUESTION_OPTIONS: usize = 4;

const QUESTION_QUEUE_CAPACITY: usize = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserQuestionOption {
    label: String,
    description: Option<String>,
}

impl UserQuestionOption {
    pub(crate) fn new(label: String, description: Option<String>) -> Self {
        Self { label, description }
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserQuestionItem {
    id: String,
    header: Option<String>,
    question: String,
    options: Vec<UserQuestionOption>,
    multi_select: bool,
}

impl UserQuestionItem {
    pub(crate) fn new(
        id: String,
        header: Option<String>,
        question: String,
        options: Vec<UserQuestionOption>,
    ) -> Self {
        Self {
            id,
            header,
            question,
            options,
            multi_select: false,
        }
    }

    pub(crate) fn with_multi_select(mut self, multi_select: bool) -> Self {
        self.multi_select = multi_select;
        self
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn header(&self) -> Option<&str> {
        self.header.as_deref()
    }

    pub(crate) fn question(&self) -> &str {
        &self.question
    }

    pub(crate) fn options(&self) -> &[UserQuestionOption] {
        &self.options
    }

    pub(crate) fn multi_select(&self) -> bool {
        self.multi_select
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserQuestionRequest {
    questions: Vec<UserQuestionItem>,
}

impl UserQuestionRequest {
    pub(crate) fn new(questions: Vec<UserQuestionItem>) -> Self {
        Self { questions }
    }

    pub(crate) fn questions(&self) -> &[UserQuestionItem] {
        &self.questions
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserQuestionAnswerItem {
    id: String,
    selected: Vec<String>,
    custom: Option<String>,
}

impl UserQuestionAnswerItem {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn selected(&self) -> &[String] {
        &self.selected
    }

    pub(crate) fn custom(&self) -> Option<&str> {
        self.custom.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserQuestionAnswer {
    answers: Vec<UserQuestionAnswerItem>,
}

impl UserQuestionAnswer {
    pub(crate) fn answers(&self) -> &[UserQuestionAnswerItem] {
        &self.answers
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserQuestionError {
    Cancelled,
    Unavailable,
    InvalidResponse,
}

#[derive(Clone, Debug)]
pub(crate) struct UserQuestionBroker {
    sender: mpsc::Sender<UserQuestionEnvelope>,
}

pub(crate) type UserQuestionReceiver = mpsc::Receiver<UserQuestionEnvelope>;

#[derive(Debug)]
struct UserQuestionResponse {
    answers: Option<Vec<UserQuestionResponseItem>>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UserQuestionResponseItem {
    Skipped,
    Selected(usize),
    MultiSelected(Vec<usize>),
    Custom(String),
    MultiCustom {
        selected: Vec<usize>,
        custom: String,
    },
}

#[derive(Debug)]
pub(crate) struct UserQuestionEnvelope {
    request: UserQuestionRequest,
    response: oneshot::Sender<UserQuestionResponse>,
}

impl UserQuestionEnvelope {
    pub(crate) fn request(&self) -> &UserQuestionRequest {
        &self.request
    }

    pub(crate) fn answer(
        self,
        answers: Vec<UserQuestionResponseItem>,
    ) -> Result<(), UserQuestionError> {
        if answers.len() != self.request.questions.len()
            || answers
                .iter()
                .zip(&self.request.questions)
                .any(|(answer, question)| !response_is_valid(answer, question))
        {
            return Err(UserQuestionError::InvalidResponse);
        }
        self.response
            .send(UserQuestionResponse {
                answers: Some(answers),
            })
            .map_err(|_| UserQuestionError::Unavailable)
    }

    pub(crate) fn cancel(self) -> Result<(), UserQuestionError> {
        self.response
            .send(UserQuestionResponse { answers: None })
            .map_err(|_| UserQuestionError::Unavailable)
    }
}

impl UserQuestionBroker {
    pub(crate) fn new() -> (Self, UserQuestionReceiver) {
        let (sender, receiver) = mpsc::channel(QUESTION_QUEUE_CAPACITY);
        (Self { sender }, receiver)
    }

    pub(crate) async fn ask(
        &self,
        request: UserQuestionRequest,
        cancellation: CancellationToken,
    ) -> Result<UserQuestionAnswer, UserQuestionError> {
        if cancellation.is_cancelled() {
            return Err(UserQuestionError::Cancelled);
        }
        let questions = request.questions.clone();
        let (response, receive_response) = oneshot::channel();
        self.sender
            .try_send(UserQuestionEnvelope { request, response })
            .map_err(|_| UserQuestionError::Unavailable)?;
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(UserQuestionError::Cancelled),
            response = receive_response => response.map_err(|_| UserQuestionError::Unavailable)?,
        };
        let Some(response_answers) = response.answers else {
            return Err(UserQuestionError::Cancelled);
        };
        if response_answers.len() != questions.len() {
            return Err(UserQuestionError::InvalidResponse);
        }
        let mut answers = Vec::new();
        answers
            .try_reserve_exact(questions.len())
            .map_err(|_| UserQuestionError::Unavailable)?;
        for (question, response) in questions.into_iter().zip(response_answers) {
            let (selected_indices, custom) = match response {
                UserQuestionResponseItem::Skipped => (Vec::new(), None),
                UserQuestionResponseItem::Selected(index) => (vec![index], None),
                UserQuestionResponseItem::MultiSelected(indices) => (indices, None),
                UserQuestionResponseItem::Custom(custom) if custom_answer_is_valid(&custom) => {
                    (Vec::new(), Some(custom))
                }
                UserQuestionResponseItem::MultiCustom { selected, custom }
                    if custom_answer_is_valid(&custom) =>
                {
                    (selected, Some(custom))
                }
                UserQuestionResponseItem::Custom(_) => {
                    return Err(UserQuestionError::InvalidResponse);
                }
                UserQuestionResponseItem::MultiCustom { .. } => {
                    return Err(UserQuestionError::InvalidResponse);
                }
            };
            let mut selected = Vec::new();
            selected
                .try_reserve_exact(selected_indices.len())
                .map_err(|_| UserQuestionError::Unavailable)?;
            for index in selected_indices {
                selected.push(
                    question
                        .options
                        .get(index)
                        .ok_or(UserQuestionError::InvalidResponse)?
                        .label
                        .clone(),
                );
            }
            answers.push(UserQuestionAnswerItem {
                id: question.id,
                selected,
                custom,
            });
        }
        Ok(UserQuestionAnswer { answers })
    }
}

fn response_is_valid(answer: &UserQuestionResponseItem, question: &UserQuestionItem) -> bool {
    match answer {
        UserQuestionResponseItem::Skipped => true,
        UserQuestionResponseItem::Selected(index) => {
            !question.multi_select && *index < question.options.len()
        }
        UserQuestionResponseItem::MultiSelected(indices) => {
            question.multi_select && indices_are_valid(indices, question)
        }
        UserQuestionResponseItem::Custom(custom) => custom_answer_is_valid(custom),
        UserQuestionResponseItem::MultiCustom { selected, custom } => {
            question.multi_select
                && custom_answer_is_valid(custom)
                && indices_are_unique_and_in_range(selected, question)
        }
    }
}

fn indices_are_valid(indices: &[usize], question: &UserQuestionItem) -> bool {
    !indices.is_empty() && indices_are_unique_and_in_range(indices, question)
}

fn indices_are_unique_and_in_range(indices: &[usize], question: &UserQuestionItem) -> bool {
    indices.iter().enumerate().all(|(position, index)| {
        *index < question.options.len() && !indices[..position].contains(index)
    })
}

pub(crate) fn custom_answer_is_valid(custom: &str) -> bool {
    !custom.is_empty()
        && custom.len() <= MAX_CUSTOM_ANSWER_BYTES
        && custom == custom.trim()
        && !custom
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
}

#[cfg(test)]
mod tests {
    use futures_util::poll;
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio_util::sync::CancellationToken;

    use super::{
        UserQuestionBroker, UserQuestionError, UserQuestionItem, UserQuestionOption,
        UserQuestionRequest, UserQuestionResponseItem,
    };

    fn item(id: &str) -> UserQuestionItem {
        UserQuestionItem::new(
            id.to_owned(),
            Some("Mode".to_owned()),
            "Which mode?".to_owned(),
            vec![
                UserQuestionOption::new("Safe".to_owned(), None),
                UserQuestionOption::new("Fast".to_owned(), None),
            ],
        )
    }

    fn request(id: &str) -> UserQuestionRequest {
        UserQuestionRequest::new(vec![item(id)])
    }

    #[tokio::test]
    async fn broker_is_lazy_and_returns_the_exact_displayed_label() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let future = broker.ask(request("mode"), CancellationToken::new());
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        let envelope = receiver.try_recv().unwrap();
        assert_eq!(envelope.request().questions()[0].id(), "mode");
        envelope
            .answer(vec![UserQuestionResponseItem::Selected(1)])
            .unwrap();
        let answer = future.await.unwrap();
        assert_eq!(answer.answers()[0].id(), "mode");
        assert_eq!(answer.answers()[0].selected(), ["Fast"]);
        assert_eq!(answer.answers()[0].custom(), None);
    }

    #[tokio::test]
    async fn broker_preserves_batch_order_and_rejects_a_short_response() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let request = UserQuestionRequest::new(vec![item("first"), item("second")]);
        let future = broker.ask(request, CancellationToken::new());
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        receiver
            .try_recv()
            .unwrap()
            .answer(vec![
                UserQuestionResponseItem::Selected(1),
                UserQuestionResponseItem::Selected(0),
            ])
            .unwrap();
        let answer = future.await.unwrap();
        assert_eq!(answer.answers()[0].id(), "first");
        assert_eq!(answer.answers()[0].selected(), ["Fast"]);
        assert_eq!(answer.answers()[1].id(), "second");
        assert_eq!(answer.answers()[1].selected(), ["Safe"]);

        let future = broker.ask(
            UserQuestionRequest::new(vec![item("first"), item("second")]),
            CancellationToken::new(),
        );
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        assert_eq!(
            receiver
                .try_recv()
                .unwrap()
                .answer(vec![UserQuestionResponseItem::Selected(0)]),
            Err(UserQuestionError::InvalidResponse)
        );
        assert_eq!(future.await, Err(UserQuestionError::Unavailable));
    }

    #[tokio::test]
    async fn cancellation_wins_and_closed_or_full_delivery_fails_closed() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            broker.ask(request("cancelled"), cancellation).await,
            Err(UserQuestionError::Cancelled)
        );

        let first = broker.ask(request("first"), CancellationToken::new());
        tokio::pin!(first);
        assert!(poll!(&mut first).is_pending());
        assert_eq!(
            broker.ask(request("full"), CancellationToken::new()).await,
            Err(UserQuestionError::Unavailable)
        );
        receiver.try_recv().unwrap().cancel().unwrap();
        assert_eq!(first.await, Err(UserQuestionError::Cancelled));

        drop(receiver);
        assert_eq!(
            broker
                .ask(request("closed"), CancellationToken::new())
                .await,
            Err(UserQuestionError::Unavailable)
        );
    }

    #[tokio::test]
    async fn custom_answers_are_trimmed_by_the_ui_and_replace_single_selection() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let request = UserQuestionRequest::new(vec![UserQuestionItem::new(
            "detail".to_owned(),
            None,
            "What should I do?".to_owned(),
            Vec::new(),
        )]);
        let future = broker.ask(request, CancellationToken::new());
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        receiver
            .try_recv()
            .unwrap()
            .answer(vec![UserQuestionResponseItem::Custom(
                "只跑必要检查".to_owned(),
            )])
            .unwrap();
        let answer = future.await.unwrap();
        assert!(answer.answers()[0].selected().is_empty());
        assert_eq!(answer.answers()[0].custom(), Some("只跑必要检查"));
    }

    #[tokio::test]
    async fn broker_rejects_blank_untrimmed_oversized_and_out_of_range_answers() {
        for response in [
            UserQuestionResponseItem::Custom(" ".to_owned()),
            UserQuestionResponseItem::Custom(" padded ".to_owned()),
            UserQuestionResponseItem::Custom("x".repeat(super::MAX_CUSTOM_ANSWER_BYTES + 1)),
            UserQuestionResponseItem::Selected(2),
        ] {
            let (broker, mut receiver) = UserQuestionBroker::new();
            let future = broker.ask(request("mode"), CancellationToken::new());
            tokio::pin!(future);
            assert!(poll!(&mut future).is_pending());
            assert_eq!(
                receiver.try_recv().unwrap().answer(vec![response]),
                Err(UserQuestionError::InvalidResponse)
            );
            assert_eq!(future.await, Err(UserQuestionError::Unavailable));
        }
    }

    #[tokio::test]
    async fn multi_select_preserves_toggle_order_and_supplements_with_custom_text() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let question = item("targets").with_multi_select(true);
        let future = broker.ask(
            UserQuestionRequest::new(vec![question]),
            CancellationToken::new(),
        );
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        receiver
            .try_recv()
            .unwrap()
            .answer(vec![UserQuestionResponseItem::MultiCustom {
                selected: vec![1, 0],
                custom: "release notes".to_owned(),
            }])
            .unwrap();
        let answer = future.await.unwrap();
        assert_eq!(answer.answers()[0].selected(), ["Fast", "Safe"]);
        assert_eq!(answer.answers()[0].custom(), Some("release notes"));
    }

    #[tokio::test]
    async fn broker_rejects_duplicate_empty_and_wrong_mode_selection_shapes() {
        for (question, response) in [
            (
                item("multi").with_multi_select(true),
                UserQuestionResponseItem::MultiSelected(Vec::new()),
            ),
            (
                item("multi").with_multi_select(true),
                UserQuestionResponseItem::MultiSelected(vec![0, 0]),
            ),
            (
                item("multi").with_multi_select(true),
                UserQuestionResponseItem::Selected(0),
            ),
            (
                item("single"),
                UserQuestionResponseItem::MultiSelected(vec![0, 1]),
            ),
        ] {
            let (broker, mut receiver) = UserQuestionBroker::new();
            let future = broker.ask(
                UserQuestionRequest::new(vec![question]),
                CancellationToken::new(),
            );
            tokio::pin!(future);
            assert!(poll!(&mut future).is_pending());
            assert_eq!(
                receiver.try_recv().unwrap().answer(vec![response]),
                Err(UserQuestionError::InvalidResponse)
            );
            assert_eq!(future.await, Err(UserQuestionError::Unavailable));
        }
    }
}
