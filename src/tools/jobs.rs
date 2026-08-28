//! Bounded process-local ownership for background Shell jobs.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::Mutex, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{BackgroundJobNotice, JobNoticeInbox, ToolExecutionResult, ToolExecutorError},
    model::{ContentBlock, JsonValue, ToolSchema},
};

use super::{
    MAX_TOOL_CONTENT_BYTES,
    error::{ToolCallError, ToolCallResult, ToolRegistryBuildError},
    json_string_content_bytes,
    process::{
        ProcessControl, ProcessOutcome, ProcessOutputCursor, ProcessOutputRead, ProcessOutputTap,
        ProcessPrimaryCause, ProcessRequest, ProcessRunner, ProcessStartFailure,
        ProcessTermination, RETAINED_TAIL_BYTES,
    },
    text_block_encoded_bytes,
};

pub(crate) const JOB_OUTPUT_TOOL_NAME: &str = "job_output";
pub(crate) const JOB_LIST_TOOL_NAME: &str = "job_list";
pub(crate) const JOB_KILL_TOOL_NAME: &str = "job_kill";

const MAX_ACTIVE_JOBS: usize = 8;
const MAX_RETAINED_JOBS: usize = 64;
const MAX_JOB_ID_BYTES: usize = 64;
const MAX_JOB_LABEL_BYTES: usize = 240;
const MAX_JOB_REASON_BYTES: usize = 512;
const DEFAULT_JOB_WAIT_MS: u64 = 30_000;
const MAX_JOB_WAIT_MS: u64 = 295_000;
const PROCESS_SETTLEMENT_MARGIN: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobStatus {
    Running,
    Stopping,
    Completed,
    Killed,
    Failed,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Completed => "completed",
            Self::Killed => "killed",
            Self::Failed => "failed",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Killed | Self::Failed)
    }
}

struct JobRecord {
    id: String,
    label: String,
    status: JobStatus,
    detail: Option<String>,
    output_tap: ProcessOutputTap,
    output_cursor: ProcessOutputCursor,
    fallback_output: Option<String>,
    started_at: u64,
    finished_at: Option<u64>,
    cancellation: CancellationToken,
    changed: tokio::sync::watch::Sender<JobStatus>,
    waiters: Arc<AtomicUsize>,
    reported: bool,
}

#[derive(Clone)]
struct JobSnapshot {
    id: String,
    label: String,
    status: JobStatus,
    detail: Option<String>,
    started_at: u64,
    finished_at: Option<u64>,
}

impl JobSnapshot {
    fn status_line(&self) -> String {
        match &self.detail {
            Some(detail) => format!("[status: {}, {detail}]", self.status.as_str()),
            None => format!("[status: {}]", self.status.as_str()),
        }
    }

    fn public_json(&self) -> Value {
        let mut value = json!({
            "id": self.id,
            "kind": "bash",
            "label": self.label,
            "status": self.status.as_str(),
            "startedAt": self.started_at
        });
        if let Some(detail) = &self.detail {
            value["detail"] = json!(detail);
        }
        if let Some(finished_at) = self.finished_at {
            value["finishedAt"] = json!(finished_at);
        }
        value
    }
}

struct JobState {
    next_id: u64,
    jobs: VecDeque<JobRecord>,
    monitors: Vec<JoinHandle<()>>,
    closed: bool,
}

struct JobInner {
    state: Mutex<JobState>,
    notices: JobNoticeInbox,
}

/// One CLI-owned, process-local background-job registry.
#[derive(Clone)]
pub(crate) struct BackgroundJobRuntime {
    inner: Arc<JobInner>,
}

impl std::fmt::Debug for BackgroundJobRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackgroundJobRuntime")
            .field("maximum_active_jobs", &MAX_ACTIVE_JOBS)
            .field("maximum_retained_jobs", &MAX_RETAINED_JOBS)
            .finish_non_exhaustive()
    }
}

