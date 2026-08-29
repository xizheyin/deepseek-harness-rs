use std::path::Path;

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    attachment::{
        AttachmentError, AttachmentRuntime, MAX_IMAGE_BYTES, MAX_REQUEST_IMAGE_BYTES,
        MAX_REQUEST_IMAGES,
    },
    model::{ContentBlock, ImageMediaType},
    provider::deepseek::DEEPSEEK_VISION_MODEL,
    tools::{ToolCallError, Workspace},
    workspace_authority::WorkspaceAuthority,
};

#[derive(Clone)]
pub(super) struct PromptImageRuntime {
    workspace: Workspace,
    attachments: AttachmentRuntime,
}

#[derive(Debug, Error)]
pub(super) enum PromptImageError {
    #[error("model `{model}` does not accept image input; select `{DEEPSEEK_VISION_MODEL}` first")]
    UnsupportedModel { model: String },
    #[error("a prompt accepts at most {MAX_REQUEST_IMAGES} images")]
    TooManyImages,
    #[error("image `{path}` must use a PNG, JPEG, WebP, or GIF extension")]
    UnsupportedType { path: String },
    #[error("{message}")]
    Workspace { code: &'static str, message: String },
    #[error("attached images exceed the {MAX_REQUEST_IMAGE_BYTES}-byte aggregate limit")]
    AggregateTooLarge,
    #[error("image `{path}` exceeds the {MAX_IMAGE_BYTES}-byte limit")]
    TooLarge { path: String },
    #[error("image `{path}` exceeds the decoded-pixel limit")]
    TooManyPixels { path: String },
    #[error("image `{path}` has bytes that do not match its file extension")]
    TypeMismatch { path: String },
    #[error("image `{path}` is empty, malformed, or unsupported")]
    InvalidImage { path: String },
    #[error("image `{path}` could not be committed to the private attachment store")]
    Store { path: String },
    #[error("image attachment was cancelled")]
    Cancelled,
    #[error("image attachment could not be prepared")]
    Unavailable,
}

struct PreparedImage {
    bytes: Vec<u8>,
    media_type: ImageMediaType,
    name: Option<String>,
    display: String,
}

impl PromptImageRuntime {
    pub(super) fn new(authority: WorkspaceAuthority, attachments: AttachmentRuntime) -> Self {
        Self {
            workspace: Workspace::from_authority(authority),
            attachments,
        }
    }

    pub(super) async fn prepare(
        &self,
        paths: &[String],
        model: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ContentBlock>, PromptImageError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if model != DEEPSEEK_VISION_MODEL {
            return Err(PromptImageError::UnsupportedModel {
                model: model.to_owned(),
            });
        }
        if paths.len() > MAX_REQUEST_IMAGES {
            return Err(PromptImageError::TooManyImages);
        }
        if cancellation.is_cancelled() {
            return Err(PromptImageError::Cancelled);
        }

        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(paths.len())
            .map_err(|_| PromptImageError::Unavailable)?;
        let mut aggregate = 0_usize;
        for path in paths {
            let media_type = media_type_for_path(path)
                .ok_or_else(|| PromptImageError::UnsupportedType { path: path.clone() })?;
            let resolved = self.workspace.resolve(path).map_err(map_workspace_error)?;
            let file = self
                .workspace
                .read_file_without_symlinks(&resolved, MAX_IMAGE_BYTES, cancellation)
                .await
                .map_err(map_workspace_error)?;
            aggregate = aggregate
                .checked_add(file.bytes.len())
                .ok_or(PromptImageError::AggregateTooLarge)?;
            if aggregate > MAX_REQUEST_IMAGE_BYTES {
                return Err(PromptImageError::AggregateTooLarge);
            }
            let name = Path::new(&resolved.display)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned);
            prepared.push(PreparedImage {
                bytes: file.bytes,
                media_type,
                name,
                display: resolved.display,
            });
        }

