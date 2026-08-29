//! Generated, capability-scoped destination for one raw Session export.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_std::fs::{Dir, MetadataExt as _, OpenOptions, OpenOptionsExt as _, PermissionsExt as _};
use thiserror::Error;

use crate::workspace_authority::WorkspaceAuthority;

use super::SessionId;

const MAX_EXPORT_DESTINATIONS: usize = 100;
const MAX_SAFE_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum SessionExportTargetError {
    #[error("CLI_SESSION_EXPORT_DESTINATION_UNAVAILABLE")]
    Unavailable,
    #[error("CLI_SESSION_EXPORT_DESTINATION_LIMIT")]
    Limit,
    #[error("CLI_SESSION_EXPORT_DESTINATION_UNSAFE")]
    Unsafe,
}

#[derive(Clone)]
pub(crate) struct SessionLogExporter {
    root: Arc<Dir>,
    display_root: Arc<PathBuf>,
}

impl std::fmt::Debug for SessionLogExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLogExporter")
            .field("workspace_capability", &true)
            .finish()
    }
}

impl SessionLogExporter {
    pub(crate) fn from_authority(authority: &WorkspaceAuthority) -> Self {
        Self {
            root: Arc::clone(authority.root()),
            display_root: Arc::new(authority.canonical_path().to_owned()),
        }
    }

    pub(crate) async fn reserve(
        &self,
        session_id: &SessionId,
    ) -> Result<PendingSessionLogExport, SessionExportTargetError> {
        let exporter = self.clone();
        let session_id = session_id.clone();
        tokio::task::spawn_blocking(move || exporter.reserve_blocking(&session_id))
            .await
            .map_err(|_| SessionExportTargetError::Unavailable)?
    }

    fn reserve_blocking(
        &self,
        session_id: &SessionId,
    ) -> Result<PendingSessionLogExport, SessionExportTargetError> {
        let safe_id = safe_session_id(session_id.as_str());
        for candidate in 1..=MAX_EXPORT_DESTINATIONS {
            let leaf = if candidate == 1 {
                format!("dsh-session-{safe_id}.jsonl")
            } else {
                format!("dsh-session-{safe_id}-{candidate}.jsonl")
            };
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            match self.root.open_with(Path::new(&leaf), &options) {
                Ok(file) => {
                    let metadata = file
                        .metadata()
                        .map_err(|_| SessionExportTargetError::Unavailable)?;
                    if !metadata.is_file()
                        || metadata.is_symlink()
                        || metadata.nlink() != 1
                        || metadata.permissions().mode() & 0o777 != 0o600
                    {
                        let _ = self.root.remove_file(Path::new(&leaf));
                        return Err(SessionExportTargetError::Unsafe);
                    }
                    return Ok(PendingSessionLogExport {
                        root: Arc::clone(&self.root),
                        leaf: leaf.clone(),
                        display: self.display_root.join(leaf),
                        file: Some(file.into_std()),
                        keep: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(SessionExportTargetError::Unavailable),
            }
        }
        Err(SessionExportTargetError::Limit)
    }
}

pub(crate) struct PendingSessionLogExport {
    root: Arc<Dir>,
    leaf: String,
    display: PathBuf,
    file: Option<File>,
    keep: bool,
}

impl std::fmt::Debug for PendingSessionLogExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingSessionLogExport")
            .field("reserved", &self.file.is_some())
            .field("keep", &self.keep)
            .finish()
    }
}

impl PendingSessionLogExport {
    pub(crate) fn take_file(&mut self) -> Result<File, SessionExportTargetError> {
        self.file
            .take()
            .ok_or(SessionExportTargetError::Unavailable)
    }

    pub(crate) async fn commit(
        self,
        bytes: u64,
    ) -> Result<SessionLogExportArtifact, SessionExportTargetError> {
        tokio::task::spawn_blocking(move || self.commit_blocking(bytes))
            .await
            .map_err(|_| SessionExportTargetError::Unavailable)?
    }

