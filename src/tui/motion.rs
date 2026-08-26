//! Closed process-local motion preference and pure working-status presentation.

use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MotionPreference {
    #[default]
    Full,
    Reduced,
}

impl MotionPreference {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(Self::Full),
            "reduced" => Some(Self::Reduced),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MotionCommand {
    Show,
    Select(MotionPreference),
    Invalid,
}

impl MotionCommand {
    pub(crate) fn parse(record: &str) -> Option<Self> {
        let mut fields = record.split_ascii_whitespace();
        let command = fields.next()?;
        if command != "/motion" {
            return (command.eq_ignore_ascii_case("/motion") || command == "/motions")
                .then_some(Self::Invalid);
        }
        Some(match (fields.next(), fields.next()) {
            (None, None) => Self::Show,
            (Some(name), None) => MotionPreference::from_name(name)
                .map(Self::Select)
                .unwrap_or(Self::Invalid),
            _ => Self::Invalid,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MotionRequest {
    preference: MotionPreference,
    revision: u64,
}

impl MotionRequest {
    pub(crate) const fn preference(self) -> MotionPreference {
        self.preference
    }
}

#[derive(Debug)]
pub(crate) struct MotionState {
    requested: MotionRequest,
    committed: MotionRequest,
}

impl MotionState {
    pub(crate) const fn new(preference: MotionPreference) -> Self {
        let request = MotionRequest {
            preference,
            revision: 0,
        };
        Self {
            requested: request,
            committed: request,
        }
    }

    pub(crate) const fn requested(&self) -> MotionRequest {
        self.requested
    }

    pub(crate) const fn committed(&self) -> MotionRequest {
        self.committed
    }

    pub(crate) const fn is_transitioning(&self) -> bool {
        self.requested.revision != self.committed.revision
    }

    pub(crate) fn request(&mut self, preference: MotionPreference) -> Result<bool, MotionError> {
        if self.requested.preference == preference {
            return Ok(false);
        }
        self.requested = MotionRequest {
            preference,
            revision: self
                .requested
                .revision
                .checked_add(1)
                .ok_or(MotionError::Limit)?,
        };
        Ok(true)
    }

    pub(crate) fn commit(&mut self, request: MotionRequest) -> bool {
        if request != self.requested {
            return false;
        }
        self.committed = request;
        true
    }
}

impl Default for MotionState {
    fn default() -> Self {
        Self::new(MotionPreference::Full)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkingPhase {
    Plain,
    Static,
    Animated(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkingAge {
    Fresh,
    OneSecond { seconds: u64 },
    Long { seconds: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkingPresentation {
    pub(crate) phase: WorkingPhase,
    pub(crate) age: WorkingAge,
}

impl WorkingPresentation {
    pub(crate) const PLAIN: Self = Self {
        phase: WorkingPhase::Plain,
        age: WorkingAge::Fresh,
    };

    pub(crate) const STATIC: Self = Self {
        phase: WorkingPhase::Static,
        age: WorkingAge::Fresh,
    };

    pub(crate) const fn phase_glyph(self) -> Option<char> {
        match self.phase {
            WorkingPhase::Plain | WorkingPhase::Static => None,
            WorkingPhase::Animated(phase) => Some(match phase % 4 {
                0 => '|',
                1 => '/',
                2 => '-',
                _ => '\\',
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum MotionError {
    #[error("CLI_OUTPUT_LIMIT")]
    Limit,
}

#[cfg(test)]
mod tests {
    use super::{
        MotionCommand, MotionPreference, MotionState, WorkingAge, WorkingPhase, WorkingPresentation,
    };

    #[test]
    fn command_surface_is_closed_and_case_sensitive() {
        assert_eq!(MotionCommand::parse("/motion"), Some(MotionCommand::Show));
        assert_eq!(
            MotionCommand::parse("  /motion reduced  "),
            Some(MotionCommand::Select(MotionPreference::Reduced))
        );
        assert_eq!(
            MotionCommand::parse("/motion full"),
            Some(MotionCommand::Select(MotionPreference::Full))
        );
        for record in ["/motion standard", "/motion FULL", "/motion reduced extra"] {
            assert_eq!(MotionCommand::parse(record), Some(MotionCommand::Invalid));
        }
        for record in ["/motions", "/Motion reduced", "/MOTION"] {
            assert_eq!(MotionCommand::parse(record), Some(MotionCommand::Invalid));
        }
        for record in ["/motional", "motion reduced"] {
            assert_eq!(MotionCommand::parse(record), None);
        }
    }

    #[test]
    fn requests_commit_only_the_latest_revision() {
        let mut state = MotionState::default();
        let initial = state.requested();
        assert_eq!(initial.preference(), MotionPreference::Full);
        assert!(!state.is_transitioning());
        assert!(!state.request(MotionPreference::Full).unwrap());

        assert!(state.request(MotionPreference::Reduced).unwrap());
        let reduced = state.requested();
        assert!(state.is_transitioning());
        assert!(!state.commit(initial));
        assert!(state.commit(reduced));
        assert!(!state.is_transitioning());
    }

    #[test]
    fn spinner_phase_table_is_fixed_ascii_and_semantically_separate() {
        let glyphs = (0..8)
            .map(|phase| WorkingPresentation {
                phase: WorkingPhase::Animated(phase),
                age: WorkingAge::Fresh,
            })
            .map(WorkingPresentation::phase_glyph)
            .collect::<Vec<_>>();
        assert_eq!(
            glyphs,
            [
                Some('|'),
                Some('/'),
                Some('-'),
                Some('\\'),
                Some('|'),
                Some('/'),
                Some('-'),
                Some('\\'),
            ]
        );
    }
}
