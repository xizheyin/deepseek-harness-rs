//! Bounded read-only search over normally closed journals in one workspace.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use regex::{Regex, RegexBuilder};
use thiserror::Error;
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

use crate::{
    model::{Message, MessageRole},
    workspace_authority::WorkspaceIdentity,
};

use super::{
    EventKind, EventSeq, SessionEvent, SessionId, SessionStore, StoreError, TodoStatus,
    TurnEndReason, recovery::scan_jsonl_observing,
};

pub(crate) const MAX_SESSION_SEARCH_QUERY_BYTES: usize = 1_024;
pub(crate) const MAX_SESSION_SEARCH_RESULTS: usize = 20;
pub(crate) const MAX_SESSION_SEARCH_SNIPPET_CHARS: usize = 240;
pub(crate) const MAX_SESSION_EVENT_READ_WINDOW: u64 = 50;
pub(crate) const MAX_SESSION_SEARCH_SESSION_BYTES: u64 = 16 * 1_024 * 1_024;
pub(crate) const MAX_SESSION_SEARCH_AGGREGATE_BYTES: u64 = 64 * 1_024 * 1_024;
const SESSION_SEARCH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_QUERY_REGEX_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_BLOCK_NESTING: usize = 16;

#[derive(Clone)]
pub(crate) struct SessionSearchQuery {
    pattern: Regex,
}

impl std::fmt::Debug for SessionSearchQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionSearchQuery")
            .field("compiled", &true)
            .finish()
    }
}

