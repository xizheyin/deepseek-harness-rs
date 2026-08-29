use std::{collections::VecDeque, fmt, fmt::Write as _};

use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr;

use super::{
    command_palette::{CommandId, CommandPaletteSnapshot},
    composer::Composer,
    file_suggestions::FileSuggestionSnapshot,
    input_memory::PromptQueue,
    motion::{WorkingAge, WorkingPhase, WorkingPresentation},
    presentation::TextStyle,
    theme::ThemePalette,
    view::{DetailDocument, DetailTone},
    visible::{VisibleTextError, render_visible_owned},
};

pub(crate) const MIN_ENHANCED_COLUMNS: u16 = 44;
pub(crate) const MIN_ENHANCED_ROWS: u16 = 12;
pub(crate) const MIN_DOCK_COLUMNS: u16 = 12;
pub(crate) const MIN_DOCK_ROWS: u16 = 5;
const MAX_DOCK_ROWS: usize = 24;
const MAX_DETAIL_WRAPPED_ROWS: usize = 4 * 1024;
const DETAIL_WRAPPED_OMISSION: &str = "[omitted] wrapped row limit";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DockInteraction {
    Idle,
    Running,
    CommandPalette {
        running: bool,
        snapshot: CommandPaletteSnapshot,
    },
    QuestionCustom {
        retry: bool,
    },
    QuestionMulti {
        selected_mask: u8,
        retry: bool,
    },
    Approval(DockApprovalSelection),
    ExactShellApproval(DockApprovalSelection),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DockApprovalSelection {
    AllowOnce,
    Reject,
    AllowExactShellForProcess,
    Cancel,
}

pub(crate) struct DockModel<'a> {
    pub(crate) interaction: DockInteraction,
    pub(crate) composer: &'a Composer,
    pub(crate) queue: &'a PromptQueue,
    pub(crate) notice: Option<&'a str>,
    pub(crate) file_suggestions: FileSuggestionSnapshot<'a>,
    pub(crate) working: WorkingPresentation,
}

impl fmt::Debug for DockModel<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DockModel")
            .field("interaction", &self.interaction)
            .field("composer_bytes", &self.composer.byte_len())
            .field("queued", &self.queue.len())
            .field("notice_bytes", &self.notice.map(str::len))
            .field("file_suggestions", &self.file_suggestions)
            .field("working", &self.working)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DockRole {
    Queue,
    Divider,
    Composer,
    Hint,
    Notice,
    ApprovalChoice,
    ApprovalSelected,
    ApprovalWarning,
    CommandChoice,
    CommandSelected,
    CommandEmpty,
    FileChoice,
    FileSelected,
    FileStatus,
    DetailTitle,
    DetailPlain,
    DetailMuted,
    DetailAccent,
    DetailPositive,
    DetailCaution,
    DetailNegative,
    DetailCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DetailViewport {
    pub(crate) offset: usize,
    pub(crate) total_rows: usize,
    pub(crate) page_rows: usize,
    pub(crate) truncated: bool,
}

struct DockLine {
    role: DockRole,
    text: String,
}

impl fmt::Debug for DockLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DockLine")
            .field("role", &self.role)
            .field("bytes", &self.text.len())
            .finish()
    }
}

pub(crate) struct DockFrame {
    lines: Vec<DockLine>,
    cursor_row: u16,
    cursor_column: u16,
    width: u16,
    terminal_rows: u16,
    output_bottom: u16,
    software_cursor: bool,
}

impl fmt::Debug for DockFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DockFrame")
            .field("rows", &self.lines.len())
            .field("cursor_row", &self.cursor_row)
            .field("cursor_column", &self.cursor_column)
            .field("width", &self.width)
            .field("terminal_rows", &self.terminal_rows)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum DockError {
    #[error("CLI_TERMINAL_TOO_SMALL")]
    TooSmall,
    #[error("CLI_OUTPUT_CAPACITY")]
    Capacity,
    #[error("CLI_OUTPUT_LIMIT")]
    Limit,
    #[error("CLI_OUTPUT_STATE")]
    InvalidState,
}

impl From<VisibleTextError> for DockError {
    fn from(value: VisibleTextError) -> Self {
        match value {
            VisibleTextError::Capacity => Self::Capacity,
            VisibleTextError::Limit => Self::Limit,
        }
    }
}

