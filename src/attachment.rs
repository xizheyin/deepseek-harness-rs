//! Bounded durable image attachments shared by tools and model providers.

use std::{
    collections::{BTreeSet, VecDeque},
    io::{Cursor, Read as _, Write as _},
    path::Path,
    sync::{Arc, Mutex},
};

use aws_lc_rs::digest::{SHA256, digest};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt as _, OpenOptions, OpenOptionsExt as _, PermissionsExt as _,
};
use image::{GenericImageView as _, ImageFormat, ImageReader};
use thiserror::Error;
use tokio::task;
use tokio_util::sync::CancellationToken;

use crate::{
    entropy::EntropySource,
    model::{ContentBlockKind, ImageAttachmentRef, ImageMediaType, Message},
    session::{SessionStore, StoreError},
};

pub(crate) const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub(crate) const MAX_REQUEST_IMAGES: usize = 4;
pub(crate) const MAX_REQUEST_IMAGE_BYTES: usize = 4 * 1024 * 1024;

const ATTACHMENT_DIRECTORY: &str = "attachments-v1";
const OBJECT_DIRECTORY: &str = "objects";
const TEMP_DIRECTORY: &str = "tmp";

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum AttachmentError {
    #[error("IMAGE_TOO_LARGE")]
    TooLarge,
    #[error("IMAGE_TOO_MANY_PIXELS")]
    TooManyPixels,
    #[error("IMAGE_TYPE_MISMATCH")]
    TypeMismatch,
    #[error("INVALID_IMAGE")]
    InvalidImage,
    #[error("ATTACHMENT_NOT_FOUND")]
    NotFound,
    #[error("ATTACHMENT_CORRUPT")]
    Corrupt,
    #[error("ATTACHMENT_IO")]
    Io,
    #[error("ABORTED")]
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct AttachmentRuntime {
    storage: Arc<AttachmentStorage>,
    cache: Arc<Mutex<ImageCache>>,
}

impl std::fmt::Debug for AttachmentRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentRuntime")
            .field("durable", &true)
            .field("maximum_image_bytes", &MAX_IMAGE_BYTES)
            .field("maximum_request_images", &MAX_REQUEST_IMAGES)
            .finish_non_exhaustive()
    }
}

struct AttachmentStorage {
    root: Arc<Dir>,
    entropy: EntropySource,
}

#[derive(Default)]
struct ImageCache {
    entries: VecDeque<(ImageAttachmentRef, Arc<[u8]>)>,
    bytes: usize,
}

impl AttachmentRuntime {
    pub(crate) async fn open(
        store: SessionStore,
        messages: &[Message],
        cancellation: &CancellationToken,
    ) -> Result<Self, StoreError> {
        if cancellation.is_cancelled() {
            return Err(StoreError::Cancelled);
        }
        let root = task::spawn_blocking(move || store.materialize_root_for_attachments())
            .await
            .map_err(|_| StoreError::Io)??;
        let runtime = Self {
            storage: Arc::new(AttachmentStorage {
                root,
                entropy: EntropySource::system(),
            }),
            cache: Arc::new(Mutex::new(ImageCache::default())),
        };
        runtime
            .preload(messages, cancellation)
            .await
            .map_err(store_error_for_attachment)?;
        Ok(runtime)
    }

    #[cfg(test)]
    pub(crate) async fn open_for_test(
        store: SessionStore,
        messages: &[Message],
    ) -> Result<Self, StoreError> {
        Self::open(store, messages, &CancellationToken::new()).await
    }

