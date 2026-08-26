//! Owned asynchronous lifecycle for enhanced workspace-file suggestions.

use std::{fmt, mem};

use thiserror::Error;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    tools::{WorkspaceFileCatalogue, WorkspaceFileCatalogueError},
    tui::{
        composer::Composer,
        file_suggestions::{
            FileSuggestionError, FileSuggestionSnapshot, FileTokenHit, RankedFileSnapshot,
            rank_catalogue,
        },
        input_memory::{InputMemory, InputMemoryError},
    },
};

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum FileSuggestionControllerError {
    #[error("CLI_FILE_SUGGESTION_STATE")]
    State,
    #[error("CLI_FILE_SUGGESTION_CAPACITY")]
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingKind {
    Scan,
    Filter,
}

struct ActiveHit {
    activation: u64,
    hit: FileTokenHit,
    draft_bytes: usize,
}

impl fmt::Debug for ActiveHit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveHit")
            .field("activation", &self.activation)
            .field("hit", &self.hit)
            .field("draft_bytes", &self.draft_bytes)
            .finish()
    }
}

enum RequestedMenu {
    Hidden,
    Loading {
        activation: u64,
        menu_revision: u64,
        hit: FileTokenHit,
    },
    Ready {
        activation: u64,
        menu_revision: u64,
        hit: FileTokenHit,
        ranked: RankedFileSnapshot,
        selected: Option<usize>,
    },
    Unavailable {
        activation: u64,
        menu_revision: u64,
    },
}

impl fmt::Debug for RequestedMenu {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => formatter.write_str("Hidden"),
            Self::Loading {
                activation,
                menu_revision,
                hit,
            } => formatter
                .debug_struct("Loading")
                .field("activation", activation)
                .field("menu_revision", menu_revision)
                .field("hit", hit)
                .finish(),
            Self::Ready {
                activation,
                menu_revision,
                hit,
                ranked,
                selected,
            } => formatter
                .debug_struct("Ready")
                .field("activation", activation)
                .field("menu_revision", menu_revision)
                .field("hit", hit)
                .field("ranked", ranked)
                .field("selected", selected)
                .finish(),
            Self::Unavailable {
                activation,
                menu_revision,
            } => formatter
                .debug_struct("Unavailable")
                .field("activation", activation)
                .field("menu_revision", menu_revision)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileSuggestionMove {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FileSuggestionEnter {
    Ordinary,
    Consumed,
    Completed,
}

enum PresentedMenu {
    Loading,
    Ready {
        ranked: RankedFileSnapshot,
        selected: usize,
    },
    Empty,
    Unavailable,
}

impl fmt::Debug for PresentedMenu {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loading => formatter.write_str("Loading"),
            Self::Ready { ranked, selected } => formatter
                .debug_struct("Ready")
                .field("ranked", ranked)
                .field("selected", selected)
                .finish(),
            Self::Empty => formatter.write_str("Empty"),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

pub(super) struct FileSuggestionPresentation {
    activation: u64,
    menu_revision: u64,
    hit: Option<FileTokenHit>,
    menu: PresentedMenu,
}

impl fmt::Debug for FileSuggestionPresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSuggestionPresentation")
            .field("activation", &self.activation)
            .field("menu_revision", &self.menu_revision)
            .field("hit", &self.hit)
            .field("menu", &self.menu)
            .finish()
    }
}

#[derive(Debug)]
pub(super) enum StagedFileSuggestionPresentation {
    Absent,
    Valid(FileSuggestionPresentation),
}

enum PresentedState {
    Absent,
    Valid(FileSuggestionPresentation),
    Invalidated,
}

impl fmt::Debug for PresentedState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absent => formatter.write_str("Absent"),
            Self::Valid(presentation) => {
                formatter.debug_tuple("Valid").field(presentation).finish()
            }
            Self::Invalidated => formatter.write_str("Invalidated"),
        }
    }
}

struct RunningJob {
    activation: u64,
    job_revision: u64,
    kind: BlockingKind,
    cancellation: CancellationToken,
    handle: JoinHandle<JobSettlement>,
}

impl fmt::Debug for RunningJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunningJob")
            .field("activation", &self.activation)
            .field("job_revision", &self.job_revision)
            .field("kind", &self.kind)
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("finished", &self.handle.is_finished())
            .finish()
    }
}

enum Lifecycle {
    Dormant,
    Scanning(RunningJob),
    Filtering(RunningJob),
    Cancelling {
        job: RunningJob,
        pending: Option<ActiveHit>,
    },
    Ready {
        activation: u64,
        catalogue: Vec<String>,
    },
    Failed {
        activation: u64,
    },
}

impl fmt::Debug for Lifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dormant => formatter.write_str("Dormant"),
            Self::Scanning(job) => formatter.debug_tuple("Scanning").field(job).finish(),
            Self::Filtering(job) => formatter.debug_tuple("Filtering").field(job).finish(),
            Self::Cancelling { job, pending } => formatter
                .debug_struct("Cancelling")
                .field("job", job)
                .field(
                    "pending_activation",
                    &pending.as_ref().map(|pending| pending.activation),
                )
                .finish(),
            Self::Ready {
                activation,
                catalogue,
            } => formatter
                .debug_struct("Ready")
                .field("activation", activation)
                .field("catalogue_count", &catalogue.len())
                .field(
                    "catalogue_bytes",
                    &catalogue.iter().map(String::len).sum::<usize>(),
                )
                .finish(),
            Self::Failed { activation } => formatter
                .debug_struct("Failed")
                .field("activation", activation)
                .finish(),
        }
    }
}