impl DockFrame {
    pub(crate) fn layout(model: DockModel<'_>, rows: u16, columns: u16) -> Result<Self, DockError> {
        if rows < MIN_DOCK_ROWS || columns < MIN_DOCK_COLUMNS {
            return Err(DockError::TooSmall);
        }
        let width = columns.checked_sub(1).ok_or(DockError::TooSmall)?;
        let width_usize = usize::from(width);
        let compact = rows < MIN_ENHANCED_ROWS || columns < MIN_ENHANCED_COLUMNS;
        // A fixed-height dock makes every redraw and supported resize a
        // replace-in-place operation. Extra history belongs in Inspect, not in
        // an ever-growing input surface.
        let composer_rows = if compact { 1 } else { 4 };
        // Reserve one cell for a software cursor. The enhanced renderer keeps
        // the real cursor on the physical last row so a resize does not anchor
        // reflow in the middle of the transcript.
        let wrapped = wrap_composer(model.composer, width_usize - 3, composer_rows)?;
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(MAX_DOCK_ROWS)
            .map_err(|_| DockError::Capacity)?;

        let software_cursor = !matches!(
            model.interaction,
            DockInteraction::Approval(_) | DockInteraction::ExactShellApproval(_)
        );
        let approval = match model.interaction {
            DockInteraction::Approval(selected) => Some((selected, false)),
            DockInteraction::ExactShellApproval(selected) => Some((selected, true)),
            _ => None,
        };
        let composer_start = if let Some((selected, allow_exact_shell)) = approval {
            lines.push(line(
                DockRole::Notice,
                fit_ascii(
                    if compact {
                        "Not applied"
                    } else {
                        "Approval required | proposed action above"
                    },
                    width_usize,
                ),
            ));
            lines.push(line(DockRole::Divider, "-".repeat(width_usize)));
            let choices = [
                (
                    DockApprovalSelection::AllowOnce,
                    "Allow once | apply exact preview",
                ),
                (DockApprovalSelection::Reject, "Reject | make no change"),
                (
                    DockApprovalSelection::AllowExactShellForProcess,
                    "Allow exact Shell | until dsh exits",
                ),
                (DockApprovalSelection::Cancel, "Stop turn | cancel work"),
            ];
            for (choice, label) in choices {
                if choice == DockApprovalSelection::AllowExactShellForProcess && !allow_exact_shell
                {
                    continue;
                }
                if compact && choice != selected {
                    continue;
                }
                let label = if compact {
                    match choice {
                        DockApprovalSelection::AllowOnce => "Allow once",
                        DockApprovalSelection::Reject => "Reject",
                        DockApprovalSelection::AllowExactShellForProcess => "Allow exact",
                        DockApprovalSelection::Cancel => "Stop turn",
                    }
                } else {
                    label
                };
                let marker = if choice == selected { ">" } else { " " };
                lines.push(line(
                    if choice == selected {
                        DockRole::ApprovalSelected
                    } else {
                        DockRole::ApprovalChoice
                    },
                    fit_ascii(&format!(" {marker} {label}"), width_usize),
                ));
            }
            if !compact {
                lines.push(line(
                    DockRole::ApprovalWarning,
                    fit_ascii("Not sandboxed | Reject is the safe default", width_usize),
                ));
            }
            lines.push(line(
                DockRole::Hint,
                fit_ascii(
                    if compact {
                        "Arrows + Enter"
                    } else {
                        "Arrow keys move | Enter confirms | Esc stops"
                    },
                    width_usize,
                ),
            ));
            0
        } else {
            let palette = match model.interaction {
                DockInteraction::CommandPalette { snapshot, .. } => Some(snapshot),
                DockInteraction::Idle
                | DockInteraction::Running
                | DockInteraction::QuestionCustom { .. }
                | DockInteraction::QuestionMulti { .. }
                | DockInteraction::Approval(_)
                | DockInteraction::ExactShellApproval(_) => None,
            };
            let file_suggestions = model
                .file_suggestions
                .is_visible()
                .then_some(model.file_suggestions);
            if compact && file_suggestions.is_some() {
                lines.push(compact_file_line(
                    file_suggestions.ok_or(DockError::InvalidState)?,
                    width_usize,
                )?);
            } else if compact && palette.is_some() {
                lines.push(compact_command_line(
                    palette.ok_or(DockError::InvalidState)?,
                    width_usize,
                ));
            } else if matches!(
                file_suggestions,
                Some(FileSuggestionSnapshot::Ready { capped: true, .. })
            ) {
                let running = matches!(
                    model.interaction,
                    DockInteraction::Running
                        | DockInteraction::CommandPalette { running: true, .. }
                );
                lines.push(line(
                    DockRole::Queue,
                    fit_ascii(
                        if running {
                            "Working · Workspace files · showing top matches"
                        } else {
                            "Workspace files · showing top matches"
                        },
                        width_usize,
                    ),
                ));
            } else if let DockInteraction::QuestionMulti {
                selected_mask,
                retry,
            } = model.interaction
            {
                lines.push(line(
                    DockRole::Queue,
                    fit_ascii(&multi_select_status(selected_mask, retry), width_usize),
                ));
            } else if let Some(notice) = model.notice {
                let notice = render_visible_owned(notice, false)?;
                lines.push(line(DockRole::Notice, truncate_cells(&notice, width_usize)));
            } else if model.queue.len() != 0 {
                let queue = if columns < 60 {
                    format!("next: {} queued | Up edits newest", model.queue.len())
                } else {
                    format!(
                        "Next turn queued | {} item{} | {} KiB | Up edits newest",
                        model.queue.len(),
                        if model.queue.len() == 1 { "" } else { "s" },
                        model.queue.total_bytes().div_ceil(1024),
                    )
                };
                lines.push(line(DockRole::Queue, fit_ascii(&queue, width_usize)));
            } else {
                let status = match model.interaction {
                    DockInteraction::Idle => "Ready".to_owned(),
                    DockInteraction::Running => working_status(model.working),
                    DockInteraction::CommandPalette { running: false, .. } => "Ready".to_owned(),
                    DockInteraction::CommandPalette { running: true, .. } => {
                        working_status(model.working)
                    }
                    DockInteraction::QuestionCustom { retry: false } => {
                        "Type a custom answer".to_owned()
                    }
                    DockInteraction::QuestionCustom { retry: true } => {
                        "Answer required · 4 KiB maximum".to_owned()
                    }
                    DockInteraction::QuestionMulti {
                        selected_mask,
                        retry,
                    } => multi_select_status(selected_mask, retry),
                    DockInteraction::Approval(_) | DockInteraction::ExactShellApproval(_) => {
                        "Approval required".to_owned()
                    }
                };
                lines.push(line(DockRole::Queue, fit_ascii(&status, width_usize)));
            }
            lines.push(line(DockRole::Divider, "-".repeat(width_usize)));
            let composer_start = lines.len();
            for (index, content) in wrapped.rows.into_iter().enumerate() {
                let prefix = if index == 0 && !wrapped.hidden_above {
                    "❯ "
                } else if index == 0 {
                    "^ "
                } else {
                    "  "
                };
                let mut text = String::new();
                text.try_reserve_exact(prefix.len() + content.len())
                    .map_err(|_| DockError::Capacity)?;
                text.push_str(prefix);
                text.push_str(&content);
                lines.push(line(DockRole::Composer, text));
            }
            while lines.len() < composer_start + composer_rows {
                lines.push(line(DockRole::Composer, "  ".to_owned()));
            }
            if !compact {
                if let Some(file_suggestions) = file_suggestions {
                    push_file_lines(&mut lines, file_suggestions, rows, width_usize)?;
                } else if let Some(palette) = palette {
                    push_command_lines(&mut lines, palette, rows, columns, width_usize)?;
                }
            }
            let hint = if file_suggestions.is_some() {
                if compact {
                    "Enter · Esc"
                } else if matches!(file_suggestions, Some(FileSuggestionSnapshot::Ready { .. })) {
                    "Enter complete · Esc close"
                } else {
                    "Enter send · Esc close"
                }
            } else {
                match model.interaction {
                    DockInteraction::Idle if compact => "Enter send",
                    DockInteraction::Idle => "Enter send | Ctrl+J newline | Up history | ? help",
                    DockInteraction::Running if compact => "Enter queue",
                    DockInteraction::Running => {
                        "Enter queue | Ctrl+J newline | Ctrl+C stop current turn"
                    }
                    DockInteraction::CommandPalette { .. } if compact => "Enter · Esc",
                    DockInteraction::CommandPalette { .. } => "Enter complete · Esc close",
                    DockInteraction::QuestionCustom { .. } if compact => "Enter · Esc",
                    DockInteraction::QuestionCustom { .. } => {
                        "Enter answer | Ctrl+J newline | Ctrl+P/N pages | Ctrl+S skip | Esc cancels question"
                    }
                    DockInteraction::QuestionMulti { .. } if compact => "Digits · Enter · Esc",
                    DockInteraction::QuestionMulti { .. } => {
                        "Number toggles | Enter submits | [/] pages | s skips | Esc cancels"
                    }
                    DockInteraction::Approval(_) | DockInteraction::ExactShellApproval(_) => {
                        "Arrow keys move | Enter confirms | Esc stops"
                    }
                }
            };
            let mut hint = if wrapped.hidden_below {
                format!("v more | {hint}")
            } else {
                hint.to_owned()
            };
            if model.notice.is_some() && model.queue.len() != 0 {
                hint = format!("next: {} queued | {hint}", model.queue.len());
            }
            lines.push(line(DockRole::Hint, fit_ascii(&hint, width_usize)));
            composer_start
        };
        if lines.len() > MAX_DOCK_ROWS || lines.len() >= usize::from(rows) {
            return Err(DockError::Limit);
        }
        let dock_rows = u16::try_from(lines.len()).map_err(|_| DockError::Limit)?;
        let output_bottom = rows.checked_sub(dock_rows).ok_or(DockError::TooSmall)?;
        if output_bottom == 0 {
            return Err(DockError::TooSmall);
        }
        let cursor_row = composer_start
            .checked_add(wrapped.cursor_row)
            .and_then(|row| u16::try_from(row).ok())
            .ok_or(DockError::Limit)?;
        let cursor_column = wrapped
            .cursor_column
            .checked_add(2)
            .and_then(|column| u16::try_from(column).ok())
            .ok_or(DockError::Limit)?;
        if cursor_column > width {
            return Err(DockError::InvalidState);
        }
        Ok(Self {
            lines,
            cursor_row,
            cursor_column,
            width,
            terminal_rows: rows,
            output_bottom,
            software_cursor,
        })
    }

