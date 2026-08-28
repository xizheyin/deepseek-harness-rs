use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_QUESTION_ID_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_HEADER_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_TEXT_BYTES: usize = 512;
pub(crate) const MAX_QUESTION_OPTION_LABEL_BYTES: usize = 128;
pub(crate) const MAX_QUESTION_OPTION_DESCRIPTION_BYTES: usize = 256;
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
        }
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
    selected: String,
}

impl UserQuestionAnswerItem {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn selected(&self) -> &str {
        &self.selected
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
    selected_indices: Option<Vec<usize>>,
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

    pub(crate) fn answer(self, selected_indices: Vec<usize>) -> Result<(), UserQuestionError> {
        if selected_indices.len() != self.request.questions.len()
            || selected_indices
                .iter()
                .zip(&self.request.questions)
                .any(|(index, question)| *index >= question.options.len())
        {
            return Err(UserQuestionError::InvalidResponse);
        }
        self.response
            .send(UserQuestionResponse {
                selected_indices: Some(selected_indices),
            })
            .map_err(|_| UserQuestionError::Unavailable)
    }

    pub(crate) fn cancel(self) -> Result<(), UserQuestionError> {
        self.response
            .send(UserQuestionResponse {
                selected_indices: None,
            })
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
        let Some(selected_indices) = response.selected_indices else {
            return Err(UserQuestionError::Cancelled);
        };
        if selected_indices.len() != questions.len() {
            return Err(UserQuestionError::InvalidResponse);
        }
        let mut answers = Vec::new();
        answers
            .try_reserve_exact(questions.len())
            .map_err(|_| UserQuestionError::Unavailable)?;
        for (question, selected_index) in questions.into_iter().zip(selected_indices) {
            let selected = question
                .options
                .get(selected_index)
                .ok_or(UserQuestionError::InvalidResponse)?
                .label
                .clone();
            answers.push(UserQuestionAnswerItem {
                id: question.id,
                selected,
            });
        }
        Ok(UserQuestionAnswer { answers })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::poll;
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio_util::sync::CancellationToken;

    use super::{
        UserQuestionBroker, UserQuestionError, UserQuestionItem, UserQuestionOption,
        UserQuestionRequest,
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
        envelope.answer(vec![1]).unwrap();
        let answer = future.await.unwrap();
        assert_eq!(answer.answers()[0].id(), "mode");
        assert_eq!(answer.answers()[0].selected(), "Fast");
    }

    #[tokio::test]
    async fn broker_preserves_batch_order_and_rejects_a_short_response() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let request = UserQuestionRequest::new(vec![item("first"), item("second")]);
        let future = broker.ask(request, CancellationToken::new());
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        receiver.try_recv().unwrap().answer(vec![1, 0]).unwrap();
        let answer = future.await.unwrap();
        assert_eq!(answer.answers()[0].id(), "first");
        assert_eq!(answer.answers()[0].selected(), "Fast");
        assert_eq!(answer.answers()[1].id(), "second");
        assert_eq!(answer.answers()[1].selected(), "Safe");

        let future = broker.ask(
            UserQuestionRequest::new(vec![item("first"), item("second")]),
            CancellationToken::new(),
        );
        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        assert_eq!(
            receiver.try_recv().unwrap().answer(vec![0]),
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
}
