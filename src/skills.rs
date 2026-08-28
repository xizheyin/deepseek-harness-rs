//! Bounded project-local Skill discovery, catalog context, and body loading.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use cap_std::fs::Dir;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::task;
use tokio_util::sync::CancellationToken;

use crate::{
    model::{ContentBlock, Message, MessageSource},
    session::Session,
    workspace_authority::WorkspaceAuthority,
};

pub(crate) const SKILL_TOOL_NAME: &str = "skill";
pub(crate) const MAX_SKILLS: usize = 64;
pub(crate) const MAX_SKILL_FILE_BYTES: usize = 256 * 1024;
const MAX_SKILL_ROOT_ENTRIES: usize = 256;
const MAX_SKILL_NAME_BYTES: usize = 128;
const MAX_SKILL_DESCRIPTION_CHARS: usize = 500;
const MAX_SKILL_CATALOG_BYTES: usize = 64 * 1024;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const SKILL_ROOTS: [(&str, &str); 2] = [
    (".dsh/skills", "project-dsh"),
    (".agents/skills", "project-agents"),
];

const INITIAL_CATALOG_INTRO: &str = "A skill is a reusable set of task-specific instructions. The following skills are available in this session:";
const CATALOG_GUIDANCE: &str = "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SkillCatalogEntry {
    pub(crate) name: String,
    pub(crate) description: String,
}

#[derive(Clone, Debug)]
struct SkillCandidate {
    entry: SkillCatalogEntry,
    model_invocable: bool,
    resource_base: PathBuf,
    body: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum SkillRuntimeError {
    #[error("project Skill discovery was cancelled")]
    Cancelled,
    #[error("project Skill discovery task failed")]
    Task,
    #[error("project Skill catalog could not be represented safely")]
    Message,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum SkillLoadError {
    #[error("the Skill name is invalid")]
    Invalid,
    #[error("the Skill is unknown or no longer available")]
    Unknown,
    #[error("the Skill is not available for model invocation")]
    Disabled,
    #[error("the project Skill catalogue is temporarily unavailable")]
    Unavailable,
    #[error("project Skill loading was cancelled")]
    Cancelled,
}

#[derive(Clone)]
pub(crate) struct SkillRuntime {
    authority: WorkspaceAuthority,
}

impl std::fmt::Debug for SkillRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillRuntime")
            .field("workspace_scoped", &true)
            .finish_non_exhaustive()
    }
}

impl SkillRuntime {
    pub(crate) fn from_authority(authority: &WorkspaceAuthority) -> Self {
        Self {
            authority: authority.clone(),
        }
    }

    /// Prepare a complete catalog only when it differs from the currently
    /// visible catalog. Incomplete observations preserve the prior fact.
    pub(crate) async fn prepare_catalog(
        &self,
        session: &Session,
        cancellation: &CancellationToken,
    ) -> Result<Option<Message>, SkillRuntimeError> {
        let candidates = match self.discover(cancellation).await? {
            Discovery::Complete(candidates) => candidates,
            Discovery::Unavailable => return Ok(None),
        };
        let entries = candidates
            .values()
            .filter(|candidate| candidate.model_invocable)
            .map(|candidate| candidate.entry.clone())
            .collect::<Vec<_>>();
        let previous = visible_catalog(session);
        if previous.as_ref() == Some(&entries) || previous.is_none() && entries.is_empty() {
            return Ok(None);
        }
        if cancellation.is_cancelled() {
            return Err(SkillRuntimeError::Cancelled);
        }
        build_catalog_message(session, entries, previous.is_some()).map(Some)
    }

