//! Closed semantic palettes for the enhanced primary-screen renderer.

use thiserror::Error;

use super::presentation::TextStyle;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThemePalette {
    #[default]
    Adaptive,
    Midnight,
    Paper,
    ColorBlind,
    HighContrast,
    Mono,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThemeCommand {
    Show,
    Select(ThemePalette),
    Invalid,
}

impl ThemeCommand {
    pub(crate) fn parse(record: &str) -> Option<Self> {
        let mut fields = record.split_ascii_whitespace();
        if fields.next()? != "/theme" {
            return None;
        }
        let command = match (fields.next(), fields.next()) {
            (None, None) => Self::Show,
            (Some(name), None) => ThemePalette::from_name(name)
                .map(Self::Select)
                .unwrap_or(Self::Invalid),
            _ => Self::Invalid,
        };
        Some(command)
    }
}

impl ThemePalette {
    pub(crate) const ALL: [Self; 6] = [
        Self::Adaptive,
        Self::Midnight,
        Self::Paper,
        Self::ColorBlind,
        Self::HighContrast,
        Self::Mono,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Midnight => "midnight",
            Self::Paper => "paper",
            Self::ColorBlind => "color-blind",
            Self::HighContrast => "high-contrast",
            Self::Mono => "mono",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|palette| palette.name() == name)
    }

    /// Returns only compile-time SGR values. No palette owns a background,
    /// terminal query, OSC string, or user-provided escape sequence.
    pub(crate) const fn sgr(self, style: TextStyle) -> &'static str {
        match self {
            Self::Adaptive => adaptive(style),
            Self::Midnight => midnight(style),
            Self::Paper => paper(style),
            Self::ColorBlind => color_blind(style),
            Self::HighContrast => high_contrast(style),
            Self::Mono => mono(style),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeRequest {
    palette: ThemePalette,
    revision: u64,
}

impl ThemeRequest {
    pub(crate) const fn palette(self) -> ThemePalette {
        self.palette
    }
}

#[derive(Debug, Default)]
pub(crate) struct ThemeState {
    requested: ThemeRequest,
    committed: ThemeRequest,
}

impl Default for ThemeRequest {
    fn default() -> Self {
        Self {
            palette: ThemePalette::Adaptive,
            revision: 0,
        }
    }
}

impl ThemeState {
    pub(crate) const fn requested(&self) -> ThemeRequest {
        self.requested
    }

    #[cfg(test)]
    pub(crate) const fn committed(&self) -> ThemeRequest {
        self.committed
    }

    pub(crate) const fn is_transitioning(&self) -> bool {
        self.requested.revision != self.committed.revision
    }

    pub(crate) fn request(&mut self, palette: ThemePalette) -> Result<bool, ThemeError> {
        if self.requested.palette == palette {
            return Ok(false);
        }
        self.requested = ThemeRequest {
            palette,
            revision: self
                .requested
                .revision
                .checked_add(1)
                .ok_or(ThemeError::Limit)?,
        };
        Ok(true)
    }

    pub(crate) fn commit(&mut self, request: ThemeRequest) -> bool {
        if request != self.requested {
            return false;
        }
        self.committed = request;
        true
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum ThemeError {
    #[error("CLI_OUTPUT_LIMIT")]
    Limit,
}

const fn adaptive(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain => "\x1b[0m",
        TextStyle::Muted => "\x1b[2m",
        TextStyle::Accent | TextStyle::Heading => "\x1b[1;36m",
        TextStyle::User => "\x1b[1;35m",
        TextStyle::Assistant | TextStyle::DiffHunk => "\x1b[36m",
        TextStyle::Code => "\x1b[1m",
        TextStyle::Quote | TextStyle::Border => "\x1b[2;36m",
        TextStyle::DiffHeader => "\x1b[1;36m",
        TextStyle::DiffAdd | TextStyle::Success => "\x1b[32m",
        TextStyle::DiffRemove => "\x1b[31m",
        TextStyle::Warning => "\x1b[1;33m",
        TextStyle::Error => "\x1b[1;31m",
        TextStyle::Selection => "\x1b[1;7m",
    }
}

const fn midnight(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain => "\x1b[0m",
        TextStyle::Muted => "\x1b[2;38;5;245m",
        TextStyle::Accent | TextStyle::Border => "\x1b[1;38;5;81m",
        TextStyle::User => "\x1b[1;38;5;213m",
        TextStyle::Assistant | TextStyle::Quote | TextStyle::Heading => "\x1b[38;5;117m",
        TextStyle::Code => "\x1b[38;5;252m",
        TextStyle::DiffHeader => "\x1b[1;38;5;81m",
        TextStyle::DiffHunk => "\x1b[38;5;117m",
        TextStyle::DiffAdd | TextStyle::Success => "\x1b[38;5;114m",
        TextStyle::DiffRemove => "\x1b[38;5;210m",
        TextStyle::Warning => "\x1b[1;38;5;221m",
        TextStyle::Error => "\x1b[1;38;5;203m",
        TextStyle::Selection => "\x1b[1;7m",
    }
}

const fn paper(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain => "\x1b[0m",
        TextStyle::Muted => "\x1b[2;38;5;240m",
        TextStyle::Accent | TextStyle::Border => "\x1b[1;38;5;25m",
        TextStyle::User => "\x1b[1;38;5;90m",
        TextStyle::Assistant | TextStyle::Quote | TextStyle::Heading => "\x1b[38;5;24m",
        TextStyle::Code => "\x1b[38;5;236m",
        TextStyle::DiffHeader => "\x1b[1;38;5;25m",
        TextStyle::DiffHunk => "\x1b[38;5;24m",
        TextStyle::DiffAdd | TextStyle::Success => "\x1b[38;5;28m",
        TextStyle::DiffRemove | TextStyle::Error => "\x1b[1;38;5;124m",
        TextStyle::Warning => "\x1b[1;38;5;130m",
        TextStyle::Selection => "\x1b[1;7m",
    }
}

const fn color_blind(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain => "\x1b[0m",
        TextStyle::Muted => "\x1b[2m",
        TextStyle::Accent | TextStyle::Border => "\x1b[1;38;5;33m",
        TextStyle::User => "\x1b[1;38;5;127m",
        TextStyle::Assistant | TextStyle::Quote | TextStyle::Heading => "\x1b[38;5;37m",
        TextStyle::Code => "\x1b[1m",
        TextStyle::DiffHeader => "\x1b[1;38;5;33m",
        TextStyle::DiffHunk => "\x1b[38;5;127m",
        TextStyle::DiffAdd | TextStyle::Success => "\x1b[38;5;33m",
        TextStyle::DiffRemove | TextStyle::Warning => "\x1b[1;38;5;208m",
        TextStyle::Error => "\x1b[1;38;5;127m",
        TextStyle::Selection => "\x1b[1;7m",
    }
}

