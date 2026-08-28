use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::protocol::{LspLocation, LspOperation, LspPosition, LspResult};

pub(super) const MAX_LSP_LOCATIONS: usize = 100;
pub(super) const MAX_LSP_RESULT_CHARS: usize = 16_000;
const MAX_LSP_FILE_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LspArguments {
    pub(super) operation: LspOperation,
    pub(super) file_path: String,
    pub(super) position: LspPosition,
}

pub(super) fn parse_arguments(value: &Value) -> Result<LspArguments, String> {
    let fields = value
        .as_object()
        .ok_or_else(|| "lsp arguments must be an object".to_owned())?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "operation" | "file_path" | "line" | "character"
        )
    }) {
        return Err("lsp received an unknown argument".to_owned());
    }
    let operation = fields
        .get("operation")
        .and_then(Value::as_str)
        .and_then(LspOperation::parse)
        .ok_or_else(|| {
            "operation must be goToDefinition, findReferences, goToImplementation, or hover"
                .to_owned()
        })?;
    let file_path = fields
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= MAX_LSP_FILE_PATH_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| "file_path must be a bounded non-empty string".to_owned())?
        .to_owned();
    let line = one_based(fields.get("line"), "line")?;
    let character = one_based(fields.get("character"), "character")?;
    Ok(LspArguments {
        operation,
        file_path,
        position: LspPosition {
            line: line - 1,
            character: character - 1,
        },
    })
}

fn one_based(value: Option<&Value>, name: &str) -> Result<u32, String> {
    let value = value
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{name} must be a positive one-based integer"))?;
    Ok(value)
}

pub(super) fn render_result(result: &LspResult, workspace: &Path) -> String {
    match result {
        LspResult::Locations(locations) => format_locations(locations, workspace),
        LspResult::Hover(None) => "No hover information.".to_owned(),
        LspResult::Hover(Some(hover)) => bound_result(&hover.contents, "hover"),
    }
}

fn format_locations(locations: &[LspLocation], workspace: &Path) -> String {
    if locations.is_empty() {
        return "No results.".to_owned();
    }
    let shown = locations.len().min(MAX_LSP_LOCATIONS);
    let mut lines = Vec::new();
    let _ = lines.try_reserve_exact(shown.saturating_add(1));
    for location in &locations[..shown] {
        lines.push(format!(
            "{}:{}:{}",
            render_uri(&location.uri, workspace),
            location.range.start.line.saturating_add(1),
            location.range.start.character.saturating_add(1)
        ));
    }
    let omitted = locations.len().saturating_sub(shown);
    if omitted > 0 {
        lines.push(format!(
            "… {omitted} more location{} omitted (limit {MAX_LSP_LOCATIONS}).",
            if omitted == 1 { "" } else { "s" }
        ));
    }
    bound_result(&lines.join("\n"), "locations")
}

fn bound_result(text: &str, label: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_LSP_RESULT_CHARS {
        return text.to_owned();
    }
    let notice = format!("\n… {label} truncated (limit {MAX_LSP_RESULT_CHARS} characters).");
    let notice_chars = notice.chars().count();
    if notice_chars >= MAX_LSP_RESULT_CHARS {
        return notice.chars().take(MAX_LSP_RESULT_CHARS).collect();
    }
    let prefix: String = text
        .chars()
        .take(MAX_LSP_RESULT_CHARS - notice_chars)
        .collect();
    format!("{prefix}{notice}")
}

pub(super) fn file_uri(path: &Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let bytes = path.as_os_str().as_bytes();
    let mut uri = String::from("file://");
    uri.try_reserve(bytes.len().saturating_mul(3)).ok()?;
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut uri, "%{byte:02X}").ok()?;
        }
    }
    Some(uri)
}

fn render_uri(uri: &str, workspace: &Path) -> String {
    let Some(path) = decode_file_uri(uri) else {
        return uri.to_owned();
    };
    let display = path
        .strip_prefix(workspace)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path.as_path());
    display
        .to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| uri.to_owned())
}

fn decode_file_uri(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(bytes.len()).ok()?;
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex(high)?.checked_mul(16)?.checked_add(hex(low)?)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    if decoded.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_LSP_RESULT_CHARS, file_uri, parse_arguments, render_result};
    use crate::tools::lsp::protocol::{LspHover, LspLocation, LspPosition, LspRange, LspResult};

    #[test]
    fn arguments_are_closed_and_convert_one_based_coordinates() {
        let parsed = parse_arguments(&serde_json::json!({
            "operation":"hover","file_path":"src/é.rs","line":3,"character":5
        }))
        .unwrap();
        assert_eq!(
            parsed.position,
            LspPosition {
                line: 2,
                character: 4
            }
        );
        assert!(
            parse_arguments(&serde_json::json!({
                "operation":"rename","file_path":"a.rs","line":1,"character":1
            }))
            .is_err()
        );
        assert!(
            parse_arguments(&serde_json::json!({
                "operation":"hover","file_path":"a.rs","line":0,"character":1
            }))
            .is_err()
        );
        assert!(
            parse_arguments(&serde_json::json!({
                "operation":"hover","file_path":"a.rs","line":1,"character":1,"extra":true
            }))
            .is_err()
        );
    }

    #[test]
    fn locations_render_relative_percent_decoded_one_based_paths() {
        let workspace = std::path::Path::new("/tmp/space root");
        let result = LspResult::Locations(vec![LspLocation {
            uri: "file:///tmp/space%20root/src/a.rs".to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 2,
                    character: 3,
                },
                end: LspPosition {
                    line: 2,
                    character: 4,
                },
            },
        }]);
        assert_eq!(render_result(&result, workspace), "src/a.rs:3:4");
        assert_eq!(
            file_uri(workspace).as_deref(),
            Some("file:///tmp/space%20root")
        );
    }

    #[test]
    fn hover_and_locations_keep_complete_character_bound() {
        assert_eq!(
            render_result(&LspResult::Hover(None), std::path::Path::new("/w")),
            "No hover information."
        );
        let text = "界".repeat(MAX_LSP_RESULT_CHARS + 100);
        let rendered = render_result(
            &LspResult::Hover(Some(LspHover {
                contents: text,
                range: None,
            })),
            std::path::Path::new("/w"),
        );
        assert_eq!(rendered.chars().count(), MAX_LSP_RESULT_CHARS);
        assert!(rendered.ends_with("characters)."));
    }
}
