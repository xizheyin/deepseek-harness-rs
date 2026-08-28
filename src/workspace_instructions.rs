//! Bounded, durable workspace guidance loaded before the first model request.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use aws_lc_rs::digest::{SHA1_FOR_LEGACY_USE_ONLY, digest};
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

pub(crate) const MAX_WORKSPACE_INSTRUCTION_BYTES: usize = 65_536;
const MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES: usize = 1_048_576;
const PROJECT_CANDIDATES: [&str; 4] = [
    "AGENTS.md",
    "CLAUDE.md",
    "AGENTS.local.md",
    "CLAUDE.local.md",
];
const BASELINE_INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";
const REPLACEMENT_INTRO: &str = "This complete workspace instruction baseline replaces all earlier workspace instruction baselines. The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";
const EMPTY_REPLACEMENT_INTRO: &str = "This complete workspace instruction baseline replaces all earlier workspace instruction baselines. No workspace instructions are currently active.";
const COMPACT_INTRO: &str =
    "Workspace instructions were omitted or truncated to fit the configured byte budget.";

#[derive(Debug, Error)]
pub(crate) enum WorkspaceInstructionError {
    #[error("workspace instruction loading was cancelled")]
    Cancelled,
    #[error("workspace instruction loading task failed")]
    Task,
    #[error("workspace instruction message could not be constructed")]
    Message,
    #[error("workspace instruction message identity is unavailable")]
    Identity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangeAction {
    Set,
    Replace,
    Remove,
}

impl ChangeAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Replace => "replace",
            Self::Remove => "remove",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InstructionChange {
    action: ChangeAction,
    scope: String,
    path: String,
    digest: Option<String>,
}

#[derive(Clone, Debug)]
struct LoadedInstruction {
    scope: String,
    display_path: String,
    content: String,
    digest: String,
}

#[derive(Clone, Debug)]
enum CandidateState {
    Present(LoadedInstruction),
    Absent,
    Unavailable,
}

#[derive(Clone, Debug)]
struct CandidateObservation {
    scope: String,
    state: CandidateState,
}

#[derive(Clone, Debug)]
enum RenderKind {
    Baseline,
    Change(ChangeAction),
}

#[derive(Clone, Debug)]
struct RenderEntry {
    instruction: LoadedInstruction,
    kind: RenderKind,
}

#[derive(Debug)]
struct RenderedEntries {
    text: String,
    represented: Vec<usize>,
}

#[derive(Debug)]
struct LoadedWorkspaceInstructions {
    observations: Vec<CandidateObservation>,
    entries: Vec<RenderEntry>,
    baseline_identity: String,
}

#[derive(Default)]
struct VisibleInstructionState {
    baseline_identity: Option<String>,
    saw_baseline: bool,
    changes: BTreeMap<String, InstructionChange>,
}

/// Compose the one context message that should enter the next non-empty turn.
pub(crate) async fn prepare_workspace_instructions(
    session: &Session,
    authority: &WorkspaceAuthority,
    cancellation: &CancellationToken,
) -> Result<Option<Message>, WorkspaceInstructionError> {
    if cancellation.is_cancelled() {
        return Err(WorkspaceInstructionError::Cancelled);
    }
    let root = Arc::clone(authority.root());
    let workspace = authority.canonical_path().to_owned();
    let dsh_home = resolve_dsh_home();
    let token = cancellation.clone();
    let loaded = task::spawn_blocking(move || load_workspace(root, workspace, dsh_home, &token))
        .await
        .map_err(|_| WorkspaceInstructionError::Task)??;
    if cancellation.is_cancelled() {
        return Err(WorkspaceInstructionError::Cancelled);
    }

    let visible = visible_instruction_state(session);
    let message = reconcile(session, loaded, visible)?;
    if cancellation.is_cancelled() {
        return Err(WorkspaceInstructionError::Cancelled);
    }
    Ok(message)
}

fn resolve_dsh_home() -> Option<(PathBuf, &'static str)> {
    if let Some(value) = std::env::var_os("DSH_HOME") {
        let path = PathBuf::from(value);
        return path.is_absolute().then_some((path, "$DSH_HOME/AGENTS.md"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute()
        .then(|| (home.join(".dsh"), "~/.dsh/AGENTS.md"))
}

fn load_workspace(
    root: Arc<Dir>,
    workspace: PathBuf,
    dsh_home: Option<(PathBuf, &'static str)>,
    cancellation: &CancellationToken,
) -> Result<LoadedWorkspaceInstructions, WorkspaceInstructionError> {
    let baseline_identity = serde_json::to_string(&json!({
        "projectRoot": "",
        "projectRootMarkers": [".git"],
        "maxBytes": MAX_WORKSPACE_INSTRUCTION_BYTES,
        "maxSourceBytes": MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES,
        "instructionFileCandidates": ["AGENTS.md", "CLAUDE.md"],
        "localInstructionFileCandidates": ["AGENTS.local.md", "CLAUDE.local.md"]
    }))
    .map_err(|_| WorkspaceInstructionError::Message)?;
    let mut observations = Vec::with_capacity(1 + PROJECT_CANDIDATES.len());
    let mut global_path = None;
    if let Some((home, display)) = dsh_home {
        cancellation_check(cancellation)?;
        let path = home.join("AGENTS.md");
        global_path = Some(path.clone());
        observations.push(CandidateObservation {
            scope: scope_key("user-global", "AGENTS.md"),
            state: observe_ambient(&path, display, cancellation),
        });
    } else {
        observations.push(CandidateObservation {
            scope: scope_key("user-global", "AGENTS.md"),
            state: CandidateState::Unavailable,
        });
    }

    for candidate in PROJECT_CANDIDATES {
        cancellation_check(cancellation)?;
        let project_path = workspace.join(candidate);
        let state = if global_path.as_ref() == Some(&project_path)
            && observations
                .first()
                .is_some_and(|observation| matches!(observation.state, CandidateState::Present(_)))
        {
            CandidateState::Absent
        } else {
            observe_workspace(&root, candidate, cancellation)
        };
        observations.push(CandidateObservation {
            scope: scope_key(".", candidate),
            state,
        });
    }

    let mut seen_project_content = BTreeSet::new();
    let mut entries = Vec::with_capacity(observations.len());
    for (index, observation) in observations.iter_mut().enumerate() {
        let CandidateState::Present(instruction) = &observation.state else {
            continue;
        };
        if index != 0 {
            let trimmed = sha1_hex(instruction.content.trim().as_bytes());
            if !seen_project_content.insert(trimmed) {
                observation.state = CandidateState::Absent;
                continue;
            }
        }
        entries.push(RenderEntry {
            instruction: instruction.clone(),
            kind: RenderKind::Baseline,
        });
    }
    cancellation_check(cancellation)?;
    Ok(LoadedWorkspaceInstructions {
        observations,
        entries,
        baseline_identity,
    })
}

fn observe_workspace(
    root: &Dir,
    candidate: &str,
    cancellation: &CancellationToken,
) -> CandidateState {
    let metadata = match root.symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if is_absent(&error) => return CandidateState::Absent,
        Err(_) => return CandidateState::Unavailable,
    };
    if metadata.file_type().is_symlink() {
        return CandidateState::Unavailable;
    }
    if !metadata.is_file() {
        return CandidateState::Absent;
    }
    if metadata.len() > MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES as u64 {
        return CandidateState::Unavailable;
    }
    let file = match root.open(candidate) {
        Ok(file) => file,
        Err(_) => return CandidateState::Unavailable,
    };
    read_instruction(file, candidate, scope_key(".", candidate), cancellation)
}

fn observe_ambient(path: &Path, display: &str, cancellation: &CancellationToken) -> CandidateState {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if is_absent(&error) => return CandidateState::Absent,
        Err(_) => return CandidateState::Unavailable,
    };
    if metadata.file_type().is_symlink() {
        return CandidateState::Unavailable;
    }
    if !metadata.is_file() {
        return CandidateState::Absent;
    }
    if metadata.len() > MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES as u64 {
        return CandidateState::Unavailable;
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return CandidateState::Unavailable,
    };
    read_instruction(
        file,
        display,
        scope_key("user-global", "AGENTS.md"),
        cancellation,
    )
}

fn read_instruction(
    mut file: impl Read,
    display_path: &str,
    scope: String,
    cancellation: &CancellationToken,
) -> CandidateState {
    if cancellation.is_cancelled() {
        return CandidateState::Unavailable;
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(8 * 1024).is_err() {
        return CandidateState::Unavailable;
    }
    if file
        .by_ref()
        .take((MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES
        || cancellation.is_cancelled()
    {
        return CandidateState::Unavailable;
    }
    let Ok(content) = String::from_utf8(bytes) else {
        return CandidateState::Unavailable;
    };
    CandidateState::Present(LoadedInstruction {
        scope,
        display_path: display_path.to_owned(),
        digest: sha1_hex(content.as_bytes()),
        content,
    })
}

fn reconcile(
    session: &Session,
    loaded: LoadedWorkspaceInstructions,
    visible: VisibleInstructionState,
) -> Result<Option<Message>, WorkspaceInstructionError> {
    let compatible = visible.saw_baseline
        && visible.baseline_identity.as_deref() == Some(&loaded.baseline_identity);
    if !visible.saw_baseline {
        if loaded.entries.is_empty() {
            return Ok(None);
        }
        let rendered = render_entries(
            &loaded.entries,
            MAX_WORKSPACE_INSTRUCTION_BYTES,
            Some(BASELINE_INTRO),
        );
        if rendered.text.is_empty() {
            return Ok(None);
        }
        let changes = represented_set_changes(&loaded.entries, &rendered.represented);
        return build_message(
            session,
            rendered.text,
            true,
            Some(loaded.baseline_identity),
            changes,
        )
        .map(Some);
    }

    if !compatible {
        if loaded
            .observations
            .iter()
            .any(|observation| matches!(observation.state, CandidateState::Unavailable))
        {
            return Ok(None);
        }
        let intro = if loaded.entries.is_empty() {
            EMPTY_REPLACEMENT_INTRO
        } else {
            REPLACEMENT_INTRO
        };
        let rendered = render_entries(
            &loaded.entries,
            MAX_WORKSPACE_INSTRUCTION_BYTES,
            Some(intro),
        );
        let mut changes =
            removals_not_represented(&visible.changes, &loaded.entries, &rendered.represented);
        changes.extend(represented_set_changes(
            &loaded.entries,
            &rendered.represented,
        ));
        return build_message(
            session,
            rendered.text,
            true,
            Some(loaded.baseline_identity),
            changes,
        )
        .map(Some);
    }

    let current = render_entries(
        &loaded.entries,
        MAX_WORKSPACE_INSTRUCTION_BYTES,
        Some(BASELINE_INTRO),
    );
    let desired = represented_desired(&loaded.entries, &current.represented);
    let mut change_entries = Vec::new();
    for observation in &loaded.observations {
        match desired.get(&observation.scope) {
            Some(current) => match visible.changes.get(&observation.scope) {
                Some(previous)
                    if previous.action != ChangeAction::Remove
                        && previous.path == current.display_path
                        && previous.digest.as_deref() == Some(&current.digest) => {}
                Some(previous) if previous.action != ChangeAction::Remove => {
                    change_entries.push(RenderEntry {
                        instruction: (*current).clone(),
                        kind: RenderKind::Change(ChangeAction::Replace),
                    });
                }
                _ => change_entries.push(RenderEntry {
                    instruction: (*current).clone(),
                    kind: RenderKind::Change(ChangeAction::Set),
                }),
            },
            None if matches!(observation.state, CandidateState::Unavailable) => {}
            None => {
                if let Some(previous) = visible.changes.get(&observation.scope) {
                    if previous.action != ChangeAction::Remove {
                        change_entries.push(RenderEntry {
                            instruction: LoadedInstruction {
                                scope: observation.scope.clone(),
                                display_path: previous.path.clone(),
                                content: String::new(),
                                digest: String::new(),
                            },
                            kind: RenderKind::Change(ChangeAction::Remove),
                        });
                    }
                }
            }
        }
    }
    if change_entries.is_empty() {
        return Ok(None);
    }
    let rendered = render_entries(&change_entries, MAX_WORKSPACE_INSTRUCTION_BYTES, None);
    let changes = represented_change_entries(&change_entries, &rendered.represented);
    if changes.is_empty() {
        return Ok(None);
    }
    build_message(session, rendered.text, false, None, changes).map(Some)
}

fn visible_instruction_state(session: &Session) -> VisibleInstructionState {
    let mut visible = VisibleInstructionState::default();
    for message in session.visible_messages() {
        let source = message.source().raw().as_value();
        let Some(fields) = source.as_object() else {
            continue;
        };
        if fields.get("kind").and_then(Value::as_str) != Some("agent-instructions") {
            continue;
        }
        if fields.get("baseline").and_then(Value::as_bool) == Some(true) {
            visible.saw_baseline = true;
            visible.baseline_identity = fields
                .get("baselineIdentity")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        let Some(changes) = fields.get("changes").and_then(Value::as_array) else {
            continue;
        };
        for change in changes {
            let Some(change) = parse_change(change) else {
                continue;
            };
            visible.changes.insert(change.scope.clone(), change);
        }
    }
    visible
}

fn parse_change(value: &Value) -> Option<InstructionChange> {
    let fields = value.as_object()?;
    let action = match fields.get("action")?.as_str()? {
        "set" => ChangeAction::Set,
        "replace" => ChangeAction::Replace,
        "remove" => ChangeAction::Remove,
        _ => return None,
    };
    Some(InstructionChange {
        action,
        scope: fields.get("scope")?.as_str()?.to_owned(),
        path: fields.get("path")?.as_str()?.to_owned(),
        digest: fields
            .get("digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn build_message(
    session: &Session,
    text: String,
    baseline: bool,
    baseline_identity: Option<String>,
    changes: Vec<InstructionChange>,
) -> Result<Message, WorkspaceInstructionError> {
    let seq = session
        .next_seq()
        .ok_or(WorkspaceInstructionError::Identity)?;
    let changes = changes
        .into_iter()
        .map(|change| {
            let mut value = json!({
                "action": change.action.as_str(),
                "scope": change.scope,
                "path": change.path
            });
            if let Some(content_digest) = change.digest {
                value["digest"] = Value::String(content_digest);
            }
            value
        })
        .collect::<Vec<_>>();
    let mut source = json!({
        "kind": "agent-instructions",
        "form": "instructions",
        "changes": changes
    });
    if baseline {
        source["baseline"] = Value::Bool(true);
        if let Some(identity) = baseline_identity {
            source["baselineIdentity"] = Value::String(identity);
        }
    }
    Message::user(
        format!("workspace-instructions-{}", seq.get()),
        vec![ContentBlock::text(text).map_err(|_| WorkspaceInstructionError::Message)?],
        MessageSource::from_value(source).map_err(|_| WorkspaceInstructionError::Message)?,
    )
    .map_err(|_| WorkspaceInstructionError::Message)
}

fn represented_desired<'a>(
    entries: &'a [RenderEntry],
    represented: &[usize],
) -> BTreeMap<String, &'a LoadedInstruction> {
    represented
        .iter()
        .map(|index| {
            let instruction = &entries[*index].instruction;
            (instruction.scope.clone(), instruction)
        })
        .collect()
}

fn represented_set_changes(
    entries: &[RenderEntry],
    represented: &[usize],
) -> Vec<InstructionChange> {
    represented
        .iter()
        .map(|index| {
            let instruction = &entries[*index].instruction;
            InstructionChange {
                action: ChangeAction::Set,
                scope: instruction.scope.clone(),
                path: instruction.display_path.clone(),
                digest: Some(instruction.digest.clone()),
            }
        })
        .collect()
}

fn represented_change_entries(
    entries: &[RenderEntry],
    represented: &[usize],
) -> Vec<InstructionChange> {
    represented
        .iter()
        .filter_map(|index| {
            let entry = &entries[*index];
            let RenderKind::Change(action) = entry.kind else {
                return None;
            };
            Some(InstructionChange {
                action,
                scope: entry.instruction.scope.clone(),
                path: entry.instruction.display_path.clone(),
                digest: (action != ChangeAction::Remove).then(|| entry.instruction.digest.clone()),
            })
        })
        .collect()
}

fn removals_not_represented(
    previous: &BTreeMap<String, InstructionChange>,
    entries: &[RenderEntry],
    represented: &[usize],
) -> Vec<InstructionChange> {
    let retained = represented
        .iter()
        .map(|index| entries[*index].instruction.scope.as_str())
        .collect::<BTreeSet<_>>();
    previous
        .values()
        .filter(|change| change.action != ChangeAction::Remove)
        .filter(|change| !retained.contains(change.scope.as_str()))
        .map(|change| InstructionChange {
            action: ChangeAction::Remove,
            scope: change.scope.clone(),
            path: change.path.clone(),
            digest: None,
        })
        .collect()
}

fn render_entries(
    entries: &[RenderEntry],
    max_bytes: usize,
    intro: Option<&str>,
) -> RenderedEntries {
    if max_bytes == 0 {
        return RenderedEntries {
            text: String::new(),
            represented: Vec::new(),
        };
    }
    let full = build_rendered_text(entries, intro, max_bytes, &[], &[]);
    if full.len() <= max_bytes {
        return RenderedEntries {
            text: full,
            represented: (0..entries.len()).collect(),
        };
    }
    for start in 1..entries.len() {
        let omitted = entries[..start]
            .iter()
            .map(|entry| entry.instruction.display_path.clone())
            .collect::<Vec<_>>();
        let candidate = build_rendered_text(&entries[start..], intro, max_bytes, &omitted, &[]);
        if candidate.len() <= max_bytes {
            return RenderedEntries {
                text: candidate,
                represented: (start..entries.len()).collect(),
            };
        }
    }
    let Some(last) = entries.last() else {
        return RenderedEntries {
            text: truncate_utf8(&full, max_bytes).to_owned(),
            represented: Vec::new(),
        };
    };
    let original_bytes = last.instruction.content.len();
    let omitted = entries[..entries.len() - 1]
        .iter()
        .map(|entry| entry.instruction.display_path.clone())
        .collect::<Vec<_>>();
    for candidate_intro in [intro, Some(COMPACT_INTRO)] {
        let mut low = 0_usize;
        let mut high = original_bytes;
        let mut best = 0_usize;
        while low <= high {
            let middle = low + (high - low) / 2;
            let content = truncate_utf8(&last.instruction.content, middle);
            let mut candidate = last.clone();
            candidate.instruction.content = content.to_owned();
            let truncated = [(
                last.instruction.display_path.clone(),
                original_bytes,
                content.len(),
            )];
            let text = build_rendered_text(
                std::slice::from_ref(&candidate),
                candidate_intro,
                max_bytes,
                &omitted,
                &truncated,
            );
            if text.len() <= max_bytes {
                best = content.len();
                low = middle.saturating_add(1);
            } else if middle == 0 {
                break;
            } else {
                high = middle - 1;
            }
        }
        let content = truncate_utf8(&last.instruction.content, best);
        let mut candidate = last.clone();
        candidate.instruction.content = content.to_owned();
        let truncated = [(
            last.instruction.display_path.clone(),
            original_bytes,
            content.len(),
        )];
        let text = build_rendered_text(
            std::slice::from_ref(&candidate),
            candidate_intro,
            max_bytes,
            &omitted,
            &truncated,
        );
        if text.len() <= max_bytes {
            return RenderedEntries {
                text,
                represented: if best > 0
                    || original_bytes == 0
                    || matches!(last.kind, RenderKind::Change(ChangeAction::Remove))
                {
                    vec![entries.len() - 1]
                } else {
                    Vec::new()
                },
            };
        }
    }
    let marker = escape_frame(&budget_marker(
        max_bytes,
        &omitted,
        &[(last.instruction.display_path.clone(), original_bytes, 0)],
    ));
    RenderedEntries {
        text: truncate_utf8(&marker, max_bytes).to_owned(),
        represented: Vec::new(),
    }
}

fn build_rendered_text(
    entries: &[RenderEntry],
    intro: Option<&str>,
    max_bytes: usize,
    omitted: &[String],
    truncated: &[(String, usize, usize)],
) -> String {
    let mut blocks = Vec::new();
    let marker = budget_marker(max_bytes, omitted, truncated);
    if !marker.is_empty() {
        blocks.push(marker);
    }
    if let Some(intro) = intro {
        if !intro.is_empty() {
            blocks.push(intro.to_owned());
        }
    }
    blocks.extend(entries.iter().map(section_text));
    format!(
        "<system-reminder>\n{}\n</system-reminder>",
        escape_frame(&blocks.join("\n\n"))
    )
}

fn section_text(entry: &RenderEntry) -> String {
    match entry.kind {
        RenderKind::Baseline => format!(
            "Instructions from: {}\n\n{}",
            entry.instruction.display_path, entry.instruction.content
        ),
        RenderKind::Change(ChangeAction::Set) => format!(
            "Additional instructions from: {}\n\nThese instructions apply to work under `{}`. Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.\n\n{}",
            entry.instruction.display_path,
            scope_directory(&entry.instruction.scope),
            entry.instruction.content
        ),
        RenderKind::Change(ChangeAction::Replace) => format!(
            "Updated instructions from: {}\n\nThis file changed after it was loaded. Use the following content instead of the previously loaded instructions from this file.\n\n{}",
            entry.instruction.display_path, entry.instruction.content
        ),
        RenderKind::Change(ChangeAction::Remove) => format!(
            "Instructions removed: {}\n\nThe previously loaded instructions from this file no longer apply.",
            entry.instruction.display_path
        ),
    }
}

fn budget_marker(
    max_bytes: usize,
    omitted: &[String],
    truncated: &[(String, usize, usize)],
) -> String {
    let mut facts = Vec::new();
    if !omitted.is_empty() {
        facts.push(format!("omitted {}", omitted.join(", ")));
    }
    if !truncated.is_empty() {
        facts.push(format!(
            "truncated {}",
            truncated
                .iter()
                .map(|(path, original, included)| {
                    format!("{path} from {original} to {included} bytes")
                })
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if facts.is_empty() {
        String::new()
    } else {
        format!(
            "Workspace instruction budget {max_bytes} bytes: {}",
            facts.join("; ")
        )
    }
}

fn escape_frame(value: &str) -> String {
    value.replace("</system-reminder>", "<\\/system-reminder>")
}

fn truncate_utf8(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn scope_key(directory: &str, candidate: &str) -> String {
    format!("{directory}\0{candidate}")
}

fn scope_directory(scope: &str) -> &str {
    scope
        .split_once('\0')
        .map_or(scope, |(directory, _)| directory)
}

fn sha1_hex(value: &[u8]) -> String {
    use std::fmt::Write as _;

    let bytes = digest(&SHA1_FOR_LEGACY_USE_ONLY, value);
    let mut encoded = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn is_absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn cancellation_check(cancellation: &CancellationToken) -> Result<(), WorkspaceInstructionError> {
    if cancellation.is_cancelled() {
        Err(WorkspaceInstructionError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{
        session::{EventKind, NewEvent, StepId, SurfaceIntent, TurnEndReason},
        workspace_authority::WorkspaceAuthority,
    };

    use super::*;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("dsh-{label}-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn text(message: &Message) -> &str {
        let crate::model::ContentBlockKind::Text { text } = message.content()[0].kind() else {
            panic!("workspace context must contain text")
        };
        text
    }

    fn append_context(session: &mut Session, message: Message) {
        let turn = session.state().next_turn();
        let step = StepId::new(1).unwrap();
        session
            .append(NewEvent::log(EventKind::turn_start(turn)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_start(turn, step)))
            .unwrap();
        session
            .append(NewEvent::surface(
                EventKind::user_message(message),
                SurfaceIntent::append(),
            ))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::step_end(turn, step)))
            .unwrap();
        session
            .append(NewEvent::log(EventKind::turn_end(
                turn,
                TurnEndReason::Completed,
            )))
            .unwrap();
    }

    #[tokio::test]
    async fn baseline_uses_official_order_dedup_framing_and_source_shape() {
        let root = TempRoot::new("workspace-instructions-baseline");
        let home = TempRoot::new("workspace-instructions-home");
        fs::create_dir(home.0.join(".dsh")).unwrap();
        fs::write(home.0.join(".dsh/AGENTS.md"), "global rule").unwrap();
        fs::write(root.0.join("AGENTS.md"), "root rule\n").unwrap();
        fs::write(root.0.join("CLAUDE.md"), "  root rule\n\n").unwrap();
        fs::write(
            root.0.join("AGENTS.local.md"),
            "local </system-reminder> rule",
        )
        .unwrap();
        let authority = WorkspaceAuthority::open(&root.0).unwrap();
        let session = Session::new("workspace-instructions-baseline").unwrap();
        // This test calls the blocking loader directly so it does not mutate the
        // process-global HOME used by parallel tests.
        let loaded = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            Some((home.0.join(".dsh"), "~/.dsh/AGENTS.md")),
            &CancellationToken::new(),
        )
        .unwrap();
        let message = reconcile(&session, loaded, VisibleInstructionState::default())
            .unwrap()
            .unwrap();
        let rendered = text(&message);
        let global = rendered
            .find("Instructions from: ~/.dsh/AGENTS.md")
            .unwrap();
        let root_rule = rendered.find("Instructions from: AGENTS.md").unwrap();
        let local = rendered.find("Instructions from: AGENTS.local.md").unwrap();
        assert!(global < root_rule && root_rule < local);
        assert!(!rendered.contains("Instructions from: CLAUDE.md"));
        assert_eq!(rendered.matches("</system-reminder>").count(), 1);
        assert!(rendered.contains("<\\/system-reminder>"));
        let source = message.source().raw().as_value();
        assert_eq!(source["kind"], "agent-instructions");
        assert_eq!(source["form"], "instructions");
        assert_eq!(source["baseline"], true);
        assert_eq!(source["changes"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn renderer_preserves_the_most_specific_suffix_and_utf8_boundary() {
        let entries = [
            RenderEntry {
                instruction: LoadedInstruction {
                    scope: scope_key(".", "AGENTS.md"),
                    display_path: "AGENTS.md".to_owned(),
                    content: "broad ".repeat(200),
                    digest: "broad".to_owned(),
                },
                kind: RenderKind::Baseline,
            },
            RenderEntry {
                instruction: LoadedInstruction {
                    scope: scope_key(".", "AGENTS.local.md"),
                    display_path: "AGENTS.local.md".to_owned(),
                    content: "界".repeat(300),
                    digest: "specific".to_owned(),
                },
                kind: RenderKind::Baseline,
            },
        ];
        let rendered = render_entries(&entries, 500, Some(BASELINE_INTRO));
        assert!(rendered.text.len() <= 500);
        assert!(rendered.text.contains("omitted AGENTS.md"));
        assert!(rendered.text.contains("truncated AGENTS.local.md"));
        assert!(rendered.text.is_char_boundary(rendered.text.len()));
        assert_eq!(rendered.represented, vec![1]);
    }

    #[tokio::test]
    async fn unchanged_resume_is_silent_and_changed_or_removed_files_append_transitions() {
        let root = TempRoot::new("workspace-instructions-resume");
        fs::write(root.0.join("AGENTS.md"), "first").unwrap();
        let authority = WorkspaceAuthority::open(&root.0).unwrap();
        let mut session = Session::new("workspace-instructions-resume").unwrap();
        let first = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        let message = reconcile(&session, first, VisibleInstructionState::default())
            .unwrap()
            .unwrap();
        append_context(&mut session, message);

        let unchanged = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(
            reconcile(&session, unchanged, visible_instruction_state(&session))
                .unwrap()
                .is_none()
        );

        fs::write(root.0.join("AGENTS.md"), "second").unwrap();
        fs::write(root.0.join("CLAUDE.md"), "new sibling").unwrap();
        let changed = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        let update = reconcile(&session, changed, visible_instruction_state(&session))
            .unwrap()
            .unwrap();
        assert!(text(&update).contains("Updated instructions from: AGENTS.md"));
        assert!(text(&update).contains("Additional instructions from: CLAUDE.md"));
        assert_eq!(
            update.source().raw().as_value()["changes"][0]["action"],
            "replace"
        );
        assert_eq!(
            update.source().raw().as_value()["changes"][1]["action"],
            "set"
        );
        append_context(&mut session, update);

        fs::remove_file(root.0.join("AGENTS.md")).unwrap();
        let removed = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        let removal = reconcile(&session, removed, visible_instruction_state(&session))
            .unwrap()
            .unwrap();
        assert!(text(&removal).contains("Instructions removed: AGENTS.md"));
        assert!(!text(&removal).contains("Additional instructions from: CLAUDE.md"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_invalid_and_symlink_candidates_are_unavailable_not_loaded() {
        use std::os::unix::fs::symlink;

        let root = TempRoot::new("workspace-instructions-unavailable");
        let outside = TempRoot::new("workspace-instructions-outside");
        fs::write(root.0.join("AGENTS.md"), "previous visible rule").unwrap();
        let authority = WorkspaceAuthority::open(&root.0).unwrap();
        let mut session = Session::new("workspace-instructions-unavailable").unwrap();
        let first = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        let baseline = reconcile(&session, first, VisibleInstructionState::default())
            .unwrap()
            .unwrap();
        append_context(&mut session, baseline);

        fs::write(outside.0.join("secret"), "must not load").unwrap();
        fs::remove_file(root.0.join("AGENTS.md")).unwrap();
        symlink(outside.0.join("secret"), root.0.join("AGENTS.md")).unwrap();
        fs::write(root.0.join("CLAUDE.md"), [0xff, 0xfe]).unwrap();
        fs::write(
            root.0.join("AGENTS.local.md"),
            vec![b'x'; MAX_WORKSPACE_INSTRUCTION_SOURCE_BYTES + 1],
        )
        .unwrap();
        let loaded = load_workspace(
            Arc::clone(authority.root()),
            authority.canonical_path().to_owned(),
            None,
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(loaded.entries.is_empty());
        assert!(
            loaded
                .observations
                .iter()
                .skip(1)
                .take(3)
                .all(|observation| { matches!(observation.state, CandidateState::Unavailable) })
        );
        assert!(
            reconcile(&session, loaded, visible_instruction_state(&session))
                .unwrap()
                .is_none(),
            "temporary unavailability must not revoke visible instructions"
        );
    }

    #[tokio::test]
    async fn cancellation_stops_before_instruction_work() {
        let root = TempRoot::new("workspace-instructions-cancel");
        let authority = WorkspaceAuthority::open(&root.0).unwrap();
        let session = Session::new("workspace-instructions-cancel").unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            prepare_workspace_instructions(&session, &authority, &cancellation).await,
            Err(WorkspaceInstructionError::Cancelled)
        ));
    }
}
