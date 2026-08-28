use std::{collections::VecDeque, path::Path};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    agent::{ToolExecutionResult, ToolExecutorError, ToolPreparation},
    model::ContentBlock,
};

use super::{
    MAX_TOOL_CONTENT_BYTES, MAX_TRAVERSAL_PATH_BYTES,
    error::{ToolCallError, ToolCallResult},
    json_string_content_bytes,
    patch::{self, GeneratedTextMutation},
    text_block_encoded_bytes,
    workspace::{EntryKind, ResolvedPath, Workspace, WorkspaceMutationOperation},
};

pub(crate) const TOOL_NAME: &str = "str_replace_editor";
const MAX_OUTPUT_CHARS: usize = 16_000;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_VIEW_LINES: usize = 100_000;
const CLIPPED_MESSAGE: &str = "<response clipped><NOTE>To save on context only part of this file has been shown to you. You should retry this tool after you have searched inside the file with `grep -n` in order to find the line numbers of what you are looking for.</NOTE>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorCommand {
    View,
    Create,
    StrReplace,
    Insert,
}

struct EditorArgs {
    command: EditorCommand,
    path: String,
    file_text: Option<String>,
    insert_line: Option<usize>,
    new_str: Option<String>,
    old_str: Option<String>,
    view_range: Option<(i64, i64)>,
}

pub(crate) async fn execute(
    workspace: &Workspace,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    let parsed = match parse_arguments(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_execution_result(),
    };
    if parsed.command != EditorCommand::View {
        return ToolCallError::model(
            "ApprovalError",
            "APPROVAL_REQUIRED",
            "str_replace_editor mutations must use the Agent approval preparation stage",
        )
        .into_execution_result();
    }
    view_result(workspace, &parsed, cancellation).await
}

