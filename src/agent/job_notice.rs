//! Bounded handoff from process-local jobs into ordinary Agent input.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use tokio::sync::Notify;

use crate::model::{ContentBlock, Message, MessageSource};

use super::{AgentIdKind, AgentRuntime, next_id};

const MAX_PENDING_JOB_NOTICES: usize = 64;
const MAX_CONSECUTIVE_JOB_WAKES: usize = 3;
const MAX_NOTICE_FIELD_BYTES: usize = 1_024;
const MAX_NOTICE_SUMMARY_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackgroundJobNotice {
    job_id: String,
    kind: String,
    label: String,
    status: String,
    detail: Option<String>,
}

impl BackgroundJobNotice {
    pub(crate) fn new(
        job_id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
        status: impl Into<String>,
        detail: Option<String>,
    ) -> Self {
        Self {
            job_id: bounded_visible(job_id.into(), MAX_NOTICE_FIELD_BYTES),
            kind: bounded_visible(kind.into(), MAX_NOTICE_FIELD_BYTES),
            label: bounded_visible(label.into(), MAX_NOTICE_FIELD_BYTES),
            status: bounded_visible(status.into(), MAX_NOTICE_FIELD_BYTES),
            detail: detail.map(|value| bounded_visible(value, MAX_NOTICE_FIELD_BYTES)),
        }
    }

    pub(crate) fn job_id(&self) -> &str {
        &self.job_id
    }

    fn status_line(&self) -> String {
        self.detail.as_ref().map_or_else(
            || format!("[status: {}]", self.status),
            |detail| format!("[status: {}, {detail}]", self.status),
        )
    }

    fn text(&self) -> String {
        format!(
            "background job {} ({}: {}) finished {}. Read its output with job_output.",
            self.job_id,
            self.kind,
            self.label,
            self.status_line()
        )
    }

    fn summary(&self) -> String {
        bounded_visible(
            format!("{} {} {}", self.kind, self.label, self.status_line()),
            MAX_NOTICE_SUMMARY_BYTES,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingJobNotice {
    Job(BackgroundJobNotice),
    Overflow { omitted: usize },
}

impl PendingJobNotice {
    fn text(&self) -> String {
        match self {
            Self::Job(notice) => notice.text(),
            Self::Overflow { omitted } => format!(
                "{omitted} additional background jobs finished while completion notices were full. Use job_list and job_output to inspect retained jobs."
            ),
        }
    }

    fn summary(&self) -> String {
        match self {
            Self::Job(notice) => notice.summary(),
            Self::Overflow { omitted } => {
                format!("background job completion overflow × {omitted}")
            }
        }
    }
}

#[derive(Default)]
struct JobNoticeState {
    pending: VecDeque<BackgroundJobNotice>,
    omitted: usize,
    active_turn: bool,
    consecutive_wakes: usize,
    closed: bool,
}

struct JobNoticeInner {
    state: Mutex<JobNoticeState>,
    changed: Notify,
}

/// Process-local, bounded inbox shared by the job registry and one Agent.
#[derive(Clone)]
pub(crate) struct JobNoticeInbox {
    inner: Arc<JobNoticeInner>,
}

impl Default for JobNoticeInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for JobNoticeInbox {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobNoticeInbox")
            .field("maximum_pending", &MAX_PENDING_JOB_NOTICES)
            .field("maximum_consecutive_wakes", &MAX_CONSECUTIVE_JOB_WAKES)
            .finish_non_exhaustive()
    }
}

impl JobNoticeInbox {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(JobNoticeInner {
                state: Mutex::new(JobNoticeState::default()),
                changed: Notify::new(),
            }),
        }
    }