        for image in &prepared {
            self.attachments
                .validate_image(image.bytes.clone(), image.media_type, cancellation)
                .await
                .map_err(|error| map_attachment_error(error, &image.display))?;
        }

        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(prepared.len())
            .map_err(|_| PromptImageError::Unavailable)?;
        for image in prepared {
            let reference = self
                .attachments
                .save_image(image.bytes, image.media_type, image.name, cancellation)
                .await
                .map_err(|error| map_attachment_error(error, &image.display))?;
            blocks.push(ContentBlock::image(reference).map_err(|_| PromptImageError::Unavailable)?);
        }
        Ok(blocks)
    }
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

fn map_workspace_error(error: ToolCallError) -> PromptImageError {
    match error.into_model_parts() {
        Ok((_name, code, message)) => PromptImageError::Workspace { code, message },
        Err(_) => PromptImageError::Unavailable,
    }
}

fn map_attachment_error(error: AttachmentError, path: &str) -> PromptImageError {
    match error {
        AttachmentError::TooLarge => PromptImageError::TooLarge {
            path: path.to_owned(),
        },
        AttachmentError::TooManyPixels => PromptImageError::TooManyPixels {
            path: path.to_owned(),
        },
        AttachmentError::TypeMismatch => PromptImageError::TypeMismatch {
            path: path.to_owned(),
        },
        AttachmentError::InvalidImage => PromptImageError::InvalidImage {
            path: path.to_owned(),
        },
        AttachmentError::Cancelled => PromptImageError::Cancelled,
        AttachmentError::NotFound | AttachmentError::Corrupt | AttachmentError::Io => {
            PromptImageError::Store {
                path: path.to_owned(),
            }
        }
    }
}

impl PromptImageError {
    pub(super) const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedModel { .. } => "MODEL_NOT_IMAGE_CAPABLE",
            Self::TooManyImages => "TOO_MANY_IMAGES",
            Self::UnsupportedType { .. } => "UNSUPPORTED_IMAGE_TYPE",
            Self::Workspace { code, .. } => code,
            Self::AggregateTooLarge => "IMAGES_TOO_LARGE",
            Self::TooLarge { .. } => "IMAGE_TOO_LARGE",
            Self::TooManyPixels { .. } => "IMAGE_TOO_MANY_PIXELS",
            Self::TypeMismatch { .. } => "IMAGE_TYPE_MISMATCH",
            Self::InvalidImage { .. } => "INVALID_IMAGE",
            Self::Store { .. } => "ATTACHMENT_STORE_FAILED",
            Self::Cancelled => "ABORTED",
            Self::Unavailable => "IMAGE_INPUT_UNAVAILABLE",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use tokio_util::sync::CancellationToken;

    use crate::{
        attachment::AttachmentRuntime, model::ContentBlockKind, session::SessionStore,
        workspace_authority::WorkspaceAuthority,
    };