    pub(crate) fn layout_detail(
        document: &DetailDocument,
        requested_offset: usize,
        rows: u16,
        columns: u16,
    ) -> Result<(Self, DetailViewport), DockError> {
        if rows < MIN_DOCK_ROWS || columns < MIN_DOCK_COLUMNS {
            return Err(DockError::TooSmall);
        }
        let width = columns.checked_sub(1).ok_or(DockError::TooSmall)?;
        let width_usize = usize::from(width);
        let panel_rows = usize::from(rows.saturating_sub(1)).min(MAX_DOCK_ROWS);
        if panel_rows < 4 {
            return Err(DockError::TooSmall);
        }
        let has_divider = panel_rows >= 7;
        let fixed_rows = 2 + usize::from(has_divider);
        let page_rows = panel_rows
            .checked_sub(fixed_rows)
            .filter(|rows| *rows != 0)
            .ok_or(DockError::TooSmall)?;
        let mut physical = Vec::new();
        physical
            .try_reserve(MAX_DETAIL_WRAPPED_ROWS.min(document.lines().len().saturating_mul(2)))
            .map_err(|_| DockError::Capacity)?;
        let omission_line = line(DockRole::DetailCaution, DETAIL_WRAPPED_OMISSION.to_owned());
        let mut truncated = false;
        for source in document.lines() {
            if wrap_detail_line(source.tone(), source.text(), width_usize, &mut physical)? {
                truncated = true;
                break;
            }
        }
        if truncated {
            let last = physical.last_mut().ok_or(DockError::InvalidState)?;
            *last = omission_line;
        }
        if physical.is_empty() {
            physical.push(line(
                DockRole::DetailMuted,
                "No retained details.".to_owned(),
            ));
        }
        let total_rows = physical.len();
        let offset = requested_offset.min(total_rows.saturating_sub(page_rows));
        let mut lines = Vec::new();
        lines
            .try_reserve_exact(panel_rows)
            .map_err(|_| DockError::Capacity)?;
        lines.push(line(
            DockRole::DetailTitle,
            fit_ascii(document.title(), width_usize),
        ));
        if has_divider {
            lines.push(line(DockRole::Divider, "-".repeat(width_usize)));
        }
        for source in physical.iter().skip(offset).take(page_rows) {
            lines.push(line(source.role, truncate_cells(&source.text, width_usize)));
        }
        while lines.len() < panel_rows - 1 {
            lines.push(line(DockRole::DetailPlain, String::new()));
        }
        let total_label = if truncated {
            format!("{total_rows}+")
        } else {
            total_rows.to_string()
        };
        let position = if total_rows <= page_rows && !truncated {
            format!("all {total_label} rows")
        } else {
            format!(
                "rows {}-{} / {total_label}",
                offset + 1,
                (offset + page_rows).min(total_rows)
            )
        };
        let footer = if columns >= 80 {
            format!("{position} · Up/Down/PgUp/PgDn scroll · Tab switch · Esc Focus")
        } else {
            format!("{position} · PgUp/Dn · Esc Focus")
        };
        lines.push(line(DockRole::Hint, fit_ascii(&footer, width_usize)));
        if lines.len() != panel_rows {
            return Err(DockError::InvalidState);
        }
        let dock_rows = u16::try_from(lines.len()).map_err(|_| DockError::Limit)?;
        let output_bottom = rows.checked_sub(dock_rows).ok_or(DockError::TooSmall)?;
        if output_bottom == 0 {
            return Err(DockError::TooSmall);
        }
        Ok((
            Self {
                lines,
                cursor_row: 0,
                cursor_column: 1,
                width,
                terminal_rows: rows,
                output_bottom,
                software_cursor: false,
            },
            DetailViewport {
                offset,
                total_rows,
                page_rows,
                truncated,
            },
        ))
    }

    pub(crate) fn rows(&self) -> Result<u16, DockError> {
        u16::try_from(self.lines.len()).map_err(|_| DockError::Limit)
    }

    pub(crate) const fn terminal_rows(&self) -> u16 {
        self.terminal_rows
    }

    pub(crate) const fn terminal_columns(&self) -> u16 {
        self.width + 1
    }

    pub(crate) const fn output_bottom(&self) -> u16 {
        self.output_bottom
    }

    /// Writes only the owned bottom rows. Coordinate ownership remains with
    /// `InlineScreen`; this pure layout object never establishes a scrolling
    /// region or decides how transcript output moves.
    pub(crate) fn render_bottom(
        &self,
        output: &mut String,
        theme: ThemePalette,
    ) -> Result<(), DockError> {
        let start_row = self.output_bottom.checked_add(1).ok_or(DockError::Limit)?;
        output.push_str("\x1b[?25l");
        push_absolute_frame_lines(
            output,
            &self.lines,
            start_row,
            theme,
            self.software_cursor
                .then_some((self.cursor_row, self.cursor_column)),
        )?;
        push_cup(output, self.terminal_rows, 1);
        output.push_str("\x1b[0m");
        Ok(())
    }

    pub(crate) fn clear_bottom(&self, output: &mut String) -> Result<(), DockError> {
        let start = self.output_bottom.checked_add(1).ok_or(DockError::Limit)?;
        for row in start..=self.terminal_rows {
            push_cup(output, row, 1);
            output.push_str("\x1b[2K");
        }
        Ok(())
    }
}

fn multi_select_status(mask: u8, retry: bool) -> String {
    if mask == 0 {
        return if retry {
            "Choose at least one option".to_owned()
        } else {
            "No options selected".to_owned()
        };
    }
    let mut selected = String::from("Selected options · ");
    for index in 0..4_u8 {
        if mask & (1 << index) == 0 {
            continue;
        }
        if !selected.ends_with(' ') {
            selected.push(',');
        }
        selected.push(char::from(b'1' + index));
    }
    selected
}

