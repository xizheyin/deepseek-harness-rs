use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::{
    ambient_authority,
    fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt, PermissionsExt},
};
use tokio::io::AsyncWriteExt;

use crate::entropy::EntropySource;

use super::capture::TailCapture;

const CREATE_ATTEMPTS: usize = 4;

pub(super) struct SpillDirectory {
    directory: Arc<Dir>,
    locator_root: PathBuf,
    entropy: EntropySource,
}

impl std::fmt::Debug for SpillDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpillDirectory")
            .field("private_directory", &true)
            .finish()
    }
}

impl SpillDirectory {
    pub(super) async fn create() -> Result<Self, ()> {
        tokio::task::spawn_blocking(create_private_directory)
            .await
            .map_err(|_| ())?
            .map_err(|_| ())
    }

    async fn open_file(self: &Arc<Self>, label: &'static str) -> io::Result<SpillFile> {
        let directory = Arc::clone(self);
        let leaf = self.random_leaf(label)?;
        let locator = self.locator_root.join(&leaf);
        let open_leaf = leaf.clone();
        let open_directory = Arc::clone(&self.directory);
        let file = tokio::task::spawn_blocking(move || {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            let file = open_directory.open_with(Path::new(&open_leaf), &options)?;
            if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "spill file mode is not owner-only",
                ));
            }
            Ok::<_, io::Error>(file.into_std())
        })
        .await
        .map_err(|_| io::Error::other("spill file creation task failed"))??;
        Ok(SpillFile {
            file: tokio::fs::File::from_std(file),
            directory,
            leaf,
            locator,
            keep: false,
        })
    }

    fn random_leaf(&self, label: &'static str) -> io::Result<String> {
        self.entropy
            .uuid_v4()
            .map(|id| format!("{}-{label}.log", id.simple()))
            .map_err(|_| io::Error::other("spill filename entropy unavailable"))
    }
}

impl Drop for SpillDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.locator_root);
    }
}

struct SpillFile {
    file: tokio::fs::File,
    directory: Arc<SpillDirectory>,
    leaf: String,
    locator: PathBuf,
    keep: bool,
}

impl Drop for SpillFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.directory.directory.remove_file(Path::new(&self.leaf));
        }
    }
}

enum SpillState {
    Dormant,
    Active(SpillFile),
    Disabled,
}

pub(super) struct SpillCapture {
    label: &'static str,
    tail: TailCapture,
    tail_limit: usize,
    captured_bytes: usize,
    state: SpillState,
    #[cfg(test)]
    fail_write: bool,
    #[cfg(test)]
    fail_flush: bool,
}

impl SpillCapture {
    pub(super) fn new(label: &'static str, tail_limit: usize) -> Self {
        Self {
            label,
            tail: TailCapture::new(tail_limit),
            tail_limit,
            captured_bytes: 0,
            state: SpillState::Dormant,
            #[cfg(test)]
            fail_write: false,
            #[cfg(test)]
            fail_flush: false,
        }
    }

    pub(super) fn needs_spill(&self, incoming: usize) -> bool {
        matches!(self.state, SpillState::Dormant)
            && self.captured_bytes.saturating_add(incoming) > self.tail_limit
    }

    pub(super) async fn push(&mut self, chunk: &[u8], directory: Option<Arc<SpillDirectory>>) {
        if chunk.is_empty() {
            return;
        }
        self.captured_bytes = self.captured_bytes.saturating_add(chunk.len());
        if self.needs_active_spill() {
            let prior = self.tail.snapshot();
            self.state = match directory {
                Some(directory) => match directory.open_file(self.label).await {
                    Ok(mut spill) => {
                        if self.write(&mut spill, &prior).await.is_ok()
                            && self.write(&mut spill, chunk).await.is_ok()
                        {
                            SpillState::Active(spill)
                        } else {
                            SpillState::Disabled
                        }
                    }
                    Err(_) => SpillState::Disabled,
                },
                None => SpillState::Disabled,
            };
        } else if matches!(self.state, SpillState::Active(_)) {
            if let SpillState::Active(mut spill) =
                std::mem::replace(&mut self.state, SpillState::Disabled)
            {
                self.state = if self.write(&mut spill, chunk).await.is_ok() {
                    SpillState::Active(spill)
                } else {
                    SpillState::Disabled
                };
            }
        }
        self.tail.push(chunk);
    }

