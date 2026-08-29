use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::{
    agent::{ToolExecutionRequest, ToolExecutionResult, ToolExecutorError},
    attachment::{AttachmentError, AttachmentRuntime, MAX_IMAGE_BYTES},
    model::{ContentBlock, ImageMediaType},
    provider::deepseek::{DEEPSEEK_PROVIDER, DEEPSEEK_VISION_MODEL},
};

use super::{arguments::parse_read_image, error::ToolCallError, workspace::Workspace};

pub(crate) const READ_IMAGE_TOOL_NAME: &str = "read_image";

pub(crate) async fn execute(
    workspace: &Workspace,
    attachments: Option<AttachmentRuntime>,
    request: &ToolExecutionRequest,
    cancellation: &CancellationToken,
) -> Result<ToolExecutionResult, ToolExecutorError> {
    let arguments = match parse_read_image(request.arguments().as_value()) {
        Ok(arguments) => arguments,
        Err(error) => return error.into_execution_result(),
    };
    let media_type = match media_type_for_path(&arguments.file_path) {
        Some(media_type) => media_type,
        None => {
            return ToolCallError::model(
                "ImageError",
                "UNSUPPORTED_IMAGE_TYPE",
                format!(
                    "cannot read `{}`: read_image accepts only PNG, JPEG, WebP, or GIF paths",
                    arguments.file_path
                ),
            )
            .into_execution_result();
        }
    };
    let Some(attachments) = attachments else {
        return ToolCallError::model(
            "ImageError",
            "ATTACHMENTS_UNAVAILABLE",
            "the durable image attachment store is unavailable",
        )
        .into_execution_result();
    };
    if request.route().provider() != DEEPSEEK_PROVIDER
        || request.route().model() != DEEPSEEK_VISION_MODEL
    {
        return ToolCallError::model(
            "ImageError",
            "MODEL_NOT_IMAGE_CAPABLE",
            format!(
                "model `{}` does not accept image input; select `{DEEPSEEK_VISION_MODEL}` first",
                request.route().model()
            ),
        )
        .into_execution_result();
    }
    if cancellation.is_cancelled() {
        return ToolCallError::aborted().into_execution_result();
    }

    let path = match workspace.resolve(&arguments.file_path) {
        Ok(path) => path,
        Err(error) => return error.into_execution_result(),
    };
    let file = match workspace
        .read_file(&path, MAX_IMAGE_BYTES, cancellation)
        .await
    {
        Ok(file) => file,
        Err(error) => return error.into_execution_result(),
    };
    let name = Path::new(&path.display)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let reference = match attachments
        .save_image(file.bytes, media_type, name, cancellation)
        .await
    {
        Ok(reference) => reference,
        Err(error) => return attachment_error(error, &path.display).into_execution_result(),
    };
    let envelope = format!(
        "<path>{}</path>\n<type>image</type>\n<content>\n{} image, {}x{} px, {} bytes\n</content>",
        path.display,
        media_type_name(reference.media_type()),
        reference.width().get(),
        reference.height().get(),
        reference.bytes().get(),
    );
    let text = ContentBlock::text(envelope)
        .map_err(|_| ToolExecutorError::new("image result normalization failed"))?;
    let image = ContentBlock::image(reference)
        .map_err(|_| ToolExecutorError::new("image result normalization failed"))?;
    ToolExecutionResult::success(vec![text, image])
        .map(|result| result.with_workspace_touch(arguments.file_path))
        .map_err(|_| ToolExecutorError::new("image result normalization failed"))
}

fn media_type_for_path(path: &str) -> Option<ImageMediaType> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some(ImageMediaType::Png),
        "jpg" | "jpeg" => Some(ImageMediaType::Jpeg),
        "webp" => Some(ImageMediaType::Webp),
        "gif" => Some(ImageMediaType::Gif),
        _ => None,
    }
}

fn media_type_name(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    }
}

