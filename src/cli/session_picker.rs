//! Header-only startup selection for bare resume.

use std::{fmt::Write as _, ops::ControlFlow, path::Path, time::Duration};

use tokio::time::Instant;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::{
    session::{SessionId, SessionMetadata},
    tui::{
        key_decoder::{InputEvent, Key, KeyDecoder},
        visible::render_visible_owned_bounded,
    },
};

use super::{
    approval_selector::ESCAPE_SEQUENCE_WAIT,
    input::{CanonicalRecordParser, InputRecordEvent, MAX_INTERACTIVE_PROMPT_BYTES},
    interactive::{InteractivePresentation, presentation_uses_enhanced},
    signal::{InteractiveSignal, SignalStreams, UiSignal},
    terminal::{AsyncTerminal, TERMINAL_READ_BYTES, TerminalError, TerminalSize},
};

const MAX_VISIBLE_SESSIONS: usize = 8;
const OUTPUT_DEADLINE: Duration = Duration::from_secs(5);
const MAX_SAFE_WORKSPACE_LABEL_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PickerOutcome {
    Selected(SessionId),
    Cancelled,
    Signal(UiSignal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PickerError {
    Terminal(TerminalError),
    Output,
}

impl From<TerminalError> for PickerError {
    fn from(error: TerminalError) -> Self {
        Self::Terminal(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PickerUpdate {
    None,
    Redraw,
    Confirm,
    Cancel,
}

#[derive(Debug)]
struct PickerState {
    selected: usize,
    invalid_input: bool,
    decoder: KeyDecoder,
}

impl PickerState {
    fn new() -> Result<Self, PickerError> {
        let mut decoder = KeyDecoder::default();
        decoder.reset_epoch().map_err(|_| PickerError::Output)?;
        Ok(Self {
            selected: 0,
            invalid_input: false,
            decoder,
        })
    }

    fn selected(&self) -> usize {
        self.selected
    }

    fn escape_pending(&self) -> bool {
        self.decoder.escape_pending()
    }

    fn expire_escape(&mut self) -> PickerUpdate {
        match self.decoder.expire_escape().map(|decoded| decoded.event) {
            Some(InputEvent::Key(Key::Escape)) => PickerUpdate::Cancel,
            Some(_) => {
                self.invalid_input = true;
                PickerUpdate::Redraw
            }
            None => PickerUpdate::None,
        }
    }

    fn feed(&mut self, bytes: &[u8], session_count: usize) -> PickerUpdate {
        let mut moved_in_this_read = false;
        let mut redraw = false;
        let mut final_update = None;
        let mut decoder = std::mem::take(&mut self.decoder);
        let expected_epoch = decoder.epoch();
        let _ = decoder.feed(bytes, |decoded| {
            if decoded.epoch != expected_epoch {
                self.invalid_input = true;
                redraw = true;
                return ControlFlow::Continue(());
            }
            let update = self.feed_event(decoded.event, session_count, moved_in_this_read);
            match update {
                PickerUpdate::None => {}
                PickerUpdate::Redraw => {
                    redraw = true;
                    moved_in_this_read = true;
                }
                PickerUpdate::Confirm | PickerUpdate::Cancel => {
                    final_update = Some(update);
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        self.decoder = decoder;
        final_update.unwrap_or(if redraw {
            PickerUpdate::Redraw
        } else {
            PickerUpdate::None
        })
    }

    fn feed_event(
        &mut self,
        event: InputEvent,
        session_count: usize,
        moved_in_this_read: bool,
    ) -> PickerUpdate {
        self.invalid_input = false;
        match event {
            InputEvent::Key(Key::Escape | Key::Eof) => PickerUpdate::Cancel,
            InputEvent::Key(Key::Enter | Key::Newline) => {
                if moved_in_this_read {
                    PickerUpdate::Redraw
                } else {
                    PickerUpdate::Confirm
                }
            }
            InputEvent::Key(Key::Up | Key::Left | Key::BackTab) => {
                self.selected = self.selected.saturating_sub(1);
                PickerUpdate::Redraw
            }
            InputEvent::Key(Key::Down | Key::Right | Key::Tab) => {
                self.selected = self
                    .selected
                    .saturating_add(1)
                    .min(session_count.saturating_sub(1));
                PickerUpdate::Redraw
            }
            InputEvent::Key(Key::PageUp) => {
                self.selected = self.selected.saturating_sub(MAX_VISIBLE_SESSIONS);
                PickerUpdate::Redraw
            }
            InputEvent::Key(Key::PageDown) => {
                self.selected = self
                    .selected
                    .saturating_add(MAX_VISIBLE_SESSIONS)
                    .min(session_count.saturating_sub(1));
                PickerUpdate::Redraw
            }
            InputEvent::Key(Key::Home) => {
                self.selected = 0;
                PickerUpdate::Redraw
            }
            InputEvent::Key(Key::End) => {
                self.selected = session_count.saturating_sub(1);
                PickerUpdate::Redraw
            }
            InputEvent::Key(_)
            | InputEvent::PasteStarted
            | InputEvent::Paste(_)
            | InputEvent::PasteRejected(_)
            | InputEvent::Rejected(_) => {
                self.invalid_input = true;
                PickerUpdate::Redraw
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PaintState {
    lines: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WriteReport {
    resized: bool,
    signal: Option<UiSignal>,
}

pub(super) async fn pick(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    sessions: &[SessionMetadata],
    presentation: InteractivePresentation,
) -> Result<PickerOutcome, PickerError> {
    if sessions.is_empty() {
        let report = write_all(
            terminal,
            signals,
            b"No resumable sessions for this workspace.\n",
        )
        .await?;
        return Ok(report
            .signal
            .map(PickerOutcome::Signal)
            .unwrap_or(PickerOutcome::Cancelled));
    }

    if presentation_uses_enhanced(presentation, terminal.size()) {
        pick_enhanced(terminal, signals, sessions).await
    } else {
        pick_linear(terminal, signals, sessions).await
    }
}

async fn pick_enhanced(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    sessions: &[SessionMetadata],
) -> Result<PickerOutcome, PickerError> {
    let mut state = PickerState::new()?;
    terminal.flush_input()?;
    let mode = terminal.enter_selector_mode()?;
    let mut paint = PaintState::default();
    let mut cleanup_safe = true;
    let mut scratch = [0_u8; TERMINAL_READ_BYTES];

    let result = async {
        loop {
            let size = terminal.size().unwrap_or(TerminalSize {
                rows: 12,
                columns: 44,
            });
            let frame = render_enhanced_frame(sessions, &state, size, paint)?;
            let frame_lines = frame.line_count;
            let report = match write_all(terminal, signals, frame.bytes.as_bytes()).await {
                Ok(report) => report,
                Err(error) => {
                    cleanup_safe = false;
                    return Err(error);
                }
            };
            if let Some(signal) = report.signal {
                cleanup_safe = false;
                return Ok(PickerOutcome::Signal(signal));
            }
            paint.lines = frame_lines;
            if report.resized {
                continue;
            }

            loop {
                let escape_deadline = state
                    .escape_pending()
                    .then(|| Instant::now() + ESCAPE_SEQUENCE_WAIT);
                let escape_at = escape_deadline.unwrap_or_else(Instant::now);
                let event = tokio::select! {
                    biased;
                    signal = signals.next_interactive() => EnhancedEvent::Signal(signal),
                    () = tokio::time::sleep_until(escape_at), if escape_deadline.is_some() => EnhancedEvent::EscapeExpired,
                    read = terminal.read_once(&mut scratch) => EnhancedEvent::Read(read),
                };
                let update = match event {
                    EnhancedEvent::Signal(InteractiveSignal::Resize) => PickerUpdate::Redraw,
                    EnhancedEvent::Signal(InteractiveSignal::Stop(signal)) => {
                        return Ok(PickerOutcome::Signal(signal));
                    }
                    EnhancedEvent::EscapeExpired => state.expire_escape(),
                    EnhancedEvent::Read(Ok(0)) => PickerUpdate::Cancel,
                    EnhancedEvent::Read(Ok(count)) => state.feed(&scratch[..count], sessions.len()),
                    EnhancedEvent::Read(Err(_)) => {
                        return Err(PickerError::Terminal(TerminalError::Unavailable));
                    }
                };
                match update {
                    PickerUpdate::None => {}
                    PickerUpdate::Redraw => break,
                    PickerUpdate::Confirm => {
                        return Ok(PickerOutcome::Selected(
                            sessions[state.selected()].id().clone(),
                        ));
                    }
                    PickerUpdate::Cancel => return Ok(PickerOutcome::Cancelled),
                }
            }
        }
    }
    .await;

    terminal.best_effort_cursor_reset();
    mode.restore()?;
    let result = result?;
    if !cleanup_safe {
        return Ok(result);
    }
    let cleanup = render_cleanup(paint.lines)?;
    let report = write_all(terminal, signals, cleanup.as_bytes()).await?;
    Ok(report.signal.map(PickerOutcome::Signal).unwrap_or(result))
}

enum EnhancedEvent {
    Signal(InteractiveSignal),
    EscapeExpired,
    Read(std::io::Result<usize>),
}

async fn pick_linear(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    sessions: &[SessionMetadata],
) -> Result<PickerOutcome, PickerError> {
    terminal.flush_input()?;
    let snapshot = render_linear_snapshot(sessions)?;
    let report = write_all(terminal, signals, snapshot.as_bytes()).await?;
    if let Some(signal) = report.signal {
        return Ok(PickerOutcome::Signal(signal));
    }

    let mut parser = CanonicalRecordParser::new(MAX_INTERACTIVE_PROMPT_BYTES);
    let mut scratch = [0_u8; TERMINAL_READ_BYTES];
    loop {
        let event = tokio::select! {
            biased;
            signal = signals.next_interactive() => LinearEvent::Signal(signal),
            read = terminal.read_once(&mut scratch) => LinearEvent::Read(read),
        };
        match event {
            LinearEvent::Signal(InteractiveSignal::Resize) => {}
            LinearEvent::Signal(InteractiveSignal::Stop(signal)) => {
                return Ok(PickerOutcome::Signal(signal));
            }
            LinearEvent::Read(Ok(0)) => return Ok(PickerOutcome::Cancelled),
            LinearEvent::Read(Err(_)) => {
                return Err(PickerError::Terminal(TerminalError::Unavailable));
            }
            LinearEvent::Read(Ok(count)) => {
                let mut first = None;
                parser.feed(&scratch[..count], count < TERMINAL_READ_BYTES, |event| {
                    if first.is_none() {
                        first = Some(event);
                    }
                });
                let Some(event) = first else {
                    continue;
                };
                match parse_linear_event(event, sessions.len()) {
                    LinearChoice::Select(index) => {
                        return Ok(PickerOutcome::Selected(sessions[index].id().clone()));
                    }
                    LinearChoice::Cancel => return Ok(PickerOutcome::Cancelled),
                    LinearChoice::Retry => {
                        parser.reset(MAX_INTERACTIVE_PROMPT_BYTES);
                        let report = write_all(
                            terminal,
                            signals,
                            b"Enter a session number, or q to cancel: ",
                        )
                        .await?;
                        if let Some(signal) = report.signal {
                            return Ok(PickerOutcome::Signal(signal));
                        }
                    }
                }
            }
        }
    }
}

enum LinearEvent {
    Signal(InteractiveSignal),
    Read(std::io::Result<usize>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinearChoice {
    Select(usize),
    Cancel,
    Retry,
}

fn parse_linear_event(event: InputRecordEvent, session_count: usize) -> LinearChoice {
    let InputRecordEvent::Record { text, .. } = event else {
        return LinearChoice::Retry;
    };
    if text.is_empty() {
        return LinearChoice::Select(0);
    }
    if text == "q" {
        return LinearChoice::Cancel;
    }
    if text.starts_with('0') || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return LinearChoice::Retry;
    }
    text.parse::<usize>()
        .ok()
        .filter(|number| (1..=session_count).contains(number))
        .map(|number| LinearChoice::Select(number - 1))
        .unwrap_or(LinearChoice::Retry)
}

struct RenderedFrame {
    bytes: String,
    line_count: usize,
}

fn render_enhanced_frame(
    sessions: &[SessionMetadata],
    state: &PickerState,
    size: TerminalSize,
    previous: PaintState,
) -> Result<RenderedFrame, PickerError> {
    let columns = usize::from(size.columns.max(1));
    let visible = usize::from(size.rows.saturating_sub(4))
        .clamp(1, MAX_VISIBLE_SESSIONS)
        .min(sessions.len());
    let max_start = sessions.len().saturating_sub(visible);
    let start = state.selected().saturating_sub(visible / 2).min(max_start);
    let mut lines = Vec::new();
    lines
        .try_reserve_exact(visible + 3)
        .map_err(|_| PickerError::Output)?;
    lines.push(clip_cells("Resume a session", columns));
    lines.push(clip_cells("workspace · created · id", columns));
    for (index, session) in sessions.iter().enumerate().skip(start).take(visible) {
        let row = format_session_row(session, columns.saturating_sub(2))?;
        let marker = if index == state.selected() {
            "› "
        } else {
            "  "
        };
        let line = clip_cells(&format!("{marker}{row}"), columns);
        lines.push(if index == state.selected() {
            format!("\x1b[1;30;46m{line}\x1b[0m")
        } else {
            line
        });
    }
    let hint = if state.invalid_input {
        "Use arrows and Enter; Esc cancels"
    } else {
        "↑/↓ move · Enter resume · Esc cancel"
    };
    lines.push(clip_cells(hint, columns));

    let line_count = lines.len();
    let painted = previous.lines.max(line_count);
    let capacity = painted
        .saturating_mul(columns.saturating_mul(4).saturating_add(64))
        .saturating_add(64);
    let mut bytes = String::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| PickerError::Output)?;
    if previous.lines == 0 {
        bytes.push_str("\x1b[?25l");
    } else {
        write!(&mut bytes, "\x1b[{}A", previous.lines).map_err(|_| PickerError::Output)?;
    }
    for index in 0..painted {
        bytes.push_str("\r\x1b[2K");
        if let Some(line) = lines.get(index) {
            bytes.push_str(line);
        }
        bytes.push_str("\r\n");
    }
    if painted > line_count {
        write!(&mut bytes, "\x1b[{}A", painted - line_count).map_err(|_| PickerError::Output)?;
    }
    Ok(RenderedFrame { bytes, line_count })
}

fn render_cleanup(lines: usize) -> Result<String, PickerError> {
    let mut output = String::new();
    output
        .try_reserve_exact(lines.saturating_mul(16).saturating_add(32))
        .map_err(|_| PickerError::Output)?;
    if lines != 0 {
        write!(&mut output, "\x1b[{lines}A").map_err(|_| PickerError::Output)?;
        for _ in 0..lines {
            output.push_str("\r\x1b[2K\r\n");
        }
        write!(&mut output, "\x1b[{lines}A").map_err(|_| PickerError::Output)?;
    }
    output.push_str("\r\x1b[2K\x1b[?25h\x1b[0m");
    Ok(output)
}

fn render_linear_snapshot(sessions: &[SessionMetadata]) -> Result<String, PickerError> {
    let mut output = String::new();
    output
        .try_reserve_exact(sessions.len().saturating_mul(128).saturating_add(128))
        .map_err(|_| PickerError::Output)?;
    output.push_str("Resume a session:\n");
    for (index, session) in sessions.iter().enumerate() {
        let row = format_session_row(session, 112)?;
        writeln!(&mut output, "  {}. {row}", index + 1).map_err(|_| PickerError::Output)?;
    }
    output.push_str("Enter a session number (empty selects newest), or q to cancel: ");
    Ok(output)
}

fn format_session_row(
    session: &SessionMetadata,
    maximum_cells: usize,
) -> Result<String, PickerError> {
    let basename = Path::new(session.workspace())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(session.workspace());
    let visible = render_visible_owned_bounded(basename, false, MAX_SAFE_WORKSPACE_LABEL_BYTES)
        .map_err(|_| PickerError::Output)?
        .unwrap_or_else(|| "[workspace]".to_owned());
    let id = session
        .id()
        .as_str()
        .strip_prefix("session-")
        .unwrap_or(session.id().as_str());
    let short_id = id.get(..8).unwrap_or(id);
    let created_and_id = format!("{} · {short_id}", session.created_at());
    if created_and_id.width() >= maximum_cells {
        return Ok(clip_cells(short_id, maximum_cells));
    }
    let suffix = format!(" · {created_and_id}");
    let label_cells = maximum_cells.saturating_sub(suffix.width());
    Ok(format!("{}{suffix}", clip_cells(&visible, label_cells)))
}

fn clip_cells(input: &str, maximum: usize) -> String {
    if maximum == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0_usize;
    for grapheme in input.graphemes(true) {
        let width = grapheme.width();
        if used.saturating_add(width) > maximum {
            break;
        }
        output.push_str(grapheme);
        used = used.saturating_add(width);
    }
    output
}

async fn write_all(
    terminal: &AsyncTerminal,
    signals: &mut SignalStreams,
    bytes: &[u8],
) -> Result<WriteReport, PickerError> {
    let deadline = Instant::now() + OUTPUT_DEADLINE;
    let mut written = 0_usize;
    let mut report = WriteReport::default();
    while written < bytes.len() {
        let event = tokio::select! {
            biased;
            signal = signals.next_interactive() => WriteEvent::Signal(signal),
            () = tokio::time::sleep_until(deadline) => WriteEvent::Expired,
            write = terminal.write_once(&bytes[written..]) => WriteEvent::Write(write),
        };
        match event {
            WriteEvent::Signal(InteractiveSignal::Resize) => {
                report.resized = true;
                if Instant::now() >= deadline {
                    return Err(PickerError::Output);
                }
            }
            WriteEvent::Signal(InteractiveSignal::Stop(signal)) => {
                report.signal = Some(signal);
                return Ok(report);
            }
            WriteEvent::Expired | WriteEvent::Write(Err(_)) => return Err(PickerError::Output),
            WriteEvent::Write(Ok(count)) => {
                written = written.checked_add(count).ok_or(PickerError::Output)?;
            }
        }
    }
    Ok(report)
}

enum WriteEvent {
    Signal(InteractiveSignal),
    Expired,
    Write(std::io::Result<usize>),
}

#[cfg(test)]
mod tests {
    use crate::session::{SessionId, SessionMetadata, UnixMillis};

    use super::{
        LinearChoice, PaintState, PickerState, PickerUpdate, clip_cells, parse_linear_event,
        render_enhanced_frame, render_linear_snapshot,
    };
    use crate::cli::{input::InputRecordEvent, terminal::TerminalSize};

    fn metadata(id: &str, created_at: i64, workspace: &str) -> SessionMetadata {
        SessionMetadata::new_for_test(
            SessionId::new(id),
            UnixMillis::new(created_at).unwrap(),
            workspace,
        )
    }

    #[test]
    fn navigation_clamps_and_same_read_enter_cannot_confirm() {
        let mut picker = PickerState::new().unwrap();
        assert_eq!(picker.feed(b"\x1b[B\r", 3), PickerUpdate::Redraw);
        assert_eq!(picker.selected(), 1);
        assert_eq!(picker.feed(b"\r", 3), PickerUpdate::Confirm);
        assert_eq!(picker.feed(b"\x1b[F", 3), PickerUpdate::Redraw);
        assert_eq!(picker.selected(), 2);
        assert_eq!(picker.feed(b"\x1b[B", 3), PickerUpdate::Redraw);
        assert_eq!(picker.selected(), 2);
        assert_eq!(picker.feed(b"\x1b[H", 3), PickerUpdate::Redraw);
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.feed(b"\x1b[A", 3), PickerUpdate::Redraw);
        assert_eq!(picker.selected(), 0);
    }

    #[test]
    fn enhanced_frame_is_bounded_safe_and_keeps_the_selected_identity_visible() {
        let sessions = (0..12)
            .map(|index| {
                metadata(
                    &format!("session-{index:08}-e29b-41d4-a716-446655440000"),
                    100 - index,
                    if index == 6 {
                        "/work/evil\u{1b}]52;c;secret\u{7}\u{202e}"
                    } else {
                        "/work/project"
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut picker = PickerState::new().unwrap();
        picker.selected = 6;
        let frame = render_enhanced_frame(
            &sessions,
            &picker,
            TerminalSize {
                rows: 12,
                columns: 44,
            },
            PaintState::default(),
        )
        .unwrap();
        assert_eq!(frame.line_count, 11);
        assert!(frame.bytes.contains("00000006"));
        assert!(!frame.bytes.contains("]52;c;secret\u{7}"));
        assert!(frame.bytes.contains("\\u{1b}"));
        assert!(frame.bytes.len() < 8 * 1024);

        for (rows, columns, expected_lines) in
            [(34, 112, 11), (24, 80, 11), (20, 44, 11), (5, 12, 4)]
        {
            let compact = render_enhanced_frame(
                &sessions,
                &picker,
                TerminalSize { rows, columns },
                PaintState::default(),
            )
            .unwrap();
            assert_eq!(compact.line_count, expected_lines);
            assert!(compact.bytes.contains("00000006"));
        }

        assert_eq!(clip_cells("中文abc", 4), "中文");
        assert_eq!(clip_cells("👨‍👩‍👧‍👦x", 2), "👨‍👩‍👧‍👦");
    }

    #[test]
    fn linear_snapshot_has_no_escape_and_accepts_only_bounded_choices() {
        let sessions = [
            metadata(
                "session-550e8400-e29b-41d4-a716-446655440000",
                7,
                "/work/a\u{1b}[31m",
            ),
            metadata("session-650e8400-e29b-41d4-a716-446655440000", 6, "/work/b"),
        ];
        let output = render_linear_snapshot(&sessions).unwrap();
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("\\u{1b}[31m"));
        assert_eq!(
            parse_linear_event(
                InputRecordEvent::Record {
                    text: String::new(),
                    terminated_by_lf: true,
                },
                2,
            ),
            LinearChoice::Select(0)
        );
        assert_eq!(
            parse_linear_event(
                InputRecordEvent::Record {
                    text: "2".to_owned(),
                    terminated_by_lf: true,
                },
                2,
            ),
            LinearChoice::Select(1)
        );
        assert_eq!(
            parse_linear_event(
                InputRecordEvent::Record {
                    text: "q".to_owned(),
                    terminated_by_lf: true,
                },
                2,
            ),
            LinearChoice::Cancel
        );
        assert_eq!(
            parse_linear_event(InputRecordEvent::InvalidUtf8, 2),
            LinearChoice::Retry
        );
        for invalid in ["0", "3", "01", "+1", " 1 ", "Q"] {
            assert_eq!(
                parse_linear_event(
                    InputRecordEvent::Record {
                        text: invalid.to_owned(),
                        terminated_by_lf: true,
                    },
                    2,
                ),
                LinearChoice::Retry
            );
        }
    }
}
