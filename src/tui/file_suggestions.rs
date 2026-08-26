//! Pure workspace-file token detection, ranking, and presentation facts.

use std::{cmp::Ordering, collections::HashMap, fmt};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::composer::{Composer, MAX_PROMPT_BYTES};

pub(crate) const MAX_FILE_QUERY_BYTES: usize = 1_024;
pub(crate) const MAX_FILE_CANDIDATES: usize = 256;
pub(crate) const MAX_FILE_CANDIDATE_BYTES: usize = 256 * 1_024;
const MAX_MATCH_INSPECTIONS: usize = 64 * 1_024 * 1_024;
const MATCH_CANCEL_INTERVAL: usize = 4 * 1_024;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum FileSuggestionError {
    #[error("CLI_FILE_SUGGESTION_CANCELLED")]
    Cancelled,
    #[error("CLI_FILE_SUGGESTION_CAPACITY")]
    Capacity,
    #[error("CLI_FILE_SUGGESTION_LIMIT")]
    Limit,
    #[error("CLI_FILE_SUGGESTION_INVALID")]
    Invalid,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FileSuggestionSnapshot<'a> {
    Hidden,
    Loading,
    Ready {
        candidates: &'a [String],
        selected: usize,
        capped: bool,
    },
    Empty,
    Unavailable,
}

impl fmt::Debug for FileSuggestionSnapshot<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => formatter.write_str("Hidden"),
            Self::Loading => formatter.write_str("Loading"),
            Self::Ready {
                candidates,
                selected,
                capped,
            } => formatter
                .debug_struct("Ready")
                .field("candidates", &candidates.len())
                .field(
                    "candidate_bytes",
                    &candidates.iter().map(String::len).sum::<usize>(),
                )
                .field("selected", selected)
                .field("capped", capped)
                .finish(),
            Self::Empty => formatter.write_str("Empty"),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

impl FileSuggestionSnapshot<'_> {
    pub(crate) const fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct FileTokenHit {
    start: usize,
    end: usize,
    composer_revision: u64,
    query: String,
}

impl fmt::Debug for FileTokenHit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileTokenHit")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("composer_revision", &self.composer_revision)
            .field("query_bytes", &self.query.len())
            .finish()
    }
}

impl FileTokenHit {
    pub(crate) fn detect(composer: &Composer) -> Result<Option<Self>, FileSuggestionError> {
        let text = composer.text();
        let cursor = composer.cursor();
        if cursor == 0 || cursor > text.len() || !text.is_char_boundary(cursor) {
            return Ok(None);
        }
        if text[cursor..]
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace())
        {
            return Ok(None);
        }

