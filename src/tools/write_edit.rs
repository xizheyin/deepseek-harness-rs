use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent::{ToolExecutionResult, ToolExecutorError, ToolPreparation};

use super::{
    error::{ToolCallError, ToolCallResult},
    patch::{self, GeneratedTextMutation, MAX_MUTATION_FILE_BYTES},
    workspace::{Workspace, WorkspaceMutationOperation},
};

pub(crate) const WRITE_TOOL_NAME: &str = "write";
pub(crate) const EDIT_TOOL_NAME: &str = "edit";

struct WriteArgs {
    file_path: String,
    content: String,
}

struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    replace_all: bool,
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(name, WRITE_TOOL_NAME | EDIT_TOOL_NAME)
}

pub(crate) fn approval_required_result(
    name: &str,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    ToolCallError::model(
        "ApprovalError",
        "APPROVAL_REQUIRED",
        format!("{name} must use the Agent approval preparation stage"),
    )
    .into_execution_result()
}

pub(crate) async fn prepare(
    workspace: &Workspace,
    name: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<ToolPreparation, ToolExecutorError> {
    match name {
        WRITE_TOOL_NAME => prepare_write(workspace, arguments, cancellation).await,
        EDIT_TOOL_NAME => prepare_edit(workspace, arguments, cancellation).await,
        _ => ToolCallError::unknown_tool()
            .into_execution_result()
            .map(ToolPreparation::Complete),
    }
}

async fn prepare_write(
    workspace: &Workspace,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<ToolPreparation, ToolExecutorError> {
    let parsed = match parse_write(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_execution_result().map(ToolPreparation::Complete),
    };
    let create = format_write_output(&parsed.file_path, "Created");
    let update = format_write_output(&parsed.file_path, "Updated");
    patch::prepare_text_write(
        workspace,
        &parsed.file_path,
        parsed.content,
        create,
        update,
        cancellation,
    )
    .await
}

async fn prepare_edit(
    workspace: &Workspace,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<ToolPreparation, ToolExecutorError> {
    let parsed = match parse_edit(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return error.into_execution_result().map(ToolPreparation::Complete),
    };
    let old_string = normalize_argument_newlines(parsed.old_string);
    let new_string = normalize_argument_newlines(parsed.new_string);
    let error_path = parsed.file_path.clone();
    let success_path = parsed.file_path.clone();
    let replace_all = parsed.replace_all;
    let scan_cancellation = cancellation.clone();
    patch::prepare_text_mutation(
        workspace,
        &parsed.file_path,
        WorkspaceMutationOperation::Update,
        cancellation,
        move |before| {
            let mut replacements = 0_usize;
            for _ in before.match_indices(&old_string) {
                replacements = replacements.saturating_add(1);
                if replacements % 1_024 == 0 && scan_cancellation.is_cancelled() {
                    return Err(ToolCallError::aborted());
                }
            }
            if replacements == 0 {
                return Err(ToolCallError::model(
                    "FsError",
                    "FS_EDIT_NOT_FOUND",
                    format!("old_string was not found in \"{error_path}\""),
                ));
            }
            if !replace_all && replacements != 1 {
                return Err(ToolCallError::model(
                    "FsError",
                    "FS_AMBIGUOUS_EDIT",
                    format!(
                        "old_string matched {replacements} times in \"{error_path}\"; provide a more specific old_string or set replace_all to true"
                    ),
                ));
            }
            let candidate_bytes = replacements
                .checked_mul(old_string.len())
                .and_then(|removed| before.len().checked_sub(removed))
                .and_then(|retained| {
                    replacements
                        .checked_mul(new_string.len())
                        .and_then(|added| retained.checked_add(added))
                });
            if candidate_bytes.is_none_or(|bytes| bytes > MAX_MUTATION_FILE_BYTES) {
                return Err(ToolCallError::model(
                    "PatchError",
                    "PATCH_TOO_LARGE",
                    "the edited file exceeds the configured size limit",
                ));
            }
            let candidate = if replace_all {
                before.replace(&old_string, &new_string)
            } else {
                before.replacen(&old_string, &new_string, 1)
            };
            Ok(GeneratedTextMutation {
                candidate,
                success_message: if replace_all {
                    format!(
                        "The file {success_path} has been updated. All occurrences were successfully replaced."
                    )
                } else {
                    format!("The file {success_path} has been updated successfully.")
                },
            })
        },
    )
    .await
}

fn parse_write(arguments: &Value) -> ToolCallResult<WriteArgs> {
    let fields = closed_fields(arguments, WRITE_TOOL_NAME, &["file_path", "content"])?;
    let file_path = required_string(fields, "file_path")?;
    validate_file_path(&file_path)?;
    Ok(WriteArgs {
        file_path,
        content: required_string(fields, "content")?,
    })
}

fn parse_edit(arguments: &Value) -> ToolCallResult<EditArgs> {
    let fields = closed_fields(
        arguments,
        EDIT_TOOL_NAME,
        &["file_path", "old_string", "new_string", "replace_all"],
    )?;
    let file_path = required_string(fields, "file_path")?;
    validate_file_path(&file_path)?;
    let old_string = required_string(fields, "old_string")?;
    let new_string = required_string(fields, "new_string")?;
    if old_string.is_empty() {
        return Err(ToolCallError::invalid_args(
            "old_string must be a non-empty string",
        ));
    }
    if old_string == new_string {
        return Err(ToolCallError::invalid_args(
            "old_string and new_string must differ",
        ));
    }
    let replace_all = fields
        .get("replace_all")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolCallError::invalid_args("replace_all must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    Ok(EditArgs {
        file_path,
        old_string,
        new_string,
        replace_all,
    })
}

fn closed_fields<'a>(
    arguments: &'a Value,
    name: &str,
    allowed: &[&str],
) -> ToolCallResult<&'a serde_json::Map<String, Value>> {
    let fields = arguments.as_object().ok_or_else(|| {
        ToolCallError::invalid_args(format!("{name} arguments must be an object"))
    })?;
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolCallError::invalid_args(format!(
            "{name} received an unknown argument"
        )));
    }
    Ok(fields)
}

