use crate::{
    goal::{GoalCommand, GoalError},
    plan_mode::{PlanModeCommand, PlanModeError},
    session::EventSeq,
    tui::{motion::MotionCommand, theme::ThemeCommand},
};

pub(super) const MAX_INTERACTIVE_PROMPT_BYTES: usize = 1_000;
pub(super) const MAX_APPROVAL_RECORD_BYTES: usize = 64;
pub(super) const MAX_MODEL_ID_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ModelEffort {
    Off,
    High,
    Max,
}

impl ModelEffort {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ModelCommand {
    Show,
    Select {
        model: String,
        effort: Option<ModelEffort>,
    },
    Usage,
}

pub(super) fn parse_model_command(command: &str) -> Option<ModelCommand> {
    if command == "/model" {
        return Some(ModelCommand::Show);
    }
    let suffix = command.strip_prefix("/model")?;
    if !suffix.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let mut fields = suffix.split_whitespace();
    let Some(model) = fields.next() else {
        return Some(ModelCommand::Show);
    };
    if model.len() > MAX_MODEL_ID_BYTES || model.chars().any(char::is_control) {
        return Some(ModelCommand::Usage);
    }
    let effort = match fields.next() {
        None => None,
        Some("off") => Some(ModelEffort::Off),
        Some("high") => Some(ModelEffort::High),
        Some("max") => Some(ModelEffort::Max),
        Some(_) => return Some(ModelCommand::Usage),
    };
    if fields.next().is_some() {
        return Some(ModelCommand::Usage);
    }
    Some(ModelCommand::Select {
        model: model.to_owned(),
        effort,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RenameCommand {
    Show,
    Set(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForkCommand {
    Latest,
    At(EventSeq),
    Usage,
}

pub(super) fn parse_fork_command(command: &str) -> Option<ForkCommand> {
    if command == "/fork" {
        return Some(ForkCommand::Latest);
    }
    let suffix = command.strip_prefix("/fork")?;
    if !suffix.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let value = suffix.trim_matches(|character: char| character.is_ascii_whitespace());
    let parsed = value.parse::<u64>().ok();
    match parsed {
        Some(parsed) if parsed.to_string() == value => {
            Some(EventSeq::new(parsed).map_or(ForkCommand::Usage, ForkCommand::At))
        }
        _ => Some(ForkCommand::Usage),
    }
}

pub(super) fn parse_rename_command(command: &str) -> Option<RenameCommand> {
    if command == "/rename" {
        return Some(RenameCommand::Show);
    }
    command.strip_prefix("/rename").and_then(|suffix| {
        suffix
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
            .then(|| RenameCommand::Set(suffix.to_owned()))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum InputRecordEvent {
    Record {
        text: String,
        terminated_by_lf: bool,
    },
    TooLarge,
    InvalidUtf8,
}

pub(super) struct CanonicalRecordParser {
    buffer: [u8; MAX_INTERACTIVE_PROMPT_BYTES],
    length: usize,
    limit: usize,
    draining_oversized: bool,
}

impl CanonicalRecordParser {
    pub(super) fn new(limit: usize) -> Self {
        debug_assert!((1..=MAX_INTERACTIVE_PROMPT_BYTES).contains(&limit));
        Self {
            buffer: [0; MAX_INTERACTIVE_PROMPT_BYTES],
            length: 0,
            limit: limit.clamp(1, MAX_INTERACTIVE_PROMPT_BYTES),
            draining_oversized: false,
        }
    }

    pub(super) fn reset(&mut self, limit: usize) {
        debug_assert!((1..=MAX_INTERACTIVE_PROMPT_BYTES).contains(&limit));
        self.length = 0;
        self.limit = limit.clamp(1, MAX_INTERACTIVE_PROMPT_BYTES);
        self.draining_oversized = false;
    }

    pub(super) fn feed(
        &mut self,
        bytes: &[u8],
        canonical_boundary_at_end: bool,
        mut emit: impl FnMut(InputRecordEvent),
    ) {
        for &byte in bytes {
            if self.draining_oversized {
                if byte == b'\n' {
                    self.draining_oversized = false;
                }
                continue;
            }
            if byte == b'\n' {
                self.emit_record(true, &mut emit);
                continue;
            }
            if self.length == self.limit {
                self.length = 0;
                self.draining_oversized = true;
                emit(InputRecordEvent::TooLarge);
                continue;
            }
            self.buffer[self.length] = byte;
            self.length += 1;
        }

        if canonical_boundary_at_end {
            if self.draining_oversized {
                self.draining_oversized = false;
            } else if self.length != 0 {
                self.emit_record(false, &mut emit);
            }
        }
    }

    fn emit_record(&mut self, terminated_by_lf: bool, emit: &mut impl FnMut(InputRecordEvent)) {
        let event = match std::str::from_utf8(&self.buffer[..self.length]) {
            Ok(text) => InputRecordEvent::Record {
                text: text.to_owned(),
                terminated_by_lf,
            },
            Err(_) => InputRecordEvent::InvalidUtf8,
        };
        self.length = 0;
        emit(event);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum IdleInput {
    Redraw,
    Help,
    Inspect,
    Review,
    Theme(ThemeCommand),
    Motion(MotionCommand),
    Goal(Result<GoalCommand, GoalError>),
    Plan(Result<PlanModeCommand, PlanModeError>),
    Model(ModelCommand),
    Rename(RenameCommand),
    RefreshTitle,
    RefreshTitleUsage,
    Export,
    ExportUsage,
    Fork(ForkCommand),
    Compact,
    CompactUsage,
    Exit,
    Submit(String),
}

pub(super) fn classify_idle_record(record: &str, _terminated_by_lf: bool) -> IdleInput {
    let command = record.trim_matches(|character: char| character.is_ascii_whitespace());
    if let Some(theme) = ThemeCommand::parse(command) {
        return IdleInput::Theme(theme);
    }
    if let Some(motion) = MotionCommand::parse(command) {
        return IdleInput::Motion(motion);
    }
    if let Some(goal) = GoalCommand::parse(command) {
        return IdleInput::Goal(goal);
    }
    if let Some(plan) = PlanModeCommand::parse(command) {
        return IdleInput::Plan(plan);
    }
    if let Some(model) = parse_model_command(command) {
        return IdleInput::Model(model);
    }
    if let Some(rename) = parse_rename_command(command) {
        return IdleInput::Rename(rename);
    }
    if let Some(fork) = parse_fork_command(command) {
        return IdleInput::Fork(fork);
    }
    if command
        .strip_prefix("/compact")
        .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
    {
        return IdleInput::CompactUsage;
    }
    if command
        .strip_prefix("/refresh-title")
        .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
    {
        return IdleInput::RefreshTitleUsage;
    }
    if command
        .strip_prefix("/export")
        .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
    {
        return IdleInput::ExportUsage;
    }
    match command {
        "" => IdleInput::Redraw,
        "/help" => IdleInput::Help,
        "/inspect" => IdleInput::Inspect,
        "/review" => IdleInput::Review,
        "/compact" => IdleInput::Compact,
        "/refresh-title" => IdleInput::RefreshTitle,
        "/export" => IdleInput::Export,
        "/exit" | "/quit" => IdleInput::Exit,
        _ => IdleInput::Submit(record.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CanonicalRecordParser, ForkCommand, IdleInput, InputRecordEvent, ModelCommand, ModelEffort,
        RenameCommand, classify_idle_record,
    };
    use crate::tui::{
        motion::{MotionCommand, MotionPreference},
        theme::{ThemeCommand, ThemePalette},
    };

    fn feed(
        parser: &mut CanonicalRecordParser,
        bytes: &[u8],
        boundary: bool,
    ) -> Vec<InputRecordEvent> {
        let mut events = Vec::new();
        parser.feed(bytes, boundary, |event| events.push(event));
        events
    }

    #[test]
    fn exact_limit_is_accepted_and_one_byte_over_is_drained() {
        let mut parser = CanonicalRecordParser::new(1_000);
        let mut exact = vec![b'x'; 1_000];
        exact.push(b'\n');
        assert_eq!(
            feed(&mut parser, &exact, true),
            vec![InputRecordEvent::Record {
                text: "x".repeat(1_000),
                terminated_by_lf: true,
            }]
        );

        let mut over = vec![b'x'; 1_001];
        over.push(b'\n');
        assert_eq!(
            feed(&mut parser, &over, true),
            vec![InputRecordEvent::TooLarge]
        );
        assert_eq!(
            feed(&mut parser, b"next\n", true),
            vec![InputRecordEvent::Record {
                text: "next".to_owned(),
                terminated_by_lf: true,
            }]
        );
    }

    #[test]
    fn utf8_limits_are_bytes_and_invalid_records_are_rejected_without_echoing() {
        let mut parser = CanonicalRecordParser::new(1_000);
        let exact = format!("{}\n", "é".repeat(500));
        assert!(matches!(
            feed(&mut parser, exact.as_bytes(), true).as_slice(),
            [InputRecordEvent::Record { text, .. }] if text.len() == 1_000
        ));
        let over = format!("{}\n", "é".repeat(501));
        assert_eq!(
            feed(&mut parser, over.as_bytes(), true),
            vec![InputRecordEvent::TooLarge]
        );
        assert_eq!(
            feed(&mut parser, &[0xff, b'\n'], true),
            vec![InputRecordEvent::InvalidUtf8]
        );
    }

    #[test]
    fn an_oversized_record_drains_to_its_boundary_before_accepting_another() {
        let mut parser = CanonicalRecordParser::new(4);
        assert_eq!(
            feed(&mut parser, b"abcde", false),
            vec![InputRecordEvent::TooLarge]
        );
        assert!(feed(&mut parser, b"still discarded", false).is_empty());
        assert_eq!(
            feed(&mut parser, b"\nok\n", true),
            vec![InputRecordEvent::Record {
                text: "ok".to_owned(),
                terminated_by_lf: true,
            }]
        );
    }

    #[test]
    fn multiple_lines_and_a_non_lf_veof_record_keep_their_boundaries() {
        let mut parser = CanonicalRecordParser::new(16);
        assert_eq!(
            feed(&mut parser, b"one\n\ntwo", true),
            vec![
                InputRecordEvent::Record {
                    text: "one".to_owned(),
                    terminated_by_lf: true,
                },
                InputRecordEvent::Record {
                    text: String::new(),
                    terminated_by_lf: true,
                },
                InputRecordEvent::Record {
                    text: "two".to_owned(),
                    terminated_by_lf: false,
                },
            ]
        );
    }

    #[test]
    fn resetting_an_input_fence_discards_partial_and_oversized_state() {
        let mut parser = CanonicalRecordParser::new(4);
        assert!(feed(&mut parser, b"ab", false).is_empty());
        parser.reset(8);
        assert_eq!(
            feed(&mut parser, b"fresh\n", true),
            vec![InputRecordEvent::Record {
                text: "fresh".to_owned(),
                terminated_by_lf: true,
            }]
        );

        assert_eq!(
            feed(&mut parser, b"123456789", false),
            vec![InputRecordEvent::TooLarge]
        );
        parser.reset(4);
        assert_eq!(
            feed(&mut parser, b"ok\n", true),
            vec![InputRecordEvent::Record {
                text: "ok".to_owned(),
                terminated_by_lf: true,
            }]
        );
    }

    #[test]
    fn idle_classification_trims_only_for_commands_and_preserves_prompt_bytes() {
        assert_eq!(classify_idle_record("  \t", true), IdleInput::Redraw);
        assert_eq!(classify_idle_record("  /help \t", true), IdleInput::Help);
        assert_eq!(classify_idle_record(" /inspect ", true), IdleInput::Inspect);
        assert_eq!(classify_idle_record(" /review ", true), IdleInput::Review);
        assert_eq!(classify_idle_record(" /compact ", true), IdleInput::Compact);
        assert_eq!(
            classify_idle_record(" /refresh-title ", true),
            IdleInput::RefreshTitle
        );
        assert_eq!(classify_idle_record(" /export ", true), IdleInput::Export);
        assert_eq!(
            classify_idle_record(" /model ", true),
            IdleInput::Model(ModelCommand::Show)
        );
        assert_eq!(
            classify_idle_record(" /model private-preview max ", true),
            IdleInput::Model(ModelCommand::Select {
                model: "private-preview".to_owned(),
                effort: Some(ModelEffort::Max),
            })
        );
        assert_eq!(
            classify_idle_record(" /model deepseek-v4-pro ", true),
            IdleInput::Model(ModelCommand::Select {
                model: "deepseek-v4-pro".to_owned(),
                effort: None,
            })
        );
        for invalid in [
            "/model model medium",
            "/model model HIGH",
            "/model model high extra",
        ] {
            assert_eq!(
                classify_idle_record(invalid, true),
                IdleInput::Model(ModelCommand::Usage)
            );
        }
        assert_eq!(
            classify_idle_record(&format!("/model {}", "x".repeat(257)), true),
            IdleInput::Model(ModelCommand::Usage)
        );
        assert_eq!(
            classify_idle_record(" /fork ", true),
            IdleInput::Fork(ForkCommand::Latest)
        );
        assert_eq!(
            classify_idle_record(" /fork 42 ", true),
            IdleInput::Fork(ForkCommand::At(crate::session::EventSeq::new(42).unwrap()))
        );
        for invalid in ["/fork -1", "/fork 1.5", "/fork 01", "/fork nope"] {
            assert_eq!(
                classify_idle_record(invalid, true),
                IdleInput::Fork(ForkCommand::Usage)
            );
        }
        assert_eq!(
            classify_idle_record("/refresh-title now", true),
            IdleInput::RefreshTitleUsage
        );
        assert_eq!(
            classify_idle_record("/export output.zip", true),
            IdleInput::ExportUsage
        );
        assert_eq!(
            classify_idle_record(" /rename ", true),
            IdleInput::Rename(RenameCommand::Show)
        );
        assert_eq!(
            classify_idle_record(" /rename  Hand\tpicked   name ", true),
            IdleInput::Rename(RenameCommand::Set("  Hand\tpicked   name".to_owned()))
        );
        assert_eq!(
            classify_idle_record(" /renamed is a prompt ", true),
            IdleInput::Submit(" /renamed is a prompt ".to_owned())
        );
        assert_eq!(
            classify_idle_record("/compact now", true),
            IdleInput::CompactUsage
        );
        assert_eq!(
            classify_idle_record(" /theme paper ", true),
            IdleInput::Theme(ThemeCommand::Select(ThemePalette::Paper))
        );
        assert_eq!(
            classify_idle_record(" /theme secret-name ", true),
            IdleInput::Theme(ThemeCommand::Invalid)
        );
        assert_eq!(
            classify_idle_record(" /motion reduced ", true),
            IdleInput::Motion(MotionCommand::Select(MotionPreference::Reduced))
        );
        assert_eq!(
            classify_idle_record(" /motion secret-name ", true),
            IdleInput::Motion(MotionCommand::Invalid)
        );
        assert_eq!(
            classify_idle_record(" /Motion reduced ", true),
            IdleInput::Motion(MotionCommand::Invalid)
        );
        assert_eq!(
            classify_idle_record(" /motions ", true),
            IdleInput::Motion(MotionCommand::Invalid)
        );
        assert_eq!(classify_idle_record(" /exit ", false), IdleInput::Exit);
        assert_eq!(classify_idle_record("\t/quit", true), IdleInput::Exit);
        assert_eq!(
            classify_idle_record("  ordinary prompt  ", false),
            IdleInput::Submit("  ordinary prompt  ".to_owned())
        );
        assert_eq!(
            classify_idle_record("/unknown", true),
            IdleInput::Submit("/unknown".to_owned())
        );
    }
}
