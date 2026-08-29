//! Private, collision-safe destination and metadata for one Session fork.

use std::{fs::File, sync::Arc};

use cap_std::fs::Dir;
use tokio::task;

use crate::{model::JsonValue, workspace_authority::WorkspaceAuthority};

use super::{
    Clock as _, EventKind, EventSeq, NewEvent, SessionEvent, SessionHeader, SessionId,
    SessionStore, SessionTitleEvent, SessionTitleSource, StoreError, SystemClock, UnixMillis,
    codec::kind_data_value,
    jsonl::{encode_event_line, encode_header_line},
    path_policy::RootPlan,
    store::{
        canonical_filename, check_creation_capacity, lock_error, lock_store_root,
        named_journal_still_matches, sync_directory, validate_opened_journal,
    },
    title::{PROVIDER_TITLE_MAX_BYTES, normalize_title},
};

#[derive(Clone)]
pub(crate) struct SessionForker {
    root: RootPlan,
    cwd: Arc<String>,
    workspace: crate::workspace_authority::WorkspaceIdentity,
}

impl std::fmt::Debug for SessionForker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionForker")
            .field("session_store_capability", &true)
            .finish()
    }
}

impl SessionForker {
    pub(crate) fn new(
        store: &SessionStore,
        authority: &WorkspaceAuthority,
    ) -> Result<Self, StoreError> {
        let cwd = authority
            .canonical_path()
            .to_str()
            .ok_or(StoreError::InvalidHeader)?
            .to_owned();
        Ok(Self {
            root: store.root_plan(),
            cwd: Arc::new(cwd),
            workspace: authority.identity(),
        })
    }

    pub(crate) async fn reserve(
        &self,
        parent: SessionId,
        child: SessionId,
        seed_length: u64,
        seed_ends_with_end_seed: bool,
        source_title: Option<String>,
    ) -> Result<PendingSessionFork, StoreError> {
        let forker = self.clone();
        task::spawn_blocking(move || {
            forker.reserve_blocking(
                parent,
                child,
                seed_length,
                seed_ends_with_end_seed,
                source_title,
            )
        })
        .await
        .map_err(|_| StoreError::Io)?
    }

    fn reserve_blocking(
        &self,
        parent: SessionId,
        child: SessionId,
        seed_length: u64,
        seed_ends_with_end_seed: bool,
        source_title: Option<String>,
    ) -> Result<PendingSessionFork, StoreError> {
        let created_at = SystemClock.now().map_err(|_| StoreError::InvalidHeader)?;
        let header = SessionHeader::new_durable_fork(
            child.clone(),
            created_at,
            self.cwd.as_ref().clone(),
            self.workspace,
            parent,
            seed_length,
        )
        .map_err(|_| StoreError::InvalidHeader)?;
        let header_line = encode_header_line(&header)?;
        let mut suffix = Vec::new();
        let marker_count = u64::from(!seed_ends_with_end_seed);
        if marker_count == 1 {
            let marker_seq = EventSeq::new(seed_length).map_err(|_| StoreError::Limit)?;
            suffix.extend_from_slice(&generated_event_line(
                marker_seq,
                created_at,
                EventKind::EndSeed,
            )?);
        }
        if let Some(title) = source_title.and_then(|title| increased_fork_title(&title)) {
            let title_seq = EventSeq::new(
                seed_length
                    .checked_add(marker_count)
                    .ok_or(StoreError::Limit)?,
            )
            .map_err(|_| StoreError::Limit)?;
            let title = SessionTitleEvent::new(title, Vec::new(), SessionTitleSource::User)
                .map_err(|_| StoreError::InvalidHeader)?;
            suffix.extend_from_slice(&generated_event_line(
                title_seq,
                created_at,
                EventKind::session_title(title),
            )?);
        }

        let final_filename = canonical_filename(&child)?;
        let staging_filename = format!(".{child}.fork-tmp");
        let materialized = self.root.materialize()?;
        let root = materialized.root;
        let root_sync = materialized.sync_file;
        lock_store_root(&root_sync)?;
        reserve_empty_locked(
            root,
            root_sync,
            staging_filename,
            final_filename,
            child,
            header_line,
            suffix,
        )
    }
}