fn working_status(presentation: WorkingPresentation) -> String {
    let prefix = match presentation.phase {
        WorkingPhase::Plain => "",
        WorkingPhase::Static => "● ",
        WorkingPhase::Animated(_) => "●  ",
    };
    let phase = presentation.phase_glyph();
    let label = if matches!(presentation.age, WorkingAge::Long { .. }) {
        "Still working"
    } else {
        "Working"
    };
    let age = match (presentation.phase, presentation.age) {
        (WorkingPhase::Static, WorkingAge::OneSecond { .. }) => Some("1s+".to_owned()),
        (WorkingPhase::Static, WorkingAge::Long { .. }) => None,
        (_, WorkingAge::Fresh) => None,
        (_, WorkingAge::OneSecond { seconds } | WorkingAge::Long { seconds }) => {
            Some(format!("{seconds}s"))
        }
    };
    let mut status = String::new();
    status.push_str(prefix);
    if let Some(phase) = phase {
        // Replace the reserved second cell without changing the stable semantic
        // icon or the remaining text columns.
        status.pop();
        status.push(phase);
        status.push(' ');
    }
    status.push_str(label);
    if let Some(age) = age {
        status.push_str(" · ");
        status.push_str(&age);
    }
    status.push_str(" | type the next prompt while dsh runs");
    status
}

fn push_absolute_frame_lines(
    output: &mut String,
    lines: &[DockLine],
    start_row: u16,
    theme: ThemePalette,
    software_cursor: Option<(u16, u16)>,
) -> Result<(), DockError> {
    for (index, line) in lines.iter().enumerate() {
        let row = start_row
            .checked_add(u16::try_from(index).map_err(|_| DockError::Limit)?)
            .ok_or(DockError::Limit)?;
        push_cup(output, row, 1);
        output.push_str("\x1b[2K");
        let cursor_column = software_cursor
            .filter(|(cursor_row, _)| usize::from(*cursor_row) == index)
            .map(|(_, cursor_column)| cursor_column);
        push_line(output, line, theme, cursor_column)?;
    }
    Ok(())
}

fn push_cup(output: &mut String, row: u16, column: u16) {
    write!(output, "\x1b[{row};{column}H")
        .expect("writing a bounded cursor-position command cannot fail");
}

fn line(role: DockRole, text: String) -> DockLine {
    DockLine { role, text }
}

fn compact_command_line(snapshot: CommandPaletteSnapshot, width: usize) -> DockLine {
    let Some(command) = snapshot.selected() else {
        return line(DockRole::CommandEmpty, fit_ascii("! No match", width));
    };
    line(
        DockRole::CommandSelected,
        fit_ascii(&format!(" > {}", command.command()), width),
    )
}

fn compact_file_line(
    snapshot: FileSuggestionSnapshot<'_>,
    width: usize,
) -> Result<DockLine, DockError> {
    match snapshot {
        FileSuggestionSnapshot::Ready {
            candidates,
            selected,
            ..
        } => file_line(
            candidates.get(selected).ok_or(DockError::InvalidState)?,
            true,
            width,
        ),
        FileSuggestionSnapshot::Loading => {
            Ok(line(DockRole::FileStatus, fit_ascii("… Scan", width)))
        }
        FileSuggestionSnapshot::Empty => {
            Ok(line(DockRole::FileStatus, fit_ascii("! No match", width)))
        }
        FileSuggestionSnapshot::Unavailable => {
            Ok(line(DockRole::FileStatus, fit_ascii("! Offline", width)))
        }
        FileSuggestionSnapshot::Hidden => Err(DockError::InvalidState),
    }
}

fn push_file_lines(
    lines: &mut Vec<DockLine>,
    snapshot: FileSuggestionSnapshot<'_>,
    terminal_rows: u16,
    width: usize,
) -> Result<(), DockError> {
    match snapshot {
        FileSuggestionSnapshot::Ready {
            candidates,
            selected,
            ..
        } => {
            if selected >= candidates.len() || candidates.is_empty() {
                return Err(DockError::InvalidState);
            }
            let visible = candidates
                .len()
                .min(12)
                .min(usize::from(terminal_rows).saturating_sub(8));
            if visible == 0 {
                return Err(DockError::TooSmall);
            }
            let start = selected
                .saturating_sub((visible - 1) / 2)
                .min(candidates.len() - visible);
            for (index, path) in candidates.iter().enumerate().skip(start).take(visible) {
                lines.push(file_line(path, index == selected, width)?);
            }
        }
        FileSuggestionSnapshot::Loading => lines.push(line(
            DockRole::FileStatus,
            fit_ascii(" … Scanning workspace...", width),
        )),
        FileSuggestionSnapshot::Empty => lines.push(line(
            DockRole::FileStatus,
            fit_ascii(" ! No matching workspace file", width),
        )),
        FileSuggestionSnapshot::Unavailable => lines.push(line(
            DockRole::FileStatus,
            fit_ascii(" ! Workspace files unavailable", width),
        )),
        FileSuggestionSnapshot::Hidden => return Err(DockError::InvalidState),
    }
    Ok(())
}

fn file_line(path: &str, selected: bool, width: usize) -> Result<DockLine, DockError> {
    let visible = render_visible_owned(path, false)?;
    let marker = if selected { '>' } else { ' ' };
    let text = truncate_cells(&format!(" {marker} @{visible}"), width);
    Ok(line(
        if selected {
            DockRole::FileSelected
        } else {
            DockRole::FileChoice
        },
        text,
    ))
}

fn push_command_lines(
    lines: &mut Vec<DockLine>,
    snapshot: CommandPaletteSnapshot,
    terminal_rows: u16,
    columns: u16,
    width: usize,
) -> Result<(), DockError> {
    let count = snapshot.count();
    if count == 0 {
        lines.push(line(
            DockRole::CommandEmpty,
            truncate_cells(" ! No matching local command", width),
        ));
        return Ok(());
    }
    let width_cap = if columns < 60 { 3 } else { 9 };
    let visible = count
        .min(width_cap)
        .min(usize::from(terminal_rows).saturating_sub(8));
    if visible == 0 {
        return Err(DockError::TooSmall);
    }
    let selected = snapshot.selected().ok_or(DockError::InvalidState)?;
    let selected_index = (0..count)
        .find(|index| snapshot.command_at(*index) == Some(selected))
        .ok_or(DockError::InvalidState)?;
    let start = selected_index
        .saturating_sub((visible - 1) / 2)
        .min(count - visible);
    for index in start..start + visible {
        let command = snapshot.command_at(index).ok_or(DockError::InvalidState)?;
        lines.push(command_line(command, command == selected, width));
    }
    Ok(())
}