    /// Reread the current winning Skill and render its canonical model-facing
    /// body. A stale catalog name never grants access to a renamed/disabled file.
    pub(crate) async fn load(
        &self,
        name: &str,
        cancellation: &CancellationToken,
    ) -> Result<String, SkillLoadError> {
        if !is_skill_name(name) {
            return Err(SkillLoadError::Invalid);
        }
        let candidates = match self
            .discover(cancellation)
            .await
            .map_err(|error| match error {
                SkillRuntimeError::Cancelled => SkillLoadError::Cancelled,
                SkillRuntimeError::Task | SkillRuntimeError::Message => SkillLoadError::Unavailable,
            })? {
            Discovery::Complete(candidates) => candidates,
            Discovery::Unavailable => return Err(SkillLoadError::Unavailable),
        };
        let candidate = candidates.get(name).ok_or(SkillLoadError::Unknown)?;
        if !candidate.model_invocable {
            return Err(SkillLoadError::Disabled);
        }
        if cancellation.is_cancelled() {
            return Err(SkillLoadError::Cancelled);
        }
        render_skill(candidate, self.authority.canonical_path()).ok_or(SkillLoadError::Unavailable)
    }

    async fn discover(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Discovery, SkillRuntimeError> {
        if cancellation.is_cancelled() {
            return Err(SkillRuntimeError::Cancelled);
        }
        let root = Arc::clone(self.authority.root());
        let cancellation = cancellation.clone();
        task::spawn_blocking(move || discover_blocking(root.as_ref(), &cancellation))
            .await
            .map_err(|_| SkillRuntimeError::Task)?
    }
}

enum Discovery {
    Complete(BTreeMap<String, SkillCandidate>),
    Unavailable,
}

enum ReadSkill {
    Candidate(SkillCandidate),
    Skip,
    Unavailable,
}

fn discover_blocking(
    root: &Dir,
    cancellation: &CancellationToken,
) -> Result<Discovery, SkillRuntimeError> {
    let mut winners = BTreeMap::new();
    for (skill_root, _source) in SKILL_ROOTS {
        check_cancel(cancellation)?;
        let directory = match open_directory_no_follow(root, Path::new(skill_root), cancellation) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => continue,
            Err(_) => return Ok(Discovery::Unavailable),
        };
        let cursor = match directory.read_dir(Path::new(".")) {
            Ok(cursor) => cursor,
            Err(_) => return Ok(Discovery::Unavailable),
        };
        let mut entries = Vec::new();
        for item in cursor {
            check_cancel(cancellation)?;
            if entries.len() >= MAX_SKILL_ROOT_ENTRIES {
                return Ok(Discovery::Unavailable);
            }
            let item = match item {
                Ok(item) => item,
                Err(_) => return Ok(Discovery::Unavailable),
            };
            let name = match item.file_name().into_string() {
                Ok(name) if !name.is_empty() && !name.chars().any(char::is_control) => name,
                _ => continue,
            };
            let kind = match item.file_type() {
                Ok(kind) if kind.is_file() => EntryKind::File,
                Ok(kind) if kind.is_dir() => EntryKind::Directory,
                Ok(_) => continue,
                Err(_) => return Ok(Discovery::Unavailable),
            };
            entries.push((name, kind));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, kind) in entries {
            check_cancel(cancellation)?;
            let (relative_path, resource_base) = match kind {
                EntryKind::Directory => (
                    Path::new(skill_root).join(&name).join("SKILL.md"),
                    Path::new(skill_root).join(&name),
                ),
                EntryKind::File if name.ends_with(".md") => {
                    (Path::new(skill_root).join(&name), PathBuf::from(skill_root))
                }
                EntryKind::File => continue,
            };
            match read_skill(root, relative_path, resource_base, cancellation)? {
                ReadSkill::Candidate(candidate) => {
                    if winners.len() >= MAX_SKILLS && !winners.contains_key(&candidate.entry.name) {
                        return Ok(Discovery::Unavailable);
                    }
                    winners
                        .entry(candidate.entry.name.clone())
                        .or_insert(candidate);
                }
                ReadSkill::Skip => {}
                ReadSkill::Unavailable => return Ok(Discovery::Unavailable),
            }
        }
    }
    Ok(Discovery::Complete(winners))
}

#[derive(Clone, Copy)]
enum EntryKind {
    File,
    Directory,
}