fn required_string(fields: &serde_json::Map<String, Value>, name: &str) -> ToolCallResult<String> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolCallError::invalid_args(format!("{name} must be a string")))
}

fn validate_file_path(path: &str) -> ToolCallResult<()> {
    if path.trim().is_empty() {
        return Err(ToolCallError::invalid_args(
            "file_path must be a non-empty string",
        ));
    }
    Ok(())
}

fn normalize_argument_newlines(value: String) -> String {
    value.replace("\r\n", "\n")
}

fn format_write_output(path: &str, verb: &str) -> String {
    format!("<path>{path}</path>\n<type>file</type>\n<content>\n{verb} file\n</content>")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_edit, parse_write};

    #[test]
    fn parsers_are_closed_and_keep_the_official_value_rules() {
        assert!(parse_write(&json!({ "file_path": "a.txt", "content": "" })).is_ok());
        assert!(
            parse_write(&json!({ "file_path": "a.txt", "content": "x", "extra": true })).is_err()
        );
        assert!(
            parse_edit(&json!({
                "file_path": "a.txt",
                "old_string": "a",
                "new_string": "b"
            }))
            .is_ok()
        );
        for invalid in [
            json!({ "file_path": " ", "old_string": "a", "new_string": "b" }),
            json!({ "file_path": "a.txt", "old_string": "", "new_string": "b" }),
            json!({ "file_path": "a.txt", "old_string": "a", "new_string": "a" }),
            json!({ "file_path": "a.txt", "old_string": "a", "new_string": "b", "replace_all": 1 }),
        ] {
            assert!(parse_edit(&invalid).is_err());
        }
    }
}