pub(super) struct FileSuggestionController {
    source: WorkspaceFileCatalogue,
    lifecycle: Lifecycle,
    active: Option<ActiveHit>,
    requested: RequestedMenu,
    presented: PresentedState,
    decoder_reset_required: bool,
    selected_path: Option<String>,
    dismissed_revision: Option<u64>,
    suppressed: bool,
    next_activation: u64,
    next_job_revision: u64,
    next_menu_revision: u64,
    scan_starts: u64,
    filter_starts: u64,
}

impl fmt::Debug for FileSuggestionController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSuggestionController")
            .field("source", &self.source)
            .field("lifecycle", &self.lifecycle)
            .field(
                "active_activation",
                &self.active.as_ref().map(|active| active.activation),
            )
            .field("requested", &self.requested)
            .field("presented", &self.presented)
            .field("decoder_reset_required", &self.decoder_reset_required)
            .field(
                "selected_path_bytes",
                &self.selected_path.as_ref().map(String::len),
            )
            .field("dismissed_revision", &self.dismissed_revision)
            .field("suppressed", &self.suppressed)
            .field("scan_starts", &self.scan_starts)
            .field("filter_starts", &self.filter_starts)
            .finish()
    }
}

pub(super) enum JobSettlement {
    Scan {
        activation: u64,
        job_revision: u64,
        result: Result<Vec<String>, WorkspaceFileCatalogueError>,
    },
    Filter {
        activation: u64,
        job_revision: u64,
        hit: FileTokenHit,
        catalogue: Vec<String>,
        result: Result<RankedFileSnapshot, FileSuggestionError>,
    },
}

impl fmt::Debug for JobSettlement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scan {
                activation,
                job_revision,
                result,
            } => formatter
                .debug_struct("Scan")
                .field("activation", activation)
                .field("job_revision", job_revision)
                .field("success_count", &result.as_ref().ok().map(Vec::len))
                .field("failed", &result.is_err())
                .finish(),
            Self::Filter {
                activation,
                job_revision,
                hit,
                catalogue,
                result,
            } => formatter
                .debug_struct("Filter")
                .field("activation", activation)
                .field("job_revision", job_revision)
                .field("hit", hit)
                .field("catalogue_count", &catalogue.len())
                .field(
                    "success_count",
                    &result.as_ref().ok().map(RankedFileSnapshot::count),
                )
                .field("failed", &result.is_err())
                .finish(),
        }
    }
}

impl FileSuggestionController {
    pub(super) fn new(source: WorkspaceFileCatalogue) -> Self {
        Self {
            source,
            lifecycle: Lifecycle::Dormant,
            active: None,
            requested: RequestedMenu::Hidden,
            presented: PresentedState::Absent,
            decoder_reset_required: false,
            selected_path: None,
            dismissed_revision: None,
            suppressed: false,
            next_activation: 1,
            next_job_revision: 1,
            next_menu_revision: 1,
            scan_starts: 0,
            filter_starts: 0,
        }
    }