fn read_skill(
    root: &Dir,
    relative_path: PathBuf,
    resource_base: PathBuf,
    cancellation: &CancellationToken,
) -> Result<ReadSkill, SkillRuntimeError> {
    let raw = match read_bounded_file(root, &relative_path, cancellation)? {
        BoundedRead::Text(raw) => raw,
        BoundedRead::Skip => return Ok(ReadSkill::Skip),
        BoundedRead::Unavailable => return Ok(ReadSkill::Unavailable),
    };
    let Some(parsed) = parse_skill_file(&raw) else {
        return Ok(ReadSkill::Skip);
    };
    Ok(ReadSkill::Candidate(SkillCandidate {
        entry: SkillCatalogEntry {
            name: parsed.name,
            description: normalize_description(&parsed.description),
        },
        model_invocable: parsed.model_invocable,
        resource_base,
        body: parsed.body,
    }))
}

enum BoundedRead {
    Text(String),
    Skip,
    Unavailable,
}

fn read_bounded_file(
    root: &Dir,
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<BoundedRead, SkillRuntimeError> {
    check_cancel(cancellation)?;
    let metadata = match no_follow_metadata(root, path) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return Ok(BoundedRead::Skip),
        Err(_) => return Ok(BoundedRead::Unavailable),
    };
    if !metadata.is_file() || metadata.len() > MAX_SKILL_FILE_BYTES as u64 {
        return Ok(BoundedRead::Skip);
    }
    let mut file = match open_file_no_follow(root, path, cancellation) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BoundedRead::Skip),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Ok(BoundedRead::Unavailable);
        }
        Err(_) => return Ok(BoundedRead::Unavailable),
    };
    let opened = match file.metadata() {
        Ok(metadata) => metadata,
        Err(_) => return Ok(BoundedRead::Unavailable),
    };
    if !opened.is_file() || opened.len() > MAX_SKILL_FILE_BYTES as u64 {
        return Ok(BoundedRead::Skip);
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(opened.len() as usize).is_err() {
        return Ok(BoundedRead::Unavailable);
    }
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    loop {
        check_cancel(cancellation)?;
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => return Ok(BoundedRead::Unavailable),
        };
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > MAX_SKILL_FILE_BYTES {
            return Ok(BoundedRead::Skip);
        }
        if bytes.try_reserve(count).is_err() {
            return Ok(BoundedRead::Unavailable);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    check_cancel(cancellation)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(BoundedRead::Text(text)),
        Err(_) => Ok(BoundedRead::Skip),
    }
}

fn no_follow_metadata(root: &Dir, path: &Path) -> io::Result<Option<cap_std::fs::Metadata>> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => prefix.push(part),
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Ok(None);
            }
        }
        let metadata = match root.symlink_metadata(&prefix) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            return Ok(None);
        }
    }
    root.metadata(path).map(Some)
}

fn open_directory_no_follow(
    root: &Dir,
    relative: &Path,
    cancellation: &CancellationToken,
) -> io::Result<Dir> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        if cancellation.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let Component::Normal(part) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Skill root",
            ));
        };
        current = open_child_directory_no_follow(&current, Path::new(part))?;
    }
    Ok(current)
}

#[cfg(unix)]
fn open_child_directory_no_follow(parent: &Dir, name: &Path) -> io::Result<Dir> {
    let descriptor = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| {
        if parent
            .symlink_metadata(name)
            .is_ok_and(|metadata| metadata.is_symlink())
        {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Skill root crosses a symbolic link",
            )
        } else {
            io::Error::from(error)
        }
    })?;
    Ok(Dir::from_std_file(std::fs::File::from(descriptor)))
}

#[cfg(not(unix))]
fn open_child_directory_no_follow(parent: &Dir, name: &Path) -> io::Result<Dir> {
    let metadata = parent.symlink_metadata(name)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Skill root crosses a symbolic link",
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Skill root component is not a directory",
        ));
    }
    parent.open_dir(name)
}