        let before = &text[..cursor];
        let token_start = before
            .char_indices()
            .rev()
            .find_map(|(index, character)| {
                character
                    .is_whitespace()
                    .then_some(index + character.len_utf8())
            })
            .unwrap_or(0);
        let token = &text[token_start..cursor];
        let trigger = token.char_indices().rev().find_map(|(offset, character)| {
            if character != '@' {
                return None;
            }
            let start = token_start + offset;
            let boundary_ok = text[..start].chars().next_back().is_none_or(|previous| {
                previous.is_whitespace() || (!previous.is_alphanumeric() && previous != '_')
            });
            boundary_ok.then_some(start)
        });
        let Some(start) = trigger else {
            return Ok(None);
        };
        let query = &text[start + 1..cursor];
        if query.len() > MAX_FILE_QUERY_BYTES || query.chars().any(char::is_control) {
            return Ok(None);
        }
        let query = try_copy(query)?;
        Ok(Some(Self {
            start,
            end: cursor,
            composer_revision: composer.content_revision(),
            query,
        }))
    }

    pub(crate) fn start(&self) -> usize {
        self.start
    }

    pub(crate) fn end(&self) -> usize {
        self.end
    }

    pub(crate) fn composer_revision(&self) -> u64 {
        self.composer_revision
    }

    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn span_bytes(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub(crate) fn try_clone_bounded(&self) -> Result<Self, FileSuggestionError> {
        Ok(Self {
            start: self.start,
            end: self.end,
            composer_revision: self.composer_revision,
            query: try_copy(&self.query)?,
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RankedFileSnapshot {
    candidates: Vec<String>,
    total_bytes: usize,
    capped: bool,
}

impl fmt::Debug for RankedFileSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RankedFileSnapshot")
            .field("candidates", &self.candidates.len())
            .field("total_bytes", &self.total_bytes)
            .field("capped", &self.capped)
            .finish()
    }
}

impl RankedFileSnapshot {
    pub(crate) fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub(crate) fn count(&self) -> usize {
        self.candidates.len()
    }

    pub(crate) fn is_capped(&self) -> bool {
        self.capped
    }

    pub(crate) fn candidate(&self, index: usize) -> Option<&str> {
        self.candidates.get(index).map(String::as_str)
    }

    pub(crate) fn selected_index(&self, previous: Option<&str>) -> Option<usize> {
        if self.candidates.is_empty() {
            return None;
        }
        previous
            .and_then(|path| {
                self.candidates
                    .iter()
                    .position(|candidate| candidate == path)
            })
            .or(Some(0))
    }

    pub(crate) fn try_clone_bounded(&self) -> Result<Self, FileSuggestionError> {
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(self.candidates.len())
            .map_err(|_| FileSuggestionError::Capacity)?;
        for candidate in &self.candidates {
            candidates.push(try_copy(candidate)?);
        }
        Ok(Self {
            candidates,
            total_bytes: self.total_bytes,
            capped: self.capped,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MatchScore {
    class: u8,
    index: usize,
    path_bytes: usize,
    ordinal: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RankedIndex {
    score: MatchScore,
    ordinal: usize,
}

impl Ord for RankedIndex {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score.cmp(&other.score)
    }
}

impl PartialOrd for RankedIndex {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn rank_catalogue(
    catalogue: &[String],
    hit: &FileTokenHit,
    draft_bytes: usize,
    cancellation: &CancellationToken,
) -> Result<RankedFileSnapshot, FileSuggestionError> {
    if hit.query.len() > MAX_FILE_QUERY_BYTES || draft_bytes > MAX_PROMPT_BYTES {
        return Err(FileSuggestionError::Invalid);
    }
    check_cancel(cancellation)?;
    let mut inspections = InspectionBudget::default();
    let sensitive = KmpPattern::new(&hit.query, false, &mut inspections, cancellation)?;
    let folded = KmpPattern::new(&hit.query, true, &mut inspections, cancellation)?;
    let mut heap = Vec::new();
    heap.try_reserve_exact(MAX_FILE_CANDIDATES)
        .map_err(|_| FileSuggestionError::Capacity)?;
    let mut matched = 0_usize;
    let mut seen: HashMap<u64, Vec<usize>> = HashMap::new();
    seen.try_reserve(catalogue.len())
        .map_err(|_| FileSuggestionError::Capacity)?;

    for (ordinal, path) in catalogue.iter().enumerate() {
        check_cancel(cancellation)?;
        validate_catalogue_path(path, &mut inspections, cancellation)?;
        let hash = hash_path(path, &mut inspections, cancellation)?;
        let bucket = seen.entry(hash).or_default();
        let mut duplicate = false;
        for prior in bucket.iter().copied() {
            let prior = catalogue.get(prior).ok_or(FileSuggestionError::Invalid)?;
            if bytes_equal(path, prior, false, &mut inspections, cancellation)? {
                duplicate = true;
                break;
            }
        }
        if duplicate {
            continue;
        }
        bucket
            .try_reserve(1)
            .map_err(|_| FileSuggestionError::Capacity)?;
        bucket.push(ordinal);
        if !completion_fits(draft_bytes, hit.span_bytes(), path.len())? {
            continue;
        }
        let score = match_score(
            path,
            &hit.query,
            &sensitive,
            &folded,
            ordinal,
            &mut inspections,
            cancellation,
        )?;
        let Some(score) = score else {
            continue;
        };
        matched = matched.checked_add(1).ok_or(FileSuggestionError::Limit)?;
        let _ = push_best(&mut heap, RankedIndex { score, ordinal });
    }

    heap.sort_unstable_by(|left, right| left.score.cmp(&right.score));
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(heap.len())
        .map_err(|_| FileSuggestionError::Capacity)?;
    let mut total_bytes = 0_usize;
    let mut capped = matched > heap.len();
    for ranked in heap {
        let path = catalogue
            .get(ranked.ordinal)
            .ok_or(FileSuggestionError::Invalid)?;
        let next = total_bytes
            .checked_add(path.len())
            .ok_or(FileSuggestionError::Limit)?;
        if next > MAX_FILE_CANDIDATE_BYTES {
            capped = true;
            break;
        }
        candidates.push(try_copy_inspected(path, &mut inspections, cancellation)?);
        total_bytes = next;
    }
    Ok(RankedFileSnapshot {
        candidates,
        total_bytes,
        capped,
    })
}

fn match_score(
    path: &str,
    query: &str,
    sensitive: &KmpPattern,
    folded: &KmpPattern,
    ordinal: usize,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<Option<MatchScore>, FileSuggestionError> {
    if query.is_empty() {
        return Ok(Some(MatchScore {
            class: 0,
            index: 0,
            path_bytes: 0,
            ordinal,
        }));
    }
    if bytes_equal(path, query, false, inspections, cancellation)? {
        return Ok(Some(score(0, 0, path, ordinal)));
    }
    if bytes_start_with(path, query, false, inspections, cancellation)? {
        return Ok(Some(score(1, 0, path, ordinal)));
    }
    if let Some(index) = component_prefix(path, query, false, inspections, cancellation)? {
        return Ok(Some(score(2, index, path, ordinal)));
    }
    if let Some(index) = sensitive.find(path.as_bytes(), inspections, cancellation)? {
        return Ok(Some(score(3, index, path, ordinal)));
    }
    if bytes_start_with(path, query, true, inspections, cancellation)? {
        return Ok(Some(score(4, 0, path, ordinal)));
    }
    if let Some(index) = component_prefix(path, query, true, inspections, cancellation)? {
        return Ok(Some(score(5, index, path, ordinal)));
    }
    Ok(folded
        .find(path.as_bytes(), inspections, cancellation)?
        .map(|index| score(6, index, path, ordinal)))
}

const fn score(class: u8, index: usize, path: &str, ordinal: usize) -> MatchScore {
    MatchScore {
        class,
        index,
        path_bytes: path.len(),
        ordinal,
    }
}

fn push_best(heap: &mut Vec<RankedIndex>, candidate: RankedIndex) -> usize {
    let mut comparisons = 0_usize;
    if heap.len() < MAX_FILE_CANDIDATES {
        heap.push(candidate);
        let mut index = heap.len() - 1;
        while index != 0 {
            let parent = (index - 1) / 2;
            comparisons += 1;
            if heap[parent] >= heap[index] {
                break;
            }
            heap.swap(parent, index);
            index = parent;
        }
        return comparisons;
    }
    comparisons += 1;
    if heap.first().is_none_or(|worst| candidate >= *worst) {
        return comparisons;
    }
    heap[0] = candidate;
    let mut index = 0_usize;
    loop {
        let left = index * 2 + 1;
        if left >= heap.len() {
            break;
        }
        let right = left + 1;
        let child = if right < heap.len() {
            comparisons += 1;
            if heap[right] > heap[left] {
                right
            } else {
                left
            }
        } else {
            left
        };
        comparisons += 1;
        if heap[index] >= heap[child] {
            break;
        }
        heap.swap(index, child);
        index = child;
    }
    comparisons
}

fn component_prefix(
    path: &str,
    query: &str,
    folded: bool,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<Option<usize>, FileSuggestionError> {
    let mut start = 0_usize;
    loop {
        let tail = path.get(start..).ok_or(FileSuggestionError::Invalid)?;
        let component_end = find_component_end(tail, inspections, cancellation)?;
        let component = tail
            .get(..component_end)
            .ok_or(FileSuggestionError::Invalid)?;
        if bytes_start_with(component, query, folded, inspections, cancellation)? {
            return Ok(Some(start));
        }
        if component_end == tail.len() {
            return Ok(None);
        }
        start = start
            .checked_add(component_end)
            .and_then(|index| index.checked_add(1))
            .ok_or(FileSuggestionError::Limit)?;
    }
}

fn find_component_end(
    value: &str,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<usize, FileSuggestionError> {
    let mut offset = 0_usize;
    for chunk in value.as_bytes().chunks(MATCH_CANCEL_INTERVAL) {
        inspections.inspect(chunk.len(), cancellation)?;
        if let Some(index) = chunk.iter().position(|byte| *byte == b'/') {
            return offset.checked_add(index).ok_or(FileSuggestionError::Limit);
        }
        offset = offset
            .checked_add(chunk.len())
            .ok_or(FileSuggestionError::Limit)?;
    }
    Ok(value.len())
}

fn bytes_start_with(
    value: &str,
    prefix: &str,
    folded: bool,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<bool, FileSuggestionError> {
    if value.len() < prefix.len() {
        return Ok(false);
    }
    bytes_equal(
        &value[..prefix.len()],
        prefix,
        folded,
        inspections,
        cancellation,
    )
}

fn bytes_equal(
    left: &str,
    right: &str,
    folded: bool,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<bool, FileSuggestionError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .as_bytes()
        .chunks(MATCH_CANCEL_INTERVAL)
        .zip(right.as_bytes().chunks(MATCH_CANCEL_INTERVAL))
    {
        inspections.inspect(left.len(), cancellation)?;
        let equal = left
            .iter()
            .zip(right)
            .all(|(left, right)| fold(*left, folded) == fold(*right, folded));
        if !equal {
            return Ok(false);
        }
    }
    Ok(true)
}

struct KmpPattern {
    bytes: Vec<u8>,
    failure: Vec<usize>,
    fold_ascii: bool,
}

impl KmpPattern {
    fn new(
        value: &str,
        fold_ascii: bool,
        inspections: &mut InspectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, FileSuggestionError> {
        inspect_chunks(value.as_bytes(), inspections, cancellation)?;
        inspections.inspect(value.len(), cancellation)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(value.len())
            .map_err(|_| FileSuggestionError::Capacity)?;
        bytes.extend(value.bytes().map(|byte| fold(byte, fold_ascii)));
        let mut failure = Vec::new();
        failure
            .try_reserve_exact(bytes.len())
            .map_err(|_| FileSuggestionError::Capacity)?;
        failure.resize(bytes.len(), 0);
        let mut matched = 0_usize;
        for index in 1..bytes.len() {
            while matched != 0 && bytes[matched] != bytes[index] {
                matched = failure[matched - 1];
            }
            if bytes.get(matched) == bytes.get(index) {
                matched += 1;
            }
            failure[index] = matched;
        }
        Ok(Self {
            bytes,
            failure,
            fold_ascii,
        })
    }

    fn find(
        &self,
        haystack: &[u8],
        inspections: &mut InspectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<Option<usize>, FileSuggestionError> {
        if self.bytes.is_empty() {
            return Ok(Some(0));
        }
        let mut matched = 0_usize;
        let mut offset = 0_usize;
        for chunk in haystack.chunks(MATCH_CANCEL_INTERVAL) {
            inspections.inspect(chunk.len(), cancellation)?;
            for (index, byte) in chunk.iter().copied().enumerate() {
                let byte = fold(byte, self.fold_ascii);
                while matched != 0 && self.bytes[matched] != byte {
                    matched = self.failure[matched - 1];
                }
                if self.bytes[matched] == byte {
                    matched += 1;
                }
                if matched == self.bytes.len() {
                    return Ok(Some(offset + index + 1 - matched));
                }
            }
            offset = offset
                .checked_add(chunk.len())
                .ok_or(FileSuggestionError::Limit)?;
        }
        Ok(None)
    }
}

const fn fold(byte: u8, enabled: bool) -> u8 {
    if enabled {
        byte.to_ascii_lowercase()
    } else {
        byte
    }
}

fn completion_fits(
    draft_bytes: usize,
    span_bytes: usize,
    path_bytes: usize,
) -> Result<bool, FileSuggestionError> {
    draft_bytes
        .checked_sub(span_bytes)
        .and_then(|bytes| bytes.checked_add(1))
        .and_then(|bytes| bytes.checked_add(path_bytes))
        .and_then(|bytes| bytes.checked_add(1))
        .map(|bytes| bytes <= MAX_PROMPT_BYTES)
        .ok_or(FileSuggestionError::Limit)
}

fn validate_catalogue_path(
    path: &str,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<(), FileSuggestionError> {
    if path.is_empty() || path.len() > MAX_PROMPT_BYTES {
        return Err(FileSuggestionError::Invalid);
    }
    let mut component_start = 0_usize;
    for (index, character) in path.char_indices() {
        inspections.inspect(character.len_utf8(), cancellation)?;
        if character.is_control() {
            return Err(FileSuggestionError::Invalid);
        }
        if character == '/' {
            validate_component(
                path.get(component_start..index)
                    .ok_or(FileSuggestionError::Invalid)?,
            )?;
            component_start = index.checked_add(1).ok_or(FileSuggestionError::Limit)?;
        }
    }
    validate_component(
        path.get(component_start..)
            .ok_or(FileSuggestionError::Invalid)?,
    )?;
    Ok(())
}

fn validate_component(component: &str) -> Result<(), FileSuggestionError> {
    if component.is_empty() || component == "." || component == ".." {
        return Err(FileSuggestionError::Invalid);
    }
    Ok(())
}

#[derive(Default)]
struct InspectionBudget {
    bytes: usize,
    #[cfg(test)]
    cancel_after: Option<usize>,
}

impl InspectionBudget {
    fn inspect(
        &mut self,
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<(), FileSuggestionError> {
        check_cancel(cancellation)?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(FileSuggestionError::Limit)?;
        if self.bytes > MAX_MATCH_INSPECTIONS {
            return Err(FileSuggestionError::Limit);
        }
        #[cfg(test)]
        if self
            .cancel_after
            .is_some_and(|threshold| self.bytes >= threshold)
        {
            cancellation.cancel();
            return Err(FileSuggestionError::Cancelled);
        }
        Ok(())
    }

    #[cfg(test)]
    fn cancelling_after(bytes: usize) -> Self {
        Self {
            bytes: 0,
            cancel_after: Some(bytes),
        }
    }
}

fn inspect_chunks(
    bytes: &[u8],
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<(), FileSuggestionError> {
    for chunk in bytes.chunks(MATCH_CANCEL_INTERVAL) {
        inspections.inspect(chunk.len(), cancellation)?;
    }
    Ok(())
}

fn hash_path(
    path: &str,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<u64, FileSuggestionError> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for chunk in path.as_bytes().chunks(MATCH_CANCEL_INTERVAL) {
        inspections.inspect(chunk.len(), cancellation)?;
        for byte in chunk {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(hash)
}

fn check_cancel(cancellation: &CancellationToken) -> Result<(), FileSuggestionError> {
    if cancellation.is_cancelled() {
        Err(FileSuggestionError::Cancelled)
    } else {
        Ok(())
    }
}

fn try_copy(value: &str) -> Result<String, FileSuggestionError> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|_| FileSuggestionError::Capacity)?;
    output.push_str(value);
    Ok(output)
}

fn try_copy_inspected(
    value: &str,
    inspections: &mut InspectionBudget,
    cancellation: &CancellationToken,
) -> Result<String, FileSuggestionError> {
    let mut output = String::new();
    output
        .try_reserve_exact(value.len())
        .map_err(|_| FileSuggestionError::Capacity)?;
    let mut start = 0_usize;
    while start < value.len() {
        let mut end = start.saturating_add(MATCH_CANCEL_INTERVAL).min(value.len());
        while end > start && !value.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            return Err(FileSuggestionError::Invalid);
        }
        inspections.inspect(end - start, cancellation)?;
        output.push_str(value.get(start..end).ok_or(FileSuggestionError::Invalid)?);
        start = end;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{
        FileSuggestionError, FileTokenHit, InspectionBudget, MAX_FILE_CANDIDATES,
        MAX_MATCH_INSPECTIONS, MatchScore, RankedFileSnapshot, RankedIndex, component_prefix,
        push_best, rank_catalogue, try_copy_inspected,
    };
    use crate::tui::composer::{Composer, MAX_PROMPT_BYTES};
    use tokio_util::sync::CancellationToken;

    fn composer(text: &str) -> Composer {
        let mut composer = Composer::default();
        composer.insert_text(text).unwrap();
        composer
    }

    #[test]
    fn token_detection_is_unicode_safe_bounded_and_scans_past_an_invalid_nearer_at() {
        for (text, query) in [
            ("@src", "src"),
            ("see @src", "src"),
            ("see (@src/lib", "src/lib"),
            ("first\n@src", "src"),
            ("@foo@bar", "foo@bar"),
        ] {
            assert_eq!(
                FileTokenHit::detect(&composer(text))
                    .unwrap()
                    .unwrap()
                    .query(),
                query
            );
        }
        for text in ["", "user@host", "foo_1@bar", "@src done"] {
            assert_eq!(FileTokenHit::detect(&composer(text)).unwrap(), None);
        }
        let exact = format!("@{}", "q".repeat(1_024));
        assert!(FileTokenHit::detect(&composer(&exact)).unwrap().is_some());
        let over = format!("@{}", "q".repeat(1_025));
        assert_eq!(FileTokenHit::detect(&composer(&over)).unwrap(), None);

        let hit = FileTokenHit::detect(&composer("SECRET\u{1b}\u{202e}@src")).unwrap();
        assert!(format!("{hit:?}").contains("query_bytes"));
        assert!(!format!("{hit:?}").contains("SECRET"));
    }

    #[test]
    fn ranking_is_deterministic_preserves_classes_and_redacts_debug() {
        let draft = composer("inspect @src");
        let hit = FileTokenHit::detect(&draft).unwrap().unwrap();
        let catalogue = [
            "other/src/lib.rs",
            "SRC/upper.rs",
            "src",
            "src/main.rs",
            "src/main.rs",
            "nested/mysrc.txt",
            "other/SRC/folded.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let ranked = rank_catalogue(
            &catalogue,
            &hit,
            draft.byte_len(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(
            ranked.candidates(),
            [
                "src",
                "src/main.rs",
                "other/src/lib.rs",
                "nested/mysrc.txt",
                "SRC/upper.rs",
                "other/SRC/folded.rs",
            ]
        );
        let debug = format!("{ranked:?}");
        assert!(debug.contains("candidates: 6"));
        assert!(!debug.contains("main.rs"));
    }

    #[test]
    fn ranking_caps_the_deterministic_prefix_and_keeps_selected_identity() {
        let draft = composer("@");
        let hit = FileTokenHit::detect(&draft).unwrap().unwrap();
        let catalogue = (0..=MAX_FILE_CANDIDATES)
            .map(|index| format!("file-{index:03}.rs"))
            .collect::<Vec<_>>();
        let ranked = rank_catalogue(
            &catalogue,
            &hit,
            draft.byte_len(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(ranked.count(), MAX_FILE_CANDIDATES);
        assert!(ranked.is_capped());
        assert_eq!(ranked.candidate(0), Some("file-000.rs"));
        assert_eq!(ranked.selected_index(Some("file-099.rs")), Some(99));
        assert_eq!(ranked.selected_index(Some("missing")), Some(0));
        let clone = ranked.try_clone_bounded().unwrap();
        assert_eq!(clone, ranked);
    }

    #[test]
    fn ranking_accepts_exact_candidate_bytes_and_bounds_an_eight_mibibyte_scan() {
        let draft = composer("@");
        let hit = FileTokenHit::detect(&draft).unwrap().unwrap();
        let mut exact = (0..4)
            .map(|index| format!("{index}{}", "x".repeat(65_533)))
            .collect::<Vec<_>>();
        exact.push("12345678".to_owned());
        exact.push("z".to_owned());
        let ranked =
            rank_catalogue(&exact, &hit, draft.byte_len(), &CancellationToken::new()).unwrap();
        assert_eq!(
            ranked.candidates().iter().map(String::len).sum::<usize>(),
            256 * 1_024
        );
        assert!(ranked.is_capped());

        let query_draft = composer(&format!("@{}", "q".repeat(1_024)));
        let query_hit = FileTokenHit::detect(&query_draft).unwrap().unwrap();
        let catalogue = (0..128)
            .map(|index| {
                format!(
                    "{}x{:05}{}",
                    "q".repeat(1_023),
                    index,
                    "y".repeat(65_534 - 1_023 - 1 - 5)
                )
            })
            .collect::<Vec<_>>();
        assert!(catalogue.iter().map(String::len).sum::<usize>() <= 8 * 1_024 * 1_024);
        let ranked = rank_catalogue(
            &catalogue,
            &query_hit,
            query_draft.byte_len(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(ranked.count(), 0);
    }

    #[test]
    fn cancellation_invalid_catalogue_and_whole_draft_capacity_fail_closed() {
        let draft = composer("@");
        let hit = FileTokenHit::detect(&draft).unwrap().unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(rank_catalogue(&[], &hit, 1, &cancellation).is_err());
        assert!(
            rank_catalogue(
                &["../escape".to_owned()],
                &hit,
                1,
                &CancellationToken::new()
            )
            .is_err()
        );

        let prefix = "x".repeat(MAX_PROMPT_BYTES - 4);
        let full = composer(&format!("{prefix} @"));
        let full_hit = FileTokenHit::detect(&full).unwrap().unwrap();
        let ranked = rank_catalogue(
            &["a".to_owned(), "ab".to_owned()],
            &full_hit,
            full.byte_len(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(ranked.candidates(), ["a"]);
    }

    #[test]
    fn empty_snapshot_has_no_selection() {
        let snapshot = RankedFileSnapshot {
            candidates: Vec::new(),
            total_bytes: 0,
            capped: false,
        };
        assert_eq!(snapshot.selected_index(Some("anything")), None);
    }

    #[test]
    fn heap_matches_a_full_sort_and_never_exceeds_seventeen_comparisons() {
        let mut all = (0..700_usize)
            .map(|ordinal| RankedIndex {
                score: MatchScore {
                    class: ((ordinal * 37) % 7) as u8,
                    index: (ordinal * 97) % 211,
                    path_bytes: (ordinal * 53) % 1_009,
                    ordinal,
                },
                ordinal,
            })
            .collect::<Vec<_>>();
        let mut heap = Vec::with_capacity(MAX_FILE_CANDIDATES);
        let mut maximum = 0_usize;
        let mut full_replacements = 0_usize;
        for candidate in all.iter().copied().rev() {
            let old_root = heap.first().copied();
            let comparisons = push_best(&mut heap, candidate);
            maximum = maximum.max(comparisons);
            if old_root.is_some_and(|root| candidate < root) && heap.len() == MAX_FILE_CANDIDATES {
                full_replacements += 1;
            }
        }
        heap.sort_unstable();
        all.sort_unstable();
        all.truncate(MAX_FILE_CANDIDATES);

        assert_eq!(heap, all);
        assert!(full_replacements > 0);
        assert!(maximum <= 17, "observed {maximum} comparisons");
    }

    #[test]
    fn inspection_budget_is_exact_and_cancellation_reaches_inner_loops() {
        let cancellation = CancellationToken::new();
        let mut exact = InspectionBudget::default();
        exact.inspect(MAX_MATCH_INSPECTIONS, &cancellation).unwrap();
        assert_eq!(
            exact.inspect(1, &cancellation),
            Err(FileSuggestionError::Limit)
        );

        let path = format!("{} /needle", "x".repeat(8 * 1_024));
        let component_cancellation = CancellationToken::new();
        let mut component_budget = InspectionBudget::cancelling_after(4 * 1_024);
        assert_eq!(
            component_prefix(
                &path,
                "needle",
                false,
                &mut component_budget,
                &component_cancellation,
            ),
            Err(FileSuggestionError::Cancelled)
        );

        let copy_cancellation = CancellationToken::new();
        let mut copy_budget = InspectionBudget::cancelling_after(4 * 1_024);
        assert_eq!(
            try_copy_inspected(&path, &mut copy_budget, &copy_cancellation),
            Err(FileSuggestionError::Cancelled)
        );
    }
}