fn reserve_empty_locked(
    root: Arc<Dir>,
    root_sync: File,
    staging_filename: String,
    final_filename: String,
    child: SessionId,
    header_line: Vec<u8>,
    suffix: Vec<u8>,
) -> Result<PendingSessionFork, StoreError> {
    check_creation_capacity(root.as_ref())?;
    match rustix::fs::statat(
        root.as_ref(),
        final_filename.as_str(),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Err(rustix::io::Errno::NOENT) => {}
        Ok(_) => return Err(StoreError::Busy),
        Err(_) => return Err(StoreError::Io),
    }
    let descriptor = rustix::fs::openat(
        root.as_ref(),
        staging_filename.as_str(),
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            StoreError::Busy
        } else {
            StoreError::Io
        }
    })?;
    let file = File::from(descriptor);
    let setup = (|| {
        rustix::fs::fchmod(&file, rustix::fs::Mode::from_raw_mode(0o600))
            .map_err(|_| StoreError::Io)?;
        validate_opened_journal(&file)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(lock_error)?;
        if !named_journal_still_matches(root.as_ref(), staging_filename.as_ref(), &file)? {
            return Err(StoreError::Changed);
        }
        Ok(())
    })();
    if let Err(error) = setup {
        cleanup_created(root.as_ref(), &root_sync, &staging_filename, &file);
        return Err(error);
    }
    rustix::fs::flock(&root_sync, rustix::fs::FlockOperation::Unlock)
        .map_err(|_| StoreError::Io)?;
    Ok(PendingSessionFork {
        root,
        root_sync,
        current_filename: staging_filename,
        final_filename,
        child,
        file,
        header_line: Some(header_line),
        suffix: Some(suffix),
        destination_taken: false,
        keep: false,
    })
}

pub(crate) struct SessionForkWritePlan {
    pub(crate) destination: File,
    pub(crate) header_line: Vec<u8>,
    pub(crate) suffix: Vec<u8>,
}

pub(crate) struct PendingSessionFork {
    root: Arc<Dir>,
    root_sync: File,
    current_filename: String,
    final_filename: String,
    child: SessionId,
    file: File,
    header_line: Option<Vec<u8>>,
    suffix: Option<Vec<u8>>,
    destination_taken: bool,
    keep: bool,
}

impl std::fmt::Debug for PendingSessionFork {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingSessionFork")
            .field("destination_taken", &self.destination_taken)
            .field("keep", &self.keep)
            .finish()
    }
}

impl PendingSessionFork {
    pub(crate) fn take_write_plan(&mut self) -> Result<SessionForkWritePlan, StoreError> {
        if self.destination_taken {
            return Err(StoreError::WriterStopped);
        }
        let destination = self.file.try_clone().map_err(|_| StoreError::Io)?;
        let header_line = self.header_line.take().ok_or(StoreError::WriterStopped)?;
        let suffix = self.suffix.take().ok_or(StoreError::WriterStopped)?;
        self.destination_taken = true;
        Ok(SessionForkWritePlan {
            destination,
            header_line,
            suffix,
        })
    }

    pub(crate) async fn commit(
        self,
        bytes: u64,
        seed_events: u64,
    ) -> Result<SessionForkArtifact, StoreError> {
        task::spawn_blocking(move || self.commit_blocking(bytes, seed_events))
            .await
            .map_err(|_| StoreError::Io)?
    }

    fn commit_blocking(
        mut self,
        bytes: u64,
        seed_events: u64,
    ) -> Result<SessionForkArtifact, StoreError> {
        if !self.destination_taken
            || self.file.metadata().map_err(|_| StoreError::Io)?.len() != bytes
            || !named_journal_still_matches(
                self.root.as_ref(),
                self.current_filename.as_ref(),
                &self.file,
            )?
        {
            return Err(StoreError::Changed);
        }
        lock_store_root(&self.root_sync)?;
        rustix::fs::renameat_with(
            self.root.as_ref(),
            self.current_filename.as_str(),
            self.root.as_ref(),
            self.final_filename.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                StoreError::Busy
            } else {
                StoreError::Io
            }
        })?;
        self.current_filename.clone_from(&self.final_filename);
        if !named_journal_still_matches(
            self.root.as_ref(),
            self.current_filename.as_ref(),
            &self.file,
        )? {
            return Err(StoreError::Changed);
        }
        sync_directory(&self.root_sync)?;
        self.keep = true;
        Ok(SessionForkArtifact {
            child: self.child.clone(),
            seed_events,
            bytes,
        })
    }
}

