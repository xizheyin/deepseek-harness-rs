//! Closed local-command completion state for the enhanced composer.

use std::fmt;

use super::composer::Composer;

pub(crate) const COMMAND_COUNT: usize = 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandId {
    Help,
    Inspect,
    Review,
    Focus,
    Theme,
    Motion,
    Goal,
    Compact,
    Rename,
    RefreshTitle,
    Export,
    Fork,
    Exit,
    Quit,
}

impl CommandId {
    pub(crate) const ALL: [Self; COMMAND_COUNT] = [
        Self::Help,
        Self::Inspect,
        Self::Review,
        Self::Focus,
        Self::Theme,
        Self::Motion,
        Self::Exit,
        Self::Quit,
        Self::Goal,
        Self::Compact,
        Self::Rename,
        Self::RefreshTitle,
        Self::Export,
        Self::Fork,
    ];

    pub(crate) const fn command(self) -> &'static str {
        match self {
            Self::Help => "/help",
            Self::Inspect => "/inspect",
            Self::Review => "/review",
            Self::Focus => "/focus",
            Self::Theme => "/theme",
            Self::Motion => "/motion",
            Self::Goal => "/goal",
            Self::Compact => "/compact",
            Self::Rename => "/rename",
            Self::RefreshTitle => "/refresh-title",
            Self::Export => "/export",
            Self::Fork => "/fork",
            Self::Exit => "/exit",
            Self::Quit => "/quit",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Help => "show local commands",
            Self::Inspect => "inspect current turn",
            Self::Review => "review last settled turn",
            Self::Focus => "return to Focus",
            Self::Theme => "show or select theme",
            Self::Motion => "show or select motion",
            Self::Goal => "show or manage Goal",
            Self::Compact => "summarize older history",
            Self::Rename => "rename the current session",
            Self::RefreshTitle => "refresh the session title",
            Self::Export => "export the current session log",
            Self::Fork => "fork the current session",
            Self::Exit => "clean up and exit",
            Self::Quit => "clean up and exit",
        }
    }

    pub(crate) fn from_exact(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|command| command.command() == value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaletteMove {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaletteEnter {
    Submit,
    Complete(CommandId),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum CommandPaletteSnapshot {
    Hidden,
    Visible {
        matches: [CommandId; COMMAND_COUNT],
        count: usize,
        selected: Option<CommandId>,
    },
}

impl fmt::Debug for CommandPaletteSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => formatter.write_str("Hidden"),
            Self::Visible {
                count, selected, ..
            } => formatter
                .debug_struct("Visible")
                .field("matches", count)
                .field("selected", selected)
                .finish(),
        }
    }
}

impl CommandPaletteSnapshot {
    pub(crate) const fn is_visible(self) -> bool {
        matches!(self, Self::Visible { .. })
    }

    pub(crate) const fn count(self) -> usize {
        match self {
            Self::Hidden => 0,
            Self::Visible { count, .. } => count,
        }
    }

    pub(crate) const fn command_at(self, index: usize) -> Option<CommandId> {
        match self {
            Self::Hidden => None,
            Self::Visible { matches, count, .. } if index < count => Some(matches[index]),
            Self::Visible { .. } => None,
        }
    }

    pub(crate) const fn selected(self) -> Option<CommandId> {
        match self {
            Self::Hidden => None,
            Self::Visible { selected, .. } => selected,
        }
    }
}

#[derive(Default)]
pub(crate) struct CommandPaletteState {
    selected: Option<CommandId>,
    dismissed_revision: Option<u64>,
}

impl fmt::Debug for CommandPaletteState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandPaletteState")
            .field("selected", &self.selected)
            .field("dismissed_revision", &self.dismissed_revision)
            .finish()
    }
}

impl CommandPaletteState {
    pub(crate) fn sync(&mut self, composer: &Composer) -> CommandPaletteSnapshot {
        if self.dismissed_revision != Some(composer.content_revision()) {
            self.dismissed_revision = None;
        }
        let snapshot = derive_snapshot(composer, self.dismissed_revision);
        if let CommandPaletteSnapshot::Visible {
            matches,
            count,
            selected: _,
        } = snapshot
        {
            self.selected = if count == 0 {
                None
            } else if self
                .selected
                .is_some_and(|selected| matches[..count].contains(&selected))
            {
                self.selected
            } else {
                Some(matches[0])
            };
            CommandPaletteSnapshot::Visible {
                matches,
                count,
                selected: self.selected,
            }
        } else {
            snapshot
        }
    }

    pub(crate) fn snapshot(&self, composer: &Composer) -> CommandPaletteSnapshot {
        let snapshot = derive_snapshot(composer, self.dismissed_revision);
        match snapshot {
            CommandPaletteSnapshot::Visible {
                matches,
                count,
                selected: _,
            } => CommandPaletteSnapshot::Visible {
                matches,
                count,
                selected: self
                    .selected
                    .filter(|selected| matches[..count].contains(selected))
                    .or_else(|| matches[..count].first().copied()),
            },
            CommandPaletteSnapshot::Hidden => CommandPaletteSnapshot::Hidden,
        }
    }