    pub(crate) fn enqueue(&self, notice: BackgroundJobNotice) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if state.pending.len() < MAX_PENDING_JOB_NOTICES {
            state.pending.push_back(notice);
        } else {
            state.omitted = state.omitted.saturating_add(1);
        }
        let should_wake = can_wake(&state);
        drop(state);
        if should_wake {
            self.inner.changed.notify_one();
        }
    }

    pub(crate) fn suppress_job(&self, job_id: &str) {
        let mut state = self.lock();
        state.pending.retain(|notice| notice.job_id() != job_id);
    }

    /// Mark an ordinary turn active and claim older pending notices for it.
    pub(crate) fn begin_turn(&self) -> Vec<PendingJobNotice> {
        let mut state = self.lock();
        state.active_turn = true;
        drain(&mut state)
    }

    pub(crate) fn observe_direct_human_claim(&self) {
        self.lock().consecutive_wakes = 0;
    }

    /// Claim notices at a normal step boundary while retaining active state.
    pub(crate) fn claim_for_active_step(&self) -> Vec<PendingJobNotice> {
        let mut state = self.lock();
        if !state.active_turn {
            return Vec::new();
        }
        drain(&mut state)
    }

    /// Atomically keep a completed turn active when another notice is pending.
    pub(crate) fn continue_after_completed_step(&self) -> bool {
        let mut state = self.lock();
        let pending = !state.pending.is_empty() || state.omitted != 0;
        if !pending {
            state.active_turn = false;
        }
        pending
    }

    pub(crate) fn finish_turn(&self) {
        let mut state = self.lock();
        state.active_turn = false;
        let should_wake = can_wake(&state);
        drop(state);
        if should_wake {
            self.inner.changed.notify_one();
        }
    }

    pub(crate) async fn wait_for_idle_wake(&self) {
        loop {
            let changed = self.inner.changed.notified();
            if can_wake(&self.lock()) {
                return;
            }
            changed.await;
        }
    }

    /// Reserve one automatic turn and spend one wake from the human-reset budget.
    pub(crate) fn claim_idle_wake(&self) -> Option<Vec<PendingJobNotice>> {
        let mut state = self.lock();
        if !can_wake(&state) {
            return None;
        }
        state.active_turn = true;
        state.consecutive_wakes = state.consecutive_wakes.saturating_add(1);
        Some(drain(&mut state))
    }

    pub(crate) fn restore_claimed(&self, notices: Vec<PendingJobNotice>) {
        self.restore(notices, true);
    }

    pub(crate) fn restore_active(&self, notices: Vec<PendingJobNotice>) {
        self.restore(notices, false);
    }

    fn restore(&self, notices: Vec<PendingJobNotice>, spent_wake: bool) {
        let mut state = self.lock();
        for notice in notices.into_iter().rev() {
            match notice {
                PendingJobNotice::Job(notice) => state.pending.push_front(notice),
                PendingJobNotice::Overflow { omitted } => {
                    state.omitted = state.omitted.saturating_add(omitted);
                }
            }
        }
        state.active_turn = false;
        if spent_wake {
            state.consecutive_wakes = state.consecutive_wakes.saturating_sub(1);
        }
        let should_wake = can_wake(&state);
        drop(state);
        if should_wake {
            self.inner.changed.notify_one();
        }
    }

    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.pending.clear();
        state.omitted = 0;
        state.active_turn = false;
        drop(state);
        self.inner.changed.notify_waiters();
    }

    fn lock(&self) -> MutexGuard<'_, JobNoticeState> {
        // A poisoned bookkeeping lock must not turn job cleanup into a panic.
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn can_wake(state: &JobNoticeState) -> bool {
    !state.closed
        && !state.active_turn
        && (!state.pending.is_empty() || state.omitted != 0)
        && state.consecutive_wakes < MAX_CONSECUTIVE_JOB_WAKES
}

fn drain(state: &mut JobNoticeState) -> Vec<PendingJobNotice> {
    let mut notices = state
        .pending
        .drain(..)
        .map(PendingJobNotice::Job)
        .collect::<Vec<_>>();
    if state.omitted != 0 {
        notices.push(PendingJobNotice::Overflow {
            omitted: std::mem::take(&mut state.omitted),
        });
    }
    notices
}

pub(crate) fn messages_for_notices(
    notices: &[PendingJobNotice],
    runtime: &dyn AgentRuntime,
) -> Result<Vec<Message>, super::AgentLoopError> {
    notices
        .iter()
        .map(|notice| -> Result<Message, super::AgentLoopError> {
            let source = MessageSource::plugin_notice("tool-jobs", notice.summary())?;
            let content = ContentBlock::text(notice.text())?;
            Ok(Message::user(
                next_id(runtime, AgentIdKind::Message)?,
                vec![content],
                source,
            )?)
        })
        .collect()
}

