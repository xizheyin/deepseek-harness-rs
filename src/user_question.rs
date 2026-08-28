use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_QUESTION_ID_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_HEADER_BYTES: usize = 64;
pub(crate) const MAX_QUESTION_TEXT_BYTES: usize = 512;
pub(crate) const MAX_QUESTION_OPTION_LABEL_BYTES: usize = 128;
pub(crate) const MAX_QUESTION_OPTION_DESCRIPTION_BYTES: usize = 256;
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
pub(crate) struct UserQuestionRequest {
    id: String,
    header: Option<String>,
    question: String,
    options: Vec<UserQuestionOption>,
}

impl UserQuestionRequest {
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

    #[cfg(test)]
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
pub(crate) struct UserQuestionAnswer {
    id: String,
    selected: String,
}

impl UserQuestionAnswer {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn selected(&self) -> &str {
        &self.selected
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
    id: String,
    selected_index: Option<usize>,
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

    pub(crate) fn select(self, selected_index: usize) -> Result<(), UserQuestionError> {
        if selected_index >= self.request.options.len() {
            return Err(UserQuestionError::InvalidResponse);
        }
        self.response
            .send(UserQuestionResponse {
                id: self.request.id,
                selected_index: Some(selected_index),
            })
            .map_err(|_| UserQuestionError::Unavailable)
    }

    pub(crate) fn cancel(self) -> Result<(), UserQuestionError> {
        self.response
            .send(UserQuestionResponse {
                id: self.request.id,
                selected_index: None,
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
        let expected_id = request.id.clone();
        let options = request.options.clone();
        let (response, receive_response) = oneshot::channel();
        self.sender
            .try_send(UserQuestionEnvelope { request, response })
            .map_err(|_| UserQuestionError::Unavailable)?;
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(UserQuestionError::Cancelled),
            response = receive_response => response.map_err(|_| UserQuestionError::Unavailable)?,
        };
        if response.id != expected_id {
            return Err(UserQuestionError::InvalidResponse);
        }
        let Some(selected_index) = response.selected_index else {
            return Err(UserQuestionError::Cancelled);
        };
        let selected = options
            .get(selected_index)
            .ok_or(UserQuestionError::InvalidResponse)?
            .label
            .clone();
        Ok(UserQuestionAnswer {
            id: expected_id,
            selected,
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::poll;
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio_util::sync::CancellationToken;

    use super::{UserQuestionBroker, UserQuestionError, UserQuestionOption, UserQuestionRequest};

    fn request(id: &str) -> UserQuestionRequest {
        UserQuestionRequest::new(
            id.to_owned(),
            Some("Mode".to_owned()),
            "Which mode?".to_owned(),
            vec![
                UserQuestionOption::new("Safe".to_owned(), None),
                UserQuestionOption::new("Fast".to_owned(), None),
            ],
        )
    }

    #[tokio::test]
    async fn broker_is_lazy_and_returns_the_exact_displayed_label() {
        let (broker, mut receiver) = UserQuestionBroker::new();
        let future = broker.ask(request("mode"), CancellationToken::new());
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        tokio::pin!(future);
        assert!(poll!(&mut future).is_pending());
        let envelope = receiver.try_recv().unwrap();
        assert_eq!(envelope.request().id(), "mode");
        envelope.select(1).unwrap();
        assert_eq!(
            future.await.unwrap(),
            super::UserQuestionAnswer {
                id: "mode".to_owned(),
                selected: "Fast".to_owned(),
            }
        );
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
