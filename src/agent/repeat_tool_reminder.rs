use serde_json::Value;

use crate::model::JsonValue;

const THRESHOLDS: [u32; 3] = [3, 5, 8];
const ARGUMENT_PREVIEW_CHARS: usize = 500;

const GENTLE_REMINDER: &str = "You are repeating the exact same tool call with identical arguments. Carefully analyze the previous result before calling again: if the task is not complete, try a different approach or different arguments instead of repeating the call.";

#[derive(Default)]
pub(super) struct RepeatToolReminder {
    chain: Option<RepeatChain>,
}

struct RepeatChain {
    tool_name: String,
    canonical_arguments: String,
    count: u32,
}

pub(super) struct RepeatToolNotice {
    text: String,
    summary: String,
}

impl RepeatToolNotice {
    pub(super) fn text(self) -> String {
        self.text
    }

    pub(super) fn summary(&self) -> &str {
        &self.summary
    }
}

impl RepeatToolReminder {
    pub(super) fn reset(&mut self) {
        self.chain = None;
    }

    pub(super) fn observe(
        &mut self,
        tool_name: &str,
        raw_arguments: &str,
    ) -> Result<Option<RepeatToolNotice>, serde_json::Error> {
        let canonical_arguments = canonicalize_arguments(raw_arguments)?;
        let count = match &self.chain {
            Some(chain)
                if chain.tool_name == tool_name
                    && chain.canonical_arguments == canonical_arguments =>
            {
                chain.count.saturating_add(1)
            }
            _ => 1,
        };
        self.chain = Some(RepeatChain {
            tool_name: tool_name.to_owned(),
            canonical_arguments: canonical_arguments.clone(),
            count,
        });

        if !THRESHOLDS.contains(&count) {
            return Ok(None);
        }
        let text = if count == THRESHOLDS[0] {
            GENTLE_REMINDER.to_owned()
        } else {
            let preview = preview_arguments(&canonical_arguments, ARGUMENT_PREVIEW_CHARS);
            format!(
                "Repeated tool call detected:\n- tool: {tool_name}\n- consecutive_calls: {count}\n- arguments: {preview}\nThe repeated calls are not making progress. Do not call this tool with these exact arguments again. Inspect the latest result and choose a different action, different arguments, or finish the task if enough evidence has been gathered."
            )
        };
        Ok(Some(RepeatToolNotice {
            text,
            summary: format!("{tool_name} × {count}"),
        }))
    }
}

fn canonicalize_arguments(raw_arguments: &str) -> Result<String, serde_json::Error> {
    let raw_arguments = if raw_arguments.is_empty() {
        "{}"
    } else {
        raw_arguments
    };
    match serde_json::from_str::<Value>(raw_arguments)
        .ok()
        .and_then(|value| JsonValue::new(value).ok())
    {
        Some(value) => serde_json::to_string(value.as_value()),
        None => serde_json::to_string(raw_arguments),
    }
}

fn preview_arguments(canonical: &str, cap: usize) -> String {
    let total = canonical.chars().count();
    if total <= cap {
        return canonical.to_owned();
    }
    let end = canonical
        .char_indices()
        .nth(cap)
        .map_or(canonical.len(), |(index, _)| index);
    format!("{}… (+{} more chars)", &canonical[..end], total - cap)
}

#[cfg(test)]
mod tests {
    use super::{
        ARGUMENT_PREVIEW_CHARS, GENTLE_REMINDER, RepeatToolReminder, canonicalize_arguments,
        preview_arguments,
    };

    #[test]
    fn canonicalization_is_deep_and_uses_the_raw_string_fallback() {
        assert_eq!(
            canonicalize_arguments(r#"{"z":{"b":2,"a":1},"a":1.0}"#).unwrap(),
            r#"{"a":1,"z":{"a":1,"b":2}}"#
        );
        assert_eq!(canonicalize_arguments("not json").unwrap(), r#""not json""#);
        assert_eq!(canonicalize_arguments("").unwrap(), "{}");
    }

    #[test]
    fn default_thresholds_escalate_and_other_counts_stay_silent() {
        let mut reminder = RepeatToolReminder::default();
        assert!(
            reminder
                .observe("read", r#"{"path":"a"}"#)
                .unwrap()
                .is_none()
        );
        assert!(
            reminder
                .observe("read", r#"{"path":"a"}"#)
                .unwrap()
                .is_none()
        );
        let third = reminder
            .observe("read", r#"{"path":"a"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(third.text, GENTLE_REMINDER);
        assert_eq!(third.summary, "read × 3");
        assert!(
            reminder
                .observe("read", r#"{"path":"a"}"#)
                .unwrap()
                .is_none()
        );
        let fifth = reminder
            .observe("read", r#"{"path":"a"}"#)
            .unwrap()
            .unwrap();
        assert!(fifth.text.contains("consecutive_calls: 5"));
        assert!(fifth.text.contains(r#"- arguments: {"path":"a"}"#));
        for _ in 0..2 {
            assert!(
                reminder
                    .observe("read", r#"{"path":"a"}"#)
                    .unwrap()
                    .is_none()
            );
        }
        let eighth = reminder
            .observe("read", r#"{"path":"a"}"#)
            .unwrap()
            .unwrap();
        assert!(eighth.text.contains("consecutive_calls: 8"));
        assert!(
            reminder
                .observe("read", r#"{"path":"a"}"#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reset_and_a_different_call_restart_the_chain() {
        let mut reminder = RepeatToolReminder::default();
        for _ in 0..2 {
            assert!(reminder.observe("read", "{}").unwrap().is_none());
        }
        reminder.reset();
        assert!(reminder.observe("read", "{}").unwrap().is_none());
        assert!(reminder.observe("grep", "{}").unwrap().is_none());
        assert!(reminder.observe("read", "{}").unwrap().is_none());
    }

    #[test]
    fn preview_is_unicode_safe_and_reports_omitted_scalars() {
        assert_eq!(preview_arguments("a😀bc", 2), "a😀… (+2 more chars)");
        assert_eq!(preview_arguments("short", 5), "short");
        assert_eq!(
            preview_arguments(
                &"x".repeat(ARGUMENT_PREVIEW_CHARS + 7),
                ARGUMENT_PREVIEW_CHARS
            ),
            format!("{}… (+7 more chars)", "x".repeat(ARGUMENT_PREVIEW_CHARS))
        );
    }
}