fn bounded_visible(mut value: String, maximum: usize) -> String {
    value = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum.saturating_sub('…'.len_utf8());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push('…');
    value
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        BackgroundJobNotice, JobNoticeInbox, MAX_CONSECUTIVE_JOB_WAKES, MAX_PENDING_JOB_NOTICES,
        PendingJobNotice, messages_for_notices,
    };
    use crate::agent::{AgentIdKind, AgentRuntime, AgentRuntimeError};

    #[derive(Default)]
    struct Runtime(AtomicUsize);

    impl AgentRuntime for Runtime {
        fn next_id(&self, kind: AgentIdKind) -> Result<String, AgentRuntimeError> {
            Ok(format!(
                "{}-{}",
                kind.prefix(),
                self.0.fetch_add(1, Ordering::Relaxed)
            ))
        }

        fn sample_unit(&self) -> Result<f64, AgentRuntimeError> {
            Ok(0.5)
        }
    }

    fn notice(index: usize) -> BackgroundJobNotice {
        BackgroundJobNotice::new(
            format!("bash-{index}"),
            "bash",
            "pnpm test",
            "completed",
            Some("exit code: 0".to_owned()),
        )
    }

    #[test]
    fn exact_notice_and_source_match_the_fixed_fixture() {
        let messages =
            messages_for_notices(&[PendingJobNotice::Job(notice(1))], &Runtime::default()).unwrap();
        assert_eq!(
            messages[0].content()[0].raw().as_value()["text"],
            "background job bash-1 (bash: pnpm test) finished [status: completed, exit code: 0]. Read its output with job_output."
        );
        assert_eq!(
            messages[0].source().raw().as_value(),
            &serde_json::json!({
                "kind": "plugin",
                "plugin": "tool-jobs",
                "form": "notice",
                "summary": "bash pnpm test [status: completed, exit code: 0]"
            })
        );
    }

    #[test]
    fn wake_budget_resets_only_for_direct_human_input() {
        let inbox = JobNoticeInbox::new();
        for index in 1..=MAX_CONSECUTIVE_JOB_WAKES {
            inbox.enqueue(notice(index));
            assert!(inbox.claim_idle_wake().is_some());
            inbox.finish_turn();
        }
        inbox.enqueue(notice(9));
        assert!(inbox.claim_idle_wake().is_none());

        assert_eq!(inbox.begin_turn().len(), 1);
        inbox.finish_turn();
        inbox.enqueue(notice(10));
        assert!(inbox.claim_idle_wake().is_none());

        assert_eq!(inbox.begin_turn().len(), 1);
        inbox.observe_direct_human_claim();
        inbox.finish_turn();
        inbox.enqueue(notice(11));
        assert!(inbox.claim_idle_wake().is_some());
    }

    #[test]
    fn pending_queue_is_bounded_and_reports_overflow() {
        let inbox = JobNoticeInbox::new();
        for index in 1..=MAX_PENDING_JOB_NOTICES + 7 {
            inbox.enqueue(notice(index));
        }
        let claimed = inbox.begin_turn();
        assert_eq!(claimed.len(), MAX_PENDING_JOB_NOTICES + 1);
        assert!(matches!(
            claimed.last(),
            Some(PendingJobNotice::Overflow { omitted: 7 })
        ));
    }

    #[test]
    fn completed_step_atomically_claims_work_or_marks_idle() {
        let inbox = JobNoticeInbox::new();
        assert!(inbox.begin_turn().is_empty());
        inbox.enqueue(notice(1));
        assert!(inbox.continue_after_completed_step());
        assert_eq!(inbox.claim_for_active_step().len(), 1);
        assert!(!inbox.continue_after_completed_step());
        inbox.enqueue(notice(2));
        assert!(inbox.claim_idle_wake().is_some());
    }

    #[test]
    fn suppression_and_close_remove_pending_delivery() {
        let inbox = JobNoticeInbox::new();
        inbox.enqueue(notice(1));
        inbox.suppress_job("bash-1");
        assert!(inbox.claim_idle_wake().is_none());
        inbox.enqueue(notice(2));
        inbox.close();
        assert!(inbox.claim_idle_wake().is_none());
    }
}