impl SessionSearchQuery {
    pub(crate) fn new(value: &str) -> Result<Self, SessionSearchError> {
        if value.len() > MAX_SESSION_SEARCH_QUERY_BYTES || value.contains('\0') {
            return Err(SessionSearchError::Invalid);
        }
        let parts = value.split_whitespace().collect::<Vec<_>>();
        if parts.is_empty() {
            return Err(SessionSearchError::Invalid);
        }
        let mut source = String::new();
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 {
                source.push_str(r"\s+");
            }
            source.push_str(&regex::escape(part));
        }
        let pattern = RegexBuilder::new(&source)
            .case_insensitive(true)
            .unicode(true)
            .size_limit(MAX_QUERY_REGEX_BYTES)
            .build()
            .map_err(|_| SessionSearchError::Invalid)?;
        Ok(Self { pattern })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSearchHit {
    session_id: SessionId,
    created_at: i64,
    event_seq: u64,
    event_type: String,
    event_time: i64,
    snippet: String,
    score: u32,
}

impl SessionSearchHit {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_test(
        session_id: SessionId,
        created_at: i64,
        event_seq: u64,
        event_type: impl Into<String>,
        event_time: i64,
        snippet: impl Into<String>,
        score: u32,
    ) -> Self {
        Self {
            session_id,
            created_at,
            event_seq,
            event_type: event_type.into(),
            event_time,
            snippet: snippet.into(),
            score,
        }
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn created_at(&self) -> i64 {
        self.created_at
    }

    pub(crate) fn event_seq(&self) -> u64 {
        self.event_seq
    }

    pub(crate) fn event_type(&self) -> &str {
        &self.event_type
    }

    pub(crate) fn event_time(&self) -> i64 {
        self.event_time
    }

    pub(crate) fn snippet(&self) -> &str {
        &self.snippet
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionSearchOutcome {
    hits: Vec<SessionSearchHit>,
    result_capped: bool,
    scan_capped: bool,
}

impl SessionSearchOutcome {
    #[cfg(test)]
    pub(crate) fn for_test(
        hits: Vec<SessionSearchHit>,
        result_capped: bool,
        scan_capped: bool,
    ) -> Self {
        Self {
            hits,
            result_capped,
            scan_capped,
        }
    }

    pub(crate) fn hits(&self) -> &[SessionSearchHit] {
        &self.hits
    }

    pub(crate) fn result_capped(&self) -> bool {
        self.result_capped
    }

    pub(crate) fn scan_capped(&self) -> bool {
        self.scan_capped
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionEventSurface {
    Current,
    Shadowed,
    LogOnly,
}

impl std::fmt::Display for SessionEventSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Current => "current",
            Self::Shadowed => "shadowed",
            Self::LogOnly => "log-only",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventSearchHit {
    event_seq: u64,
    event_type: String,
    event_time: i64,
    surface: SessionEventSurface,
    snippet: String,
    score: u32,
    document_chars: usize,
    surface_event: bool,
}

impl SessionEventSearchHit {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_test(
        event_seq: u64,
        event_type: impl Into<String>,
        event_time: i64,
        surface: SessionEventSurface,
        snippet: impl Into<String>,
        score: u32,
        document_chars: usize,
    ) -> Self {
        Self {
            event_seq,
            event_type: event_type.into(),
            event_time,
            surface,
            snippet: snippet.into(),
            score,
            document_chars,
            surface_event: surface != SessionEventSurface::LogOnly,
        }
    }

    pub(crate) fn event_seq(&self) -> u64 {
        self.event_seq
    }

    pub(crate) fn event_type(&self) -> &str {
        &self.event_type
    }

    pub(crate) fn event_time(&self) -> i64 {
        self.event_time
    }

    pub(crate) fn surface(&self) -> SessionEventSurface {
        self.surface
    }

    pub(crate) fn snippet(&self) -> &str {
        &self.snippet
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventSearchOutcome {
    session_id: SessionId,
    hits: Vec<SessionEventSearchHit>,
    result_capped: bool,
}

impl SessionEventSearchOutcome {
    #[cfg(test)]
    pub(crate) fn for_test(
        session_id: SessionId,
        hits: Vec<SessionEventSearchHit>,
        result_capped: bool,
    ) -> Self {
        Self {
            session_id,
            hits,
            result_capped,
        }
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn hits(&self) -> &[SessionEventSearchHit] {
        &self.hits
    }

    pub(crate) fn result_capped(&self) -> bool {
        self.result_capped
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventSummary {
    event_seq: u64,
    event_type: String,
    event_time: i64,
    text: Option<String>,
}

impl SessionEventSummary {
    #[cfg(test)]
    pub(crate) fn for_test(
        event_seq: u64,
        event_type: impl Into<String>,
        event_time: i64,
        text: Option<String>,
    ) -> Self {
        Self {
            event_seq,
            event_type: event_type.into(),
            event_time,
            text,
        }
    }

    pub(crate) fn event_seq(&self) -> u64 {
        self.event_seq
    }

    pub(crate) fn event_type(&self) -> &str {
        &self.event_type
    }

    pub(crate) fn event_time(&self) -> i64 {
        self.event_time
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventReadOutcome {
    session_id: SessionId,
    target: SessionEvent,
    before: Vec<SessionEventSummary>,
    after: Vec<SessionEventSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionLineageRecord {
    session_id: SessionId,
    created_at: i64,
    parent_session: Option<SessionId>,
}

impl SessionLineageRecord {
    #[cfg(test)]
    pub(crate) fn for_test(
        session_id: SessionId,
        created_at: i64,
        parent_session: Option<SessionId>,
    ) -> Self {
        Self {
            session_id,
            created_at,
            parent_session,
        }
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn created_at(&self) -> i64 {
        self.created_at
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionLineageNode {
    record: SessionLineageRecord,
    descendants: Vec<SessionLineageNode>,
}

impl SessionLineageNode {
    #[cfg(test)]
    pub(crate) fn for_test(
        record: SessionLineageRecord,
        descendants: Vec<SessionLineageNode>,
    ) -> Self {
        Self {
            record,
            descendants,
        }
    }

    pub(crate) fn record(&self) -> &SessionLineageRecord {
        &self.record
    }

    pub(crate) fn descendants(&self) -> &[SessionLineageNode] {
        &self.descendants
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionTraceOutcome {
    target: SessionLineageRecord,
    ancestors: Vec<SessionLineageRecord>,
    ancestor_boundary: bool,
    descendants: Vec<SessionLineageNode>,
    corpus_incomplete: bool,
}

impl SessionTraceOutcome {
    #[cfg(test)]
    pub(crate) fn for_test(
        target: SessionLineageRecord,
        ancestors: Vec<SessionLineageRecord>,
        ancestor_boundary: bool,
        descendants: Vec<SessionLineageNode>,
        corpus_incomplete: bool,
    ) -> Self {
        Self {
            target,
            ancestors,
            ancestor_boundary,
            descendants,
            corpus_incomplete,
        }
    }

    pub(crate) fn target(&self) -> &SessionLineageRecord {
        &self.target
    }

    pub(crate) fn ancestors(&self) -> &[SessionLineageRecord] {
        &self.ancestors
    }

    pub(crate) fn ancestor_boundary(&self) -> bool {
        self.ancestor_boundary
    }

    pub(crate) fn descendants(&self) -> &[SessionLineageNode] {
        &self.descendants
    }

    pub(crate) fn corpus_incomplete(&self) -> bool {
        self.corpus_incomplete
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionEventTraceOutcome {
    session_id: SessionId,
    target_seq: u64,
    target_type: String,
    target_time: i64,
    target_surface: SessionEventSurface,
    replaced_by: Option<u64>,
    replacement_chain: Vec<u64>,
    replaced_event_seqs: Vec<u64>,
    source_event_seqs: Vec<u64>,
    derived_event_seqs: Vec<u64>,
}

impl SessionEventTraceOutcome {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_test(
        session_id: SessionId,
        target_seq: u64,
        target_type: impl Into<String>,
        target_time: i64,
        target_surface: SessionEventSurface,
        replaced_by: Option<u64>,
        replacement_chain: Vec<u64>,
        replaced_event_seqs: Vec<u64>,
        source_event_seqs: Vec<u64>,
        derived_event_seqs: Vec<u64>,
    ) -> Self {
        Self {
            session_id,
            target_seq,
            target_type: target_type.into(),
            target_time,
            target_surface,
            replaced_by,
            replacement_chain,
            replaced_event_seqs,
            source_event_seqs,
            derived_event_seqs,
        }
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn target_seq(&self) -> u64 {
        self.target_seq
    }

    pub(crate) fn target_type(&self) -> &str {
        &self.target_type
    }

    pub(crate) fn target_time(&self) -> i64 {
        self.target_time
    }

    pub(crate) fn target_surface(&self) -> SessionEventSurface {
        self.target_surface
    }

    pub(crate) fn replaced_by(&self) -> Option<u64> {
        self.replaced_by
    }

    pub(crate) fn replacement_chain(&self) -> &[u64] {
        &self.replacement_chain
    }

    pub(crate) fn replaced_event_seqs(&self) -> &[u64] {
        &self.replaced_event_seqs
    }

    pub(crate) fn source_event_seqs(&self) -> &[u64] {
        &self.source_event_seqs
    }

    pub(crate) fn derived_event_seqs(&self) -> &[u64] {
        &self.derived_event_seqs
    }
}

impl SessionEventReadOutcome {
    #[cfg(test)]
    pub(crate) fn for_test(
        session_id: SessionId,
        target: SessionEvent,
        before: Vec<SessionEventSummary>,
        after: Vec<SessionEventSummary>,
    ) -> Self {
        Self {
            session_id,
            target,
            before,
            after,
        }
    }

    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub(crate) fn target(&self) -> &SessionEvent {
        &self.target
    }

    pub(crate) fn before(&self) -> &[SessionEventSummary] {
        &self.before
    }

    pub(crate) fn after(&self) -> &[SessionEventSummary] {
        &self.after
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum SessionSearchError {
    #[error("session search input is invalid")]
    Invalid,
    #[error("session search was cancelled")]
    Cancelled,
    #[error("session search exceeded its deadline")]
    Timeout,
    #[error("session search is unavailable")]
    Unavailable,
    #[error("the requested prior session is unavailable")]
    SessionNotFound,
    #[error("the requested prior session event does not exist")]
    EventNotFound,
}

#[derive(Clone)]
pub(crate) struct SessionSearchRuntime {
    store: SessionStore,
    workspace: WorkspaceIdentity,
    caller: SessionId,
}

impl std::fmt::Debug for SessionSearchRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionSearchRuntime")
            .field("workspace_bound", &true)
            .field("caller_bound", &true)
            .finish()
    }
}

impl SessionSearchRuntime {
    pub(crate) fn new(
        store: SessionStore,
        workspace: WorkspaceIdentity,
        caller: SessionId,
    ) -> Self {
        Self {
            store,
            workspace,
            caller,
        }
    }

    pub(crate) async fn search(
        &self,
        query: SessionSearchQuery,
        cancellation: CancellationToken,
    ) -> Result<SessionSearchOutcome, SessionSearchError> {
        let store = self.store.clone();
        let workspace = self.workspace;
        let caller = self.caller.clone();
        run_bounded_operation(cancellation, move |cancelled, deadline| {
            search_sync(&store, workspace, &caller, &query, cancelled, deadline)
        })
        .await
    }

    pub(crate) async fn search_events(
        &self,
        session_id: SessionId,
        query: SessionSearchQuery,
        cancellation: CancellationToken,
    ) -> Result<SessionEventSearchOutcome, SessionSearchError> {
        let store = self.store.clone();
        let workspace = self.workspace;
        let caller = self.caller.clone();
        run_bounded_operation(cancellation, move |cancelled, deadline| {
            event_search_sync(
                &store, workspace, &caller, session_id, &query, cancelled, deadline,
            )
        })
        .await
    }

    pub(crate) async fn read_event(
        &self,
        session_id: SessionId,
        seq: u64,
        before: u64,
        after: u64,
        cancellation: CancellationToken,
    ) -> Result<SessionEventReadOutcome, SessionSearchError> {
        if before > MAX_SESSION_EVENT_READ_WINDOW || after > MAX_SESSION_EVENT_READ_WINDOW {
            return Err(SessionSearchError::Invalid);
        }
        let store = self.store.clone();
        let workspace = self.workspace;
        let caller = self.caller.clone();
        run_bounded_operation(cancellation, move |cancelled, deadline| {
            event_read_sync(
                &store, workspace, &caller, session_id, seq, before, after, cancelled, deadline,
            )
        })
        .await
    }

    pub(crate) async fn trace_session(
        &self,
        session_id: SessionId,
        cancellation: CancellationToken,
    ) -> Result<SessionTraceOutcome, SessionSearchError> {
        let store = self.store.clone();
        let workspace = self.workspace;
        let caller = self.caller.clone();
        run_bounded_operation(cancellation, move |cancelled, deadline| {
            session_trace_sync(&store, workspace, &caller, session_id, cancelled, deadline)
        })
        .await
    }

    pub(crate) async fn trace_event(
        &self,
        session_id: SessionId,
        seq: u64,
        cancellation: CancellationToken,
    ) -> Result<SessionEventTraceOutcome, SessionSearchError> {
        let store = self.store.clone();
        let workspace = self.workspace;
        let caller = self.caller.clone();
        run_bounded_operation(cancellation, move |cancelled, deadline| {
            event_trace_sync(
                &store, workspace, &caller, session_id, seq, cancelled, deadline,
            )
        })
        .await
    }
}

async fn run_bounded_operation<T, F>(
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, SessionSearchError>
where
    T: Send + 'static,
    F: FnOnce(&AtomicBool, Instant) -> Result<T, SessionSearchError> + Send + 'static,
{
    if cancellation.is_cancelled() {
        return Err(SessionSearchError::Cancelled);
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let deadline = Instant::now() + SESSION_SEARCH_TIMEOUT;
    let mut worker = tokio::task::spawn_blocking(move || operation(&worker_cancelled, deadline));
    let timeout = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
    tokio::pin!(timeout);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            cancelled.store(true, Ordering::Release);
            settle_worker(worker).await?;
            Err(SessionSearchError::Cancelled)
        }
        () = &mut timeout => {
            cancelled.store(true, Ordering::Release);
            settle_worker(worker).await?;
            Err(SessionSearchError::Timeout)
        }
        result = &mut worker => result.map_err(map_join_error)?,
    }
}

async fn settle_worker<T>(
    worker: tokio::task::JoinHandle<Result<T, SessionSearchError>>,
) -> Result<(), SessionSearchError> {
    worker.await.map(|_| ()).map_err(map_join_error)
}

fn map_join_error(_error: JoinError) -> SessionSearchError {
    SessionSearchError::Unavailable
}

fn search_sync(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    query: &SessionSearchQuery,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<SessionSearchOutcome, SessionSearchError> {
    check_stop(cancelled, deadline)?;
    let candidates = store
        .search_candidates(workspace, caller)
        .map_err(map_store_error)?;
    let mut hits = Vec::new();
    hits.try_reserve_exact(candidates.len())
        .map_err(|_| SessionSearchError::Unavailable)?;
    let mut scanned_bytes = 0_u64;
    let mut scan_capped = false;
    for candidate in candidates {
        check_stop(cancelled, deadline)?;
        let length = candidate.file_length().map_err(map_store_error)?;
        if length > MAX_SESSION_SEARCH_SESSION_BYTES {
            scan_capped = true;
            continue;
        }
        let Some(next_total) = scanned_bytes.checked_add(length) else {
            scan_capped = true;
            break;
        };
        if next_total > MAX_SESSION_SEARCH_AGGREGATE_BYTES {
            scan_capped = true;
            break;
        }
        scanned_bytes = next_total;
        let metadata = candidate.metadata().clone();
        let mut file = candidate.into_file();
        let mut best = None;
        let scan = scan_jsonl_observing(
            &mut file,
            metadata.id(),
            cancelled,
            |header, identity| {
                if identity != workspace || header.id() != metadata.id() {
                    return Err(StoreError::WorkspaceMismatch);
                }
                Ok(())
            },
            |event| {
                check_stop_store(cancelled, deadline)?;
                consider_event(&mut best, event, query);
                Ok(())
            },
        );
        match scan {
            Ok(scan)
                if scan.physical_bytes() == length
                    && scan.valid_bytes() == length
                    && scan.is_quiescent_for_search() =>
            {
                if let Some(best) = best {
                    hits.push(SessionSearchHit {
                        session_id: metadata.id().clone(),
                        created_at: metadata.created_at().get(),
                        event_seq: best.event_seq,
                        event_type: best.event_type,
                        event_time: best.event_time,
                        snippet: best.snippet,
                        score: best.score,
                    });
                }
            }
            Ok(_) => {}
            Err(StoreError::Cancelled) => return Err(stop_error(deadline)),
            Err(_) => {}
        }
    }
    hits.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.event_time.cmp(&left.event_time))
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let result_capped = hits.len() > MAX_SESSION_SEARCH_RESULTS;
    hits.truncate(MAX_SESSION_SEARCH_RESULTS);
    Ok(SessionSearchOutcome {
        hits,
        result_capped,
        scan_capped,
    })
}

fn event_search_sync(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    session_id: SessionId,
    query: &SessionSearchQuery,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<SessionEventSearchOutcome, SessionSearchError> {
    let candidate = find_target_candidate(store, workspace, caller, &session_id)?;
    check_target_length(&candidate)?;
    let metadata = candidate.metadata().clone();
    let length = candidate.file_length().map_err(map_store_error)?;
    let mut file = candidate.into_file();
    let mut hits = Vec::new();
    hits.try_reserve_exact(MAX_SESSION_SEARCH_RESULTS + 1)
        .map_err(|_| SessionSearchError::Unavailable)?;
    let mut match_count = 0_u64;
    let scan = scan_jsonl_observing(
        &mut file,
        &session_id,
        cancelled,
        |header, identity| {
            if identity != workspace || header.id() != metadata.id() {
                return Err(StoreError::WorkspaceMismatch);
            }
            Ok(())
        },
        |event| {
            check_stop_store(cancelled, deadline)?;
            consider_event_hit(&mut hits, &mut match_count, event, query)
                .map_err(|_| StoreError::Limit)
        },
    )
    .map_err(|error| map_target_scan_error(error, deadline))?;
    if scan.physical_bytes() != length
        || scan.valid_bytes() != length
        || !scan.is_quiescent_for_search()
    {
        return Err(SessionSearchError::Unavailable);
    }
    for hit in &mut hits {
        hit.surface = if !hit.surface_event {
            SessionEventSurface::LogOnly
        } else if EventSeq::new(hit.event_seq)
            .ok()
            .is_some_and(|seq| scan.current_surface_contains(seq))
        {
            SessionEventSurface::Current
        } else {
            SessionEventSurface::Shadowed
        };
    }
    Ok(SessionEventSearchOutcome {
        session_id,
        hits,
        result_capped: match_count > MAX_SESSION_SEARCH_RESULTS as u64,
    })
}

#[allow(clippy::too_many_arguments)]
fn event_read_sync(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    session_id: SessionId,
    seq: u64,
    before: u64,
    after: u64,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<SessionEventReadOutcome, SessionSearchError> {
    let candidate = find_target_candidate(store, workspace, caller, &session_id)?;
    check_target_length(&candidate)?;
    let metadata = candidate.metadata().clone();
    let length = candidate.file_length().map_err(map_store_error)?;
    let mut file = candidate.into_file();
    let start = seq.saturating_sub(before);
    let end = seq.saturating_add(after).min(super::MAX_SAFE_INTEGER);
    let mut target = None;
    let mut preceding = Vec::new();
    let mut following = Vec::new();
    preceding
        .try_reserve_exact(usize::try_from(before).map_err(|_| SessionSearchError::Invalid)?)
        .map_err(|_| SessionSearchError::Unavailable)?;
    following
        .try_reserve_exact(usize::try_from(after).map_err(|_| SessionSearchError::Invalid)?)
        .map_err(|_| SessionSearchError::Unavailable)?;
    let scan = scan_jsonl_observing(
        &mut file,
        &session_id,
        cancelled,
        |header, identity| {
            if identity != workspace || header.id() != metadata.id() {
                return Err(StoreError::WorkspaceMismatch);
            }
            Ok(())
        },
        |event| {
            check_stop_store(cancelled, deadline)?;
            let event_seq = event.seq().get();
            if event_seq == seq {
                target = Some(event.clone());
            } else if event_seq >= start && event_seq < seq {
                preceding.push(summarize_event(event));
            } else if event_seq > seq && event_seq <= end {
                following.push(summarize_event(event));
            }
            Ok(())
        },
    )
    .map_err(|error| map_target_scan_error(error, deadline))?;
    if scan.physical_bytes() != length
        || scan.valid_bytes() != length
        || !scan.is_quiescent_for_search()
    {
        return Err(SessionSearchError::Unavailable);
    }
    let target = target.ok_or(SessionSearchError::EventNotFound)?;
    Ok(SessionEventReadOutcome {
        session_id,
        target,
        before: preceding,
        after: following,
    })
}

fn session_trace_sync(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    session_id: SessionId,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<SessionTraceOutcome, SessionSearchError> {
    check_stop(cancelled, deadline)?;
    let mut candidates = store
        .search_candidates(workspace, caller)
        .map_err(map_store_error)?;
    let target_index = candidates
        .iter()
        .position(|candidate| candidate.metadata().id() == &session_id)
        .ok_or(SessionSearchError::SessionNotFound)?;
    let target_candidate = candidates.remove(target_index);
    let target_length = target_candidate.file_length().map_err(map_store_error)?;
    if target_length > MAX_SESSION_SEARCH_SESSION_BYTES {
        return Err(SessionSearchError::Unavailable);
    }
    let target = scan_lineage_candidate(target_candidate, workspace, cancelled, deadline, true)?
        .ok_or(SessionSearchError::Unavailable)?;

    let mut records = BTreeMap::new();
    records.insert(target.session_id.clone(), target.clone());
    let mut scanned_bytes = target_length;
    let mut corpus_incomplete = false;
    for candidate in candidates {
        check_stop(cancelled, deadline)?;
        let length = candidate.file_length().map_err(map_store_error)?;
        if length > MAX_SESSION_SEARCH_SESSION_BYTES {
            corpus_incomplete = true;
            continue;
        }
        let Some(next_total) = scanned_bytes.checked_add(length) else {
            corpus_incomplete = true;
            break;
        };
        if next_total > MAX_SESSION_SEARCH_AGGREGATE_BYTES {
            corpus_incomplete = true;
            break;
        }
        scanned_bytes = next_total;
        match scan_lineage_candidate(candidate, workspace, cancelled, deadline, false)? {
            Some(record) => {
                records.insert(record.session_id.clone(), record);
            }
            None => corpus_incomplete = true,
        }
    }

    let mut ancestors = Vec::new();
    let mut seen = BTreeSet::from([session_id.clone()]);
    let mut parent = target.parent_session.clone();
    let mut ancestor_boundary = false;
    while let Some(parent_id) = parent {
        if !seen.insert(parent_id.clone()) {
            return Err(SessionSearchError::Unavailable);
        }
        let Some(record) = records.get(&parent_id) else {
            ancestor_boundary = true;
            break;
        };
        ancestors.push(record.clone());
        parent = record.parent_session.clone();
    }

    let mut children = BTreeMap::<SessionId, Vec<SessionLineageRecord>>::new();
    for record in records.values() {
        if let Some(parent) = &record.parent_session {
            children
                .entry(parent.clone())
                .or_default()
                .push(record.clone());
        }
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
    }
    let mut descendant_seen = BTreeSet::from([session_id]);
    let descendants = build_descendants(&children, target.session_id(), &mut descendant_seen)?;
    Ok(SessionTraceOutcome {
        target,
        ancestors,
        ancestor_boundary,
        descendants,
        corpus_incomplete,
    })
}

fn scan_lineage_candidate(
    candidate: super::store::SessionSearchCandidate,
    workspace: WorkspaceIdentity,
    cancelled: &AtomicBool,
    deadline: Instant,
    strict_target: bool,
) -> Result<Option<SessionLineageRecord>, SessionSearchError> {
    let metadata = candidate.metadata().clone();
    let length = candidate.file_length().map_err(map_store_error)?;
    let mut file = candidate.into_file();
    let scan = scan_jsonl_observing(
        &mut file,
        metadata.id(),
        cancelled,
        |header, identity| {
            if identity != workspace || header.id() != metadata.id() {
                return Err(StoreError::WorkspaceMismatch);
            }
            Ok(())
        },
        |_| {
            check_stop_store(cancelled, deadline)?;
            Ok(())
        },
    );
    let scan = match scan {
        Ok(scan) => scan,
        Err(StoreError::Cancelled) => return Err(stop_error(deadline)),
        Err(_) if !strict_target => return Ok(None),
        Err(_) => return Err(SessionSearchError::Unavailable),
    };
    if scan.physical_bytes() != length
        || scan.valid_bytes() != length
        || !scan.is_quiescent_for_search()
    {
        return if strict_target {
            Err(SessionSearchError::Unavailable)
        } else {
            Ok(None)
        };
    }
    Ok(Some(SessionLineageRecord {
        session_id: scan.header().id().clone(),
        created_at: scan.header().created_at().get(),
        parent_session: scan.header().parent_session().cloned(),
    }))
}

fn build_descendants(
    children: &BTreeMap<SessionId, Vec<SessionLineageRecord>>,
    parent: &SessionId,
    seen: &mut BTreeSet<SessionId>,
) -> Result<Vec<SessionLineageNode>, SessionSearchError> {
    let Some(records) = children.get(parent) else {
        return Ok(Vec::new());
    };
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(records.len())
        .map_err(|_| SessionSearchError::Unavailable)?;
    for record in records {
        if !seen.insert(record.session_id.clone()) {
            return Err(SessionSearchError::Unavailable);
        }
        let descendants = build_descendants(children, &record.session_id, seen)?;
        nodes.push(SessionLineageNode {
            record: record.clone(),
            descendants,
        });
    }
    Ok(nodes)
}

#[allow(clippy::too_many_arguments)]
fn event_trace_sync(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    session_id: SessionId,
    seq: u64,
    cancelled: &AtomicBool,
    deadline: Instant,
) -> Result<SessionEventTraceOutcome, SessionSearchError> {
    let candidate = find_target_candidate(store, workspace, caller, &session_id)?;
    check_target_length(&candidate)?;
    let metadata = candidate.metadata().clone();
    let length = candidate.file_length().map_err(map_store_error)?;
    let mut file = candidate.into_file();
    let target_seq = EventSeq::new(seq).map_err(|_| SessionSearchError::Invalid)?;
    let mut target = None;
    let mut surface_nodes = Vec::<EventSeq>::new();
    let mut replaced_by = BTreeMap::<EventSeq, EventSeq>::new();
    let mut replaced_events = BTreeMap::<EventSeq, Vec<EventSeq>>::new();
    let mut derived = Vec::new();
    let scan = scan_jsonl_observing(
        &mut file,
        &session_id,
        cancelled,
        |header, identity| {
            if identity != workspace || header.id() != metadata.id() {
                return Err(StoreError::WorkspaceMismatch);
            }
            Ok(())
        },
        |event| {
            check_stop_store(cancelled, deadline)?;
            if event.seq() == target_seq {
                target = Some((
                    event.kind().event_type().to_owned(),
                    event.time().get(),
                    event.surface_op().is_some(),
                    event.source_event_seqs().unwrap_or_default().to_vec(),
                ));
            } else if event.seq() > target_seq
                && event
                    .source_event_seqs()
                    .is_some_and(|sources| sources.contains(&target_seq))
            {
                derived.push(event.seq());
            }
            match event.surface_op() {
                Some(super::SurfaceOp::Append(_)) => surface_nodes.push(event.seq()),
                Some(super::SurfaceOp::Replace(replacement)) => {
                    let start = surface_nodes
                        .iter()
                        .position(|node| *node == replacement.start)
                        .ok_or(StoreError::Corrupt)?;
                    let end = surface_nodes
                        .iter()
                        .position(|node| *node == replacement.end)
                        .ok_or(StoreError::Corrupt)?;
                    if start > end {
                        return Err(StoreError::Corrupt);
                    }
                    let removed = surface_nodes[start..=end].to_vec();
                    for removed_seq in &removed {
                        replaced_by.insert(*removed_seq, event.seq());
                    }
                    replaced_events.insert(event.seq(), removed);
                    surface_nodes.splice(start..=end, [event.seq()]);
                }
                None => {}
            }
            Ok(())
        },
    )
    .map_err(|error| map_target_scan_error(error, deadline))?;
    if scan.physical_bytes() != length
        || scan.valid_bytes() != length
        || !scan.is_quiescent_for_search()
    {
        return Err(SessionSearchError::Unavailable);
    }
    let (target_type, target_time, surface_event, sources) =
        target.ok_or(SessionSearchError::EventNotFound)?;
    let immediate = replaced_by.get(&target_seq).copied();
    let mut replacement_chain = Vec::new();
    let mut replacement = immediate;
    while let Some(replacer) = replacement {
        replacement_chain.push(replacer.get());
        replacement = replaced_by.get(&replacer).copied();
    }
    let target_surface = if !surface_event {
        SessionEventSurface::LogOnly
    } else if surface_nodes.contains(&target_seq) {
        SessionEventSurface::Current
    } else {
        SessionEventSurface::Shadowed
    };
    Ok(SessionEventTraceOutcome {
        session_id,
        target_seq: seq,
        target_type,
        target_time,
        target_surface,
        replaced_by: immediate.map(EventSeq::get),
        replacement_chain,
        replaced_event_seqs: replaced_events
            .remove(&target_seq)
            .unwrap_or_default()
            .into_iter()
            .map(EventSeq::get)
            .collect(),
        source_event_seqs: sources.into_iter().map(EventSeq::get).collect(),
        derived_event_seqs: derived.into_iter().map(EventSeq::get).collect(),
    })
}

fn find_target_candidate(
    store: &SessionStore,
    workspace: WorkspaceIdentity,
    caller: &SessionId,
    session_id: &SessionId,
) -> Result<super::store::SessionSearchCandidate, SessionSearchError> {
    store
        .search_candidates(workspace, caller)
        .map_err(map_store_error)?
        .into_iter()
        .find(|candidate| candidate.metadata().id() == session_id)
        .ok_or(SessionSearchError::SessionNotFound)
}

fn check_target_length(
    candidate: &super::store::SessionSearchCandidate,
) -> Result<(), SessionSearchError> {
    if candidate.file_length().map_err(map_store_error)? > MAX_SESSION_SEARCH_SESSION_BYTES {
        return Err(SessionSearchError::Unavailable);
    }
    Ok(())
}

fn map_target_scan_error(error: StoreError, deadline: Instant) -> SessionSearchError {
    match error {
        StoreError::Cancelled => stop_error(deadline),
        _ => SessionSearchError::Unavailable,
    }
}

fn consider_event_hit(
    hits: &mut Vec<SessionEventSearchHit>,
    match_count: &mut u64,
    event: &SessionEvent,
    query: &SessionSearchQuery,
) -> Result<(), ()> {
    let text = extract_event_text(event);
    if text.is_empty() {
        return Ok(());
    }
    let mut matches = query.pattern.find_iter(&text);
    let Some(first) = matches.next() else {
        return Ok(());
    };
    let mut score = 1_u32;
    for _ in matches {
        score = score.saturating_add(1);
    }
    *match_count = match_count.saturating_add(1);
    hits.try_reserve(1).map_err(|_| ())?;
    hits.push(SessionEventSearchHit {
        event_seq: event.seq().get(),
        event_type: event.kind().event_type().to_owned(),
        event_time: event.time().get(),
        surface: SessionEventSurface::LogOnly,
        snippet: make_snippet(&text, first.start(), MAX_SESSION_SEARCH_SNIPPET_CHARS),
        score,
        document_chars: text.chars().count(),
        surface_event: event.surface_op().is_some(),
    });
    hits.sort_by(compare_event_hits);
    hits.truncate(MAX_SESSION_SEARCH_RESULTS);
    Ok(())
}

fn compare_event_hits(
    left: &SessionEventSearchHit,
    right: &SessionEventSearchHit,
) -> std::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document_chars.cmp(&right.document_chars))
        .then_with(|| right.event_time.cmp(&left.event_time))
        .then_with(|| right.event_seq.cmp(&left.event_seq))
}

fn summarize_event(event: &SessionEvent) -> SessionEventSummary {
    let text = extract_event_text(event);
    SessionEventSummary {
        event_seq: event.seq().get(),
        event_type: event.kind().event_type().to_owned(),
        event_time: event.time().get(),
        text: (!text.is_empty()).then(|| make_snippet(&text, 0, MAX_SESSION_SEARCH_SNIPPET_CHARS)),
    }
}

fn check_stop(cancelled: &AtomicBool, deadline: Instant) -> Result<(), SessionSearchError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(stop_error(deadline));
    }
    if Instant::now() >= deadline {
        return Err(SessionSearchError::Timeout);
    }
    Ok(())
}

fn check_stop_store(cancelled: &AtomicBool, deadline: Instant) -> Result<(), StoreError> {
    if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn stop_error(deadline: Instant) -> SessionSearchError {
    if Instant::now() >= deadline {
        SessionSearchError::Timeout
    } else {
        SessionSearchError::Cancelled
    }
}

fn map_store_error(error: StoreError) -> SessionSearchError {
    match error {
        StoreError::Cancelled => SessionSearchError::Cancelled,
        _ => SessionSearchError::Unavailable,
    }
}

struct BestMatch {
    event_seq: u64,
    event_type: String,
    event_time: i64,
    snippet: String,
    score: u32,
}

fn consider_event(best: &mut Option<BestMatch>, event: &SessionEvent, query: &SessionSearchQuery) {
    let text = extract_event_text(event);
    if text.is_empty() {
        return;
    }
    let mut matches = query.pattern.find_iter(&text);
    let Some(first) = matches.next() else {
        return;
    };
    let mut score = 1_u32;
    for _ in matches {
        score = score.saturating_add(1);
    }
    let event_seq = event.seq().get();
    let replace = best.as_ref().is_none_or(|current| {
        score > current.score || (score == current.score && event_seq > current.event_seq)
    });
    if replace {
        *best = Some(BestMatch {
            event_seq,
            event_type: event.kind().event_type().to_owned(),
            event_time: event.time().get(),
            snippet: make_snippet(&text, first.start(), MAX_SESSION_SEARCH_SNIPPET_CHARS),
            score,
        });
    }
}

fn extract_event_text(event: &SessionEvent) -> String {
    match event.kind() {
        EventKind::UserMessage { message } | EventKind::AssistantMessage { message, .. } => {
            message_text(message)
        }
        EventKind::ToolCall {
            name, arguments, ..
        } => join_text([name.as_str(), arguments.as_str()]),
        EventKind::ToolResult { message, error, .. } => {
            let mut parts = Vec::new();
            push_message_text(message, &mut parts);
            if let Some(error) = error {
                parts.push(error.name.as_str());
                parts.push(error.code.as_str());
            }
            join_text(parts)
        }
        EventKind::TodoWrite { todos } => {
            let mut parts = Vec::with_capacity(todos.len().saturating_mul(2));
            for todo in todos {
                parts.push(match todo.status {
                    TodoStatus::Pending => "pending",
                    TodoStatus::InProgress => "in_progress",
                    TodoStatus::Completed => "completed",
                });
                parts.push(todo.content.as_str());
            }
            join_text(parts)
        }
        EventKind::TurnEnd { reason, .. } => match reason {
            TurnEndReason::Error { error } => join_text(["error", error.message()]),
            TurnEndReason::Aborted { .. } => "aborted".to_owned(),
            TurnEndReason::MaxTokens => "max-tokens".to_owned(),
            TurnEndReason::Interrupted => "interrupted".to_owned(),
            TurnEndReason::Completed | TurnEndReason::Blocked | TurnEndReason::Other { .. } => {
                String::new()
            }
        },
        EventKind::TurnStart { .. }
        | EventKind::StepStart { .. }
        | EventKind::StepEnd { .. }
        | EventKind::AssistantChunk { .. }
        | EventKind::GoalChange { .. }
        | EventKind::PlanMode { .. }
        | EventKind::RequestHeader { .. }
        | EventKind::RequestContext { .. }
        | EventKind::LlmRetry { .. }
        | EventKind::LlmRetryStarted { .. }
        | EventKind::ApprovalAsked { .. }
        | EventKind::ApprovalDecided { .. }
        | EventKind::CompactionStart { .. }
        | EventKind::CompactionSummary { .. }
        | EventKind::CompactionEnd { .. }
        | EventKind::CompactionPrune { .. }
        | EventKind::EndSeed
        | EventKind::Unknown { .. } => String::new(),
    }
}

fn message_text(message: &Message) -> String {
    let mut parts = Vec::new();
    push_message_text(message, &mut parts);
    join_text(parts)
}

fn push_message_text<'a>(message: &'a Message, parts: &mut Vec<&'a str>) {
    if !matches!(message.role(), MessageRole::User | MessageRole::Assistant) {
        return;
    }
    for block in message.content() {
        push_raw_block_text(block.raw().as_value(), 0, parts);
    }
}

fn push_raw_block_text<'a>(value: &'a serde_json::Value, depth: usize, parts: &mut Vec<&'a str>) {
    if depth >= MAX_BLOCK_NESTING {
        return;
    }
    let Some(fields) = value.as_object() else {
        return;
    };
    match fields.get("type").and_then(serde_json::Value::as_str) {
        Some("text") => {
            if let Some(text) = fields.get("text").and_then(serde_json::Value::as_str) {
                parts.push(text);
            }
        }
        Some("tool-call") => {
            if let Some(name) = fields.get("name").and_then(serde_json::Value::as_str) {
                parts.push(name);
            }
            if let Some(arguments) = fields.get("arguments").and_then(serde_json::Value::as_str) {
                parts.push(arguments);
            }
        }
        Some("tool-result") => {
            if let Some(content) = fields.get("content").and_then(serde_json::Value::as_array) {
                for block in content {
                    push_raw_block_text(block, depth + 1, parts);
                }
            }
        }
        Some("reasoning") | Some(_) | None => {}
    }
}

fn join_text<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_snippet(text: &str, match_byte: usize, maximum: usize) -> String {
    let (normalized, match_index) = normalize_with_match(text, match_byte);
    let characters = normalized.chars().collect::<Vec<_>>();
    if characters.len() <= maximum {
        return normalized;
    }
    if maximum == 1 {
        return "…".to_owned();
    }
    let matched = match_index.min(characters.len().saturating_sub(1));
    let mut start = matched.saturating_sub(maximum / 3);
    let mut prefix = usize::from(start > 0);
    let mut suffix = 1_usize;
    let mut content = maximum.saturating_sub(prefix + suffix);
    if content == 0 {
        start = matched;
        suffix = 0;
        content = maximum.saturating_sub(prefix);
    } else if matched >= start.saturating_add(content) {
        start = matched.saturating_sub(content.saturating_sub(1));
        prefix = usize::from(start > 0);
        content = maximum.saturating_sub(prefix + suffix);
    }
    let mut end = characters.len().min(start.saturating_add(content));
    if end == characters.len() {
        suffix = 0;
        content = maximum.saturating_sub(prefix);
        start = end.saturating_sub(content);
        prefix = usize::from(start > 0);
        content = maximum.saturating_sub(prefix);
    }
    end = characters.len().min(start.saturating_add(content));
    let mut result = String::new();
    if prefix > 0 {
        result.push('…');
    }
    result.extend(characters[start..end].iter());
    if suffix > 0 {
        result.push('…');
    }
    result
}

fn normalize_with_match(text: &str, match_byte: usize) -> (String, usize) {
    let mut output = String::new();
    let mut output_chars = 0_usize;
    let mut match_index = None;
    let mut pending_space = false;
    for (byte, character) in text.char_indices() {
        if match_index.is_none() && byte >= match_byte {
            match_index = Some(output_chars + usize::from(pending_space && !output.is_empty()));
        }
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            output_chars = output_chars.saturating_add(1);
            pending_space = false;
        }
        output.push(character);
        output_chars = output_chars.saturating_add(1);
    }
    (output, match_index.unwrap_or(output_chars))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write as _,
        os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    };

    use serde_json::json;

    use crate::{
        model::{CallId, Message, MessageSource},
        session::{
            EventSeq, NewEvent, Session, SessionHeader, SurfaceIntent, TodoItem, TurnEndReason,
            TurnId, UnixMillis,
        },
        workspace_authority::WorkspaceAuthority,
    };

    use super::*;

    fn event(seq: u64, kind: EventKind) -> SessionEvent {
        let data = super::super::codec::kind_data_value(&kind).unwrap();
        SessionEvent::from_new(
            EventSeq::new(seq).unwrap(),
            UnixMillis::new(1_000 + seq as i64).unwrap(),
            NewEvent::log(kind),
            crate::model::JsonValue::new(data).unwrap(),
        )
    }

    #[test]
    fn query_is_literal_case_insensitive_and_whitespace_flexible() {
        let query = SessionSearchQuery::new("  CAFÉ   (ai)+ ").unwrap();
        assert!(query.pattern.is_match("prefix café\n(AI)+ suffix"));
        assert!(!query.pattern.is_match("café anything (AI)+"));
        assert_eq!(
            SessionSearchQuery::new(" \n ").unwrap_err(),
            SessionSearchError::Invalid
        );
        assert_eq!(
            SessionSearchQuery::new("bad\0query").unwrap_err(),
            SessionSearchError::Invalid
        );
        assert!(SessionSearchQuery::new(&"x".repeat(MAX_SESSION_SEARCH_QUERY_BYTES)).is_ok());
        assert_eq!(
            SessionSearchQuery::new(&"x".repeat(MAX_SESSION_SEARCH_QUERY_BYTES + 1)).unwrap_err(),
            SessionSearchError::Invalid
        );
    }

    #[test]
    fn semantic_extraction_matches_the_fixed_first_party_boundary() {
        let user = Message::user(
            "user-1",
            vec![crate::model::ContentBlock::text("hello").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        assert_eq!(
            extract_event_text(&event(0, EventKind::user_message(user))),
            "hello"
        );
        assert_eq!(
            extract_event_text(&event(
                1,
                EventKind::tool_call(
                    TurnId::new(1).unwrap(),
                    crate::session::StepId::new(1).unwrap(),
                    CallId::new("call-1"),
                    "grep",
                    r#"{"pattern":"needle"}"#,
                ),
            )),
            "grep\n{\"pattern\":\"needle\"}"
        );
        let structural = event(
            2,
            EventKind::step_start(
                TurnId::new(1).unwrap(),
                crate::session::StepId::new(1).unwrap(),
            ),
        );
        assert_eq!(extract_event_text(&structural), "");
    }

    #[test]
    fn ranking_prefers_occurrences_then_later_event_and_snippets_are_bounded() {
        let source = MessageSource::user().unwrap();
        let first = Message::user(
            "user-1",
            vec![crate::model::ContentBlock::text("needle once").unwrap()],
            source.clone(),
        )
        .unwrap();
        let second = Message::user(
            "user-2",
            vec![
                crate::model::ContentBlock::text(format!(
                    "{} needle middle needle {}",
                    "x".repeat(300),
                    "y".repeat(300)
                ))
                .unwrap(),
            ],
            source,
        )
        .unwrap();
        let query = SessionSearchQuery::new("needle").unwrap();
        let mut best = None;
        consider_event(&mut best, &event(0, EventKind::user_message(first)), &query);
        consider_event(
            &mut best,
            &event(1, EventKind::user_message(second)),
            &query,
        );
        let best = best.unwrap();
        assert_eq!(best.event_seq, 1);
        assert_eq!(best.score, 2);
        assert!(best.snippet.chars().count() <= MAX_SESSION_SEARCH_SNIPPET_CHARS);
        assert!(best.snippet.contains("needle"));
        assert!(best.snippet.starts_with('…'));
        assert!(best.snippet.ends_with('…'));
    }

    #[test]
    fn fixed_fixture_records_the_reduced_contract() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase36_session_search.json"
        ))
        .unwrap();
        assert_eq!(fixture["schema"]["name"], "session_search");
        assert_eq!(
            fixture["limits"]["queryBytes"],
            json!(MAX_SESSION_SEARCH_QUERY_BYTES)
        );
        assert_eq!(
            fixture["limits"]["results"],
            json!(MAX_SESSION_SEARCH_RESULTS)
        );
        assert_eq!(
            fixture["limits"]["snippetCodePoints"],
            json!(MAX_SESSION_SEARCH_SNIPPET_CHARS)
        );

        let navigation: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase39_session_event_navigation.json"
        ))
        .unwrap();
        assert_eq!(navigation["eventSearch"]["name"], "session_event_search");
        assert_eq!(navigation["eventRead"]["name"], "session_event_read");
        assert_eq!(
            navigation["eventRead"]["maxSideWindow"],
            MAX_SESSION_EVENT_READ_WINDOW
        );

        let tracing: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase40_session_tracing.json"
        ))
        .unwrap();
        assert_eq!(tracing["sessionTrace"]["name"], "session_trace");
        assert_eq!(tracing["eventTrace"]["name"], "session_event_trace");
        assert_eq!(
            tracing["rustIntentionalDifference"]["lineageCorpusBytes"],
            MAX_SESSION_SEARCH_AGGREGATE_BYTES
        );
    }

    #[tokio::test]
    async fn traces_complete_partial_and_cyclic_session_lineage_deterministically() {
        let root = private_directory("lineage-store");
        let workspace = private_directory("lineage-workspace");
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let root_id = SessionId::new("session-015e8400-e29b-41d4-a716-446655440000");
        let parent_id = SessionId::new("session-115e8400-e29b-41d4-a716-446655440000");
        let target_id = SessionId::new("session-215e8400-e29b-41d4-a716-446655440000");
        let older_id = SessionId::new("session-315e8400-e29b-41d4-a716-446655440000");
        let child_id = SessionId::new("session-415e8400-e29b-41d4-a716-446655440000");
        let grandchild_id = SessionId::new("session-515e8400-e29b-41d4-a716-446655440000");
        write_lineage_history(&root, &authority, &root_id, 1_000, None);
        write_lineage_history(&root, &authority, &parent_id, 2_000, Some(&root_id));
        write_lineage_history(&root, &authority, &target_id, 3_000, Some(&parent_id));
        write_lineage_history(&root, &authority, &older_id, 4_000, Some(&target_id));
        write_lineage_history(&root, &authority, &child_id, 5_000, Some(&target_id));
        write_lineage_history(&root, &authority, &grandchild_id, 6_000, Some(&child_id));
        let runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            SessionId::new("session-615e8400-e29b-41d4-a716-446655440000"),
        );
        let trace = runtime
            .trace_session(target_id.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(trace.target().session_id(), &target_id);
        assert_eq!(
            trace
                .ancestors()
                .iter()
                .map(SessionLineageRecord::session_id)
                .collect::<Vec<_>>(),
            [&parent_id, &root_id]
        );
        assert!(!trace.ancestor_boundary());
        assert_eq!(
            trace
                .descendants()
                .iter()
                .map(|node| node.record().session_id())
                .collect::<Vec<_>>(),
            [&older_id, &child_id]
        );
        assert_eq!(
            trace.descendants()[1].descendants()[0]
                .record()
                .session_id(),
            &grandchild_id
        );

        let partial_id = SessionId::new("session-715e8400-e29b-41d4-a716-446655440000");
        let absent_id = SessionId::new("session-815e8400-e29b-41d4-a716-446655440000");
        write_lineage_history(&root, &authority, &partial_id, 7_000, Some(&absent_id));
        let partial = runtime
            .trace_session(partial_id, CancellationToken::new())
            .await
            .unwrap();
        assert!(partial.ancestor_boundary());
        assert!(partial.ancestors().is_empty());

        let cycle_a = SessionId::new("session-915e8400-e29b-41d4-a716-446655440000");
        let cycle_b = SessionId::new("session-a15e8400-e29b-41d4-a716-446655440000");
        write_lineage_history(&root, &authority, &cycle_a, 8_000, Some(&cycle_b));
        write_lineage_history(&root, &authority, &cycle_b, 9_000, Some(&cycle_a));
        assert_eq!(
            runtime
                .trace_session(cycle_a, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn event_trace_separates_replacements_sources_and_derived_events() {
        let root = private_directory("event-trace-store");
        let workspace = private_directory("event-trace-workspace");
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let id = SessionId::new("session-b15e8400-e29b-41d4-a716-446655440000");
        let (original, first, final_seq, log_only) =
            write_event_trace_history(&root, &authority, &id);
        let runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            SessionId::new("session-c15e8400-e29b-41d4-a716-446655440000"),
        );
        let trace = runtime
            .trace_event(id.clone(), original, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(trace.target_surface(), SessionEventSurface::Shadowed);
        assert_eq!(trace.replaced_by(), Some(first));
        assert_eq!(trace.replacement_chain(), &[first, final_seq]);
        assert!(trace.replaced_event_seqs().is_empty());
        assert!(trace.source_event_seqs().is_empty());
        assert_eq!(trace.derived_event_seqs(), &[first]);

        let replacement = runtime
            .trace_event(id.clone(), first, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(replacement.replaced_by(), Some(final_seq));
        assert_eq!(replacement.replaced_event_seqs(), &[original]);
        assert_eq!(replacement.source_event_seqs(), &[original]);
        assert_eq!(replacement.derived_event_seqs(), &[final_seq]);
        let current = runtime
            .trace_event(id.clone(), final_seq, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(current.target_surface(), SessionEventSurface::Current);
        assert_eq!(current.replaced_event_seqs(), &[first]);
        assert_eq!(current.source_event_seqs(), &[first]);
        let log = runtime
            .trace_event(id.clone(), log_only, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(log.target_surface(), SessionEventSurface::LogOnly);
        assert_eq!(
            runtime
                .trace_event(id, 9_999, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::EventNotFound
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn event_navigation_ranks_surfaces_reads_exact_json_and_hides_unauthorized_targets() {
        let root = private_directory("event-navigation-store");
        let workspace = private_directory("event-navigation-workspace");
        let other_workspace = private_directory("event-navigation-other");
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let other = WorkspaceAuthority::open(&other_workspace).unwrap();
        let visible = SessionId::new("session-155e8400-e29b-41d4-a716-446655440000");
        let hidden = SessionId::new("session-255e8400-e29b-41d4-a716-446655440000");
        let busy = SessionId::new("session-355e8400-e29b-41d4-a716-446655440000");
        let target_seq = write_navigation_history(&root, &authority, &visible);
        write_history(&root, &other, &hidden, "needle hidden");
        let busy_path = write_history(&root, &authority, &busy, "needle busy");
        let busy_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&busy_path)
            .unwrap();
        rustix::fs::flock(
            &busy_file,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();

        let caller = SessionId::new("session-455e8400-e29b-41d4-a716-446655440000");
        let runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            caller,
        );
        let searched = runtime
            .search_events(
                visible.clone(),
                SessionSearchQuery::new("needle").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(searched.session_id(), &visible);
        assert_eq!(searched.hits().len(), 3);
        assert_eq!(searched.hits()[0].surface(), SessionEventSurface::LogOnly);
        assert_eq!(searched.hits()[1].surface(), SessionEventSurface::Current);
        assert_eq!(searched.hits()[2].surface(), SessionEventSurface::Shadowed);
        assert!(!searched.result_capped());

        let read = runtime
            .read_event(visible.clone(), target_seq, 1, 1, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(read.target().seq().get(), target_seq);
        assert_eq!(read.before().len(), 1);
        assert_eq!(read.after().len(), 1);
        assert!(read.before()[0].text().unwrap().contains("needle shadowed"));
        assert!(read.after()[0].text().unwrap().contains("needle log"));
        let target = serde_json::to_value(read.target()).unwrap();
        assert_eq!(target["type"], "user/message");
        assert!(target.to_string().contains("needle current needle"));

        assert_eq!(
            runtime
                .read_event(visible.clone(), 9_999, 0, 0, CancellationToken::new(),)
                .await
                .unwrap_err(),
            SessionSearchError::EventNotFound
        );
        for unavailable in [&hidden, &busy] {
            assert_eq!(
                runtime
                    .search_events(
                        unavailable.clone(),
                        SessionSearchQuery::new("needle").unwrap(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap_err(),
                SessionSearchError::SessionNotFound
            );
            assert_eq!(
                runtime
                    .trace_event(unavailable.clone(), target_seq, CancellationToken::new())
                    .await
                    .unwrap_err(),
                SessionSearchError::SessionNotFound
            );
            assert_eq!(
                runtime
                    .trace_session(unavailable.clone(), CancellationToken::new())
                    .await
                    .unwrap_err(),
                SessionSearchError::SessionNotFound
            );
        }
        let self_runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            visible.clone(),
        );
        assert_eq!(
            self_runtime
                .read_event(visible.clone(), target_seq, 0, 0, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::SessionNotFound
        );
        assert_eq!(
            self_runtime
                .trace_session(visible, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::SessionNotFound
        );

        drop(busy_file);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(other_workspace).unwrap();
    }

    #[tokio::test]
    async fn store_search_is_workspace_bound_and_excludes_caller_and_busy_journals() {
        let root = private_directory("search-store");
        let workspace = private_directory("search-workspace");
        let other_workspace = private_directory("search-other-workspace");
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let other = WorkspaceAuthority::open(&other_workspace).unwrap();
        let visible = SessionId::new("session-550e8400-e29b-41d4-a716-446655440000");
        let busy = SessionId::new("session-650e8400-e29b-41d4-a716-446655440000");
        let hidden = SessionId::new("session-750e8400-e29b-41d4-a716-446655440000");
        write_history(&root, &authority, &visible, "alpha shared marker");
        let busy_path = write_history(&root, &authority, &busy, "alpha busy marker");
        write_history(&root, &other, &hidden, "alpha hidden marker");

        let busy_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&busy_path)
            .unwrap();
        rustix::fs::flock(
            &busy_file,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();

        let runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            SessionId::new("session-850e8400-e29b-41d4-a716-446655440000"),
        );
        let outcome = runtime
            .search(
                SessionSearchQuery::new("ALPHA   shared").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome
                .hits()
                .iter()
                .map(|hit| hit.session_id().clone())
                .collect::<Vec<_>>(),
            [visible.clone()]
        );

        let caller_runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            visible,
        );
        let caller_outcome = caller_runtime
            .search(
                SessionSearchQuery::new("alpha shared").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(caller_outcome.hits().is_empty());

        drop(busy_file);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(other_workspace).unwrap();
    }

    #[tokio::test]
    async fn malformed_and_oversized_candidates_are_not_exposed_and_cancellation_wins() {
        let root = private_directory("search-bounds-store");
        let workspace = private_directory("search-bounds-workspace");
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let malformed = SessionId::new("session-950e8400-e29b-41d4-a716-446655440000");
        let malformed_path = write_history(&root, &authority, &malformed, "bounded marker");
        OpenOptions::new()
            .append(true)
            .open(&malformed_path)
            .unwrap()
            .write_all(b"{broken\n")
            .unwrap();
        let oversized = SessionId::new("session-a50e8400-e29b-41d4-a716-446655440000");
        let oversized_path = write_history(&root, &authority, &oversized, "bounded marker");
        OpenOptions::new()
            .write(true)
            .open(&oversized_path)
            .unwrap()
            .set_len(MAX_SESSION_SEARCH_SESSION_BYTES + 1)
            .unwrap();
        let runtime = SessionSearchRuntime::new(
            SessionStore::open_existing(&root).unwrap(),
            authority.identity(),
            SessionId::new("session-b50e8400-e29b-41d4-a716-446655440000"),
        );
        let outcome = runtime
            .search(
                SessionSearchQuery::new("bounded marker").unwrap(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(outcome.hits().is_empty());
        assert!(outcome.scan_capped());

        assert_eq!(
            runtime
                .search_events(
                    malformed.clone(),
                    SessionSearchQuery::new("bounded").unwrap(),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );
        assert_eq!(
            runtime
                .read_event(oversized.clone(), 0, 0, 0, CancellationToken::new(),)
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );
        assert_eq!(
            runtime
                .trace_session(oversized, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );
        assert_eq!(
            runtime
                .trace_event(malformed.clone(), 0, CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );
        assert_eq!(
            runtime
                .trace_session(malformed.clone(), CancellationToken::new())
                .await
                .unwrap_err(),
            SessionSearchError::Unavailable
        );

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            runtime
                .search(SessionSearchQuery::new("bounded").unwrap(), cancellation,)
                .await
                .unwrap_err(),
            SessionSearchError::Cancelled
        );
        let event_cancellation = CancellationToken::new();
        event_cancellation.cancel();
        assert_eq!(
            runtime
                .search_events(
                    malformed.clone(),
                    SessionSearchQuery::new("bounded").unwrap(),
                    event_cancellation,
                )
                .await
                .unwrap_err(),
            SessionSearchError::Cancelled
        );
        let trace_cancellation = CancellationToken::new();
        trace_cancellation.cancel();
        assert_eq!(
            runtime
                .trace_session(malformed.clone(), trace_cancellation)
                .await
                .unwrap_err(),
            SessionSearchError::Cancelled
        );
        assert_eq!(
            search_sync(
                &SessionStore::open_existing(&root).unwrap(),
                authority.identity(),
                &SessionId::new("session-c50e8400-e29b-41d4-a716-446655440000"),
                &SessionSearchQuery::new("bounded").unwrap(),
                &AtomicBool::new(false),
                Instant::now() - Duration::from_millis(1),
            )
            .unwrap_err(),
            SessionSearchError::Timeout
        );
        assert_eq!(
            event_search_sync(
                &SessionStore::open_existing(&root).unwrap(),
                authority.identity(),
                &SessionId::new("session-c50e8400-e29b-41d4-a716-446655440000"),
                malformed.clone(),
                &SessionSearchQuery::new("bounded").unwrap(),
                &AtomicBool::new(false),
                Instant::now() - Duration::from_millis(1),
            )
            .unwrap_err(),
            SessionSearchError::Timeout
        );
        assert_eq!(
            event_trace_sync(
                &SessionStore::open_existing(&root).unwrap(),
                authority.identity(),
                &SessionId::new("session-c50e8400-e29b-41d4-a716-446655440000"),
                malformed,
                0,
                &AtomicBool::new(false),
                Instant::now() - Duration::from_millis(1),
            )
            .unwrap_err(),
            SessionSearchError::Timeout
        );
        assert_eq!(
            session_trace_sync(
                &SessionStore::open_existing(&root).unwrap(),
                authority.identity(),
                &SessionId::new("session-c50e8400-e29b-41d4-a716-446655440000"),
                SessionId::new("session-d50e8400-e29b-41d4-a716-446655440000"),
                &AtomicBool::new(false),
                Instant::now() - Duration::from_millis(1),
            )
            .unwrap_err(),
            SessionSearchError::Timeout
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
    }

    fn private_directory(label: &str) -> std::path::PathBuf {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!(
            "dsh-session-search-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn write_history(
        root: &std::path::Path,
        authority: &WorkspaceAuthority,
        id: &SessionId,
        text: &str,
    ) -> std::path::PathBuf {
        let header = SessionHeader::new_durable(
            id.clone(),
            UnixMillis::new(1_000).unwrap(),
            authority.canonical_path().to_str().unwrap().to_owned(),
            authority.identity(),
        )
        .unwrap();
        let mut session = Session::new(id.as_str()).unwrap();
        let turn = TurnId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        let message = Message::user(
            "historical-user",
            vec![crate::model::ContentBlock::text(text).unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        session
            .append(NewEvent::surface(
                EventKind::user_message(message),
                SurfaceIntent::append(),
            ))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();

        write_session(root, &header, &session)
    }

    fn write_navigation_history(
        root: &std::path::Path,
        authority: &WorkspaceAuthority,
        id: &SessionId,
    ) -> u64 {
        let header = SessionHeader::new_durable(
            id.clone(),
            UnixMillis::new(1_000).unwrap(),
            authority.canonical_path().to_str().unwrap().to_owned(),
            authority.identity(),
        )
        .unwrap();
        let mut session = Session::new(id.as_str()).unwrap();
        let turn = TurnId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        let original = Message::user(
            "historical-original",
            vec![crate::model::ContentBlock::text("needle shadowed").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        let original = session
            .append(NewEvent::surface(
                EventKind::user_message(original),
                SurfaceIntent::append(),
            ))
            .unwrap();
        let replacement = Message::user(
            "historical-replacement",
            vec![crate::model::ContentBlock::text("needle current needle").unwrap()],
            MessageSource::plugin("test-replacement").unwrap(),
        )
        .unwrap();
        let replacement = session
            .append(NewEvent::surface(
                EventKind::user_message(replacement),
                SurfaceIntent::replace(original.seq(), original.seq(), vec![original.seq()]),
            ))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::TodoWrite {
                todos: vec![TodoItem {
                    content: "needle log needle log needle log".to_owned(),
                    status: TodoStatus::Pending,
                }],
            }))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();
        write_session(root, &header, &session);
        replacement.seq().get()
    }

    fn write_lineage_history(
        root: &std::path::Path,
        authority: &WorkspaceAuthority,
        id: &SessionId,
        created_at: i64,
        parent: Option<&SessionId>,
    ) -> std::path::PathBuf {
        let base = SessionHeader::new_durable(
            id.clone(),
            UnixMillis::new(created_at).unwrap(),
            authority.canonical_path().to_str().unwrap().to_owned(),
            authority.identity(),
        )
        .unwrap();
        let mut raw = base.raw().as_value().clone();
        if let Some(parent) = parent {
            raw.as_object_mut()
                .unwrap()
                .insert("parentSession".to_owned(), json!(parent));
        }
        let header = SessionHeader::from_value(raw).unwrap();
        let mut session = Session::new(id.as_str()).unwrap();
        let turn = TurnId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();
        write_session(root, &header, &session)
    }

    fn write_event_trace_history(
        root: &std::path::Path,
        authority: &WorkspaceAuthority,
        id: &SessionId,
    ) -> (u64, u64, u64, u64) {
        let header = SessionHeader::new_durable(
            id.clone(),
            UnixMillis::new(1_000).unwrap(),
            authority.canonical_path().to_str().unwrap().to_owned(),
            authority.identity(),
        )
        .unwrap();
        let mut session = Session::new(id.as_str()).unwrap();
        let turn = TurnId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        let append_message = |id: &str, text: &str| {
            Message::user(
                id,
                vec![crate::model::ContentBlock::text(text).unwrap()],
                MessageSource::plugin("trace-test").unwrap(),
            )
            .unwrap()
        };
        let original = session
            .append(NewEvent::surface(
                EventKind::user_message(append_message("trace-original", "original")),
                SurfaceIntent::append(),
            ))
            .unwrap();
        let first = session
            .append(NewEvent::surface(
                EventKind::user_message(append_message("trace-first", "first")),
                SurfaceIntent::replace(original.seq(), original.seq(), vec![original.seq()]),
            ))
            .unwrap();
        let final_event = session
            .append(NewEvent::surface(
                EventKind::user_message(append_message("trace-final", "final")),
                SurfaceIntent::replace(first.seq(), first.seq(), vec![first.seq()]),
            ))
            .unwrap();
        let log_only = session
            .append(NewEvent::log(EventKind::TodoWrite {
                todos: vec![TodoItem {
                    content: "trace log".to_owned(),
                    status: TodoStatus::Pending,
                }],
            }))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();
        write_session(root, &header, &session);
        (
            original.seq().get(),
            first.seq().get(),
            final_event.seq().get(),
            log_only.seq().get(),
        )
    }

    fn write_session(
        root: &std::path::Path,
        header: &SessionHeader,
        session: &Session,
    ) -> std::path::PathBuf {
        let path = root.join(format!("{}.jsonl", header.id()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&super::super::jsonl::encode_header_line(header).unwrap())
            .unwrap();
        for event in session.events() {
            file.write_all(&super::super::jsonl::encode_event_line(event).unwrap())
                .unwrap();
        }
        file.flush().unwrap();
        path
    }
}
