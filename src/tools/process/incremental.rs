//! Bounded process-output snapshots for consuming background-job reads.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Default)]
struct StreamTail {
    bytes: VecDeque<u8>,
    total: usize,
}

impl StreamTail {
    fn push(&mut self, chunk: &[u8], limit: usize) {
        self.total = self.total.saturating_add(chunk.len());
        if limit == 0 {
            self.bytes.clear();
            return;
        }
        if chunk.len() >= limit {
            self.bytes.clear();
            self.bytes
                .extend(chunk[chunk.len().saturating_sub(limit)..].iter().copied());
            return;
        }
        self.bytes.extend(chunk.iter().copied());
        let overflow = self.bytes.len().saturating_sub(limit);
        if overflow != 0 {
            self.bytes.drain(..overflow);
        }
    }

    fn read(&self, offset: &mut usize) -> (Vec<u8>, bool) {
        let window_start = self.total.saturating_sub(self.bytes.len());
        let lossy = *offset < window_start;
        let start = if lossy {
            0
        } else {
            offset.saturating_sub(window_start).min(self.bytes.len())
        };
        *offset = self.total;
        (self.bytes.iter().skip(start).copied().collect(), lossy)
    }
}

#[derive(Default)]
struct OutputState {
    stdout: StreamTail,
    stderr: StreamTail,
    finished: bool,
    incomplete: bool,
    stdout_spill: Option<PathBuf>,
    stderr_spill: Option<PathBuf>,
}

struct OutputInner {
    state: Mutex<OutputState>,
    tail_limit: usize,
}

/// One process-owned bounded snapshot, shared with its background job record.
#[derive(Clone)]
pub(crate) struct ProcessOutputTap {
    inner: Arc<OutputInner>,
}

impl std::fmt::Debug for ProcessOutputTap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessOutputTap")
            .field("tail_limit", &self.inner.tail_limit)
            .finish_non_exhaustive()
    }
}

impl ProcessOutputTap {
    pub(crate) fn new(tail_limit: usize) -> Self {
        Self {
            inner: Arc::new(OutputInner {
                state: Mutex::new(OutputState::default()),
                tail_limit,
            }),
        }
    }

    pub(crate) fn push_stdout(&self, chunk: &[u8]) {
        let limit = self.inner.tail_limit;
        self.lock().stdout.push(chunk, limit);
    }

    pub(crate) fn push_stderr(&self, chunk: &[u8]) {
        let limit = self.inner.tail_limit;
        self.lock().stderr.push(chunk, limit);
    }

    pub(crate) fn finish(
        &self,
        stdout_spill: Option<PathBuf>,
        stderr_spill: Option<PathBuf>,
        incomplete: bool,
    ) {
        let mut state = self.lock();
        state.finished = true;
        state.incomplete = incomplete;
        state.stdout_spill = stdout_spill;
        state.stderr_spill = stderr_spill;
    }

    pub(crate) fn read(&self, cursor: &mut ProcessOutputCursor) -> ProcessOutputRead {
        let state = self.lock();
        let (stdout, stdout_lossy) = state.stdout.read(&mut cursor.stdout);
        let (stderr, stderr_lossy) = state.stderr.read(&mut cursor.stderr);
        let stream_lossy = stdout_lossy || stderr_lossy;
        let spill_available = state.stdout_spill.is_some() || state.stderr_spill.is_some();
        let newly_available_spill =
            state.finished && cursor.pending_spill_notice && spill_available;
        if stream_lossy && !state.finished && !spill_available {
            // Live reads cannot safely name a spill file until its collector
            // has flushed it. Remember to reveal the finalized locator once.
            cursor.pending_spill_notice = true;
        }
        if state.finished {
            cursor.pending_spill_notice = false;
        }
        let newly_incomplete = state.finished && state.incomplete && !cursor.incomplete_reported;
        cursor.incomplete_reported |= newly_incomplete;
        ProcessOutputRead {
            stdout,
            stderr,
            lossy: stream_lossy || newly_available_spill || newly_incomplete,
            stdout_spill: state.stdout_spill.clone(),
            stderr_spill: state.stderr_spill.clone(),
            spill_is_full: !state.incomplete,
        }
    }