impl BackgroundJobRuntime {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(JobInner {
                state: Mutex::new(JobState {
                    next_id: 0,
                    jobs: VecDeque::new(),
                    monitors: Vec::new(),
                    closed: false,
                }),
                notices: JobNoticeInbox::new(),
            }),
        }
    }

    pub(crate) fn notices(&self) -> JobNoticeInbox {
        self.inner.notices.clone()
    }

    pub(crate) async fn start_shell(
        &self,
        command: &str,
        timeout_ms: u64,
        request: ProcessRequest,
        runner: Arc<ProcessRunner>,
    ) -> Result<String, ToolExecutorError> {
        let output_tap = ProcessOutputTap::new(RETAINED_TAIL_BYTES);
        let request = request.with_output_tap(output_tap.clone());
        let mut state = self.inner.state.lock().await;
        if state.closed {
            return Err(ToolExecutorError::new(
                "background job runtime is shutting down",
            ));
        }
        let active = state
            .jobs
            .iter()
            .filter(|job| !job.status.is_terminal())
            .count();
        if active >= MAX_ACTIVE_JOBS {
            return Err(ToolExecutorError::new(
                "background job active limit reached",
            ));
        }
        while state.jobs.len() >= MAX_RETAINED_JOBS {
            let Some(index) = state.jobs.iter().position(|job| job.status.is_terminal()) else {
                return Err(ToolExecutorError::new(
                    "background job retained-record limit reached",
                ));
            };
            state.jobs.remove(index);
        }

        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| ToolExecutorError::new("background job id space exhausted"))?;
        let id = format!("bash-{}", state.next_id);
        let cancellation = CancellationToken::new();
        let (changed, _) = tokio::sync::watch::channel(JobStatus::Running);
        state.jobs.push_back(JobRecord {
            id: id.clone(),
            label: bounded_label(command),
            status: JobStatus::Running,
            detail: None,
            output_tap,
            output_cursor: ProcessOutputCursor::default(),
            fallback_output: None,
            started_at: epoch_millis(),
            finished_at: None,
            cancellation: cancellation.clone(),
            changed,
            waiters: Arc::new(AtomicUsize::new(0)),
            reported: false,
        });

        let runtime = self.clone();
        let task_id = id.clone();
        let monitor = tokio::spawn(async move {
            let deadline =
                Instant::now() + Duration::from_millis(timeout_ms) + PROCESS_SETTLEMENT_MARGIN;
            let worker = tokio::spawn(async move {
                runner
                    .run(
                        request,
                        ProcessControl::new(cancellation, deadline, deadline),
                    )
                    .await
            });
            let completion = match worker.await {
                Ok(outcome) => completion_from_process(outcome, timeout_ms),
                Err(_) => JobCompletion::failed(
                    "background process monitor failed",
                    "Error: background process monitor failed",
                ),
            };
            runtime.finish(&task_id, completion).await;
        });
        state.monitors.push(monitor);
        Ok(id)
    }

    async fn finish(&self, id: &str, completion: JobCompletion) {
        let mut state = self.inner.state.lock().await;
        let closed = state.closed;
        let Some(job) = state.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        if job.status.is_terminal() {
            return;
        }
        job.status = completion.status;
        job.detail = Some(completion.detail);
        job.fallback_output = completion.fallback_output;
        job.finished_at = Some(epoch_millis());
        job.changed.send_replace(job.status);
        let notice =
            (!closed && !job.reported && job.waiters.load(Ordering::Acquire) == 0).then(|| {
                job.reported = true;
                BackgroundJobNotice::new(
                    job.id.clone(),
                    "bash",
                    job.label.clone(),
                    job.status.as_str(),
                    job.detail.clone(),
                )
            });
        drop(state);
        if let Some(notice) = notice {
            self.inner.notices.enqueue(notice);
        }
    }

    pub(crate) async fn execute(
        &self,
        name: &str,
        arguments: &Value,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionResult, ToolExecutorError> {
        match name {
            JOB_LIST_TOOL_NAME => self.list(arguments).await,
            JOB_OUTPUT_TOOL_NAME => self.output(arguments, cancellation).await,
            JOB_KILL_TOOL_NAME => self.kill(arguments).await,
            _ => ToolCallError::unknown_tool().into_execution_result(),
        }
    }

    async fn list(&self, arguments: &Value) -> Result<ToolExecutionResult, ToolExecutorError> {
        if let Err(error) = parse_empty(arguments) {
            return error.into_execution_result();
        }
        let state = self.inner.state.lock().await;
        let snapshots = state.jobs.iter().map(snapshot).collect::<Vec<_>>();
        drop(state);
        let text = if snapshots.is_empty() {
            "(no background jobs)".to_owned()
        } else {
            snapshots
                .iter()
                .map(|job| format!("{} [bash] {} — {}", job.id, job.status.as_str(), job.label))
                .collect::<Vec<_>>()
                .join("\n")
        };
        success_with_meta(
            text,
            json!({
                "kind": "job-list",
                "jobs": snapshots.iter().map(JobSnapshot::public_json).collect::<Vec<_>>()
            }),
        )
    }

    async fn output(
        &self,
        arguments: &Value,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionResult, ToolExecutorError> {
        let args = match parse_output(arguments) {
            Ok(args) => args,
            Err(error) => return error.into_execution_result(),
        };
        if args.wait {
            match self
                .wait(&args.job_id, args.timeout_ms, &cancellation)
                .await
            {
                Ok(_) => {}
                Err(error) => return error.into_execution_result(),
            }
        }
        let (snapshot, delta) = match self.read(&args.job_id).await {
            Ok(read) => read,
            Err(error) => return error.into_execution_result(),
        };
        let body = if delta.is_empty() {
            "(no new output)"
        } else {
            delta.as_str()
        };
        let separator = if body.ends_with('\n') { "" } else { "\n" };
        let suffix = format!("{separator}{}", snapshot.status_line());
        let text = fit_with_suffix(body, &suffix);
        success_with_meta(
            text,
            json!({
                "kind": "job-output",
                "textFinal": snapshot.status.is_terminal(),
                "job": snapshot.public_json()
            }),
        )
    }

    async fn wait(
        &self,
        id: &str,
        timeout_ms: u64,
        cancellation: &CancellationToken,
    ) -> ToolCallResult<JobSnapshot> {
        validate_job_id(id)?;
        let waiters = {
            let state = self.inner.state.lock().await;
            let job = expect_job(&state, id)?;
            Arc::clone(&job.waiters)
        };
        waiters.fetch_add(1, Ordering::AcqRel);
        let _waiter = JobWaiter { waiters };
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let (current, mut changed) = {
                let state = self.inner.state.lock().await;
                let job = expect_job(&state, id)?;
                (snapshot(job), job.changed.subscribe())
            };
            if current.status.is_terminal() || Instant::now() >= deadline {
                return Ok(current);
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    let latest = self.get(id).await?;
                    if latest.status.is_terminal() {
                        return Ok(latest);
                    }
                    return Err(ToolCallError::aborted());
                }
                _ = tokio::time::sleep_until(deadline) => return self.get(id).await,
                changed = changed.changed() => {
                    if changed.is_err() {
                        return Err(ToolCallError::model(
                            "JobError",
                            "JOB_UNAVAILABLE",
                            "background job state became unavailable",
                        ));
                    }
                }
            }
        }
    }

    async fn get(&self, id: &str) -> ToolCallResult<JobSnapshot> {
        validate_job_id(id)?;
        let state = self.inner.state.lock().await;
        expect_job(&state, id).map(snapshot)
    }

    async fn read(&self, id: &str) -> ToolCallResult<(JobSnapshot, String)> {
        validate_job_id(id)?;
        let mut state = self.inner.state.lock().await;
        let job = state
            .jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or_else(|| unknown_job(id))?;
        let read = job.output_tap.read(&mut job.output_cursor);
        let mut text = render_process_read(&read);
        if let Some(fallback) = job.fallback_output.take() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&fallback);
        }
        let terminal = job.status.is_terminal();
        if terminal {
            job.reported = true;
        }
        let snapshot = snapshot(job);
        drop(state);
        if terminal {
            self.inner.notices.suppress_job(id);
        }
        Ok((snapshot, text))
    }

    async fn kill(&self, arguments: &Value) -> Result<ToolExecutionResult, ToolExecutorError> {
        let args = match parse_kill(arguments) {
            Ok(args) => args,
            Err(error) => return error.into_execution_result(),
        };
        let mut state = self.inner.state.lock().await;
        let job = match state.jobs.iter_mut().find(|job| job.id == args.job_id) {
            Some(job) => job,
            None => return unknown_job(&args.job_id).into_execution_result(),
        };
        job.reported = true;
        if job.status.is_terminal() {
            let snapshot = snapshot(job);
            drop(state);
            self.inner.notices.suppress_job(&args.job_id);
            return success_with_meta(
                format!(
                    "job {} had already finished {}",
                    snapshot.id,
                    snapshot.status_line()
                ),
                json!({
                    "kind": "job-kill",
                    "outcome": "already-finished",
                    "job": snapshot.public_json()
                }),
            );
        }
        job.cancellation.cancel();
        job.status = JobStatus::Stopping;
        job.changed.send_replace(job.status);
        let snapshot = snapshot(job);
        drop(state);
        self.inner.notices.suppress_job(&args.job_id);
        success_with_meta(
            format!("requested cancellation of job {}", snapshot.id),
            json!({
                "kind": "job-kill",
                "outcome": "cancellation-requested",
                "reason": args.reason,
                "job": snapshot.public_json()
            }),
        )
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ToolExecutorError> {
        let monitors = {
            let mut state = self.inner.state.lock().await;
            state.closed = true;
            for job in &mut state.jobs {
                job.reported = true;
                if !job.status.is_terminal() {
                    job.cancellation.cancel();
                    job.status = JobStatus::Stopping;
                    job.changed.send_replace(job.status);
                }
            }
            std::mem::take(&mut state.monitors)
        };
        self.inner.notices.close();
        let mut failed = false;
        for monitor in monitors {
            failed |= monitor.await.is_err();
        }
        if failed {
            Err(ToolExecutorError::new(
                "background job monitor shutdown failed",
            ))
        } else {
            Ok(())
        }
    }
}