    fn commit_blocking(
        mut self,
        bytes: u64,
    ) -> Result<SessionLogExportArtifact, SessionExportTargetError> {
        if self.file.is_some() {
            return Err(SessionExportTargetError::Unavailable);
        }
        let metadata = self
            .root
            .symlink_metadata(Path::new(&self.leaf))
            .map_err(|_| SessionExportTargetError::Unavailable)?;
        if !metadata.is_file()
            || metadata.is_symlink()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.len() != bytes
        {
            return Err(SessionExportTargetError::Unsafe);
        }
        self.keep = true;
        Ok(SessionLogExportArtifact {
            path: self.display.clone(),
            bytes,
        })
    }
}

impl Drop for PendingSessionLogExport {
    fn drop(&mut self) {
        if !self.keep {
            let _ = self.root.remove_file(Path::new(&self.leaf));
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionLogExportArtifact {
    path: PathBuf,
    bytes: u64,
}

impl SessionLogExportArtifact {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn safe_session_id(value: &str) -> String {
    let mut safe = String::new();
    for byte in value.bytes().take(MAX_SAFE_ID_BYTES) {
        safe.push(
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                char::from(byte)
            } else {
                '_'
            },
        );
    }
    if safe.is_empty() {
        safe.push_str("session");
    }
    safe
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _, os::unix::fs::PermissionsExt as _};

    use super::*;
    use crate::workspace_authority::WorkspaceAuthority;

    #[tokio::test]
    async fn generated_targets_are_private_sanitized_and_never_overwrite() {
        let root = temp_workspace("export-target");
        let authority = WorkspaceAuthority::open(&root).unwrap();
        let exporter = SessionLogExporter::from_authority(&authority);
        let id = SessionId::new("session/../unsafe");

        let mut first = exporter.reserve(&id).await.unwrap();
        assert_eq!(
            first.display.file_name().and_then(|name| name.to_str()),
            Some("dsh-session-session____unsafe.jsonl")
        );
        let mut file = first.take_file().unwrap();
        file.write_all(b"reserved").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let first = first.commit(8).await.unwrap();
        assert_eq!(first.bytes(), 8);
        assert_eq!(
            fs::metadata(first.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let second = exporter.reserve(&id).await.unwrap();
        assert_eq!(
            second.display.file_name().and_then(|name| name.to_str()),
            Some("dsh-session-session____unsafe-2.jsonl")
        );
        drop(second);
        assert_eq!(fs::read(first.path()).unwrap(), b"reserved");
        assert!(!root.join("dsh-session-session____unsafe-2.jsonl").exists());

        fs::remove_file(first.path()).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn uncommitted_target_is_removed() {
        let root = temp_workspace("export-cleanup");
        let authority = WorkspaceAuthority::open(&root).unwrap();
        let exporter = SessionLogExporter::from_authority(&authority);
        let target = exporter
            .reserve(&SessionId::new("session-safe"))
            .await
            .unwrap();
        let path = target.display.clone();
        drop(target);
        assert!(!path.exists());
        fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn bounded_collision_search_fails_without_overwriting() {
        let root = temp_workspace("export-collision-limit");
        let authority = WorkspaceAuthority::open(&root).unwrap();
        let exporter = SessionLogExporter::from_authority(&authority);
        for candidate in 1..=MAX_EXPORT_DESTINATIONS {
            let leaf = if candidate == 1 {
                "dsh-session-session-full.jsonl".to_owned()
            } else {
                format!("dsh-session-session-full-{candidate}.jsonl")
            };
            fs::write(root.join(leaf), b"existing").unwrap();
        }

        assert_eq!(
            exporter
                .reserve(&SessionId::new("session-full"))
                .await
                .unwrap_err(),
            SessionExportTargetError::Limit
        );
        assert!(
            fs::read_dir(&root)
                .unwrap()
                .all(|entry| { fs::read(entry.unwrap().path()).unwrap() == b"existing" })
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("dsh-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }
}