    pub(crate) async fn save_image(
        &self,
        bytes: Vec<u8>,
        declared: ImageMediaType,
        name: Option<String>,
        cancellation: &CancellationToken,
    ) -> Result<ImageAttachmentRef, AttachmentError> {
        if cancellation.is_cancelled() {
            return Err(AttachmentError::Cancelled);
        }
        if bytes.is_empty() {
            return Err(AttachmentError::InvalidImage);
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(AttachmentError::TooLarge);
        }
        let storage = Arc::clone(&self.storage);
        let token = cancellation.clone();
        let (reference, bytes) = task::spawn_blocking(move || {
            if token.is_cancelled() {
                return Err(AttachmentError::Cancelled);
            }
            let (detected, width, height) = inspect_image(&bytes)?;
            if detected != declared {
                return Err(AttachmentError::TypeMismatch);
            }
            if token.is_cancelled() {
                return Err(AttachmentError::Cancelled);
            }
            let object_id = object_id(&bytes);
            storage.publish(&object_id, &bytes)?;
            let byte_count = u64::try_from(bytes.len()).map_err(|_| AttachmentError::TooLarge)?;
            let reference = ImageAttachmentRef::new(
                format!("sha256:{object_id}"),
                detected,
                byte_count,
                u64::from(width),
                u64::from(height),
                clean_name(name),
            )
            .map_err(|_| AttachmentError::InvalidImage)?;
            Ok((reference, bytes))
        })
        .await
        .map_err(|_| AttachmentError::Io)??;
        if cancellation.is_cancelled() {
            return Err(AttachmentError::Cancelled);
        }
        self.insert_cache(reference.clone(), bytes.into())?;
        Ok(reference)
    }

    pub(crate) fn resolve_cached(
        &self,
        reference: &ImageAttachmentRef,
    ) -> Result<Arc<[u8]>, AttachmentError> {
        let cache = self.cache.lock().map_err(|_| AttachmentError::Io)?;
        let bytes = cache
            .entries
            .iter()
            .find_map(|(cached, bytes)| (cached == reference).then(|| bytes.clone()))
            .ok_or(AttachmentError::NotFound)?;
        Ok(bytes)
    }

    async fn preload(
        &self,
        messages: &[Message],
        cancellation: &CancellationToken,
    ) -> Result<(), AttachmentError> {
        let retained = retained_request_images(messages);
        let mut seen = BTreeSet::new();
        for reference in retained {
            if !seen.insert(reference.attachment_id().as_str().to_owned()) {
                continue;
            }
            if cancellation.is_cancelled() {
                return Err(AttachmentError::Cancelled);
            }
            let storage = Arc::clone(&self.storage);
            let reference_for_read = reference.clone();
            let bytes = task::spawn_blocking(move || storage.read(&reference_for_read))
                .await
                .map_err(|_| AttachmentError::Io)??;
            self.insert_cache(reference, bytes.into())?;
        }
        Ok(())
    }

    fn insert_cache(
        &self,
        reference: ImageAttachmentRef,
        bytes: Arc<[u8]>,
    ) -> Result<(), AttachmentError> {
        let mut cache = self.cache.lock().map_err(|_| AttachmentError::Io)?;
        if let Some(index) = cache
            .entries
            .iter()
            .position(|(cached, _)| cached.attachment_id() == reference.attachment_id())
        {
            if let Some((_, prior)) = cache.entries.remove(index) {
                cache.bytes = cache.bytes.saturating_sub(prior.len());
            }
        }
        cache.bytes = cache
            .bytes
            .checked_add(bytes.len())
            .ok_or(AttachmentError::TooLarge)?;
        cache.entries.push_back((reference, bytes));
        while cache.bytes > MAX_REQUEST_IMAGE_BYTES || cache.entries.len() > MAX_REQUEST_IMAGES {
            let Some((_, removed)) = cache.entries.pop_front() else {
                break;
            };
            cache.bytes = cache.bytes.saturating_sub(removed.len());
        }
        Ok(())
    }
}