struct JobWaiter {
    waiters: Arc<AtomicUsize>,
}

impl Drop for JobWaiter {
    fn drop(&mut self) {
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

fn snapshot(job: &JobRecord) -> JobSnapshot {
    JobSnapshot {
        id: job.id.clone(),
        label: job.label.clone(),
        status: job.status,
        detail: job.detail.clone(),
        started_at: job.started_at,
        finished_at: job.finished_at,
    }
}

fn expect_job<'a>(state: &'a JobState, id: &str) -> ToolCallResult<&'a JobRecord> {
    validate_job_id(id)?;
    state
        .jobs
        .iter()
        .find(|job| job.id == id)
        .ok_or_else(|| unknown_job(id))
}

struct JobCompletion {
    status: JobStatus,
    detail: String,
    fallback_output: Option<String>,
}

impl JobCompletion {
    fn failed(detail: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            status: JobStatus::Failed,
            detail: detail.into(),
            fallback_output: Some(output.into()),
        }
    }
}

fn completion_from_process(outcome: ProcessOutcome, timeout_ms: u64) -> JobCompletion {
    match outcome {
        ProcessOutcome::NotStarted { cause, .. } => {
            if cause == ProcessStartFailure::CallerCancelled {
                return JobCompletion {
                    status: JobStatus::Killed,
                    detail: "cancelled before process creation".to_owned(),
                    fallback_output: Some(
                        "Error: background shell was cancelled before process creation".to_owned(),
                    ),
                };
            }
            JobCompletion::failed(
                start_failure_detail(cause),
                format!(
                    "Error: background shell could not start ({})",
                    start_failure_detail(cause)
                ),
            )
        }
        ProcessOutcome::StartedOwnershipLost { .. } => JobCompletion::failed(
            "process ownership lost",
            "Error: background shell process ownership was lost during cleanup",
        ),
        ProcessOutcome::StartedAndQuiescent(report) => {
            let primary = report.primary();
            let termination = report.termination();
            let (status, detail) = match primary {
                ProcessPrimaryCause::Natural => {
                    (JobStatus::Completed, termination_detail(termination))
                }
                ProcessPrimaryCause::CallerCancelled => (
                    JobStatus::Killed,
                    termination.canonical_signal_name().map_or_else(
                        || "cancelled".to_owned(),
                        |signal| format!("signal: {signal}"),
                    ),
                ),
                ProcessPrimaryCause::CommandTimeout => {
                    (JobStatus::Failed, format!("timed out after {timeout_ms}ms"))
                }
                other => (JobStatus::Failed, primary_detail(other).to_owned()),
            };
            JobCompletion {
                status,
                detail,
                fallback_output: None,
            }
        }
    }
}