const fn high_contrast(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain => "\x1b[0m",
        TextStyle::Muted => "\x1b[1m",
        TextStyle::Accent | TextStyle::Heading | TextStyle::DiffHeader => "\x1b[1;4m",
        TextStyle::User
        | TextStyle::Assistant
        | TextStyle::DiffAdd
        | TextStyle::Success
        | TextStyle::Border => "\x1b[1m",
        TextStyle::Code | TextStyle::Quote | TextStyle::DiffHunk | TextStyle::DiffRemove => {
            "\x1b[4m"
        }
        TextStyle::Warning => "\x1b[1;7m",
        TextStyle::Error | TextStyle::Selection => "\x1b[1;4;7m",
    }
}

const fn mono(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Plain | TextStyle::Assistant => "\x1b[0m",
        TextStyle::Muted | TextStyle::Quote | TextStyle::DiffRemove | TextStyle::Border => {
            "\x1b[2m"
        }
        TextStyle::Accent
        | TextStyle::User
        | TextStyle::Code
        | TextStyle::DiffHeader
        | TextStyle::DiffAdd
        | TextStyle::Warning
        | TextStyle::Success => "\x1b[1m",
        TextStyle::Heading | TextStyle::Error => "\x1b[1;4m",
        TextStyle::DiffHunk => "\x1b[4m",
        TextStyle::Selection => "\x1b[1;7m",
    }
}