pub(crate) async fn prepare(
    workspace: &Workspace,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<ToolPreparation, ToolExecutorError> {
    let parsed = match parse_arguments(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_execution_result().map(ToolPreparation::Complete),
    };
    match parsed.command {
        EditorCommand::View => view_result(workspace, &parsed, cancellation)
            .await
            .map(ToolPreparation::Complete),
        EditorCommand::Create => {
            let Some(file_text) = parsed.file_text else {
                return ToolCallError::invalid_args(
                    "Parameter `file_text` is required for command: create",
                )
                .into_execution_result()
                .map(ToolPreparation::Complete);
            };
            let success_path = parsed.path.clone();
            patch::prepare_text_mutation(
                workspace,
                &parsed.path,
                WorkspaceMutationOperation::Create,
                cancellation,
                move |_| {
                    Ok(GeneratedTextMutation {
                        candidate: file_text,
                        success_message: format!(
                            "New file created successfully at: {success_path}"
                        ),
                    })
                },
            )
            .await
        }
        EditorCommand::StrReplace => {
            let Some(old_str) = parsed.old_str else {
                return ToolCallError::invalid_args(
                    "Parameter `old_str` is required for command: str_replace",
                )
                .into_execution_result()
                .map(ToolPreparation::Complete);
            };
            if old_str.is_empty() {
                return ToolCallError::invalid_args(
                    "Parameter `old_str` is empty for command: str_replace",
                )
                .into_execution_result()
                .map(ToolPreparation::Complete);
            }
            let old_str = normalize_argument_newlines(old_str);
            let new_str = normalize_argument_newlines(parsed.new_str.unwrap_or_default());
            let success_path = parsed.path.clone();
            let error_path = parsed.path.clone();
            patch::prepare_text_mutation(
                workspace,
                &parsed.path,
                WorkspaceMutationOperation::Update,
                cancellation,
                move |before| {
                    let offsets = match_offsets(before, &old_str);
                    let Some(offset) = offsets.first().copied() else {
                        return Err(ToolCallError::model(
                            "FsError",
                            "FS_EDIT_NOT_FOUND",
                            format!(
                                "No replacement was performed, old_str `{old_str}` did not appear verbatim in {error_path}."
                            ),
                        ));
                    };
                    if offsets.len() > 1 {
                        let lines = offsets
                            .iter()
                            .map(|offset| {
                                before[..*offset]
                                    .bytes()
                                    .filter(|byte| *byte == b'\n')
                                    .count()
                                    + 1
                            })
                            .collect::<Vec<_>>();
                        return Err(ToolCallError::model(
                            "FsError",
                            "FS_AMBIGUOUS_EDIT",
                            format!(
                                "No replacement was performed. Multiple occurrences of old_str `{old_str}` in lines [{}]. Please ensure it is unique",
                                lines
                                    .iter()
                                    .map(usize::to_string)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        ));
                    }
                    let mut candidate = String::with_capacity(
                        before
                            .len()
                            .saturating_sub(old_str.len())
                            .saturating_add(new_str.len()),
                    );
                    candidate.push_str(&before[..offset]);
                    candidate.push_str(&new_str);
                    candidate.push_str(&before[offset + old_str.len()..]);
                    Ok(GeneratedTextMutation {
                        candidate,
                        success_message: format!(
                            "The file {success_path} has been edited successfully."
                        ),
                    })
                },
            )
            .await
        }
        EditorCommand::Insert => {
            let Some(insert_line) = parsed.insert_line else {
                return ToolCallError::invalid_args(
                    "Parameter `insert_line` is required for command: insert",
                )
                .into_execution_result()
                .map(ToolPreparation::Complete);
            };
            let Some(new_str) = parsed.new_str else {
                return ToolCallError::invalid_args(
                    "Parameter `new_str` is required for command: insert",
                )
                .into_execution_result()
                .map(ToolPreparation::Complete);
            };
            let new_str = normalize_argument_newlines(new_str);
            let success_path = parsed.path.clone();
            patch::prepare_text_mutation(
                workspace,
                &parsed.path,
                WorkspaceMutationOperation::Update,
                cancellation,
                move |before| {
                    let lines = before.split('\n').collect::<Vec<_>>();
                    if insert_line > lines.len() {
                        return Err(ToolCallError::invalid_args(format!(
                            "Invalid `insert_line` parameter: {insert_line}. It should be within the range of lines of the file: [0, {}]",
                            lines.len()
                        )));
                    }
                    let mut output = Vec::with_capacity(lines.len().saturating_add(1));
                    output.extend_from_slice(&lines[..insert_line]);
                    output.extend(new_str.split('\n'));
                    output.extend_from_slice(&lines[insert_line..]);
                    Ok(GeneratedTextMutation {
                        candidate: output.join("\n"),
                        success_message: format!(
                            "The file {success_path} has been edited successfully."
                        ),
                    })
                },
            )
            .await
        }
    }
}

async fn view_result(
    workspace: &Workspace,
    arguments: &EditorArgs,
    cancellation: &CancellationToken,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    let resolved = match workspace.resolve(&arguments.path) {
        Ok(resolved) => resolved,
        Err(error) => return error.into_execution_result(),
    };
    let kind = match workspace.classify(&resolved, cancellation).await {
        Ok(kind) => kind,
        Err(error) => return error.into_execution_result(),
    };
    let (text, touch) = match kind {
        EntryKind::File => {
            match view_file(
                workspace,
                &resolved,
                &arguments.path,
                arguments.view_range,
                cancellation,
            )
            .await
            {
                Ok(text) => (text, Some(resolved.display.clone())),
                Err(error) => return error.into_execution_result(),
            }
        }
        EntryKind::Directory => {
            if arguments.view_range.is_some() {
                return ToolCallError::invalid_args(
                    "The `view_range` parameter is not allowed when `path` points to a directory.",
                )
                .into_execution_result();
            }
            match view_directory(workspace, &resolved, &arguments.path, cancellation).await {
                Ok(text) => (text, None),
                Err(error) => return error.into_execution_result(),
            }
        }
        EntryKind::Symlink | EntryKind::Other => {
            return ToolCallError::not_regular_file(&resolved.display).into_execution_result();
        }
    };
    let block = bounded_text_block(text)?;
    let result = ToolExecutionResult::success(vec![block])
        .map_err(|_| ToolExecutorError::new("editor view normalization failed"))?;
    Ok(match touch {
        Some(path) => result.with_workspace_touch(path),
        None => result,
    })
}

async fn view_file(
    workspace: &Workspace,
    path: &ResolvedPath,
    requested_path: &str,
    view_range: Option<(i64, i64)>,
    cancellation: &CancellationToken,
) -> ToolCallResult<String> {
    let file = workspace
        .read_file(path, 16 * 1024 * 1024, cancellation)
        .await?;
    if file.bytes.contains(&0) {
        return Err(ToolCallError::not_text(&path.display));
    }
    let raw =
        std::str::from_utf8(&file.bytes).map_err(|_| ToolCallError::not_text(&path.display))?;
    let content = normalized_view_text(raw, &path.display)?;
    let line_count = content.split('\n').count();
    if line_count > MAX_VIEW_LINES {
        return Err(ToolCallError::too_large(&path.display));
    }
    let lines = content.split('\n').collect::<Vec<_>>();
    let total = lines.len();
    let (initial, selected, range_suffix) = match view_range {
        None => (1_usize, lines.as_slice(), String::new()),
        Some((requested_initial, requested_final)) => {
            let initial = usize::try_from(requested_initial)
                .ok()
                .filter(|line| *line >= 1);
            let Some(initial) = initial.filter(|line| *line <= total) else {
                return Err(ToolCallError::invalid_args(format!(
                    "Invalid `view_range`: [{requested_initial}, {requested_final}]. Its first element `{requested_initial}` should be within the range of lines of the file: [1, {total}]"
                )));
            };
            let final_line = if requested_final == -1 {
                total
            } else {
                let final_line = usize::try_from(requested_final).map_err(|_| {
                    ToolCallError::invalid_args(format!(
                        "Invalid `view_range`: [{requested_initial}, {requested_final}]"
                    ))
                })?;
                if final_line > total {
                    return Err(ToolCallError::invalid_args(format!(
                        "Invalid `view_range`: [{requested_initial}, {requested_final}]. Its second element `{requested_final}` should be smaller than the number of lines in the file: `{total}`"
                    )));
                }
                if final_line < initial {
                    return Err(ToolCallError::invalid_args(format!(
                        "Invalid `view_range`: [{requested_initial}, {requested_final}]. Its second element `{requested_final}` should be larger or equal than its first `{requested_initial}`"
                    )));
                }
                final_line
            };
            (
                initial,
                &lines[initial - 1..final_line],
                format!(" with view_range=[{requested_initial}, {requested_final}]"),
            )
        }
    };
    let numbered = selected
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:>6}  {line}", initial + index))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(clip_text(format!(
        "Here's the content of {requested_path} with line numbers (which has a total of {total} lines){range_suffix}:\n{numbered}\n"
    )))
}