    pub(super) fn snapshot(&self) -> FileSuggestionSnapshot<'_> {
        if self.suppressed {
            return FileSuggestionSnapshot::Hidden;
        }
        match &self.requested {
            RequestedMenu::Hidden => FileSuggestionSnapshot::Hidden,
            RequestedMenu::Loading { .. } => FileSuggestionSnapshot::Loading,
            RequestedMenu::Ready {
                ranked, selected, ..
            } => match selected {
                Some(selected) => FileSuggestionSnapshot::Ready {
                    candidates: ranked.candidates(),
                    selected: *selected,
                    capped: ranked.is_capped(),
                },
                None => FileSuggestionSnapshot::Empty,
            },
            RequestedMenu::Unavailable { .. } => FileSuggestionSnapshot::Unavailable,
        }
    }

    pub(super) fn stage_presentation(
        &mut self,
        show_in_focus: bool,
    ) -> Result<StagedFileSuggestionPresentation, FileSuggestionControllerError> {
        match self.try_stage_presentation(show_in_focus) {
            Err(FileSuggestionControllerError::Capacity) => {
                self.degrade_unavailable();
                self.try_stage_presentation(show_in_focus)
            }
            result => result,
        }
    }

    fn try_stage_presentation(
        &self,
        show_in_focus: bool,
    ) -> Result<StagedFileSuggestionPresentation, FileSuggestionControllerError> {
        if !show_in_focus || self.suppressed {
            return Ok(StagedFileSuggestionPresentation::Absent);
        }
        controller_allocation_gate()?;
        let presentation = match &self.requested {
            RequestedMenu::Hidden => return Ok(StagedFileSuggestionPresentation::Absent),
            RequestedMenu::Loading {
                activation,
                menu_revision,
                hit,
            } => FileSuggestionPresentation {
                activation: *activation,
                menu_revision: *menu_revision,
                hit: Some(hit.try_clone_bounded().map_err(map_file_error)?),
                menu: PresentedMenu::Loading,
            },
            RequestedMenu::Ready {
                activation,
                menu_revision,
                hit,
                ranked,
                selected,
            } => FileSuggestionPresentation {
                activation: *activation,
                menu_revision: *menu_revision,
                hit: Some(hit.try_clone_bounded().map_err(map_file_error)?),
                menu: match selected {
                    Some(selected) => PresentedMenu::Ready {
                        ranked: ranked.try_clone_bounded().map_err(map_file_error)?,
                        selected: *selected,
                    },
                    None => PresentedMenu::Empty,
                },
            },
            RequestedMenu::Unavailable {
                activation,
                menu_revision,
            } => FileSuggestionPresentation {
                activation: *activation,
                menu_revision: *menu_revision,
                hit: None,
                menu: PresentedMenu::Unavailable,
            },
        };
        Ok(StagedFileSuggestionPresentation::Valid(presentation))
    }

    pub(super) fn commit_presentation(&mut self, staged: StagedFileSuggestionPresentation) {
        self.presented = match staged {
            StagedFileSuggestionPresentation::Absent => PresentedState::Absent,
            StagedFileSuggestionPresentation::Valid(presentation) => {
                PresentedState::Valid(presentation)
            }
        };
    }

    pub(super) fn invalidate_presentation(&mut self) {
        self.presented = PresentedState::Invalidated;
        self.decoder_reset_required = true;
    }

    pub(super) const fn decoder_reset_required(&self) -> bool {
        self.decoder_reset_required
    }

    pub(super) fn mark_decoder_reset(&mut self) {
        self.decoder_reset_required = false;
    }

    pub(super) fn presented_is_invalidated(&self) -> bool {
        matches!(self.presented, PresentedState::Invalidated)
    }

    pub(super) fn presented_menu_is_visible(&self) -> bool {
        matches!(self.presented, PresentedState::Valid(_))
    }

    /// Returns true when a committed file menu owns this navigation key.
    pub(super) fn navigate_presented(
        &mut self,
        movement: FileSuggestionMove,
    ) -> Result<bool, FileSuggestionControllerError> {
        match self.try_navigate_presented(movement) {
            Err(FileSuggestionControllerError::Capacity) => {
                self.degrade_unavailable();
                Ok(true)
            }
            result => result,
        }
    }

    fn try_navigate_presented(
        &mut self,
        movement: FileSuggestionMove,
    ) -> Result<bool, FileSuggestionControllerError> {
        let PresentedState::Valid(presentation) = &self.presented else {
            return Ok(false);
        };
        let PresentedMenu::Ready { ranked, selected } = &presentation.menu else {
            return Ok(true);
        };
        let next = match movement {
            FileSuggestionMove::Previous => selected.saturating_sub(1),
            FileSuggestionMove::Next => selected.saturating_add(1).min(ranked.count() - 1),
        };
        let selected_path = ranked
            .candidate(next)
            .ok_or(FileSuggestionControllerError::State)
            .and_then(try_copy)?;
        let RequestedMenu::Ready {
            activation,
            menu_revision,
            hit,
            ranked: requested_ranked,
            selected: requested_selected,
        } = &mut self.requested
        else {
            return Ok(true);
        };
        if *activation != presentation.activation
            || *menu_revision != presentation.menu_revision
            || presentation.hit.as_ref() != Some(hit)
            || requested_ranked.candidate(next) != Some(selected_path.as_str())
        {
            return Ok(true);
        }
        *requested_selected = Some(next);
        self.selected_path = Some(selected_path);
        Ok(true)
    }

    pub(super) fn enter_presented(
        &mut self,
        input: &mut InputMemory,
    ) -> Result<FileSuggestionEnter, InputMemoryError> {
        let PresentedState::Valid(presentation) = &self.presented else {
            return Ok(FileSuggestionEnter::Ordinary);
        };
        let PresentedMenu::Ready { ranked, selected } = &presentation.menu else {
            return Ok(FileSuggestionEnter::Ordinary);
        };
        let Some(presentation_hit) = presentation.hit.as_ref() else {
            return Ok(FileSuggestionEnter::Ordinary);
        };
        let Some(path) = ranked.candidate(*selected) else {
            return Ok(FileSuggestionEnter::Consumed);
        };
        let requested_matches = matches!(
            &self.requested,
            RequestedMenu::Ready {
                activation,
                menu_revision,
                hit,
                ranked: requested_ranked,
                selected: Some(requested_selected),
            } if *activation == presentation.activation
                && *menu_revision == presentation.menu_revision
                && hit == presentation_hit
                && *requested_selected == *selected
                && requested_ranked.candidate(*selected) == Some(path)
        );
        if !requested_matches {
            return Ok(FileSuggestionEnter::Consumed);
        }
        if !input.complete_file_reference(presentation_hit, path)? {
            return Ok(FileSuggestionEnter::Consumed);
        }
        self.active = None;
        self.selected_path = None;
        self.requested = RequestedMenu::Hidden;
        self.cancel_current(None);
        Ok(FileSuggestionEnter::Completed)
    }

    pub(super) fn has_job(&self) -> bool {
        matches!(
            self.lifecycle,
            Lifecycle::Scanning(_) | Lifecycle::Filtering(_) | Lifecycle::Cancelling { .. }
        )
    }

    pub(super) async fn wait_job(&mut self) -> Result<JobSettlement, JoinError> {
        match &mut self.lifecycle {
            Lifecycle::Scanning(job)
            | Lifecycle::Filtering(job)
            | Lifecycle::Cancelling { job, .. } => (&mut job.handle).await,
            Lifecycle::Dormant | Lifecycle::Ready { .. } | Lifecycle::Failed { .. } => {
                std::future::pending().await
            }
        }
    }

    pub(super) fn sync(
        &mut self,
        composer: &Composer,
        detail_suppressed: bool,
        approval_suppressed: bool,
    ) -> Result<bool, FileSuggestionControllerError> {
        match self.try_sync(composer, detail_suppressed, approval_suppressed) {
            Err(FileSuggestionControllerError::Capacity) => Ok(self.degrade_unavailable()),
            result => result,
        }
    }

    fn try_sync(
        &mut self,
        composer: &Composer,
        detail_suppressed: bool,
        approval_suppressed: bool,
    ) -> Result<bool, FileSuggestionControllerError> {
        if approval_suppressed {
            return self.suppress_for_approval();
        }
        if self.suppressed {
            self.suppressed = false;
        }
        let detected = FileTokenHit::detect(composer).map_err(map_file_error)?;
        let Some(hit) = detected else {
            return self.close_active();
        };
        if self.dismissed_revision != Some(composer.content_revision()) {
            self.dismissed_revision = None;
        }
        if self.dismissed_revision == Some(composer.content_revision()) {
            return Ok(self.hide_requested());
        }

        let same_activation = self
            .active
            .as_ref()
            .is_some_and(|active| active.hit.start() == hit.start());
        let changed = self.active.as_ref().is_none_or(|active| active.hit != hit);
        if !same_activation {
            let activation = self.take_activation()?;
            let active = ActiveHit {
                activation,
                hit,
                draft_bytes: composer.byte_len(),
            };
            self.active = Some(try_clone_active(&active)?);
            self.request_loading(&active)?;
            self.replace_or_start_activation(active)?;
        } else if changed {
            let activation = self
                .active
                .as_ref()
                .map(|active| active.activation)
                .ok_or(FileSuggestionControllerError::State)?;
            let active = ActiveHit {
                activation,
                hit,
                draft_bytes: composer.byte_len(),
            };
            self.active = Some(try_clone_active(&active)?);
            self.refine_active(active)?;
        }
        Ok(!detail_suppressed && self.snapshot().is_visible())
    }

    pub(super) fn dismiss(
        &mut self,
        composer: &Composer,
    ) -> Result<bool, FileSuggestionControllerError> {
        if !self.snapshot().is_visible() {
            return Ok(false);
        }
        self.dismissed_revision = Some(composer.content_revision());
        self.cancel_current(None);
        self.active = None;
        self.selected_path = None;
        Ok(self.hide_requested())
    }

    pub(super) fn accept_job(
        &mut self,
        settlement: Result<JobSettlement, JoinError>,
    ) -> Result<bool, FileSuggestionControllerError> {
        match self.try_accept_job(settlement) {
            Err(FileSuggestionControllerError::Capacity) => Ok(self.degrade_unavailable()),
            result => result,
        }
    }

    fn try_accept_job(
        &mut self,
        settlement: Result<JobSettlement, JoinError>,
    ) -> Result<bool, FileSuggestionControllerError> {
        let lifecycle = mem::replace(&mut self.lifecycle, Lifecycle::Dormant);
        let (job, pending, cancelled_without_replacement) = match lifecycle {
            Lifecycle::Scanning(job) | Lifecycle::Filtering(job) => (job, None, false),
            Lifecycle::Cancelling { job, pending } => {
                let without_replacement = pending.is_none();
                (job, pending, without_replacement)
            }
            other => {
                self.lifecycle = other;
                return Err(FileSuggestionControllerError::State);
            }
        };
        let settlement = match settlement {
            Ok(settlement) => settlement,
            Err(_) => return self.recover_join_failure(job, pending),
        };
        if settlement_identity(&settlement) != (job.activation, job.job_revision, job.kind) {
            return Err(FileSuggestionControllerError::State);
        }
        if cancelled_without_replacement {
            self.lifecycle = self
                .active
                .as_ref()
                .filter(|active| active.activation == job.activation)
                .map_or(Lifecycle::Dormant, |active| Lifecycle::Failed {
                    activation: active.activation,
                });
            return Ok(true);
        }
        match (settlement, pending) {
            (
                JobSettlement::Scan {
                    activation, result, ..
                },
                pending,
            ) => self.accept_scan(activation, result, pending),
            (
                JobSettlement::Filter {
                    activation,
                    hit,
                    catalogue,
                    result,
                    ..
                },
                pending,
            ) => self.accept_filter(activation, hit, catalogue, result, pending),
        }
    }

    pub(super) fn cancel_for_shutdown(&mut self) {
        self.active = None;
        self.requested = RequestedMenu::Hidden;
        self.selected_path = None;
        self.cancel_current(None);
    }

    pub(super) async fn finish_shutdown(&mut self) -> Result<(), FileSuggestionControllerError> {
        self.cancel_for_shutdown();
        if self.has_job() {
            let settlement = self.wait_job().await;
            let _ = self.accept_job(settlement)?;
        }
        if self.has_job() {
            return Err(FileSuggestionControllerError::State);
        }
        Ok(())
    }

    fn suppress_for_approval(&mut self) -> Result<bool, FileSuggestionControllerError> {
        let changed = !self.suppressed || !matches!(self.requested, RequestedMenu::Hidden);
        self.suppressed = true;
        self.active = None;
        self.requested = RequestedMenu::Hidden;
        self.invalidate_presentation();
        self.cancel_current(None);
        Ok(changed)
    }

    fn close_active(&mut self) -> Result<bool, FileSuggestionControllerError> {
        let changed =
            self.active.take().is_some() || !matches!(self.requested, RequestedMenu::Hidden);
        self.cancel_current(None);
        self.selected_path = None;
        self.requested = RequestedMenu::Hidden;
        Ok(changed)
    }

    fn hide_requested(&mut self) -> bool {
        if matches!(self.requested, RequestedMenu::Hidden) {
            false
        } else {
            self.requested = RequestedMenu::Hidden;
            true
        }
    }

    fn replace_or_start_activation(
        &mut self,
        active: ActiveHit,
    ) -> Result<(), FileSuggestionControllerError> {
        match mem::replace(&mut self.lifecycle, Lifecycle::Dormant) {
            Lifecycle::Dormant | Lifecycle::Ready { .. } | Lifecycle::Failed { .. } => {
                self.start_scan(active)
            }
            Lifecycle::Scanning(job) | Lifecycle::Filtering(job) => {
                job.cancellation.cancel();
                self.lifecycle = Lifecycle::Cancelling {
                    job,
                    pending: Some(active),
                };
                Ok(())
            }
            Lifecycle::Cancelling { job, .. } => {
                self.lifecycle = Lifecycle::Cancelling {
                    job,
                    pending: Some(active),
                };
                Ok(())
            }
        }
    }

    fn refine_active(&mut self, active: ActiveHit) -> Result<(), FileSuggestionControllerError> {
        self.request_loading(&active)?;
        match mem::replace(&mut self.lifecycle, Lifecycle::Dormant) {
            Lifecycle::Scanning(job) => {
                self.lifecycle = Lifecycle::Scanning(job);
                Ok(())
            }
            Lifecycle::Filtering(job) => {
                job.cancellation.cancel();
                self.lifecycle = Lifecycle::Cancelling {
                    job,
                    pending: Some(active),
                };
                Ok(())
            }
            Lifecycle::Cancelling { job, .. } => {
                self.lifecycle = Lifecycle::Cancelling {
                    job,
                    pending: Some(active),
                };
                Ok(())
            }
            Lifecycle::Ready {
                activation,
                catalogue,
            } if activation == active.activation => self.start_filter(active, catalogue),
            Lifecycle::Failed { activation } if activation == active.activation => {
                self.lifecycle = Lifecycle::Failed { activation };
                self.request_unavailable()?;
                Ok(())
            }
            other => {
                self.lifecycle = other;
                Err(FileSuggestionControllerError::State)
            }
        }
    }

    fn start_scan(&mut self, active: ActiveHit) -> Result<(), FileSuggestionControllerError> {
        self.scan_starts = self
            .scan_starts
            .checked_add(1)
            .ok_or(FileSuggestionControllerError::Capacity)?;
        let job_revision = self.take_job_revision()?;
        let activation = active.activation;
        let source = self.source.clone();
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let handle = tokio::task::spawn_blocking(move || JobSettlement::Scan {
            activation,
            job_revision,
            result: source.scan_blocking(&task_token),
        });
        self.lifecycle = Lifecycle::Scanning(RunningJob {
            activation,
            job_revision,
            kind: BlockingKind::Scan,
            cancellation,
            handle,
        });
        Ok(())
    }

    fn start_filter(
        &mut self,
        active: ActiveHit,
        catalogue: Vec<String>,
    ) -> Result<(), FileSuggestionControllerError> {
        self.filter_starts = self
            .filter_starts
            .checked_add(1)
            .ok_or(FileSuggestionControllerError::Capacity)?;
        let job_revision = self.take_job_revision()?;
        let activation = active.activation;
        let hit = active.hit.try_clone_bounded().map_err(map_file_error)?;
        let draft_bytes = active.draft_bytes;
        let cancellation = CancellationToken::new();
        let task_token = cancellation.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let result = rank_catalogue(&catalogue, &hit, draft_bytes, &task_token);
            JobSettlement::Filter {
                activation,
                job_revision,
                hit,
                catalogue,
                result,
            }
        });
        self.lifecycle = Lifecycle::Filtering(RunningJob {
            activation,
            job_revision,
            kind: BlockingKind::Filter,
            cancellation,
            handle,
        });
        Ok(())
    }

    fn cancel_current(&mut self, pending: Option<ActiveHit>) {
        let lifecycle = mem::replace(&mut self.lifecycle, Lifecycle::Dormant);
        self.lifecycle = match lifecycle {
            Lifecycle::Scanning(job) | Lifecycle::Filtering(job) => {
                job.cancellation.cancel();
                Lifecycle::Cancelling { job, pending }
            }
            Lifecycle::Cancelling { job, .. } => Lifecycle::Cancelling { job, pending },
            Lifecycle::Dormant | Lifecycle::Ready { .. } | Lifecycle::Failed { .. } => {
                Lifecycle::Dormant
            }
        };
    }

    fn accept_scan(
        &mut self,
        activation: u64,
        result: Result<Vec<String>, WorkspaceFileCatalogueError>,
        pending: Option<ActiveHit>,
    ) -> Result<bool, FileSuggestionControllerError> {
        if let Some(pending) = pending {
            self.start_scan(pending)?;
            return Ok(true);
        }
        let Some(active) = self.active.as_ref() else {
            return Ok(false);
        };
        if self.suppressed || active.activation != activation {
            return Ok(false);
        }
        match result {
            Ok(catalogue) => self.start_filter(try_clone_active(active)?, catalogue)?,
            Err(_) => {
                self.lifecycle = Lifecycle::Failed { activation };
                self.request_unavailable()?;
            }
        }
        Ok(true)
    }

    fn accept_filter(
        &mut self,
        activation: u64,
        hit: FileTokenHit,
        catalogue: Vec<String>,
        result: Result<RankedFileSnapshot, FileSuggestionError>,
        pending: Option<ActiveHit>,
    ) -> Result<bool, FileSuggestionControllerError> {
        if let Some(pending) = pending {
            if !self.suppressed && pending.activation == activation {
                self.start_filter(pending, catalogue)?;
            } else if !self.suppressed {
                self.start_scan(pending)?;
            }
            return Ok(true);
        }
        let Some(active) = self.active.as_ref() else {
            return Ok(false);
        };
        if self.suppressed || active.activation != activation {
            return Ok(false);
        }
        if active.hit != hit {
            self.start_filter(try_clone_active(active)?, catalogue)?;
            return Ok(true);
        }
        match result {
            Ok(ranked) => {
                let selected = ranked.selected_index(self.selected_path.as_deref());
                self.selected_path = selected
                    .and_then(|index| ranked.candidate(index))
                    .map(try_copy)
                    .transpose()?;
                self.request_ready(activation, hit, ranked, selected)?;
                self.lifecycle = Lifecycle::Ready {
                    activation,
                    catalogue,
                };
            }
            Err(FileSuggestionError::Cancelled) => {
                self.start_filter(try_clone_active(active)?, catalogue)?;
            }
            Err(_) => {
                self.lifecycle = Lifecycle::Failed { activation };
                self.request_unavailable()?;
            }
        }
        Ok(true)
    }

    fn recover_join_failure(
        &mut self,
        job: RunningJob,
        pending: Option<ActiveHit>,
    ) -> Result<bool, FileSuggestionControllerError> {
        if let Some(pending) = pending {
            if job.kind == BlockingKind::Filter && pending.activation == job.activation {
                self.active = Some(try_clone_active(&pending)?);
                self.lifecycle = Lifecycle::Failed {
                    activation: job.activation,
                };
                self.request_unavailable()?;
            } else {
                self.start_scan(pending)?;
            }
        } else if self
            .active
            .as_ref()
            .is_some_and(|active| active.activation == job.activation)
        {
            self.lifecycle = Lifecycle::Failed {
                activation: job.activation,
            };
            self.request_unavailable()?;
        }
        Ok(true)
    }

    fn request_loading(&mut self, active: &ActiveHit) -> Result<(), FileSuggestionControllerError> {
        let menu_revision = self.take_menu_revision()?;
        self.requested = RequestedMenu::Loading {
            activation: active.activation,
            menu_revision,
            hit: active.hit.try_clone_bounded().map_err(map_file_error)?,
        };
        Ok(())
    }

    fn request_ready(
        &mut self,
        activation: u64,
        hit: FileTokenHit,
        ranked: RankedFileSnapshot,
        selected: Option<usize>,
    ) -> Result<(), FileSuggestionControllerError> {
        let menu_revision = self.take_menu_revision()?;
        self.requested = RequestedMenu::Ready {
            activation,
            menu_revision,
            hit,
            ranked,
            selected,
        };
        Ok(())
    }

    fn request_unavailable(&mut self) -> Result<(), FileSuggestionControllerError> {
        let active = self
            .active
            .as_ref()
            .ok_or(FileSuggestionControllerError::State)?;
        let activation = active.activation;
        let menu_revision = self.take_menu_revision()?;
        self.requested = RequestedMenu::Unavailable {
            activation,
            menu_revision,
        };
        Ok(())
    }

    /// Converts a recoverable resource failure into a local, non-allocating
    /// status. The Session and ordinary composer remain usable.
    fn degrade_unavailable(&mut self) -> bool {
        self.selected_path = None;
        self.cancel_current(None);
        let Some(activation) = self.active.as_ref().map(|active| active.activation) else {
            return self.hide_requested();
        };
        if !matches!(self.lifecycle, Lifecycle::Cancelling { .. }) {
            self.lifecycle = Lifecycle::Failed { activation };
        }
        if let Ok(menu_revision) = self.take_menu_revision() {
            self.requested = RequestedMenu::Unavailable {
                activation,
                menu_revision,
            };
            true
        } else {
            self.hide_requested()
        }
    }

    fn take_activation(&mut self) -> Result<u64, FileSuggestionControllerError> {
        take_counter(&mut self.next_activation)
    }

    fn take_job_revision(&mut self) -> Result<u64, FileSuggestionControllerError> {
        take_counter(&mut self.next_job_revision)
    }

    fn take_menu_revision(&mut self) -> Result<u64, FileSuggestionControllerError> {
        take_counter(&mut self.next_menu_revision)
    }
}