    pub(crate) fn navigate(&mut self, composer: &Composer, movement: PaletteMove) -> bool {
        let snapshot = self.sync(composer);
        let CommandPaletteSnapshot::Visible {
            matches,
            count,
            selected,
        } = snapshot
        else {
            return false;
        };
        let Some(selected) = selected else {
            return true;
        };
        let index = matches[..count]
            .iter()
            .position(|candidate| *candidate == selected)
            .unwrap_or(0);
        let next = match movement {
            PaletteMove::Previous => index.saturating_sub(1),
            PaletteMove::Next => index.saturating_add(1).min(count - 1),
        };
        self.selected = Some(matches[next]);
        true
    }

    pub(crate) fn enter(&mut self, composer: &Composer) -> PaletteEnter {
        let snapshot = self.sync(composer);
        let Some(selected) = snapshot.selected() else {
            return PaletteEnter::Submit;
        };
        if composer.text() == selected.command() {
            PaletteEnter::Submit
        } else {
            PaletteEnter::Complete(selected)
        }
    }

    pub(crate) fn dismiss(&mut self, composer: &Composer) -> bool {
        if !self.snapshot(composer).is_visible() {
            return false;
        }
        self.dismissed_revision = Some(composer.content_revision());
        true
    }
}

fn derive_snapshot(composer: &Composer, dismissed_revision: Option<u64>) -> CommandPaletteSnapshot {
    let text = composer.text();
    if dismissed_revision == Some(composer.content_revision())
        || composer.cursor() != text.len()
        || !text.starts_with('/')
        || text.contains('\n')
    {
        return CommandPaletteSnapshot::Hidden;
    }
    let mut matches = [CommandId::Help; COMMAND_COUNT];
    let mut count = 0_usize;
    for command in CommandId::ALL {
        if command.command().starts_with(text) {
            matches[count] = command;
            count += 1;
        }
    }
    CommandPaletteSnapshot::Visible {
        matches,
        count,
        selected: matches[..count].first().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandId, CommandPaletteSnapshot, CommandPaletteState, PaletteEnter, PaletteMove,
    };
    use crate::tui::composer::Composer;

    fn composer(text: &str) -> Composer {
        let mut composer = Composer::default();
        composer.insert_text(text).unwrap();
        composer
    }

    #[test]
    fn catalogue_and_prefixes_are_closed_ordered_and_ascii() {
        assert_eq!(CommandId::ALL.len(), 14);
        assert_eq!(CommandId::ALL[0], CommandId::Help);
        for (index, command) in CommandId::ALL.into_iter().enumerate() {
            assert!(command.command().is_ascii());
            assert!(command.description().is_ascii());
            assert!(command.command().starts_with('/'));
            assert!(!command.description().is_empty());
            assert!(!CommandId::ALL[..index].contains(&command));
            assert_eq!(CommandId::from_exact(command.command()), Some(command));
        }
        assert_eq!(CommandId::from_exact("/unknown"), None);
    }

    #[test]
    fn visibility_selection_navigation_and_stale_fallback_are_deterministic() {
        let mut state = CommandPaletteState::default();
        let mut draft = composer("/");
        let snapshot = state.sync(&draft);
        assert_eq!(snapshot.selected(), Some(CommandId::Help));
        assert!(state.navigate(&draft, PaletteMove::Next));
        assert_eq!(state.snapshot(&draft).selected(), Some(CommandId::Inspect));
        assert!(state.navigate(&draft, PaletteMove::Previous));
        assert_eq!(state.snapshot(&draft).selected(), Some(CommandId::Help));
        assert!(state.navigate(&draft, PaletteMove::Previous));
        assert_eq!(state.snapshot(&draft).selected(), Some(CommandId::Help));

        draft.insert_text("th").unwrap();
        assert_eq!(state.sync(&draft).selected(), Some(CommandId::Theme));
        assert_eq!(
            state.enter(&draft),
            PaletteEnter::Complete(CommandId::Theme)
        );

        let no_match = composer("/unknown");
        assert!(matches!(
            state.sync(&no_match),
            CommandPaletteSnapshot::Visible {
                count: 0,
                selected: None,
                ..
            }
        ));
        assert!(state.navigate(&no_match, PaletteMove::Next));
        assert_eq!(state.enter(&no_match), PaletteEnter::Submit);
        for upper_case in ["/H", "/HELP"] {
            assert!(matches!(
                state.sync(&composer(upper_case)),
                CommandPaletteSnapshot::Visible {
                    count: 0,
                    selected: None,
                    ..
                }
            ));
        }
    }

    #[test]
    fn dismissal_is_revision_scoped_and_non_tokens_stay_hidden() {
        let mut state = CommandPaletteState::default();
        let mut draft = composer("/he");
        assert!(state.dismiss(&draft));
        assert_eq!(state.snapshot(&draft), CommandPaletteSnapshot::Hidden);
        draft.insert_char('l').unwrap();
        assert!(state.sync(&draft).is_visible());

        for text in ["", " /he", "say /he", "/he\n"] {
            assert_eq!(
                state.sync(&composer(text)),
                CommandPaletteSnapshot::Hidden,
                "{text:?}"
            );
        }
        let mut inside = composer("/help");
        assert!(inside.move_left());
        assert_eq!(state.sync(&inside), CommandPaletteSnapshot::Hidden);
    }
}
