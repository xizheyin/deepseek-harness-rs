use std::time::Duration;
use std::{mem, ops::ControlFlow};

use futures_util::FutureExt as _;
use thiserror::Error;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::AgentLoop,
    goal::{GoalCommand, GoalError, GoalRound, GoalRuntime},
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
        motion::{
            MotionCommand, MotionPreference, MotionRequest, MotionState, WorkingAge, WorkingPhase,
            WorkingPresentation,
        },
        theme::{ThemeCommand, ThemePalette, ThemeRequest, ThemeState},
        view::{ContextEstimate, ViewMode, ViewRequest, ViewState},
    },
    user_question::{MAX_CUSTOM_ANSWER_BYTES, UserQuestionEnvelope, UserQuestionReceiver},
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
    identity::{prepare_goal_turn, prepare_user_turn},
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
    user_question::{QuestionAcceptingMode, QuestionInputUpdate, UserQuestionUiState},
};

const FRAME_DEADLINE: Duration = Duration::from_secs(5);
const VISUAL_RESET_DEADLINE: Duration = Duration::from_millis(250);
const APPROVAL_INPUT_QUIET: Duration = Duration::from_millis(100);
const PASTE_INPUT_QUIET: Duration = Duration::from_millis(100);
const MOTION_DELAY: Duration = Duration::from_millis(300);
const MOTION_INTERVAL: Duration = Duration::from_millis(125);
const MOTION_ONE_SECOND: Duration = Duration::from_secs(1);
const MOTION_LONG_WAIT: Duration = Duration::from_secs(5);

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
    MotionDock {
        interaction: DockInteraction,
        working: WorkingPresentation,
    },
    Inline(PendingInlineOutput),
}

enum InlineIntent {
    Transcript(PreparedPresentation),
    Dock(DockInteraction),
    MotionDock {
        interaction: DockInteraction,
        working: WorkingPresentation,
    },
}

struct PendingInlineOutput {
    write: PendingScreenWrite,
    intent: InlineIntent,
    surface: SurfaceCommit,
    working: Option<WorkingPresentation>,
    file_suggestions: StagedFileSuggestionPresentation,
}

#[derive(Clone, Copy, Debug)]
struct SurfaceCommit {
    request: ViewRequest,
    theme: ThemeRequest,
    motion: MotionRequest,
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
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) | Self::MotionDock { .. } => {
                &[]
            }
            Self::Linear(frame) => frame.bytes(),
            Self::Inline(output) => output.write.bytes(),
        }
    }

    fn advance(&mut self, count: usize) -> Result<(), InteractiveError> {
        match self {
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) | Self::MotionDock { .. } => {
                Err(InteractiveError::Agent)
            }
            Self::Linear(frame) => frame.advance(count).map_err(|_| InteractiveError::Output),
            Self::Inline(output) => output.write.advance(count).map_err(map_inline_screen_error),
        }
    }

    fn has_started(&self) -> bool {
        match self {
            Self::Unprepared(_) | Self::Prepared(_) | Self::Dock(_) | Self::MotionDock { .. } => {
                false
            }
            Self::Linear(_) => false,
            Self::Inline(output) => output.write.has_started(),
        }
    }

    fn is_motion_only(&self) -> bool {
        matches!(self, Self::MotionDock { .. })
            || matches!(
                self,
                Self::Inline(PendingInlineOutput {
                    intent: InlineIntent::MotionDock { .. },
                    ..
                })
            )
    }
}