fn render_process_read(read: &ProcessOutputRead) -> String {
    let stdout = String::from_utf8_lossy(read.stdout());
    let stderr = String::from_utf8_lossy(read.stderr());
    let mut output = stdout.into_owned();
    if !stderr.is_empty() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(&stderr);
    }
    if read.lossy() {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        let paths = [read.stdout_spill(), read.stderr_spill()]
            .into_iter()
            .flatten()
            .filter_map(Path::to_str)
            .collect::<Vec<_>>();
        let availability = if paths.is_empty() {
            "(unavailable)".to_owned()
        } else {
            paths.join(", ")
        };
        let kind = if read.spill_is_full() {
            "full output"
        } else {
            "captured output"
        };
        output.push_str(&format!(
            "[some output was dropped from memory; {kind}: {availability}]"
        ));
    }
    output
}

fn termination_detail(termination: ProcessTermination) -> String {
    match termination {
        ProcessTermination::ExitCode(code) => format!("exit code: {code}"),
        signal @ ProcessTermination::Signal(_) => signal.canonical_signal_name().map_or_else(
            || "killed by signal".to_owned(),
            |name| format!("signal: {name}"),
        ),
    }
}

fn start_failure_detail(cause: ProcessStartFailure) -> &'static str {
    match cause {
        ProcessStartFailure::CallerCancelled => "cancelled before process creation",
        ProcessStartFailure::TurnTimeout => "owner deadline expired before process creation",
        ProcessStartFailure::ActionTimeout => "action deadline expired before process creation",
        ProcessStartFailure::ObserverUnavailable => "process observer unavailable",
        ProcessStartFailure::AsyncRuntimeUnavailable => "async runtime unavailable",
        ProcessStartFailure::PipePreflightFailed => "output pipe preflight failed",
        ProcessStartFailure::SpawnFailed => "process spawn failed",
    }
}