    fn lock(&self) -> MutexGuard<'_, OutputState> {
        // Output is diagnostic state; recover the bounded buffer rather than
        // letting a poisoned observer lock break process cleanup.
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Default)]
pub(crate) struct ProcessOutputCursor {
    stdout: usize,
    stderr: usize,
    incomplete_reported: bool,
    pending_spill_notice: bool,
}

pub(crate) struct ProcessOutputRead {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    lossy: bool,
    stdout_spill: Option<PathBuf>,
    stderr_spill: Option<PathBuf>,
    spill_is_full: bool,
}

impl ProcessOutputRead {
    pub(crate) fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub(crate) fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub(crate) fn lossy(&self) -> bool {
        self.lossy
    }

    pub(crate) fn stdout_spill(&self) -> Option<&Path> {
        self.stdout_spill.as_deref()
    }

    pub(crate) fn stderr_spill(&self) -> Option<&Path> {
        self.stderr_spill.as_deref()
    }

    pub(crate) fn spill_is_full(&self) -> bool {
        self.spill_is_full
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ProcessOutputCursor, ProcessOutputTap};

    #[test]
    fn reads_each_stream_delta_once() {
        let tap = ProcessOutputTap::new(16);
        let mut cursor = ProcessOutputCursor::default();
        tap.push_stdout(b"first");
        let first = tap.read(&mut cursor);
        assert_eq!(first.stdout(), b"first");
        assert!(first.stderr().is_empty());
        assert!(!first.lossy());

        let empty = tap.read(&mut cursor);
        assert!(empty.stdout().is_empty());
        tap.push_stdout(b" second");
        tap.push_stderr(b"problem");
        let second = tap.read(&mut cursor);
        assert_eq!(second.stdout(), b" second");
        assert_eq!(second.stderr(), b"problem");
    }

    #[test]
    fn falling_behind_returns_the_exact_tail_and_reports_loss_once() {
        let tap = ProcessOutputTap::new(4);
        let mut cursor = ProcessOutputCursor::default();
        tap.push_stdout(b"abcdefgh");
        let read = tap.read(&mut cursor);
        assert_eq!(read.stdout(), b"efgh");
        assert!(read.lossy());
        assert!(!tap.read(&mut cursor).lossy());
    }

    #[test]
    fn finalization_publishes_spills_and_incomplete_capture_once() {
        let tap = ProcessOutputTap::new(8);
        let mut cursor = ProcessOutputCursor::default();
        tap.finish(
            Some(PathBuf::from("/tmp/stdout")),
            Some(PathBuf::from("/tmp/stderr")),
            true,
        );
        let first = tap.read(&mut cursor);
        assert!(first.lossy());
        assert_eq!(first.stdout_spill().unwrap(), PathBuf::from("/tmp/stdout"));
        assert_eq!(first.stderr_spill().unwrap(), PathBuf::from("/tmp/stderr"));
        assert!(!first.spill_is_full());
        assert!(!tap.read(&mut cursor).lossy());
    }

    #[test]
    fn finalized_spill_is_revealed_after_a_lossy_live_read() {
        let tap = ProcessOutputTap::new(4);
        let mut cursor = ProcessOutputCursor::default();
        tap.push_stdout(b"abcdefgh");
        let live = tap.read(&mut cursor);
        assert!(live.lossy());
        assert!(live.stdout_spill().is_none());

        tap.finish(Some(PathBuf::from("/tmp/stdout")), None, false);
        let final_read = tap.read(&mut cursor);
        assert!(final_read.stdout().is_empty());
        assert!(final_read.lossy());
        assert_eq!(
            final_read.stdout_spill().unwrap(),
            PathBuf::from("/tmp/stdout")
        );
        assert!(!tap.read(&mut cursor).lossy());
    }
}
