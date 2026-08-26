use std::time::Duration;
use std::{mem, ops::ControlFlow};

use futures_util::FutureExt as _;
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::AgentLoop,
    session::{
        ApprovalOutcome, CommittedUiKind, CommittedUiReceiver, EventSeq, StoreError, TurnEndReason,
        TurnId, UiUserSource,
    },
    tui::{
        command_palette::{
            CommandId, CommandPaletteSnapshot, CommandPaletteState, PaletteEnter, PaletteMove,
        },
        dock::{
            DetailViewport, DockApprovalSelection, DockError, DockFrame, DockInteraction,
            DockModel, MIN_ENHANCED_COLUMNS, MIN_ENHANCED_ROWS,
        },
        file_suggestions::FileSuggestionSnapshot,
        inline_screen::{
            InlineScreen, InlineScreenError, POISON_REATTACH_BYTES, POISON_TEARDOWN_BYTES,
            PendingScreenWrite, ScreenSize,
        },
        input_memory::{InputMemory, InputMemoryError, LocalPromptId},
        key_decoder::{InputEvent, Key, KeyDecoder},
        theme::{ThemeCommand, ThemePalette, ThemeRequest, ThemeState},
        view::{ContextEstimate, ViewMode, ViewRequest, ViewState},
    },
};

use super::{
    approval::{ApprovalEnvelope, ApprovalEnvelopeReceiver},
    approval_join::{ApprovalJoin, ApprovalJoinError, ApprovalResetMode},
    approval_selector::{
        ApprovalInputProfile, ApprovalSelector, ESCAPE_SEQUENCE_WAIT, SelectorUpdate,
    },
    assembly::InteractiveAssembly,
    file_suggestions::{
        FileSuggestionController, FileSuggestionEnter, FileSuggestionMove, JobSettlement,
        StagedFileSuggestionPresentation,
    },
    identity::prepare_user_turn,
    input::{
        CanonicalRecordParser, IdleInput, InputRecordEvent, MAX_APPROVAL_RECORD_BYTES,
        MAX_INTERACTIVE_PROMPT_BYTES, classify_idle_record,
    },
    live::{
        EnhancedPresenter, InteractivePresenter, LiveFrame, LiveLifecycle, LiveRenderer,
        PendingLiveFrame, PreparedPresentation,
    },
    shutdown,
    signal::{DriverMode, InteractiveSignal, SignalLatch, SignalStreams, UiSignal, self_suspend},
    storage_failure,
    terminal::{
        ApprovalTerminalMode, AsyncTerminal, ENHANCED_VISUAL_RESET_BYTES, TERMINAL_READ_BYTES,
        TerminalError, TerminalPanicRestore, TerminalSession, TerminalSize,
    },
};

const FRAME_DEADLINE: Duration = Duration::from_secs(5);
const VISUAL_RESET_DEADLINE: Duration = Duration::from_millis(250);
const APPROVAL_INPUT_QUIET: Duration = Duration::from_millis(100);
const PASTE_INPUT_QUIET: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum InteractiveError {
    #[error("CLI_TERMINAL_UNAVAILABLE")]
    TerminalUnavailable,
    #[error("CLI_TERMINAL_UNSUPPORTED")]
    TerminalUnsupported,
    #[error("CLI_AGENT_UNAVAILABLE")]
    Agent,
    #[error(transparent)]
    Storage(StoreError),
    #[error("CLI_OUTPUT_FAILED")]
    Output,
}

impl From<TerminalError> for InteractiveError {
    fn from(value: TerminalError) -> Self {
        match value {
            TerminalError::Unavailable => Self::TerminalUnavailable,
            TerminalError::Unsupported => Self::TerminalUnsupported,
        }
    }
}

impl From<ApprovalJoinError> for InteractiveError {
    fn from(_value: ApprovalJoinError) -> Self {
        Self::Agent
    }
}

impl From<InputMemoryError> for InteractiveError {
    fn from(_: InputMemoryError) -> Self {
        Self::Agent
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopIntent {
    Interrupt,
    Eof,
    Suspend,
    Exit(UiSignal),
    Failure(InteractiveError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AfterFrame {
    #[default]
    None,
    ApprovalFence,
    ApprovalAccepting,
    TurnEnd,
}

// Inline transactions deliberately retain their exact bounded presentation
// credential until the screen commit; keeping it in-place avoids a second
// fallible allocation on every redraw.
#[allow(clippy::large_enum_variant)]
enum PendingOutput {
    Unprepared(LiveFrame),
    Prepared(PreparedPresentation),
    Linear(PendingLiveFrame),
    Dock(DockInteraction),
    Inline(PendingInlineOutput),
}

enum InlineIntent {
    Transcript(PreparedPresentation),
    Dock(DockInteraction),
}

struct PendingInlineOutput {
    write: PendingScreenWrite,
    intent: InlineIntent,
    surface: SurfaceCommit,
    file_suggestions: StagedFileSuggestionPresentation,
}

#[derive(Clone, Copy, Debug)]
struct SurfaceCommit {
    request: ViewRequest,
    theme: ThemeRequest,
    offset: usize,
    total_rows: usize,
    page_rows: usize,
}

struct EnhancedSurface {
    frame: DockFrame,
    commit: SurfaceCommit,
}

impl PendingOutput {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) => &[],
            Self::Linear(frame) => frame.bytes(),
            Self::Inline(output) => output.write.bytes(),
        }
    }

    fn advance(&mut self, count: usize) -> Result<(), InteractiveError> {
        match self {
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) => Err(InteractiveError::Agent),
            Self::Linear(frame) => frame.advance(count).map_err(|_| InteractiveError::Output),
            Self::Inline(output) => output.write.advance(count).map_err(map_inline_screen_error),
        }
    }

    fn has_started(&self) -> bool {
        match self {
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) => false,
            Self::Linear(_) => false,
            Self::Inline(output) => output.write.has_started(),
        }
    }
}