    use super::{PromptImageError, PromptImageRuntime};

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    async fn harness(label: &str) -> (std::path::PathBuf, PromptImageRuntime) {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dsh-image-input-{label}-{}", uuid::Uuid::new_v4()));
        let workspace = base.join("workspace");
        let store_root = base.join("state");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir(&store_root).unwrap();
        fs::set_permissions(&store_root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SessionStore::open_existing(&store_root).unwrap();
        let attachments = AttachmentRuntime::open_for_test(store, &[]).await.unwrap();
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        (base, PromptImageRuntime::new(authority, attachments))
    }

    #[tokio::test]
    async fn route_gate_precedes_path_io_and_cancellation_is_explicit() {
        let (base, runtime) = harness("route").await;
        let paths = vec!["missing.png".to_owned()];
        assert!(matches!(
            runtime
                .prepare(&paths, "deepseek-v4-flash", &CancellationToken::new())
                .await,
            Err(PromptImageError::UnsupportedModel { .. })
        ));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            runtime
                .prepare(&paths, "deepseek-v4-flash-vision-exp", &cancelled)
                .await,
            Err(PromptImageError::Cancelled)
        ));
        fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn malformed_later_member_is_rejected_before_any_object_is_saved() {
        let (base, runtime) = harness("atomic").await;
        let workspace = base.join("workspace");
        fs::write(workspace.join("first.png"), PNG).unwrap();
        fs::write(workspace.join("bad.png"), b"not an image").unwrap();
        let paths = vec!["first.png".to_owned(), "bad.png".to_owned()];
        assert!(matches!(
            runtime
                .prepare(
                    &paths,
                    "deepseek-v4-flash-vision-exp",
                    &CancellationToken::new(),
                )
                .await,
            Err(PromptImageError::InvalidImage { .. })
        ));
        assert!(!base.join("state/attachments-v1/objects").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn valid_batch_preserves_path_order_and_clean_names() {
        let (base, runtime) = harness("order").await;
        let workspace = base.join("workspace");
        fs::write(workspace.join("first.png"), PNG).unwrap();
        fs::write(workspace.join("second.png"), PNG).unwrap();
        let blocks = runtime
            .prepare(
                &["first.png".to_owned(), "second.png".to_owned()],
                "deepseek-v4-flash-vision-exp",
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let names = blocks
            .iter()
            .map(|block| match block.kind() {
                ContentBlockKind::Image { attachment } => attachment.name().unwrap(),
                _ => panic!("expected image block"),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["first.png", "second.png"]);
        assert_eq!(
            fs::read_dir(base.join("state/attachments-v1/objects"))
                .unwrap()
                .count(),
            1
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn count_extension_symlink_type_and_aggregate_guards_publish_nothing() {
        use std::os::unix::fs::symlink;

        let (base, runtime) = harness("guards").await;
        let workspace = base.join("workspace");
        let token = CancellationToken::new();
        let model = "deepseek-v4-flash-vision-exp";

        assert!(matches!(
            runtime
                .prepare(&vec!["missing.png".to_owned(); 5], model, &token,)
                .await,
            Err(PromptImageError::TooManyImages)
        ));
        assert!(matches!(
            runtime
                .prepare(&["missing.txt".to_owned()], model, &token)
                .await,
            Err(PromptImageError::UnsupportedType { .. })
        ));

        fs::write(workspace.join("pixel.png"), PNG).unwrap();
        symlink("pixel.png", workspace.join("linked.png")).unwrap();
        assert!(matches!(
            runtime
                .prepare(&["linked.png".to_owned()], model, &token)
                .await,
            Err(PromptImageError::Workspace {
                code: "WORKSPACE_PATH_DENIED",
                ..
            })
        ));

        fs::write(workspace.join("wrong.jpg"), PNG).unwrap();
        assert!(matches!(
            runtime
                .prepare(&["wrong.jpg".to_owned()], model, &token)
                .await,
            Err(PromptImageError::TypeMismatch { .. })
        ));

        fs::write(workspace.join("large-a.png"), vec![0_u8; 2_100_000]).unwrap();
        fs::write(workspace.join("large-b.png"), vec![0_u8; 2_100_000]).unwrap();
        assert!(matches!(
            runtime
                .prepare(
                    &["large-a.png".to_owned(), "large-b.png".to_owned()],
                    model,
                    &token,
                )
                .await,
            Err(PromptImageError::AggregateTooLarge)
        ));
        assert!(!base.join("state/attachments-v1/objects").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn source_fixture_fixes_the_batch_contract() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/tools/upstream_phase55_direct_image_input.json"
        ))
        .unwrap();
        assert_eq!(
            fixture["fixedCommit"],
            "47f943859bef60e4160492346772ded9b24f765a"
        );
        assert_eq!(
            fixture["fixedPromptAdmission"]["validateEveryImageBeforeFirstSave"],
            true
        );
        assert_eq!(fixture["rustCliMapping"]["maxImages"], 4);
    }
}