    fn needs_active_spill(&self) -> bool {
        matches!(self.state, SpillState::Dormant) && self.captured_bytes > self.tail_limit
    }

    async fn write(&self, spill: &mut SpillFile, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.fail_write {
            return Err(io::Error::other("injected spill write failure"));
        }
        spill.file.write_all(bytes).await
    }

    pub(super) fn mark_truncated(&mut self) {
        self.tail.mark_truncated();
    }

    pub(super) async fn finish(mut self) -> SpillOutput {
        let spill_path = match std::mem::replace(&mut self.state, SpillState::Disabled) {
            SpillState::Active(mut spill) => {
                #[cfg(test)]
                let flushed = if self.fail_flush {
                    Err(io::Error::other("injected spill flush failure"))
                } else {
                    spill.file.flush().await
                };
                #[cfg(not(test))]
                let flushed = spill.file.flush().await;
                if flushed.is_ok() {
                    spill.keep = true;
                    Some(spill.locator.clone())
                } else {
                    None
                }
            }
            SpillState::Dormant | SpillState::Disabled => None,
        };
        let (tail, truncated) = self.tail.finish();
        SpillOutput {
            tail,
            truncated,
            spill_path,
            captured_bytes: self.captured_bytes,
        }
    }

    #[cfg(test)]
    fn inject_write_failure(&mut self) {
        self.fail_write = true;
    }

    #[cfg(test)]
    fn inject_flush_failure(&mut self) {
        self.fail_flush = true;
    }
}

pub(super) struct SpillOutput {
    pub(super) tail: Vec<u8>,
    pub(super) truncated: bool,
    pub(super) spill_path: Option<PathBuf>,
    pub(super) captured_bytes: usize,
}

fn create_private_directory() -> io::Result<SpillDirectory> {
    let entropy = EntropySource::system();
    let temporary = std::env::temp_dir();
    if temporary.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "temporary directory is not valid UTF-8",
        ));
    }
    let root = Dir::open_ambient_dir(&temporary, ambient_authority())?;
    for _ in 0..CREATE_ATTEMPTS {
        let id = entropy
            .uuid_v4()
            .map_err(|_| io::Error::other("spill directory entropy unavailable"))?;
        let leaf = format!("dsh-subprocess-{}-{}", std::process::id(), id.simple());
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match root.create_dir_with(Path::new(&leaf), &builder) {
            Ok(()) => {
                let directory = match root.open_dir(Path::new(&leaf)) {
                    Ok(directory) => directory,
                    Err(error) => {
                        let _ = root.remove_dir(Path::new(&leaf));
                        return Err(error);
                    }
                };
                if directory.dir_metadata()?.permissions().mode() & 0o777 != 0o700 {
                    let _ = root.remove_dir(Path::new(&leaf));
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "spill directory mode is not owner-only",
                    ));
                }
                return Ok(SpillDirectory {
                    directory: Arc::new(directory),
                    locator_root: temporary.join(leaf),
                    entropy,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique spill directory",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exact_tail_does_not_spill_and_overflow_keeps_full_bytes() {
        let directory = Arc::new(SpillDirectory::create().await.unwrap());
        let mut exact = SpillCapture::new("stdout", 4);
        exact.push(b"abcd", Some(Arc::clone(&directory))).await;
        let exact = exact.finish().await;
        assert_eq!(exact.tail, b"abcd");
        assert!(!exact.truncated);
        assert!(exact.spill_path.is_none());

        let mut overflow = SpillCapture::new("stdout", 4);
        overflow.push(b"abc", Some(Arc::clone(&directory))).await;
        overflow.push(b"def", Some(Arc::clone(&directory))).await;
        let overflow = overflow.finish().await;
        assert_eq!(overflow.tail, b"cdef");
        let path = overflow.spill_path.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn write_and_flush_failures_never_publish_a_locator() {
        let directory = Arc::new(SpillDirectory::create().await.unwrap());
        let mut write = SpillCapture::new("stdout", 2);
        write.inject_write_failure();
        write.push(b"abc", Some(Arc::clone(&directory))).await;
        let write = write.finish().await;
        assert!(write.truncated);
        assert!(write.spill_path.is_none());

        let mut flush = SpillCapture::new("stderr", 2);
        flush.push(b"abc", Some(directory)).await;
        flush.inject_flush_failure();
        let flush = flush.finish().await;
        assert!(flush.truncated);
        assert!(flush.spill_path.is_none());
    }
}