impl Drop for PendingSessionFork {
    fn drop(&mut self) {
        if !self.keep {
            cleanup_created(
                self.root.as_ref(),
                &self.root_sync,
                &self.current_filename,
                &self.file,
            );
        }
    }
}

fn cleanup_created(root: &Dir, root_sync: &File, filename: &str, file: &File) {
    if named_journal_still_matches(root, filename.as_ref(), file).ok() == Some(true) {
        let _ = rustix::fs::unlinkat(root, filename, rustix::fs::AtFlags::empty());
        let _ = sync_directory(root_sync);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionForkArtifact {
    child: SessionId,
    seed_events: u64,
    bytes: u64,
}

impl SessionForkArtifact {
    pub(crate) fn child(&self) -> &SessionId {
        &self.child
    }

    pub(crate) fn seed_events(&self) -> u64 {
        self.seed_events
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
}

fn generated_event_line(
    seq: EventSeq,
    time: UnixMillis,
    kind: EventKind,
) -> Result<Vec<u8>, StoreError> {
    let data = kind_data_value(&kind).map_err(|_| StoreError::InvalidHeader)?;
    let data = JsonValue::new(data).map_err(|_| StoreError::InvalidHeader)?;
    let event = SessionEvent::from_new(seq, time, NewEvent::log(kind), data);
    encode_event_line(&event).map_err(StoreError::from)
}

fn increased_fork_title(title: &str) -> Option<String> {
    let (prefix, open, close, digits) = numbered_suffix(title).unwrap_or((title, " (", ")", "1"));
    let incremented = if numbered_suffix(title).is_some() {
        increment_decimal(digits)?
    } else {
        digits.to_owned()
    };
    let suffix = format!("{open}{incremented}{close}");
    let maximum_prefix = PROVIDER_TITLE_MAX_BYTES.checked_sub(suffix.len())?;
    let mut end = prefix.len().min(maximum_prefix);
    while end != 0 && !prefix.is_char_boundary(end) {
        end -= 1;
    }
    normalize_title(
        &format!("{}{suffix}", &prefix[..end]),
        PROVIDER_TITLE_MAX_BYTES,
    )
}

fn numbered_suffix(title: &str) -> Option<(&str, &'static str, &'static str, &str)> {
    if let Some(without_close) = title.strip_suffix(')') {
        let open = without_close.rfind('(')?;
        let digits = &without_close[open + 1..];
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some((&without_close[..open], "(", ")", digits));
        }
    }
    let without_close = title.strip_suffix('）')?;
    let open = without_close.rfind('（')?;
    let digits = &without_close[open + '（'.len_utf8()..];
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some((
        &without_close[..open],
        "（",
        "）",
        digits,
    ))
}

fn increment_decimal(digits: &str) -> Option<String> {
    let mut bytes = digits.as_bytes().to_vec();
    let mut carry = true;
    for byte in bytes.iter_mut().rev() {
        if !carry {
            break;
        }
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte = byte.checked_add(1)?;
            carry = false;
        }
    }
    if carry {
        bytes.try_reserve_exact(1).ok()?;
        bytes.insert(0, b'1');
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _, os::unix::fs::PermissionsExt as _};

    use crate::{
        session::{SessionId, SessionStore},
        workspace_authority::WorkspaceAuthority,
    };

    use super::{PROVIDER_TITLE_MAX_BYTES, SessionForker, increased_fork_title};

    #[test]
    fn fork_titles_increment_both_bracket_styles_without_integer_overflow() {
        assert_eq!(increased_fork_title("Work").as_deref(), Some("Work (1)"));
        assert_eq!(
            increased_fork_title("Work (9)").as_deref(),
            Some("Work (10)")
        );
        assert_eq!(
            increased_fork_title("Work（99）").as_deref(),
            Some("Work（100）")
        );
        assert_eq!(
            increased_fork_title("Work (9999999999999999999999999999999999999999)").as_deref(),
            Some("Work (10000000000000000000000000000000000000000)")
        );
        let bounded = increased_fork_title(&"界".repeat(40)).unwrap();
        assert!(bounded.len() <= PROVIDER_TITLE_MAX_BYTES);
        assert!(bounded.ends_with(" (1)"));
    }

    #[tokio::test]
    async fn fork_target_is_private_visible_and_never_overwritten() {
        let root = private_dir("fork-store");
        let workspace = private_dir("fork-workspace");
        let store = SessionStore::open_existing(&root).unwrap();
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let forker = SessionForker::new(&store, &authority).unwrap();
        let parent = SessionId::new("session-550e8400-e29b-41d4-a716-446655440000");
        let child = SessionId::new("session-550e8400-e29b-41d4-a716-446655440001");

        let mut target = forker
            .reserve(
                parent.clone(),
                child.clone(),
                0,
                false,
                Some("Work".to_owned()),
            )
            .await
            .unwrap();
        assert!(!root.join(format!("{child}.jsonl")).exists());
        let plan = target.take_write_plan().unwrap();
        let bytes = plan.header_line.len() + plan.suffix.len();
        let mut destination = plan.destination;
        destination.write_all(&plan.header_line).unwrap();
        destination.write_all(&plan.suffix).unwrap();
        destination.sync_all().unwrap();
        drop(destination);
        let artifact = target.commit(bytes as u64, 0).await.unwrap();
        assert_eq!(artifact.child(), &child);
        let path = root.join(format!("{child}.jsonl"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let lines = fs::read_to_string(&path).unwrap();
        assert!(lines.contains(&format!("\"parentSession\":\"{parent}\"")));
        assert!(lines.contains("\"seedLength\":0"));
        assert!(lines.contains("\"type\":\"session/end-seed\",\"seq\":0"));
        assert!(lines.contains("\"title\":\"Work (1)\""));
        let listed = store.list(Some(authority.identity())).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id(), &child);
        assert_eq!(listed[0].title(), Some("Work (1)"));

        assert_eq!(
            forker
                .reserve(parent, child, 0, false, None)
                .await
                .unwrap_err(),
            crate::session::StoreError::Busy
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), bytes as u64);
        fs::remove_file(path).unwrap();
        fs::remove_dir(root).unwrap();
        fs::remove_dir(workspace).unwrap();
    }

    #[tokio::test]
    async fn uncommitted_fork_target_is_removed() {
        let root = private_dir("fork-cleanup-store");
        let workspace = private_dir("fork-cleanup-workspace");
        let store = SessionStore::open_existing(&root).unwrap();
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let forker = SessionForker::new(&store, &authority).unwrap();
        let child = SessionId::new("session-550e8400-e29b-41d4-a716-446655440002");
        let target = forker
            .reserve(
                SessionId::new("session-550e8400-e29b-41d4-a716-446655440000"),
                child.clone(),
                0,
                false,
                None,
            )
            .await
            .unwrap();
        drop(target);
        assert!(!root.join(format!("{child}.jsonl")).exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
        fs::remove_dir(workspace).unwrap();
    }

    #[tokio::test]
    async fn inherited_end_seed_is_not_duplicated_before_the_fork_title() {
        let root = private_dir("fork-existing-marker-store");
        let workspace = private_dir("fork-existing-marker-workspace");
        let store = SessionStore::open_existing(&root).unwrap();
        let authority = WorkspaceAuthority::open(&workspace).unwrap();
        let forker = SessionForker::new(&store, &authority).unwrap();
        let mut target = forker
            .reserve(
                SessionId::new("session-550e8400-e29b-41d4-a716-446655440000"),
                SessionId::new("session-550e8400-e29b-41d4-a716-446655440003"),
                4,
                true,
                Some("Already resumed".to_owned()),
            )
            .await
            .unwrap();
        let plan = target.take_write_plan().unwrap();
        let rows = std::str::from_utf8(&plan.suffix)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["type"], "session/title");
        assert_eq!(rows[0]["seq"], 4);
        drop(plan);
        drop(target);
        fs::remove_dir(root).unwrap();
        fs::remove_dir(workspace).unwrap();
    }

    fn private_dir(label: &str) -> std::path::PathBuf {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("dsh-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}