impl AttachmentStorage {
    fn publish(&self, expected_id: &str, bytes: &[u8]) -> Result<(), AttachmentError> {
        prepare_storage_root(Arc::clone(&self.root)).map_err(|_| AttachmentError::Io)?;
        let attachments = self
            .root
            .open_dir(ATTACHMENT_DIRECTORY)
            .map_err(|_| AttachmentError::Io)?;
        let objects = attachments
            .open_dir(OBJECT_DIRECTORY)
            .map_err(|_| AttachmentError::Io)?;
        let temporary = attachments
            .open_dir(TEMP_DIRECTORY)
            .map_err(|_| AttachmentError::Io)?;
        let temporary_name = self
            .entropy
            .uuid_v4()
            .map_err(|_| AttachmentError::Io)?
            .simple()
            .to_string();
        let mut options = OpenOptions::new();
        options.create_new(true).write(true).mode(0o600);
        let mut file = temporary
            .open_with(&temporary_name, &options)
            .map_err(|_| AttachmentError::Io)?;
        let outcome = (|| {
            file.write_all(bytes).map_err(|_| AttachmentError::Io)?;
            file.sync_all().map_err(|_| AttachmentError::Io)?;
            match temporary.hard_link(&temporary_name, &objects, expected_id) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = read_bounded_object(&objects, expected_id)?;
                    if object_id(&existing) != expected_id {
                        return Err(AttachmentError::Corrupt);
                    }
                }
                Err(_) => return Err(AttachmentError::Io),
            }
            sync_dir(&objects)?;
            sync_dir(&attachments)?;
            Ok(())
        })();
        drop(file);
        let cleanup = temporary.remove_file(&temporary_name);
        if outcome.is_ok() && cleanup.is_err() {
            return Err(AttachmentError::Io);
        }
        outcome
    }

    fn read(&self, reference: &ImageAttachmentRef) -> Result<Vec<u8>, AttachmentError> {
        let id = reference
            .attachment_id()
            .as_str()
            .strip_prefix("sha256:")
            .filter(|value| valid_object_id(value))
            .ok_or(AttachmentError::Corrupt)?;
        let objects = self
            .root
            .open_dir(Path::new(ATTACHMENT_DIRECTORY).join(OBJECT_DIRECTORY))
            .map_err(|_| AttachmentError::NotFound)?;
        let bytes = read_bounded_object(&objects, id)?;
        verify_reference(reference, &bytes)?;
        Ok(bytes)
    }
}

pub(crate) fn retained_request_images(messages: &[Message]) -> Vec<ImageAttachmentRef> {
    let mut images = Vec::new();
    for message in messages {
        collect_images_from_blocks(message.content(), &mut images);
    }
    let mut retained_bytes = images.iter().fold(0_usize, |total, reference| {
        total.saturating_add(usize::try_from(reference.bytes().get()).unwrap_or(usize::MAX))
    });
    let mut first = 0_usize;
    while images.len().saturating_sub(first) > MAX_REQUEST_IMAGES
        || retained_bytes > MAX_REQUEST_IMAGE_BYTES
    {
        let Some(reference) = images.get(first) else {
            break;
        };
        retained_bytes = retained_bytes
            .saturating_sub(usize::try_from(reference.bytes().get()).unwrap_or(usize::MAX));
        first += 1;
    }
    images.split_off(first)
}

fn collect_images_from_blocks(
    blocks: &[crate::model::ContentBlock],
    output: &mut Vec<ImageAttachmentRef>,
) {
    for block in blocks {
        match block.kind() {
            ContentBlockKind::Image { attachment } => output.push(attachment.clone()),
            ContentBlockKind::ToolResult { .. } => {
                if let Some(content) = block.tool_result_content() {
                    for value in content {
                        collect_images_from_value(value, output);
                    }
                }
            }
            ContentBlockKind::Text { .. }
            | ContentBlockKind::Reasoning { .. }
            | ContentBlockKind::ToolCall { .. }
            | ContentBlockKind::Other { .. } => {}
        }
    }
}

