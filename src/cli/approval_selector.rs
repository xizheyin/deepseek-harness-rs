use std::{fmt::Write as _, ops::ControlFlow, time::Duration};

use crate::{
    session::ApprovalOutcome,
    tui::key_decoder::{InputError, InputEvent, Key, KeyDecoder},
};

use super::{
    approval::{ApprovalAnswer, parse_approval_answer},
    input::MAX_APPROVAL_RECORD_BYTES,
};

pub(super) const ESCAPE_SEQUENCE_WAIT: Duration = Duration::from_millis(35);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApprovalSelection {
    AllowOnce,
    Reject,
    AllowExactShellForProcess,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApprovalInputProfile {
    LinearRecord,
    EnhancedDirectional,
}

impl ApprovalSelection {
    const fn previous(self, allow_exact_shell: bool) -> Self {
        match (self, allow_exact_shell) {
            (Self::AllowOnce, _) => Self::Cancel,
            (Self::Reject, _) => Self::AllowOnce,
            (Self::AllowExactShellForProcess, true) => Self::Reject,
            (Self::Cancel, true) => Self::AllowExactShellForProcess,
            (Self::AllowExactShellForProcess | Self::Cancel, false) => Self::Reject,
        }
    }

    const fn next(self, allow_exact_shell: bool) -> Self {
        match (self, allow_exact_shell) {
            (Self::AllowOnce, _) => Self::Reject,
            (Self::Reject, true) => Self::AllowExactShellForProcess,
            (Self::AllowExactShellForProcess, true) => Self::Cancel,
            (Self::Reject | Self::AllowExactShellForProcess, false) => Self::Cancel,
            (Self::Cancel, _) => Self::AllowOnce,
        }
    }

    const fn outcome(self) -> Option<ApprovalOutcome> {
        match self {
            Self::AllowOnce => Some(ApprovalOutcome::AllowedOnce),
            Self::Reject => Some(ApprovalOutcome::Rejected),
            Self::AllowExactShellForProcess => None,
            Self::Cancel => Some(ApprovalOutcome::Cancelled),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectorUpdate {
    None,
    Redraw,
    Decide(ApprovalOutcome),
    RememberExactShell,
    Eof,
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SelectorRenderError;

pub(super) struct ApprovalSelector {
    selected: ApprovalSelection,
    profile: ApprovalInputProfile,
    allow_exact_shell: bool,
    record: [u8; MAX_APPROVAL_RECORD_BYTES],
    record_len: usize,
    decoder: KeyDecoder,
    feed_serial: u64,
    allow_focus_serial: Option<u64>,
    draining_rejected_sequence: bool,
    draining_rejected_paste: bool,
}

impl ApprovalSelector {
    #[cfg(test)]
    pub(super) fn new(profile: ApprovalInputProfile) -> Result<Self, InputError> {
        Self::new_for_request(profile, false)
    }

    pub(super) fn new_for_request(
        profile: ApprovalInputProfile,
        allow_exact_shell: bool,
    ) -> Result<Self, InputError> {
        let mut decoder = KeyDecoder::default();
        decoder.reset_epoch()?;
        Ok(Self {
            // Enter must be safe even if a stale byte crosses the input fence.
            selected: ApprovalSelection::Reject,
            profile,
            allow_exact_shell,
            record: [0; MAX_APPROVAL_RECORD_BYTES],
            record_len: 0,
            decoder,
            feed_serial: 0,
            allow_focus_serial: None,
            draining_rejected_sequence: false,
            draining_rejected_paste: false,
        })
    }

    pub(super) const fn selected(&self) -> ApprovalSelection {
        self.selected
    }

    pub(super) const fn allows_exact_shell(&self) -> bool {
        self.allow_exact_shell
    }

    pub(super) fn render(
        &self,
        color: bool,
        compact: bool,
        redraw: bool,
    ) -> Result<String, SelectorRenderError> {
        let mut output = String::new();
        output
            .try_reserve_exact(512)
            .map_err(|_| SelectorRenderError)?;
        if color && redraw {
            let rows = if self.allow_exact_shell { 6 } else { 5 };
            write!(&mut output, "\x1b[{rows}A").map_err(|_| SelectorRenderError)?;
        }

        let title = if color {
            "\x1b[1;33m◆ Approval required\x1b[0m"
        } else {
            "[approval required]"
        };
        push_selector_line(&mut output, color && redraw, title)?;
        let choices = [
            (ApprovalSelection::AllowOnce, "Allow once"),
            (ApprovalSelection::Reject, "Reject"),
            (
                ApprovalSelection::AllowExactShellForProcess,
                "Allow exact Shell for this process",
            ),
            (ApprovalSelection::Cancel, "Cancel"),
        ];
        for (choice, label) in choices {
            if choice == ApprovalSelection::AllowExactShellForProcess && !self.allow_exact_shell {
                continue;
            }
            let selected = self.selected == choice;
            if color && redraw {
                output.push_str("\r\x1b[2K");
            }
            if color && selected {
                writeln!(&mut output, "  \x1b[1;30;43m › {label} \x1b[0m")
                    .map_err(|_| SelectorRenderError)?;
            } else if color {
                writeln!(&mut output, "     {label}").map_err(|_| SelectorRenderError)?;
            } else if selected {
                writeln!(&mut output, "  [x] {label}").map_err(|_| SelectorRenderError)?;
            } else {
                writeln!(&mut output, "  [ ] {label}").map_err(|_| SelectorRenderError)?;
            }
        }
        let hint = if compact {
            "arrows · Enter confirm · Esc cancel"
        } else {
            "↑/↓ or ←/→ move · Enter confirm · Esc cancel"
        };
        if color && redraw {
            output.push_str("\r\x1b[2K");
        }
        if color {
            writeln!(&mut output, "  \x1b[2m{hint}\x1b[0m").map_err(|_| SelectorRenderError)?;
        } else {
            writeln!(&mut output, "  {hint}").map_err(|_| SelectorRenderError)?;
        }
        Ok(output)
    }

    pub(super) fn escape_is_pending(&self) -> bool {
        self.decoder.escape_pending()
    }

    pub(super) fn expire_escape(&mut self) -> SelectorUpdate {
        if self.decoder.expire_escape().is_some() {
            self.record_len = 0;
            SelectorUpdate::Decide(ApprovalOutcome::Cancelled)
        } else {
            SelectorUpdate::None
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8], challenge: uuid::Uuid) -> SelectorUpdate {
        let Some(feed_serial) = self.feed_serial.checked_add(1) else {
            return SelectorUpdate::Invalid;
        };
        self.feed_serial = feed_serial;
        let mut redraw = false;
        let mut final_update = None;
        let mut decoder = std::mem::take(&mut self.decoder);
        let expected_epoch = decoder.epoch();
        let _ = decoder.feed(bytes, |decoded| {
            let update = if decoded.epoch == expected_epoch {
                self.feed_event(decoded.event, challenge)
            } else {
                SelectorUpdate::Invalid
            };
            match update {
                SelectorUpdate::None => {}
                SelectorUpdate::Redraw => redraw = true,
                decision @ (SelectorUpdate::Decide(_)
                | SelectorUpdate::RememberExactShell
                | SelectorUpdate::Eof
                | SelectorUpdate::Invalid) => {
                    final_update = Some(decision);
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        self.decoder = decoder;
        if let Some(update) = final_update {
            update
        } else if redraw {
            SelectorUpdate::Redraw
        } else {
            SelectorUpdate::None
        }
    }

    fn feed_event(&mut self, event: InputEvent, challenge: uuid::Uuid) -> SelectorUpdate {
        match event {
            InputEvent::Key(Key::Escape) => {
                self.record_len = 0;
                SelectorUpdate::Decide(ApprovalOutcome::Cancelled)
            }
            InputEvent::Key(Key::Enter) => self.confirm(challenge),
            InputEvent::Key(Key::Newline) if self.profile == ApprovalInputProfile::LinearRecord => {
                self.confirm(challenge)
            }
            InputEvent::Key(Key::Eof) => SelectorUpdate::Eof,
            InputEvent::Key(Key::Tab)
                if self.profile == ApprovalInputProfile::LinearRecord && self.record_len == 0 =>
            {
                self.select(self.selected.next(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::Up | Key::Left) if self.record_len == 0 => {
                self.select(self.selected.previous(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::Down | Key::Right) if self.record_len == 0 => {
                self.select(self.selected.next(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::BackTab)
                if self.profile == ApprovalInputProfile::LinearRecord && self.record_len == 0 =>
            {
                self.select(self.selected.previous(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::Char('h' | 'k')) if self.record_len == 0 => {
                if self.profile == ApprovalInputProfile::EnhancedDirectional {
                    return SelectorUpdate::Invalid;
                }
                self.select(self.selected.previous(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::Char('j' | 'l')) if self.record_len == 0 => {
                if self.profile == ApprovalInputProfile::EnhancedDirectional {
                    return SelectorUpdate::Invalid;
                }
                self.select(self.selected.next(self.allow_exact_shell));
                SelectorUpdate::Redraw
            }
            InputEvent::Key(Key::Backspace) if self.record_len != 0 => {
                self.record_len -= 1;
                self.update_shortcut_selection()
            }
            InputEvent::Key(Key::Char(character))
                if character.is_ascii_graphic() || character == ' ' =>
            {
                if self.profile == ApprovalInputProfile::EnhancedDirectional {
                    return SelectorUpdate::Invalid;
                }
                if self.record_len == self.record.len() {
                    self.record_len = 0;
                    return SelectorUpdate::Invalid;
                }
                self.record[self.record_len] = character as u8;
                self.record_len += 1;
                self.update_shortcut_selection()
            }
            // A paste is rejected only after its closing marker. Staying in
            // the decoder's Paste state prevents a fragmented tail from being
            // reinterpreted as arrows or Enter after the modal is re-armed.
            InputEvent::PasteStarted => {
                self.draining_rejected_paste = true;
                SelectorUpdate::None
            }
            InputEvent::Paste(_) if self.draining_rejected_paste => {
                self.draining_rejected_paste = false;
                self.record_len = 0;
                SelectorUpdate::Invalid
            }
            InputEvent::PasteRejected(_) if self.draining_rejected_paste => {
                self.draining_rejected_paste = false;
                self.record_len = 0;
                SelectorUpdate::Invalid
            }
            InputEvent::Rejected(InputError::SequenceTooLong) => {
                if self.draining_rejected_sequence {
                    self.draining_rejected_sequence = false;
                    self.record_len = 0;
                    SelectorUpdate::Invalid
                } else {
                    self.draining_rejected_sequence = true;
                    SelectorUpdate::None
                }
            }
            InputEvent::Rejected(_) if self.draining_rejected_paste => {
                self.draining_rejected_paste = false;
                self.record_len = 0;
                SelectorUpdate::Invalid
            }
            InputEvent::Paste(_) | InputEvent::PasteRejected(_) | InputEvent::Rejected(_) => {
                self.record_len = 0;
                SelectorUpdate::Invalid
            }
            _ => SelectorUpdate::Invalid,
        }
    }

    fn select(&mut self, selected: ApprovalSelection) {
        self.selected = selected;
        self.allow_focus_serial = matches!(
            selected,
            ApprovalSelection::AllowOnce | ApprovalSelection::AllowExactShellForProcess
        )
        .then_some(self.feed_serial);
    }

    fn update_shortcut_selection(&mut self) -> SelectorUpdate {
        let selected = match &self.record[..self.record_len] {
            b"y" | b"yes" | b"allow" => Some(ApprovalSelection::AllowOnce),
            b"n" | b"no" | b"reject" => Some(ApprovalSelection::Reject),
            b"c" | b"cancel" => Some(ApprovalSelection::Cancel),
            _ => None,
        };
        if let Some(selected) = selected {
            let changed = self.selected != selected;
            self.select(selected);
            if changed {
                return SelectorUpdate::Redraw;
            }
        }
        SelectorUpdate::None
    }

    fn confirm(&mut self, challenge: uuid::Uuid) -> SelectorUpdate {
        if self.record_len == 0 {
            let requires_fresh_input = self.selected
                == ApprovalSelection::AllowExactShellForProcess
                || (self.profile == ApprovalInputProfile::EnhancedDirectional
                    && self.selected == ApprovalSelection::AllowOnce);
            if requires_fresh_input
                && self
                    .allow_focus_serial
                    .is_none_or(|serial| serial >= self.feed_serial)
            {
                self.select(ApprovalSelection::Reject);
                return SelectorUpdate::Invalid;
            }
            return self
                .selected
                .outcome()
                .map_or(SelectorUpdate::RememberExactShell, SelectorUpdate::Decide);
        }
        let record = std::str::from_utf8(&self.record[..self.record_len]);
        self.record_len = 0;
        match record
            .ok()
            .map(|record| parse_approval_answer(record, true, challenge))
        {
            Some(ApprovalAnswer::Decide(outcome)) => SelectorUpdate::Decide(outcome),
            Some(ApprovalAnswer::Retry) | None => SelectorUpdate::Invalid,
        }
    }
}

fn push_selector_line(
    output: &mut String,
    clear: bool,
    line: &str,
) -> Result<(), SelectorRenderError> {
    if clear {
        output.push_str("\r\x1b[2K");
    }
    writeln!(output, "{line}").map_err(|_| SelectorRenderError)
}

#[cfg(test)]
mod tests {
    use super::{ApprovalInputProfile, ApprovalSelection, ApprovalSelector, SelectorUpdate};
    use crate::session::ApprovalOutcome;

    fn challenge() -> uuid::Uuid {
        uuid::Uuid::parse_str("00112233-4455-4677-8899-aabbccddeeff").unwrap()
    }

    fn linear_selector() -> ApprovalSelector {
        ApprovalSelector::new(ApprovalInputProfile::LinearRecord).unwrap()
    }

    #[test]
    fn exact_shell_process_choice_is_explicit_request_scoped_and_fresh() {
        let mut ordinary = linear_selector();
        assert_eq!(
            ordinary.feed(b"\x1b[B", challenge()),
            SelectorUpdate::Redraw
        );
        assert_eq!(ordinary.selected(), ApprovalSelection::Cancel);

        let mut selector =
            ApprovalSelector::new_for_request(ApprovalInputProfile::EnhancedDirectional, true)
                .unwrap();
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
        assert_eq!(
            selector.feed(b"\x1b[B\r", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(selector.selected(), ApprovalSelection::Reject);

        let mut selector =
            ApprovalSelector::new_for_request(ApprovalInputProfile::EnhancedDirectional, true)
                .unwrap();
        assert_eq!(
            selector.feed(b"\x1b[B", challenge()),
            SelectorUpdate::Redraw
        );
        assert_eq!(
            selector.selected(),
            ApprovalSelection::AllowExactShellForProcess
        );
        assert_eq!(
            selector.feed(b"\r", challenge()),
            SelectorUpdate::RememberExactShell
        );

        let mut linear =
            ApprovalSelector::new_for_request(ApprovalInputProfile::LinearRecord, true).unwrap();
        assert_eq!(
            linear.feed(b"\x1b[B\r", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(linear.selected(), ApprovalSelection::Reject);
    }

    #[test]
    fn exact_shell_process_render_has_a_dynamic_redraw_height() {
        let selector =
            ApprovalSelector::new_for_request(ApprovalInputProfile::LinearRecord, true).unwrap();
        let plain = selector.render(false, false, false).unwrap();
        assert!(plain.contains("Allow exact Shell for this process"));
        let styled = selector.render(true, false, true).unwrap();
        assert!(styled.starts_with("\x1b[6A"));

        let ordinary = linear_selector().render(false, false, false).unwrap();
        assert!(!ordinary.contains("exact Shell"));
    }

    #[test]
    fn reject_is_the_safe_default_and_enter_is_the_only_confirmation() {
        let mut selector = linear_selector();
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
        assert_eq!(selector.feed(b"y", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::AllowOnce);
        assert_eq!(
            selector.feed(b"\n", challenge()),
            SelectorUpdate::Decide(ApprovalOutcome::AllowedOnce)
        );

        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(b"\n", challenge()),
            SelectorUpdate::Decide(ApprovalOutcome::Rejected)
        );
    }

    #[test]
    fn fragmented_arrows_tab_and_vim_keys_move_without_authorizing() {
        let mut selector = linear_selector();
        assert_eq!(selector.feed(b"\x1b", challenge()), SelectorUpdate::None);
        assert!(selector.escape_is_pending());
        assert_eq!(selector.feed(b"[", challenge()), SelectorUpdate::None);
        assert_eq!(selector.feed(b"A", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::AllowOnce);
        assert_eq!(selector.feed(b"\t", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
        assert_eq!(
            selector.feed(b"\x1b[B", challenge()),
            SelectorUpdate::Redraw
        );
        assert_eq!(selector.selected(), ApprovalSelection::Cancel);
        assert_eq!(selector.feed(b"k", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
        assert_eq!(selector.feed(b"h", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::AllowOnce);
        assert_eq!(selector.feed(b"j", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
        assert_eq!(selector.feed(b"l", challenge()), SelectorUpdate::Redraw);
        assert_eq!(selector.selected(), ApprovalSelection::Cancel);
    }

    #[test]
    fn isolated_escape_cancels_and_unknown_sequences_fail_closed() {
        let mut selector = linear_selector();
        assert_eq!(selector.feed(b"\x1b", challenge()), SelectorUpdate::None);
        assert_eq!(
            selector.expire_escape(),
            SelectorUpdate::Decide(ApprovalOutcome::Cancelled)
        );

        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(b"\x1b[200~y\n\x1b[201~", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(selector.selected(), ApprovalSelection::Reject);

        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(b"\x1b[999~\x1b[C\r", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(selector.selected(), ApprovalSelection::Reject);

        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(b"\x1by\r", challenge()),
            SelectorUpdate::Decide(ApprovalOutcome::Cancelled)
        );
    }

    #[test]
    fn ctrl_d_remains_an_explicit_eof_in_cbreak_mode() {
        let mut selector = linear_selector();
        assert_eq!(selector.feed(&[0x04], challenge()), SelectorUpdate::Eof);
    }

    #[test]
    fn exact_automation_records_remain_bounded_and_correlated() {
        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(b"allow 00112233-4455-4677-8899-aabbccddeeff\n", challenge(),),
            SelectorUpdate::Decide(ApprovalOutcome::AllowedOnce)
        );

        let mut selector = linear_selector();
        assert_eq!(
            selector.feed(&[b'x'; 65], challenge()),
            SelectorUpdate::Invalid
        );
    }

    #[test]
    fn enhanced_directional_mode_requires_a_later_enter_and_drains_paste() {
        let mut selector =
            ApprovalSelector::new(ApprovalInputProfile::EnhancedDirectional).unwrap();
        assert_eq!(selector.feed(b"y\r", challenge()), SelectorUpdate::Invalid);
        assert_eq!(selector.selected(), ApprovalSelection::Reject);

        let mut selector =
            ApprovalSelector::new(ApprovalInputProfile::EnhancedDirectional).unwrap();
        assert_eq!(
            selector.feed(b"\x1b[A\r", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(selector.selected(), ApprovalSelection::Reject);

        let mut selector =
            ApprovalSelector::new(ApprovalInputProfile::EnhancedDirectional).unwrap();
        assert_eq!(
            selector.feed(b"\x1b[A", challenge()),
            SelectorUpdate::Redraw
        );
        assert_eq!(selector.selected(), ApprovalSelection::AllowOnce);
        assert_eq!(selector.feed(b"\n", challenge()), SelectorUpdate::Invalid);

        let mut selector =
            ApprovalSelector::new(ApprovalInputProfile::EnhancedDirectional).unwrap();
        assert_eq!(
            selector.feed(b"\x1b[A", challenge()),
            SelectorUpdate::Redraw
        );
        assert_eq!(
            selector.feed(b"\r", challenge()),
            SelectorUpdate::Decide(ApprovalOutcome::AllowedOnce)
        );

        let mut selector =
            ApprovalSelector::new(ApprovalInputProfile::EnhancedDirectional).unwrap();
        assert_eq!(
            selector.feed(b"\x1b[200~", challenge()),
            SelectorUpdate::None
        );
        assert_eq!(
            selector.feed(b"\x1b[A\r", challenge()),
            SelectorUpdate::None
        );
        assert_eq!(
            selector.feed(b"\x1b[201~", challenge()),
            SelectorUpdate::Invalid
        );
        assert_eq!(selector.selected(), ApprovalSelection::Reject);
    }

    #[test]
    fn styled_redraw_is_product_owned_and_plain_output_has_no_escape_bytes() {
        let mut selector = linear_selector();
        let plain = selector.render(false, false, false).unwrap();
        assert!(!plain.contains('\x1b'));
        assert!(plain.contains("[x] Reject"));
        assert!(plain.contains("Enter confirm"));

        assert_eq!(selector.feed(b"y", challenge()), SelectorUpdate::Redraw);
        let styled = selector.render(true, false, true).unwrap();
        assert!(styled.starts_with("\x1b[5A"));
        assert!(styled.contains("› Allow once"));
        assert!(styled.ends_with("\x1b[0m\n"));

        let narrow = selector.render(true, true, false).unwrap();
        assert!(!narrow.contains("\x1b[5A"));
        assert!(narrow.contains("Enter confirm"));
    }
}