fn settlement_identity(settlement: &JobSettlement) -> (u64, u64, BlockingKind) {
    match settlement {
        JobSettlement::Scan {
            activation,
            job_revision,
            ..
        } => (*activation, *job_revision, BlockingKind::Scan),
        JobSettlement::Filter {
            activation,
            job_revision,
            ..
        } => (*activation, *job_revision, BlockingKind::Filter),
    }
}

fn try_clone_active(active: &ActiveHit) -> Result<ActiveHit, FileSuggestionControllerError> {
    controller_allocation_gate()?;
    Ok(ActiveHit {
        activation: active.activation,
        hit: active.hit.try_clone_bounded().map_err(map_file_error)?,
        draft_bytes: active.draft_bytes,
    })
}

fn try_copy(value: &str) -> Result<String, FileSuggestionControllerError> {
    controller_allocation_gate()?;
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|_| FileSuggestionControllerError::Capacity)?;
    output.push_str(value);
    Ok(output)
}

#[cfg(not(test))]
fn controller_allocation_gate() -> Result<(), FileSuggestionControllerError> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static CONTROLLER_ALLOCATION_FAILURE: std::cell::Cell<bool> = const {
        std::cell::Cell::new(false)
    };
}

#[cfg(test)]
fn controller_allocation_gate() -> Result<(), FileSuggestionControllerError> {
    CONTROLLER_ALLOCATION_FAILURE.with(|fail| {
        if fail.replace(false) {
            Err(FileSuggestionControllerError::Capacity)
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
fn fail_next_controller_allocation() {
    CONTROLLER_ALLOCATION_FAILURE.with(|fail| fail.set(true));
}

fn take_counter(counter: &mut u64) -> Result<u64, FileSuggestionControllerError> {
    let value = *counter;
    *counter = counter
        .checked_add(1)
        .ok_or(FileSuggestionControllerError::Capacity)?;
    Ok(value)
}

fn map_file_error(_error: FileSuggestionError) -> FileSuggestionControllerError {
    FileSuggestionControllerError::Capacity
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};
    use tokio_util::sync::CancellationToken;

    use crate::{
        tui::{
            file_suggestions::{FileSuggestionSnapshot, FileTokenHit},
            input_memory::InputMemory,
        },
        workspace_authority::WorkspaceAuthority,
    };

    use super::{
        ActiveHit, BlockingKind, FileSuggestionController, FileSuggestionEnter, FileSuggestionMove,
        Lifecycle, RunningJob, StagedFileSuggestionPresentation, WorkspaceFileCatalogue,
        fail_next_controller_allocation,
    };

    struct TempWorkspace(std::path::PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("dsh-file-suggestions-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn controller(workspace: &TempWorkspace) -> FileSuggestionController {
        let authority = WorkspaceAuthority::open(workspace.path()).unwrap();
        FileSuggestionController::new(WorkspaceFileCatalogue::from_authority(authority))
    }

    async fn settle_one(controller: &mut FileSuggestionController) {
        assert!(controller.has_job());
        let settlement = controller.wait_job().await;
        controller.accept_job(settlement).unwrap();
    }

    async fn settle_all(controller: &mut FileSuggestionController) {
        for _ in 0..4 {
            if !controller.has_job() {
                return;
            }
            settle_one(controller).await;
        }
        panic!("file suggestion jobs did not converge");
    }

    #[tokio::test]
    async fn scan_filter_present_and_complete_are_owned_and_redacted() {
        let workspace = TempWorkspace::new();
        fs::create_dir(workspace.path().join("src")).unwrap();
        fs::write(workspace.path().join("src/lib.rs"), "fn main() {}\n").unwrap();
        fs::write(workspace.path().join("space name.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("review @src").unwrap();

        assert!(controller.sync(input.composer(), false, false).unwrap());
        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Loading);
        let loading = controller.stage_presentation(true).unwrap();
        controller.commit_presentation(loading);
        settle_all(&mut controller).await;
        let FileSuggestionSnapshot::Ready {
            candidates,
            selected,
            capped,
        } = controller.snapshot()
        else {
            panic!("expected ready file suggestions");
        };
        assert_eq!(candidates, &["src/lib.rs"]);
        assert_eq!(selected, 0);
        assert!(!capped);
        assert_eq!(controller.scan_starts, 1);
        assert_eq!(controller.filter_starts, 1);

        let staged = controller.stage_presentation(true).unwrap();
        assert!(matches!(staged, StagedFileSuggestionPresentation::Valid(_)));
        controller.commit_presentation(staged);
        assert!(
            controller
                .navigate_presented(FileSuggestionMove::Next)
                .unwrap()
        );
        fs::rename(
            workspace.path().join("src/lib.rs"),
            workspace.path().join("src/renamed.rs"),
        )
        .unwrap();
        assert_eq!(
            controller.enter_presented(&mut input).unwrap(),
            FileSuggestionEnter::Completed
        );
        assert_eq!(input.composer().text(), "review @src/lib.rs ");
        assert_eq!(input.queue().len(), 0);
        assert!(!format!("{controller:?}").contains("src/lib.rs"));
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn refinement_reuses_catalogue_but_new_activation_rescans() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("src.rs"), "safe\n").unwrap();
        fs::write(workspace.path().join("sibling.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@s").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        input.insert_char('r').unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        assert_eq!(controller.scan_starts, 1);

        settle_one(&mut controller).await;
        assert_eq!(controller.filter_starts, 1);
        input.backspace().unwrap();
        input.insert_char('i').unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        settle_one(&mut controller).await;
        assert_eq!(controller.scan_starts, 1);
        assert_eq!(controller.filter_starts, 2);
        settle_one(&mut controller).await;
        assert!(matches!(
            controller.snapshot(),
            FileSuggestionSnapshot::Ready { .. } | FileSuggestionSnapshot::Empty
        ));

        input.insert_char(' ').unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        input.insert_text("@s").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        assert_eq!(controller.scan_starts, 2);
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn approval_takeover_invalidates_and_requires_a_fresh_activation() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("safe.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@safe").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        assert_eq!(controller.scan_starts, 1);

        controller.sync(input.composer(), false, true).unwrap();
        assert!(controller.presented_is_invalidated());
        settle_all(&mut controller).await;
        controller.sync(input.composer(), false, false).unwrap();
        assert_eq!(controller.scan_starts, 2);
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_filter_join_panic_loses_the_catalogue_and_does_not_rescan() {
        let workspace = TempWorkspace::new();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        input.insert_char('s').unwrap();
        let pending = ActiveHit {
            activation: 7,
            hit: FileTokenHit::detect(input.composer()).unwrap().unwrap(),
            draft_bytes: input.composer().byte_len(),
        };
        controller.active = Some(super::try_clone_active(&pending).unwrap());
        controller.request_loading(&pending).unwrap();
        let cancellation = CancellationToken::new();
        controller.lifecycle = Lifecycle::Cancelling {
            job: RunningJob {
                activation: 7,
                job_revision: 9,
                kind: BlockingKind::Filter,
                cancellation,
                handle: tokio::task::spawn_blocking(|| -> super::JobSettlement {
                    panic!("SECRET_FILTER_PANIC")
                }),
            },
            pending: Some(pending),
        };

        let settlement = controller.wait_job().await;
        controller.accept_job(settlement).unwrap();
        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        assert_eq!(controller.scan_starts, 0);
        assert!(!format!("{controller:?}").contains("SECRET_FILTER_PANIC"));
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn ranking_commit_allocation_failure_degrades_without_a_stuck_job() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("safe.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        settle_one(&mut controller).await;

        let settlement = controller.wait_job().await;
        fail_next_controller_allocation();
        controller.accept_job(settlement).unwrap();

        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        assert!(!controller.has_job());
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn staged_roster_allocation_failure_degrades_to_unavailable() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("safe.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        settle_all(&mut controller).await;
        assert!(matches!(
            controller.snapshot(),
            FileSuggestionSnapshot::Ready { .. }
        ));

        fail_next_controller_allocation();
        let staged = controller.stage_presentation(true).unwrap();

        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        assert!(matches!(
            &staged,
            StagedFileSuggestionPresentation::Valid(presentation)
                if matches!(presentation.menu, super::PresentedMenu::Unavailable)
        ));
        controller.commit_presentation(staged);
        assert!(controller.presented_menu_is_visible());
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn sync_allocation_failure_cancels_and_joins_the_running_scan() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("safe.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@s").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        assert!(controller.has_job());

        input.insert_char('a').unwrap();
        fail_next_controller_allocation();
        controller.sync(input.composer(), false, false).unwrap();

        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        assert!(controller.has_job());
        controller.finish_shutdown().await.unwrap();
        assert!(!controller.has_job());
    }

    #[tokio::test]
    async fn sync_allocation_failure_does_not_restart_a_cancelled_filter() {
        let workspace = TempWorkspace::new();
        fs::write(workspace.path().join("safe.rs"), "safe\n").unwrap();
        let mut controller = controller(&workspace);
        let mut input = InputMemory::default();
        input.insert_text("@s").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        settle_one(&mut controller).await;
        assert_eq!(controller.filter_starts, 1);
        assert!(controller.has_job());

        input.insert_char('a').unwrap();
        fail_next_controller_allocation();
        controller.sync(input.composer(), false, false).unwrap();
        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        assert!(controller.has_job());

        settle_one(&mut controller).await;
        assert_eq!(controller.filter_starts, 1);
        assert!(!controller.has_job());
        assert_eq!(controller.snapshot(), FileSuggestionSnapshot::Unavailable);
        controller.finish_shutdown().await.unwrap();
    }
}