fn collect_images_from_value(value: &serde_json::Value, output: &mut Vec<ImageAttachmentRef>) {
    let Some(fields) = value.as_object() else {
        return;
    };
    match fields.get("type").and_then(serde_json::Value::as_str) {
        Some("image") => {
            if let Some(reference) = fields
                .get("attachment")
                .and_then(|value| serde_json::from_value(value.clone()).ok())
            {
                output.push(reference);
            }
        }
        Some("tool-result") => {
            if let Some(content) = fields.get("content").and_then(serde_json::Value::as_array) {
                for nested in content {
                    collect_images_from_value(nested, output);
                }
            }
        }
        _ => {}
    }
}

fn prepare_storage_root(root: Arc<Dir>) -> std::io::Result<Arc<Dir>> {
    ensure_private_dir(&root, ATTACHMENT_DIRECTORY)?;
    let attachments = root.open_dir(ATTACHMENT_DIRECTORY)?;
    ensure_private_dir(&attachments, OBJECT_DIRECTORY)?;
    ensure_private_dir(&attachments, TEMP_DIRECTORY)?;
    Ok(root)
}

fn ensure_private_dir(parent: &Dir, name: &str) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match parent.create_dir_with(name, &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let directory = parent.open_dir(name)?;
    let metadata = directory.dir_metadata()?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "attachment directory is not private",
        ));
    }
    Ok(())
}

fn inspect_image(bytes: &[u8]) -> Result<(ImageMediaType, u32, u32), AttachmentError> {
    let format = image::guess_format(bytes).map_err(|_| AttachmentError::InvalidImage)?;
    let media_type = match format {
        ImageFormat::Png => ImageMediaType::Png,
        ImageFormat::Jpeg => ImageMediaType::Jpeg,
        ImageFormat::WebP => ImageMediaType::Webp,
        ImageFormat::Gif => ImageMediaType::Gif,
        _ => return Err(AttachmentError::InvalidImage),
    };
    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| AttachmentError::InvalidImage)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(AttachmentError::TooManyPixels)?;
    if width == 0 || height == 0 {
        return Err(AttachmentError::InvalidImage);
    }
    if pixels > MAX_IMAGE_PIXELS {
        return Err(AttachmentError::TooManyPixels);
    }
    let decoded = ImageReader::with_format(Cursor::new(bytes), format)
        .decode()
        .map_err(|_| AttachmentError::InvalidImage)?;
    if decoded.dimensions() != (width, height) {
        return Err(AttachmentError::InvalidImage);
    }
    Ok((media_type, width, height))
}

fn verify_reference(reference: &ImageAttachmentRef, bytes: &[u8]) -> Result<(), AttachmentError> {
    let expected = reference
        .attachment_id()
        .as_str()
        .strip_prefix("sha256:")
        .filter(|value| valid_object_id(value))
        .ok_or(AttachmentError::Corrupt)?;
    if object_id(bytes) != expected
        || u64::try_from(bytes.len()).ok() != Some(reference.bytes().get())
    {
        return Err(AttachmentError::Corrupt);
    }
    let (media_type, width, height) = inspect_image(bytes)?;
    if media_type != reference.media_type()
        || u64::from(width) != reference.width().get()
        || u64::from(height) != reference.height().get()
    {
        return Err(AttachmentError::Corrupt);
    }
    Ok(())
}

fn object_id(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let value = digest(&SHA256, bytes);
    let mut output = String::with_capacity(64);
    for byte in value.as_ref() {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn valid_object_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_bounded_object(directory: &Dir, name: &str) -> Result<Vec<u8>, AttachmentError> {
    let mut file = directory.open(name).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            AttachmentError::NotFound
        } else {
            AttachmentError::Io
        }
    })?;
    let metadata = file.metadata().map_err(|_| AttachmentError::Io)?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(AttachmentError::Corrupt);
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| AttachmentError::Corrupt)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|_| AttachmentError::Io)?;
    if bytes.len() != capacity {
        return Err(AttachmentError::Corrupt);
    }
    Ok(bytes)
}

fn sync_dir(directory: &Dir) -> Result<(), AttachmentError> {
    directory
        .try_clone()
        .and_then(|directory| directory.into_std_file().sync_all())
        .map_err(|_| AttachmentError::Io)
}