#[cfg(test)]
mod tests {
    use super::{ThemeCommand, ThemePalette, ThemeState};
    use crate::tui::presentation::TextStyle;

    #[test]
    fn all_six_palettes_are_closed_background_free_semantic_maps() {
        assert_eq!(
            ThemePalette::ALL.map(ThemePalette::name),
            [
                "adaptive",
                "midnight",
                "paper",
                "color-blind",
                "high-contrast",
                "mono",
            ]
        );
        for palette in ThemePalette::ALL {
            assert_eq!(ThemePalette::from_name(palette.name()), Some(palette));
            for style in TextStyle::ALL {
                let sgr = palette.sgr(style);
                assert!(sgr.starts_with("\x1b["));
                assert!(sgr.ends_with('m'));
                assert!(!sgr.contains("\x1b]"));
                assert!(!sgr.contains("48;5;"));
                for parameter in sgr[2..sgr.len() - 1].split(';') {
                    let parameter = parameter.parse::<u16>().unwrap();
                    assert_ne!(
                        parameter, 48,
                        "true-color/indexed backgrounds are forbidden"
                    );
                    assert!(!(40..=47).contains(&parameter));
                    assert!(!(100..=107).contains(&parameter));
                }
            }
        }
        assert_eq!(ThemePalette::from_name("Adaptive"), None);
        assert_eq!(ThemePalette::from_name("unknown"), None);
    }

    #[test]
    fn mono_has_no_foreground_color_and_keeps_non_color_distinctions() {
        for style in TextStyle::ALL {
            let sgr = ThemePalette::Mono.sgr(style);
            assert!(!sgr.contains("38;"));
            for parameter in sgr[2..sgr.len() - 1].split(';') {
                let parameter = parameter.parse::<u16>().unwrap();
                assert!(!(30..=37).contains(&parameter));
                assert!(!(90..=97).contains(&parameter));
            }
        }
        assert_ne!(
            ThemePalette::Mono.sgr(TextStyle::DiffAdd),
            ThemePalette::Mono.sgr(TextStyle::DiffRemove)
        );
        assert!(ThemePalette::Mono.sgr(TextStyle::Selection).contains('7'));
    }

    #[test]
    fn a_palette_becomes_committed_only_for_the_current_request() {
        let mut state = ThemeState::default();
        let adaptive = state.requested();
        assert_eq!(adaptive.palette(), ThemePalette::Adaptive);
        assert!(!state.is_transitioning());

        assert!(state.request(ThemePalette::Paper).unwrap());
        let paper = state.requested();
        assert!(state.is_transitioning());
        assert!(!state.commit(adaptive));
        assert_eq!(state.committed().palette(), ThemePalette::Adaptive);
        assert!(state.commit(paper));
        assert_eq!(state.committed().palette(), ThemePalette::Paper);
        assert!(!state.is_transitioning());
        assert!(!state.request(ThemePalette::Paper).unwrap());
    }

    #[test]
    fn theme_commands_are_exact_finite_and_never_echo_unknown_names() {
        assert_eq!(ThemeCommand::parse("/theme"), Some(ThemeCommand::Show));
        assert_eq!(
            ThemeCommand::parse("  /theme\tcolor-blind  "),
            Some(ThemeCommand::Select(ThemePalette::ColorBlind))
        );
        assert_eq!(
            ThemeCommand::parse("/theme unknown"),
            Some(ThemeCommand::Invalid)
        );
        assert_eq!(
            ThemeCommand::parse("/theme paper extra"),
            Some(ThemeCommand::Invalid)
        );
        assert_eq!(ThemeCommand::parse("/themes"), None);
        assert_eq!(ThemeCommand::parse("/Theme paper"), None);
    }
}