#[cfg(unix)]
fn open_file_no_follow(
    root: &Dir,
    path: &Path,
    cancellation: &CancellationToken,
) -> io::Result<std::fs::File> {
    let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = open_directory_no_follow(root, parent_path, cancellation)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid Skill file"))?;
    let descriptor = rustix::fs::openat(
        &parent,
        Path::new(name),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?;
    Ok(std::fs::File::from(descriptor))
}

#[cfg(not(unix))]
fn open_file_no_follow(
    root: &Dir,
    path: &Path,
    _cancellation: &CancellationToken,
) -> io::Result<std::fs::File> {
    root.open(path).map(cap_std::fs::File::into_std)
}

fn check_cancel(cancellation: &CancellationToken) -> Result<(), SkillRuntimeError> {
    if cancellation.is_cancelled() {
        Err(SkillRuntimeError::Cancelled)
    } else {
        Ok(())
    }
}

struct ParsedSkill {
    name: String,
    description: String,
    model_invocable: bool,
    body: String,
}

fn parse_skill_file(raw: &str) -> Option<ParsedSkill> {
    let first_end = raw.find('\n')?;
    if raw[..first_end].trim_end_matches('\r') != "---" {
        return None;
    }
    let frontmatter_start = first_end + 1;
    let (frontmatter_end, body_start) = find_frontmatter_end(raw, frontmatter_start)?;
    let frontmatter = &raw[frontmatter_start..frontmatter_end];
    let mut fields = BTreeMap::<String, String>::new();
    let mut seen = BTreeSet::new();
    for raw_line in frontmatter.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            return None;
        }
        let (key, value) = line.split_once(':')?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        if matches!(
            key,
            "disableModelInvocation" | "modelInvocable" | "userInvocable"
        ) {
            return None;
        }
        if !matches!(
            key,
            "name" | "description" | "whenToUse" | "disable-model-invocation" | "user-invocable"
        ) {
            continue;
        }
        if !seen.insert(key.to_owned()) {
            return None;
        }
        fields.insert(key.to_owned(), parse_scalar(value.trim())?);
    }
    let name = fields.remove("name")?;
    let description = fields.remove("description")?;
    if !is_skill_name(&name) || description.is_empty() {
        return None;
    }
    let disabled = match fields.remove("disable-model-invocation") {
        Some(value) => Some(parse_bool(&value)?),
        None => None,
    };
    let _user_invocable = match fields.remove("user-invocable") {
        Some(value) => Some(parse_bool(&value)?),
        None => None,
    };
    Some(ParsedSkill {
        name,
        description,
        model_invocable: disabled != Some(true),
        body: raw[body_start..].trim().to_owned(),
    })
}

fn find_frontmatter_end(raw: &str, start: usize) -> Option<(usize, usize)> {
    let mut line_start = start;
    while line_start <= raw.len() {
        let next = raw[line_start..]
            .find('\n')
            .map(|offset| line_start + offset);
        let line_end = next.unwrap_or(raw.len());
        if raw[line_start..line_end].trim_end_matches('\r') == "---" {
            return Some((line_start, next.map_or(raw.len(), |index| index + 1)));
        }
        line_start = next? + 1;
    }
    None
}

fn parse_scalar(raw: &str) -> Option<String> {
    if raw.is_empty() || matches!(raw, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
        return None;
    }
    if raw.starts_with('"') {
        return serde_json::from_str::<String>(raw).ok();
    }
    if raw.starts_with('\'') {
        if raw.len() < 2 || !raw.ends_with('\'') {
            return None;
        }
        return Some(raw[1..raw.len() - 1].replace("''", "'"));
    }
    if raw.starts_with(['[', '{', '&', '*', '!', '|', '>']) {
        return None;
    }
    Some(raw.to_owned())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub(crate) fn is_skill_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_SKILL_NAME_BYTES {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
        return false;
    }
    let mut previous_hyphen = false;
    for byte in bytes {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_hyphen = false,
            b'-' if !previous_hyphen => previous_hyphen = true,
            _ => return false,
        }
    }
    true
}