async fn view_directory(
    workspace: &Workspace,
    root: &ResolvedPath,
    requested_path: &str,
    cancellation: &CancellationToken,
) -> ToolCallResult<String> {
    let mut rows = vec![format!("d\t{requested_path}")];
    let mut queue = VecDeque::from([(root.clone(), 1_usize)]);
    let mut observed = 0_usize;
    let mut retained_path_bytes = 0_usize;
    while let Some((directory, depth)) = queue.pop_front() {
        if cancellation.is_cancelled() {
            return Err(ToolCallError::aborted());
        }
        let remaining = MAX_DIRECTORY_ENTRIES.saturating_sub(observed);
        if remaining == 0 {
            return Err(ToolCallError::search_limit(
                "editor directory view exceeds its entry limit",
            ));
        }
        let remaining_path_bytes = MAX_TRAVERSAL_PATH_BYTES.saturating_sub(retained_path_bytes);
        if remaining_path_bytes == 0 {
            return Err(ToolCallError::search_limit(
                "editor directory view exceeds its retained-path limit",
            ));
        }
        let entries = workspace
            .read_directory(&directory, remaining, remaining_path_bytes, cancellation)
            .await?;
        for entry in entries {
            observed = observed.saturating_add(1);
            retained_path_bytes = retained_path_bytes
                .checked_add(entry.display.len())
                .ok_or_else(|| {
                    ToolCallError::search_limit("editor directory path byte count overflow")
                })?;
            if entry.name.starts_with('.')
                || entry.name == "node_modules"
                || entry.name == "__pycache__"
            {
                continue;
            }
            let suffix = entry.relative.strip_prefix(&root.relative).map_err(|_| {
                ToolCallError::invalid_args("editor directory entry escaped its root")
            })?;
            let absolute = Path::new(requested_path).join(suffix);
            let absolute = absolute
                .to_str()
                .ok_or_else(|| ToolCallError::invalid_args("workspace path is not valid UTF-8"))?;
            let marker = match entry.kind {
                EntryKind::Directory => 'd',
                EntryKind::File => 'f',
                EntryKind::Symlink | EntryKind::Other => '?',
            };
            rows.push(format!("{marker}\t{absolute}"));
            if entry.kind == EntryKind::Directory && depth < 2 {
                queue.push_back((
                    ResolvedPath {
                        relative: entry.relative,
                        display: entry.display,
                    },
                    depth + 1,
                ));
            }
        }
    }
    rows.sort_by(|left, right| {
        left.split_once('\t')
            .map_or("", |(_, path)| path)
            .as_bytes()
            .cmp(
                right
                    .split_once('\t')
                    .map_or("", |(_, path)| path)
                    .as_bytes(),
            )
    });
    let listing = clip_text(format!("{}\n", rows.join("\n")));
    Ok(format!(
        "Here're the files and directories up to 2 levels deep in {requested_path}, excluding hidden items, node_modules, and Python cache directories:\n{listing}\n"
    ))
}

