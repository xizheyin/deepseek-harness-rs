//! Bounded, terminal-safe Session title facts.

use serde::{Deserialize, Serialize};

use crate::model::{ContentBlockKind, Message, MessageSourceKind, NonNegativeSafeInteger};

use super::{EventSeq, MAX_SOURCE_EVENT_SEQS, error::EventValidationError};

pub const FALLBACK_TITLE_MAX_WORDS: usize = 5;
pub const FALLBACK_TITLE_MAX_BYTES: usize = 40;
pub const PROVIDER_TITLE_MAX_BYTES: usize = 80;
pub const TITLE_INPUT_MAX_BYTES: usize = 4_096;
pub const TITLE_OUTPUT_MAX_TOKENS: u64 = 64;

/// Capture only the bounded text the shipped first-prompt title provider may
/// receive. Keeping this in the projection lets a resumed durable Session
/// refresh its title without retaining the complete historical event body.
pub(crate) fn title_prompt_text(message: &Message) -> Option<String> {
    if !matches!(message.source().kind(), MessageSourceKind::User) {
        return None;
    }
    let mut text = String::new();
    text.try_reserve(TITLE_INPUT_MAX_BYTES).ok()?;
    for block in message.content() {
        let ContentBlockKind::Text { text: part } = block.kind() else {
            continue;
        };
        if !text.is_empty() && text.len() < TITLE_INPUT_MAX_BYTES {
            text.push('\n');
        }
        let remaining = TITLE_INPUT_MAX_BYTES.saturating_sub(text.len());
        if remaining == 0 {
            break;
        }
        let mut end = remaining.min(part.len());
        while end != 0 && !part.is_char_boundary(end) {
            end -= 1;
        }
        text.push_str(&part[..end]);
        if text.len() == TITLE_INPUT_MAX_BYTES {
            break;
        }
    }
    (!text.trim().is_empty()).then_some(text)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum SessionTitleSource {
    Fallback,
    Provider {
        provider: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    User,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTitleEvent {
    title: String,
    message_seqs: Vec<EventSeq>,
    source: SessionTitleSource,
}

impl SessionTitleEvent {
    pub fn new(
        title: impl Into<String>,
        message_seqs: Vec<EventSeq>,
        source: SessionTitleSource,
    ) -> Result<Self, EventValidationError> {
        let value = Self {
            title: title.into(),
            message_seqs,
            source,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), EventValidationError> {
        if normalize_title(&self.title, PROVIDER_TITLE_MAX_BYTES).as_deref()
            != Some(self.title.as_str())
        {
            return Err(EventValidationError::InvalidTitleEvent(
                "title must be canonical, non-empty, and at most 80 UTF-8 bytes",
            ));
        }
        if self.message_seqs.len() > MAX_SOURCE_EVENT_SEQS
            || self.message_seqs.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(EventValidationError::InvalidTitleEvent(
                "messageSeqs must be a bounded strictly increasing sequence",
            ));
        }
        if !matches!(self.source, SessionTitleSource::User) && self.message_seqs.is_empty() {
            return Err(EventValidationError::InvalidTitleEvent(
                "automatic titles require at least one source message",
            ));
        }
        if let SessionTitleSource::Provider { provider, model } = &self.source {
            if !valid_route_part(provider)
                || model.as_ref().is_some_and(|value| !valid_route_part(value))
            {
                return Err(EventValidationError::InvalidTitleEvent(
                    "provider title attribution is invalid",
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn message_seqs(&self) -> &[EventSeq] {
        &self.message_seqs
    }

    #[must_use]
    pub fn source(&self) -> &SessionTitleSource {
        &self.source
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTitleRoute {
    provider: String,
    model: String,
}

impl SessionTitleRoute {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, EventValidationError> {
        let value = Self {
            provider: provider.into(),
            model: model.into(),
        };
        if !valid_route_part(&value.provider) || !valid_route_part(&value.model) {
            return Err(EventValidationError::InvalidTitleEvent(
                "title route is invalid",
            ));
        }
        Ok(value)
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionTitleLlmRequestEvent {
    title_provider: String,
    message_seqs: Vec<EventSeq>,
    route: SessionTitleRoute,
    system: String,
    messages: Vec<Message>,
    max_tokens: NonNegativeSafeInteger,
}

impl SessionTitleLlmRequestEvent {
    pub fn new(
        title_provider: impl Into<String>,
        message_seqs: Vec<EventSeq>,
        route: SessionTitleRoute,
        system: impl Into<String>,
        messages: Vec<Message>,
        max_tokens: NonNegativeSafeInteger,
    ) -> Result<Self, EventValidationError> {
        let value = Self {
            title_provider: title_provider.into(),
            message_seqs,
            route,
            system: system.into(),
            messages,
            max_tokens,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), EventValidationError> {
        if !valid_route_part(&self.title_provider) {
            return Err(EventValidationError::InvalidTitleEvent(
                "title provider is invalid",
            ));
        }
        if self.message_seqs.is_empty()
            || self.message_seqs.len() > MAX_SOURCE_EVENT_SEQS
            || self.message_seqs.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(EventValidationError::InvalidTitleEvent(
                "title request sources are invalid",
            ));
        }
        if self.system.is_empty() || self.system.len() > 8 * 1_024 || self.messages.len() != 1 {
            return Err(EventValidationError::InvalidTitleEvent(
                "title request shape is invalid",
            ));
        }
        if self.messages[0].validate_user_event().is_err()
            || self.max_tokens.get() == 0
            || self.max_tokens.get() > TITLE_OUTPUT_MAX_TOKENS
        {
            return Err(EventValidationError::InvalidTitleEvent(
                "title request payload is invalid",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn route(&self) -> &SessionTitleRoute {
        &self.route
    }
}

fn valid_route_part(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1_024 && !value.chars().any(char::is_control)
}

/// Official first-prompt fallback: five words, then a 40-byte safe title.
#[must_use]
pub fn fallback_title(input: &str) -> Option<String> {
    let words = input
        .split_whitespace()
        .take(FALLBACK_TITLE_MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    normalize_title(&words, FALLBACK_TITLE_MAX_BYTES)
}

/// Remove terminal/invisible controls, collapse whitespace, and truncate at a
/// UTF-8 boundary so title text is safe in logs and terminal rows.
#[must_use]
pub fn normalize_title(input: &str, maximum_bytes: usize) -> Option<String> {
    let mut clean = String::new();
    let mut chars = input.chars().peekable();
    let mut pending_space = false;
    while let Some(character) = chars.next() {
        if character == '\u{1b}' || character == '\u{009b}' || character == '\u{009d}' {
            skip_escape(character, &mut chars);
            continue;
        }
        if invisible(character) || (character.is_control() && !character.is_whitespace()) {
            continue;
        }
        if character.is_whitespace() {
            pending_space = !clean.is_empty();
            continue;
        }
        if pending_space {
            clean.push(' ');
            pending_space = false;
        }
        clean.push(character);
    }
    if clean.len() > maximum_bytes {
        let mut end = maximum_bytes.min(clean.len());
        while end != 0 && !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
        while clean.ends_with(' ') {
            clean.pop();
        }
    }
    (!clean.is_empty()).then_some(clean)
}

fn skip_escape(first: char, chars: &mut std::iter::Peekable<impl Iterator<Item = char>>) {
    let kind = if first == '\u{1b}' {
        chars.next()
    } else {
        Some(first)
    };
    match kind {
        Some('[' | '\u{009b}') => {
            for character in chars.by_ref() {
                if ('@'..='~').contains(&character) {
                    break;
                }
            }
        }
        Some(']' | '\u{009d}') => {
            let mut escaped = false;
            for character in chars.by_ref() {
                if character == '\u{7}' || (escaped && character == '\\') {
                    break;
                }
                escaped = character == '\u{1b}';
            }
        }
        Some(_) | None => {}
    }
}

fn invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{115f}' | '\u{1160}'
            | '\u{17b4}' | '\u{17b5}' | '\u{180e}' | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{3164}'
            | '\u{feff}' | '\u{ffa0}'
    )
}

#[cfg(test)]
mod tests {
    use super::{TITLE_INPUT_MAX_BYTES, fallback_title, normalize_title, title_prompt_text};
    use crate::model::{ContentBlock, Message, MessageSource};

    #[test]
    fn fallback_and_normalization_are_bounded_and_terminal_safe() {
        assert_eq!(
            fallback_title("  one  two three four five six  ").as_deref(),
            Some("one two three four five")
        );
        assert_eq!(
            normalize_title("hello\u{1b}]52;c;secret\u{7}\n world\u{202e}", 80).as_deref(),
            Some("hello world")
        );
        assert_eq!(
            normalize_title("你好世界测试标题", 7).as_deref(),
            Some("你好")
        );
    }

    #[test]
    fn first_prompt_text_is_direct_human_text_and_utf8_bounded() {
        let message = Message::user(
            "title-prompt",
            vec![
                ContentBlock::text("first").unwrap(),
                ContentBlock::text(format!("{}尾", "x".repeat(TITLE_INPUT_MAX_BYTES))).unwrap(),
            ],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        let text = title_prompt_text(&message).unwrap();
        assert_eq!(text.len(), TITLE_INPUT_MAX_BYTES);
        assert!(text.starts_with("first\n"));
        assert!(text.is_char_boundary(text.len()));

        let generated = Message::user(
            "generated",
            vec![ContentBlock::text("not direct").unwrap()],
            MessageSource::plugin("test").unwrap(),
        )
        .unwrap();
        assert_eq!(title_prompt_text(&generated), None);
    }
}