fn primary_detail(primary: ProcessPrimaryCause) -> &'static str {
    match primary {
        ProcessPrimaryCause::Natural => "completed",
        ProcessPrimaryCause::CallerCancelled => "cancelled",
        ProcessPrimaryCause::TurnTimeout => "owner deadline expired",
        ProcessPrimaryCause::ActionTimeout => "action deadline expired",
        ProcessPrimaryCause::CommandTimeout => "command timed out",
        ProcessPrimaryCause::PipeSetupFailed => "output pipe setup failed",
        ProcessPrimaryCause::PipeReadFailed => "output pipe read failed",
        ProcessPrimaryCause::OutputLimit => "observed output limit exceeded",
        ProcessPrimaryCause::PipeDrainTimeout => "output pipe drain timed out",
        ProcessPrimaryCause::BackgroundNotSupported => "background process observation unavailable",
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

fn parse_empty(value: &Value) -> ToolCallResult<()> {
    serde_json::from_value::<EmptyArguments>(value.clone())
        .map(|_| ())
        .map_err(|_| ToolCallError::invalid_args("job_list arguments must be an empty object"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobOutputWire {
    job_id: String,
    #[serde(default)]
    wait: bool,
    #[serde(default, deserialize_with = "deserialize_present")]
    timeout_ms: Option<u64>,
}

struct JobOutputArguments {
    job_id: String,
    wait: bool,
    timeout_ms: u64,
}

fn parse_output(value: &Value) -> ToolCallResult<JobOutputArguments> {
    let wire: JobOutputWire = serde_json::from_value(value.clone()).map_err(|_| {
        ToolCallError::invalid_args("job_output arguments must match the advertised object schema")
    })?;
    validate_job_id(&wire.job_id)?;
    let timeout_ms = wire.timeout_ms.unwrap_or(DEFAULT_JOB_WAIT_MS);
    if !(1..=MAX_JOB_WAIT_MS).contains(&timeout_ms) {
        return Err(ToolCallError::invalid_args(format!(
            "job_output.timeout_ms must be between 1 and {MAX_JOB_WAIT_MS}"
        )));
    }
    Ok(JobOutputArguments {
        job_id: wire.job_id,
        wait: wire.wait,
        timeout_ms,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobKillWire {
    job_id: String,
    #[serde(default, deserialize_with = "deserialize_present")]
    reason: Option<String>,
}

struct JobKillArguments {
    job_id: String,
    reason: Option<String>,
}

fn parse_kill(value: &Value) -> ToolCallResult<JobKillArguments> {
    let wire: JobKillWire = serde_json::from_value(value.clone()).map_err(|_| {
        ToolCallError::invalid_args("job_kill arguments must match the advertised object schema")
    })?;
    validate_job_id(&wire.job_id)?;
    if let Some(reason) = &wire.reason {
        if reason.trim().is_empty()
            || reason.len() > MAX_JOB_REASON_BYTES
            || reason.chars().any(char::is_control)
        {
            return Err(ToolCallError::invalid_args(format!(
                "job_kill.reason must be one visible line of at most {MAX_JOB_REASON_BYTES} bytes"
            )));
        }
    }
    Ok(JobKillArguments {
        job_id: wire.job_id,
        reason: wire.reason,
    })
}

fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn validate_job_id(id: &str) -> ToolCallResult<()> {
    if id.is_empty() || id.len() > MAX_JOB_ID_BYTES {
        return Err(ToolCallError::invalid_args("invalid background job id"));
    }
    let Some(number) = id.strip_prefix("bash-") else {
        return Err(ToolCallError::invalid_args("invalid background job id"));
    };
    if number.is_empty()
        || number.starts_with('0')
        || !number.bytes().all(|byte| byte.is_ascii_digit())
        || number.parse::<u64>().is_err()
    {
        return Err(ToolCallError::invalid_args("invalid background job id"));
    }
    Ok(())
}

fn unknown_job(id: &str) -> ToolCallError {
    ToolCallError::model(
        "JobError",
        "JOB_UNKNOWN",
        format!("unknown background job {id}"),
    )
}

fn bounded_label(command: &str) -> String {
    let single_line = command
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if single_line.len() <= MAX_JOB_LABEL_BYTES {
        return single_line;
    }
    let mut end = MAX_JOB_LABEL_BYTES.saturating_sub('…'.len_utf8());
    while !single_line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &single_line[..end])
}

fn fit_with_suffix(body: &str, suffix: &str) -> String {
    let complete = format!("{body}{suffix}");
    if text_block_encoded_bytes(json_string_content_bytes(&complete)) <= MAX_TOOL_CONTENT_BYTES {
        return complete;
    }
    let notice = "[output truncated]\n";
    let fixed = format!("{notice}{suffix}");
    let fixed_bytes = json_string_content_bytes(&fixed);
    let budget = MAX_TOOL_CONTENT_BYTES.saturating_sub(text_block_encoded_bytes(fixed_bytes));
    let mut start = body.len();
    let mut used = 0_usize;
    for (index, character) in body.char_indices().rev() {
        let encoded = json_string_content_bytes(&character.to_string());
        if used.saturating_add(encoded) > budget {
            break;
        }
        used += encoded;
        start = index;
    }
    format!("{notice}{}{suffix}", &body[start..])
}

fn success_with_meta(text: String, meta: Value) -> Result<ToolExecutionResult, ToolExecutorError> {
    let content = ContentBlock::text(text)
        .map_err(|_| ToolExecutorError::new("background job output normalization failed"))?;
    let meta = JsonValue::new(meta)
        .map_err(|_| ToolExecutorError::new("background job metadata normalization failed"))?;
    ToolExecutionResult::new(vec![content], false, None, Some(meta), false)
        .map_err(|_| ToolExecutorError::new("background job result normalization failed"))
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

pub(crate) fn schemas() -> Result<[ToolSchema; 3], ToolRegistryBuildError> {
    Ok([
        schema(
            JOB_OUTPUT_TOOL_NAME,
            "Read output produced by a background job since the previous read. Set wait to block until it settles or the bounded wait expires; waiting never cancels the job.",
            json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "minLength": 6, "maxLength": MAX_JOB_ID_BYTES },
                    "wait": { "type": "boolean" },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": MAX_JOB_WAIT_MS }
                },
                "required": ["job_id"],
                "additionalProperties": false
            }),
        )?,
        schema(
            JOB_LIST_TOOL_NAME,
            "List the background Bash jobs owned by this CLI process.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )?,
        schema(
            JOB_KILL_TOOL_NAME,
            "Request cancellation of a background job. This returns before process cleanup necessarily finishes.",
            json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "minLength": 6, "maxLength": MAX_JOB_ID_BYTES },
                    "reason": { "type": "string", "minLength": 1, "maxLength": MAX_JOB_REASON_BYTES }
                },
                "required": ["job_id"],
                "additionalProperties": false
            }),
        )?,
    ])
}