fn attachment_error(error: AttachmentError, path: &str) -> ToolCallError {
    let (code, message) = match error {
        AttachmentError::TooLarge => (
            "IMAGE_TOO_LARGE",
            format!("image `{path}` exceeds the {MAX_IMAGE_BYTES}-byte limit"),
        ),
        AttachmentError::TooManyPixels => (
            "IMAGE_TOO_MANY_PIXELS",
            format!("image `{path}` exceeds the decoded-pixel limit"),
        ),
        AttachmentError::TypeMismatch => (
            "IMAGE_TYPE_MISMATCH",
            format!("image `{path}` has bytes that do not match its file extension"),
        ),
        AttachmentError::InvalidImage => (
            "INVALID_IMAGE",
            format!("image `{path}` is empty, malformed, or unsupported"),
        ),
        AttachmentError::Cancelled => return ToolCallError::aborted(),
        AttachmentError::NotFound | AttachmentError::Corrupt | AttachmentError::Io => (
            "ATTACHMENT_STORE_FAILED",
            format!("image `{path}` could not be committed to the private attachment store"),
        ),
    };
    ToolCallError::model("ImageError", code, message)
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use tokio_util::sync::CancellationToken;

    use crate::{
        agent::{ToolDispatchBinding, ToolExecutionRequest},
        attachment::AttachmentRuntime,
        model::{CallId, ImageMediaType, JsonValue, LlmCallConfig},
        provider::deepseek::{DEEPSEEK_PROVIDER, DEEPSEEK_VISION_MODEL},
        session::SessionStore,
        tools::workspace::Workspace,
    };

    use super::{READ_IMAGE_TOOL_NAME, execute, media_type_for_path};

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn extension_gate_is_case_insensitive_and_closed() {
        assert_eq!(media_type_for_path("a.PNG"), Some(ImageMediaType::Png));
        assert_eq!(media_type_for_path("a.jpeg"), Some(ImageMediaType::Jpeg));
        assert_eq!(media_type_for_path("a.webp"), Some(ImageMediaType::Webp));
        assert_eq!(media_type_for_path("a.GIF"), Some(ImageMediaType::Gif));
        assert_eq!(media_type_for_path("a.svg"), None);
    }

    #[tokio::test]
    async fn route_gate_precedes_workspace_io_and_vision_returns_two_blocks() {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dsh-read-image-test-{}", uuid::Uuid::new_v4()));
        let workspace_root = base.join("workspace");
        let session_root = base.join("sessions");
        fs::create_dir_all(&workspace_root).unwrap();
        fs::create_dir(&session_root).unwrap();
        fs::set_permissions(&session_root, fs::Permissions::from_mode(0o700)).unwrap();
        let workspace = Workspace::open(&workspace_root).unwrap();
        let runtime = AttachmentRuntime::open_for_test(
            SessionStore::open_existing(&session_root).unwrap(),
            &[],
        )
        .await
        .unwrap();
        let arguments = serde_json::json!({"file_path": "missing.png"});
        let text_route = request(
            arguments,
            LlmCallConfig::new(DEEPSEEK_PROVIDER, "deepseek-v4-flash").unwrap(),
        );
        let rejected = execute(
            &workspace,
            Some(runtime.clone()),
            &text_route,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            rejected.error().map(|failure| failure.code.as_str()),
            Some("MODEL_NOT_IMAGE_CAPABLE")
        );

        fs::write(workspace_root.join("pixel.png"), PNG).unwrap();
        let vision = request(
            serde_json::json!({"file_path": "pixel.png"}),
            LlmCallConfig::new(DEEPSEEK_PROVIDER, DEEPSEEK_VISION_MODEL).unwrap(),
        );
        let result = execute(
            &workspace,
            Some(runtime),
            &vision,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!result.is_error());
        assert_eq!(result.content().len(), 2);
        assert!(matches!(
            result.content()[1].kind(),
            crate::model::ContentBlockKind::Image { .. }
        ));
        fs::remove_dir_all(base).unwrap();
    }

    fn request(arguments: serde_json::Value, route: LlmCallConfig) -> ToolExecutionRequest {
        let raw = serde_json::to_string(&arguments).unwrap();
        ToolExecutionRequest::new(
            CallId::new("image-call"),
            READ_IMAGE_TOOL_NAME.to_owned(),
            raw,
            JsonValue::new(arguments).unwrap(),
            route,
            ToolDispatchBinding::new(),
        )
    }
}