fn parse_arguments(arguments: &Value) -> ToolCallResult<EditorArgs> {
    let fields = arguments.as_object().ok_or_else(|| {
        ToolCallError::invalid_args("str_replace_editor arguments must be an object")
    })?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "command" | "path" | "file_text" | "insert_line" | "new_str" | "old_str" | "view_range"
        )
    }) {
        return Err(ToolCallError::invalid_args(
            "str_replace_editor received an unknown argument",
        ));
    }
    let command = match fields.get("command").and_then(Value::as_str) {
        Some("view") => EditorCommand::View,
        Some("create") => EditorCommand::Create,
        Some("str_replace") => EditorCommand::StrReplace,
        Some("insert") => EditorCommand::Insert,
        _ => {
            return Err(ToolCallError::invalid_args(
                "command must be one of view, create, str_replace, or insert",
            ));
        }
    };
    let path = fields
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolCallError::invalid_args("path must be a string"))?;
    if path.trim().is_empty() {
        return Err(ToolCallError::invalid_args(
            "path must be a non-empty string",
        ));
    }
    if !Path::new(path).is_absolute() {
        return Err(ToolCallError::invalid_args(format!(
            "The path {path} is not an absolute path; str_replace_editor paths must start at the workspace root."
        )));
    }
    let string_field = |name: &str| -> ToolCallResult<Option<String>> {
        fields
            .get(name)
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ToolCallError::invalid_args(format!("{name} must be a string")))
            })
            .transpose()
    };
    let insert_line = fields
        .get("insert_line")
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    ToolCallError::invalid_args("insert_line must be a non-negative integer")
                })
        })
        .transpose()?;
    let view_range = fields
        .get("view_range")
        .map(|value| {
            let values = value.as_array().ok_or_else(|| {
                ToolCallError::invalid_args(
                    "Invalid `view_range`. It should be a list of two integers.",
                )
            })?;
            if values.len() != 2 {
                return Err(ToolCallError::invalid_args(
                    "Invalid `view_range`. It should be a list of two integers.",
                ));
            }
            let first = values[0].as_i64().ok_or_else(|| {
                ToolCallError::invalid_args(
                    "Invalid `view_range`. It should be a list of two integers.",
                )
            })?;
            let second = values[1].as_i64().ok_or_else(|| {
                ToolCallError::invalid_args(
                    "Invalid `view_range`. It should be a list of two integers.",
                )
            })?;
            Ok((first, second))
        })
        .transpose()?;
    Ok(EditorArgs {
        command,
        path: path.to_owned(),
        file_text: string_field("file_text")?,
        insert_line,
        new_str: string_field("new_str")?,
        old_str: string_field("old_str")?,
        view_range,
    })
}