impl InlineIntent {
    fn into_pending(self) -> PendingOutput {
        match self {
            Self::Transcript(presentation) => PendingOutput::Prepared(presentation),
            Self::Dock(interaction) => PendingOutput::Dock(interaction),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnDisposition {
    Continue,
    Exit(u8),
    Signal(UiSignal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InteractiveExit {
    Ordinary(u8),
    Signal(UiSignal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InteractivePresentation {
    Auto,
    Enhanced,
    Linear,
}

pub(super) async fn run(
    assembly: InteractiveAssembly,
    terminal: AsyncTerminal,
    signals: &mut SignalStreams,
    presentation: InteractivePresentation,
) -> Result<u8, InteractiveError> {
    let enhanced = presentation_uses_enhanced(presentation, terminal.size());
    if enhanced {
        run_enhanced(assembly, terminal, signals).await
    } else {
        run_linear(assembly, terminal, signals, false).await
    }
}

fn presentation_uses_enhanced(
    presentation: InteractivePresentation,
    size: Option<TerminalSize>,
) -> bool {
    !matches!(presentation, InteractivePresentation::Linear)
        && size.is_some_and(|size| {
            size.columns >= MIN_ENHANCED_COLUMNS && size.rows >= MIN_ENHANCED_ROWS
        })
}

async fn run_enhanced(
    assembly: InteractiveAssembly,
    terminal: AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<u8, InteractiveError> {
    let InteractiveAssembly {
        mut agent,
        mut events,
        mut approvals,
        mut joins,
        session_id,
        resumed,
        file_suggestions,
    } = assembly;
    let mut file_suggestions = FileSuggestionController::new(file_suggestions);
    let mut live = LiveRenderer::for_session(resumed);
    live.set_context_estimate(session_context_estimate(agent.session(), None, None));
    let mut presenter = InteractivePresenter::with_color(true);
    let mut enhanced_presenter = EnhancedPresenter::new();
    let mut parser = CanonicalRecordParser::new(MAX_INTERACTIVE_PROMPT_BYTES);
    let mut scratch = [0_u8; TERMINAL_READ_BYTES];

    let banner = match LiveFrame::startup_banner(&session_id, resumed) {
        Ok(banner) => banner,
        Err(_) => {
            return shutdown_after_enhanced_error(&mut agent, signals, InteractiveError::Output)
                .await;
        }
    };
    let banner_signal = match write_frame(banner, &mut presenter, &terminal, signals).await {
        Ok(signal) => signal,
        Err(error) => return shutdown_after_enhanced_error(&mut agent, signals, error).await,
    };
    let banner_exit = match banner_signal {
        Some(signal) => match handle_idle_signal(signal, &terminal, signals).await {
            Ok(exit) => exit,
            Err(error) => {
                return shutdown_after_enhanced_error(&mut agent, signals, error).await;
            }
        },
        None => None,
    };
    if let Some(signal) = banner_exit {
        let mut agent_result = Ok(());
        let (shutdown, observed) = shutdown::agent_with_signals(
            &mut agent,
            DriverMode::Interactive,
            signals,
            Some(signal),
        )
        .await;
        if let Err(error) = shutdown {
            agent_result = Err(error);
        }
        let signal = observed.unwrap_or(signal);
        return match agent_result {
            Err(error) => match error.session_error() {
                Some(error) => Err(InteractiveError::Storage(storage_failure::from_shutdown(
                    error,
                ))),
                None => Err(InteractiveError::Agent),
            },
            Ok(()) => signal.exit_code().ok_or(InteractiveError::Agent),
        };
    }

    let mut last_size = match terminal.size() {
        Some(size) => size,
        None => {
            return shutdown_after_enhanced_error(
                &mut agent,
                signals,
                InteractiveError::TerminalUnsupported,
            )
            .await;
        }
    };
    let mut terminal = match terminal.into_application_session() {
        Ok(terminal) => terminal,
        Err(error) => {
            return shutdown_after_enhanced_error(&mut agent, signals, error.into()).await;
        }
    };
    let mut decoder = KeyDecoder::default();
    if decoder.reset_epoch().is_err() {
        let _ = terminal.finish();
        return shutdown_after_enhanced_error(&mut agent, signals, InteractiveError::Agent).await;
    }
    let mut input = InputMemory::default();
    let mut command_palette = CommandPaletteState::default();
    let mut view = ViewState::default();
    let mut theme = ThemeState::default();
    let mut notice = None;
    let mut screen = InlineScreen::default();
    let _ = file_suggestions
        .sync(input.composer(), false, false)
        .map_err(|_| InteractiveError::Agent)?;
    let initial_dock = render_enhanced_dock(
        DockRenderModel {
            input: &input,
            notice: notice.as_deref(),
            interaction: DockInteraction::Idle,
            command_palette: command_palette.snapshot(input.composer()),
            live: &live,
        },
        &terminal,
        &mut last_size,
        signals,
        &mut screen,
        &mut view,
        &mut theme,
        &mut file_suggestions,
    )
    .await;
    let mut pending_signal = match initial_dock {
        Ok(signal) => signal,
        Err(error) => {
            terminal.best_effort_visual_reset();
            let _ = terminal.finish();
            return shutdown_after_enhanced_error(&mut agent, signals, error).await;
        }
    };
    let mut auto_queue_paused = false;
    let mut input_escape_deadline = None;

    let result = std::panic::AssertUnwindSafe(async {
        loop {
            reset_file_suggestion_decoder(
                &mut file_suggestions,
                Some(&mut decoder),
                &mut input_escape_deadline,
            )?;
            let event = if let Some(signal) = pending_signal.take() {
                EnhancedIdleEvent::Signal(signal)
            } else if input.queue().len() != 0 && !auto_queue_paused {
                EnhancedIdleEvent::AutoSubmit
            } else {
                terminal.revalidate_application()?;
                let escape_pending = input_escape_deadline.is_some();
                let escape_deadline = input_escape_deadline.unwrap_or_else(Instant::now);
                tokio::select! {
                    biased;
                    signal = signals.next_interactive() => match signal {
                        InteractiveSignal::Stop(signal) => EnhancedIdleEvent::Signal(signal),
                        InteractiveSignal::Resize => EnhancedIdleEvent::Resize,
                    },
                    () = tokio::time::sleep_until(escape_deadline), if escape_pending => {
                        EnhancedIdleEvent::EscapeExpired
                    }
                    settlement = file_suggestions.wait_job(), if file_suggestions.has_job() => {
                        EnhancedIdleEvent::FileSuggestion(settlement)
                    }
                    read = terminal.read_once(&mut scratch) => {
                        let count = read.map_err(|_| InteractiveError::TerminalUnavailable)?;
                        if count == 0 {
                            EnhancedIdleEvent::Eof
                        } else {
                            EnhancedIdleEvent::Bytes(count)
                        }
                    }
                }
            };
            let auto_submit = matches!(&event, EnhancedIdleEvent::AutoSubmit);
            let action = match event {
                EnhancedIdleEvent::Signal(UiSignal::Interrupt) => {
                    decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                    input_escape_deadline = None;
                    if input.composer().is_empty() {
                        break Ok(InteractiveExit::Ordinary(0));
                    }
                    let _ = input.take_draft_for_turn()?;
                    notice = Some("Draft cleared · Ctrl+C again to exit".to_owned());
                    EnhancedInputAction::Redraw
                }
                EnhancedIdleEvent::Signal(UiSignal::Suspend) => {
                    input_escape_deadline = None;
                    let mut dock = ActiveDock {
                        screen: &mut screen,
                        last_size: &mut last_size,
                        view: &mut view,
                        theme: &mut theme,
                        command_palette: &mut command_palette,
                        file_suggestions: &mut file_suggestions,
                        palette_suppressed: false,
                    };
                    pending_signal = suspend_enhanced(
                        &mut terminal,
                        signals,
                        &mut decoder,
                        DockRenderModel {
                            input: &input,
                            notice: notice.as_deref(),
                            interaction: DockInteraction::Idle,
                            command_palette: dock.command_palette.snapshot(input.composer()),
                            live: &live,
                        },
                        &mut dock,
                    )
                    .await?;
                    continue;
                }
                EnhancedIdleEvent::Signal(
                    signal @ (UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate),
                ) => break Ok(InteractiveExit::Signal(signal)),
                EnhancedIdleEvent::Eof => break Ok(InteractiveExit::Ordinary(0)),
                EnhancedIdleEvent::Resize => EnhancedInputAction::Redraw,
                EnhancedIdleEvent::FileSuggestion(settlement) => {
                    let _ = file_suggestions
                        .accept_job(settlement)
                        .map_err(|_| InteractiveError::Agent)?;
                    EnhancedInputAction::Redraw
                }
                EnhancedIdleEvent::EscapeExpired => {
                    input_escape_deadline = None;
                    expire_enhanced_escape(
                        &mut decoder,
                        &mut input,
                        &mut command_palette,
                        Some(&mut file_suggestions),
                        &mut view,
                        &theme,
                        last_size,
                        &mut notice,
                    )?
                }
                EnhancedIdleEvent::AutoSubmit => EnhancedInputAction::Submit,
                EnhancedIdleEvent::Bytes(count) => {
                    auto_queue_paused = false;
                    let action = apply_enhanced_input(
                        &mut decoder,
                        &scratch[..count],
                        &mut input,
                        &mut command_palette,
                        Some(&mut file_suggestions),
                        &mut view,
                        &theme,
                        last_size,
                        &mut notice,
                    )?;
                    refresh_decoder_escape_deadline(&decoder, &mut input_escape_deadline);
                    action
                }
            };

            match action {
                EnhancedInputAction::None => continue,
                EnhancedInputAction::Redraw
                | EnhancedInputAction::RedrawFence
                | EnhancedInputAction::PasteFence => {}
                EnhancedInputAction::Exit => break Ok(InteractiveExit::Ordinary(0)),
                EnhancedInputAction::Submit => {
                    let mut queued_id: Option<LocalPromptId> = None;
                    let composer_submission =
                        classify_enhanced_submission(input.composer().text());
                    let local_command = !auto_submit
                        && matches!(
                            composer_submission,
                            EnhancedSubmission::Command(_) | EnhancedSubmission::Theme(_)
                        );
                    let (draft, cursor) = if local_command {
                        let cursor = input.composer().cursor();
                        (input.take_draft_for_turn()?, cursor)
                    } else if input.queue().len() != 0 {
                        let reserved = input.reserve_front()?;
                        let id = reserved.id();
                        let text = copy_enhanced_prompt(reserved.text())?;
                        queued_id = Some(id);
                        let cursor = text.len();
                        (text, cursor)
                    } else {
                        let cursor = input.composer().cursor();
                        (input.take_draft_for_turn()?, cursor)
                    };
                    let submission = if local_command {
                        composer_submission
                    } else if queued_id.is_some() {
                        EnhancedSubmission::Prompt
                    } else {
                        classify_enhanced_submission(&draft)
                    };
                    if queued_id.is_none() {
                        let _ = file_suggestions
                            .sync(input.composer(), false, false)
                            .map_err(|_| InteractiveError::Agent)?;
                    }
                    match submission {
                        EnhancedSubmission::Empty => notice = None,
                        EnhancedSubmission::Command(command) => match command {
                            CommandId::Help => {
                                notice = Some(
                                    "/inspect | /review | /focus | /theme | /help | /exit | /quit | Ctrl+O inspect"
                                        .to_owned(),
                                );
                            }
                            CommandId::Inspect | CommandId::Review | CommandId::Focus => {
                                let mode = match command {
                                    CommandId::Inspect => ViewMode::Inspect,
                                    CommandId::Review => ViewMode::Review,
                                    CommandId::Focus => ViewMode::Focus,
                                    _ => return Err(InteractiveError::Agent),
                                };
                                let _ = view
                                    .request_mode(mode)
                                    .map_err(|_| InteractiveError::Output)?;
                                notice = None;
                            }
                            CommandId::Theme => {
                                apply_theme_command(
                                    ThemeCommand::Show,
                                    &mut theme,
                                    &mut notice,
                                )?;
                            }
                            CommandId::Exit | CommandId::Quit => {
                                break Ok(InteractiveExit::Ordinary(0));
                            }
                        },
                        EnhancedSubmission::Theme(command) => {
                            apply_theme_command(command, &mut theme, &mut notice)?;
                        }
                        EnhancedSubmission::Prompt => {
                            let prompt = copy_enhanced_prompt(&draft)?;
                            presenter.observe_external_line_start();
                            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                            let mut prompt_committed = false;
                            let active_terminal = terminal.application_terminal()?;
                            let mut active_dock = ActiveDock {
                                screen: &mut screen,
                                last_size: &mut last_size,
                                view: &mut view,
                                theme: &mut theme,
                                command_palette: &mut command_palette,
                                file_suggestions: &mut file_suggestions,
                                palette_suppressed: false,
                            };
                            let _ = active_dock
                                .view
                                .request_mode(ViewMode::Focus)
                                .map_err(|_| InteractiveError::Output)?;
                            if let Some(signal) = render_active_dock(
                                &input,
                                notice.as_deref(),
                                DockInteraction::Running,
                                &live,
                                active_terminal,
                                signals,
                                &mut active_dock,
                            )
                            .await?
                            {
                                if let Some(id) = queued_id {
                                    input.release_reserved(id)?;
                                } else {
                                    input
                                        .restore_uncommitted_draft(draft, cursor)
                                        .map_err(|_| InteractiveError::Agent)?;
                                }
                                pending_signal = Some(signal);
                                continue;
                            }
                            let panic_restore = terminal.panic_restore()?;
                            let disposition = run_turn(ActiveTurn {
                                agent: &mut agent,
                                events: &mut events,
                                approvals: &mut approvals,
                                joins: &mut joins,
                                live: &mut live,
                                presenter: &mut presenter,
                                terminal: active_terminal,
                                panic_restore: Some(panic_restore),
                                signals,
                                parser: &mut parser,
                                scratch: &mut scratch,
                                prompt,
                                prompt_committed: &mut prompt_committed,
                                queued_input: Some(&mut input),
                                queue_notice: Some(&mut notice),
                                enhanced_decoder: Some(&mut decoder),
                                active_dock: Some(active_dock),
                                enhanced_presenter: Some(&mut enhanced_presenter),
                                color: true,
                                enhanced: true,
                            })
                            .await?;
                            if matches!(
                                disposition,
                                TurnDisposition::Continue
                                    | TurnDisposition::Signal(UiSignal::Suspend)
                            ) {
                                settle_enhanced_prompt(
                                    &mut input,
                                    queued_id,
                                    draft,
                                    cursor,
                                    prompt_committed,
                                    &mut notice,
                                    &mut auto_queue_paused,
                                )?;
                                decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                            }
                            match disposition {
                                TurnDisposition::Continue => {}
                                TurnDisposition::Exit(code) => {
                                    break Ok(InteractiveExit::Ordinary(code));
                                }
                                TurnDisposition::Signal(UiSignal::Suspend) => {
                                    let mut dock = ActiveDock {
                                        screen: &mut screen,
                                        last_size: &mut last_size,
                                        view: &mut view,
                                        theme: &mut theme,
                                        command_palette: &mut command_palette,
                                        file_suggestions: &mut file_suggestions,
                                        palette_suppressed: false,
                                    };
                                    pending_signal = suspend_enhanced(
                                        &mut terminal,
                                        signals,
                                        &mut decoder,
                                        DockRenderModel {
                                            input: &input,
                                            notice: notice.as_deref(),
                                            interaction: DockInteraction::Idle,
                                            command_palette: dock
                                                .command_palette
                                                .snapshot(input.composer()),
                                            live: &live,
                                        },
                                        &mut dock,
                                    )
                                    .await?;
                                    continue;
                                }
                                TurnDisposition::Signal(signal) => {
                                    break Ok(InteractiveExit::Signal(signal));
                                }
                            }
                        }
                    }
                }
            }

            pending_signal = render_enhanced_dock(
                DockRenderModel {
                    input: &input,
                    notice: notice.as_deref(),
                    interaction: DockInteraction::Idle,
                    command_palette: command_palette.snapshot(input.composer()),
                    live: &live,
                },
                &terminal,
                &mut last_size,
                signals,
                &mut screen,
                &mut view,
                &mut theme,
                &mut file_suggestions,
            )
            .await?;
            if action == EnhancedInputAction::PasteFence && pending_signal.is_none() {
                match complete_paste_input_fence(
                    terminal.application_terminal()?,
                    signals,
                    &mut scratch,
                )
                .await?
                {
                    PasteFenceOutcome::Ready => {
                        decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                        input_escape_deadline = None;
                        notice = Some("Paste ready · Enter sends".to_owned());
                        pending_signal = render_enhanced_dock(
                            DockRenderModel {
                                input: &input,
                                notice: notice.as_deref(),
                                interaction: DockInteraction::Idle,
                                command_palette: command_palette.snapshot(input.composer()),
                                live: &live,
                            },
                            &terminal,
                            &mut last_size,
                            signals,
                            &mut screen,
                            &mut view,
                            &mut theme,
                            &mut file_suggestions,
                        )
                        .await?;
                    }
                    PasteFenceOutcome::Signal(signal) => pending_signal = Some(signal),
                    PasteFenceOutcome::Eof => break Ok(InteractiveExit::Ordinary(0)),
                }
            }
        }
    })
    .catch_unwind()
    .await;

    let mut result = match result {
        Ok(result) => result,
        Err(_) => {
            file_suggestions.cancel_for_shutdown();
            terminal.best_effort_visual_reset();
            let _ = terminal.finish();
            Err(InteractiveError::Agent)
        }
    };
    // Stop filesystem work before terminal teardown; the blocking join is
    // drained concurrently with Agent/tool cleanup after termios is restored.
    file_suggestions.cancel_for_shutdown();
    let mut cleanup_signals = SignalLatch::default();
    let mut visual_reset_complete = false;
    let cleanup_geometry_changed = terminal.size().is_none_or(|size| size != last_size);
    let mut visual_reset_requires_clear = screen.is_poisoned() || cleanup_geometry_changed;
    if !screen.is_detached() && !screen.is_poisoned() && !cleanup_geometry_changed {
        match screen.stage_detach().map_err(map_inline_screen_error) {
            Ok(write) => match write_screen_transaction(
                terminal.output_terminal(),
                signals,
                &mut screen,
                write,
            )
            .await
            {
                Ok(ScreenWriteOutcome::Complete) => visual_reset_complete = true,
                Ok(ScreenWriteOutcome::Signal(signal)) => {
                    visual_reset_requires_clear = true;
                    observe_enhanced_cleanup_signal(&mut result, &mut cleanup_signals, signal);
                }
                Ok(ScreenWriteOutcome::Resize | ScreenWriteOutcome::PoisonedResize) => {
                    // The emulator changes geometry before SIGWINCH is
                    // delivered, so the old dock coordinates are no longer
                    // safe to clear selectively.
                    visual_reset_requires_clear = true;
                }
                Err(error) => {
                    if result.is_ok() {
                        result = Err(error);
                    }
                }
            },
            Err(error) => {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
    }
    if !visual_reset_complete {
        visual_reset_requires_clear |= screen.is_poisoned();
        let reset = if visual_reset_requires_clear {
            POISON_TEARDOWN_BYTES
        } else {
            ENHANCED_VISUAL_RESET_BYTES
        };
        match write_enhanced_bytes(terminal.output_terminal(), reset, signals).await {
            Ok(Some(signal)) => {
                observe_enhanced_cleanup_signal(&mut result, &mut cleanup_signals, signal);
            }
            Ok(None) => visual_reset_complete = true,
            Err(error) => {
                if matches!(result, Ok(InteractiveExit::Ordinary(_))) {
                    result = Err(error);
                }
            }
        }
    }
    if !visual_reset_complete {
        terminal.best_effort_visual_reset();
    }
    if let Err(error) = terminal.finish() {
        if result.is_ok() {
            result = Err(error.into());
        }
    }

    if let Some(signal) = result.as_ref().ok().and_then(|exit| match exit {
        InteractiveExit::Signal(signal) => Some(*signal),
        InteractiveExit::Ordinary(_) => None,
    }) {
        cleanup_signals.observe(DriverMode::Interactive, signal);
    }
    let initial_signal = cleanup_signals.observed();
    let (agent_cleanup, suggestion_cleanup) = tokio::join!(
        shutdown::agent_with_signals(&mut agent, DriverMode::Interactive, signals, initial_signal,),
        file_suggestions.finish_shutdown(),
    );
    if suggestion_cleanup.is_err() && result.is_ok() {
        result = Err(InteractiveError::Agent);
    }
    let (shutdown, signal) = agent_cleanup;
    if let Some(signal) = signal {
        if let Some(code) =
            finish_signal_after_shutdown(signal, terminal.restored_terminal()?, signals).await?
        {
            return Ok(code);
        }
    }
    match (result, shutdown) {
        (Err(InteractiveError::Agent), Err(error)) => match error.session_error() {
            Some(error) => Err(InteractiveError::Storage(storage_failure::from_shutdown(
                error,
            ))),
            None => Err(InteractiveError::Agent),
        },
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => match error.session_error() {
            Some(error) => Err(InteractiveError::Storage(storage_failure::from_shutdown(
                error,
            ))),
            None => Err(InteractiveError::Agent),
        },
        (Ok(InteractiveExit::Ordinary(exit)), Ok(())) => Ok(exit),
        (Ok(InteractiveExit::Signal(_)), Ok(())) => Err(InteractiveError::Agent),
    }
}

fn observe_enhanced_cleanup_signal(
    result: &mut Result<InteractiveExit, InteractiveError>,
    signals: &mut SignalLatch,
    signal: UiSignal,
) {
    signals.observe(DriverMode::Interactive, signal);
    let terminating = matches!(
        signal,
        UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate
    );
    let terminating_already_latched = matches!(
        result,
        Ok(InteractiveExit::Signal(
            UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate
        ))
    );
    if terminating && !terminating_already_latched {
        *result = Ok(InteractiveExit::Signal(signal));
    }
}

#[allow(clippy::too_many_arguments)]
fn settle_enhanced_prompt(
    input: &mut InputMemory,
    queued_id: Option<LocalPromptId>,
    draft: String,
    cursor: usize,
    prompt_committed: bool,
    notice: &mut Option<String>,
    auto_queue_paused: &mut bool,
) -> Result<(), InteractiveError> {
    if prompt_committed {
        let history_prompt = if let Some(id) = queued_id {
            let admitted = input.commit_reserved(id)?;
            if admitted.id() != id {
                return Err(InteractiveError::Agent);
            }
            admitted.into_text()
        } else {
            draft
        };
        if input.record_committed_human(&history_prompt).is_err() {
            *notice = Some("History is full; the conversation is safe".to_owned());
        } else {
            *notice = None;
        }
        *auto_queue_paused = false;
    } else {
        if let Some(id) = queued_id {
            input.release_reserved(id)?;
            *auto_queue_paused = true;
        } else {
            input
                .restore_uncommitted_draft(draft, cursor)
                .map_err(|_| InteractiveError::Agent)?;
        }
        *notice = Some("Prompt was not admitted; draft or queue entry kept".to_owned());
    }
    Ok(())
}

async fn complete_paste_input_fence(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    scratch: &mut [u8; TERMINAL_READ_BYTES],
) -> Result<PasteFenceOutcome, InteractiveError> {
    let mut quiet_deadline = Instant::now() + PASTE_INPUT_QUIET;
    loop {
        tokio::select! {
            biased;
            signal = signals.next_interactive() => match signal {
                InteractiveSignal::Stop(signal) => {
                    return Ok(PasteFenceOutcome::Signal(signal));
                }
                InteractiveSignal::Resize => {}
            },
            read = terminal.read_once(scratch) => {
                let count = read.map_err(|_| InteractiveError::TerminalUnavailable)?;
                if count == 0 {
                    return Ok(PasteFenceOutcome::Eof);
                }
                quiet_deadline = Instant::now() + PASTE_INPUT_QUIET;
            }
            () = tokio::time::sleep_until(quiet_deadline) => {
                terminal.flush_input()?;
                return Ok(PasteFenceOutcome::Ready);
            }
        }
    }
}

async fn shutdown_after_enhanced_error(
    agent: &mut AgentLoop,
    signals: &mut SignalStreams,
    error: InteractiveError,
) -> Result<u8, InteractiveError> {
    let (shutdown, signal) =
        shutdown::agent_with_signals(agent, DriverMode::Interactive, signals, None).await;
    if let Some(signal @ (UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate)) = signal {
        return signal.exit_code().ok_or(InteractiveError::Agent);
    }
    if let Err(shutdown) = shutdown {
        if let Some(storage) = shutdown.session_error() {
            return Err(InteractiveError::Storage(storage_failure::from_shutdown(
                storage,
            )));
        }
        if error == InteractiveError::Agent {
            return Err(InteractiveError::Agent);
        }
    }
    Err(error)
}

enum EnhancedIdleEvent {
    Signal(UiSignal),
    Resize,
    EscapeExpired,
    Eof,
    AutoSubmit,
    Bytes(usize),
    FileSuggestion(Result<JobSettlement, tokio::task::JoinError>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScreenWriteOutcome {
    Complete,
    Signal(UiSignal),
    Resize,
    PoisonedResize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnhancedInputAction {
    None,
    Redraw,
    RedrawFence,
    PasteFence,
    Submit,
    Exit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PasteFenceOutcome {
    Ready,
    Signal(UiSignal),
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnhancedSubmission {
    Empty,
    Command(CommandId),
    Theme(ThemeCommand),
    Prompt,
}

fn classify_enhanced_submission(prompt: &str) -> EnhancedSubmission {
    let command = prompt.trim_matches(|character: char| character.is_ascii_whitespace());
    if command.is_empty() {
        EnhancedSubmission::Empty
    } else if let Some(command) = CommandId::from_exact(command) {
        EnhancedSubmission::Command(command)
    } else if let Some(theme) = ThemeCommand::parse(command) {
        EnhancedSubmission::Theme(theme)
    } else {
        EnhancedSubmission::Prompt
    }
}

const THEME_LIST_NOTICE: &str =
    "Themes · adaptive · midnight · paper · color-blind · high-contrast · mono";

fn apply_theme_command(
    command: ThemeCommand,
    theme: &mut ThemeState,
    notice: &mut Option<String>,
) -> Result<(), InteractiveError> {
    *notice = Some(match command {
        ThemeCommand::Show => format!(
            "Theme · {} | {THEME_LIST_NOTICE}",
            theme.requested().palette().name()
        ),
        ThemeCommand::Select(palette) => {
            let changed = theme
                .request(palette)
                .map_err(|_| InteractiveError::Output)?;
            if changed {
                format!("Theme changed · {}", palette.name())
            } else {
                format!("Theme already active · {}", palette.name())
            }
        }
        ThemeCommand::Invalid => {
            format!("Unknown theme | {THEME_LIST_NOTICE}")
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_enhanced_input(
    decoder: &mut KeyDecoder,
    bytes: &[u8],
    input: &mut InputMemory,
    command_palette: &mut CommandPaletteState,
    mut file_suggestions: Option<&mut FileSuggestionController>,
    view: &mut ViewState,
    theme: &ThemeState,
    size: super::terminal::TerminalSize,
    notice: &mut Option<String>,
) -> Result<EnhancedInputAction, InteractiveError> {
    if file_suggestions.as_deref().is_some_and(|controller| {
        controller.presented_is_invalidated() || controller.decoder_reset_required()
    }) {
        decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
        if let Some(controller) = file_suggestions.as_deref_mut() {
            controller.mark_decoder_reset();
        }
        return Ok(EnhancedInputAction::Redraw);
    }
    if view.requested() != view.committed() || theme.is_transitioning() {
        decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
        return Ok(EnhancedInputAction::Redraw);
    }
    if view.committed().mode() != ViewMode::Focus {
        return apply_detail_input(decoder, bytes, view, notice);
    }
    let mut action = EnhancedInputAction::None;
    let width = usize::from(size.columns.saturating_sub(3)).max(1);
    let _ = decoder.feed(bytes, |decoded| {
        let rejected = matches!(
            &decoded.event,
            InputEvent::Rejected(_) | InputEvent::PasteRejected(_)
        );
        let completed_paste = matches!(
            &decoded.event,
            InputEvent::Paste(_) | InputEvent::PasteRejected(_)
        );
        let update = match decoded.event {
            InputEvent::PasteStarted => Ok(EnhancedInputAction::None),
            InputEvent::Paste(text) => match input.insert_paste(&text) {
                Ok(()) => {
                    let _ = command_palette.sync(input.composer());
                    let sync = file_suggestions
                        .as_deref_mut()
                        .map_or(Ok(false), |controller| {
                            controller
                                .sync(input.composer(), false, false)
                                .map_err(|_| InputMemoryError::InvalidState)
                        });
                    match sync {
                        Ok(_) => {
                            *notice = Some(
                                "Paste inserted · Enter sends after the input fence".to_owned(),
                            );
                            Ok(EnhancedInputAction::PasteFence)
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => {
                    *notice = Some(format!("{error} · draft kept behind the input fence"));
                    Ok(EnhancedInputAction::PasteFence)
                }
            },
            InputEvent::PasteRejected(error) => {
                let _ = command_palette.dismiss(input.composer());
                let dismissal = file_suggestions
                    .as_deref_mut()
                    .map_or(Ok(false), |controller| {
                        controller
                            .dismiss(input.composer())
                            .map_err(|_| InputMemoryError::InvalidState)
                    });
                dismissal.map(|_| {
                    *notice = Some(error.to_string());
                    EnhancedInputAction::PasteFence
                })
            }
            InputEvent::Rejected(error) => {
                let _ = command_palette.dismiss(input.composer());
                let dismissal = file_suggestions
                    .as_deref_mut()
                    .map_or(Ok(false), |controller| {
                        controller
                            .dismiss(input.composer())
                            .map_err(|_| InputMemoryError::InvalidState)
                    });
                dismissal.map(|_| {
                    *notice = Some(error.to_string());
                    EnhancedInputAction::Redraw
                })
            }
            InputEvent::Key(Key::Inspect) => view
                .toggle_inspect()
                .map(|()| EnhancedInputAction::Redraw)
                .map_err(|_| InputMemoryError::InvalidState),
            InputEvent::Key(key) => apply_enhanced_key(
                key,
                input,
                command_palette,
                file_suggestions.as_deref_mut(),
                width,
                notice,
            ),
        };
        match update {
            Ok(EnhancedInputAction::None) => ControlFlow::Continue(()),
            Ok(next) => {
                action = next;
                if rejected
                    || completed_paste
                    || view.requested() != view.committed()
                    || matches!(
                        next,
                        EnhancedInputAction::RedrawFence
                            | EnhancedInputAction::Submit
                            | EnhancedInputAction::Exit
                    )
                {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            Err(error) => {
                *notice = Some(error.to_string());
                action = EnhancedInputAction::Redraw;
                ControlFlow::Break(())
            }
        }
    });
    Ok(action)
}

fn apply_detail_input(
    decoder: &mut KeyDecoder,
    bytes: &[u8],
    view: &mut ViewState,
    notice: &mut Option<String>,
) -> Result<EnhancedInputAction, InteractiveError> {
    let mut action = EnhancedInputAction::None;
    let _ = decoder.feed(bytes, |decoded| {
        let completed = matches!(
            &decoded.event,
            InputEvent::Paste(_) | InputEvent::PasteRejected(_) | InputEvent::Rejected(_)
        );
        let next = match decoded.event {
            InputEvent::PasteStarted => Ok(EnhancedInputAction::None),
            InputEvent::Paste(_) | InputEvent::PasteRejected(_) | InputEvent::Rejected(_) => {
                Ok(EnhancedInputAction::Redraw)
            }
            InputEvent::Key(key) => apply_detail_key(key, view),
        };
        match next {
            Ok(EnhancedInputAction::None) => ControlFlow::Continue(()),
            Ok(next) => {
                action = next;
                *notice = None;
                if completed || view.requested() != view.committed() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            Err(()) => {
                action = EnhancedInputAction::Redraw;
                *notice = Some("View navigation limit reached".to_owned());
                ControlFlow::Break(())
            }
        }
    });
    Ok(action)
}

fn apply_detail_key(key: Key, view: &mut ViewState) -> Result<EnhancedInputAction, ()> {
    let changed = match key {
        Key::Inspect | Key::Escape | Key::Eof | Key::Char('q') => {
            view.request_mode(ViewMode::Focus).map_err(|_| ())?
        }
        Key::Tab | Key::BackTab => {
            view.switch_detail().map_err(|_| ())?;
            true
        }
        Key::Up => view.scroll_lines(-1).map_err(|_| ())?,
        Key::Down => view.scroll_lines(1).map_err(|_| ())?,
        Key::PageUp => view.scroll_page(false).map_err(|_| ())?,
        Key::PageDown => view.scroll_page(true).map_err(|_| ())?,
        Key::Home => view.request_offset(0).map_err(|_| ())?,
        Key::End => view.scroll_end().map_err(|_| ())?,
        Key::Left => view.request_mode(ViewMode::Inspect).map_err(|_| ())?,
        Key::Right => view.request_mode(ViewMode::Review).map_err(|_| ())?,
        Key::Enter
        | Key::Newline
        | Key::Char(_)
        | Key::Backspace
        | Key::Delete
        | Key::WordErase
        | Key::ClearBefore
        | Key::ClearAfter
        | Key::Yank
        | Key::Undo
        | Key::ReverseSearch => false,
    };
    Ok(if changed {
        EnhancedInputAction::Redraw
    } else {
        EnhancedInputAction::None
    })
}

fn refresh_decoder_escape_deadline(decoder: &KeyDecoder, deadline: &mut Option<Instant>) {
    if decoder.escape_pending() {
        if deadline.is_none() {
            *deadline = Some(Instant::now() + ESCAPE_SEQUENCE_WAIT);
        }
    } else {
        *deadline = None;
    }
}

fn reset_file_suggestion_decoder(
    controller: &mut FileSuggestionController,
    decoder: Option<&mut KeyDecoder>,
    deadline: &mut Option<Instant>,
) -> Result<(), InteractiveError> {
    if !controller.decoder_reset_required() {
        return Ok(());
    }
    let decoder = decoder.ok_or(InteractiveError::Agent)?;
    decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
    *deadline = None;
    controller.mark_decoder_reset();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn expire_enhanced_escape(
    decoder: &mut KeyDecoder,
    input: &mut InputMemory,
    command_palette: &mut CommandPaletteState,
    mut file_suggestions: Option<&mut FileSuggestionController>,
    view: &mut ViewState,
    theme: &ThemeState,
    size: TerminalSize,
    notice: &mut Option<String>,
) -> Result<EnhancedInputAction, InteractiveError> {
    if file_suggestions.as_deref().is_some_and(|controller| {
        controller.presented_is_invalidated() || controller.decoder_reset_required()
    }) {
        decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
        if let Some(controller) = file_suggestions.as_deref_mut() {
            controller.mark_decoder_reset();
        }
        return Ok(EnhancedInputAction::Redraw);
    }
    if view.requested() != view.committed() || theme.is_transitioning() {
        decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
        return Ok(EnhancedInputAction::Redraw);
    }
    let Some(decoded) = decoder.expire_escape() else {
        return Ok(EnhancedInputAction::None);
    };
    match decoded.event {
        InputEvent::Key(key) if view.committed().mode() == ViewMode::Focus => apply_enhanced_key(
            key,
            input,
            command_palette,
            file_suggestions.as_deref_mut(),
            usize::from(size.columns.saturating_sub(3)).max(1),
            notice,
        )
        .map_err(InteractiveError::from),
        InputEvent::Key(key) => apply_detail_key(key, view)
            .map_err(|()| InteractiveError::Agent)
            .inspect(|_| *notice = None),
        InputEvent::Rejected(error) => {
            let _ = command_palette.dismiss(input.composer());
            if let Some(controller) = file_suggestions {
                controller
                    .dismiss(input.composer())
                    .map_err(|_| InteractiveError::Agent)?;
            }
            *notice = Some(error.to_string());
            Ok(EnhancedInputAction::Redraw)
        }
        InputEvent::PasteStarted | InputEvent::Paste(_) | InputEvent::PasteRejected(_) => {
            Err(InteractiveError::Agent)
        }
    }
}

fn apply_enhanced_key(
    key: Key,
    input: &mut InputMemory,
    command_palette: &mut CommandPaletteState,
    mut file_suggestions: Option<&mut FileSuggestionController>,
    width: usize,
    notice: &mut Option<String>,
) -> Result<EnhancedInputAction, InputMemoryError> {
    if let Some(controller) = file_suggestions.as_deref_mut() {
        match key {
            Key::Up | Key::BackTab
                if controller
                    .navigate_presented(FileSuggestionMove::Previous)
                    .map_err(|_| InputMemoryError::InvalidState)? =>
            {
                return Ok(EnhancedInputAction::RedrawFence);
            }
            Key::Down | Key::Tab
                if controller
                    .navigate_presented(FileSuggestionMove::Next)
                    .map_err(|_| InputMemoryError::InvalidState)? =>
            {
                return Ok(EnhancedInputAction::RedrawFence);
            }
            Key::Enter => match controller.enter_presented(input)? {
                FileSuggestionEnter::Ordinary => {}
                FileSuggestionEnter::Consumed | FileSuggestionEnter::Completed => {
                    let _ = command_palette.sync(input.composer());
                    return Ok(EnhancedInputAction::RedrawFence);
                }
            },
            Key::Escape if controller.presented_menu_is_visible() => {
                let _ = controller
                    .dismiss(input.composer())
                    .map_err(|_| InputMemoryError::InvalidState)?;
                *notice = None;
                return Ok(EnhancedInputAction::Redraw);
            }
            _ => {}
        }
    }
    if command_palette.sync(input.composer()).is_visible() {
        match key {
            Key::Up | Key::BackTab => {
                let _ = command_palette.navigate(input.composer(), PaletteMove::Previous);
                return Ok(EnhancedInputAction::RedrawFence);
            }
            Key::Down | Key::Tab => {
                let _ = command_palette.navigate(input.composer(), PaletteMove::Next);
                return Ok(EnhancedInputAction::RedrawFence);
            }
            Key::Enter => match command_palette.enter(input.composer()) {
                PaletteEnter::Submit => return Ok(EnhancedInputAction::Submit),
                PaletteEnter::Complete(command) => {
                    input.complete_local_command(command)?;
                    let _ = command_palette.sync(input.composer());
                    return Ok(EnhancedInputAction::RedrawFence);
                }
            },
            Key::Escape => {
                let _ = command_palette.dismiss(input.composer());
                *notice = None;
                return Ok(EnhancedInputAction::Redraw);
            }
            _ => {}
        }
    }
    let mut changed = false;
    match key {
        Key::Enter => return Ok(EnhancedInputAction::Submit),
        Key::Newline => input.insert_newline()?,
        Key::Char('?') if input.composer().is_empty() => {
            *notice = Some(
                "/inspect · /review · /focus · /theme · /help · /exit · /quit · Enter send · Ctrl+J newline"
                    .to_owned(),
            );
            return Ok(EnhancedInputAction::Redraw);
        }
        Key::Char(character) => input.insert_char(character)?,
        Key::Tab => input.insert_text("\t")?,
        Key::BackTab => changed = input.move_left(),
        Key::Left => changed = input.move_left(),
        Key::Right => changed = input.move_right(),
        Key::Up => changed = input.move_up_or_history(width)?,
        Key::Down => changed = input.move_down_or_history(width)?,
        Key::Home => changed = input.move_line_start(),
        Key::End => changed = input.move_line_end(),
        Key::Backspace => changed = input.backspace()?,
        Key::Delete => changed = input.delete()?,
        Key::WordErase => changed = input.erase_word()?,
        Key::ClearBefore => changed = input.clear_before_cursor()?,
        Key::ClearAfter => changed = input.clear_after_cursor()?,
        Key::Yank => changed = input.yank()?,
        Key::Undo => changed = input.undo()?,
        Key::ReverseSearch => {
            let found = input.reverse_search_previous()?;
            let _ = command_palette.sync(input.composer());
            *notice = Some(if found {
                "Reverse search · Ctrl+R finds the next older match".to_owned()
            } else {
                "No older history match".to_owned()
            });
            return Ok(EnhancedInputAction::Redraw);
        }
        Key::Inspect | Key::PageUp | Key::PageDown => return Ok(EnhancedInputAction::None),
        Key::Escape => {
            *notice = None;
            return Ok(EnhancedInputAction::Redraw);
        }
        Key::Eof => {
            if input.composer().is_empty() {
                return Ok(EnhancedInputAction::Exit);
            }
            changed = input.delete()?;
        }
    }
    let _ = command_palette.sync(input.composer());
    if let Some(controller) = file_suggestions {
        let _ = controller
            .sync(input.composer(), false, false)
            .map_err(|_| InputMemoryError::InvalidState)?;
    }
    *notice = None;
    Ok(
        if changed
            || !matches!(
                key,
                Key::BackTab | Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End
            )
        {
            EnhancedInputAction::Redraw
        } else {
            EnhancedInputAction::None
        },
    )
}

#[allow(clippy::too_many_arguments)]
async fn render_enhanced_dock(
    model: DockRenderModel<'_>,
    terminal: &TerminalSession,
    last_size: &mut TerminalSize,
    signals: &mut SignalStreams,
    screen: &mut InlineScreen,
    view: &mut ViewState,
    theme: &mut ThemeState,
    file_suggestions: &mut FileSuggestionController,
) -> Result<Option<UiSignal>, InteractiveError> {
    loop {
        if screen.is_poisoned() {
            file_suggestions.invalidate_presentation();
            if let Some(signal) =
                recover_poisoned_screen(terminal.output_terminal(), signals, screen).await?
            {
                return Ok(Some(signal));
            }
        }
        let size = terminal.size().unwrap_or(*last_size);
        let resized = size != *last_size;
        if resized {
            file_suggestions.invalidate_presentation();
        }
        let show_file_suggestions = view.requested().mode() == ViewMode::Focus
            && !matches!(model.interaction, DockInteraction::Approval(_));
        let staged = file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let surface = enhanced_surface_frame(
            model.input,
            model.notice,
            command_palette_interaction(
                model.interaction,
                palette_behind_files(model.command_palette, file_snapshot),
                view.requested().mode(),
            ),
            size,
            view,
            theme,
            model.live,
            file_snapshot,
        )?;
        let write = stage_surface(
            screen,
            size,
            resized,
            &surface.frame,
            surface.commit.theme.palette(),
        )?;
        match write_screen_transaction(terminal.output_terminal(), signals, screen, write).await? {
            ScreenWriteOutcome::Complete => {
                *last_size = size;
                commit_surface(view, theme, surface.commit);
                file_suggestions.commit_presentation(staged);
                return Ok(None);
            }
            ScreenWriteOutcome::Signal(signal) => {
                if screen.is_poisoned() {
                    file_suggestions.invalidate_presentation();
                }
                return Ok(Some(signal));
            }
            ScreenWriteOutcome::Resize => {
                file_suggestions.invalidate_presentation();
                continue;
            }
            ScreenWriteOutcome::PoisonedResize => {
                file_suggestions.invalidate_presentation();
                if let Some(signal) =
                    recover_poisoned_screen(terminal.output_terminal(), signals, screen).await?
                {
                    return Ok(Some(signal));
                }
            }
        }
    }
}

async fn render_active_dock(
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    live: &LiveRenderer,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    dock: &mut ActiveDock<'_>,
) -> Result<Option<UiSignal>, InteractiveError> {
    loop {
        if dock.screen.is_poisoned() {
            dock.file_suggestions.invalidate_presentation();
            if let Some(signal) = recover_poisoned_screen(terminal, signals, dock.screen).await? {
                return Ok(Some(signal));
            }
        }
        let size = terminal.size().unwrap_or(*dock.last_size);
        let resized = size != *dock.last_size;
        if resized {
            dock.file_suggestions.invalidate_presentation();
        }
        let palette = if dock.palette_suppressed {
            CommandPaletteSnapshot::Hidden
        } else {
            dock.command_palette.snapshot(input.composer())
        };
        let show_file_suggestions = dock.view.requested().mode() == ViewMode::Focus
            && !matches!(interaction, DockInteraction::Approval(_));
        let staged = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let surface = enhanced_surface_frame(
            input,
            notice,
            command_palette_interaction(
                interaction,
                palette_behind_files(palette, file_snapshot),
                dock.view.requested().mode(),
            ),
            size,
            dock.view,
            dock.theme,
            live,
            file_snapshot,
        )?;
        let write = stage_surface(
            dock.screen,
            size,
            resized,
            &surface.frame,
            surface.commit.theme.palette(),
        )?;
        match write_screen_transaction(terminal, signals, dock.screen, write).await? {
            ScreenWriteOutcome::Complete => {
                *dock.last_size = size;
                commit_surface(dock.view, dock.theme, surface.commit);
                dock.file_suggestions.commit_presentation(staged);
                return Ok(None);
            }
            ScreenWriteOutcome::Signal(signal) => {
                if dock.screen.is_poisoned() {
                    dock.file_suggestions.invalidate_presentation();
                }
                return Ok(Some(signal));
            }
            ScreenWriteOutcome::Resize => {
                dock.file_suggestions.invalidate_presentation();
                continue;
            }
            ScreenWriteOutcome::PoisonedResize => {
                dock.file_suggestions.invalidate_presentation();
                if let Some(signal) =
                    recover_poisoned_screen(terminal, signals, dock.screen).await?
                {
                    return Ok(Some(signal));
                }
            }
        }
    }
}

fn map_dock_error(error: DockError) -> InteractiveError {
    match error {
        DockError::TooSmall => InteractiveError::TerminalUnsupported,
        DockError::Capacity | DockError::Limit | DockError::InvalidState => {
            InteractiveError::Output
        }
    }
}

fn active_command_palette_snapshot(
    input: &InputMemory,
    dock: &ActiveDock<'_>,
) -> CommandPaletteSnapshot {
    if dock.palette_suppressed {
        CommandPaletteSnapshot::Hidden
    } else {
        dock.command_palette.snapshot(input.composer())
    }
}

fn enhanced_dock_frame(
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    size: TerminalSize,
    file_suggestions: FileSuggestionSnapshot<'_>,
) -> Result<DockFrame, InteractiveError> {
    DockFrame::layout(
        DockModel {
            interaction,
            composer: input.composer(),
            queue: input.queue(),
            notice,
            file_suggestions,
        },
        size.rows,
        size.columns,
    )
    .map_err(map_dock_error)
}

fn command_palette_interaction(
    interaction: DockInteraction,
    command_palette: CommandPaletteSnapshot,
    view: ViewMode,
) -> DockInteraction {
    let interaction = match interaction {
        DockInteraction::CommandPalette { running: true, .. } => DockInteraction::Running,
        DockInteraction::CommandPalette { running: false, .. } => DockInteraction::Idle,
        interaction => interaction,
    };
    if view == ViewMode::Focus
        && !matches!(interaction, DockInteraction::Approval(_))
        && command_palette.is_visible()
    {
        DockInteraction::CommandPalette {
            running: matches!(interaction, DockInteraction::Running),
            snapshot: command_palette,
        }
    } else {
        interaction
    }
}

fn palette_behind_files(
    command_palette: CommandPaletteSnapshot,
    file_suggestions: FileSuggestionSnapshot<'_>,
) -> CommandPaletteSnapshot {
    if file_suggestions.is_visible() {
        CommandPaletteSnapshot::Hidden
    } else {
        command_palette
    }
}

#[allow(clippy::too_many_arguments)]
fn enhanced_surface_frame(
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    size: TerminalSize,
    view: &mut ViewState,
    theme: &ThemeState,
    live: &LiveRenderer,
    file_suggestions: FileSuggestionSnapshot<'_>,
) -> Result<EnhancedSurface, InteractiveError> {
    if !matches!(
        interaction,
        DockInteraction::Idle | DockInteraction::Running
    ) || size.columns < MIN_ENHANCED_COLUMNS
        || size.rows < MIN_ENHANCED_ROWS
    {
        let _ = view
            .request_mode(ViewMode::Focus)
            .map_err(|_| InteractiveError::Output)?;
    }
    let request = view.requested();
    enhanced_surface_frame_for_request(
        input,
        notice,
        interaction,
        size,
        request,
        theme.requested(),
        live,
        file_suggestions,
    )
}

#[allow(clippy::too_many_arguments)]
fn enhanced_surface_frame_for_request(
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    size: TerminalSize,
    request: ViewRequest,
    theme: ThemeRequest,
    live: &LiveRenderer,
    file_suggestions: FileSuggestionSnapshot<'_>,
) -> Result<EnhancedSurface, InteractiveError> {
    match request.mode() {
        ViewMode::Focus => Ok(EnhancedSurface {
            frame: enhanced_dock_frame(input, notice, interaction, size, file_suggestions)?,
            commit: SurfaceCommit {
                request,
                theme,
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
        }),
        ViewMode::Inspect | ViewMode::Review => {
            let document = if request.mode() == ViewMode::Inspect {
                live.inspect_document()
            } else {
                live.review_document()
            }
            .map_err(|_| InteractiveError::Output)?;
            if document.mode() != request.mode() {
                return Err(InteractiveError::Output);
            }
            let (frame, viewport) =
                DockFrame::layout_detail(&document, request.offset(), size.rows, size.columns)
                    .map_err(map_dock_error)?;
            Ok(EnhancedSurface {
                frame,
                commit: surface_commit(request, theme, viewport),
            })
        }
    }
}

fn surface_commit(
    request: ViewRequest,
    theme: ThemeRequest,
    viewport: DetailViewport,
) -> SurfaceCommit {
    SurfaceCommit {
        request,
        theme,
        offset: viewport.offset,
        total_rows: viewport.total_rows,
        page_rows: viewport.page_rows,
    }
}

fn commit_surface(view: &mut ViewState, theme: &mut ThemeState, commit: SurfaceCommit) {
    let _ = view.commit(
        commit.request,
        commit.offset,
        commit.total_rows,
        commit.page_rows,
    );
    let _ = theme.commit(commit.theme);
}

fn stage_surface(
    screen: &InlineScreen,
    size: TerminalSize,
    resized: bool,
    frame: &DockFrame,
    theme: ThemePalette,
) -> Result<PendingScreenWrite, InteractiveError> {
    let write = if screen.is_detached() {
        screen.stage_attach(screen_size(size), frame, theme)
    } else if resized {
        screen.stage_resize(screen_size(size), frame, theme)
    } else if screen.dock_rows() != Some(frame.rows().map_err(map_dock_error)?) {
        screen.stage_reanchor_bottom(frame, theme)
    } else {
        screen.stage_dock(frame, theme)
    };
    write.map_err(map_inline_screen_error)
}

fn map_inline_screen_error(error: InlineScreenError) -> InteractiveError {
    match error {
        InlineScreenError::TooSmall => InteractiveError::TerminalUnsupported,
        InlineScreenError::Capacity
        | InlineScreenError::Limit
        | InlineScreenError::InvalidState
        | InlineScreenError::Poisoned => InteractiveError::Output,
    }
}

const fn screen_size(size: TerminalSize) -> ScreenSize {
    ScreenSize {
        rows: size.rows,
        columns: size.columns,
    }
}

async fn write_enhanced_bytes(
    terminal: &AsyncTerminal,
    bytes: &[u8],
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, InteractiveError> {
    let mut written = 0_usize;
    // This helper is used only after the normal coordinate transaction has
    // already failed or been poisoned. A blocked terminal must not cost a
    // second full frame deadline before termios is restored.
    let deadline = Instant::now() + VISUAL_RESET_DEADLINE;
    while written < bytes.len() {
        let work = tokio::select! {
            biased;
            signal = signals.next() => IdleWriteWork::Signal(signal),
            () = tokio::time::sleep_until(deadline) => IdleWriteWork::Expired,
            write = terminal.write_once(&bytes[written..]) => IdleWriteWork::Write(write),
        };
        match work {
            IdleWriteWork::Signal(signal) => {
                return Ok(Some(signal));
            }
            IdleWriteWork::Expired | IdleWriteWork::Write(Err(_)) => {
                return Err(InteractiveError::Output);
            }
            IdleWriteWork::Write(Ok(count)) => {
                written = written
                    .checked_add(count)
                    .filter(|written| *written <= bytes.len())
                    .ok_or(InteractiveError::Output)?;
            }
        }
    }
    Ok(None)
}

async fn recover_poisoned_screen(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    screen: &mut InlineScreen,
) -> Result<Option<UiSignal>, InteractiveError> {
    if !screen.is_poisoned() {
        return Err(InteractiveError::Output);
    }
    if let Some(signal) = write_enhanced_bytes(terminal, POISON_REATTACH_BYTES, signals).await? {
        return Ok(Some(signal));
    }
    screen.recover_after_visual_reset();
    Ok(None)
}

async fn write_screen_transaction(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    screen: &mut InlineScreen,
    mut write: PendingScreenWrite,
) -> Result<ScreenWriteOutcome, InteractiveError> {
    let deadline = Instant::now() + FRAME_DEADLINE;
    while !write.is_complete() {
        let work = tokio::select! {
            biased;
            signal = signals.next_interactive() => signal,
            () = tokio::time::sleep_until(deadline) => {
                screen.abort(write);
                return Err(InteractiveError::Output);
            }
            result = terminal.write_once(write.bytes()) => {
                match result {
                    Ok(count) => {
                        write.advance(count).map_err(map_inline_screen_error)?;
                        continue;
                    }
                    Err(_) => {
                        screen.abort(write);
                        return Err(InteractiveError::Output);
                    }
                }
            }
        };
        screen.abort(write);
        return match work {
            // A partially written coordinate batch poisons the screen ledger,
            // but it must not erase the operating-system signal that caused us
            // to stop. The caller will use a coordinate-free visual reset and
            // restore termios before honoring that signal.
            InteractiveSignal::Stop(signal) => Ok(ScreenWriteOutcome::Signal(signal)),
            InteractiveSignal::Resize if screen.is_poisoned() => {
                Ok(ScreenWriteOutcome::PoisonedResize)
            }
            InteractiveSignal::Resize => Ok(ScreenWriteOutcome::Resize),
        };
    }
    screen.commit(write).map_err(map_inline_screen_error)?;
    Ok(ScreenWriteOutcome::Complete)
}

fn copy_enhanced_prompt(prompt: &str) -> Result<String, InteractiveError> {
    let mut copy = String::new();
    copy.try_reserve_exact(prompt.len())
        .map_err(|_| InteractiveError::Agent)?;
    copy.push_str(prompt);
    Ok(copy)
}

fn session_context_estimate(
    session: &crate::session::Session,
    sampled_after_turn: Option<TurnId>,
    expected_next_seq: Option<EventSeq>,
) -> Option<ContextEstimate> {
    let at_next_seq = session.next_seq()?;
    if expected_next_seq.is_some_and(|expected| expected != at_next_seq) {
        return None;
    }
    let used_tokens = session.context_total_tokens().ok()?;
    let context = session.request_context();
    ContextEstimate::new(
        at_next_seq,
        context.and_then(|context| context.provider()),
        context.and_then(|context| context.model()),
        used_tokens,
        context.and_then(|context| context.context_window().map(|value| value.get())),
        sampled_after_turn,
    )
    .ok()
}

async fn suspend_enhanced(
    terminal: &mut TerminalSession,
    signals: &mut SignalStreams,
    decoder: &mut KeyDecoder,
    model: DockRenderModel<'_>,
    dock: &mut ActiveDock<'_>,
) -> Result<Option<UiSignal>, InteractiveError> {
    if !dock.screen.is_detached() && !dock.screen.is_poisoned() {
        let write = dock
            .screen
            .stage_detach()
            .map_err(map_inline_screen_error)?;
        match write_screen_transaction(terminal.output_terminal(), signals, dock.screen, write)
            .await?
        {
            ScreenWriteOutcome::Complete => {}
            ScreenWriteOutcome::Signal(signal) => return Ok(Some(signal)),
            ScreenWriteOutcome::Resize | ScreenWriteOutcome::PoisonedResize => {
                // Even a zero-byte resize invalidates the old absolute Dock
                // coordinates. Clear the uncertain viewport before giving
                // the terminal back to the shell.
                match write_enhanced_bytes(
                    terminal.output_terminal(),
                    POISON_TEARDOWN_BYTES,
                    signals,
                )
                .await
                {
                    Ok(Some(signal)) => return Ok(Some(signal)),
                    Ok(None) => dock.screen.recover_after_visual_reset(),
                    Err(_) => terminal.best_effort_visual_reset(),
                }
            }
        }
    }
    if dock.screen.is_poisoned() {
        match write_enhanced_bytes(terminal.output_terminal(), POISON_TEARDOWN_BYTES, signals).await
        {
            Ok(Some(signal)) => return Ok(Some(signal)),
            Ok(None) => dock.screen.recover_after_visual_reset(),
            Err(_) => terminal.best_effort_visual_reset(),
        }
    }
    terminal.restore_for_suspend()?;
    loop {
        self_suspend().map_err(|_| InteractiveError::TerminalUnsupported)?;
        let mut latch = SignalLatch::default();
        signals.drain_ready(DriverMode::Interactive, &mut latch);
        if let Some(signal @ (UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate)) =
            latch.observed()
        {
            return Ok(Some(signal));
        }
        if terminal.is_foreground()? {
            terminal.reenter_after_resume()?;
            decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
            if dock.screen.is_poisoned() {
                if let Some(signal) =
                    recover_poisoned_screen(terminal.output_terminal(), signals, dock.screen)
                        .await?
                {
                    return Ok(Some(signal));
                }
            }
            return render_enhanced_dock(
                model,
                terminal,
                dock.last_size,
                signals,
                dock.screen,
                dock.view,
                dock.theme,
                dock.file_suggestions,
            )
            .await;
        }
    }
}

async fn run_linear(
    assembly: InteractiveAssembly,
    terminal: AsyncTerminal,
    signals: &mut SignalStreams,
    color: bool,
) -> Result<u8, InteractiveError> {
    let InteractiveAssembly {
        mut agent,
        mut events,
        mut approvals,
        mut joins,
        session_id,
        resumed,
        file_suggestions: _file_suggestions,
    } = assembly;
    let mut live = LiveRenderer::for_session(resumed);
    live.set_context_estimate(session_context_estimate(agent.session(), None, None));
    let mut presenter = InteractivePresenter::with_color(color);
    let mut parser = CanonicalRecordParser::new(MAX_INTERACTIVE_PROMPT_BYTES);
    let mut scratch = [0_u8; TERMINAL_READ_BYTES];

    let result: Result<InteractiveExit, InteractiveError> = async {
        let banner = LiveFrame::startup_banner(&session_id, resumed)
            .map_err(|_| InteractiveError::Output)?;
        if let Some(signal) = write_frame(banner, &mut presenter, &terminal, signals).await? {
            if let Some(signal) = handle_idle_signal(signal, &terminal, signals).await? {
                return Ok(InteractiveExit::Signal(signal));
            }
        }

        loop {
            terminal.revalidate()?;
            terminal.flush_input()?;
            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
            let prompt = LiveFrame::idle_prompt().map_err(|_| InteractiveError::Output)?;
            if let Some(signal) = write_frame(prompt, &mut presenter, &terminal, signals).await? {
                match handle_idle_signal(signal, &terminal, signals).await? {
                    Some(signal) => return Ok(InteractiveExit::Signal(signal)),
                    None => continue,
                }
            }
            terminal.revalidate()?;

            let input = loop {
                tokio::select! {
                    biased;
                    signal = signals.next() => break IdleEvent::Signal(signal),
                    read = terminal.read_once(&mut scratch) => {
                        let count = read.map_err(|_| InteractiveError::TerminalUnavailable)?;
                        if count == 0 {
                            break IdleEvent::Eof;
                        }
                        let mut first = None;
                        parser.feed(&scratch[..count], count < TERMINAL_READ_BYTES, |event| {
                            if first.is_none() {
                                first = Some(event);
                            }
                        });
                        if let Some(event) = first {
                            break IdleEvent::Record(event);
                        }
                    }
                }
            };

            // A signal may become ready after `select!` polled its stream but
            // before the terminal read completed. Sample again before treating
            // EOF or a record as success, and coalesce all ready signal classes.
            let mut latch = SignalLatch::default();
            if let IdleEvent::Signal(signal) = input {
                latch.observe(DriverMode::Interactive, signal);
            }
            tokio::task::yield_now().await;
            signals.drain_ready(DriverMode::Interactive, &mut latch);
            let input = match latch.observed() {
                Some(signal) => IdleEvent::Signal(signal),
                None => input,
            };

            match input {
                IdleEvent::Signal(signal) => {
                    presenter.discard_partly_written_frame();
                    match handle_idle_signal(signal, &terminal, signals).await? {
                        Some(signal) => return Ok(InteractiveExit::Signal(signal)),
                        None => continue,
                    }
                }
                IdleEvent::Eof => return Ok(InteractiveExit::Ordinary(0)),
                IdleEvent::Record(InputRecordEvent::TooLarge) => {
                    if let Some(signal) = write_notice(
                        "[input exceeds 1000 bytes]\n",
                        &mut presenter,
                        &terminal,
                        signals,
                    )
                    .await?
                    {
                        return Ok(InteractiveExit::Signal(signal));
                    }
                }
                IdleEvent::Record(InputRecordEvent::InvalidUtf8) => {
                    if let Some(signal) = write_notice(
                        "[input is not valid UTF-8]\n",
                        &mut presenter,
                        &terminal,
                        signals,
                    )
                    .await?
                    {
                        return Ok(InteractiveExit::Signal(signal));
                    }
                }
                IdleEvent::Record(InputRecordEvent::Record {
                    text,
                    terminated_by_lf,
                }) => match classify_idle_record(&text, terminated_by_lf) {
                    IdleInput::Redraw => {}
                    IdleInput::Help => {
                        let help = LiveFrame::help().map_err(|_| InteractiveError::Output)?;
                        if let Some(signal) =
                            write_frame(help, &mut presenter, &terminal, signals).await?
                        {
                            if let Some(signal) =
                                handle_idle_signal(signal, &terminal, signals).await?
                            {
                                return Ok(InteractiveExit::Signal(signal));
                            }
                        }
                    }
                    IdleInput::Inspect | IdleInput::Review => {
                        let document = if matches!(
                            classify_idle_record(&text, terminated_by_lf),
                            IdleInput::Inspect
                        ) {
                            live.inspect_document()
                        } else {
                            live.review_document()
                        }
                        .map_err(|_| InteractiveError::Output)?;
                        let frame = LiveFrame::detail_document(&document)
                            .map_err(|_| InteractiveError::Output)?;
                        if let Some(signal) =
                            write_frame(frame, &mut presenter, &terminal, signals).await?
                        {
                            if let Some(signal) =
                                handle_idle_signal(signal, &terminal, signals).await?
                            {
                                return Ok(InteractiveExit::Signal(signal));
                            }
                        }
                    }
                    IdleInput::Theme(command) => {
                        let message = if matches!(command, ThemeCommand::Invalid) {
                            "[unknown theme; linear UI remains plain]\n"
                        } else {
                            "[linear UI is always plain; theme command kept local]\n"
                        };
                        if let Some(signal) =
                            write_notice(message, &mut presenter, &terminal, signals).await?
                        {
                            if let Some(signal) =
                                handle_idle_signal(signal, &terminal, signals).await?
                            {
                                return Ok(InteractiveExit::Signal(signal));
                            }
                        }
                    }
                    IdleInput::Exit => return Ok(InteractiveExit::Ordinary(0)),
                    IdleInput::Submit(prompt) => {
                        parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                        let mut prompt_committed = false;
                        match run_turn(ActiveTurn {
                            agent: &mut agent,
                            events: &mut events,
                            approvals: &mut approvals,
                            joins: &mut joins,
                            live: &mut live,
                            presenter: &mut presenter,
                            terminal: &terminal,
                            panic_restore: None,
                            signals,
                            parser: &mut parser,
                            scratch: &mut scratch,
                            prompt,
                            prompt_committed: &mut prompt_committed,
                            queued_input: None,
                            queue_notice: None,
                            enhanced_decoder: None,
                            active_dock: None,
                            enhanced_presenter: None,
                            color,
                            enhanced: false,
                        })
                        .await?
                        {
                            TurnDisposition::Continue => {}
                            TurnDisposition::Exit(code) => {
                                return Ok(InteractiveExit::Ordinary(code));
                            }
                            TurnDisposition::Signal(signal) => {
                                return Ok(InteractiveExit::Signal(signal));
                            }
                        }
                    }
                },
            }
        }
    }
    .await;
    let initial_signal = result.as_ref().ok().and_then(|exit| match exit {
        InteractiveExit::Signal(signal) => Some(*signal),
        InteractiveExit::Ordinary(_) => None,
    });
    let (shutdown, signal) =
        shutdown::agent_with_signals(&mut agent, DriverMode::Interactive, signals, initial_signal)
            .await;
    if let Some(signal) = signal {
        if let Some(code) = finish_signal_after_shutdown(signal, &terminal, signals).await? {
            return Ok(code);
        }
    }
    match (result, shutdown) {
        (Err(InteractiveError::Agent), Err(error)) => match error.session_error() {
            Some(error) => Err(InteractiveError::Storage(storage_failure::from_shutdown(
                error,
            ))),
            None => Err(InteractiveError::Agent),
        },
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => match error.session_error() {
            Some(error) => Err(InteractiveError::Storage(storage_failure::from_shutdown(
                error,
            ))),
            None => Err(InteractiveError::Agent),
        },
        (Ok(InteractiveExit::Ordinary(exit)), Ok(())) => Ok(exit),
        (Ok(InteractiveExit::Signal(_)), Ok(())) => Err(InteractiveError::Agent),
    }
}

enum IdleEvent {
    Signal(UiSignal),
    Eof,
    Record(InputRecordEvent),
}

struct ActiveTurn<'a> {
    agent: &'a mut AgentLoop,
    events: &'a mut CommittedUiReceiver,
    approvals: &'a mut ApprovalEnvelopeReceiver,
    joins: &'a mut ApprovalJoin,
    live: &'a mut LiveRenderer,
    presenter: &'a mut InteractivePresenter,
    terminal: &'a AsyncTerminal,
    panic_restore: Option<TerminalPanicRestore<'a>>,
    signals: &'a mut SignalStreams,
    parser: &'a mut CanonicalRecordParser,
    scratch: &'a mut [u8; TERMINAL_READ_BYTES],
    prompt: String,
    prompt_committed: &'a mut bool,
    queued_input: Option<&'a mut InputMemory>,
    queue_notice: Option<&'a mut Option<String>>,
    enhanced_decoder: Option<&'a mut KeyDecoder>,
    active_dock: Option<ActiveDock<'a>>,
    enhanced_presenter: Option<&'a mut EnhancedPresenter>,
    color: bool,
    enhanced: bool,
}

struct ActiveDock<'a> {
    screen: &'a mut InlineScreen,
    last_size: &'a mut TerminalSize,
    view: &'a mut ViewState,
    theme: &'a mut ThemeState,
    command_palette: &'a mut CommandPaletteState,
    file_suggestions: &'a mut FileSuggestionController,
    palette_suppressed: bool,
}

#[derive(Clone, Copy)]
struct DockRenderModel<'a> {
    input: &'a InputMemory,
    notice: Option<&'a str>,
    interaction: DockInteraction,
    command_palette: CommandPaletteSnapshot,
    live: &'a LiveRenderer,
}

enum ApprovalUiState<'a> {
    Inactive,
    Arming {
        deadline: Instant,
    },
    Rendering {
        mode: Option<ApprovalTerminalMode<'a>>,
        selector: ApprovalSelector,
        compact: bool,
    },
    Accepting {
        mode: Option<ApprovalTerminalMode<'a>>,
        selector: ApprovalSelector,
        compact: bool,
        escape_deadline: Option<Instant>,
    },
}

enum ApprovalUiUpdate {
    None,
    Redraw(String),
    Decide(ApprovalOutcome),
    Eof,
    Invalid,
}

impl<'a> ApprovalUiState<'a> {
    const fn new() -> Self {
        Self::Inactive
    }

    const fn is_inactive(&self) -> bool {
        matches!(self, Self::Inactive)
    }

    const fn is_accepting(&self) -> bool {
        matches!(self, Self::Accepting { .. })
    }

    const fn suppresses_read_while_pending(&self) -> bool {
        matches!(self, Self::Rendering { .. } | Self::Accepting { .. })
    }

    const fn arm_deadline(&self) -> Option<Instant> {
        match self {
            Self::Arming { deadline } => Some(*deadline),
            _ => None,
        }
    }

    const fn escape_deadline(&self) -> Option<Instant> {
        match self {
            Self::Accepting {
                escape_deadline, ..
            } => *escape_deadline,
            _ => None,
        }
    }

    fn begin_arming(&mut self) -> Result<(), InteractiveError> {
        if !self.is_inactive() {
            return Err(InteractiveError::Agent);
        }
        *self = Self::Arming {
            deadline: Instant::now() + APPROVAL_INPUT_QUIET,
        };
        Ok(())
    }

    fn observe_unaccepted_input(&mut self) {
        if let Self::Arming { deadline } = self {
            *deadline = Instant::now() + APPROVAL_INPUT_QUIET;
        }
    }

    fn begin_rendering(
        &mut self,
        terminal: &'a AsyncTerminal,
        color: bool,
        enhanced: bool,
    ) -> Result<String, InteractiveError> {
        if !matches!(self, Self::Arming { .. }) {
            return Err(InteractiveError::Agent);
        }
        terminal.flush_input()?;
        let compact = terminal.columns().is_none_or(|columns| columns < 48);
        let mode = if enhanced {
            terminal.revalidate_identity()?;
            None
        } else {
            Some(terminal.enter_approval_mode()?)
        };
        let profile = if enhanced {
            ApprovalInputProfile::EnhancedDirectional
        } else {
            ApprovalInputProfile::LinearRecord
        };
        let selector = ApprovalSelector::new(profile).map_err(|_| InteractiveError::Agent)?;
        let output = if enhanced {
            String::new()
        } else {
            selector
                .render(color, compact, false)
                .map_err(|_| InteractiveError::Output)?
        };
        *self = Self::Rendering {
            mode,
            selector,
            compact,
        };
        Ok(output)
    }

    fn accept_rendered(&mut self, terminal: &AsyncTerminal) -> Result<(), InteractiveError> {
        let state = std::mem::replace(self, Self::Inactive);
        let Self::Rendering {
            mode,
            selector,
            compact,
        } = state
        else {
            *self = state;
            return Err(InteractiveError::Agent);
        };
        terminal.flush_input()?;
        *self = Self::Accepting {
            mode,
            selector,
            compact,
            escape_deadline: None,
        };
        Ok(())
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        challenge: uuid::Uuid,
        color: bool,
        enhanced: bool,
    ) -> Result<ApprovalUiUpdate, InteractiveError> {
        let state = std::mem::replace(self, Self::Inactive);
        let Self::Accepting {
            mode,
            mut selector,
            compact,
            ..
        } = state
        else {
            *self = state;
            return Ok(ApprovalUiUpdate::None);
        };
        let update = selector.feed(bytes, challenge);
        match update {
            SelectorUpdate::None => {
                let escape_deadline = selector
                    .escape_is_pending()
                    .then(|| Instant::now() + ESCAPE_SEQUENCE_WAIT);
                *self = Self::Accepting {
                    mode,
                    selector,
                    compact,
                    escape_deadline,
                };
                Ok(ApprovalUiUpdate::None)
            }
            SelectorUpdate::Redraw => {
                let output = if enhanced {
                    String::new()
                } else {
                    selector
                        .render(color, compact, color && !compact)
                        .map_err(|_| InteractiveError::Output)?
                };
                *self = Self::Accepting {
                    mode,
                    selector,
                    compact,
                    escape_deadline: None,
                };
                Ok(ApprovalUiUpdate::Redraw(output))
            }
            SelectorUpdate::Decide(outcome) => {
                if let Some(mode) = mode {
                    mode.restore()?;
                }
                Ok(ApprovalUiUpdate::Decide(outcome))
            }
            SelectorUpdate::Eof => {
                if let Some(mode) = mode {
                    mode.restore()?;
                }
                Ok(ApprovalUiUpdate::Eof)
            }
            SelectorUpdate::Invalid => {
                if let Some(mode) = mode {
                    mode.restore()?;
                }
                Ok(ApprovalUiUpdate::Invalid)
            }
        }
    }

    fn expire_escape(&mut self) -> Result<ApprovalUiUpdate, InteractiveError> {
        let state = std::mem::replace(self, Self::Inactive);
        let Self::Accepting {
            mode,
            mut selector,
            compact,
            ..
        } = state
        else {
            *self = state;
            return Ok(ApprovalUiUpdate::None);
        };
        match selector.expire_escape() {
            SelectorUpdate::Decide(outcome) => {
                if let Some(mode) = mode {
                    mode.restore()?;
                }
                Ok(ApprovalUiUpdate::Decide(outcome))
            }
            SelectorUpdate::None => {
                *self = Self::Accepting {
                    mode,
                    selector,
                    compact,
                    escape_deadline: None,
                };
                Ok(ApprovalUiUpdate::None)
            }
            SelectorUpdate::Redraw | SelectorUpdate::Eof | SelectorUpdate::Invalid => {
                Err(InteractiveError::Agent)
            }
        }
    }

    fn restore(&mut self) -> Result<(), InteractiveError> {
        let state = std::mem::replace(self, Self::Inactive);
        match state {
            Self::Rendering { mode, .. } | Self::Accepting { mode, .. } => {
                mode.map_or(Ok(()), |mode| mode.restore().map_err(Into::into))
            }
            Self::Inactive | Self::Arming { .. } => Ok(()),
        }
    }

    fn dock_selection(&self) -> Result<DockApprovalSelection, InteractiveError> {
        let selector = match self {
            Self::Rendering { selector, .. } | Self::Accepting { selector, .. } => selector,
            Self::Inactive | Self::Arming { .. } => return Err(InteractiveError::Agent),
        };
        Ok(match selector.selected() {
            super::approval_selector::ApprovalSelection::AllowOnce => {
                DockApprovalSelection::AllowOnce
            }
            super::approval_selector::ApprovalSelection::Reject => DockApprovalSelection::Reject,
            super::approval_selector::ApprovalSelection::Cancel => DockApprovalSelection::Cancel,
        })
    }

    fn dock_interaction(&self) -> DockInteraction {
        match self {
            Self::Rendering { selector, .. } | Self::Accepting { selector, .. } => {
                DockInteraction::Approval(match selector.selected() {
                    super::approval_selector::ApprovalSelection::AllowOnce => {
                        DockApprovalSelection::AllowOnce
                    }
                    super::approval_selector::ApprovalSelection::Reject => {
                        DockApprovalSelection::Reject
                    }
                    super::approval_selector::ApprovalSelection::Cancel => {
                        DockApprovalSelection::Cancel
                    }
                })
            }
            Self::Inactive | Self::Arming { .. } => DockInteraction::Running,
        }
    }
}

async fn next_turn_signal(signals: &mut SignalStreams, enhanced: bool) -> InteractiveSignal {
    if enhanced {
        signals.next_interactive().await
    } else {
        InteractiveSignal::Stop(signals.next().await)
    }
}

async fn redraw_active_after_resize(
    enhanced: bool,
    live: &LiveRenderer,
    input: Option<&InputMemory>,
    notice: Option<&str>,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    dock: Option<&mut ActiveDock<'_>>,
) -> Result<Option<UiSignal>, InteractiveError> {
    if !enhanced {
        return Ok(None);
    }
    render_active_dock(
        input.ok_or(InteractiveError::Agent)?,
        notice,
        DockInteraction::Running,
        live,
        terminal,
        signals,
        dock.ok_or(InteractiveError::Agent)?,
    )
    .await
}

fn prepare_pending_for_resize(
    pending: &mut Option<PendingOutput>,
    screen: &mut InlineScreen,
) -> Result<bool, InteractiveError> {
    if pending.as_ref().is_some_and(PendingOutput::has_started)
        && !matches!(
            pending.as_ref(),
            Some(PendingOutput::Inline(PendingInlineOutput {
                intent: InlineIntent::Dock(_),
                ..
            }))
        )
    {
        return Err(InteractiveError::Output);
    }
    let Some(output) = pending.take() else {
        return Ok(false);
    };
    let mut recover_visual_state = false;
    *pending = Some(match output {
        PendingOutput::Inline(output) => {
            recover_visual_state = output.write.has_started();
            screen.abort(output.write);
            if screen.is_poisoned() != recover_visual_state {
                return Err(InteractiveError::Output);
            }
            output.intent.into_pending()
        }
        output => output,
    });
    Ok(recover_visual_state)
}

#[allow(clippy::too_many_arguments)]
async fn reconcile_active_geometry(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    input: Option<&InputMemory>,
    notice: Option<&str>,
    interaction: DockInteraction,
    live: &LiveRenderer,
    dock: Option<&mut ActiveDock<'_>>,
    pending: &mut Option<PendingOutput>,
    presenter: Option<&mut EnhancedPresenter>,
) -> Result<Option<UiSignal>, InteractiveError> {
    let Some(dock) = dock else {
        return Ok(None);
    };
    let size = terminal.size().unwrap_or(*dock.last_size);
    if size == *dock.last_size {
        return Ok(None);
    }
    dock.file_suggestions.invalidate_presentation();
    if prepare_pending_for_resize(pending, dock.screen)? {
        if let Some(signal) = recover_poisoned_screen(terminal, signals, dock.screen).await? {
            return Ok(Some(signal));
        }
    }
    let signal = render_active_dock(
        input.ok_or(InteractiveError::Agent)?,
        notice,
        interaction,
        live,
        terminal,
        signals,
        dock,
    )
    .await?;
    if signal.is_none() {
        if let Some(PendingOutput::Prepared(presentation)) = pending.as_mut() {
            presentation.force_next_line_boundary();
        }
        if let Some(presenter) = presenter {
            presenter.force_line_boundary();
        }
    }
    Ok(signal)
}

fn approval_owns_active_input(joins: &ApprovalJoin, approval_ui: &ApprovalUiState<'_>) -> bool {
    joins.question().is_some() || !approval_ui.is_inactive()
}

async fn wait_active_file_suggestion(
    dock: Option<&mut ActiveDock<'_>>,
) -> Result<JobSettlement, tokio::task::JoinError> {
    match dock {
        Some(dock) => dock.file_suggestions.wait_job().await,
        None => std::future::pending().await,
    }
}

async fn run_turn(mut active: ActiveTurn<'_>) -> Result<TurnDisposition, InteractiveError> {
    let prepared = prepare_user_turn(active.agent.session(), &active.prompt)
        .map_err(|_| InteractiveError::Agent)?;
    let start_seq = prepared.start_seq;
    let turn = prepared.turn;
    active.joins.begin_turn()?;
    active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
    let cancellation = CancellationToken::new();
    let mut pending = None;
    let mut frame_deadline = None;
    let mut after_frame = AfterFrame::None;
    let mut approval_ui = ApprovalUiState::new();
    let mut turn_end_seen = false;
    let mut turn_end_rendered = false;
    let mut stop = None;
    let mut prefer_input = true;
    let mut dock_redraw_requested = false;
    let mut input_escape_deadline = None;

    let result = {
        let future = active
            .agent
            .run_turn(prepared.proposal, cancellation.clone());
        tokio::pin!(future);
        let ui_result = std::panic::AssertUnwindSafe(async {
            loop {
            if let Some(dock) = active.active_dock.as_mut() {
                dock.palette_suppressed =
                    active.joins.question().is_some() || !approval_ui.is_inactive();
                let input = active
                    .queued_input
                    .as_deref()
                    .ok_or(InteractiveError::Agent)?;
                let _ = dock
                    .file_suggestions
                    .sync(
                        input.composer(),
                        dock.view.requested().mode() != ViewMode::Focus,
                        dock.palette_suppressed,
                    )
                    .map_err(|_| InteractiveError::Agent)?;
                reset_file_suggestion_decoder(
                    dock.file_suggestions,
                    active.enhanced_decoder.as_deref_mut(),
                    &mut input_escape_deadline,
                )?;
            }
            if latch_observer_fault(active.events, &mut stop, &cancellation) {
                discard_pending(&mut pending, active.presenter);
            }
            if stop.is_some() {
                if let Err(error) = approval_ui.restore() {
                    observe_failure(&mut stop, error);
                    cancellation.cancel();
                }
                tokio::select! {
                    biased;
                    result = &mut future => break Ok(result),
                    signal = active.signals.next() => {
                        observe_signal(&mut stop, signal);
                        cancellation.cancel();
                    }
                }
                continue;
            }

            match reconcile_active_geometry(
                active.terminal,
                active.signals,
                active.queued_input.as_deref(),
                active.queue_notice.as_deref().and_then(Option::as_deref),
                approval_ui.dock_interaction(),
                active.live,
                active.active_dock.as_mut(),
                &mut pending,
                active.enhanced_presenter.as_deref_mut(),
            )
            .await
            {
                Ok(Some(signal)) => {
                    observe_signal(&mut stop, signal);
                    cancellation.cancel();
                    discard_pending(&mut pending, active.presenter);
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    latch_active_failure(
                        &mut stop,
                        &cancellation,
                        &mut pending,
                        active.presenter,
                        error,
                    );
                    continue;
                }
            }

            if let Err(error) = complete_ready_frame(
                &mut pending,
                &mut frame_deadline,
                &mut after_frame,
                &mut approval_ui,
                &mut turn_end_rendered,
                active.presenter,
                active.live,
                active.terminal,
                active.parser,
                active.enhanced,
                active.enhanced_presenter.as_deref_mut(),
                active.queued_input.as_deref(),
                active.queue_notice.as_deref().and_then(Option::as_deref),
                active.active_dock.as_mut(),
            ) {
                latch_active_failure(
                    &mut stop,
                    &cancellation,
                    &mut pending,
                    active.presenter,
                    error,
                );
                continue;
            }
            if pending.is_none() && mem::take(&mut dock_redraw_requested) {
                match redraw_active_after_resize(
                    active.enhanced,
                    active.live,
                    active.queued_input.as_deref(),
                    active.queue_notice.as_deref().and_then(Option::as_deref),
                    active.terminal,
                    active.signals,
                    active.active_dock.as_mut(),
                )
                .await
                {
                    Ok(Some(signal)) => {
                        observe_signal(&mut stop, signal);
                        cancellation.cancel();
                    }
                    Ok(None) => {}
                    Err(error) => {
                        observe_failure(&mut stop, error);
                        cancellation.cancel();
                    }
                }
                if stop.is_some() {
                    continue;
                }
            }
            if pending.is_none() && active.joins.question().is_some() && approval_ui.is_inactive() {
                if active.enhanced {
                    active
                        .enhanced_decoder
                        .as_deref_mut()
                        .ok_or(InteractiveError::Agent)?
                        .reset_epoch()
                        .map_err(|_| InteractiveError::Agent)?;
                    input_escape_deadline = None;
                    let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                    let _ = dock
                        .view
                        .request_mode(ViewMode::Focus)
                        .map_err(|_| InteractiveError::Output)?;
                    match render_active_dock(
                        active
                            .queued_input
                            .as_deref()
                            .ok_or(InteractiveError::Agent)?,
                        active.queue_notice.as_deref().and_then(Option::as_deref),
                        DockInteraction::Running,
                        active.live,
                        active.terminal,
                        active.signals,
                        dock,
                    )
                    .await
                    {
                        Ok(Some(signal)) => {
                            observe_signal(&mut stop, signal);
                            cancellation.cancel();
                            continue;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            latch_active_failure(
                                &mut stop,
                                &cancellation,
                                &mut pending,
                                active.presenter,
                                error,
                            );
                            continue;
                        }
                    }
                }
                let enqueue = approval_frame(active.joins, false).and_then(|frame| {
                    enqueue_frame(
                        frame,
                        AfterFrame::ApprovalFence,
                        &mut pending,
                        &mut frame_deadline,
                        &mut after_frame,
                    )
                });
                if let Err(error) = enqueue {
                    latch_active_failure(
                        &mut stop,
                        &cancellation,
                        &mut pending,
                        active.presenter,
                        error,
                    );
                    continue;
                }
            }
            // A freshly enqueued frame has no rendered bytes yet. Return to
            // `complete_ready_frame` before polling terminal writability;
            // writing an empty slice would look like a fatal WriteZero.
            if pending
                .as_ref()
                .is_some_and(|frame| frame.bytes().is_empty())
            {
                continue;
            }

            let work = next_ui_work(
                active.terminal,
                active.approvals,
                active.events,
                active.scratch,
                pending.as_ref(),
                frame_deadline,
                approval_ui.arm_deadline(),
                approval_ui.escape_deadline(),
                input_escape_deadline,
                !(pending.is_some() && approval_ui.suppresses_read_while_pending()),
                prefer_input,
            );
            let suggestion_running = active
                .active_dock
                .as_ref()
                .is_some_and(|dock| dock.file_suggestions.has_job());
            tokio::select! {
                biased;
                signal = next_turn_signal(active.signals, active.enhanced) => {
                    match signal {
                        InteractiveSignal::Stop(signal) => {
                            observe_signal(&mut stop, signal);
                            cancellation.cancel();
                            discard_pending(&mut pending, active.presenter);
                        }
                        InteractiveSignal::Resize => {
                            match reconcile_active_geometry(
                                active.terminal,
                                active.signals,
                                active.queued_input.as_deref(),
                                active
                                    .queue_notice
                                    .as_deref()
                                    .and_then(Option::as_deref),
                                approval_ui.dock_interaction(),
                                active.live,
                                active.active_dock.as_mut(),
                                &mut pending,
                                active.enhanced_presenter.as_deref_mut(),
                            )
                            .await
                            {
                                Ok(Some(signal)) => {
                                    observe_signal(&mut stop, signal);
                                    cancellation.cancel();
                                    discard_pending(&mut pending, active.presenter);
                                }
                                Ok(None) => {}
                                Err(error) => latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                ),
                            }
                        }
                    }
                }
                result = &mut future => break Ok(result),
                settlement = wait_active_file_suggestion(active.active_dock.as_mut()), if suggestion_running => {
                    let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                    let _ = dock
                        .file_suggestions
                        .accept_job(settlement)
                        .map_err(|_| InteractiveError::Agent)?;
                    dock_redraw_requested = true;
                }
                work = work => {
                    prefer_input = !prefer_input;
                    match work {
                        UiWork::FrameExpired => latch_active_failure(
                            &mut stop,
                            &cancellation,
                            &mut pending,
                            active.presenter,
                            InteractiveError::Output,
                        ),
                        UiWork::ApprovalArmed => {
                            let prepared = approval_ui
                                .begin_rendering(active.terminal, active.color, active.enhanced)
                                .and_then(|output| {
                                    enqueue_approval_selector_surface(
                                        output,
                                        AfterFrame::ApprovalAccepting,
                                        &approval_ui,
                                        active.enhanced,
                                        &mut pending,
                                        &mut frame_deadline,
                                        &mut after_frame,
                                    )
                                });
                            if let Err(error) = prepared {
                                latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                );
                            }
                        }
                        UiWork::EscapeExpired => {
                            let handled = approval_ui.expire_escape().and_then(|update| {
                                dispatch_approval_update(
                                    update,
                                    active.joins,
                                    active.parser,
                                    &approval_ui,
                                    active.enhanced,
                                    &mut pending,
                                    &mut frame_deadline,
                                    &mut after_frame,
                                )
                            });
                            match handled {
                                Ok(false) => {}
                                Ok(true) => {
                                    stop = Some(StopIntent::Eof);
                                    cancellation.cancel();
                                    discard_pending(&mut pending, active.presenter);
                                }
                                Err(error) => latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                ),
                            }
                        }
                        UiWork::InputEscapeExpired => {
                            input_escape_deadline = None;
                            if approval_owns_active_input(active.joins, &approval_ui) {
                                if let Some(decoder) = active.enhanced_decoder.as_deref_mut() {
                                    decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                                }
                            } else {
                                match handle_active_escape_expiry(
                                    active.terminal,
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    active.active_dock.as_mut(),
                                    active.queue_notice.as_deref_mut(),
                                )? {
                                    ActiveInputOutcome::Continue => {}
                                    ActiveInputOutcome::Redraw => dock_redraw_requested = true,
                                    ActiveInputOutcome::PasteFence | ActiveInputOutcome::Eof => {
                                        latch_active_failure(
                                            &mut stop,
                                            &cancellation,
                                            &mut pending,
                                            active.presenter,
                                            InteractiveError::Agent,
                                        );
                                    }
                                }
                            }
                        }
                        UiWork::Write(write) => match write {
                        Ok(count) => {
                            if count != 0 {
                                if let Some(dock) = active.active_dock.as_mut() {
                                    dock.file_suggestions.invalidate_presentation();
                                }
                            }
                            let advanced = pending
                                .as_mut()
                                .ok_or(InteractiveError::Agent)
                                .and_then(|frame| frame.advance(count));
                            if let Err(error) = advanced {
                                latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                );
                            }
                        }
                        Err(_) => {
                            if let Some(dock) = active.active_dock.as_mut() {
                                dock.file_suggestions.invalidate_presentation();
                            }
                            latch_active_failure(
                                &mut stop,
                                &cancellation,
                                &mut pending,
                                active.presenter,
                                InteractiveError::Output,
                            );
                        }
                        },
                        UiWork::Envelope(envelope) => {
                            let received = envelope
                        .ok_or(InteractiveError::Agent)
                        .and_then(|envelope| active.joins.receive_envelope(envelope).map_err(Into::into));
                            if let Err(error) = received {
                                latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                );
                            }
                        }
                        UiWork::Event(event) => {
                            let processed = event.ok_or(InteractiveError::Agent).and_then(|event| {
                                process_event(
                                    event,
                                    start_seq,
                                    turn,
                                    active.live,
                                    active.joins,
                                    EventTargets {
                                        pending: &mut pending,
                                        frame_deadline: &mut frame_deadline,
                                        after_frame: &mut after_frame,
                                        approval_ui: &mut approval_ui,
                                        turn_end_seen: &mut turn_end_seen,
                                        prompt_committed: active.prompt_committed,
                                        expected_prompt: &active.prompt,
                                        render_committed_prompt: active.enhanced,
                                        detail_view_requested: active.active_dock.as_ref().is_some_and(
                                            |dock| dock.view.requested().mode() != ViewMode::Focus,
                                        ),
                                        dock_notice: active.queue_notice.as_deref_mut(),
                                        dock_redraw_requested: active
                                            .enhanced
                                            .then_some(&mut dock_redraw_requested),
                                    },
                                )
                            });
                            if let Err(error) = processed {
                                latch_active_failure(
                                    &mut stop,
                                    &cancellation,
                                    &mut pending,
                                    active.presenter,
                                    error,
                                );
                            }
                        }
                        UiWork::Read(read) => match read {
                        Ok(0) => {
                            stop = Some(StopIntent::Eof);
                            cancellation.cancel();
                            discard_pending(&mut pending, active.presenter);
                        }
                        Ok(count) => {
                            tokio::task::yield_now().await;
                            drain_active_signals(active.signals, &mut stop);
                            latch_observer_fault(active.events, &mut stop, &cancellation);
                            if stop.is_some() {
                                cancellation.cancel();
                                discard_pending(&mut pending, active.presenter);
                            } else if approval_ui.is_accepting() {
                                let handled = active
                                    .joins
                                    .question()
                                    .ok_or(InteractiveError::Agent)
                                    .and_then(|question| {
                                        approval_ui.feed(
                                            &active.scratch[..count],
                                            question.challenge(),
                                            active.color,
                                            active.enhanced,
                                        )
                                    })
                                    .and_then(|update| {
                                        dispatch_approval_update(
                                            update,
                                            active.joins,
                                            active.parser,
                                            &approval_ui,
                                            active.enhanced,
                                            &mut pending,
                                            &mut frame_deadline,
                                            &mut after_frame,
                                        )
                                    });
                                match handled {
                                    Ok(false) => {}
                                    Ok(true) => {
                                        stop = Some(StopIntent::Eof);
                                        cancellation.cancel();
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                    Err(error) => latch_active_failure(
                                        &mut stop,
                                        &cancellation,
                                        &mut pending,
                                        active.presenter,
                                        error,
                                    ),
                                }
                            } else if !approval_owns_active_input(active.joins, &approval_ui) {
                                let input_outcome = handle_active_input(
                                    active.terminal,
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    active.active_dock.as_mut(),
                                    active.queue_notice.as_deref_mut(),
                                    &active.scratch[..count],
                                )?;
                                if let Some(decoder) = active.enhanced_decoder.as_deref() {
                                    refresh_decoder_escape_deadline(
                                        decoder,
                                        &mut input_escape_deadline,
                                    );
                                }
                                match input_outcome {
                                    ActiveInputOutcome::Continue => {}
                                    ActiveInputOutcome::Eof => {
                                        stop = Some(StopIntent::Eof);
                                        cancellation.cancel();
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                    ActiveInputOutcome::Redraw
                                    | ActiveInputOutcome::PasteFence => {
                                        let paste_fence =
                                            input_outcome == ActiveInputOutcome::PasteFence;
                                        if pending.is_some() {
                                            dock_redraw_requested = true;
                                        } else if let (Some(input), Some(dock)) = (
                                            active.queued_input.as_deref(),
                                            active.active_dock.as_mut(),
                                        ) {
                                            match render_active_dock(
                                                input,
                                                active
                                                    .queue_notice
                                                    .as_deref()
                                                    .and_then(Option::as_deref),
                                                DockInteraction::Running,
                                                active.live,
                                                active.terminal,
                                                active.signals,
                                                dock,
                                            )
                                            .await
                                            {
                                                Ok(Some(signal)) => {
                                                    observe_signal(&mut stop, signal);
                                                    cancellation.cancel();
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                                Ok(None) => {}
                                                Err(error) => latch_active_failure(
                                                    &mut stop,
                                                    &cancellation,
                                                    &mut pending,
                                                    active.presenter,
                                                    error,
                                                ),
                                            }
                                        }
                                        if paste_fence && stop.is_none() {
                                            match complete_paste_input_fence(
                                                active.terminal,
                                                active.signals,
                                                active.scratch,
                                            )
                                            .await?
                                            {
                                                PasteFenceOutcome::Ready => {
                                                    active
                                                        .enhanced_decoder
                                                        .as_deref_mut()
                                                        .ok_or(InteractiveError::Agent)?
                                                        .reset_epoch()
                                                        .map_err(|_| InteractiveError::Agent)?;
                                                    input_escape_deadline = None;
                                                    *active
                                                        .queue_notice
                                                        .as_deref_mut()
                                                        .ok_or(InteractiveError::Agent)? =
                                                        Some("Paste ready · Enter sends".to_owned());
                                                    dock_redraw_requested = true;
                                                }
                                                PasteFenceOutcome::Signal(signal) => {
                                                    observe_signal(&mut stop, signal);
                                                    cancellation.cancel();
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                                PasteFenceOutcome::Eof => {
                                                    stop = Some(StopIntent::Eof);
                                                    cancellation.cancel();
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                approval_ui.observe_unaccepted_input();
                            }
                        }
                        Err(_) => {
                            observe_failure(&mut stop, InteractiveError::TerminalUnavailable);
                            cancellation.cancel();
                            discard_pending(&mut pending, active.presenter);
                        }
                        },
                    }
                }
            }
        }
        })
        .catch_unwind()
        .await;
        match ui_result {
            Ok(Ok(result)) => result,
            failed => {
                let error = match failed {
                    Ok(Err(error)) => error,
                    Err(_) => InteractiveError::Agent,
                    Ok(Ok(_)) => return Err(InteractiveError::Agent),
                };
                cancellation.cancel();
                discard_pending(&mut pending, active.presenter);
                if let Some(dock) = active.active_dock.as_mut() {
                    dock.file_suggestions.invalidate_presentation();
                    dock.file_suggestions.cancel_for_shutdown();
                }
                let _ = approval_ui.restore();
                if let Some(restorer) = active.panic_restore.as_ref() {
                    restorer.restore_now();
                }
                // The Agent future has already been polled. It and any owned
                // scanner/filter must both settle before this function may
                // return, otherwise the append-only Session can keep an open
                // turn tail or a blocking job can become detached.
                if let Some(dock) = active.active_dock.as_mut() {
                    let _ = tokio::join!((&mut future), dock.file_suggestions.finish_shutdown());
                } else {
                    let _ = (&mut future).await;
                }
                active
                    .joins
                    .finish_turn(active.approvals, ApprovalResetMode::Discard)?;
                return Err(error);
            }
        }
    };

    tokio::task::yield_now().await;
    drain_active_signals(active.signals, &mut stop);
    if latch_observer_fault(active.events, &mut stop, &cancellation) {
        discard_pending(&mut pending, active.presenter);
    }
    if let Err(error) = approval_ui.restore() {
        observe_failure(&mut stop, error);
    }
    let session_capacity_exhausted = match &result {
        Ok(outcome) => turn_exhausted_session_capacity(outcome.reason()),
        Err(_) => false,
    };
    match &result {
        Ok(outcome) if outcome.turn() == turn => {
            if outcome.turn_end_seq().get() < start_seq.get() {
                observe_failure(&mut stop, InteractiveError::Agent);
            }
        }
        Ok(_) => observe_failure(&mut stop, InteractiveError::Agent),
        Err(error) => observe_failure(
            &mut stop,
            storage_failure::from_agent(error)
                .map_or(InteractiveError::Agent, InteractiveError::Storage),
        ),
    }

    if stop.is_none() {
        let final_deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            if let Some(dock) = active.active_dock.as_mut() {
                dock.palette_suppressed =
                    active.joins.question().is_some() || !approval_ui.is_inactive();
                reset_file_suggestion_decoder(
                    dock.file_suggestions,
                    active.enhanced_decoder.as_deref_mut(),
                    &mut input_escape_deadline,
                )?;
            }
            drain_active_signals(active.signals, &mut stop);
            if latch_observer_fault(active.events, &mut stop, &cancellation) {
                discard_pending(&mut pending, active.presenter);
            }
            if stop.is_some() {
                discard_pending(&mut pending, active.presenter);
                break;
            }
            match reconcile_active_geometry(
                active.terminal,
                active.signals,
                active.queued_input.as_deref(),
                active.queue_notice.as_deref().and_then(Option::as_deref),
                approval_ui.dock_interaction(),
                active.live,
                active.active_dock.as_mut(),
                &mut pending,
                active.enhanced_presenter.as_deref_mut(),
            )
            .await
            {
                Ok(Some(signal)) => {
                    observe_signal(&mut stop, signal);
                    discard_pending(&mut pending, active.presenter);
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    observe_failure(&mut stop, error);
                    discard_pending(&mut pending, active.presenter);
                    continue;
                }
            }
            if let Err(error) = complete_ready_frame(
                &mut pending,
                &mut frame_deadline,
                &mut after_frame,
                &mut approval_ui,
                &mut turn_end_rendered,
                active.presenter,
                active.live,
                active.terminal,
                active.parser,
                active.enhanced,
                active.enhanced_presenter.as_deref_mut(),
                active.queued_input.as_deref(),
                active.queue_notice.as_deref().and_then(Option::as_deref),
                active.active_dock.as_mut(),
            ) {
                observe_failure(&mut stop, error);
                discard_pending(&mut pending, active.presenter);
                continue;
            }
            if pending.is_none() && mem::take(&mut dock_redraw_requested) {
                match redraw_active_after_resize(
                    active.enhanced,
                    active.live,
                    active.queued_input.as_deref(),
                    active.queue_notice.as_deref().and_then(Option::as_deref),
                    active.terminal,
                    active.signals,
                    active.active_dock.as_mut(),
                )
                .await
                {
                    Ok(Some(signal)) => observe_signal(&mut stop, signal),
                    Ok(None) => {}
                    Err(error) => observe_failure(&mut stop, error),
                }
                if stop.is_some() {
                    continue;
                }
            }
            if turn_end_seen && pending.is_none() {
                break;
            }
            if pending
                .as_ref()
                .is_some_and(|frame| frame.bytes().is_empty())
            {
                continue;
            }
            let work = next_ui_work(
                active.terminal,
                active.approvals,
                active.events,
                active.scratch,
                pending.as_ref(),
                frame_deadline,
                approval_ui.arm_deadline(),
                approval_ui.escape_deadline(),
                input_escape_deadline,
                !(pending.is_some() && approval_ui.suppresses_read_while_pending()),
                prefer_input,
            );
            tokio::select! {
                biased;
                signal = next_turn_signal(active.signals, active.enhanced) => {
                    match signal {
                        InteractiveSignal::Stop(signal) => {
                            observe_signal(&mut stop, signal);
                            discard_pending(&mut pending, active.presenter);
                        }
                        InteractiveSignal::Resize => {
                            match reconcile_active_geometry(
                                active.terminal,
                                active.signals,
                                active.queued_input.as_deref(),
                                active
                                    .queue_notice
                                    .as_deref()
                                    .and_then(Option::as_deref),
                                approval_ui.dock_interaction(),
                                active.live,
                                active.active_dock.as_mut(),
                                &mut pending,
                                active.enhanced_presenter.as_deref_mut(),
                            )
                            .await
                            {
                                Ok(Some(signal)) => {
                                    observe_signal(&mut stop, signal);
                                    discard_pending(&mut pending, active.presenter);
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    observe_failure(&mut stop, error);
                                    discard_pending(&mut pending, active.presenter);
                                }
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(final_deadline) => {
                    observe_failure(&mut stop, InteractiveError::Output);
                    discard_pending(&mut pending, active.presenter);
                }
                work = work => {
                    prefer_input = !prefer_input;
                    match work {
                        UiWork::FrameExpired => {
                            observe_failure(&mut stop, InteractiveError::Output);
                            discard_pending(&mut pending, active.presenter);
                        }
                        UiWork::ApprovalArmed => {
                            let prepared = approval_ui
                                .begin_rendering(active.terminal, active.color, active.enhanced)
                                .and_then(|output| {
                                    enqueue_approval_selector_surface(
                                        output,
                                        AfterFrame::ApprovalAccepting,
                                        &approval_ui,
                                        active.enhanced,
                                        &mut pending,
                                        &mut frame_deadline,
                                        &mut after_frame,
                                    )
                                });
                            if let Err(error) = prepared {
                                observe_failure(&mut stop, error);
                                discard_pending(&mut pending, active.presenter);
                            }
                        }
                        UiWork::EscapeExpired => {
                            let handled = approval_ui.expire_escape().and_then(|update| {
                                dispatch_approval_update(
                                    update,
                                    active.joins,
                                    active.parser,
                                    &approval_ui,
                                    active.enhanced,
                                    &mut pending,
                                    &mut frame_deadline,
                                    &mut after_frame,
                                )
                            });
                            match handled {
                                Ok(false) => {}
                                Ok(true) => {
                                    stop = Some(StopIntent::Eof);
                                    discard_pending(&mut pending, active.presenter);
                                }
                                Err(error) => {
                                    observe_failure(&mut stop, error);
                                    discard_pending(&mut pending, active.presenter);
                                }
                            }
                        }
                        UiWork::InputEscapeExpired => {
                            input_escape_deadline = None;
                            if approval_owns_active_input(active.joins, &approval_ui) {
                                if let Some(decoder) = active.enhanced_decoder.as_deref_mut() {
                                    decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                                }
                            } else {
                                match handle_active_escape_expiry(
                                    active.terminal,
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    active.active_dock.as_mut(),
                                    active.queue_notice.as_deref_mut(),
                                )? {
                                    ActiveInputOutcome::Continue => {}
                                    ActiveInputOutcome::Redraw => dock_redraw_requested = true,
                                    ActiveInputOutcome::PasteFence | ActiveInputOutcome::Eof => {
                                        observe_failure(&mut stop, InteractiveError::Agent);
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                }
                            }
                        }
                        UiWork::Write(write) => match write {
                            Ok(count) => {
                                if count != 0 {
                                    if let Some(dock) = active.active_dock.as_mut() {
                                        dock.file_suggestions.invalidate_presentation();
                                    }
                                }
                                let advanced = pending
                                    .as_mut()
                                    .ok_or(InteractiveError::Agent)
                                    .and_then(|frame| frame.advance(count));
                                if let Err(error) = advanced {
                                    observe_failure(&mut stop, error);
                                    discard_pending(&mut pending, active.presenter);
                                }
                            }
                            Err(_) => {
                                if let Some(dock) = active.active_dock.as_mut() {
                                    dock.file_suggestions.invalidate_presentation();
                                }
                                observe_failure(&mut stop, InteractiveError::Output);
                                discard_pending(&mut pending, active.presenter);
                            }
                        },
                        UiWork::Envelope(envelope) => {
                            let received = envelope
                                .ok_or(InteractiveError::Agent)
                                .and_then(|envelope| {
                                    active.joins.receive_envelope(envelope).map_err(Into::into)
                                });
                            if let Err(error) = received {
                                observe_failure(&mut stop, error);
                                discard_pending(&mut pending, active.presenter);
                            }
                        }
                        UiWork::Event(event) => {
                            let processed = event.ok_or(InteractiveError::Agent).and_then(|event| {
                                process_event(
                                    event,
                                    start_seq,
                                    turn,
                                    active.live,
                                    active.joins,
                                    EventTargets {
                                        pending: &mut pending,
                                        frame_deadline: &mut frame_deadline,
                                        after_frame: &mut after_frame,
                                        approval_ui: &mut approval_ui,
                                        turn_end_seen: &mut turn_end_seen,
                                        prompt_committed: active.prompt_committed,
                                        expected_prompt: &active.prompt,
                                        render_committed_prompt: active.enhanced,
                                        detail_view_requested: active.active_dock.as_ref().is_some_and(
                                            |dock| dock.view.requested().mode() != ViewMode::Focus,
                                        ),
                                        dock_notice: active.queue_notice.as_deref_mut(),
                                        dock_redraw_requested: active
                                            .enhanced
                                            .then_some(&mut dock_redraw_requested),
                                    },
                                )
                            });
                            if let Err(error) = processed {
                                observe_failure(&mut stop, error);
                                discard_pending(&mut pending, active.presenter);
                            }
                        }
                        UiWork::Read(Ok(0)) => {
                            stop = Some(StopIntent::Eof);
                            discard_pending(&mut pending, active.presenter);
                        }
                        UiWork::Read(Ok(count)) => {
                            if approval_ui.is_accepting() {
                                let handled = active
                                    .joins
                                    .question()
                                    .ok_or(InteractiveError::Agent)
                                    .and_then(|question| {
                                        approval_ui.feed(
                                            &active.scratch[..count],
                                            question.challenge(),
                                            active.color,
                                            active.enhanced,
                                        )
                                    })
                                    .and_then(|update| {
                                        dispatch_approval_update(
                                            update,
                                            active.joins,
                                            active.parser,
                                            &approval_ui,
                                            active.enhanced,
                                            &mut pending,
                                            &mut frame_deadline,
                                            &mut after_frame,
                                        )
                                    });
                                match handled {
                                    Ok(false) => {}
                                    Ok(true) => {
                                        stop = Some(StopIntent::Eof);
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                    Err(error) => {
                                        observe_failure(&mut stop, error);
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                }
                            } else if !approval_owns_active_input(active.joins, &approval_ui) {
                                let input_outcome = handle_active_input(
                                    active.terminal,
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    active.active_dock.as_mut(),
                                    active.queue_notice.as_deref_mut(),
                                    &active.scratch[..count],
                                )?;
                                if let Some(decoder) = active.enhanced_decoder.as_deref() {
                                    refresh_decoder_escape_deadline(
                                        decoder,
                                        &mut input_escape_deadline,
                                    );
                                }
                                match input_outcome {
                                    ActiveInputOutcome::Continue => {}
                                    ActiveInputOutcome::Eof => {
                                        stop = Some(StopIntent::Eof);
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                    ActiveInputOutcome::Redraw
                                    | ActiveInputOutcome::PasteFence => {
                                        let paste_fence =
                                            input_outcome == ActiveInputOutcome::PasteFence;
                                        if pending.is_some() {
                                            dock_redraw_requested = true;
                                        } else if let (Some(input), Some(dock)) = (
                                            active.queued_input.as_deref(),
                                            active.active_dock.as_mut(),
                                        ) {
                                            match render_active_dock(
                                                input,
                                                active
                                                    .queue_notice
                                                    .as_deref()
                                                    .and_then(Option::as_deref),
                                                DockInteraction::Running,
                                                active.live,
                                                active.terminal,
                                                active.signals,
                                                dock,
                                            )
                                            .await
                                            {
                                                Ok(Some(signal)) => {
                                                    observe_signal(&mut stop, signal);
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                                Ok(None) => {}
                                                Err(error) => {
                                                    observe_failure(&mut stop, error);
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                            }
                                        }
                                        if paste_fence && stop.is_none() {
                                            match complete_paste_input_fence(
                                                active.terminal,
                                                active.signals,
                                                active.scratch,
                                            )
                                            .await?
                                            {
                                                PasteFenceOutcome::Ready => {
                                                    active
                                                        .enhanced_decoder
                                                        .as_deref_mut()
                                                        .ok_or(InteractiveError::Agent)?
                                                        .reset_epoch()
                                                        .map_err(|_| InteractiveError::Agent)?;
                                                    input_escape_deadline = None;
                                                    *active
                                                        .queue_notice
                                                        .as_deref_mut()
                                                        .ok_or(InteractiveError::Agent)? =
                                                        Some("Paste ready · Enter sends".to_owned());
                                                    dock_redraw_requested = true;
                                                }
                                                PasteFenceOutcome::Signal(signal) => {
                                                    observe_signal(&mut stop, signal);
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                                PasteFenceOutcome::Eof => {
                                                    stop = Some(StopIntent::Eof);
                                                    discard_pending(
                                                        &mut pending,
                                                        active.presenter,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                approval_ui.observe_unaccepted_input();
                            }
                        }
                        UiWork::Read(Err(_)) => {
                            observe_failure(&mut stop, InteractiveError::TerminalUnavailable);
                            discard_pending(&mut pending, active.presenter);
                        }
                    }
                }
            }
        }
    }

    drain_active_signals(active.signals, &mut stop);
    if latch_observer_fault(active.events, &mut stop, &cancellation) {
        discard_pending(&mut pending, active.presenter);
    }
    if let Err(error) = approval_ui.restore() {
        observe_failure(&mut stop, error);
    }

    if stop.is_none() {
        if let Ok(outcome) = &result {
            let expected_next_seq = outcome
                .turn_end_seq()
                .get()
                .checked_add(1)
                .and_then(|value| EventSeq::new(value).ok());
            active
                .live
                .set_context_estimate(expected_next_seq.and_then(|expected| {
                    session_context_estimate(
                        active.agent.session(),
                        Some(outcome.turn()),
                        Some(expected),
                    )
                }));
            active.live.freeze_joined_review(outcome);
        }
    }

    if active.enhanced && stop.is_none() {
        if let Ok(outcome) = &result {
            let input = active
                .queued_input
                .as_deref()
                .ok_or(InteractiveError::Agent)?;
            let command_palette = active
                .active_dock
                .as_ref()
                .map(|dock| active_command_palette_snapshot(input, dock))
                .ok_or(InteractiveError::Agent)?;
            match active.live.receipt_frame(outcome) {
                Ok(frame) => match write_enhanced_terminal_frame(
                    frame,
                    DockRenderModel {
                        input,
                        notice: active.queue_notice.as_deref().and_then(Option::as_deref),
                        interaction: DockInteraction::Running,
                        command_palette,
                        live: active.live,
                    },
                    active
                        .enhanced_presenter
                        .as_deref_mut()
                        .ok_or(InteractiveError::Agent)?,
                    active.terminal,
                    active.signals,
                    active.active_dock.as_mut().ok_or(InteractiveError::Agent)?,
                )
                .await
                {
                    Ok(None) => turn_end_rendered = true,
                    Ok(Some(signal)) => observe_signal(&mut stop, signal),
                    Err(error) => observe_failure(&mut stop, error),
                },
                Err(_) => observe_failure(&mut stop, InteractiveError::Output),
            }
        }
    }

    let mut skipped = 0_usize;
    if stop.is_some() {
        skipped = discard_ready_updates_after_stop(
            active.events,
            start_seq,
            &active.prompt,
            active.prompt_committed,
        )?;
        active
            .joins
            .finish_turn(active.approvals, ApprovalResetMode::Discard)?;
    } else {
        active
            .joins
            .finish_turn(active.approvals, ApprovalResetMode::Normal)?;
    }

    let disposition = finish_turn_disposition(
        stop,
        skipped,
        turn_end_rendered,
        active.presenter,
        active.live,
        active.terminal,
        active.signals,
        active.enhanced,
        active.queued_input.as_deref(),
        active
            .queue_notice
            .as_deref()
            .and_then(|notice| notice.as_deref()),
        active.enhanced_presenter.as_deref_mut(),
        active.active_dock.as_mut(),
    )
    .await?;
    if session_capacity_exhausted && disposition == TurnDisposition::Continue {
        Ok(TurnDisposition::Exit(1))
    } else {
        Ok(disposition)
    }
}

fn turn_exhausted_session_capacity(reason: &TurnEndReason) -> bool {
    matches!(reason, TurnEndReason::Error { error } if error.code() == "AGENT_EVENT_BUDGET")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveInputOutcome {
    Continue,
    Redraw,
    PasteFence,
    Eof,
}

fn handle_active_input(
    terminal: &AsyncTerminal,
    decoder: Option<&mut KeyDecoder>,
    input: Option<&mut InputMemory>,
    dock: Option<&mut ActiveDock<'_>>,
    notice: Option<&mut Option<String>>,
    bytes: &[u8],
) -> Result<ActiveInputOutcome, InteractiveError> {
    let (Some(decoder), Some(input), Some(dock), Some(notice)) = (decoder, input, dock, notice)
    else {
        return Ok(ActiveInputOutcome::Continue);
    };
    let size = terminal.size().unwrap_or(TerminalSize {
        rows: MIN_ENHANCED_ROWS,
        columns: MIN_ENHANCED_COLUMNS,
    });
    match apply_enhanced_input(
        decoder,
        bytes,
        input,
        dock.command_palette,
        Some(dock.file_suggestions),
        dock.view,
        dock.theme,
        size,
        notice,
    )? {
        EnhancedInputAction::None => Ok(ActiveInputOutcome::Continue),
        EnhancedInputAction::Redraw | EnhancedInputAction::RedrawFence => {
            Ok(ActiveInputOutcome::Redraw)
        }
        EnhancedInputAction::PasteFence => Ok(ActiveInputOutcome::PasteFence),
        EnhancedInputAction::Exit => Ok(ActiveInputOutcome::Eof),
        EnhancedInputAction::Submit => {
            match classify_enhanced_submission(input.composer().text()) {
                EnhancedSubmission::Command(command) => {
                    match command {
                        CommandId::Exit | CommandId::Quit => {
                            return Ok(ActiveInputOutcome::Eof);
                        }
                        CommandId::Help => {
                            let _ = input.take_draft_for_turn()?;
                            *notice = Some(
                                "/inspect | /review | /focus | /theme | /help | /exit | /quit | Enter queue | Ctrl+J newline"
                                    .to_owned(),
                            );
                        }
                        CommandId::Inspect | CommandId::Review | CommandId::Focus => {
                            let mode = match command {
                                CommandId::Inspect => ViewMode::Inspect,
                                CommandId::Review => ViewMode::Review,
                                CommandId::Focus => ViewMode::Focus,
                                _ => return Err(InteractiveError::Agent),
                            };
                            let _ = input.take_draft_for_turn()?;
                            let _ = dock
                                .view
                                .request_mode(mode)
                                .map_err(|_| InteractiveError::Output)?;
                            *notice = None;
                        }
                        CommandId::Theme => {
                            let _ = input.take_draft_for_turn()?;
                            apply_theme_command(ThemeCommand::Show, dock.theme, notice)?;
                        }
                    }
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Theme(command) => {
                    let _ = input.take_draft_for_turn()?;
                    apply_theme_command(command, dock.theme, notice)?;
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Empty => {
                    *notice = Some("Type a prompt before queueing the next turn".to_owned());
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Prompt => {}
            }
            match input.enqueue_draft() {
                Ok(_) => {
                    *notice = Some(format!(
                        "{} next-turn prompt(s) queued",
                        input.queue().len()
                    ));
                }
                Err(error) => {
                    *notice = Some(format!("{error} · draft kept"));
                }
            }
            let _ = dock
                .file_suggestions
                .sync(input.composer(), false, false)
                .map_err(|_| InteractiveError::Agent)?;
            Ok(ActiveInputOutcome::Redraw)
        }
    }
}

fn handle_active_escape_expiry(
    terminal: &AsyncTerminal,
    decoder: Option<&mut KeyDecoder>,
    input: Option<&mut InputMemory>,
    dock: Option<&mut ActiveDock<'_>>,
    notice: Option<&mut Option<String>>,
) -> Result<ActiveInputOutcome, InteractiveError> {
    let (Some(decoder), Some(input), Some(dock), Some(notice)) = (decoder, input, dock, notice)
    else {
        return Ok(ActiveInputOutcome::Continue);
    };
    let size = terminal.size().unwrap_or(TerminalSize {
        rows: MIN_ENHANCED_ROWS,
        columns: MIN_ENHANCED_COLUMNS,
    });
    match expire_enhanced_escape(
        decoder,
        input,
        dock.command_palette,
        Some(dock.file_suggestions),
        dock.view,
        dock.theme,
        size,
        notice,
    )? {
        EnhancedInputAction::None => Ok(ActiveInputOutcome::Continue),
        EnhancedInputAction::Redraw | EnhancedInputAction::RedrawFence => {
            Ok(ActiveInputOutcome::Redraw)
        }
        EnhancedInputAction::PasteFence => Ok(ActiveInputOutcome::PasteFence),
        EnhancedInputAction::Exit | EnhancedInputAction::Submit => Err(InteractiveError::Agent),
    }
}

fn process_event(
    event: crate::session::CommittedUiEvent,
    expected_start: crate::session::EventSeq,
    expected_turn: TurnId,
    live: &mut LiveRenderer,
    joins: &mut ApprovalJoin,
    mut targets: EventTargets<'_, '_>,
) -> Result<(), InteractiveError> {
    if event.seq.get() < expected_start.get() {
        return Err(InteractiveError::Agent);
    }
    let committed_prompt = match &event.kind {
        CommittedUiKind::UserMessage {
            source: UiUserSource::Human,
            content,
        } => {
            let content = content.as_str().ok_or(InteractiveError::Agent)?;
            if content != targets.expected_prompt {
                return Err(InteractiveError::Agent);
            }
            targets
                .render_committed_prompt
                .then(|| copy_enhanced_prompt(content))
                .transpose()?
        }
        _ => None,
    };
    if committed_prompt.is_some()
        || matches!(
            &event.kind,
            CommittedUiKind::UserMessage {
                source: UiUserSource::Human,
                ..
            }
        )
    {
        *targets.prompt_committed = true;
    }
    let mut update = live.consume(event).map_err(|_| InteractiveError::Output)?;
    let mut frame_after = AfterFrame::None;
    match std::mem::replace(&mut update.lifecycle, LiveLifecycle::None) {
        LiveLifecycle::None => {}
        LiveLifecycle::ApprovalAsked {
            id,
            tool_name,
            call_id,
            reason,
        } => joins.observe_asked(id, tool_name, call_id, reason)?,
        LiveLifecycle::ApprovalDecided { id, outcome } => {
            targets.approval_ui.restore()?;
            joins.observe_decided(id, outcome)?;
        }
        LiveLifecycle::TurnEnded { turn } => {
            if turn != expected_turn {
                return Err(InteractiveError::Agent);
            }
            joins.observe_turn_end()?;
            *targets.turn_end_seen = true;
            frame_after = AfterFrame::TurnEnd;
        }
    }
    if targets.render_committed_prompt {
        if let Some(notice) = targets.dock_notice.as_deref_mut() {
            if update.apply_dock_notice(notice) {
                if let Some(redraw) = targets.dock_redraw_requested.as_deref_mut() {
                    *redraw = true;
                }
            }
        }
        if targets.detail_view_requested {
            if let Some(redraw) = targets.dock_redraw_requested.as_deref_mut() {
                *redraw = true;
            }
        }
    }
    let product_frame = update.take_frame(targets.render_committed_prompt);
    let frame = match (committed_prompt, product_frame) {
        (Some(prompt), None) => {
            Some(LiveFrame::human_message(prompt).map_err(|_| InteractiveError::Output)?)
        }
        (None, frame) => frame,
        (Some(_), Some(_)) => return Err(InteractiveError::Agent),
    };
    if let Some(frame) = frame {
        enqueue_frame(
            frame,
            frame_after,
            targets.pending,
            targets.frame_deadline,
            targets.after_frame,
        )?;
    }
    Ok(())
}

struct EventTargets<'a, 'terminal> {
    pending: &'a mut Option<PendingOutput>,
    frame_deadline: &'a mut Option<Instant>,
    after_frame: &'a mut AfterFrame,
    approval_ui: &'a mut ApprovalUiState<'terminal>,
    turn_end_seen: &'a mut bool,
    prompt_committed: &'a mut bool,
    expected_prompt: &'a str,
    render_committed_prompt: bool,
    detail_view_requested: bool,
    dock_notice: Option<&'a mut Option<String>>,
    dock_redraw_requested: Option<&'a mut bool>,
}

fn apply_approval_update(
    update: ApprovalUiUpdate,
    joins: &mut ApprovalJoin,
    parser: &mut CanonicalRecordParser,
    pending: &mut Option<PendingOutput>,
    frame_deadline: &mut Option<Instant>,
    after_frame: &mut AfterFrame,
) -> Result<bool, InteractiveError> {
    match update {
        ApprovalUiUpdate::None => {}
        ApprovalUiUpdate::Redraw(output) => {
            let frame =
                LiveFrame::approval_selector(output).map_err(|_| InteractiveError::Output)?;
            enqueue_frame(
                frame,
                AfterFrame::None,
                pending,
                frame_deadline,
                after_frame,
            )?;
        }
        ApprovalUiUpdate::Decide(outcome) => {
            joins.answer(outcome)?;
            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
        }
        ApprovalUiUpdate::Eof => return Ok(true),
        ApprovalUiUpdate::Invalid => {
            let frame = approval_frame(joins, true)?;
            enqueue_frame(
                frame,
                AfterFrame::ApprovalFence,
                pending,
                frame_deadline,
                after_frame,
            )?;
            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
        }
    }
    Ok(false)
}

fn approval_frame(joins: &ApprovalJoin, retry: bool) -> Result<LiveFrame, InteractiveError> {
    let question = joins.question().ok_or(InteractiveError::Agent)?;
    let frame = LiveFrame::approval(
        question.tool_name(),
        question.call_id(),
        question.reason(),
        question.preview_arc(),
        question.preview_kind(),
        retry,
    )
    .map_err(|_| InteractiveError::Output)?;
    Ok(frame)
}

fn enqueue_frame(
    frame: LiveFrame,
    after: AfterFrame,
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    pending_after: &mut AfterFrame,
) -> Result<(), InteractiveError> {
    if pending.is_some() {
        return Err(InteractiveError::Agent);
    }
    *pending = Some(PendingOutput::Unprepared(frame));
    *deadline = Some(Instant::now() + FRAME_DEADLINE);
    *pending_after = after;
    Ok(())
}

fn enqueue_enhanced_dock(
    interaction: DockInteraction,
    after: AfterFrame,
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    pending_after: &mut AfterFrame,
) -> Result<(), InteractiveError> {
    if pending.is_some() {
        return Err(InteractiveError::Agent);
    }
    *pending = Some(PendingOutput::Dock(interaction));
    *deadline = Some(Instant::now() + FRAME_DEADLINE);
    *pending_after = after;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn enqueue_approval_selector_surface(
    output: String,
    after: AfterFrame,
    approval_ui: &ApprovalUiState<'_>,
    enhanced: bool,
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    pending_after: &mut AfterFrame,
) -> Result<(), InteractiveError> {
    if enhanced {
        enqueue_enhanced_dock(
            DockInteraction::Approval(approval_ui.dock_selection()?),
            after,
            pending,
            deadline,
            pending_after,
        )
    } else {
        let frame = LiveFrame::approval_selector(output).map_err(|_| InteractiveError::Output)?;
        enqueue_frame(frame, after, pending, deadline, pending_after)
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_approval_update(
    update: ApprovalUiUpdate,
    joins: &mut ApprovalJoin,
    parser: &mut CanonicalRecordParser,
    approval_ui: &ApprovalUiState<'_>,
    enhanced: bool,
    pending: &mut Option<PendingOutput>,
    frame_deadline: &mut Option<Instant>,
    after_frame: &mut AfterFrame,
) -> Result<bool, InteractiveError> {
    if !enhanced {
        return apply_approval_update(update, joins, parser, pending, frame_deadline, after_frame);
    }
    match update {
        ApprovalUiUpdate::None => {}
        ApprovalUiUpdate::Redraw(output) => enqueue_approval_selector_surface(
            output,
            AfterFrame::None,
            approval_ui,
            true,
            pending,
            frame_deadline,
            after_frame,
        )?,
        ApprovalUiUpdate::Decide(outcome) => {
            joins.answer(outcome)?;
            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
        }
        ApprovalUiUpdate::Eof => return Ok(true),
        ApprovalUiUpdate::Invalid => {
            enqueue_enhanced_dock(
                DockInteraction::Running,
                AfterFrame::ApprovalFence,
                pending,
                frame_deadline,
                after_frame,
            )?;
            parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn complete_ready_frame(
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    after: &mut AfterFrame,
    approval_ui: &mut ApprovalUiState<'_>,
    turn_end_rendered: &mut bool,
    presenter: &mut InteractivePresenter,
    live: &LiveRenderer,
    terminal: &AsyncTerminal,
    parser: &mut CanonicalRecordParser,
    enhanced: bool,
    mut enhanced_presenter: Option<&mut EnhancedPresenter>,
    input: Option<&InputMemory>,
    notice: Option<&str>,
    mut active_dock: Option<&mut ActiveDock<'_>>,
) -> Result<(), InteractiveError> {
    if matches!(pending, Some(PendingOutput::Unprepared(_))) {
        let frame = match pending.take() {
            Some(PendingOutput::Unprepared(frame)) => frame,
            _ => return Err(InteractiveError::Agent),
        };
        *pending = Some(if enhanced {
            let presenter = enhanced_presenter
                .as_deref_mut()
                .ok_or(InteractiveError::Agent)?;
            PendingOutput::Prepared(
                presenter
                    .prepare(frame)
                    .map_err(|_| InteractiveError::Output)?,
            )
        } else {
            PendingOutput::Linear(frame.into_pending().map_err(|_| InteractiveError::Output)?)
        });
    }
    if matches!(pending, Some(PendingOutput::Prepared(_))) {
        let presentation = match pending.take() {
            Some(PendingOutput::Prepared(presentation)) => presentation,
            _ => return Err(InteractiveError::Agent),
        };
        let staged = (|| {
            let input = input.ok_or(InteractiveError::Agent)?;
            let dock = active_dock.as_deref_mut().ok_or(InteractiveError::Agent)?;
            let size = terminal.size().unwrap_or(*dock.last_size);
            if size != *dock.last_size {
                return Err(InteractiveError::TerminalUnsupported);
            }
            let show_file_suggestions = dock.view.committed().mode() == ViewMode::Focus;
            let staged_file_suggestions = dock
                .file_suggestions
                .stage_presentation(show_file_suggestions)
                .map_err(|_| InteractiveError::Agent)?;
            let file_snapshot = if show_file_suggestions {
                dock.file_suggestions.snapshot()
            } else {
                FileSuggestionSnapshot::Hidden
            };
            let surface = enhanced_surface_frame_for_request(
                input,
                notice,
                command_palette_interaction(
                    DockInteraction::Running,
                    palette_behind_files(
                        active_command_palette_snapshot(input, dock),
                        file_snapshot,
                    ),
                    dock.view.committed().mode(),
                ),
                size,
                dock.view.committed(),
                dock.theme.requested(),
                live,
                file_snapshot,
            )?;
            let write = dock
                .screen
                .stage_transcript(
                    presentation.chunk(),
                    &surface.frame,
                    surface.commit.theme.palette(),
                )
                .map_err(map_inline_screen_error)?;
            Ok(PendingOutput::Inline(PendingInlineOutput {
                write,
                intent: InlineIntent::Transcript(presentation),
                surface: surface.commit,
                file_suggestions: staged_file_suggestions,
            }))
        })();
        match staged {
            Ok(staged) => *pending = Some(staged),
            Err(error) => return Err(error),
        }
    }
    if matches!(pending, Some(PendingOutput::Dock(_))) {
        let interaction = match pending.take() {
            Some(PendingOutput::Dock(interaction)) => interaction,
            _ => return Err(InteractiveError::Agent),
        };
        let input = input.ok_or(InteractiveError::Agent)?;
        let dock = active_dock.as_deref_mut().ok_or(InteractiveError::Agent)?;
        let size = terminal.size().unwrap_or(*dock.last_size);
        if size != *dock.last_size {
            return Err(InteractiveError::TerminalUnsupported);
        }
        let show_file_suggestions = dock.view.requested().mode() == ViewMode::Focus
            && !matches!(interaction, DockInteraction::Approval(_));
        let staged_file_suggestions = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let surface = enhanced_surface_frame(
            input,
            notice,
            command_palette_interaction(
                interaction,
                palette_behind_files(active_command_palette_snapshot(input, dock), file_snapshot),
                dock.view.requested().mode(),
            ),
            size,
            dock.view,
            dock.theme,
            live,
            file_snapshot,
        )?;
        let write = stage_surface(
            dock.screen,
            size,
            false,
            &surface.frame,
            surface.commit.theme.palette(),
        )?;
        *pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::Dock(interaction),
            surface: surface.commit,
            file_suggestions: staged_file_suggestions,
        }));
    }
    let Some(frame) = pending.as_mut() else {
        return Ok(());
    };
    match frame {
        PendingOutput::Unprepared(_) | PendingOutput::Prepared(_) | PendingOutput::Dock(_) => {
            return Err(InteractiveError::Agent);
        }
        PendingOutput::Linear(frame) => {
            if frame
                .prepare_next(presenter)
                .map_err(|_| InteractiveError::Output)?
            {
                return Ok(());
            }
        }
        PendingOutput::Inline(output) => {
            if !output.write.is_complete() {
                return Ok(());
            }
            let output = match pending.take() {
                Some(PendingOutput::Inline(output)) => output,
                _ => return Err(InteractiveError::Agent),
            };
            let dock = active_dock.ok_or(InteractiveError::Agent)?;
            let mut transcript_presenter = if matches!(output.intent, InlineIntent::Transcript(_)) {
                Some(enhanced_presenter.take().ok_or(InteractiveError::Agent)?)
            } else {
                None
            };
            dock.screen
                .commit(output.write)
                .map_err(map_inline_screen_error)?;
            commit_surface(dock.view, dock.theme, output.surface);
            dock.file_suggestions
                .commit_presentation(output.file_suggestions);
            if let InlineIntent::Transcript(presentation) = output.intent {
                transcript_presenter
                    .take()
                    .expect("transcript presenter was proven before screen commit")
                    .commit(presentation);
            }
        }
    }
    *pending = None;
    *deadline = None;
    match mem::take(after) {
        AfterFrame::None => {}
        AfterFrame::ApprovalFence => {
            if enhanced {
                terminal.revalidate_identity()?;
            } else {
                terminal.revalidate()?;
            }
            terminal.flush_input()?;
            parser.reset(MAX_APPROVAL_RECORD_BYTES);
            approval_ui.begin_arming()?;
        }
        AfterFrame::ApprovalAccepting => {
            parser.reset(MAX_APPROVAL_RECORD_BYTES);
            approval_ui.accept_rendered(terminal)?;
        }
        AfterFrame::TurnEnd => *turn_end_rendered = true,
    }
    Ok(())
}

fn discard_pending(pending: &mut Option<PendingOutput>, presenter: &mut InteractivePresenter) {
    if pending.take().is_some() {
        presenter.discard_partly_written_frame();
    }
}

fn latch_active_failure(
    stop: &mut Option<StopIntent>,
    cancellation: &CancellationToken,
    pending: &mut Option<PendingOutput>,
    presenter: &mut InteractivePresenter,
    error: InteractiveError,
) {
    observe_failure(stop, error);
    cancellation.cancel();
    discard_pending(pending, presenter);
}

enum UiWork {
    FrameExpired,
    ApprovalArmed,
    EscapeExpired,
    InputEscapeExpired,
    Write(std::io::Result<usize>),
    Envelope(Option<ApprovalEnvelope>),
    Event(Option<crate::session::CommittedUiEvent>),
    Read(std::io::Result<usize>),
}

#[allow(clippy::too_many_arguments)]
async fn next_ui_work(
    terminal: &AsyncTerminal,
    approvals: &mut ApprovalEnvelopeReceiver,
    events: &mut CommittedUiReceiver,
    scratch: &mut [u8; TERMINAL_READ_BYTES],
    pending: Option<&PendingOutput>,
    frame_deadline: Option<Instant>,
    approval_arm_deadline: Option<Instant>,
    escape_deadline: Option<Instant>,
    input_escape_deadline: Option<Instant>,
    read_enabled: bool,
    prefer_input: bool,
) -> UiWork {
    let deadline = frame_deadline.unwrap_or_else(Instant::now);
    let arm_deadline = approval_arm_deadline.unwrap_or_else(Instant::now);
    let escape_pending = escape_deadline.is_some();
    let escape_deadline_at = escape_deadline.unwrap_or_else(Instant::now);
    let input_escape_pending = input_escape_deadline.is_some();
    let input_escape_deadline_at = input_escape_deadline.unwrap_or_else(Instant::now);
    if prefer_input {
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline), if pending.is_some() => UiWork::FrameExpired,
            () = tokio::time::sleep_until(arm_deadline), if approval_arm_deadline.is_some() => UiWork::ApprovalArmed,
            () = tokio::time::sleep_until(escape_deadline_at), if escape_pending => UiWork::EscapeExpired,
            () = tokio::time::sleep_until(input_escape_deadline_at), if input_escape_pending => UiWork::InputEscapeExpired,
            read = terminal.read_once(scratch), if read_enabled => UiWork::Read(read),
            write = write_pending(terminal, pending), if pending.is_some() => UiWork::Write(write),
            envelope = approvals.recv() => UiWork::Envelope(envelope),
            event = events.recv(), if pending.is_none() => UiWork::Event(event),
        }
    } else {
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline), if pending.is_some() => UiWork::FrameExpired,
            () = tokio::time::sleep_until(arm_deadline), if approval_arm_deadline.is_some() => UiWork::ApprovalArmed,
            () = tokio::time::sleep_until(escape_deadline_at), if escape_pending => UiWork::EscapeExpired,
            () = tokio::time::sleep_until(input_escape_deadline_at), if input_escape_pending => UiWork::InputEscapeExpired,
            write = write_pending(terminal, pending), if pending.is_some() => UiWork::Write(write),
            envelope = approvals.recv() => UiWork::Envelope(envelope),
            event = events.recv(), if pending.is_none() => UiWork::Event(event),
            read = terminal.read_once(scratch), if read_enabled => UiWork::Read(read),
        }
    }
}

async fn write_pending(
    terminal: &AsyncTerminal,
    pending: Option<&PendingOutput>,
) -> std::io::Result<usize> {
    let bytes = pending.map(PendingOutput::bytes).unwrap_or_default();
    terminal.write_once(bytes).await
}

async fn write_frame(
    frame: LiveFrame,
    presenter: &mut InteractivePresenter,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, InteractiveError> {
    let mut pending = frame.into_pending().map_err(|_| InteractiveError::Output)?;
    let deadline = Instant::now() + FRAME_DEADLINE;
    loop {
        if !pending
            .prepare_next(presenter)
            .map_err(|_| InteractiveError::Output)?
        {
            return Ok(None);
        }
        let work = tokio::select! {
            biased;
            signal = signals.next() => IdleWriteWork::Signal(signal),
            () = tokio::time::sleep_until(deadline) => IdleWriteWork::Expired,
            write = terminal.write_once(pending.bytes()) => IdleWriteWork::Write(write),
        };
        let mut latch = SignalLatch::default();
        if let IdleWriteWork::Signal(signal) = &work {
            latch.observe(DriverMode::Interactive, *signal);
        }
        tokio::task::yield_now().await;
        signals.drain_ready(DriverMode::Interactive, &mut latch);
        match work {
            IdleWriteWork::Signal(_) => {
                presenter.discard_partly_written_frame();
                return Ok(latch.observed());
            }
            IdleWriteWork::Expired | IdleWriteWork::Write(Err(_)) => {
                if let Some(signal @ (UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate)) =
                    latch.observed()
                {
                    presenter.discard_partly_written_frame();
                    return Ok(Some(signal));
                }
                return Err(InteractiveError::Output);
            }
            IdleWriteWork::Write(Ok(count)) => {
                if let Some(signal) = latch.observed() {
                    presenter.discard_partly_written_frame();
                    return Ok(Some(signal));
                }
                pending
                    .advance(count)
                    .map_err(|_| InteractiveError::Output)?;
            }
        }
    }
}

enum IdleWriteWork {
    Signal(UiSignal),
    Expired,
    Write(std::io::Result<usize>),
}

async fn write_notice(
    notice: &'static str,
    presenter: &mut InteractivePresenter,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, InteractiveError> {
    let frame = LiveFrame::notice(notice).map_err(|_| InteractiveError::Output)?;
    if let Some(signal) = write_frame(frame, presenter, terminal, signals).await? {
        if let Some(signal) = handle_idle_signal(signal, terminal, signals).await? {
            return Ok(Some(signal));
        }
    }
    Ok(None)
}

async fn handle_idle_signal(
    signal: UiSignal,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, InteractiveError> {
    match signal {
        UiSignal::Interrupt => {
            terminal.flush_input()?;
            Ok(None)
        }
        UiSignal::Suspend => Ok(suspend_and_resume(terminal, signals).await?),
        UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate => Ok(Some(signal)),
    }
}

async fn finish_signal_after_shutdown(
    signal: UiSignal,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<u8>, InteractiveError> {
    match signal {
        UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate => {
            signal.exit_code().map(Some).ok_or(InteractiveError::Agent)
        }
        UiSignal::Suspend => match suspend_and_resume(terminal, signals).await? {
            Some(terminating) => terminating
                .exit_code()
                .map(Some)
                .ok_or(InteractiveError::Agent),
            None => Ok(None),
        },
        UiSignal::Interrupt => Ok(None),
    }
}

pub(super) async fn suspend_and_resume(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, TerminalError> {
    loop {
        self_suspend().map_err(|_| TerminalError::Unsupported)?;
        let mut latch = SignalLatch::default();
        signals.drain_ready(DriverMode::Interactive, &mut latch);
        if let Some(signal @ (UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate)) =
            latch.observed()
        {
            return Ok(Some(signal));
        }
        if terminal.is_foreground()? {
            terminal.revalidate()?;
            terminal.flush_input()?;
            return Ok(None);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finish_turn_disposition(
    stop: Option<StopIntent>,
    skipped: usize,
    turn_end_rendered: bool,
    presenter: &mut InteractivePresenter,
    live: &LiveRenderer,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    defer_suspend: bool,
    input: Option<&InputMemory>,
    notice: Option<&str>,
    enhanced_presenter: Option<&mut EnhancedPresenter>,
    active_dock: Option<&mut ActiveDock<'_>>,
) -> Result<TurnDisposition, InteractiveError> {
    match stop {
        None => Ok(TurnDisposition::Continue),
        Some(StopIntent::Interrupt) => {
            if !turn_end_rendered {
                let frame = LiveFrame::stopped(skipped).map_err(|_| InteractiveError::Output)?;
                let signal = if defer_suspend {
                    let input = input.ok_or(InteractiveError::Agent)?;
                    let dock = active_dock.ok_or(InteractiveError::Agent)?;
                    write_enhanced_terminal_frame(
                        frame,
                        DockRenderModel {
                            input,
                            notice,
                            interaction: DockInteraction::Running,
                            command_palette: active_command_palette_snapshot(input, dock),
                            live,
                        },
                        enhanced_presenter.ok_or(InteractiveError::Agent)?,
                        terminal,
                        signals,
                        dock,
                    )
                    .await?
                } else {
                    write_frame(frame, presenter, terminal, signals).await?
                };
                if let Some(signal) = signal {
                    return finish_signal_after_cleanup(signal, terminal, signals, defer_suspend)
                        .await;
                }
            }
            Ok(TurnDisposition::Continue)
        }
        Some(StopIntent::Eof) => Ok(TurnDisposition::Exit(0)),
        Some(StopIntent::Suspend) if defer_suspend => {
            Ok(TurnDisposition::Signal(UiSignal::Suspend))
        }
        Some(StopIntent::Suspend) => match suspend_and_resume(terminal, signals).await? {
            Some(signal) => Ok(TurnDisposition::Signal(signal)),
            None => Ok(TurnDisposition::Continue),
        },
        Some(StopIntent::Exit(signal)) => Ok(TurnDisposition::Signal(signal)),
        Some(StopIntent::Failure(error)) => Err(error),
    }
}

async fn write_enhanced_terminal_frame(
    frame: LiveFrame,
    model: DockRenderModel<'_>,
    presenter: &mut EnhancedPresenter,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    dock: &mut ActiveDock<'_>,
) -> Result<Option<UiSignal>, InteractiveError> {
    let mut presentation = presenter
        .prepare(frame)
        .map_err(|_| InteractiveError::Output)?;
    loop {
        let mut boundary_changed = false;
        if dock.screen.is_poisoned() {
            dock.file_suggestions.invalidate_presentation();
            if let Some(signal) = recover_poisoned_screen(terminal, signals, dock.screen).await? {
                return Ok(Some(signal));
            }
            boundary_changed = true;
        }
        let size = terminal.size().unwrap_or(*dock.last_size);
        if size != *dock.last_size {
            dock.file_suggestions.invalidate_presentation();
        }
        if dock.screen.is_detached() || size != *dock.last_size {
            if let Some(signal) = render_active_dock(
                model.input,
                model.notice,
                model.interaction,
                model.live,
                terminal,
                signals,
                dock,
            )
            .await?
            {
                return Ok(Some(signal));
            }
            boundary_changed = true;
        }
        if boundary_changed {
            presentation.force_next_line_boundary();
        }

        let size = terminal.size().unwrap_or(*dock.last_size);
        if size != *dock.last_size {
            continue;
        }
        let show_file_suggestions = dock.view.committed().mode() == ViewMode::Focus
            && !matches!(model.interaction, DockInteraction::Approval(_));
        let staged_file_suggestions = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let surface = enhanced_surface_frame_for_request(
            model.input,
            model.notice,
            command_palette_interaction(
                model.interaction,
                palette_behind_files(model.command_palette, file_snapshot),
                dock.view.committed().mode(),
            ),
            size,
            dock.view.committed(),
            dock.theme.requested(),
            model.live,
            file_snapshot,
        )?;
        let write = dock
            .screen
            .stage_transcript(
                presentation.chunk(),
                &surface.frame,
                surface.commit.theme.palette(),
            )
            .map_err(map_inline_screen_error)?;
        match write_screen_transaction(terminal, signals, dock.screen, write).await? {
            ScreenWriteOutcome::Complete => {
                commit_surface(dock.view, dock.theme, surface.commit);
                dock.file_suggestions
                    .commit_presentation(staged_file_suggestions);
                presenter.commit(presentation);
                return Ok(None);
            }
            ScreenWriteOutcome::Signal(signal) => {
                if dock.screen.is_poisoned() {
                    dock.file_suggestions.invalidate_presentation();
                }
                return Ok(Some(signal));
            }
            ScreenWriteOutcome::Resize | ScreenWriteOutcome::PoisonedResize => {
                dock.file_suggestions.invalidate_presentation();
                continue;
            }
        }
    }
}

async fn finish_signal_after_cleanup(
    signal: UiSignal,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    defer_suspend: bool,
) -> Result<TurnDisposition, InteractiveError> {
    match signal {
        UiSignal::Interrupt => Ok(TurnDisposition::Continue),
        UiSignal::Suspend if defer_suspend => Ok(TurnDisposition::Signal(UiSignal::Suspend)),
        UiSignal::Suspend => match suspend_and_resume(terminal, signals).await? {
            Some(signal) => Ok(TurnDisposition::Signal(signal)),
            None => Ok(TurnDisposition::Continue),
        },
        UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate => {
            Ok(TurnDisposition::Signal(signal))
        }
    }
}

fn observe_signal(stop: &mut Option<StopIntent>, signal: UiSignal) {
    match signal {
        UiSignal::Hangup | UiSignal::Quit | UiSignal::Terminate => {
            if !matches!(stop, Some(StopIntent::Exit(_))) {
                *stop = Some(StopIntent::Exit(signal));
            }
        }
        UiSignal::Suspend => {
            if stop.is_none() || matches!(stop, Some(StopIntent::Interrupt)) {
                *stop = Some(StopIntent::Suspend);
            }
        }
        UiSignal::Interrupt => {
            if stop.is_none() {
                *stop = Some(StopIntent::Interrupt);
            }
        }
    }
}

fn drain_active_signals(signals: &mut SignalStreams, stop: &mut Option<StopIntent>) {
    // Tokio coalesces each of the five installed signal classes. Bounding the
    // drain prevents a signal flood from starving the owned Agent cleanup.
    for _ in 0..5 {
        let Some(signal) = signals.next().now_or_never() else {
            break;
        };
        observe_signal(stop, signal);
    }
}

fn observe_failure(stop: &mut Option<StopIntent>, error: InteractiveError) {
    if !matches!(stop, Some(StopIntent::Exit(_))) {
        *stop = Some(StopIntent::Failure(error));
    }
}

fn latch_observer_fault(
    events: &CommittedUiReceiver,
    stop: &mut Option<StopIntent>,
    cancellation: &CancellationToken,
) -> bool {
    if !events.is_producer_faulted() {
        return false;
    }
    observe_failure(stop, InteractiveError::Agent);
    cancellation.cancel();
    true
}

fn discard_ready_updates_after_stop(
    events: &mut CommittedUiReceiver,
    expected_start: crate::session::EventSeq,
    expected_prompt: &str,
    prompt_committed: &mut bool,
) -> Result<usize, InteractiveError> {
    let mut skipped = 0_usize;
    while let Ok(event) = events.try_recv() {
        if event.seq.get() < expected_start.get() {
            return Err(InteractiveError::Agent);
        }
        if let CommittedUiKind::UserMessage {
            source: UiUserSource::Human,
            content,
        } = &event.kind
        {
            if content.as_str() != Some(expected_prompt) {
                return Err(InteractiveError::Agent);
            }
            *prompt_committed = true;
        }
        skipped = skipped.saturating_add(1);
    }
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{
        AfterFrame, ApprovalUiUpdate, FileSuggestionController, InlineIntent, InteractiveError,
        InteractiveExit, InteractivePresentation, PendingInlineOutput, PendingOutput,
        StagedFileSuggestionPresentation, StopIntent, SurfaceCommit, apply_approval_update,
        apply_enhanced_input as apply_enhanced_input_with_files, apply_theme_command,
        commit_surface, discard_ready_updates_after_stop,
        expire_enhanced_escape as expire_enhanced_escape_with_files, latch_observer_fault,
        observe_enhanced_cleanup_signal, observe_failure, observe_signal,
        prepare_pending_for_resize, presentation_uses_enhanced, reset_file_suggestion_decoder,
        session_context_estimate, turn_exhausted_session_capacity,
    };
    use crate::{
        agent::{ApprovalPrompt, ApprovalRequest},
        cli::{
            approval::{ApprovalChallengePool, ApprovalEnvelope},
            approval_join::ApprovalJoin,
            input::CanonicalRecordParser,
            signal::{SignalLatch, UiSignal},
            terminal::TerminalSize,
        },
        entropy::{EntropyError, EntropySource},
        model::{CallId, ContentBlock, LlmFailure, Message, MessageSource, NonNegativeSafeInteger},
        session::{
            ApprovalOutcome, ApprovalRequestId, EventKind, MAX_SESSION_EVENTS, NewEvent,
            RequestContext, Session, SurfaceIntent, TurnEndReason,
        },
        tools::WorkspaceFileCatalogue,
        tui::{
            command_palette::{
                CommandId, CommandPaletteSnapshot, CommandPaletteState, PaletteMove,
            },
            dock::{DockApprovalSelection, DockInteraction},
            file_suggestions::FileSuggestionSnapshot,
            inline_screen::InlineScreen,
            input_memory::InputMemory,
            key_decoder::KeyDecoder,
            theme::{ThemeCommand, ThemePalette, ThemeState},
            view::{ViewMode, ViewState},
        },
        workspace_authority::WorkspaceAuthority,
    };
    use tokio::{sync::oneshot, time::Instant};

    struct FileSuggestionWorkspace(std::path::PathBuf);

    impl FileSuggestionWorkspace {
        fn with_files(paths: &[&str]) -> Self {
            let root = std::env::temp_dir().join(format!(
                "dsh-interactive-file-suggestions-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&root).unwrap();
            for path in paths {
                let target = root.join(path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(target, "safe\n").unwrap();
            }
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn controller(&self) -> FileSuggestionController {
            let authority = WorkspaceAuthority::open(self.path()).unwrap();
            FileSuggestionController::new(WorkspaceFileCatalogue::from_authority(authority))
        }
    }

    impl Drop for FileSuggestionWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    async fn present_ready_file_menu(
        controller: &mut FileSuggestionController,
        input: &InputMemory,
    ) {
        controller.sync(input.composer(), false, false).unwrap();
        for _ in 0..4 {
            if !controller.has_job() {
                break;
            }
            let settlement = controller.wait_job().await;
            controller.accept_job(settlement).unwrap();
        }
        assert!(!controller.has_job());
        let staged = controller.stage_presentation(true).unwrap();
        controller.commit_presentation(staged);
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_enhanced_input(
        decoder: &mut KeyDecoder,
        bytes: &[u8],
        input: &mut InputMemory,
        command_palette: &mut CommandPaletteState,
        view: &mut ViewState,
        theme: &ThemeState,
        size: TerminalSize,
        notice: &mut Option<String>,
    ) -> Result<super::EnhancedInputAction, InteractiveError> {
        apply_enhanced_input_with_files(
            decoder,
            bytes,
            input,
            command_palette,
            None,
            view,
            theme,
            size,
            notice,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn expire_enhanced_escape(
        decoder: &mut KeyDecoder,
        input: &mut InputMemory,
        command_palette: &mut CommandPaletteState,
        view: &mut ViewState,
        theme: &ThemeState,
        size: TerminalSize,
        notice: &mut Option<String>,
    ) -> Result<super::EnhancedInputAction, InteractiveError> {
        expire_enhanced_escape_with_files(
            decoder,
            input,
            command_palette,
            None,
            view,
            theme,
            size,
            notice,
        )
    }

    fn fill(bytes: &mut [u8]) -> Result<(), EntropyError> {
        bytes.fill(0);
        Ok(())
    }

    #[test]
    fn terminating_signals_override_local_stops_but_not_each_other() {
        let mut stop = Some(StopIntent::Interrupt);
        observe_signal(&mut stop, UiSignal::Terminate);
        assert_eq!(stop, Some(StopIntent::Exit(UiSignal::Terminate)));
        observe_signal(&mut stop, UiSignal::Hangup);
        assert_eq!(stop, Some(StopIntent::Exit(UiSignal::Terminate)));
    }

    #[test]
    fn enhanced_startup_has_an_exact_polished_geometry_threshold() {
        for presentation in [
            InteractivePresentation::Auto,
            InteractivePresentation::Enhanced,
        ] {
            assert!(presentation_uses_enhanced(
                presentation,
                Some(TerminalSize {
                    rows: 12,
                    columns: 44,
                })
            ));
            for size in [
                TerminalSize {
                    rows: 12,
                    columns: 43,
                },
                TerminalSize {
                    rows: 11,
                    columns: 44,
                },
            ] {
                assert!(!presentation_uses_enhanced(presentation, Some(size)));
            }
        }
        assert!(!presentation_uses_enhanced(
            InteractivePresentation::Linear,
            Some(TerminalSize {
                rows: 24,
                columns: 120,
            })
        ));
        assert!(!presentation_uses_enhanced(
            InteractivePresentation::Auto,
            None
        ));
    }

    #[test]
    fn view_transitions_discard_same_read_submission_and_wait_for_screen_commit() {
        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        let mut command_palette = CommandPaletteState::default();
        input.insert_text("SAFE_DRAFT").unwrap();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let mut notice = None;
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };

        let action = apply_enhanced_input(
            &mut decoder,
            b"\x0f\r",
            &mut input,
            &mut command_palette,
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(action, super::EnhancedInputAction::Redraw);
        assert_eq!(view.requested().mode(), ViewMode::Inspect);
        assert_eq!(view.committed().mode(), ViewMode::Focus);
        assert_eq!(input.composer().text(), "SAFE_DRAFT");
        assert_eq!(input.queue().len(), 0);

        let transition_input = apply_enhanced_input(
            &mut decoder,
            b"hidden\r",
            &mut input,
            &mut command_palette,
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(transition_input, super::EnhancedInputAction::Redraw);
        assert_eq!(input.composer().text(), "SAFE_DRAFT");
        assert_eq!(input.queue().len(), 0);

        let inspect = view.requested();
        assert!(view.commit(inspect, 0, 100, 10));
        let page = apply_enhanced_input(
            &mut decoder,
            b"\x1b[6~\r",
            &mut input,
            &mut command_palette,
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(page, super::EnhancedInputAction::Redraw);
        assert_eq!(view.requested().offset(), 10);
        assert_eq!(input.composer().text(), "SAFE_DRAFT");
    }

    #[test]
    fn exact_palette_catalogue_is_the_local_submission_classifier() {
        for command in CommandId::ALL {
            assert_eq!(
                super::classify_enhanced_submission(command.command()),
                super::EnhancedSubmission::Command(command)
            );
        }
        assert_eq!(
            super::classify_enhanced_submission("/theme paper"),
            super::EnhancedSubmission::Theme(ThemeCommand::Select(ThemePalette::Paper))
        );
        assert_eq!(
            super::classify_enhanced_submission("/not-local"),
            super::EnhancedSubmission::Prompt
        );
    }

    #[test]
    fn command_palette_navigation_completion_and_paste_require_fresh_reads() {
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let theme = ThemeState::default();
        let mut view = ViewState::default();
        let mut notice = None;

        for (bytes, selected) in [
            (b"\x1b[A\r".as_slice(), CommandId::Help),
            (b"\x1b[B\r".as_slice(), CommandId::Inspect),
            (b"\t\r".as_slice(), CommandId::Inspect),
            (b"\x1b[Z\r".as_slice(), CommandId::Help),
        ] {
            let mut decoder = KeyDecoder::default();
            let mut input = InputMemory::default();
            input.insert_text("/").unwrap();
            let mut palette = CommandPaletteState::default();
            assert_eq!(
                apply_enhanced_input(
                    &mut decoder,
                    bytes,
                    &mut input,
                    &mut palette,
                    &mut view,
                    &theme,
                    size,
                    &mut notice,
                )
                .unwrap(),
                super::EnhancedInputAction::RedrawFence
            );
            assert_eq!(
                palette.snapshot(input.composer()).selected(),
                Some(selected)
            );
            assert_eq!(input.composer().text(), "/");
            assert_eq!(input.queue().len(), 0);
        }

        for bytes in [
            b"\x1b[A\r".as_slice(),
            b"\x1b[B\r".as_slice(),
            b"\t\r".as_slice(),
            b"\x1b[Z\r".as_slice(),
        ] {
            let mut decoder = KeyDecoder::default();
            let mut input = InputMemory::default();
            input.insert_text("/unknown").unwrap();
            let mut palette = CommandPaletteState::default();
            assert_eq!(
                apply_enhanced_input(
                    &mut decoder,
                    bytes,
                    &mut input,
                    &mut palette,
                    &mut view,
                    &theme,
                    size,
                    &mut notice,
                )
                .unwrap(),
                super::EnhancedInputAction::RedrawFence
            );
            assert_eq!(palette.snapshot(input.composer()).selected(), None);
            assert_eq!(input.composer().text(), "/unknown");
            assert_eq!(input.queue().len(), 0);
        }

        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        input.insert_text("/exit").unwrap();
        let mut palette = CommandPaletteState::default();
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\x1b[B\r",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::RedrawFence
        );
        assert_eq!(input.composer().text(), "/exit");
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\r",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Submit
        );

        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        input.insert_text("/qu").unwrap();
        let mut palette = CommandPaletteState::default();
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\r\r",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::RedrawFence
        );
        assert_eq!(input.composer().text(), "/quit");
        assert_eq!(input.queue().len(), 0);

        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        let mut palette = CommandPaletteState::default();
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\x1b[200~/exit\x1b[201~\r",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::PasteFence
        );
        assert_eq!(input.composer().text(), "/exit");
        assert_eq!(input.queue().len(), 0);
    }

    #[tokio::test]
    async fn file_menu_uses_only_presented_rows_and_fences_same_read_enter() {
        let workspace = FileSuggestionWorkspace::with_files(&["a.rs", "b.rs"]);
        let mut controller = workspace.controller();
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        present_ready_file_menu(&mut controller, &input).await;
        let mut decoder = KeyDecoder::default();
        let mut palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let mut notice = None;

        let action = apply_enhanced_input_with_files(
            &mut decoder,
            b"\x1b[B\r",
            &mut input,
            &mut palette,
            Some(&mut controller),
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(action, super::EnhancedInputAction::RedrawFence);
        assert_eq!(input.composer().text(), "@");

        let staged = controller.stage_presentation(true).unwrap();
        controller.commit_presentation(staged);
        let action = apply_enhanced_input_with_files(
            &mut decoder,
            b"\r",
            &mut input,
            &mut palette,
            Some(&mut controller),
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(action, super::EnhancedInputAction::RedrawFence);
        assert_eq!(input.composer().text(), "@b.rs ");
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stale_real_menu_and_invalidated_screen_cannot_pick_or_submit() {
        let workspace = FileSuggestionWorkspace::with_files(&["a.rs"]);
        let mut controller = workspace.controller();
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        present_ready_file_menu(&mut controller, &input).await;
        let mut decoder = KeyDecoder::default();
        let mut palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let mut notice = None;

        let action = apply_enhanced_input_with_files(
            &mut decoder,
            b"x\r",
            &mut input,
            &mut palette,
            Some(&mut controller),
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(action, super::EnhancedInputAction::RedrawFence);
        assert_eq!(input.composer().text(), "@x");
        assert_eq!(input.queue().len(), 0);

        controller.invalidate_presentation();
        let action = apply_enhanced_input_with_files(
            &mut decoder,
            b"\rhidden",
            &mut input,
            &mut palette,
            Some(&mut controller),
            &mut view,
            &theme,
            size,
            &mut notice,
        )
        .unwrap();
        assert_eq!(action, super::EnhancedInputAction::Redraw);
        assert_eq!(input.composer().text(), "@x");
        assert_eq!(input.queue().len(), 0);
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn presentation_invalidation_clears_an_old_escape_epoch_before_recovery_input() {
        let workspace = FileSuggestionWorkspace::with_files(&["a.rs"]);
        let mut controller = workspace.controller();
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        present_ready_file_menu(&mut controller, &input).await;
        let mut decoder = KeyDecoder::default();
        let mut palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let mut notice = None;

        assert_eq!(
            apply_enhanced_input_with_files(
                &mut decoder,
                b"\x1b",
                &mut input,
                &mut palette,
                Some(&mut controller),
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::None
        );
        assert!(decoder.escape_pending());
        let mut deadline = Some(Instant::now());
        controller.invalidate_presentation();
        let recovered = controller.stage_presentation(true).unwrap();
        controller.commit_presentation(recovered);
        assert!(controller.decoder_reset_required());

        reset_file_suggestion_decoder(&mut controller, Some(&mut decoder), &mut deadline).unwrap();
        assert!(!decoder.escape_pending());
        assert_eq!(deadline, None);
        assert!(!controller.decoder_reset_required());
        assert!(controller.presented_menu_is_visible());
        controller.finish_shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn presented_loading_enter_keeps_the_ordinary_submit_path() {
        let workspace = FileSuggestionWorkspace::with_files(&["a.rs"]);
        let mut controller = workspace.controller();
        let mut input = InputMemory::default();
        input.insert_text("@").unwrap();
        controller.sync(input.composer(), false, false).unwrap();
        let staged = controller.stage_presentation(true).unwrap();
        controller.commit_presentation(staged);
        let mut decoder = KeyDecoder::default();
        let mut palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let mut notice = None;

        assert_eq!(
            apply_enhanced_input_with_files(
                &mut decoder,
                b"\r",
                &mut input,
                &mut palette,
                Some(&mut controller),
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Submit
        );
        assert_eq!(input.composer().text(), "@");
        controller.finish_shutdown().await.unwrap();
    }

    #[test]
    fn command_palette_escape_and_rejected_input_are_revision_scoped_dismissals() {
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let theme = ThemeState::default();

        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        input.insert_text("/").unwrap();
        let mut palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let mut notice = None;
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\x1b",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::None
        );
        assert!(palette.snapshot(input.composer()).is_visible());
        assert_eq!(
            expire_enhanced_escape(
                &mut decoder,
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(
            palette.snapshot(input.composer()),
            CommandPaletteSnapshot::Hidden
        );
        assert_eq!(input.composer().text(), "/");
        input.insert_char('h').unwrap();
        assert!(palette.sync(input.composer()).is_visible());

        for bytes in [b"\0".as_slice(), b"\x1b[200~\xff\x1b[201~".as_slice()] {
            let mut decoder = KeyDecoder::default();
            let mut input = InputMemory::default();
            input.insert_text("/").unwrap();
            let mut palette = CommandPaletteState::default();
            assert!(palette.navigate(input.composer(), PaletteMove::Next));
            let mut view = ViewState::default();
            let mut notice = None;
            let action = apply_enhanced_input(
                &mut decoder,
                bytes,
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap();
            assert!(matches!(
                action,
                super::EnhancedInputAction::Redraw | super::EnhancedInputAction::PasteFence
            ));
            assert_eq!(
                palette.snapshot(input.composer()),
                CommandPaletteSnapshot::Hidden
            );
            assert_eq!(input.composer().text(), "/");
            assert_eq!(palette.snapshot(input.composer()).selected(), None);
            input.insert_char('h').unwrap();
            assert_eq!(
                palette.sync(input.composer()).selected(),
                Some(CommandId::Help)
            );
        }

        let mut input = InputMemory::default();
        input.insert_text("/").unwrap();
        let mut palette = CommandPaletteState::default();
        for _ in 0..6 {
            assert!(palette.navigate(input.composer(), PaletteMove::Next));
        }
        assert_eq!(
            palette.snapshot(input.composer()).selected(),
            Some(CommandId::Quit)
        );
        let mut decoder = KeyDecoder::default();
        let mut view = ViewState::default();
        let mut notice = None;
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\x1b[200~h\x1b[201~",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::PasteFence
        );
        assert_eq!(input.composer().text(), "/h");
        assert_eq!(
            palette.snapshot(input.composer()).selected(),
            Some(CommandId::Help)
        );
        let mut decoder = KeyDecoder::default();
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\0",
                &mut input,
                &mut palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(
            palette.snapshot(input.composer()),
            CommandPaletteSnapshot::Hidden
        );
        assert!(input.backspace().unwrap());
        assert_eq!(
            palette.sync(input.composer()).selected(),
            Some(CommandId::Help)
        );
    }

    #[test]
    fn command_palette_yields_to_detail_and_approval_and_survives_partial_redraw() {
        let mut input = InputMemory::default();
        input.insert_text("/").unwrap();
        let mut palette = CommandPaletteState::default();
        for _ in 0..4 {
            assert!(palette.navigate(input.composer(), PaletteMove::Next));
        }
        let snapshot = palette.snapshot(input.composer());
        assert_eq!(snapshot.selected(), Some(CommandId::Theme));

        let interaction =
            super::command_palette_interaction(DockInteraction::Running, snapshot, ViewMode::Focus);
        assert!(matches!(
            interaction,
            DockInteraction::CommandPalette {
                running: true,
                snapshot: visible,
            } if visible.selected() == Some(CommandId::Theme)
        ));
        assert_eq!(
            super::command_palette_interaction(interaction, snapshot, ViewMode::Focus),
            interaction
        );
        assert_eq!(
            super::command_palette_interaction(
                interaction,
                CommandPaletteSnapshot::Hidden,
                ViewMode::Focus,
            ),
            DockInteraction::Running
        );
        assert_eq!(
            super::command_palette_interaction(
                DockInteraction::Approval(DockApprovalSelection::Reject),
                snapshot,
                ViewMode::Focus,
            ),
            DockInteraction::Approval(DockApprovalSelection::Reject)
        );
        assert_eq!(
            super::command_palette_interaction(interaction, snapshot, ViewMode::Inspect),
            DockInteraction::Running
        );
        assert_eq!(
            super::command_palette_interaction(interaction, snapshot, ViewMode::Review),
            DockInteraction::Running
        );

        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let frame = super::enhanced_dock_frame(
            &input,
            None,
            interaction,
            size,
            FileSuggestionSnapshot::Hidden,
        )
        .unwrap();
        let mut screen = InlineScreen::default();
        let mut attach = screen
            .stage_attach(super::screen_size(size), &frame, ThemePalette::Adaptive)
            .unwrap();
        let attach_bytes = attach.bytes().len();
        attach.advance(attach_bytes).unwrap();
        screen.commit(attach).unwrap();

        let view = ViewState::default();
        let theme = ThemeState::default();
        let zero_write = screen.stage_dock(&frame, ThemePalette::Adaptive).unwrap();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write: zero_write,
            intent: InlineIntent::Dock(interaction),
            surface: SurfaceCommit {
                request: view.requested(),
                theme: theme.requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        assert!(!prepare_pending_for_resize(&mut pending, &mut screen).unwrap());
        assert!(!screen.is_poisoned());
        assert!(matches!(
            pending,
            Some(PendingOutput::Dock(candidate)) if candidate == interaction
        ));
        assert_eq!(
            palette.snapshot(input.composer()).selected(),
            Some(CommandId::Theme)
        );

        let mut write = screen.stage_dock(&frame, ThemePalette::Adaptive).unwrap();
        write.advance(1).unwrap();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::Dock(interaction),
            surface: SurfaceCommit {
                request: view.requested(),
                theme: theme.requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        assert!(prepare_pending_for_resize(&mut pending, &mut screen).unwrap());
        assert!(screen.is_poisoned());
        assert!(matches!(
            pending,
            Some(PendingOutput::Dock(DockInteraction::CommandPalette {
                running: true,
                snapshot: visible,
            })) if visible.selected() == Some(CommandId::Theme)
        ));
        assert_eq!(
            palette.snapshot(input.composer()).selected(),
            Some(CommandId::Theme)
        );

        screen.recover_after_visual_reset();
        let compact_size = TerminalSize {
            rows: 5,
            columns: 12,
        };
        let compact = super::enhanced_dock_frame(
            &input,
            None,
            interaction,
            compact_size,
            FileSuggestionSnapshot::Hidden,
        )
        .unwrap();
        let recovered = screen
            .stage_attach(
                super::screen_size(compact_size),
                &compact,
                ThemePalette::Adaptive,
            )
            .unwrap();
        assert!(
            recovered
                .bytes()
                .windows(b"> /theme".len())
                .any(|row| row == b"> /theme")
        );
    }

    #[test]
    fn theme_transition_discards_input_until_the_palette_redraw_commits() {
        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        let mut command_palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let mut theme = ThemeState::default();
        let mut notice = None;
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };

        apply_theme_command(
            ThemeCommand::Select(ThemePalette::Paper),
            &mut theme,
            &mut notice,
        )
        .unwrap();
        assert!(theme.is_transitioning());
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"HIDDEN\r",
                &mut input,
                &mut command_palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert!(input.composer().is_empty());

        assert!(theme.commit(theme.requested()));
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"fresh",
                &mut input,
                &mut command_palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(input.composer().text(), "fresh");
    }

    #[test]
    fn detail_escape_requires_a_fresh_input_after_returning_to_focus() {
        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        let mut command_palette = CommandPaletteState::default();
        input.insert_text("SAFE_DRAFT").unwrap();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        view.request_mode(ViewMode::Inspect).unwrap();
        assert!(view.commit(view.requested(), 0, 20, 10));
        let mut notice = None;
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };

        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\x1b",
                &mut input,
                &mut command_palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::None
        );
        assert!(decoder.escape_pending());
        assert_eq!(
            expire_enhanced_escape(
                &mut decoder,
                &mut input,
                &mut command_palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(view.requested().mode(), ViewMode::Focus);
        assert_eq!(view.committed().mode(), ViewMode::Inspect);
        assert_eq!(
            apply_enhanced_input(
                &mut decoder,
                b"\r",
                &mut input,
                &mut command_palette,
                &mut view,
                &theme,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(input.composer().text(), "SAFE_DRAFT");
        assert_eq!(input.queue().len(), 0);
    }

    #[test]
    fn partially_written_approval_resize_keeps_selection_after_visual_recovery() {
        let input = InputMemory::default();
        let interaction = DockInteraction::Approval(DockApprovalSelection::AllowOnce);
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let frame = super::enhanced_dock_frame(
            &input,
            None,
            interaction,
            size,
            FileSuggestionSnapshot::Hidden,
        )
        .unwrap();
        let mut screen = InlineScreen::default();
        let mut attach = screen
            .stage_attach(super::screen_size(size), &frame, ThemePalette::Adaptive)
            .unwrap();
        let attach_bytes = attach.bytes().len();
        attach.advance(attach_bytes).unwrap();
        screen.commit(attach).unwrap();

        let mut write = screen.stage_dock(&frame, ThemePalette::Paper).unwrap();
        let mut theme = ThemeState::default();
        theme.request(ThemePalette::Paper).unwrap();
        write.advance(1).unwrap();
        let mut view = ViewState::default();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::Dock(interaction),
            surface: SurfaceCommit {
                request: view.requested(),
                theme: theme.requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        assert!(prepare_pending_for_resize(&mut pending, &mut screen).unwrap());
        assert!(screen.is_poisoned());
        assert_eq!(theme.requested().palette(), ThemePalette::Paper);
        assert_eq!(theme.committed().palette(), ThemePalette::Adaptive);
        assert!(theme.is_transitioning());
        assert!(matches!(
            pending,
            Some(PendingOutput::Dock(DockInteraction::Approval(
                DockApprovalSelection::AllowOnce
            )))
        ));

        screen.recover_after_visual_reset();
        let compact_size = TerminalSize {
            rows: 6,
            columns: 15,
        };
        let compact = super::enhanced_dock_frame(
            &input,
            None,
            interaction,
            compact_size,
            FileSuggestionSnapshot::Hidden,
        )
        .unwrap();
        let mut second_resize = screen
            .stage_attach(
                super::screen_size(compact_size),
                &compact,
                theme.requested().palette(),
            )
            .unwrap();
        second_resize.advance(1).unwrap();
        screen.abort(second_resize);
        assert!(screen.is_poisoned());
        screen.recover_after_visual_reset();
        let mut recovered = screen
            .stage_attach(
                super::screen_size(compact_size),
                &compact,
                theme.requested().palette(),
            )
            .unwrap();
        assert!(
            recovered
                .bytes()
                .windows(b"> Allow".len())
                .any(|row| row == b"> Allow")
        );
        assert!(
            !recovered
                .bytes()
                .windows(b"> Reject".len())
                .any(|row| row == b"> Reject")
        );
        assert!(
            recovered
                .bytes()
                .windows(
                    ThemePalette::Paper
                        .sgr(crate::tui::presentation::TextStyle::Selection)
                        .len()
                )
                .any(|bytes| bytes
                    == ThemePalette::Paper
                        .sgr(crate::tui::presentation::TextStyle::Selection)
                        .as_bytes())
        );
        let recovered_bytes = recovered.bytes().len();
        recovered.advance(recovered_bytes).unwrap();
        screen.commit(recovered).unwrap();
        let recovered_surface = SurfaceCommit {
            request: view.requested(),
            theme: theme.requested(),
            offset: 0,
            total_rows: 0,
            page_rows: 0,
        };
        commit_surface(&mut view, &mut theme, recovered_surface);
        assert_eq!(theme.committed().palette(), ThemePalette::Paper);
        assert!(!theme.is_transitioning());
    }

    #[test]
    fn output_failure_is_preserved_unless_a_terminating_signal_wins() {
        let mut stop = None;
        observe_failure(&mut stop, InteractiveError::Output);
        observe_signal(&mut stop, UiSignal::Interrupt);
        assert_eq!(stop, Some(StopIntent::Failure(InteractiveError::Output)));
        observe_signal(&mut stop, UiSignal::Quit);
        assert_eq!(stop, Some(StopIntent::Exit(UiSignal::Quit)));
    }

    #[test]
    fn terminating_signal_observed_during_visual_reset_overrides_output_failure() {
        let mut result = Err(InteractiveError::Output);
        let mut signals = SignalLatch::default();
        observe_enhanced_cleanup_signal(&mut result, &mut signals, UiSignal::Interrupt);
        assert_eq!(result, Err(InteractiveError::Output));
        observe_enhanced_cleanup_signal(&mut result, &mut signals, UiSignal::Terminate);
        assert_eq!(result, Ok(InteractiveExit::Signal(UiSignal::Terminate)));
        observe_enhanced_cleanup_signal(&mut result, &mut signals, UiSignal::Hangup);
        assert_eq!(result, Ok(InteractiveExit::Signal(UiSignal::Terminate)));
        assert_eq!(signals.observed(), Some(UiSignal::Terminate));

        let mut ordinary = Ok(InteractiveExit::Ordinary(7));
        let mut local_signals = SignalLatch::default();
        observe_enhanced_cleanup_signal(&mut ordinary, &mut local_signals, UiSignal::Interrupt);
        observe_enhanced_cleanup_signal(&mut ordinary, &mut local_signals, UiSignal::Suspend);
        assert_eq!(ordinary, Ok(InteractiveExit::Ordinary(7)));
        assert_eq!(local_signals.observed(), Some(UiSignal::Suspend));
    }

    #[tokio::test]
    async fn invalid_selector_input_rearms_before_a_later_decision() {
        let challenges =
            ApprovalChallengePool::from_entropy(EntropySource::injected(fill)).unwrap();
        let mut joins = ApprovalJoin::new(challenges).unwrap();
        joins.begin_turn().unwrap();
        let request = ApprovalRequest::new(
            ApprovalRequestId::new("approval-hostile"),
            "apply_patch".to_owned(),
            CallId::new("call-hostile"),
            &ApprovalPrompt::new(Some("change one file".to_owned()), "bounded preview").unwrap(),
        );
        let (response, mut receive) = oneshot::channel();
        joins
            .receive_envelope(ApprovalEnvelope { request, response })
            .unwrap();
        joins
            .observe_asked(
                "approval-hostile".to_owned(),
                "apply_patch".to_owned(),
                Some("call-hostile".to_owned()),
                Some("change one file".to_owned()),
            )
            .unwrap();
        let mut parser = CanonicalRecordParser::new(64);
        let mut pending = None;
        let mut deadline = None;
        let mut after = AfterFrame::None;

        apply_approval_update(
            ApprovalUiUpdate::Invalid,
            &mut joins,
            &mut parser,
            &mut pending,
            &mut deadline,
            &mut after,
        )
        .unwrap();
        assert!(pending.is_some());
        assert_eq!(after, AfterFrame::ApprovalFence);
        assert!(matches!(
            receive.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        pending = None;
        deadline = None;
        after = AfterFrame::None;
        apply_approval_update(
            ApprovalUiUpdate::Decide(ApprovalOutcome::AllowedOnce),
            &mut joins,
            &mut parser,
            &mut pending,
            &mut deadline,
            &mut after,
        )
        .unwrap();
        assert!(pending.is_none());
        assert_eq!(after, AfterFrame::None);
        assert_eq!(receive.await.unwrap().outcome, ApprovalOutcome::AllowedOnce);
    }

    #[test]
    fn observer_fault_cancels_once_and_discards_the_existing_fifo_without_deadlines() {
        let mut session = Session::new("interactive-observer-fault").unwrap();
        let mut events = session.attach_ui_observer().unwrap();
        for _ in 0..MAX_SESSION_EVENTS - 1 {
            session.append(NewEvent::log(EventKind::EndSeed)).unwrap();
        }
        events.fail_next_projection_for_test();
        session.append(NewEvent::log(EventKind::EndSeed)).unwrap();
        assert_eq!(session.events().len(), MAX_SESSION_EVENTS);

        let cancellation = tokio_util::sync::CancellationToken::new();
        let mut stop = None;
        assert!(latch_observer_fault(&events, &mut stop, &cancellation));
        assert!(cancellation.is_cancelled());
        assert_eq!(stop, Some(StopIntent::Failure(InteractiveError::Agent)));
        let mut prompt_committed = false;
        assert_eq!(
            discard_ready_updates_after_stop(
                &mut events,
                crate::session::EventSeq::new(0).unwrap(),
                "not present",
                &mut prompt_committed,
            )
            .unwrap(),
            MAX_SESSION_EVENTS - 1
        );
        assert_eq!(
            discard_ready_updates_after_stop(
                &mut events,
                crate::session::EventSeq::new(0).unwrap(),
                "not present",
                &mut prompt_committed,
            )
            .unwrap(),
            0
        );
        assert!(!prompt_committed);
    }

    #[test]
    fn stopped_turn_still_recognizes_a_committed_prompt_waiting_in_the_ui_fifo() {
        let mut session = Session::new("interactive-stop-admission-race").unwrap();
        let mut events = session.attach_ui_observer().unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(
                crate::session::TurnId::new(1).unwrap(),
            )))
            .unwrap();
        let message = Message::user(
            "user-stop-race",
            vec![ContentBlock::text("already committed").unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        session
            .append(NewEvent::surface(
                EventKind::user_message(message),
                SurfaceIntent::append(),
            ))
            .unwrap();

        let mut prompt_committed = false;
        let skipped = discard_ready_updates_after_stop(
            &mut events,
            crate::session::EventSeq::new(0).unwrap(),
            "already committed",
            &mut prompt_committed,
        )
        .unwrap();
        assert_eq!(skipped, 2);
        assert!(prompt_committed);
    }

    #[test]
    fn context_estimate_requires_the_exact_session_boundary_and_handles_zero_window() {
        let mut session = Session::new("interactive-context-estimate").unwrap();
        let initial = session_context_estimate(
            &session,
            None,
            Some(crate::session::EventSeq::new(0).unwrap()),
        )
        .unwrap();
        let initial_status = initial.status_line().unwrap();
        assert!(initial_status.contains("Session context estimate 0"));
        assert!(initial_status.contains("sampled before seq 0"));
        assert!(!initial_status.contains('%'));
        assert!(
            session_context_estimate(
                &session,
                None,
                Some(crate::session::EventSeq::new(1).unwrap()),
            )
            .is_none()
        );

        let turn = crate::session::TurnId::new(1).unwrap();
        let step = crate::session::StepId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_start(turn, step)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::RequestContext {
                context: RequestContext::new(
                    "deepseek-official",
                    "deepseek-chat",
                    Some(NonNegativeSafeInteger::new(0).unwrap()),
                )
                .unwrap(),
            }))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_end(turn, step)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();
        let estimate = session_context_estimate(
            &session,
            Some(turn),
            Some(crate::session::EventSeq::new(5).unwrap()),
        )
        .unwrap();
        let status = estimate.status_line().unwrap();
        assert!(status.contains("deepseek-chat"));
        assert!(status.contains("after turn 1"));
        assert!(status.contains("sampled before seq 5"));
        assert!(!status.contains('%'));
    }

    #[test]
    fn only_the_terminal_session_capacity_failure_forces_exit_after_rendering() {
        let exhausted = TurnEndReason::Error {
            error: LlmFailure::new(
                "the session has no safe room for another agent event",
                "AGENT_EVENT_BUDGET",
            )
            .unwrap(),
        };
        let ordinary = TurnEndReason::Error {
            error: LlmFailure::new("provider failed", "SERVER").unwrap(),
        };
        assert!(turn_exhausted_session_capacity(&exhausted));
        assert!(!turn_exhausted_session_capacity(&ordinary));
        assert!(!turn_exhausted_session_capacity(&TurnEndReason::Completed));
    }
}