impl InlineIntent {
    fn into_pending(self) -> PendingOutput {
        match self {
            Self::Transcript(presentation) => PendingOutput::Prepared(presentation),
            Self::Dock(interaction) => PendingOutput::Dock(interaction),
            Self::MotionDock {
                interaction,
                working,
            } => PendingOutput::MotionDock {
                interaction,
                working,
            },
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
    reduced_motion: bool,
) -> Result<u8, InteractiveError> {
    let enhanced = presentation_uses_enhanced(presentation, terminal.size());
    if enhanced {
        run_enhanced(assembly, terminal, signals, reduced_motion).await
    } else {
        run_linear(assembly, terminal, signals, false).await
    }
}

pub(super) fn presentation_uses_enhanced(
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
    reduced_motion: bool,
) -> Result<u8, InteractiveError> {
    let InteractiveAssembly {
        mut agent,
        mut events,
        mut approvals,
        mut user_questions,
        mut joins,
        session_id,
        resumed,
        file_suggestions,
        goal,
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
    let mut motion = MotionState::new(if reduced_motion {
        MotionPreference::Reduced
    } else {
        MotionPreference::Full
    });
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
        &mut motion,
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
            } else if input.composer().is_empty()
                && !auto_queue_paused
                && goal.is_armed().map_err(|_| InteractiveError::Agent)?
            {
                match goal.next_round().map_err(|_| InteractiveError::Agent)? {
                    Some(round) => EnhancedIdleEvent::AutoGoal(round),
                    None => EnhancedIdleEvent::GoalSettled,
                }
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
            let goal_round = match &event {
                EnhancedIdleEvent::AutoGoal(round) => Some(round.clone()),
                _ => None,
            };
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
                        motion: &mut motion,
                        command_palette: &mut command_palette,
                        file_suggestions: &mut file_suggestions,
                        palette_suppressed: false,
                        working: WorkingPresentation::STATIC,
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
                        &motion,
                        last_size,
                        &mut notice,
                    )?
                }
                EnhancedIdleEvent::AutoSubmit => EnhancedInputAction::Submit,
                EnhancedIdleEvent::AutoGoal(_) => EnhancedInputAction::Submit,
                EnhancedIdleEvent::GoalSettled => {
                    notice = Some("Goal stopped at its automatic round limit".to_owned());
                    EnhancedInputAction::Redraw
                }
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
                        &motion,
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
                        && goal_round.is_none()
                        && matches!(
                            &composer_submission,
                            EnhancedSubmission::Command(_)
                                | EnhancedSubmission::Theme(_)
                                | EnhancedSubmission::Motion(_)
                                | EnhancedSubmission::Goal(_)
                        );
                    let (draft, cursor) = if let Some(round) = &goal_round {
                        (round.prompt().to_owned(), 0)
                    } else if local_command {
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
                    let submission = if goal_round.is_some() {
                        EnhancedSubmission::Prompt
                    } else if local_command {
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
                                    "/goal [objective|edit|pause|resume|clear] | /inspect | /review | /focus | /theme | /motion | /help | /exit | /quit | Ctrl+O inspect"
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
                            CommandId::Motion => {
                                apply_motion_command(
                                    MotionCommand::Show,
                                    &mut motion,
                                    &mut notice,
                                )?;
                            }
                            CommandId::Goal => {
                                apply_goal_command(
                                    &mut agent,
                                    &goal,
                                    Ok(GoalCommand::Show),
                                    &mut notice,
                                )
                                .await;
                            }
                            CommandId::Exit | CommandId::Quit => {
                                break Ok(InteractiveExit::Ordinary(0));
                            }
                        },
                        EnhancedSubmission::Theme(command) => {
                            apply_theme_command(command, &mut theme, &mut notice)?;
                        }
                        EnhancedSubmission::Motion(command) => {
                            apply_motion_command(command, &mut motion, &mut notice)?;
                        }
                        EnhancedSubmission::Goal(command) => {
                            apply_goal_command(&mut agent, &goal, command, &mut notice).await;
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
                                motion: &mut motion,
                                command_palette: &mut command_palette,
                                file_suggestions: &mut file_suggestions,
                                palette_suppressed: false,
                                working: WorkingPresentation::STATIC,
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
                                None,
                            )
                            .await?
                            {
                                if let Some(round) = &goal_round {
                                    pause_goal_after_round_failure(
                                        &mut agent,
                                        &goal,
                                        round.revision(),
                                        round.number(),
                                        false,
                                    )
                                    .await?;
                                } else if let Some(id) = queued_id {
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
                            let turn_result = run_turn(ActiveTurn {
                                agent: &mut agent,
                                events: &mut events,
                                approvals: &mut approvals,
                                user_questions: &mut user_questions,
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
                                goal: &goal,
                                goal_round: goal_round
                                    .as_ref()
                                    .map(|round| (round.revision(), round.number())),
                            })
                            .await;
                            let disposition = turn_result?;
                            if matches!(
                                disposition,
                                TurnDisposition::Continue
                                    | TurnDisposition::Signal(UiSignal::Suspend)
                            ) {
                                if goal_round.is_none() {
                                    settle_enhanced_prompt(
                                        &mut input,
                                        queued_id,
                                        draft,
                                        cursor,
                                        prompt_committed,
                                        &mut notice,
                                        &mut auto_queue_paused,
                                    )?;
                                }
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
                                        motion: &mut motion,
                                        command_palette: &mut command_palette,
                                        file_suggestions: &mut file_suggestions,
                                        palette_suppressed: false,
                                        working: WorkingPresentation::STATIC,
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
                &mut motion,
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
                            &mut motion,
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
    AutoGoal(GoalRound),
    GoalSettled,
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum EnhancedSubmission {
    Empty,
    Command(CommandId),
    Theme(ThemeCommand),
    Motion(MotionCommand),
    Goal(Result<GoalCommand, GoalError>),
    Prompt,
}

fn classify_enhanced_submission(prompt: &str) -> EnhancedSubmission {
    let command = prompt.trim_matches(|character: char| character.is_ascii_whitespace());
    if command.is_empty() {
        EnhancedSubmission::Empty
    } else if let Some(command) = CommandId::from_exact(command) {
        EnhancedSubmission::Command(command)
    } else if let Some(goal) = GoalCommand::parse(command) {
        EnhancedSubmission::Goal(goal)
    } else if let Some(theme) = ThemeCommand::parse(command) {
        EnhancedSubmission::Theme(theme)
    } else if let Some(motion) = MotionCommand::parse(command) {
        EnhancedSubmission::Motion(motion)
    } else {
        EnhancedSubmission::Prompt
    }
}

async fn apply_goal_command(
    agent: &mut AgentLoop,
    goal: &GoalRuntime,
    command: Result<GoalCommand, GoalError>,
    notice: &mut Option<String>,
) {
    *notice = Some(
        match command.and_then(|command| goal.prepare_command(command)) {
            Ok(crate::goal::GoalCommandPreparation::Show(message)) => Ok(message),
            Ok(crate::goal::GoalCommandPreparation::Mutation(mutation)) => agent
                .commit_goal_mutation(mutation)
                .await
                .map_err(|error| GoalError::Commit(error.to_string())),
            Err(error) => Err(error),
        }
        .unwrap_or_else(|error| format!("Goal error · {error}")),
    );
}

async fn pause_goal_after_round_failure(
    agent: &mut AgentLoop,
    goal: &GoalRuntime,
    revision: u64,
    round: u32,
    prompt_committed: bool,
) -> Result<(), InteractiveError> {
    if !prompt_committed {
        goal.rollback_uncommitted_round(revision, round)
            .map_err(|_| InteractiveError::Agent)?;
    }
    match goal.prepare_update(revision, crate::goal::GoalUpdate::Pause, None) {
        Ok(mutation) => {
            agent
                .commit_goal_mutation(mutation)
                .await
                .map_err(|_| InteractiveError::Agent)?;
        }
        Err(GoalError::InvalidTransition | GoalError::StaleRevision | GoalError::Missing) => {}
        Err(_) => return Err(InteractiveError::Agent),
    }
    Ok(())
}

fn apply_active_goal_command(
    goal: &GoalRuntime,
    command: Result<GoalCommand, GoalError>,
    notice: &mut Option<String>,
) {
    *notice = Some(match command {
        Ok(GoalCommand::Show) => match goal.apply_command(GoalCommand::Show) {
            Ok(message) => message,
            Err(error) => format!("Goal error · {error}"),
        },
        Ok(_) => "Goal error · wait for the current turn or press Ctrl+C before changing the Goal"
            .to_owned(),
        Err(error) => format!("Goal error · {error}"),
    });
}

const MOTION_LIST_NOTICE: &str = "Motion modes · full · reduced";

fn apply_motion_command(
    command: MotionCommand,
    motion: &mut MotionState,
    notice: &mut Option<String>,
) -> Result<(), InteractiveError> {
    *notice = Some(match command {
        MotionCommand::Show => format!(
            "Motion · {} | {MOTION_LIST_NOTICE}",
            motion.requested().preference().name()
        ),
        MotionCommand::Select(preference) => {
            let changed = motion
                .request(preference)
                .map_err(|_| InteractiveError::Output)?;
            if changed {
                format!("Motion changed · {}", preference.name())
            } else {
                format!("Motion already active · {}", preference.name())
            }
        }
        MotionCommand::Invalid => format!("Unknown motion mode | {MOTION_LIST_NOTICE}"),
    });
    Ok(())
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
    motion: &MotionState,
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
    if view.requested() != view.committed() || theme.is_transitioning() || motion.is_transitioning()
    {
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
    motion: &MotionState,
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
    if view.requested() != view.committed() || theme.is_transitioning() || motion.is_transitioning()
    {
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
                "/inspect · /review · /focus · /theme · /motion · /help · /exit · /quit · Enter send · Ctrl+J newline"
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
    motion: &mut MotionState,
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
            && !matches!(
                model.interaction,
                DockInteraction::QuestionCustom { .. }
                    | DockInteraction::Approval(_)
                    | DockInteraction::ExactShellApproval(_)
            );
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
            motion,
            model.live,
            file_snapshot,
            WorkingPresentation::PLAIN,
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
                commit_surface(view, theme, motion, surface.commit);
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

#[allow(clippy::too_many_arguments)]
async fn render_active_dock(
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    live: &LiveRenderer,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    dock: &mut ActiveDock<'_>,
    mut motion_clock: Option<&mut MotionClock>,
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
            && !matches!(
                interaction,
                DockInteraction::QuestionCustom { .. }
                    | DockInteraction::Approval(_)
                    | DockInteraction::ExactShellApproval(_)
            );
        let staged = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let working = screen_working_candidate(
            motion_clock.as_deref_mut(),
            dock.motion.requested().preference(),
            dock.working,
        );
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
            dock.motion,
            live,
            file_snapshot,
            working,
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
                commit_surface(dock.view, dock.theme, dock.motion, surface.commit);
                dock.file_suggestions.commit_presentation(staged);
                if let Some(clock) = motion_clock.as_deref_mut() {
                    commit_screen_working(clock, &mut dock.working, Some(working));
                } else {
                    dock.working = working;
                }
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
    working: WorkingPresentation,
) -> Result<DockFrame, InteractiveError> {
    DockFrame::layout(
        DockModel {
            interaction,
            composer: input.composer(),
            queue: input.queue(),
            notice,
            file_suggestions,
            working,
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
        && !matches!(
            interaction,
            DockInteraction::QuestionCustom { .. }
                | DockInteraction::Approval(_)
                | DockInteraction::ExactShellApproval(_)
        )
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
    motion: &MotionState,
    live: &LiveRenderer,
    file_suggestions: FileSuggestionSnapshot<'_>,
    working: WorkingPresentation,
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
        motion.requested(),
        live,
        file_suggestions,
        working,
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
    motion: MotionRequest,
    live: &LiveRenderer,
    file_suggestions: FileSuggestionSnapshot<'_>,
    working: WorkingPresentation,
) -> Result<EnhancedSurface, InteractiveError> {
    let working = normalize_working_presentation(working, motion.preference());
    match request.mode() {
        ViewMode::Focus => Ok(EnhancedSurface {
            frame: enhanced_dock_frame(
                input,
                notice,
                interaction,
                size,
                file_suggestions,
                working,
            )?,
            commit: SurfaceCommit {
                request,
                theme,
                motion,
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
                commit: surface_commit(request, theme, motion, viewport),
            })
        }
    }
}

fn normalize_working_presentation(
    working: WorkingPresentation,
    preference: MotionPreference,
) -> WorkingPresentation {
    let age = match (preference, working.age) {
        (MotionPreference::Reduced, WorkingAge::OneSecond { .. }) => {
            WorkingAge::OneSecond { seconds: 1 }
        }
        (MotionPreference::Reduced, WorkingAge::Long { .. }) => WorkingAge::Long { seconds: 5 },
        (_, age) => age,
    };
    let phase = match preference {
        MotionPreference::Reduced => WorkingPhase::Static,
        MotionPreference::Full if working.phase == WorkingPhase::Plain => WorkingPhase::Static,
        MotionPreference::Full => working.phase,
    };
    WorkingPresentation { phase, age }
}

fn surface_commit(
    request: ViewRequest,
    theme: ThemeRequest,
    motion: MotionRequest,
    viewport: DetailViewport,
) -> SurfaceCommit {
    SurfaceCommit {
        request,
        theme,
        motion,
        offset: viewport.offset,
        total_rows: viewport.total_rows,
        page_rows: viewport.page_rows,
    }
}

fn commit_surface(
    view: &mut ViewState,
    theme: &mut ThemeState,
    motion: &mut MotionState,
    commit: SurfaceCommit,
) {
    let _ = view.commit(
        commit.request,
        commit.offset,
        commit.total_rows,
        commit.page_rows,
    );
    let _ = theme.commit(commit.theme);
    let _ = motion.commit(commit.motion);
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
                dock.motion,
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
        mut user_questions,
        mut joins,
        session_id,
        resumed,
        file_suggestions: _file_suggestions,
        goal,
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
            if goal.is_armed().map_err(|_| InteractiveError::Agent)? {
                if let Some(round) = goal.next_round().map_err(|_| InteractiveError::Agent)? {
                    parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                    let mut prompt_committed = false;
                    let turn_result = run_turn(ActiveTurn {
                        agent: &mut agent,
                        events: &mut events,
                        approvals: &mut approvals,
                        user_questions: &mut user_questions,
                        joins: &mut joins,
                        live: &mut live,
                        presenter: &mut presenter,
                        terminal: &terminal,
                        panic_restore: None,
                        signals,
                        parser: &mut parser,
                        scratch: &mut scratch,
                        prompt: round.prompt().to_owned(),
                        prompt_committed: &mut prompt_committed,
                        queued_input: None,
                        queue_notice: None,
                        enhanced_decoder: None,
                        active_dock: None,
                        enhanced_presenter: None,
                        color,
                        enhanced: false,
                        goal: &goal,
                        goal_round: Some((round.revision(), round.number())),
                    })
                    .await;
                    match turn_result? {
                        TurnDisposition::Continue => continue,
                        TurnDisposition::Exit(code) => {
                            return Ok(InteractiveExit::Ordinary(code));
                        }
                        TurnDisposition::Signal(signal) => {
                            return Ok(InteractiveExit::Signal(signal));
                        }
                    }
                }
            }
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
                    IdleInput::Motion(command) => {
                        let message = if matches!(command, MotionCommand::Invalid) {
                            "[unknown motion mode; linear UI has no periodic animation]\n"
                        } else {
                            "[linear UI has no periodic animation]\n"
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
                    IdleInput::Goal(command) => {
                        let mut notice = None;
                        apply_goal_command(&mut agent, &goal, command, &mut notice).await;
                        let message = format!(
                            "[{}]\n",
                            notice.as_deref().unwrap_or("Goal state unavailable")
                        );
                        if let Some(signal) =
                            write_dynamic_notice(message, &mut presenter, &terminal, signals)
                                .await?
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
                            user_questions: &mut user_questions,
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
                            goal: &goal,
                            goal_round: None,
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
    user_questions: &'a mut UserQuestionReceiver,
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
    goal: &'a GoalRuntime,
    goal_round: Option<(u64, u32)>,
}

struct ActiveDock<'a> {
    screen: &'a mut InlineScreen,
    last_size: &'a mut TerminalSize,
    view: &'a mut ViewState,
    theme: &'a mut ThemeState,
    motion: &'a mut MotionState,
    command_palette: &'a mut CommandPaletteState,
    file_suggestions: &'a mut FileSuggestionController,
    palette_suppressed: bool,
    working: WorkingPresentation,
}

struct MotionClock {
    turn: TurnId,
    generation: u64,
    started_at: Instant,
    eligible_since: Option<Instant>,
    next_phase: Option<Instant>,
    phase: u8,
    animated: bool,
    preference: Option<MotionPreference>,
    baseline: Option<WorkingPresentation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MotionTick {
    turn: TurnId,
    generation: u64,
    preference: MotionPreference,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MotionEligibility {
    enhanced: bool,
    turn_open: bool,
    approval_inactive: bool,
    no_question: bool,
    focus: bool,
    no_notice: bool,
    queue_empty: bool,
    file_hidden: bool,
    palette_hidden: bool,
    motion_committed: bool,
}

impl MotionEligibility {
    fn is_eligible(self) -> bool {
        self.enhanced
            && self.turn_open
            && self.approval_inactive
            && self.no_question
            && self.focus
            && self.no_notice
            && self.queue_empty
            && self.file_hidden
            && self.palette_hidden
            && self.motion_committed
    }
}

impl MotionClock {
    fn new(turn: TurnId) -> Self {
        Self {
            turn,
            generation: 0,
            started_at: Instant::now(),
            eligible_since: None,
            next_phase: None,
            phase: 0,
            animated: false,
            preference: None,
            baseline: None,
        }
    }

    fn synchronize(&mut self, turn: TurnId, eligible: bool) -> Result<bool, InteractiveError> {
        if turn != self.turn {
            return Err(InteractiveError::Agent);
        }
        if eligible == self.eligible_since.is_some() {
            return Ok(false);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(InteractiveError::Output)?;
        self.animated = false;
        self.phase = 0;
        if eligible {
            let now = Instant::now();
            self.eligible_since = Some(now);
            self.next_phase = Some(now + MOTION_DELAY);
        } else {
            self.eligible_since = None;
            self.next_phase = None;
        }
        Ok(true)
    }

    fn presentation(
        &self,
        preference: MotionPreference,
        phase: WorkingPhase,
    ) -> WorkingPresentation {
        let elapsed = Instant::now().saturating_duration_since(self.started_at);
        let seconds = elapsed.as_secs();
        let age = if elapsed >= MOTION_LONG_WAIT {
            WorkingAge::Long {
                seconds: if preference == MotionPreference::Reduced {
                    5
                } else {
                    seconds
                },
            }
        } else if elapsed >= MOTION_ONE_SECOND {
            WorkingAge::OneSecond {
                seconds: if preference == MotionPreference::Reduced {
                    1
                } else {
                    seconds
                },
            }
        } else {
            WorkingAge::Fresh
        };
        let phase = match preference {
            MotionPreference::Reduced => WorkingPhase::Static,
            MotionPreference::Full => phase,
        };
        WorkingPresentation { phase, age }
    }

    fn deadline(
        &self,
        preference: MotionPreference,
        committed: WorkingPresentation,
    ) -> Option<MotionTick> {
        self.eligible_since?;
        let deadline = match preference {
            MotionPreference::Full => self.next_phase,
            MotionPreference::Reduced => {
                let now = Instant::now();
                let current = self.presentation(preference, WorkingPhase::Static);
                if current != committed {
                    return Some(MotionTick {
                        turn: self.turn,
                        generation: self.generation,
                        preference,
                        deadline: now,
                    });
                }
                for deadline in [
                    self.started_at + MOTION_ONE_SECOND,
                    self.started_at + MOTION_LONG_WAIT,
                ] {
                    if deadline > now {
                        return Some(MotionTick {
                            turn: self.turn,
                            generation: self.generation,
                            preference,
                            deadline,
                        });
                    }
                }
                None
            }
        }?;
        Some(MotionTick {
            turn: self.turn,
            generation: self.generation,
            preference,
            deadline,
        })
    }

    fn advance(
        &mut self,
        tick: MotionTick,
        requested: MotionRequest,
        committed: MotionRequest,
    ) -> Option<WorkingPresentation> {
        if tick.turn != self.turn
            || tick.generation != self.generation
            || self.eligible_since.is_none()
            || requested != committed
            || tick.preference != committed.preference()
        {
            return None;
        }
        let now = Instant::now();
        match tick.preference {
            MotionPreference::Reduced => {
                Some(self.presentation(MotionPreference::Reduced, WorkingPhase::Static))
            }
            MotionPreference::Full => {
                if self.next_phase != Some(tick.deadline) || tick.deadline > now {
                    return None;
                }
                if self.animated {
                    self.phase = (self.phase + 1) % 4;
                } else {
                    self.animated = true;
                    self.phase = 0;
                }
                self.next_phase = Some(now + MOTION_INTERVAL);
                Some(self.presentation(MotionPreference::Full, WorkingPhase::Animated(self.phase)))
            }
        }
    }

    fn pending_baseline(&self) -> Option<WorkingPresentation> {
        self.baseline
    }

    fn pending_baseline_for(
        &mut self,
        preference: MotionPreference,
    ) -> Option<WorkingPresentation> {
        if self.preference != Some(preference) {
            self.preference = Some(preference);
            self.baseline = Some(self.presentation(preference, WorkingPhase::Static));
        } else if self.baseline.is_none() && self.eligible_since.is_none() {
            // Hidden surfaces may commit and consume an earlier baseline while
            // the turn keeps running. A later direct reveal must derive age
            // from started_at, not replay that older hidden credential.
            self.baseline = Some(self.presentation(preference, WorkingPhase::Static));
        }
        let normalized = normalize_working_presentation(self.baseline?, preference);
        self.baseline = Some(normalized);
        Some(normalized)
    }

    fn commit_presentation(&mut self, working: WorkingPresentation) {
        if self.baseline == Some(working) {
            self.baseline = None;
        }
    }
}

fn synchronize_motion_clock(
    clock: &mut MotionClock,
    turn: TurnId,
    eligible: bool,
    preference: MotionPreference,
) -> Result<(), InteractiveError> {
    let preference_changed = clock.preference != Some(preference);
    clock.preference = Some(preference);
    if clock.synchronize(turn, eligible)? || preference_changed {
        // A new eligibility generation always starts from the stable phase.
        // Keeping an old completed animation phase here would replay it before
        // the fresh 300 ms delay. Elapsed time remains a turn fact, so it still
        // comes from the original turn-owned clock.
        clock.baseline = Some(clock.presentation(preference, WorkingPhase::Static));
    }
    Ok(())
}

fn settle_motion_clock(
    clock: &mut MotionClock,
    preference: MotionPreference,
) -> Result<(), InteractiveError> {
    let turn = clock.turn;
    let _ = clock.synchronize(turn, false)?;
    clock.preference = Some(preference);
    clock.baseline = Some(clock.presentation(preference, WorkingPhase::Static));
    Ok(())
}

fn commit_screen_working(
    clock: &mut MotionClock,
    committed: &mut WorkingPresentation,
    presented: Option<WorkingPresentation>,
) {
    if let Some(working) = presented {
        *committed = working;
        clock.commit_presentation(working);
    }
}

fn screen_working_candidate(
    motion_clock: Option<&mut MotionClock>,
    preference: MotionPreference,
    committed: WorkingPresentation,
) -> WorkingPresentation {
    normalize_working_presentation(
        motion_clock
            .and_then(|clock| clock.pending_baseline_for(preference))
            .unwrap_or(committed),
        preference,
    )
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
    RememberExactShell,
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
        allow_exact_shell: bool,
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
        let selector = ApprovalSelector::new_for_request(profile, allow_exact_shell)
            .map_err(|_| InteractiveError::Agent)?;
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
            SelectorUpdate::RememberExactShell => {
                if let Some(mode) = mode {
                    mode.restore()?;
                }
                Ok(ApprovalUiUpdate::RememberExactShell)
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
            SelectorUpdate::Redraw
            | SelectorUpdate::RememberExactShell
            | SelectorUpdate::Eof
            | SelectorUpdate::Invalid => Err(InteractiveError::Agent),
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

    fn dock_interaction(&self) -> DockInteraction {
        match self {
            Self::Rendering { selector, .. } | Self::Accepting { selector, .. } => {
                let selected = match selector.selected() {
                    super::approval_selector::ApprovalSelection::AllowOnce => {
                        DockApprovalSelection::AllowOnce
                    }
                    super::approval_selector::ApprovalSelection::Reject => {
                        DockApprovalSelection::Reject
                    }
                    super::approval_selector::ApprovalSelection::AllowExactShellForProcess => {
                        DockApprovalSelection::AllowExactShellForProcess
                    }
                    super::approval_selector::ApprovalSelection::Cancel => {
                        DockApprovalSelection::Cancel
                    }
                };
                if selector.allows_exact_shell() {
                    DockInteraction::ExactShellApproval(selected)
                } else {
                    DockInteraction::Approval(selected)
                }
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

#[allow(clippy::too_many_arguments)]
async fn redraw_active_after_resize(
    enhanced: bool,
    live: &LiveRenderer,
    input: Option<&InputMemory>,
    notice: Option<&str>,
    interaction: DockInteraction,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    dock: Option<&mut ActiveDock<'_>>,
    motion_clock: &mut MotionClock,
) -> Result<Option<UiSignal>, InteractiveError> {
    if !enhanced {
        return Ok(None);
    }
    render_active_dock(
        input.ok_or(InteractiveError::Agent)?,
        notice,
        interaction,
        live,
        terminal,
        signals,
        dock.ok_or(InteractiveError::Agent)?,
        Some(motion_clock),
    )
    .await
}

fn question_dock_interaction(
    question: &UserQuestionUiState,
    approval: &ApprovalUiState<'_>,
) -> DockInteraction {
    if question.is_custom() {
        DockInteraction::QuestionCustom {
            retry: question.custom_retry(),
        }
    } else {
        approval.dock_interaction()
    }
}

fn restore_question_overlay(input: Option<&mut InputMemory>) -> Result<(), InteractiveError> {
    let Some(input) = input else {
        return Ok(());
    };
    if input.question_overlay_active() {
        let _ = input.finish_question_overlay()?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionCustomDispatch {
    Continue,
    Redraw,
    Eof,
}

fn dispatch_enhanced_question_custom(
    outcome: QuestionCustomInputOutcome,
    question: &mut UserQuestionUiState,
    input: &mut InputMemory,
) -> Result<QuestionCustomDispatch, InteractiveError> {
    match outcome {
        QuestionCustomInputOutcome::Continue => Ok(QuestionCustomDispatch::Continue),
        QuestionCustomInputOutcome::Redraw => Ok(QuestionCustomDispatch::Redraw),
        QuestionCustomInputOutcome::Invalid => {
            question.retry();
            Ok(QuestionCustomDispatch::Redraw)
        }
        QuestionCustomInputOutcome::Submit => {
            let custom = input.finish_question_overlay()?;
            if !question.submit_custom(custom) {
                input.begin_question_overlay()?;
            }
            Ok(QuestionCustomDispatch::Redraw)
        }
        QuestionCustomInputOutcome::Cancel => {
            let _ = input.finish_question_overlay()?;
            question.cancel();
            Ok(QuestionCustomDispatch::Redraw)
        }
        QuestionCustomInputOutcome::Eof => {
            let _ = input.finish_question_overlay()?;
            question.cancel();
            Ok(QuestionCustomDispatch::Eof)
        }
    }
}

fn prepare_pending_for_resize(
    pending: &mut Option<PendingOutput>,
    screen: &mut InlineScreen,
) -> Result<bool, InteractiveError> {
    if pending.as_ref().is_some_and(PendingOutput::has_started)
        && !matches!(
            pending.as_ref(),
            Some(PendingOutput::Inline(PendingInlineOutput {
                intent: InlineIntent::Dock(_) | InlineIntent::MotionDock { .. },
                ..
            }))
        )
    {
        return Err(InteractiveError::Output);
    }
    let Some(output) = pending.take() else {
        return Ok(false);
    };
    let motion_only = output.is_motion_only();
    let mut recover_visual_state = false;
    let restored = match output {
        PendingOutput::Inline(output) => {
            recover_visual_state = output.write.has_started();
            screen.abort(output.write);
            if screen.is_poisoned() != recover_visual_state {
                return Err(InteractiveError::Output);
            }
            output.intent.into_pending()
        }
        output => output,
    };
    if !motion_only {
        *pending = Some(restored);
    }
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
    motion_clock: &mut MotionClock,
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
        Some(motion_clock),
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
    let prepared = match active.goal_round {
        Some((revision, round)) => {
            let goal_id = active
                .goal
                .snapshot()
                .map_err(|_| InteractiveError::Agent)?
                .ok_or(InteractiveError::Agent)?
                .to_value()["goalId"]
                .as_str()
                .ok_or(InteractiveError::Agent)?
                .to_owned();
            prepare_goal_turn(
                active.agent.session(),
                &active.prompt,
                &goal_id,
                revision,
                round,
            )
        }
        None => prepare_user_turn(active.agent.session(), &active.prompt),
    }
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
    let mut question_ui = UserQuestionUiState::default();
    let mut turn_end_seen = false;
    let mut turn_end_rendered = false;
    let mut stop = None;
    let mut prefer_input = true;
    let mut dock_redraw_requested = false;
    let mut motion_reattach_required = false;
    let mut motion_reattach_wait_for_fact = false;
    let mut input_escape_deadline = None;
    let mut motion_clock = MotionClock::new(turn);

    let result = {
        let future = active
            .agent
            .run_turn(prepared.proposal, cancellation.clone());
        tokio::pin!(future);
        let ui_result = std::panic::AssertUnwindSafe(async {
            loop {
            let mut motion_eligible = false;
            if let Some(dock) = active.active_dock.as_mut() {
                dock.palette_suppressed =
                    active.joins.question().is_some()
                        || !approval_ui.is_inactive()
                        || question_ui.is_active();
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
                if !question_ui.is_custom() {
                    reset_file_suggestion_decoder(
                        dock.file_suggestions,
                        active.enhanced_decoder.as_deref_mut(),
                        &mut input_escape_deadline,
                    )?;
                }
                let file_snapshot = dock.file_suggestions.snapshot();
                let palette_snapshot = active_command_palette_snapshot(input, dock);
                let eligible = MotionEligibility {
                    enhanced: active.enhanced,
                    turn_open: !turn_end_seen,
                    approval_inactive: approval_ui.is_inactive(),
                    no_question: active.joins.question().is_none() && !question_ui.is_active(),
                    focus: dock.view.requested().mode() == ViewMode::Focus,
                    no_notice: active.queue_notice.as_deref().is_none_or(|notice| notice.is_none()),
                    queue_empty: input.queue().len() == 0,
                    file_hidden: !file_snapshot.is_visible(),
                    palette_hidden: !palette_snapshot.is_visible(),
                    motion_committed: !dock.motion.is_transitioning()
                        && dock.motion.requested() == dock.motion.committed(),
                }
                .is_eligible();
                motion_eligible = eligible;
                synchronize_motion_clock(
                    &mut motion_clock,
                    turn,
                    eligible,
                    dock.motion.committed().preference(),
                )?;
            }
            enqueue_standalone_motion_baseline(
                motion_eligible,
                turn_end_seen,
                &motion_clock,
                &mut pending,
                &mut frame_deadline,
                &mut after_frame,
            )?;
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
                question_dock_interaction(&question_ui, &approval_ui),
                active.live,
                active.active_dock.as_mut(),
                &mut motion_clock,
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

            if motion_reattach_required {
                motion_reattach_required = false;
                motion_reattach_wait_for_fact = false;
                let reattached = reattach_motion_screen(
                    active.terminal,
                    active.signals,
                    active
                        .queued_input
                        .as_deref()
                        .ok_or(InteractiveError::Agent)?,
                    active.queue_notice.as_deref().and_then(Option::as_deref),
                    question_dock_interaction(&question_ui, &approval_ui),
                    active.live,
                    &mut motion_clock,
                    active.active_dock.as_mut().ok_or(InteractiveError::Agent)?,
                )
                .await;
                match reattached {
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
            }

            if let Err(error) = complete_ready_frame(
                &mut pending,
                &mut frame_deadline,
                &mut after_frame,
                &mut approval_ui,
                &mut turn_end_rendered,
                &mut motion_clock,
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
            if pending.is_none() && question_ui.rendering_finished() {
                let mode = question_ui
                    .begin_accepting()
                    .map_err(|_| InteractiveError::Agent)?;
                if mode == QuestionAcceptingMode::Custom && active.enhanced {
                    active
                        .queued_input
                        .as_deref_mut()
                        .ok_or(InteractiveError::Agent)?
                        .begin_question_overlay()?;
                    active
                        .enhanced_decoder
                        .as_deref_mut()
                        .ok_or(InteractiveError::Agent)?
                        .reset_epoch()
                        .map_err(|_| InteractiveError::Agent)?;
                    input_escape_deadline = None;
                    dock_redraw_requested = true;
                }
            }
            if pending.is_none() {
                if let Some((request, retry, position, total)) = question_ui.frame_request() {
                    if active.enhanced {
                        active.terminal.revalidate_identity()?;
                        active
                            .enhanced_decoder
                            .as_deref_mut()
                            .ok_or(InteractiveError::Agent)?
                            .reset_epoch()
                            .map_err(|_| InteractiveError::Agent)?;
                        input_escape_deadline = None;
                    } else {
                        active.terminal.revalidate()?;
                    }
                    active.terminal.flush_input()?;
                    active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                    let frame = LiveFrame::user_question(
                        request,
                        retry,
                        position,
                        total,
                        active.enhanced,
                    )
                        .map_err(|_| InteractiveError::Output)?;
                    enqueue_frame(
                        frame,
                        AfterFrame::None,
                        &mut pending,
                        &mut frame_deadline,
                        &mut after_frame,
                    )?;
                    question_ui
                        .mark_rendering()
                        .map_err(|_| InteractiveError::Agent)?;
                }
            }
            if pending.is_none() && mem::take(&mut dock_redraw_requested) {
                match redraw_active_after_resize(
                    active.enhanced,
                    active.live,
                    active.queued_input.as_deref(),
                    active.queue_notice.as_deref().and_then(Option::as_deref),
                    question_dock_interaction(&question_ui, &approval_ui),
                    active.terminal,
                    active.signals,
                    active.active_dock.as_mut(),
                    &mut motion_clock,
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
                        Some(&mut motion_clock),
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
                active.user_questions,
                active.events,
                active.scratch,
                pending.as_ref(),
                frame_deadline,
                approval_ui.arm_deadline(),
                approval_ui.escape_deadline(),
                input_escape_deadline,
                active.active_dock.as_ref().and_then(|dock| {
                    motion_clock.deadline(dock.motion.committed().preference(), dock.working)
                }),
                !(pending.is_some() && approval_ui.suppresses_read_while_pending())
                    && (!question_ui.is_active() || question_ui.is_accepting()),
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
                                question_dock_interaction(&question_ui, &approval_ui),
                                active.live,
                                active.active_dock.as_mut(),
                                &mut motion_clock,
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
                result = &mut future => {
                    if preempt_motion_pending(
                        &mut pending,
                        &mut frame_deadline,
                        &mut after_frame,
                        active.active_dock.as_mut(),
                    ) {
                        let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                        let recovered =
                            recover_poisoned_screen(active.terminal, active.signals, dock.screen)
                                .await;
                        match recovered {
                            Ok(Some(signal)) => {
                                observe_signal(&mut stop, signal);
                                cancellation.cancel();
                            }
                            Ok(None) => {
                                motion_reattach_required = true;
                                motion_reattach_wait_for_fact = true;
                            }
                            Err(error) => {
                                observe_failure(&mut stop, error);
                                cancellation.cancel();
                            }
                        }
                    }
                    break Ok(result);
                },
                settlement = wait_active_file_suggestion(active.active_dock.as_mut()), if suggestion_running => {
                    if preempt_motion_pending(
                        &mut pending,
                        &mut frame_deadline,
                        &mut after_frame,
                        active.active_dock.as_mut(),
                    ) {
                        let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                        match recover_poisoned_screen(active.terminal, active.signals, dock.screen)
                            .await
                        {
                            Ok(Some(signal)) => {
                                observe_signal(&mut stop, signal);
                                cancellation.cancel();
                                continue;
                            }
                            Ok(None) => motion_reattach_required = true,
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
                    let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                    let _ = dock
                        .file_suggestions
                        .accept_job(settlement)
                        .map_err(|_| InteractiveError::Agent)?;
                    dock_redraw_requested = true;
                }
                work = work => {
                    prefer_input = !prefer_input;
                    let preempts_motion = matches!(
                        &work,
                        UiWork::ApprovalArmed
                            | UiWork::EscapeExpired
                            | UiWork::InputEscapeExpired
                            | UiWork::Envelope(_)
                            | UiWork::Question(_)
                            | UiWork::Event(_)
                            | UiWork::Read(_)
                    );
                    let motion_write_started = preempts_motion
                        && preempt_motion_pending(
                            &mut pending,
                            &mut frame_deadline,
                            &mut after_frame,
                            active.active_dock.as_mut(),
                        );
                    if motion_write_started {
                        let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                        match recover_poisoned_screen(
                            active.terminal,
                            active.signals,
                            dock.screen,
                        )
                        .await {
                            Ok(Some(signal)) => {
                                observe_signal(&mut stop, signal);
                                cancellation.cancel();
                                continue;
                            }
                            Ok(None) => motion_reattach_required = true,
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
                        if fences_ordinary_input_after_motion_preemption(&work) {
                            if let Some(decoder) = active.enhanced_decoder.as_deref_mut() {
                                decoder.reset_epoch().map_err(|_| InteractiveError::Agent)?;
                            }
                            input_escape_deadline = None;
                            dock_redraw_requested = true;
                            continue;
                        }
                    }
                    match work {
                        UiWork::FrameExpired => latch_active_failure(
                            &mut stop,
                            &cancellation,
                            &mut pending,
                            active.presenter,
                            InteractiveError::Output,
                        ),
                        UiWork::MotionTick(tick) => {
                            let dock = active.active_dock.as_mut().ok_or(InteractiveError::Agent)?;
                            if let Some(working) = motion_clock.advance(
                                tick,
                                dock.motion.requested(),
                                dock.motion.committed(),
                            ) {
                                enqueue_motion_dock(
                                    working,
                                    &mut pending,
                                    &mut frame_deadline,
                                    &mut after_frame,
                                )?;
                            }
                        }
                        UiWork::ApprovalArmed => {
                            let Some(question) = active.joins.question() else {
                                approval_ui.observe_unaccepted_input();
                                continue;
                            };
                            let allow_exact_shell = question.exact_shell_scope_available();
                            let prepared = approval_ui
                                .begin_rendering(
                                    active.terminal,
                                    active.color,
                                    active.enhanced,
                                    allow_exact_shell,
                                )
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
                            if question_ui.is_custom() && active.enhanced {
                                let width = usize::from(
                                    active
                                        .terminal
                                        .size()
                                        .unwrap_or(TerminalSize {
                                            rows: MIN_ENHANCED_ROWS,
                                            columns: MIN_ENHANCED_COLUMNS,
                                        })
                                        .columns
                                        .saturating_sub(3),
                                )
                                .max(1);
                                let outcome = handle_question_custom_escape_expiry(
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    width,
                                )?;
                                let dispatched = dispatch_enhanced_question_custom(
                                    outcome,
                                    &mut question_ui,
                                    active
                                        .queued_input
                                        .as_deref_mut()
                                        .ok_or(InteractiveError::Agent)?,
                                )?;
                                match dispatched {
                                    QuestionCustomDispatch::Continue => {}
                                    QuestionCustomDispatch::Redraw => {
                                        active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                                        dock_redraw_requested = true;
                                    }
                                    QuestionCustomDispatch::Eof => {
                                        stop = Some(StopIntent::Eof);
                                        cancellation.cancel();
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                }
                            } else if approval_owns_active_input(active.joins, &approval_ui) {
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
                        UiWork::Question(envelope) => {
                            let received = envelope
                                .ok_or(InteractiveError::Agent)
                                .and_then(|envelope| {
                                    question_ui
                                        .receive(envelope)
                                        .map_err(|_| InteractiveError::Agent)
                                });
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
                            } else if question_ui.is_custom() && active.enhanced {
                                let outcome = handle_question_custom_input(
                                    active.terminal,
                                    active.enhanced_decoder.as_deref_mut(),
                                    active.queued_input.as_deref_mut(),
                                    &active.scratch[..count],
                                )?;
                                if let Some(decoder) = active.enhanced_decoder.as_deref() {
                                    refresh_decoder_escape_deadline(
                                        decoder,
                                        &mut input_escape_deadline,
                                    );
                                }
                                let dispatched = dispatch_enhanced_question_custom(
                                    outcome,
                                    &mut question_ui,
                                    active
                                        .queued_input
                                        .as_deref_mut()
                                        .ok_or(InteractiveError::Agent)?,
                                )?;
                                match dispatched {
                                    QuestionCustomDispatch::Continue => {}
                                    QuestionCustomDispatch::Redraw => {
                                        active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                                        dock_redraw_requested = true;
                                    }
                                    QuestionCustomDispatch::Eof => {
                                        stop = Some(StopIntent::Eof);
                                        cancellation.cancel();
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                }
                            } else if question_ui.is_accepting() {
                                match question_ui.feed(&active.scratch[..count], active.enhanced) {
                                    QuestionInputUpdate::None => {}
                                    QuestionInputUpdate::Selected(index) => {
                                        question_ui.select(index);
                                        active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                                        dock_redraw_requested = active.enhanced;
                                    }
                                    QuestionInputUpdate::CustomRequested => {
                                        question_ui
                                            .begin_custom()
                                            .map_err(|_| InteractiveError::Agent)?;
                                        if active.enhanced {
                                            active
                                                .queued_input
                                                .as_deref_mut()
                                                .ok_or(InteractiveError::Agent)?
                                                .begin_question_overlay()?;
                                            active
                                                .enhanced_decoder
                                                .as_deref_mut()
                                                .ok_or(InteractiveError::Agent)?
                                                .reset_epoch()
                                                .map_err(|_| InteractiveError::Agent)?;
                                            input_escape_deadline = None;
                                            dock_redraw_requested = true;
                                        } else {
                                            enqueue_frame(
                                                LiveFrame::user_question_custom_prompt(false)
                                                    .map_err(|_| InteractiveError::Output)?,
                                                AfterFrame::None,
                                                &mut pending,
                                                &mut frame_deadline,
                                                &mut after_frame,
                                            )?;
                                        }
                                    }
                                    QuestionInputUpdate::CustomSubmitted(custom) => {
                                        if !question_ui.submit_custom(custom) {
                                            enqueue_frame(
                                                LiveFrame::user_question_custom_prompt(true)
                                                    .map_err(|_| InteractiveError::Output)?,
                                                AfterFrame::None,
                                                &mut pending,
                                                &mut frame_deadline,
                                                &mut after_frame,
                                            )?;
                                        }
                                        active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                                    }
                                    QuestionInputUpdate::Cancelled => {
                                        question_ui.cancel();
                                        active.parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                                        dock_redraw_requested = active.enhanced;
                                    }
                                    QuestionInputUpdate::Invalid => {
                                        let custom = question_ui.is_custom();
                                        question_ui.retry();
                                        if custom && !active.enhanced {
                                            enqueue_frame(
                                                LiveFrame::user_question_custom_prompt(true)
                                                    .map_err(|_| InteractiveError::Output)?,
                                                AfterFrame::None,
                                                &mut pending,
                                                &mut frame_deadline,
                                                &mut after_frame,
                                            )?;
                                        }
                                    }
                                    QuestionInputUpdate::Eof => {
                                        question_ui.cancel();
                                        stop = Some(StopIntent::Eof);
                                        cancellation.cancel();
                                        discard_pending(&mut pending, active.presenter);
                                    }
                                }
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
                                    active.goal,
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
                                                Some(&mut motion_clock),
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
                let question_overlay = restore_question_overlay(active.queued_input.as_deref_mut());
                if question_ui.is_active() {
                    question_ui.cancel();
                }
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
                question_overlay?;
                return Err(error);
            }
        }
    };

    if let Err(error) = restore_question_overlay(active.queued_input.as_deref_mut()) {
        observe_failure(&mut stop, error);
    }
    if question_ui.is_active() {
        question_ui.cancel();
    }

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
                question_dock_interaction(&question_ui, &approval_ui),
                active.live,
                active.active_dock.as_mut(),
                &mut motion_clock,
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
            if motion_reattach_required && (!motion_reattach_wait_for_fact || pending.is_some()) {
                motion_reattach_required = false;
                motion_reattach_wait_for_fact = false;
                let reattached = reattach_motion_screen(
                    active.terminal,
                    active.signals,
                    active
                        .queued_input
                        .as_deref()
                        .ok_or(InteractiveError::Agent)?,
                    active.queue_notice.as_deref().and_then(Option::as_deref),
                    question_dock_interaction(&question_ui, &approval_ui),
                    active.live,
                    &mut motion_clock,
                    active.active_dock.as_mut().ok_or(InteractiveError::Agent)?,
                )
                .await;
                match reattached {
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
            }
            if let Err(error) = complete_ready_frame(
                &mut pending,
                &mut frame_deadline,
                &mut after_frame,
                &mut approval_ui,
                &mut turn_end_rendered,
                &mut motion_clock,
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
                    question_dock_interaction(&question_ui, &approval_ui),
                    active.terminal,
                    active.signals,
                    active.active_dock.as_mut(),
                    &mut motion_clock,
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
                active.user_questions,
                active.events,
                active.scratch,
                pending.as_ref(),
                frame_deadline,
                approval_ui.arm_deadline(),
                approval_ui.escape_deadline(),
                input_escape_deadline,
                None,
                !(motion_reattach_wait_for_fact
                    || question_ui.is_active()
                    || pending.is_some() && approval_ui.suppresses_read_while_pending()),
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
                                question_dock_interaction(&question_ui, &approval_ui),
                                active.live,
                                active.active_dock.as_mut(),
                                &mut motion_clock,
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
                            let Some(question) = active.joins.question() else {
                                approval_ui.observe_unaccepted_input();
                                continue;
                            };
                            let allow_exact_shell = question.exact_shell_scope_available();
                            let prepared = approval_ui
                                .begin_rendering(
                                    active.terminal,
                                    active.color,
                                    active.enhanced,
                                    allow_exact_shell,
                                )
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
                        UiWork::Question(_) => {
                            observe_failure(&mut stop, InteractiveError::Agent);
                            discard_pending(&mut pending, active.presenter);
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
                                    active.goal,
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
                                                Some(&mut motion_clock),
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
                        UiWork::MotionTick(_) => {
                            observe_failure(&mut stop, InteractiveError::Agent);
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
                    Some(&mut motion_clock),
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

    if let Some((revision, round)) = active.goal_round {
        let completed = matches!(
            &result,
            Ok(outcome) if matches!(outcome.reason(), TurnEndReason::Completed)
        );
        if stop.is_some() || !completed {
            pause_goal_after_round_failure(
                active.agent,
                active.goal,
                revision,
                round,
                *active.prompt_committed,
            )
            .await?;
        }
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
        &mut motion_clock,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuestionCustomInputOutcome {
    Continue,
    Redraw,
    Submit,
    Cancel,
    Eof,
    Invalid,
}

fn handle_question_custom_input(
    terminal: &AsyncTerminal,
    decoder: Option<&mut KeyDecoder>,
    input: Option<&mut InputMemory>,
    bytes: &[u8],
) -> Result<QuestionCustomInputOutcome, InteractiveError> {
    let (Some(decoder), Some(input)) = (decoder, input) else {
        return Err(InteractiveError::Agent);
    };
    let size = terminal.size().unwrap_or(TerminalSize {
        rows: MIN_ENHANCED_ROWS,
        columns: MIN_ENHANCED_COLUMNS,
    });
    let width = usize::from(size.columns.saturating_sub(3)).max(1);
    let mut outcome = QuestionCustomInputOutcome::Continue;
    let _ = decoder.feed(bytes, |decoded| {
        let next = apply_question_custom_event(decoded.event, input, width);
        match next {
            Ok(QuestionCustomInputOutcome::Continue) => ControlFlow::Continue(()),
            Ok(QuestionCustomInputOutcome::Redraw) => {
                outcome = QuestionCustomInputOutcome::Redraw;
                ControlFlow::Continue(())
            }
            Ok(next) => {
                outcome = next;
                ControlFlow::Break(())
            }
            Err(_) => {
                outcome = QuestionCustomInputOutcome::Invalid;
                ControlFlow::Break(())
            }
        }
    });
    Ok(outcome)
}

fn handle_question_custom_escape_expiry(
    decoder: Option<&mut KeyDecoder>,
    input: Option<&mut InputMemory>,
    width: usize,
) -> Result<QuestionCustomInputOutcome, InteractiveError> {
    let (Some(decoder), Some(input)) = (decoder, input) else {
        return Err(InteractiveError::Agent);
    };
    let Some(decoded) = decoder.expire_escape() else {
        return Ok(QuestionCustomInputOutcome::Continue);
    };
    apply_question_custom_event(decoded.event, input, width).map_err(|_| InteractiveError::Agent)
}

fn apply_question_custom_event(
    event: InputEvent,
    input: &mut InputMemory,
    width: usize,
) -> Result<QuestionCustomInputOutcome, InputMemoryError> {
    let cursor_only = matches!(
        &event,
        InputEvent::Key(
            Key::BackTab | Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End
        )
    );
    let mut changed = false;
    match event {
        InputEvent::PasteStarted => return Ok(QuestionCustomInputOutcome::Continue),
        InputEvent::Paste(text) => {
            if !question_custom_fits(input, text.len()) {
                return Ok(QuestionCustomInputOutcome::Invalid);
            }
            input.insert_paste(&text)?;
            return Ok(QuestionCustomInputOutcome::Redraw);
        }
        InputEvent::PasteRejected(_) | InputEvent::Rejected(_) => {
            return Ok(QuestionCustomInputOutcome::Invalid);
        }
        InputEvent::Key(Key::Enter) => return Ok(QuestionCustomInputOutcome::Submit),
        InputEvent::Key(Key::Escape) => return Ok(QuestionCustomInputOutcome::Cancel),
        InputEvent::Key(Key::Eof) => return Ok(QuestionCustomInputOutcome::Eof),
        InputEvent::Key(Key::Newline) => {
            if !question_custom_fits(input, 1) {
                return Ok(QuestionCustomInputOutcome::Invalid);
            }
            input.insert_newline()?;
        }
        InputEvent::Key(Key::Char(character)) => {
            if !question_custom_fits(input, character.len_utf8()) {
                return Ok(QuestionCustomInputOutcome::Invalid);
            }
            input.insert_char(character)?;
        }
        InputEvent::Key(Key::Tab) => {
            if !question_custom_fits(input, 1) {
                return Ok(QuestionCustomInputOutcome::Invalid);
            }
            input.insert_text("\t")?;
        }
        InputEvent::Key(Key::BackTab | Key::Left) => changed = input.move_left(),
        InputEvent::Key(Key::Right) => changed = input.move_right(),
        InputEvent::Key(Key::Up) => changed = input.move_question_up(width)?,
        InputEvent::Key(Key::Down) => changed = input.move_question_down(width)?,
        InputEvent::Key(Key::Home) => changed = input.move_line_start(),
        InputEvent::Key(Key::End) => changed = input.move_line_end(),
        InputEvent::Key(Key::Backspace) => changed = input.backspace()?,
        InputEvent::Key(Key::Delete) => changed = input.delete()?,
        InputEvent::Key(Key::WordErase) => changed = input.erase_word()?,
        InputEvent::Key(Key::ClearBefore) => changed = input.clear_before_cursor()?,
        InputEvent::Key(Key::ClearAfter) => changed = input.clear_after_cursor()?,
        InputEvent::Key(Key::Yank) => {
            changed = input.yank()?;
            if input.composer().byte_len() > MAX_CUSTOM_ANSWER_BYTES {
                let _ = input.undo()?;
                return Ok(QuestionCustomInputOutcome::Invalid);
            }
        }
        InputEvent::Key(Key::Undo) => changed = input.undo()?,
        InputEvent::Key(Key::ReverseSearch | Key::Inspect | Key::PageUp | Key::PageDown) => {
            return Ok(QuestionCustomInputOutcome::Continue);
        }
    }
    Ok(if changed || !cursor_only {
        QuestionCustomInputOutcome::Redraw
    } else {
        QuestionCustomInputOutcome::Continue
    })
}

fn question_custom_fits(input: &InputMemory, added_bytes: usize) -> bool {
    input
        .composer()
        .byte_len()
        .checked_add(added_bytes)
        .is_some_and(|total| total <= MAX_CUSTOM_ANSWER_BYTES)
}

fn handle_active_input(
    terminal: &AsyncTerminal,
    decoder: Option<&mut KeyDecoder>,
    input: Option<&mut InputMemory>,
    dock: Option<&mut ActiveDock<'_>>,
    notice: Option<&mut Option<String>>,
    goal: &GoalRuntime,
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
        dock.motion,
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
                                "/goal [objective|edit|pause|resume|clear] | /inspect | /review | /focus | /theme | /motion | /help | /exit | /quit | Enter queue | Ctrl+J newline"
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
                        CommandId::Motion => {
                            let _ = input.take_draft_for_turn()?;
                            apply_motion_command(MotionCommand::Show, dock.motion, notice)?;
                        }
                        CommandId::Goal => {
                            let _ = input.take_draft_for_turn()?;
                            apply_active_goal_command(goal, Ok(GoalCommand::Show), notice);
                        }
                    }
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Theme(command) => {
                    let _ = input.take_draft_for_turn()?;
                    apply_theme_command(command, dock.theme, notice)?;
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Motion(command) => {
                    let _ = input.take_draft_for_turn()?;
                    apply_motion_command(command, dock.motion, notice)?;
                    return Ok(ActiveInputOutcome::Redraw);
                }
                EnhancedSubmission::Goal(command) => {
                    let _ = input.take_draft_for_turn()?;
                    apply_active_goal_command(goal, command, notice);
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
        dock.motion,
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
        return if matches!(
            &event.kind,
            CommittedUiKind::TypeOnly {
                event_type: "goal/change"
            }
        ) {
            Ok(())
        } else {
            Err(InteractiveError::Agent)
        };
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
        CommittedUiKind::UserMessage {
            source: UiUserSource::Other { kind },
            content,
        } if kind == "goal" => {
            if content.as_str() != Some(targets.expected_prompt) {
                return Err(InteractiveError::Agent);
            }
            *targets.prompt_committed = true;
            None
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
        ApprovalUiUpdate::RememberExactShell => {
            joins.answer_exact_shell_for_process()?;
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

fn enqueue_motion_dock(
    working: WorkingPresentation,
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    pending_after: &mut AfterFrame,
) -> Result<(), InteractiveError> {
    if pending.is_some() {
        return Err(InteractiveError::Agent);
    }
    *pending = Some(PendingOutput::MotionDock {
        interaction: DockInteraction::Running,
        working,
    });
    *deadline = Some(Instant::now() + FRAME_DEADLINE);
    *pending_after = AfterFrame::None;
    Ok(())
}

fn enqueue_standalone_motion_baseline(
    eligible: bool,
    turn_end_seen: bool,
    clock: &MotionClock,
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    pending_after: &mut AfterFrame,
) -> Result<(), InteractiveError> {
    if eligible && !turn_end_seen && pending.is_none() {
        if let Some(working) = clock.pending_baseline() {
            enqueue_motion_dock(working, pending, deadline, pending_after)?;
        }
    }
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
            approval_ui.dock_interaction(),
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
        ApprovalUiUpdate::RememberExactShell => {
            joins.answer_exact_shell_for_process()?;
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
    motion_clock: &mut MotionClock,
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
            let working = screen_working_candidate(
                Some(motion_clock),
                dock.motion.requested().preference(),
                dock.working,
            );
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
                dock.motion.requested(),
                live,
                file_snapshot,
                working,
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
                working: Some(working),
                file_suggestions: staged_file_suggestions,
            }))
        })();
        match staged {
            Ok(staged) => *pending = Some(staged),
            Err(error) => return Err(error),
        }
    }
    if matches!(
        pending,
        Some(PendingOutput::Dock(_) | PendingOutput::MotionDock { .. })
    ) {
        let (interaction, motion_working) = match pending.take() {
            Some(PendingOutput::Dock(interaction)) => (interaction, None),
            Some(PendingOutput::MotionDock {
                interaction,
                working,
            }) => (interaction, Some(working)),
            _ => return Err(InteractiveError::Agent),
        };
        let input = input.ok_or(InteractiveError::Agent)?;
        let dock = active_dock.as_deref_mut().ok_or(InteractiveError::Agent)?;
        let size = terminal.size().unwrap_or(*dock.last_size);
        if size != *dock.last_size {
            return Err(InteractiveError::TerminalUnsupported);
        }
        let show_file_suggestions = dock.view.requested().mode() == ViewMode::Focus
            && !matches!(
                interaction,
                DockInteraction::QuestionCustom { .. }
                    | DockInteraction::Approval(_)
                    | DockInteraction::ExactShellApproval(_)
            );
        let staged_file_suggestions = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let working = screen_working_candidate(
            Some(motion_clock),
            dock.motion.requested().preference(),
            motion_working.unwrap_or(dock.working),
        );
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
            dock.motion,
            live,
            file_snapshot,
            working,
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
            intent: if let Some(working) = motion_working {
                InlineIntent::MotionDock {
                    interaction,
                    working,
                }
            } else {
                InlineIntent::Dock(interaction)
            },
            surface: surface.commit,
            working: Some(working),
            file_suggestions: staged_file_suggestions,
        }));
    }
    let Some(frame) = pending.as_mut() else {
        return Ok(());
    };
    match frame {
        PendingOutput::Unprepared(_)
        | PendingOutput::Prepared(_)
        | PendingOutput::Dock(_)
        | PendingOutput::MotionDock { .. } => {
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
            commit_surface(dock.view, dock.theme, dock.motion, output.surface);
            dock.file_suggestions
                .commit_presentation(output.file_suggestions);
            commit_screen_working(motion_clock, &mut dock.working, output.working);
            match output.intent {
                InlineIntent::Transcript(presentation) => {
                    transcript_presenter
                        .take()
                        .expect("transcript presenter was proven before screen commit")
                        .commit(presentation);
                }
                InlineIntent::MotionDock { .. } => {}
                InlineIntent::Dock(_) => {}
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

fn preempt_motion_pending(
    pending: &mut Option<PendingOutput>,
    deadline: &mut Option<Instant>,
    after: &mut AfterFrame,
    dock: Option<&mut ActiveDock<'_>>,
) -> bool {
    if !pending.as_ref().is_some_and(PendingOutput::is_motion_only) {
        return false;
    }
    let started = pending.as_ref().is_some_and(PendingOutput::has_started);
    let _ = pending.take();
    *deadline = None;
    *after = AfterFrame::None;
    if started {
        if let Some(dock) = dock {
            dock.file_suggestions.invalidate_presentation();
        }
    }
    started
}

#[allow(clippy::too_many_arguments)]
async fn reattach_motion_screen(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    input: &InputMemory,
    notice: Option<&str>,
    interaction: DockInteraction,
    live: &LiveRenderer,
    motion_clock: &mut MotionClock,
    dock: &mut ActiveDock<'_>,
) -> Result<Option<UiSignal>, InteractiveError> {
    render_active_dock(
        input,
        notice,
        interaction,
        live,
        terminal,
        signals,
        dock,
        Some(motion_clock),
    )
    .await
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
    Question(Option<UserQuestionEnvelope>),
    Event(Option<crate::session::CommittedUiEvent>),
    Read(std::io::Result<usize>),
    MotionTick(MotionTick),
}

fn fences_ordinary_input_after_motion_preemption(work: &UiWork) -> bool {
    matches!(work, UiWork::Read(Ok(count)) if *count != 0)
        || matches!(work, UiWork::InputEscapeExpired)
}

#[allow(clippy::too_many_arguments)]
async fn next_ui_work(
    terminal: &AsyncTerminal,
    approvals: &mut ApprovalEnvelopeReceiver,
    user_questions: &mut UserQuestionReceiver,
    events: &mut CommittedUiReceiver,
    scratch: &mut [u8; TERMINAL_READ_BYTES],
    pending: Option<&PendingOutput>,
    frame_deadline: Option<Instant>,
    approval_arm_deadline: Option<Instant>,
    escape_deadline: Option<Instant>,
    input_escape_deadline: Option<Instant>,
    motion_deadline: Option<MotionTick>,
    read_enabled: bool,
    prefer_input: bool,
) -> UiWork {
    let deadline = frame_deadline.unwrap_or_else(Instant::now);
    let arm_deadline = approval_arm_deadline.unwrap_or_else(Instant::now);
    let escape_pending = escape_deadline.is_some();
    let escape_deadline_at = escape_deadline.unwrap_or_else(Instant::now);
    let input_escape_pending = input_escape_deadline.is_some();
    let input_escape_deadline_at = input_escape_deadline.unwrap_or_else(Instant::now);
    let motion_deadline_at = motion_deadline
        .map(|tick| tick.deadline)
        .unwrap_or_else(Instant::now);
    let motion_pending = pending.is_some_and(PendingOutput::is_motion_only);
    if motion_pending {
        return if prefer_input {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => UiWork::FrameExpired,
                () = tokio::time::sleep_until(arm_deadline), if approval_arm_deadline.is_some() => UiWork::ApprovalArmed,
                () = tokio::time::sleep_until(escape_deadline_at), if escape_pending => UiWork::EscapeExpired,
                () = tokio::time::sleep_until(input_escape_deadline_at), if input_escape_pending => UiWork::InputEscapeExpired,
                read = terminal.read_once(scratch), if read_enabled => UiWork::Read(read),
                envelope = approvals.recv() => UiWork::Envelope(envelope),
                question = user_questions.recv() => UiWork::Question(question),
                event = events.recv() => UiWork::Event(event),
                write = write_pending(terminal, pending) => UiWork::Write(write),
            }
        } else {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => UiWork::FrameExpired,
                () = tokio::time::sleep_until(arm_deadline), if approval_arm_deadline.is_some() => UiWork::ApprovalArmed,
                () = tokio::time::sleep_until(escape_deadline_at), if escape_pending => UiWork::EscapeExpired,
                () = tokio::time::sleep_until(input_escape_deadline_at), if input_escape_pending => UiWork::InputEscapeExpired,
                envelope = approvals.recv() => UiWork::Envelope(envelope),
                question = user_questions.recv() => UiWork::Question(question),
                event = events.recv() => UiWork::Event(event),
                read = terminal.read_once(scratch), if read_enabled => UiWork::Read(read),
                write = write_pending(terminal, pending) => UiWork::Write(write),
            }
        };
    }
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
            question = user_questions.recv() => UiWork::Question(question),
            event = events.recv(), if pending.is_none() => UiWork::Event(event),
            () = tokio::time::sleep_until(motion_deadline_at), if motion_deadline.is_some() && pending.is_none() => motion_deadline.map(UiWork::MotionTick).unwrap_or(UiWork::FrameExpired),
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
            question = user_questions.recv() => UiWork::Question(question),
            event = events.recv(), if pending.is_none() => UiWork::Event(event),
            read = terminal.read_once(scratch), if read_enabled => UiWork::Read(read),
            () = tokio::time::sleep_until(motion_deadline_at), if motion_deadline.is_some() && pending.is_none() => motion_deadline.map(UiWork::MotionTick).unwrap_or(UiWork::FrameExpired),
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

async fn write_dynamic_notice(
    notice: String,
    presenter: &mut InteractivePresenter,
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
) -> Result<Option<UiSignal>, InteractiveError> {
    let frame = LiveFrame::dynamic_notice(notice).map_err(|_| InteractiveError::Output)?;
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
    motion_clock: &mut MotionClock,
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
                    settle_motion_clock(motion_clock, dock.motion.requested().preference())?;
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
                        Some(motion_clock),
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
    mut motion_clock: Option<&mut MotionClock>,
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
                motion_clock.as_deref_mut(),
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
            && !matches!(
                model.interaction,
                DockInteraction::QuestionCustom { .. }
                    | DockInteraction::Approval(_)
                    | DockInteraction::ExactShellApproval(_)
            );
        let staged_file_suggestions = dock
            .file_suggestions
            .stage_presentation(show_file_suggestions)
            .map_err(|_| InteractiveError::Agent)?;
        let file_snapshot = if show_file_suggestions {
            dock.file_suggestions.snapshot()
        } else {
            FileSuggestionSnapshot::Hidden
        };
        let working = screen_working_candidate(
            motion_clock.as_deref_mut(),
            dock.motion.requested().preference(),
            dock.working,
        );
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
            dock.motion.requested(),
            model.live,
            file_snapshot,
            working,
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
                commit_surface(dock.view, dock.theme, dock.motion, surface.commit);
                dock.file_suggestions
                    .commit_presentation(staged_file_suggestions);
                if let Some(clock) = motion_clock.as_deref_mut() {
                    commit_screen_working(clock, &mut dock.working, Some(working));
                } else {
                    dock.working = working;
                }
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
            if matches!(
                &event.kind,
                CommittedUiKind::TypeOnly {
                    event_type: "goal/change"
                }
            ) {
                skipped = skipped.saturating_add(1);
                continue;
            }
            return Err(InteractiveError::Agent);
        }
        if let CommittedUiKind::UserMessage { source, content } = &event.kind {
            let is_prompt = matches!(source, UiUserSource::Human)
                || matches!(source, UiUserSource::Other { kind } if kind == "goal");
            if is_prompt {
                if content.as_str() != Some(expected_prompt) {
                    return Err(InteractiveError::Agent);
                }
                *prompt_committed = true;
            }
        }
        skipped = skipped.saturating_add(1);
    }
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::{fd::OwnedFd, unix::net::UnixStream},
        path::Path,
        time::Duration,
    };

    use super::{
        AfterFrame, ApprovalUiUpdate, EnhancedSubmission, FileSuggestionController, InlineIntent,
        InteractiveError, InteractiveExit, InteractivePresentation, MotionClock, MotionEligibility,
        PendingInlineOutput, PendingOutput, QuestionCustomInputOutcome,
        StagedFileSuggestionPresentation, StopIntent, SurfaceCommit, UiWork, apply_approval_update,
        apply_enhanced_input as apply_enhanced_input_with_files, apply_motion_command,
        apply_question_custom_event, apply_theme_command, classify_enhanced_submission,
        commit_screen_working, commit_surface, discard_ready_updates_after_stop,
        enqueue_enhanced_dock, enqueue_standalone_motion_baseline,
        expire_enhanced_escape as expire_enhanced_escape_with_files,
        fences_ordinary_input_after_motion_preemption, latch_observer_fault, next_ui_work,
        normalize_working_presentation, observe_enhanced_cleanup_signal, observe_failure,
        observe_signal, preempt_motion_pending, prepare_pending_for_resize,
        presentation_uses_enhanced, reset_file_suggestion_decoder, screen_working_candidate,
        session_context_estimate, settle_motion_clock, synchronize_motion_clock,
        turn_exhausted_session_capacity,
    };
    use crate::{
        agent::{ApprovalPrompt, ApprovalRequest},
        cli::{
            approval::{ApprovalChallengePool, ApprovalEnvelope},
            approval_join::ApprovalJoin,
            input::CanonicalRecordParser,
            signal::{SignalLatch, UiSignal},
            terminal::{AsyncTerminal, TerminalSize},
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
            key_decoder::{InputEvent, Key, KeyDecoder},
            motion::{
                MotionCommand, MotionPreference, MotionState, WorkingAge, WorkingPhase,
                WorkingPresentation,
            },
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
            &MotionState::default(),
            size,
            notice,
        )
    }

    #[test]
    fn question_custom_editor_keeps_the_exact_limit_when_one_more_byte_is_rejected() {
        let mut input = InputMemory::default();
        input.insert_text("saved draft").unwrap();
        input.begin_question_overlay().unwrap();
        input
            .insert_text(&"x".repeat(crate::user_question::MAX_CUSTOM_ANSWER_BYTES))
            .unwrap();
        assert_eq!(
            apply_question_custom_event(InputEvent::Key(Key::Char('y')), &mut input, 80).unwrap(),
            QuestionCustomInputOutcome::Invalid
        );
        assert_eq!(
            input.composer().byte_len(),
            crate::user_question::MAX_CUSTOM_ANSWER_BYTES
        );
        assert_eq!(
            apply_question_custom_event(InputEvent::Key(Key::Backspace), &mut input, 80).unwrap(),
            QuestionCustomInputOutcome::Redraw
        );
        assert_eq!(
            apply_question_custom_event(InputEvent::Key(Key::Char('y')), &mut input, 80).unwrap(),
            QuestionCustomInputOutcome::Redraw
        );
        let answer = input.finish_question_overlay().unwrap();
        assert_eq!(answer.len(), crate::user_question::MAX_CUSTOM_ANSWER_BYTES);
        assert!(answer.ends_with('y'));
        assert_eq!(input.composer().text(), "saved draft");
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
            &MotionState::default(),
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
            &MotionState::default(),
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
            &MotionState::default(),
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
            &MotionState::default(),
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
            &MotionState::default(),
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
                &MotionState::default(),
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
                &MotionState::default(),
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
        for _ in 0..7 {
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
            WorkingPresentation::PLAIN,
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
                motion: MotionState::default().requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            working: None,
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
                motion: MotionState::default().requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            working: None,
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
            WorkingPresentation::PLAIN,
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
    fn motion_only_screen_writes_abort_cleanly_or_poison_and_are_never_replayed() {
        let input = InputMemory::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let frame = super::enhanced_dock_frame(
            &input,
            None,
            DockInteraction::Running,
            size,
            FileSuggestionSnapshot::Hidden,
            WorkingPresentation {
                phase: WorkingPhase::Animated(0),
                age: WorkingAge::Fresh,
            },
        )
        .unwrap();
        let mut screen = InlineScreen::default();
        let mut attach = screen
            .stage_attach(super::screen_size(size), &frame, ThemePalette::Adaptive)
            .unwrap();
        attach.advance(attach.bytes().len()).unwrap();
        screen.commit(attach).unwrap();

        let view = ViewState::default();
        let theme = ThemeState::default();
        let motion = MotionState::default();
        let surface = || SurfaceCommit {
            request: view.requested(),
            theme: theme.requested(),
            motion: motion.requested(),
            offset: 0,
            total_rows: 0,
            page_rows: 0,
        };

        let write = screen.stage_dock(&frame, ThemePalette::Adaptive).unwrap();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::MotionDock {
                interaction: DockInteraction::Running,
                working: WorkingPresentation {
                    phase: WorkingPhase::Animated(0),
                    age: WorkingAge::Fresh,
                },
            },
            surface: surface(),
            working: Some(WorkingPresentation {
                phase: WorkingPhase::Animated(0),
                age: WorkingAge::Fresh,
            }),
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        let mut deadline = Some(Instant::now() + Duration::from_secs(5));
        let mut after = AfterFrame::TurnEnd;
        assert!(!preempt_motion_pending(
            &mut pending,
            &mut deadline,
            &mut after,
            None,
        ));
        assert!(pending.is_none());
        assert_eq!(deadline, None);
        assert_eq!(after, AfterFrame::None);
        assert!(!screen.is_poisoned());

        let mut write = screen.stage_dock(&frame, ThemePalette::Adaptive).unwrap();
        write.advance(1).unwrap();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::MotionDock {
                interaction: DockInteraction::Running,
                working: WorkingPresentation {
                    phase: WorkingPhase::Animated(0),
                    age: WorkingAge::Fresh,
                },
            },
            surface: surface(),
            working: Some(WorkingPresentation {
                phase: WorkingPhase::Animated(0),
                age: WorkingAge::Fresh,
            }),
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        assert!(preempt_motion_pending(
            &mut pending,
            &mut None,
            &mut AfterFrame::None,
            None,
        ));
        assert!(screen.is_poisoned());
        screen.recover_after_visual_reset();

        let mut attach = screen
            .stage_attach(super::screen_size(size), &frame, ThemePalette::Adaptive)
            .unwrap();
        attach.advance(attach.bytes().len()).unwrap();
        screen.commit(attach).unwrap();
        let mut write = screen.stage_dock(&frame, ThemePalette::Adaptive).unwrap();
        write.advance(1).unwrap();
        let mut pending = Some(PendingOutput::Inline(PendingInlineOutput {
            write,
            intent: InlineIntent::MotionDock {
                interaction: DockInteraction::Running,
                working: WorkingPresentation {
                    phase: WorkingPhase::Animated(0),
                    age: WorkingAge::Fresh,
                },
            },
            surface: surface(),
            working: Some(WorkingPresentation {
                phase: WorkingPhase::Animated(0),
                age: WorkingAge::Fresh,
            }),
            file_suggestions: StagedFileSuggestionPresentation::Absent,
        }));
        assert!(prepare_pending_for_resize(&mut pending, &mut screen).unwrap());
        assert!(screen.is_poisoned());
        assert!(pending.is_none());
    }

    #[test]
    fn motion_commands_are_local_closed_and_commit_through_the_screen_revision() {
        assert_eq!(
            classify_enhanced_submission(" /motion "),
            EnhancedSubmission::Command(CommandId::Motion)
        );
        assert_eq!(
            classify_enhanced_submission(" /motion reduced "),
            EnhancedSubmission::Motion(MotionCommand::Select(MotionPreference::Reduced))
        );
        assert_eq!(
            classify_enhanced_submission("/motion hidden extra"),
            EnhancedSubmission::Motion(MotionCommand::Invalid)
        );
        for command in ["/motions", "/Motion reduced"] {
            assert_eq!(
                classify_enhanced_submission(command),
                EnhancedSubmission::Motion(MotionCommand::Invalid)
            );
        }

        let mut motion = MotionState::default();
        let mut notice = None;
        apply_motion_command(
            MotionCommand::Select(MotionPreference::Reduced),
            &mut motion,
            &mut notice,
        )
        .unwrap();
        assert_eq!(notice.as_deref(), Some("Motion changed · reduced"));
        assert!(motion.is_transitioning());
        assert_eq!(motion.committed().preference(), MotionPreference::Full);
        assert!(motion.commit(motion.requested()));
        assert!(!motion.is_transitioning());

        apply_motion_command(MotionCommand::Show, &mut motion, &mut notice).unwrap();
        assert_eq!(
            notice.as_deref(),
            Some("Motion · reduced | Motion modes · full · reduced")
        );
        apply_motion_command(MotionCommand::Invalid, &mut motion, &mut notice).unwrap();
        assert_eq!(
            notice.as_deref(),
            Some("Unknown motion mode | Motion modes · full · reduced")
        );
    }

    #[test]
    fn motion_transition_fences_same_read_input_until_the_dock_commits() {
        let mut decoder = KeyDecoder::default();
        let mut input = InputMemory::default();
        let mut command_palette = CommandPaletteState::default();
        let mut view = ViewState::default();
        let theme = ThemeState::default();
        let mut motion = MotionState::default();
        let mut notice = None;
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };

        assert!(motion.request(MotionPreference::Reduced).unwrap());
        assert_eq!(
            apply_enhanced_input_with_files(
                &mut decoder,
                b"HIDDEN\r",
                &mut input,
                &mut command_palette,
                None,
                &mut view,
                &theme,
                &motion,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert!(input.composer().is_empty());

        assert!(motion.commit(motion.requested()));
        assert_eq!(
            apply_enhanced_input_with_files(
                &mut decoder,
                b"fresh",
                &mut input,
                &mut command_palette,
                None,
                &mut view,
                &theme,
                &motion,
                size,
                &mut notice,
            )
            .unwrap(),
            super::EnhancedInputAction::Redraw
        );
        assert_eq!(input.composer().text(), "fresh");
    }

    #[test]
    fn motion_recovery_fences_only_real_input_bytes_not_eof_or_read_failure() {
        assert!(fences_ordinary_input_after_motion_preemption(
            &UiWork::Read(Ok(1))
        ));
        assert!(fences_ordinary_input_after_motion_preemption(
            &UiWork::InputEscapeExpired
        ));
        assert!(!fences_ordinary_input_after_motion_preemption(
            &UiWork::Read(Ok(0))
        ));
        assert!(!fences_ordinary_input_after_motion_preemption(
            &UiWork::Read(Err(std::io::Error::other("test")))
        ));
    }

    #[test]
    fn requested_preference_normalizes_every_visible_working_surface() {
        let animated = WorkingPresentation {
            phase: WorkingPhase::Animated(2),
            age: WorkingAge::Long { seconds: 42 },
        };
        assert_eq!(
            normalize_working_presentation(animated, MotionPreference::Reduced),
            WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 5 },
            }
        );
        assert_eq!(
            normalize_working_presentation(WorkingPresentation::PLAIN, MotionPreference::Full),
            WorkingPresentation::STATIC
        );
    }

    #[tokio::test(start_paused = true)]
    async fn motion_clock_has_exact_delays_milestones_and_stale_tick_fences() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        let full = MotionState::default();
        let mut displayed = WorkingPresentation::STATIC;
        clock.synchronize(turn, true).unwrap();
        assert_eq!(
            clock.presentation(MotionPreference::Full, WorkingPhase::Static),
            WorkingPresentation::STATIC
        );
        let first = clock.deadline(MotionPreference::Full, displayed).unwrap();
        assert_eq!(first.deadline, Instant::now() + Duration::from_millis(300));

        tokio::time::advance(Duration::from_millis(299)).await;
        assert_eq!(
            clock.advance(first, full.requested(), full.committed()),
            None
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        let abandoned = clock
            .advance(first, full.requested(), full.committed())
            .unwrap();
        assert_eq!(abandoned.phase, WorkingPhase::Animated(0));
        assert_eq!(displayed, WorkingPresentation::STATIC);

        let second = clock.deadline(MotionPreference::Full, displayed).unwrap();
        assert_eq!(second.deadline, Instant::now() + Duration::from_millis(125));
        tokio::time::advance(Duration::from_millis(125)).await;
        displayed = clock
            .advance(second, full.requested(), full.committed())
            .unwrap();
        assert_eq!(displayed.phase, WorkingPhase::Animated(1));

        let stale = clock.deadline(MotionPreference::Full, displayed).unwrap();
        clock.synchronize(turn, false).unwrap();
        clock.synchronize(turn, true).unwrap();
        tokio::time::advance(Duration::from_millis(125)).await;
        assert_eq!(
            clock.advance(stale, full.requested(), full.committed()),
            None
        );

        tokio::time::advance(Duration::from_millis(575)).await;
        assert_eq!(
            clock
                .presentation(MotionPreference::Full, WorkingPhase::Static)
                .age,
            WorkingAge::OneSecond { seconds: 1 }
        );
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(
            clock
                .presentation(MotionPreference::Full, WorkingPhase::Static)
                .age,
            WorkingAge::Long { seconds: 5 }
        );

        let mut reduced_clock = MotionClock::new(turn);
        let reduced = MotionState::new(MotionPreference::Reduced);
        let mut reduced_displayed = WorkingPresentation::STATIC;
        reduced_clock.synchronize(turn, true).unwrap();
        assert_eq!(
            reduced_clock.presentation(MotionPreference::Reduced, WorkingPhase::Static),
            WorkingPresentation::STATIC
        );
        let one_second = reduced_clock
            .deadline(MotionPreference::Reduced, reduced_displayed)
            .unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        reduced_displayed = reduced_clock
            .advance(one_second, reduced.requested(), reduced.committed())
            .unwrap();
        assert_eq!(reduced_displayed.age, WorkingAge::OneSecond { seconds: 1 });
        let five_seconds = reduced_clock
            .deadline(MotionPreference::Reduced, reduced_displayed)
            .unwrap();
        tokio::time::advance(Duration::from_secs(4)).await;
        reduced_displayed = reduced_clock
            .advance(five_seconds, reduced.requested(), reduced.committed())
            .unwrap();
        assert_eq!(
            reduced_displayed,
            WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 5 },
            }
        );
        assert_eq!(
            reduced_clock.deadline(MotionPreference::Reduced, reduced_displayed),
            None
        );
        reduced_clock.synchronize(turn, false).unwrap();
        assert_eq!(
            reduced_clock.deadline(MotionPreference::Reduced, reduced_displayed),
            None
        );

        let mut coalesced = MotionClock::new(turn);
        tokio::time::advance(Duration::from_secs(6)).await;
        coalesced.synchronize(turn, true).unwrap();
        let overdue = coalesced
            .deadline(MotionPreference::Reduced, WorkingPresentation::STATIC)
            .unwrap();
        assert_eq!(overdue.deadline, Instant::now());
        assert_eq!(
            coalesced
                .advance(overdue, reduced.requested(), reduced.committed())
                .unwrap()
                .age,
            WorkingAge::Long { seconds: 5 }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_eligibility_generation_stages_a_fresh_static_baseline() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        let motion = MotionState::default();
        let mut displayed = WorkingPresentation::STATIC;
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        clock.commit_presentation(WorkingPresentation::STATIC);

        tokio::time::advance(Duration::from_millis(300)).await;
        let tick = clock.deadline(MotionPreference::Full, displayed).unwrap();
        displayed = clock
            .advance(tick, motion.requested(), motion.committed())
            .unwrap();
        assert_eq!(displayed.phase, WorkingPhase::Animated(0));

        // A notice, approval, detail view, or preference transition disarms
        // the clock. Returning to Focus starts a fresh 300 ms generation and
        // must not expose the completed Animated(0) credential meanwhile.
        tokio::time::advance(Duration::from_secs(5)).await;
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Full).unwrap();
        assert_eq!(displayed.phase, WorkingPhase::Animated(0));
        assert_eq!(
            clock.pending_baseline(),
            Some(WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 5 },
            })
        );
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        assert_eq!(displayed.phase, WorkingPhase::Animated(0));
        assert_eq!(
            clock.pending_baseline(),
            Some(WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 5 },
            })
        );
        assert_eq!(
            clock
                .deadline(MotionPreference::Full, displayed)
                .unwrap()
                .deadline,
            Instant::now() + Duration::from_millis(300)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reduced_hidden_milestone_commits_only_with_the_screen_transaction() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        let mut displayed = WorkingPresentation::STATIC;
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Reduced).unwrap();
        clock.commit_presentation(displayed);

        tokio::time::advance(Duration::from_secs(6)).await;
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Reduced).unwrap();
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Reduced).unwrap();
        let candidate = clock.pending_baseline().unwrap();
        assert_eq!(candidate.phase, WorkingPhase::Static);
        assert_eq!(candidate.age, WorkingAge::Long { seconds: 5 });
        assert_eq!(displayed, WorkingPresentation::STATIC);

        let input = InputMemory::default();
        let size = TerminalSize {
            rows: 24,
            columns: 80,
        };
        let old_frame = super::enhanced_dock_frame(
            &input,
            None,
            DockInteraction::Running,
            size,
            FileSuggestionSnapshot::Hidden,
            displayed,
        )
        .unwrap();
        let new_frame = super::enhanced_dock_frame(
            &input,
            None,
            DockInteraction::Running,
            size,
            FileSuggestionSnapshot::Hidden,
            candidate,
        )
        .unwrap();
        let mut screen = InlineScreen::default();
        let mut attach = screen
            .stage_attach(super::screen_size(size), &old_frame, ThemePalette::Adaptive)
            .unwrap();
        attach.advance(attach.bytes().len()).unwrap();
        screen.commit(attach).unwrap();

        let mut write = screen
            .stage_dock(&new_frame, ThemePalette::Adaptive)
            .unwrap();
        write.advance(write.bytes().len()).unwrap();
        assert_eq!(displayed, WorkingPresentation::STATIC);
        assert_eq!(clock.pending_baseline(), Some(candidate));

        screen.commit(write).unwrap();
        commit_screen_working(&mut clock, &mut displayed, Some(candidate));
        assert_eq!(displayed, candidate);
        assert_eq!(clock.pending_baseline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_preference_transition_normalizes_the_pending_baseline_once() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        tokio::time::advance(Duration::from_secs(6)).await;
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        assert_eq!(
            clock.pending_baseline(),
            Some(WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 6 },
            })
        );

        let rendered = clock
            .pending_baseline_for(MotionPreference::Reduced)
            .unwrap();
        assert_eq!(
            rendered,
            WorkingPresentation {
                phase: WorkingPhase::Static,
                age: WorkingAge::Long { seconds: 5 },
            }
        );
        clock.commit_presentation(rendered);
        assert_eq!(clock.pending_baseline(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn ineligible_reduced_full_round_trip_restores_whole_turn_elapsed_age() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        tokio::time::advance(Duration::from_secs(42)).await;
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Full).unwrap();
        let mut displayed = WorkingPresentation::STATIC;
        let full = screen_working_candidate(Some(&mut clock), MotionPreference::Full, displayed);
        assert_eq!(full.age, WorkingAge::Long { seconds: 42 });
        commit_screen_working(&mut clock, &mut displayed, Some(full));

        let reduced =
            screen_working_candidate(Some(&mut clock), MotionPreference::Reduced, displayed);
        assert_eq!(reduced.age, WorkingAge::Long { seconds: 5 });
        commit_screen_working(&mut clock, &mut displayed, Some(reduced));
        assert_eq!(clock.pending_baseline(), None);

        let restored =
            screen_working_candidate(Some(&mut clock), MotionPreference::Full, displayed);
        assert_eq!(restored.phase, WorkingPhase::Static);
        assert_eq!(restored.age, WorkingAge::Long { seconds: 42 });
    }

    #[tokio::test(start_paused = true)]
    async fn a_consumed_hidden_baseline_refreshes_before_direct_reveal() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        let mut displayed = WorkingPresentation::STATIC;
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        commit_screen_working(
            &mut clock,
            &mut displayed,
            Some(WorkingPresentation::STATIC),
        );

        tokio::time::advance(Duration::from_secs(5)).await;
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Full).unwrap();
        let hidden = screen_working_candidate(Some(&mut clock), MotionPreference::Full, displayed);
        assert_eq!(hidden.age, WorkingAge::Long { seconds: 5 });
        commit_screen_working(&mut clock, &mut displayed, Some(hidden));
        assert_eq!(clock.pending_baseline(), None);

        tokio::time::advance(Duration::from_secs(37)).await;
        let revealed =
            screen_working_candidate(Some(&mut clock), MotionPreference::Full, displayed);
        assert_eq!(revealed.phase, WorkingPhase::Static);
        assert_eq!(revealed.age, WorkingAge::Long { seconds: 42 });
    }

    #[test]
    fn a_hidden_baseline_never_blocks_approval_takeover() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        clock.commit_presentation(WorkingPresentation::STATIC);
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Full).unwrap();

        let mut pending = None;
        let mut deadline = None;
        let mut after = AfterFrame::None;
        enqueue_standalone_motion_baseline(
            false,
            false,
            &clock,
            &mut pending,
            &mut deadline,
            &mut after,
        )
        .unwrap();
        assert!(pending.is_none());
        assert!(clock.pending_baseline().is_some());

        enqueue_enhanced_dock(
            DockInteraction::Approval(DockApprovalSelection::Reject),
            AfterFrame::ApprovalFence,
            &mut pending,
            &mut deadline,
            &mut after,
        )
        .unwrap();
        assert!(matches!(
            pending,
            Some(PendingOutput::Dock(DockInteraction::Approval(
                DockApprovalSelection::Reject
            )))
        ));
    }

    #[test]
    fn a_zero_preempted_baseline_is_used_by_the_next_direct_screen_transaction() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        let baseline = clock.pending_baseline().unwrap();
        let mut pending = Some(PendingOutput::MotionDock {
            interaction: DockInteraction::Running,
            working: baseline,
        });
        assert!(!preempt_motion_pending(
            &mut pending,
            &mut None,
            &mut AfterFrame::None,
            None,
        ));
        assert!(pending.is_none());

        let old = WorkingPresentation {
            phase: WorkingPhase::Animated(0),
            age: WorkingAge::Fresh,
        };
        assert_eq!(
            screen_working_candidate(Some(&mut clock), MotionPreference::Full, old),
            baseline
        );
        assert_eq!(clock.pending_baseline(), Some(baseline));
    }

    #[test]
    fn interrupt_cleanup_restages_static_even_after_an_ineligible_baseline_committed() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        synchronize_motion_clock(&mut clock, turn, true, MotionPreference::Full).unwrap();
        clock.commit_presentation(WorkingPresentation::STATIC);
        synchronize_motion_clock(&mut clock, turn, false, MotionPreference::Full).unwrap();
        let hidden = clock.pending_baseline().unwrap();
        clock.commit_presentation(hidden);
        assert_eq!(clock.pending_baseline(), None);

        settle_motion_clock(&mut clock, MotionPreference::Full).unwrap();
        let abandoned = WorkingPresentation {
            phase: WorkingPhase::Animated(3),
            age: WorkingAge::Fresh,
        };
        let cleanup = screen_working_candidate(Some(&mut clock), MotionPreference::Full, abandoned);
        assert_eq!(cleanup.phase, WorkingPhase::Static);
        assert_eq!(clock.deadline(MotionPreference::Full, cleanup), None);
        assert_eq!(clock.pending_baseline(), Some(cleanup));
    }

    #[test]
    fn every_non_working_surface_disarms_the_motion_clock() {
        let eligible = MotionEligibility {
            enhanced: true,
            turn_open: true,
            approval_inactive: true,
            no_question: true,
            focus: true,
            no_notice: true,
            queue_empty: true,
            file_hidden: true,
            palette_hidden: true,
            motion_committed: true,
        };
        assert!(eligible.is_eligible());

        let excluded = [
            MotionEligibility {
                enhanced: false,
                ..eligible
            },
            MotionEligibility {
                turn_open: false,
                ..eligible
            },
            MotionEligibility {
                approval_inactive: false,
                ..eligible
            },
            MotionEligibility {
                no_question: false,
                ..eligible
            },
            MotionEligibility {
                focus: false,
                ..eligible
            },
            MotionEligibility {
                no_notice: false,
                ..eligible
            },
            MotionEligibility {
                queue_empty: false,
                ..eligible
            },
            MotionEligibility {
                file_hidden: false,
                ..eligible
            },
            MotionEligibility {
                palette_hidden: false,
                ..eligible
            },
            MotionEligibility {
                motion_committed: false,
                ..eligible
            },
        ];
        assert!(excluded.into_iter().all(|state| !state.is_eligible()));
    }

    #[test]
    fn turn_end_disarms_motion_while_the_agent_future_is_still_pending() {
        let turn = crate::session::TurnId::new(1).unwrap();
        let mut clock = MotionClock::new(turn);
        clock.synchronize(turn, true).unwrap();
        assert!(
            clock
                .deadline(MotionPreference::Full, WorkingPresentation::STATIC)
                .is_some()
        );

        // The UI can observe TurnEnd before the owning Agent future finishes
        // cleanup. That settlement fact must permanently stop this turn's clock.
        clock.synchronize(turn, false).unwrap();
        assert_eq!(
            clock.deadline(MotionPreference::Full, WorkingPresentation::STATIC),
            None
        );
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
            WorkingPresentation::PLAIN,
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
                motion: MotionState::default().requested(),
                offset: 0,
                total_rows: 0,
                page_rows: 0,
            },
            working: None,
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
            WorkingPresentation::PLAIN,
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
            motion: MotionState::default().requested(),
            offset: 0,
            total_rows: 0,
            page_rows: 0,
        };
        commit_surface(
            &mut view,
            &mut theme,
            &mut MotionState::default(),
            recovered_surface,
        );
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

    #[tokio::test]
    async fn ready_approval_envelopes_outrank_a_writable_motion_frame() {
        for prefer_input in [true, false] {
            let (terminal_side, _peer) = UnixStream::pair().unwrap();
            terminal_side.set_nonblocking(true).unwrap();
            let input: OwnedFd = terminal_side.try_clone().unwrap().into();
            let output: OwnedFd = terminal_side.into();
            let terminal = AsyncTerminal::from_owned_fds_for_test(input, output);

            let (sender, mut approvals) = tokio::sync::mpsc::channel(1);
            let request = ApprovalRequest::new(
                ApprovalRequestId::new("approval-motion-priority"),
                "apply_patch".to_owned(),
                CallId::new("call-motion-priority"),
                &ApprovalPrompt::new(Some("change one file".to_owned()), "bounded preview")
                    .unwrap(),
            );
            let (response, _receive) = oneshot::channel();
            sender
                .send(ApprovalEnvelope { request, response })
                .await
                .unwrap();

            let mut session = Session::new("motion-priority").unwrap();
            let mut events = session.attach_ui_observer().unwrap();
            let (_question_sender, mut user_questions) = tokio::sync::mpsc::channel(1);
            let mut scratch = [0_u8; super::TERMINAL_READ_BYTES];
            let pending = PendingOutput::MotionDock {
                interaction: DockInteraction::Running,
                working: WorkingPresentation {
                    phase: WorkingPhase::Animated(0),
                    age: WorkingAge::Fresh,
                },
            };
            let work = next_ui_work(
                &terminal,
                &mut approvals,
                &mut user_questions,
                &mut events,
                &mut scratch,
                Some(&pending),
                Some(Instant::now() + Duration::from_secs(5)),
                None,
                None,
                None,
                None,
                false,
                prefer_input,
            )
            .await;
            assert!(matches!(work, UiWork::Envelope(Some(_))));
        }
    }

    #[tokio::test]
    async fn ready_session_facts_outrank_a_writable_motion_frame() {
        for prefer_input in [true, false] {
            let (terminal_side, _peer) = UnixStream::pair().unwrap();
            terminal_side.set_nonblocking(true).unwrap();
            let input: OwnedFd = terminal_side.try_clone().unwrap().into();
            let output: OwnedFd = terminal_side.into();
            let terminal = AsyncTerminal::from_owned_fds_for_test(input, output);
            let (_sender, mut approvals) = tokio::sync::mpsc::channel(1);
            let (_question_sender, mut user_questions) = tokio::sync::mpsc::channel(1);
            let mut session = Session::new("motion-event-priority").unwrap();
            let mut events = session.attach_ui_observer().unwrap();
            session.append(NewEvent::log(EventKind::EndSeed)).unwrap();
            let mut scratch = [0_u8; super::TERMINAL_READ_BYTES];
            let pending = PendingOutput::MotionDock {
                interaction: DockInteraction::Running,
                working: WorkingPresentation {
                    phase: WorkingPhase::Animated(0),
                    age: WorkingAge::Fresh,
                },
            };
            let work = next_ui_work(
                &terminal,
                &mut approvals,
                &mut user_questions,
                &mut events,
                &mut scratch,
                Some(&pending),
                Some(Instant::now() + Duration::from_secs(5)),
                None,
                None,
                None,
                None,
                false,
                prefer_input,
            )
            .await;
            assert!(matches!(work, UiWork::Event(Some(_))));
        }
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