fn clean_name(name: Option<String>) -> Option<String> {
    name.filter(|value| {
        !value.is_empty()
            && value.len() <= 255
            && !value.chars().any(char::is_control)
            && Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
    })
}

fn store_error_for_attachment(error: AttachmentError) -> StoreError {
    match error {
        AttachmentError::Cancelled => StoreError::Cancelled,
        AttachmentError::NotFound
        | AttachmentError::Corrupt
        | AttachmentError::InvalidImage
        | AttachmentError::TypeMismatch
        | AttachmentError::TooLarge
        | AttachmentError::TooManyPixels => StoreError::Corrupt,
        AttachmentError::Io => StoreError::Io,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor, os::unix::fs::PermissionsExt as _};

    use tokio_util::sync::CancellationToken;

    use crate::{
        model::{ContentBlock, Message, MessageSource},
        session::{SessionStore, StoreError},
    };

    use super::{AttachmentRuntime, ImageMediaType, inspect_image, object_id, valid_object_id};

    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[test]
    fn png_is_fully_decoded_and_content_addressed() {
        assert_eq!(inspect_image(PNG).unwrap(), (ImageMediaType::Png, 1, 1));
        let id = object_id(PNG);
        assert!(valid_object_id(&id));
        assert_eq!(id.len(), 64);
    }

    #[test]
    fn malformed_image_is_rejected() {
        assert!(inspect_image(b"not an image").is_err());
    }

    #[test]
    fn every_advertised_format_is_decoded() {
        let image = image::DynamicImage::new_rgba8(1, 1);
        for (format, expected) in [
            (image::ImageFormat::Png, ImageMediaType::Png),
            (image::ImageFormat::Jpeg, ImageMediaType::Jpeg),
            (image::ImageFormat::WebP, ImageMediaType::Webp),
            (image::ImageFormat::Gif, ImageMediaType::Gif),
        ] {
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, format).unwrap();
            assert_eq!(inspect_image(bytes.get_ref()).unwrap(), (expected, 1, 1));
        }
    }

    #[tokio::test]
    async fn durable_object_is_deduplicated_and_preloaded_on_resume() {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dsh-attachment-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SessionStore::open_existing(&root).unwrap();
        let runtime = AttachmentRuntime::open_for_test(store.clone(), &[])
            .await
            .unwrap();
        let first = runtime
            .save_image(
                PNG.to_vec(),
                ImageMediaType::Png,
                Some("pixel.png".to_owned()),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let second = runtime
            .save_image(
                PNG.to_vec(),
                ImageMediaType::Png,
                Some("pixel.png".to_owned()),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(
            fs::read_dir(root.join("attachments-v1/objects"))
                .unwrap()
                .count(),
            1
        );

        let message = Message::user(
            "image-message",
            vec![ContentBlock::image(first.clone()).unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        let resumed = AttachmentRuntime::open_for_test(store, &[message])
            .await
            .unwrap();
        assert_eq!(resumed.resolve_cached(&first).unwrap().as_ref(), PNG);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupt_retained_object_fails_preload() {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("dsh-attachment-corrupt-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SessionStore::open_existing(&root).unwrap();
        let runtime = AttachmentRuntime::open_for_test(store.clone(), &[])
            .await
            .unwrap();
        let reference = runtime
            .save_image(
                PNG.to_vec(),
                ImageMediaType::Png,
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let id = reference
            .attachment_id()
            .as_str()
            .strip_prefix("sha256:")
            .unwrap();
        fs::write(root.join("attachments-v1/objects").join(id), b"changed").unwrap();
        let message = Message::user(
            "image-message",
            vec![ContentBlock::image(reference).unwrap()],
            MessageSource::user().unwrap(),
        )
        .unwrap();
        assert_eq!(
            AttachmentRuntime::open_for_test(store, &[message])
                .await
                .unwrap_err(),
            StoreError::Corrupt
        );
        fs::remove_dir_all(root).unwrap();
    }
}