fn command_line(command: CommandId, selected: bool, width: usize) -> DockLine {
    let marker = if selected { '>' } else { ' ' };
    let prefix = format!(" {marker} {}", command.command());
    let mut text = prefix.clone();
    let suffix = format!(" | {}", command.description());
    if text.len().saturating_add(suffix.len()) <= width {
        text.push_str(&suffix);
    } else if width > prefix.len() + 3 {
        text.push_str(" | ");
        text.push_str(command.description());
        text = truncate_cells(&text, width);
    }
    line(
        if selected {
            DockRole::CommandSelected
        } else {
            DockRole::CommandChoice
        },
        text,
    )
}

struct WrappedComposer {
    rows: VecDeque<String>,
    cursor_row: usize,
    cursor_column: usize,
    hidden_above: bool,
    hidden_below: bool,
}

fn wrap_composer(
    composer: &Composer,
    width: usize,
    max_rows: usize,
) -> Result<WrappedComposer, DockError> {
    if width == 0 || max_rows == 0 {
        return Err(DockError::TooSmall);
    }
    let before = render_visible_owned(&composer.text()[..composer.cursor()], true)?;
    let after = render_visible_owned(&composer.text()[composer.cursor()..], true)?;
    let mut rows = VecDeque::new();
    rows.try_reserve(max_rows)
        .map_err(|_| DockError::Capacity)?;
    rows.push_back(String::new());
    let mut hidden_above = false;
    push_visible_segment(&before, width, max_rows, &mut rows, &mut hidden_above, true)?;
    let cursor_row = rows.len() - 1;
    let cursor_column = rows
        .back()
        .map_or(0, |row| UnicodeWidthStr::width(row.as_str()));
    let mut hidden_below = false;
    push_visible_segment(&after, width, max_rows, &mut rows, &mut hidden_below, false)?;
    Ok(WrappedComposer {
        rows,
        cursor_row,
        cursor_column,
        hidden_above,
        hidden_below,
    })
}

fn push_visible_segment(
    segment: &str,
    width: usize,
    max_rows: usize,
    rows: &mut VecDeque<String>,
    hidden: &mut bool,
    may_discard_front: bool,
) -> Result<(), DockError> {
    for grapheme in segment.graphemes(true) {
        if grapheme == "\n" {
            if !start_row(rows, max_rows, hidden, may_discard_front)? {
                return Ok(());
            }
            continue;
        }
        let cells = UnicodeWidthStr::width(grapheme);
        if cells > width {
            let placeholder = fit_ascii("[wide grapheme]", width);
            let current = rows
                .back()
                .map_or(0, |row| UnicodeWidthStr::width(row.as_str()));
            let placeholder_cells = UnicodeWidthStr::width(placeholder.as_str());
            if current != 0
                && current + placeholder_cells > width
                && !start_row(rows, max_rows, hidden, may_discard_front)?
            {
                return Ok(());
            }
            let row = rows.back_mut().ok_or(DockError::InvalidState)?;
            row.try_reserve(placeholder.len())
                .map_err(|_| DockError::Capacity)?;
            row.push_str(&placeholder);
            continue;
        }
        let current = rows
            .back()
            .map_or(0, |row| UnicodeWidthStr::width(row.as_str()));
        if current != 0
            && current + cells > width
            && !start_row(rows, max_rows, hidden, may_discard_front)?
        {
            return Ok(());
        }
        let row = rows.back_mut().ok_or(DockError::InvalidState)?;
        row.try_reserve(grapheme.len())
            .map_err(|_| DockError::Capacity)?;
        row.push_str(grapheme);
    }
    Ok(())
}

fn start_row(
    rows: &mut VecDeque<String>,
    max_rows: usize,
    hidden: &mut bool,
    may_discard_front: bool,
) -> Result<bool, DockError> {
    if rows.len() == max_rows {
        *hidden = true;
        if !may_discard_front {
            return Ok(false);
        }
        let _ = rows.pop_front();
    }
    rows.try_reserve(1).map_err(|_| DockError::Capacity)?;
    rows.push_back(String::new());
    Ok(true)
}

fn truncate_cells(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut cells = 0_usize;
    for grapheme in text.graphemes(true) {
        let next = cells + UnicodeWidthStr::width(grapheme);
        if next > width {
            break;
        }
        output.push_str(grapheme);
        cells = next;
    }
    output
}

fn fit_ascii(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        text.to_owned()
    } else if width > 3 {
        let mut shortened = truncate_cells(text, width - 3);
        shortened.push_str("...");
        shortened
    } else {
        String::new()
    }
}

fn wrap_detail_line(
    tone: DetailTone,
    text: &str,
    width: usize,
    output: &mut Vec<DockLine>,
) -> Result<bool, DockError> {
    if width == 0 {
        return Err(DockError::TooSmall);
    }
    let role = detail_role(tone);
    let mut current = String::new();
    let mut cells = 0_usize;
    for grapheme in text.graphemes(true) {
        let grapheme_cells = UnicodeWidthStr::width(grapheme);
        let display = if grapheme_cells > width || grapheme.len() > 1024 {
            "[wide grapheme]"
        } else {
            grapheme
        };
        let display_cells = UnicodeWidthStr::width(display);
        if cells != 0 && cells.saturating_add(display_cells) > width {
            if push_detail_physical(output, role, std::mem::take(&mut current))? {
                return Ok(true);
            }
            cells = 0;
        }
        current
            .try_reserve(display.len())
            .map_err(|_| DockError::Capacity)?;
        current.push_str(display);
        cells = cells.saturating_add(display_cells);
    }
    push_detail_physical(output, role, current)
}

fn push_detail_physical(
    output: &mut Vec<DockLine>,
    role: DockRole,
    text: String,
) -> Result<bool, DockError> {
    if output.len() == MAX_DETAIL_WRAPPED_ROWS {
        return Ok(true);
    }
    output.try_reserve(1).map_err(|_| DockError::Capacity)?;
    output.push(line(role, text));
    Ok(false)
}

const fn detail_role(tone: DetailTone) -> DockRole {
    match tone {
        DetailTone::Plain => DockRole::DetailPlain,
        DetailTone::Muted => DockRole::DetailMuted,
        DetailTone::Accent => DockRole::DetailAccent,
        DetailTone::Positive => DockRole::DetailPositive,
        DetailTone::Caution => DockRole::DetailCaution,
        DetailTone::Negative => DockRole::DetailNegative,
        DetailTone::Code => DockRole::DetailCode,
    }
}

fn push_line(
    output: &mut String,
    line: &DockLine,
    theme: ThemePalette,
    cursor_column: Option<u16>,
) -> Result<(), DockError> {
    let style = style_for_role(line.role);
    push_absolute_style(output, theme, style);
    output
        .try_reserve(line.text.len() + 4)
        .map_err(|_| DockError::Capacity)?;
    if let Some(column) = cursor_column {
        let split = byte_at_cell(&line.text, usize::from(column)).ok_or(DockError::InvalidState)?;
        output.push_str(&line.text[..split]);
        push_absolute_style(output, theme, TextStyle::Selection);
        output.push(' ');
        push_absolute_style(output, theme, style);
        output.push_str(&line.text[split..]);
    } else {
        output.push_str(&line.text);
    }
    output.push_str("\x1b[0m");
    Ok(())
}