fn normalize_description(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = normalized.chars().count();
    if count <= MAX_SKILL_DESCRIPTION_CHARS {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(MAX_SKILL_DESCRIPTION_CHARS - 3)
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn visible_catalog(session: &Session) -> Option<Vec<SkillCatalogEntry>> {
    session
        .visible_messages()
        .into_iter()
        .rev()
        .find_map(|message| parse_catalog_source(message.source().raw().as_value()))
}

fn parse_catalog_source(source: &Value) -> Option<Vec<SkillCatalogEntry>> {
    let fields = source.as_object()?;
    if fields.get("kind")?.as_str()? != "skill-catalog" {
        return None;
    }
    let entries = fields.get("entries")?.as_array()?;
    if entries.len() > MAX_SKILLS {
        return None;
    }
    let mut parsed = Vec::new();
    parsed.try_reserve_exact(entries.len()).ok()?;
    for entry in entries {
        let entry = entry.as_object()?;
        let name = entry.get("name")?.as_str()?;
        let description = entry.get("description")?.as_str()?;
        if !is_skill_name(name) || description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS {
            return None;
        }
        parsed.push(SkillCatalogEntry {
            name: name.to_owned(),
            description: description.to_owned(),
        });
    }
    Some(parsed)
}

fn build_catalog_message(
    session: &Session,
    entries: Vec<SkillCatalogEntry>,
    replacement: bool,
) -> Result<Message, SkillRuntimeError> {
    let text = render_catalog(&entries, replacement);
    if text.len() > MAX_SKILL_CATALOG_BYTES {
        return Err(SkillRuntimeError::Message);
    }
    let seq = session.next_seq().ok_or(SkillRuntimeError::Message)?;
    let source_entries = entries
        .iter()
        .map(|entry| json!({ "name": entry.name, "description": entry.description }))
        .collect::<Vec<_>>();
    let mut source = json!({
        "kind": "skill-catalog",
        "form": "catalog",
        "entries": source_entries
    });
    if replacement {
        source["update"] = Value::Bool(true);
    }
    Message::user(
        format!("skill-catalog-{}", seq.get()),
        vec![ContentBlock::text(text).map_err(|_| SkillRuntimeError::Message)?],
        MessageSource::from_value(source).map_err(|_| SkillRuntimeError::Message)?,
    )
    .map_err(|_| SkillRuntimeError::Message)
}

fn render_catalog(entries: &[SkillCatalogEntry], replacement: bool) -> String {
    let mut lines = vec!["<system-reminder>".to_owned()];
    if replacement {
        lines.push(
            "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:"
                .to_owned(),
        );
    } else {
        lines.push(INITIAL_CATALOG_INTRO.to_owned());
    }
    lines.push(String::new());
    lines.push("<available_skills>".to_owned());
    for entry in entries {
        lines.push(format!(
            "- `{}`: {}",
            entry.name,
            escape_text(&entry.description)
        ));
    }
    lines.push("</available_skills>".to_owned());
    lines.push(String::new());
    if entries.is_empty() {
        lines.push(
            "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs."
                .to_owned(),
        );
    } else if replacement {
        lines.push(
            "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting."
                .to_owned(),
        );
    } else {
        lines.push(CATALOG_GUIDANCE.to_owned());
    }
    lines.push("</system-reminder>".to_owned());
    lines.join("\n")
}

fn render_skill(candidate: &SkillCandidate, workspace: &Path) -> Option<String> {
    let base = workspace.join(&candidate.resource_base);
    let base = base.to_str()?;
    if base.chars().any(char::is_control) {
        return None;
    }
    Some(
        [
            format!("<skill_content name=\"{}\">", candidate.entry.name),
            "<skill_resources>".to_owned(),
            format!("Base directory for this skill: {}", escape_text(base)),
            "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.".to_owned(),
            "</skill_resources>".to_owned(),
            String::new(),
            "<skill_instructions>".to_owned(),
            candidate.body.clone(),
            "</skill_instructions>".to_owned(),
            "</skill_content>".to_owned(),
        ]
        .join("\n"),
    )
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{
        MAX_SKILL_DESCRIPTION_CHARS, MAX_SKILL_FILE_BYTES, MAX_SKILL_ROOT_ENTRIES,
        SkillCatalogEntry, SkillLoadError, SkillRuntime, SkillRuntimeError, is_skill_name,
        normalize_description, parse_catalog_source, parse_skill_file, render_catalog,
    };
    use crate::{
        session::{EventKind, NewEvent, Session, SurfaceIntent},
        workspace_authority::WorkspaceAuthority,
    };
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "dsh-project-skills-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn skill(&self, root: &str, directory: &str, content: &str) -> PathBuf {
            let path = self.0.join(root).join(directory).join("SKILL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn runtime(&self) -> SkillRuntime {
            SkillRuntime::from_authority(&WorkspaceAuthority::open(&self.0).unwrap())
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn common_frontmatter_and_fail_closed_invocation_are_parsed() {
        let parsed = parse_skill_file(
            "---\nname: demo-skill\ndescription: 'Use  the demo.'\nuser-invocable: yes\n---\n\nFollow it.\n",
        )
        .unwrap();
        assert_eq!(parsed.name, "demo-skill");
        assert_eq!(parsed.description, "Use  the demo.");
        assert!(parsed.model_invocable);
        assert_eq!(parsed.body, "Follow it.");
        assert!(
            parse_skill_file(
                "---\nname: demo\ndescription: Demo\ndisable-model-invocation: maybe\n---\nbody"
            )
            .is_none()
        );
        assert!(
            parse_skill_file("---\nname: demo\ndescription: Demo\nmodelInvocable: true\n---\nbody")
                .is_none()
        );
    }

    #[test]
    fn skill_names_descriptions_and_catalog_sources_are_bounded() {
        assert!(is_skill_name("a-1"));
        for invalid in ["", "A", "-a", "a-", "a--b", "a_b"] {
            assert!(!is_skill_name(invalid));
        }
        let description = normalize_description(&"x".repeat(MAX_SKILL_DESCRIPTION_CHARS + 8));
        assert_eq!(description.chars().count(), MAX_SKILL_DESCRIPTION_CHARS);
        assert!(description.ends_with("..."));
        let entries = vec![SkillCatalogEntry {
            name: "demo".to_owned(),
            description: "Use <safely> & carefully.".to_owned(),
        }];
        let rendered = render_catalog(&entries, false);
        assert!(rendered.contains("- `demo`: Use &lt;safely&gt; &amp; carefully."));
        assert_eq!(
            parse_catalog_source(&json!({
                "kind": "skill-catalog",
                "entries": [{ "name": "demo", "description": "Demo" }]
            }))
            .unwrap()[0]
                .name,
            "demo"
        );
    }

    #[tokio::test]
    async fn project_catalog_precedence_reload_and_durable_replacement_are_exact() {
        let workspace = TempWorkspace::new("catalog");
        workspace.skill(
            ".agents/skills",
            "lower",
            "---\nname: demo-skill\ndescription: Lower priority\n---\nLower body.\n",
        );
        let winner = workspace.skill(
            ".dsh/skills",
            "winner",
            "---\nname: demo-skill\ndescription: Use   the demo safely.\n---\nFollow the demo.\n",
        );
        workspace.skill(
            ".dsh/skills",
            "disabled",
            "---\nname: hidden-skill\ndescription: Hidden\ndisable-model-invocation: true\n---\nDo not expose.\n",
        );
        let runtime = workspace.runtime();
        let cancellation = CancellationToken::new();
        let mut session = Session::new("skills-catalog").unwrap();

        let catalog = runtime
            .prepare_catalog(&session, &cancellation)
            .await
            .unwrap()
            .unwrap();
        let catalog_text = catalog.content()[0].raw().as_value()["text"]
            .as_str()
            .unwrap();
        assert!(catalog_text.contains("- `demo-skill`: Use the demo safely."));
        assert!(!catalog_text.contains("hidden-skill"));
        assert_eq!(
            catalog.source().raw().as_value()["entries"],
            json!([{ "name": "demo-skill", "description": "Use the demo safely." }])
        );
        let loaded = runtime.load("demo-skill", &cancellation).await.unwrap();
        assert!(loaded.contains("Follow the demo."));
        assert!(!loaded.contains("Lower body."));
        assert_eq!(
            runtime.load("hidden-skill", &cancellation).await,
            Err(SkillLoadError::Disabled)
        );

        session
            .append(NewEvent::surface(
                EventKind::user_message(catalog),
                SurfaceIntent::append(),
            ))
            .unwrap();
        assert!(
            runtime
                .prepare_catalog(&session, &cancellation)
                .await
                .unwrap()
                .is_none()
        );

        fs::write(
            winner,
            "---\nname: demo-skill\ndescription: Updated description\n---\nUpdated body.\n",
        )
        .unwrap();
        let replacement = runtime
            .prepare_catalog(&session, &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replacement.source().raw().as_value()["update"], true);
        assert!(
            replacement.content()[0].raw().as_value()["text"]
                .as_str()
                .unwrap()
                .contains("Updated description")
        );
        assert!(
            runtime
                .load("demo-skill", &cancellation)
                .await
                .unwrap()
                .contains("Updated body.")
        );
    }

    #[tokio::test]
    async fn flat_skill_invalid_entries_symlinks_and_cancellation_fail_closed() {
        let workspace = TempWorkspace::new("boundaries");
        let flat_root = workspace.0.join(".agents/skills");
        fs::create_dir_all(&flat_root).unwrap();
        fs::write(
            flat_root.join("flat.md"),
            "---\nname: flat-skill\ndescription: Flat skill\n---\nFlat body.\n",
        )
        .unwrap();
        workspace.skill(
            ".dsh/skills",
            "malformed",
            "---\nname: Bad_Name\ndescription: Invalid\n---\nIgnored.\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = TempWorkspace::new("outside");
            outside.skill(
                ".dsh/skills",
                "linked",
                "---\nname: linked-skill\ndescription: Linked\n---\nOutside.\n",
            );
            fs::create_dir_all(workspace.0.join(".dsh/skills")).unwrap();
            symlink(
                outside.0.join(".dsh/skills/linked"),
                workspace.0.join(".dsh/skills/linked"),
            )
            .unwrap();

            let runtime = workspace.runtime();
            let cancellation = CancellationToken::new();
            let loaded = runtime.load("flat-skill", &cancellation).await.unwrap();
            assert!(loaded.contains("Base directory for this skill:"));
            assert!(loaded.contains(".agents/skills"));
            assert!(loaded.contains("Flat body."));
            assert_eq!(
                runtime.load("linked-skill", &cancellation).await,
                Err(SkillLoadError::Unknown)
            );
        }

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            workspace
                .runtime()
                .prepare_catalog(&Session::new("cancelled-skills").unwrap(), &cancelled)
                .await,
            Err(SkillRuntimeError::Cancelled)
        );

        let oversized = workspace.0.join(".agents/skills/oversized.md");
        fs::write(&oversized, vec![b'x'; MAX_SKILL_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            workspace
                .runtime()
                .load("oversized", &CancellationToken::new())
                .await,
            Err(SkillLoadError::Unknown)
        );

        let crowded = TempWorkspace::new("crowded");
        let crowded_root = crowded.0.join(".dsh/skills");
        fs::create_dir_all(&crowded_root).unwrap();
        for index in 0..=MAX_SKILL_ROOT_ENTRIES {
            fs::write(crowded_root.join(format!("entry-{index}.md")), "invalid").unwrap();
        }
        assert_eq!(
            crowded
                .runtime()
                .load("anything", &CancellationToken::new())
                .await,
            Err(SkillLoadError::Unavailable)
        );
    }
}