fn schema(
    name: &'static str,
    description: &'static str,
    parameters: Value,
) -> Result<ToolSchema, ToolRegistryBuildError> {
    let parameters =
        JsonValue::new(parameters).map_err(|source| ToolRegistryBuildError::InvalidSchema {
            tool: name,
            source: source.into(),
        })?;
    ToolSchema::new(name, description, parameters)
        .map_err(|source| ToolRegistryBuildError::InvalidSchema { tool: name, source })
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        JOB_OUTPUT_TOOL_NAME | JOB_LIST_TOOL_NAME | JOB_KILL_TOOL_NAME
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::{
        BackgroundJobRuntime, JobCompletion, JobRecord, JobStatus, MAX_JOB_WAIT_MS,
        ProcessOutputCursor, ProcessOutputTap, RETAINED_TAIL_BYTES, ToolExecutionResult,
        bounded_label, epoch_millis, fit_with_suffix, parse_empty, parse_kill, parse_output,
        render_process_read, schemas, validate_job_id,
    };
    use crate::model::ContentBlock;

    #[test]
    fn schemas_and_arguments_are_closed_and_bounded() {
        let schemas = schemas().unwrap();
        assert_eq!(
            schemas.map(|schema| schema.name().to_owned()),
            ["job_output", "job_list", "job_kill"]
        );
        assert!(parse_empty(&json!({})).is_ok());
        assert!(parse_empty(&json!({ "extra": true })).is_err());
        assert!(parse_output(&json!({ "job_id": "bash-1" })).is_ok());
        assert!(
            parse_output(&json!({
                "job_id": "bash-1",
                "wait": true,
                "timeout_ms": MAX_JOB_WAIT_MS
            }))
            .is_ok()
        );
        assert!(parse_output(&json!({ "job_id": "bash-1", "timeout_ms": 0 })).is_err());
        assert!(parse_output(&json!({ "job_id": "bash-1", "extra": true })).is_err());
        assert!(parse_kill(&json!({ "job_id": "bash-2", "reason": "no longer needed" })).is_ok());
        assert!(parse_kill(&json!({ "job_id": "bash-2", "reason": "\n" })).is_err());
        for invalid in ["", "bash-0", "bash-01", "bash-x", "pwsh-1", "bash-1/2"] {
            assert!(validate_job_id(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn labels_and_output_preserve_bounds_and_terminal_suffix() {
        let label = bounded_label(&format!("{}\nsecret", "界".repeat(300)));
        assert!(label.len() <= 240);
        assert!(!label.contains('\n'));
        let suffix = "\n[status: completed, exit code: 0]";
        let text = fit_with_suffix(&"\\\"\n".repeat(80_000), suffix);
        assert!(text.ends_with(suffix));
        assert!(ContentBlock::text(text).unwrap().raw().encoded_len() <= 64 * 1024);
    }

    #[tokio::test]
    async fn wait_timeout_and_caller_cancellation_leave_the_job_alive() {
        let runtime = BackgroundJobRuntime::new();
        let job_cancellation = CancellationToken::new();
        {
            let mut state = runtime.inner.state.lock().await;
            state.jobs.push_back(JobRecord {
                id: "bash-1".to_owned(),
                label: "test job".to_owned(),
                status: JobStatus::Running,
                detail: None,
                output_tap: ProcessOutputTap::new(RETAINED_TAIL_BYTES),
                output_cursor: ProcessOutputCursor::default(),
                fallback_output: None,
                started_at: epoch_millis(),
                finished_at: None,
                cancellation: job_cancellation.clone(),
                changed: tokio::sync::watch::channel(JobStatus::Running).0,
                waiters: Arc::new(AtomicUsize::new(0)),
                reported: false,
            });
        }

        let timed_out = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1", "wait": true, "timeout_ms": 1 }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!timed_out.is_error());
        assert_eq!(
            timed_out.meta().unwrap().as_value()["job"]["status"],
            "running"
        );
        assert!(!job_cancellation.is_cancelled());

        let caller = CancellationToken::new();
        let cancel = caller.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel.cancel();
        });
        let cancelled = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1", "wait": true, "timeout_ms": 1000 }),
                caller,
            )
            .await
            .unwrap();
        assert!(cancelled.is_error());
        assert_eq!(cancelled.error().unwrap().code, "ABORTED");
        assert!(!job_cancellation.is_cancelled());
        assert_eq!(
            runtime.get("bash-1").await.unwrap().status,
            JobStatus::Running
        );
    }

    async fn insert_running(runtime: &BackgroundJobRuntime, id: &str) -> ProcessOutputTap {
        let output_tap = ProcessOutputTap::new(RETAINED_TAIL_BYTES);
        let mut state = runtime.inner.state.lock().await;
        state.jobs.push_back(JobRecord {
            id: id.to_owned(),
            label: "pnpm test".to_owned(),
            status: JobStatus::Running,
            detail: None,
            output_tap: output_tap.clone(),
            output_cursor: ProcessOutputCursor::default(),
            fallback_output: None,
            started_at: epoch_millis(),
            finished_at: None,
            cancellation: CancellationToken::new(),
            changed: tokio::sync::watch::channel(JobStatus::Running).0,
            waiters: Arc::new(AtomicUsize::new(0)),
            reported: false,
        });
        output_tap
    }

    fn completion() -> JobCompletion {
        JobCompletion {
            status: JobStatus::Completed,
            detail: "exit code: 0".to_owned(),
            fallback_output: None,
        }
    }

    fn result_text(result: &ToolExecutionResult) -> &str {
        result
            .content()
            .iter()
            .find_map(|block| match block.kind() {
                crate::model::ContentBlockKind::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn stream_reads_are_consuming_while_running_and_after_settlement() {
        let runtime = BackgroundJobRuntime::new();
        let output = insert_running(&runtime, "bash-1").await;
        output.push_stdout(b"line one\n");

        let first = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result_text(&first), "line one\n[status: running]");
        let empty = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result_text(&empty), "(no new output)\n[status: running]");

        output.push_stdout(b"tail");
        output.push_stderr(b"problem\n");
        runtime.finish("bash-1", completion()).await;
        let terminal = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result_text(&terminal),
            "tail\n[stderr]\nproblem\n[status: completed, exit code: 0]"
        );
        let repeated = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result_text(&repeated),
            "(no new output)\n[status: completed, exit code: 0]"
        );
    }

    #[tokio::test]
    async fn fallback_diagnostic_is_consumed_once() {
        let runtime = BackgroundJobRuntime::new();
        insert_running(&runtime, "bash-1").await;
        runtime
            .finish(
                "bash-1",
                JobCompletion::failed("spawn failed", "Error: background shell could not start"),
            )
            .await;

        let first = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result_text(&first),
            "Error: background shell could not start\n[status: failed, spawn failed]"
        );
        let repeated = runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            result_text(&repeated),
            "(no new output)\n[status: failed, spawn failed]"
        );
    }

    #[test]
    fn lossy_split_utf8_tail_is_safe_and_explicit() {
        let output = ProcessOutputTap::new(2);
        let mut cursor = ProcessOutputCursor::default();
        output.push_stdout("界".as_bytes());
        let rendered = render_process_read(&output.read(&mut cursor));
        assert!(rendered.contains('\u{fffd}'));
        assert!(rendered.contains("some output was dropped from memory"));
        assert!(rendered.contains("full output: (unavailable)"));
    }

    #[tokio::test]
    async fn terminal_output_kill_and_wait_suppress_completion_notices() {
        let output_runtime = BackgroundJobRuntime::new();
        insert_running(&output_runtime, "bash-1").await;
        output_runtime.finish("bash-1", completion()).await;
        output_runtime
            .execute(
                "job_output",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(output_runtime.notices().claim_idle_wake().is_none());

        let kill_runtime = BackgroundJobRuntime::new();
        insert_running(&kill_runtime, "bash-1").await;
        kill_runtime.finish("bash-1", completion()).await;
        kill_runtime
            .execute(
                "job_kill",
                &json!({ "job_id": "bash-1" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(kill_runtime.notices().claim_idle_wake().is_none());

        let wait_runtime = BackgroundJobRuntime::new();
        insert_running(&wait_runtime, "bash-1").await;
        let waiter_runtime = wait_runtime.clone();
        let waiter = tokio::spawn(async move {
            waiter_runtime
                .execute(
                    "job_output",
                    &json!({ "job_id": "bash-1", "wait": true, "timeout_ms": 1000 }),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
        });
        for _ in 0..32 {
            let active = {
                let state = wait_runtime.inner.state.lock().await;
                state.jobs[0].waiters.load(Ordering::Acquire)
            };
            if active != 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        wait_runtime.finish("bash-1", completion()).await;
        assert!(!waiter.await.unwrap().is_error());
        assert!(wait_runtime.notices().claim_idle_wake().is_none());
    }
}