const fn style_for_role(role: DockRole) -> TextStyle {
    match role {
        DockRole::Queue | DockRole::Hint | DockRole::DetailMuted => TextStyle::Muted,
        DockRole::Divider => TextStyle::Border,
        DockRole::Composer | DockRole::DetailPlain => TextStyle::Plain,
        DockRole::Notice | DockRole::ApprovalWarning | DockRole::DetailCaution => {
            TextStyle::Warning
        }
        DockRole::ApprovalChoice | DockRole::CommandChoice | DockRole::FileChoice => {
            TextStyle::Code
        }
        DockRole::ApprovalSelected | DockRole::CommandSelected | DockRole::FileSelected => {
            TextStyle::Selection
        }
        DockRole::CommandEmpty | DockRole::FileStatus => TextStyle::Warning,
        DockRole::DetailTitle => TextStyle::Heading,
        DockRole::DetailAccent => TextStyle::Accent,
        DockRole::DetailPositive => TextStyle::Success,
        DockRole::DetailNegative => TextStyle::Error,
        DockRole::DetailCode => TextStyle::Code,
    }
}

fn push_absolute_style(output: &mut String, theme: ThemePalette, style: TextStyle) {
    output.push_str("\x1b[0m");
    if style != TextStyle::Plain {
        output.push_str(theme.sgr(style));
    }
}

fn byte_at_cell(text: &str, target: usize) -> Option<usize> {
    let mut cells = 0_usize;
    for (byte, grapheme) in text.grapheme_indices(true) {
        if cells == target {
            return Some(byte);
        }
        cells = cells.checked_add(UnicodeWidthStr::width(grapheme))?;
        if cells > target {
            return None;
        }
    }
    (cells == target).then_some(text.len())
}

#[cfg(test)]
mod tests {
    use super::{
        DETAIL_WRAPPED_OMISSION, DockApprovalSelection, DockFrame, DockInteraction, DockModel,
        DockRole, MAX_DETAIL_WRAPPED_ROWS, working_status,
    };
    use crate::tui::{
        command_palette::{CommandPaletteState, PaletteMove},
        composer::Composer,
        file_suggestions::FileSuggestionSnapshot,
        input_memory::PromptQueue,
        motion::WorkingPresentation,
        presentation::TextStyle,
        theme::ThemePalette,
        view::{DetailDocument, DetailTone, ViewMode},
    };

    fn without_sgr(bytes: &str) -> Vec<u8> {
        let bytes = bytes.as_bytes();
        let mut output = Vec::new();
        let mut index = 0_usize;
        while index < bytes.len() {
            if index + 1 < bytes.len() && bytes[index] == 0x1b && bytes[index + 1] == b'[' {
                let mut end = index + 2;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                if end < bytes.len() && bytes[end] == b'm' {
                    index = end + 1;
                    continue;
                }
            }
            output.push(bytes[index]);
            index += 1;
        }
        output
    }

    #[test]
    fn working_motion_keeps_one_semantic_icon_and_reduced_milestones_are_static() {
        let hint = " | type the next prompt while dsh runs";
        assert_eq!(
            working_status(WorkingPresentation::PLAIN),
            format!("Working{hint}")
        );
        assert_eq!(
            working_status(WorkingPresentation {
                phase: crate::tui::motion::WorkingPhase::Animated(3),
                age: crate::tui::motion::WorkingAge::OneSecond { seconds: 4 },
            }),
            format!("● \\ Working · 4s{hint}")
        );
        assert_eq!(
            working_status(WorkingPresentation {
                phase: crate::tui::motion::WorkingPhase::Static,
                age: crate::tui::motion::WorkingAge::OneSecond { seconds: 1 },
            }),
            format!("● Working · 1s+{hint}")
        );
        assert_eq!(
            working_status(WorkingPresentation {
                phase: crate::tui::motion::WorkingPhase::Static,
                age: crate::tui::motion::WorkingAge::Long { seconds: 5 },
            }),
            format!("● Still working{hint}")
        );
    }