fn match_offsets(content: &str, search: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut cursor = 0_usize;
    while let Some(relative) = content[cursor..].find(search) {
        let offset = cursor + relative;
        offsets.push(offset);
        cursor = offset + search.len();
    }
    offsets
}

fn normalize_argument_newlines(value: String) -> String {
    value.replace("\r\n", "\n")
}

fn normalized_view_text(value: &str, path: &str) -> ToolCallResult<String> {
    if value.contains('\0') {
        return Err(ToolCallError::not_text(path));
    }
    let has_crlf = value.contains("\r\n");
    let without_crlf = value.replace("\r\n", "");
    if without_crlf.contains('\r') || (has_crlf && without_crlf.contains('\n')) {
        return Err(ToolCallError::model(
            "FsError",
            "FS_NOT_TEXT",
            "the requested file has mixed or unsupported line endings",
        ));
    }
    Ok(if has_crlf {
        value.replace("\r\n", "\n")
    } else {
        value.to_owned()
    })
}

fn clip_text(value: String) -> String {
    let needs_character_clip = value.chars().count() > MAX_OUTPUT_CHARS;
    let needs_encoded_clip =
        text_block_encoded_bytes(json_string_content_bytes(&value)) > MAX_TOOL_CONTENT_BYTES;
    if !needs_character_clip && !needs_encoded_clip {
        return value;
    }
    let mut prefix = value.chars().take(MAX_OUTPUT_CHARS).collect::<String>();
    while text_block_encoded_bytes(
        json_string_content_bytes(&prefix)
            .saturating_add(json_string_content_bytes(CLIPPED_MESSAGE)),
    ) > MAX_TOOL_CONTENT_BYTES
    {
        if prefix.pop().is_none() {
            break;
        }
    }
    prefix.push_str(CLIPPED_MESSAGE);
    prefix
}

fn bounded_text_block(value: String) -> Result<ContentBlock, ToolExecutorError> {
    let value = clip_text(value);
    let block = ContentBlock::text(value)
        .map_err(|_| ToolExecutorError::new("editor view normalization failed"))?;
    if block.raw().encoded_len() > MAX_TOOL_CONTENT_BYTES {
        return Err(ToolExecutorError::new(
            "editor view exceeded the normalized output limit",
        ));
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::{MAX_OUTPUT_CHARS, clip_text, match_offsets, parse_arguments};
    use serde_json::json;

    #[test]
    fn parsing_requires_absolute_paths_and_closed_commands() {
        assert!(parse_arguments(&json!({ "command": "view", "path": "/tmp/a" })).is_ok());
        assert!(parse_arguments(&json!({ "command": "view", "path": "a" })).is_err());
        assert!(parse_arguments(&json!({ "command": "replace_all", "path": "/tmp/a" })).is_err());
        assert!(
            parse_arguments(&json!({ "command": "view", "path": "/tmp/a", "extra": 1 })).is_err()
        );
    }

    #[test]
    fn literal_offsets_are_non_overlapping_and_clipping_is_bounded() {
        assert_eq!(match_offsets("same\nother\nsame", "same"), [0, 11]);
        let clipped = clip_text("x".repeat(MAX_OUTPUT_CHARS + 1));
        assert!(clipped.starts_with(&"x".repeat(MAX_OUTPUT_CHARS)));
        assert!(clipped.contains("<response clipped>"));
    }
}