    #[test]
    fn responsive_frames_fit_44_80_and_112_columns() {
        for (rows, columns) in [(20, 44), (24, 80), (34, 112)] {
            let mut composer = Composer::default();
            composer
                .insert_text("修复 timeout，保留 e\u{301} 和 SECRET\u{202e}SAFE")
                .unwrap();
            let queue = PromptQueue::default();
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Idle,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                rows,
                columns,
            )
            .unwrap();
            for line in &frame.lines {
                assert!(
                    unicode_width::UnicodeWidthStr::width(line.text.as_str())
                        <= usize::from(columns - 1)
                );
                assert!(!line.text.contains('\u{202e}'));
            }
            assert!(frame.cursor_column < columns);
        }
    }

    #[test]
    fn command_palette_has_deterministic_windows_and_compact_rescue_geometry() {
        let mut composer = Composer::default();
        composer.insert_text("/").unwrap();
        let queue = PromptQueue::default();
        let mut palette = CommandPaletteState::default();
        for _ in 0..6 {
            assert!(palette.navigate(&composer, PaletteMove::Next));
        }
        let snapshot = palette.snapshot(&composer);

        for (rows, columns, expected) in [
            (
                24,
                80,
                vec![
                    "/review",
                    "/focus",
                    "/theme",
                    "/motion",
                    "/exit",
                    "/quit",
                    "/goal",
                    "/model",
                    "/permission",
                ],
            ),
            (
                15,
                80,
                vec![
                    "/focus", "/theme", "/motion", "/exit", "/quit", "/goal", "/model",
                ],
            ),
            (12, 80, vec!["/motion", "/exit", "/quit", "/goal"]),
            (12, 44, vec!["/motion", "/exit", "/quit"]),
        ] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::CommandPalette {
                        running: false,
                        snapshot,
                    },
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                rows,
                columns,
            )
            .unwrap();
            let commands = frame
                .lines
                .iter()
                .filter(|line| {
                    matches!(
                        line.role,
                        DockRole::CommandChoice | DockRole::CommandSelected
                    )
                })
                .map(|line| {
                    crate::tui::command_palette::CommandId::ALL
                        .iter()
                        .map(|command| command.command())
                        .find(|command| line.text.contains(command))
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(commands, expected, "{columns}x{rows}");
            assert!(frame.lines.iter().any(|line| line.text.contains("> /exit")));
            assert!(frame.output_bottom() >= 1);
        }

        let compact = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::CommandPalette {
                    running: true,
                    snapshot,
                },
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            5,
            12,
        )
        .unwrap();
        assert_eq!(compact.rows().unwrap(), 4);
        assert_eq!(compact.output_bottom(), 1);
        assert_eq!(compact.lines[0].text, " > /exit");
        assert_eq!(compact.lines[3].text, "Enter · Esc");

        let mut unknown = Composer::default();
        unknown.insert_text("/unknown").unwrap();
        let empty = palette.sync(&unknown);
        let compact_empty = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::CommandPalette {
                    running: false,
                    snapshot: empty,
                },
                composer: &unknown,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            5,
            12,
        )
        .unwrap();
        assert_eq!(compact_empty.lines[0].text, "! No match");
        assert!(!compact_empty.lines[0].text.contains('>'));
    }

    #[test]
    fn file_suggestions_have_exact_windows_compact_rescue_and_safe_display() {
        let mut composer = Composer::default();
        composer.insert_text("see @src").unwrap();
        let queue = PromptQueue::default();
        let mut candidates = (0..20)
            .map(|index| format!("src/module-{index:02}.rs"))
            .collect::<Vec<_>>();
        candidates[10] = "src/SECRET\u{1b}[2J.rs".to_owned();
        let snapshot = FileSuggestionSnapshot::Ready {
            candidates: &candidates,
            selected: 10,
            capped: true,
        };

        for (rows, columns, expected_rows) in [(34, 112, 12_usize), (24, 80, 12), (12, 44, 4)] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Idle,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: snapshot,
                    working: WorkingPresentation::PLAIN,
                },
                rows,
                columns,
            )
            .unwrap();
            let file_rows = frame
                .lines
                .iter()
                .filter(|line| matches!(line.role, DockRole::FileChoice | DockRole::FileSelected))
                .count();
            assert_eq!(file_rows, expected_rows, "{columns}x{rows}");
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains("top matches"))
            );
            assert!(frame.lines.iter().any(|line| line.text.contains("> @src/")));
            assert!(!frame.lines.iter().any(|line| line.text.contains('\u{1b}')));
            assert_eq!(candidates[10], "src/SECRET\u{1b}[2J.rs");
        }

        let compact = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::Running,
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: snapshot,
                working: WorkingPresentation::PLAIN,
            },
            5,
            12,
        )
        .unwrap();
        assert_eq!(compact.rows().unwrap(), 4);
        assert_eq!(compact.output_bottom(), 1);
        assert!(compact.lines[0].text.starts_with(" > @"));
        assert_eq!(compact.lines[3].text, "Enter · Esc");
        assert!(!compact.lines.iter().any(|line| line.text.contains("top")));

        for (snapshot, expected) in [
            (FileSuggestionSnapshot::Loading, "Scan"),
            (FileSuggestionSnapshot::Empty, "No match"),
            (FileSuggestionSnapshot::Unavailable, "Offline"),
        ] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Idle,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: snapshot,
                    working: WorkingPresentation::PLAIN,
                },
                5,
                12,
            )
            .unwrap();
            assert!(frame.lines[0].text.contains(expected));
            assert!(!frame.lines[0].text.contains('>'));
        }
    }

    #[test]
    fn compact_rescue_frames_keep_input_and_approval_visible_at_15_by_6() {
        let mut composer = Composer::default();
        composer.insert_text("draft").unwrap();
        let queue = PromptQueue::default();
        for interaction in [
            DockInteraction::Running,
            DockInteraction::Approval(DockApprovalSelection::Reject),
        ] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                6,
                15,
            )
            .unwrap();
            assert_eq!(frame.rows().unwrap(), 4);
            assert_eq!(frame.output_bottom(), 2);
            if matches!(
                interaction,
                DockInteraction::Approval(_) | DockInteraction::ExactShellApproval(_)
            ) {
                assert_eq!(frame.lines[0].text, "Not applied");
                assert!(
                    frame
                        .lines
                        .iter()
                        .any(|line| line.text.contains("> Reject"))
                );
            }
            assert!(
                frame.lines.iter().all(|line| {
                    unicode_width::UnicodeWidthStr::width(line.text.as_str()) <= 14
                })
            );
        }
    }

    #[test]
    fn compact_rescue_has_exact_twelve_by_five_boundaries() {
        let mut composer = Composer::default();
        composer.insert_text("x").unwrap();
        let queue = PromptQueue::default();
        let frame = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::Approval(DockApprovalSelection::Reject),
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            5,
            12,
        )
        .unwrap();
        assert_eq!(frame.rows().unwrap(), 4);
        assert_eq!(frame.output_bottom(), 1);
        assert_eq!(frame.lines[0].text, "Not applied");
        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.text.contains("> Reject"))
        );
        assert!(
            DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Idle,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                5,
                11,
            )
            .is_err()
        );
        assert!(
            DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Idle,
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                4,
                12,
            )
            .is_err()
        );
    }

    #[test]
    fn full_approval_surface_keeps_status_choices_and_safety_text_at_supported_widths() {
        let composer = Composer::default();
        let queue = PromptQueue::default();
        for (rows, columns) in [(20, 44), (24, 80), (34, 112)] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Approval(DockApprovalSelection::Reject),
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                rows,
                columns,
            )
            .unwrap();
            assert_eq!(frame.lines.len(), 7);
            assert!(frame.lines[0].text.contains("Approval required"));
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains("Allow once"))
            );
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains("> Reject"))
            );
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains("Stop turn"))
            );
            let mut rendered = String::new();
            frame
                .render_bottom(&mut rendered, ThemePalette::Adaptive)
                .unwrap();
            assert_eq!(
                rendered
                    .matches(ThemePalette::Adaptive.sgr(TextStyle::Selection))
                    .count(),
                1,
                "only the selected approval choice may use selection styling"
            );
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains("Not sandboxed"))
            );
            assert!(frame.lines.iter().all(|line| {
                unicode_width::UnicodeWidthStr::width(line.text.as_str())
                    <= usize::from(columns - 1)
            }));
        }
    }

    #[test]
    fn exact_shell_approval_adds_one_explicit_process_choice_only() {
        let composer = Composer::default();
        let queue = PromptQueue::default();
        let exact = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::ExactShellApproval(
                    DockApprovalSelection::AllowExactShellForProcess,
                ),
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            24,
            80,
        )
        .unwrap();
        assert_eq!(exact.lines.len(), 8);
        assert!(
            exact
                .lines
                .iter()
                .any(|line| line.text.contains("> Allow exact Shell"))
        );
        assert!(
            exact
                .lines
                .iter()
                .any(|line| line.text.contains("until dsh exits"))
        );

        let ordinary = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::Approval(DockApprovalSelection::Reject),
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            24,
            80,
        )
        .unwrap();
        assert!(
            !ordinary
                .lines
                .iter()
                .any(|line| line.text.contains("exact Shell"))
        );
    }

    #[test]
    fn every_palette_preserves_the_same_compact_44_80_112_geometry_and_text() {
        let composer = Composer::default();
        let queue = PromptQueue::default();
        for (rows, columns) in [(5, 12), (20, 44), (24, 80), (34, 112)] {
            let frame = DockFrame::layout(
                DockModel {
                    interaction: DockInteraction::Approval(DockApprovalSelection::Reject),
                    composer: &composer,
                    queue: &queue,
                    notice: None,
                    file_suggestions: FileSuggestionSnapshot::Hidden,
                    working: WorkingPresentation::PLAIN,
                },
                rows,
                columns,
            )
            .unwrap();
            let mut expected = None;
            for palette in ThemePalette::ALL {
                let mut bytes = String::new();
                frame.render_bottom(&mut bytes, palette).unwrap();
                let visible = without_sgr(&bytes);
                assert!(
                    visible
                        .windows(b"> Reject".len())
                        .any(|window| window == b"> Reject")
                );
                assert_eq!(
                    bytes.matches(palette.sgr(TextStyle::Selection)).count(),
                    1,
                    "{palette:?} must style exactly the selected Reject choice at {columns}x{rows}"
                );
                if let Some(expected) = &expected {
                    assert_eq!(&visible, expected);
                } else {
                    expected = Some(visible);
                }
            }
        }
    }

    #[test]
    fn one_overwide_grapheme_is_replaced_without_autowrap() {
        let mut composer = Composer::default();
        let mut grapheme = String::from("क");
        grapheme.extend(std::iter::repeat_n('ा', 50));
        composer.insert_text(&grapheme).unwrap();
        let queue = PromptQueue::default();
        let frame = DockFrame::layout(
            DockModel {
                interaction: DockInteraction::Idle,
                composer: &composer,
                queue: &queue,
                notice: None,
                file_suggestions: FileSuggestionSnapshot::Hidden,
                working: WorkingPresentation::PLAIN,
            },
            20,
            44,
        )
        .unwrap();
        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.text.contains("[wide grapheme]"))
        );
        for line in &frame.lines {
            assert!(unicode_width::UnicodeWidthStr::width(line.text.as_str()) <= 43);
        }
    }

    #[test]
    fn debug_never_contains_composer_or_notice_text() {
        let mut composer = Composer::default();
        composer.insert_text("SECRET_DRAFT").unwrap();
        let queue = PromptQueue::default();
        let model = DockModel {
            interaction: DockInteraction::Idle,
            composer: &composer,
            queue: &queue,
            notice: Some("SECRET_NOTICE"),
            file_suggestions: FileSuggestionSnapshot::Hidden,
            working: WorkingPresentation::PLAIN,
        };
        let model_debug = format!("{model:?}");
        assert!(!model_debug.contains("SECRET_DRAFT"));
        assert!(!model_debug.contains("SECRET_NOTICE"));
        let frame = DockFrame::layout(model, 24, 80).unwrap();
        assert!(!format!("{frame:?}").contains("SECRET_DRAFT"));
    }

    #[test]
    fn detail_wrapped_rows_exact_and_one_over_have_truthful_markers() {
        let exact_text = "x".repeat(43 * MAX_DETAIL_WRAPPED_ROWS);
        let exact_document = DetailDocument::from_lines_for_test(
            ViewMode::Inspect,
            "INSPECT",
            &[(DetailTone::Code, exact_text.as_str())],
        );
        let (exact, exact_viewport) =
            DockFrame::layout_detail(&exact_document, usize::MAX, 20, 44).unwrap();
        assert_eq!(exact_viewport.total_rows, MAX_DETAIL_WRAPPED_ROWS);
        assert!(!exact_viewport.truncated);
        assert!(
            !exact
                .lines
                .iter()
                .any(|line| line.text.contains(DETAIL_WRAPPED_OMISSION))
        );
        assert!(!exact.lines.last().unwrap().text.contains("4096+"));

        let one_over_text = "x".repeat(43 * MAX_DETAIL_WRAPPED_ROWS + 1);
        let one_over_document = DetailDocument::from_lines_for_test(
            ViewMode::Inspect,
            "INSPECT",
            &[(DetailTone::Code, one_over_text.as_str())],
        );
        let (one_over, one_over_viewport) =
            DockFrame::layout_detail(&one_over_document, usize::MAX, 20, 44).unwrap();
        assert_eq!(one_over_viewport.total_rows, MAX_DETAIL_WRAPPED_ROWS);
        assert!(one_over_viewport.truncated);
        assert_eq!(
            one_over
                .lines
                .iter()
                .filter(|line| line.text.contains(DETAIL_WRAPPED_OMISSION))
                .count(),
            1
        );
        assert!(one_over.lines.last().unwrap().text.contains("4096+"));
        assert!(
            one_over
                .lines
                .iter()
                .all(|line| { unicode_width::UnicodeWidthStr::width(line.text.as_str()) <= 43 })
        );
    }

    #[test]
    fn detail_frames_fit_supported_widths_and_report_truthful_viewports() {
        let document = DetailDocument::from_lines_for_test(
            ViewMode::Review,
            "REVIEW · SECRET\u{202e}SAFE",
            &[
                (DetailTone::Accent, "ACTIONS"),
                (DetailTone::Positive, "Updated  src/界面.rs"),
                (DetailTone::Negative, "Command failed  exit 1"),
                (
                    DetailTone::Plain,
                    "e\u{301} and a long detail that wraps without autowrap",
                ),
            ],
        );
        for (rows, columns, expected_rows, expected_page_rows) in
            [(20, 44, 19, 16), (24, 80, 23, 20), (34, 112, 24, 21)]
        {
            let (frame, viewport) =
                DockFrame::layout_detail(&document, usize::MAX, rows, columns).unwrap();
            assert_eq!(frame.rows().unwrap(), expected_rows);
            assert_eq!(viewport.page_rows, expected_page_rows);
            assert_eq!(
                viewport.offset,
                viewport.total_rows.saturating_sub(viewport.page_rows)
            );
            assert!(!viewport.truncated);
            assert!(!frame.software_cursor);
            assert!(frame.lines[0].text.contains("REVIEW"));
            assert!(
                !frame
                    .lines
                    .iter()
                    .any(|line| line.text.contains('\u{202e}'))
            );
            assert!(frame.lines.iter().all(|line| {
                unicode_width::UnicodeWidthStr::width(line.text.as_str())
                    <= usize::from(columns - 1)
            }));
        }
        assert!(!format!("{document:?}").contains("SECRET"));
    }

    #[test]
    fn detail_replaces_one_overlong_zero_width_grapheme() {
        let mut grapheme = String::from("क");
        grapheme.extend(std::iter::repeat_n('\u{93e}', 1_100));
        let document = DetailDocument::from_lines_for_test(
            ViewMode::Inspect,
            "INSPECT",
            &[(DetailTone::Code, grapheme.as_str())],
        );
        let (frame, _) = DockFrame::layout_detail(&document, 0, 20, 44).unwrap();
        assert!(
            frame
                .lines
                .iter()
                .any(|line| line.text.contains("[wide grapheme]"))
        );
        assert!(
            frame
                .lines
                .iter()
                .all(|line| { unicode_width::UnicodeWidthStr::width(line.text.as_str()) <= 43 })
        );
    }
}
