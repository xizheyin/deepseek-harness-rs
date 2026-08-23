use std::{fmt, sync::Arc};

use super::presentation::{PresentationError, PresentedChunkBuilder, TextStyle};

const MAX_LINE_PREFIX_BYTES: usize = 64;
const MAX_INLINE_CODE_BYTES: usize = 4 * 1024;
const MAX_FENCE_BYTES: usize = 64 * 1024;
const FENCE_BLOCK_BYTES: usize = 1024;
const MAX_TABLE_COLUMNS: usize = 8;
const MAX_TABLE_BODY_ROWS: usize = 64;
const MAX_TABLE_ROW_BYTES: usize = 16 * 1024;
const MAX_TABLE_BYTES: usize = 64 * 1024;
const MAX_STYLE_RUNS: usize = 4 * 1024;
const MAX_MARKUP_FRAME_ITEMS: usize = 96 * 1024;
pub(crate) const MAX_MARKUP_FRAME_TEXT_BYTES: usize = 768 * 1024;
const MARKUP_ITEM_HEADROOM: usize = MAX_STYLE_RUNS * 2 + 16;
const DISPLAY_OMITTED: &str = "[assistant display omitted: presentation limit exceeded]";

#[derive(Clone)]
pub(crate) struct MarkupState {
    block: BlockState,
    line: LineState,
    inline: InlineState,
    inline_disabled: bool,
    style_runs: usize,
    last_style: Option<TextStyle>,
    degraded: bool,
    output_omitted: bool,
}

impl Default for MarkupState {
    fn default() -> Self {
        Self {
            block: BlockState::Markdown,
            line: LineState::prefix(),
            inline: InlineState::Text,
            inline_disabled: false,
            style_runs: 0,
            last_style: None,
            degraded: false,
            output_omitted: false,
        }
    }
}

impl fmt::Debug for MarkupState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MarkupState")
            .field("block", &self.block.label())
            .field("block_bytes", &self.block.bytes())
            .field("line_bytes", &self.line.bytes())
            .field("inline_bytes", &self.inline.bytes())
            .field("style_runs", &self.style_runs)
            .field("degraded", &self.degraded)
            .field("output_omitted", &self.output_omitted)
            .finish()
    }
}

#[derive(Clone)]
enum BlockState {
    Markdown,
    FenceHeld(FenceHeld),
    FencePlain(FencePlain),
    TableCandidate(TableCandidate),
    TableOpen(TableOpen),
    TablePlain,
}

impl BlockState {
    fn label(&self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::FenceHeld(_) => "FenceHeld",
            Self::FencePlain(_) => "FencePlain",
            Self::TableCandidate(_) => "TableCandidate",
            Self::TableOpen(_) => "TableOpen",
            Self::TablePlain => "TablePlain",
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Markdown | Self::FencePlain(_) | Self::TablePlain => 0,
            Self::FenceHeld(fence) => fence.buffer.len(),
            Self::TableCandidate(table) => table.bytes(),
            Self::TableOpen(table) => table.pending.len(),
        }
    }

    fn text_bytes(&self) -> usize {
        match self {
            Self::Markdown | Self::FencePlain(_) | Self::TablePlain => 0,
            Self::FenceHeld(fence) => fence.buffer.text_bytes(),
            Self::TableCandidate(table) => table.text_bytes(),
            Self::TableOpen(table) => estimated_literal_text_bytes(&table.pending),
        }
    }

    fn literal_item_state(&self) -> (usize, bool) {
        match self {
            Self::Markdown | Self::FencePlain(_) | Self::TablePlain => (0, false),
            Self::FenceHeld(fence) => fence.buffer.literal_item_state(),
            Self::TableCandidate(table) => table.literal_item_state(),
            Self::TableOpen(table) => literal_item_state(&table.pending),
        }
    }
}

#[derive(Clone)]
enum TableCandidate {
    Header(String),
    Delimiter {
        header: String,
        columns: usize,
        pending: String,
    },
}

impl TableCandidate {
    fn bytes(&self) -> usize {
        match self {
            Self::Header(header) => header.len(),
            Self::Delimiter {
                header, pending, ..
            } => header.len().saturating_add(pending.len()),
        }
    }

    fn text_bytes(&self) -> usize {
        match self {
            Self::Header(header) => estimated_literal_text_bytes(header),
            Self::Delimiter {
                header, pending, ..
            } => estimated_literal_text_bytes(header)
                .saturating_add(estimated_literal_text_bytes(pending)),
        }
    }

    fn literal_item_state(&self) -> (usize, bool) {
        match self {
            Self::Header(header) => literal_item_state(header),
            Self::Delimiter {
                header, pending, ..
            } => append_literal_items_from(literal_item_state(header), pending)
                .unwrap_or((usize::MAX, true)),
        }
    }
}

#[derive(Clone)]
struct TableOpen {
    columns: usize,
    body_rows: usize,
    source_bytes: usize,
    pending: String,
}

#[derive(Clone, Copy)]
enum TableRowKind {
    Header,
    Delimiter,
    Body,
}

#[derive(Clone)]
struct FenceHeld {
    kind: FenceKind,
    buffer: ChunkBuffer,
    closer: CloserTracker,
}

#[derive(Clone)]
struct FencePlain {
    closer: CloserTracker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FenceKind {
    Code,
    Diff,
}

#[derive(Clone)]
enum LineState {
    Prefix(String),
    Body(LineFormat),
}

impl LineState {
    fn prefix() -> Self {
        Self::Prefix(String::new())
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Prefix(prefix) => prefix.len(),
            Self::Body(_) => 0,
        }
    }

    fn append_literal_items(&self, state: (usize, bool)) -> Option<(usize, bool)> {
        match self {
            Self::Prefix(prefix) => append_literal_items_from(state, prefix),
            Self::Body(_) => Some(state),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineFormat {
    Paragraph,
    Heading,
    List,
    Quote,
}

impl LineFormat {
    fn body_style(self) -> TextStyle {
        match self {
            Self::Paragraph | Self::List => TextStyle::Assistant,
            Self::Heading => TextStyle::Heading,
            Self::Quote => TextStyle::Quote,
        }
    }

    fn marker_style(self) -> TextStyle {
        match self {
            Self::Paragraph => TextStyle::Assistant,
            Self::Heading => TextStyle::Heading,
            Self::List => TextStyle::Accent,
            Self::Quote => TextStyle::Quote,
        }
    }
}

#[derive(Clone)]
enum InlineState {
    Text,
    Pending(String),
}

impl InlineState {
    fn bytes(&self) -> usize {
        match self {
            Self::Text => 0,
            Self::Pending(pending) => pending.len(),
        }
    }

    fn append_literal_items(&self, state: (usize, bool)) -> Option<(usize, bool)> {
        match self {
            Self::Text => Some(state),
            Self::Pending(pending) => append_literal_items_from(state, pending),
        }
    }
}

enum PrefixDecision {
    NeedMore,
    Literal,
    Body {
        format: LineFormat,
        marker_bytes: usize,
    },
    FenceOpen(FenceKind),
}

#[derive(Clone, Default)]
struct CloserTracker {
    candidate: String,
    viable: bool,
}

impl CloserTracker {
    fn new() -> Self {
        Self {
            candidate: String::new(),
            viable: true,
        }
    }

    fn observe(&mut self, text: &str) -> Result<(), PresentationError> {
        if !self.viable || text.is_empty() {
            return Ok(());
        }
        let next = self
            .candidate
            .len()
            .checked_add(text.len())
            .ok_or(PresentationError::Limit)?;
        if next > MAX_LINE_PREFIX_BYTES {
            self.candidate.clear();
            self.viable = false;
            return Ok(());
        }
        self.candidate
            .try_reserve(text.len())
            .map_err(|_| PresentationError::Capacity)?;
        self.candidate.push_str(text);
        let bytes = self.candidate.as_bytes();
        self.viable = if bytes.len() <= 3 {
            bytes.iter().all(|byte| *byte == b'`')
        } else {
            bytes[..3] == *b"```" && bytes[3..].iter().all(|byte| *byte == b' ')
        };
        if !self.viable {
            self.candidate.clear();
        }
        Ok(())
    }

    fn finish_line(&mut self) -> bool {
        let closing = self.is_closing();
        self.candidate.clear();
        self.viable = true;
        closing
    }

    fn is_closing(&self) -> bool {
        self.viable
            && self.candidate.len() >= 3
            && self.candidate.as_bytes()[..3] == *b"```"
            && self.candidate.as_bytes()[3..]
                .iter()
                .all(|byte| *byte == b' ')
    }
}

#[derive(Clone, Default)]
struct ChunkBuffer {
    blocks: Arc<Vec<Arc<str>>>,
    tail: String,
    bytes: usize,
    text_bytes: usize,
    literal_items: usize,
    line_has_text: bool,
}

enum BufferError {
    Capacity,
    Limit,
}

impl ChunkBuffer {
    fn len(&self) -> usize {
        self.bytes
    }

    fn text_bytes(&self) -> usize {
        self.text_bytes
    }

    fn literal_item_state(&self) -> (usize, bool) {
        (self.literal_items, self.line_has_text)
    }

    fn append(&mut self, text: &str) -> Result<(), BufferError> {
        if text.is_empty() {
            return Ok(());
        }
        let next_bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or(BufferError::Limit)?;
        let next_text_bytes = self
            .text_bytes
            .checked_add(estimated_literal_text_bytes(text))
            .ok_or(BufferError::Limit)?;
        let (next_literal_items, next_line_has_text) =
            append_literal_items(self.literal_items, self.line_has_text, text)
                .ok_or(BufferError::Limit)?;
        if next_bytes > MAX_FENCE_BYTES {
            return Err(BufferError::Limit);
        }

        let mut remaining = text;
        while !remaining.is_empty() {
            let available = FENCE_BLOCK_BYTES.saturating_sub(self.tail.len());
            let take = boundary_at_or_before(remaining, available);
            if take != 0 {
                self.tail
                    .try_reserve(take)
                    .map_err(|_| BufferError::Capacity)?;
                self.tail.push_str(&remaining[..take]);
                remaining = &remaining[take..];
            }
            if !remaining.is_empty() || self.tail.len() == FENCE_BLOCK_BYTES {
                self.seal_tail()?;
            }
        }
        self.bytes = next_bytes;
        self.text_bytes = next_text_bytes;
        self.literal_items = next_literal_items;
        self.line_has_text = next_line_has_text;
        Ok(())
    }

    fn seal_tail(&mut self) -> Result<(), BufferError> {
        if self.tail.is_empty() {
            return Ok(());
        }
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(self.blocks.len().saturating_add(1))
            .map_err(|_| BufferError::Capacity)?;
        blocks.extend(self.blocks.iter().cloned());
        blocks.push(Arc::from(std::mem::take(&mut self.tail).into_boxed_str()));
        self.blocks = Arc::new(blocks);
        Ok(())
    }

    fn collect(&self) -> Result<String, PresentationError> {
        let mut text = String::new();
        text.try_reserve_exact(self.bytes)
            .map_err(|_| PresentationError::Capacity)?;
        for block in self.blocks.iter() {
            text.push_str(block);
        }
        text.push_str(&self.tail);
        Ok(text)
    }
}

fn boundary_at_or_before(text: &str, limit: usize) -> usize {
    let mut end = text.len().min(limit);
    while end != 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

impl MarkupState {
    pub(crate) fn has_pending_source(&self) -> bool {
        self.block.bytes() != 0 || self.line.bytes() != 0 || self.inline.bytes() != 0
    }

    pub(crate) fn push(
        &mut self,
        text: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if self.output_omitted {
            return Ok(());
        }
        let builder_has_current_line_text = builder.item_count() != 0 && !*at_line_start;
        let source_items = self
            .retained_literal_item_state(builder_has_current_line_text)
            .and_then(|state| append_literal_items_from(state, text))
            .map(|(items, _)| items);
        let source_text_bytes = self
            .retained_text_bytes()
            .checked_add(estimated_literal_text_bytes(text));
        if source_items
            .zip(source_text_bytes)
            .is_none_or(|(items, bytes)| !self.has_output_budget(items, bytes, builder))
        {
            self.omit_output(builder, at_line_start)?;
            return Ok(());
        }
        for segment in text.split_inclusive('\n') {
            let (content, line_feed) = segment
                .strip_suffix('\n')
                .map_or((segment, false), |content| (content, true));
            if self.degraded {
                self.push_literal_segment(content, line_feed, builder, at_line_start)?;
                continue;
            }
            let block = std::mem::replace(&mut self.block, BlockState::Markdown);
            self.block = match block {
                BlockState::Markdown
                    if self.markdown_line_is_fresh() && content.starts_with('|') =>
                {
                    self.push_table_candidate_segment(
                        TableCandidate::Header(String::new()),
                        segment,
                        content,
                        line_feed,
                        builder,
                        at_line_start,
                    )?
                }
                BlockState::Markdown => {
                    self.push_markdown_segment(content, line_feed, builder, at_line_start)?
                }
                BlockState::FenceHeld(fence) => self.push_held_fence_segment(
                    fence,
                    segment,
                    content,
                    line_feed,
                    builder,
                    at_line_start,
                )?,
                BlockState::FencePlain(fence) => self.push_plain_fence_segment(
                    fence,
                    content,
                    line_feed,
                    builder,
                    at_line_start,
                )?,
                BlockState::TableCandidate(table) => self.push_table_candidate_segment(
                    table,
                    segment,
                    content,
                    line_feed,
                    builder,
                    at_line_start,
                )?,
                BlockState::TableOpen(table) => self.push_open_table_segment(
                    table,
                    segment,
                    content,
                    line_feed,
                    builder,
                    at_line_start,
                )?,
                BlockState::TablePlain => {
                    self.push_literal_segment(content, line_feed, builder, at_line_start)?;
                    if line_feed {
                        BlockState::Markdown
                    } else {
                        BlockState::TablePlain
                    }
                }
            };
            if self.output_omitted {
                break;
            }
        }
        Ok(())
    }

    fn markdown_line_is_fresh(&self) -> bool {
        matches!(&self.line, LineState::Prefix(prefix) if prefix.is_empty())
            && matches!(self.inline, InlineState::Text)
    }

    fn retained_text_bytes(&self) -> usize {
        self.block
            .text_bytes()
            .saturating_add(self.line.bytes())
            .saturating_add(self.inline.bytes())
    }

    fn retained_literal_item_state(
        &self,
        builder_has_current_line_text: bool,
    ) -> Option<(usize, bool)> {
        let state = if self.block.bytes() == 0 {
            (0, builder_has_current_line_text)
        } else {
            self.block.literal_item_state()
        };
        let state = self.line.append_literal_items(state)?;
        self.inline.append_literal_items(state)
    }

    fn push_markdown_segment(
        &mut self,
        content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<BlockState, PresentationError> {
        self.push_markdown_content(content, builder, at_line_start)?;
        if line_feed {
            self.end_markdown_line(builder, at_line_start)?;
        }
        Ok(std::mem::replace(&mut self.block, BlockState::Markdown))
    }

    pub(crate) fn omit_remaining_display(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if self.output_omitted {
            return Ok(());
        }
        self.omit_output(builder, at_line_start)
    }

    pub(crate) fn finish_authoritative(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        self.finish_inner(true, builder, at_line_start)
    }

    pub(crate) fn abort_plain(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        self.finish_inner(false, builder, at_line_start)
    }

    fn finish_inner(
        &mut self,
        authoritative: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if self.output_omitted {
            *self = Self::default();
            return Ok(());
        }
        let block = std::mem::replace(&mut self.block, BlockState::Markdown);
        match block {
            BlockState::Markdown => self.finish_markdown_line(builder, at_line_start)?,
            BlockState::FenceHeld(fence) => {
                let text = fence.buffer.collect()?;
                if authoritative && fence.closer.is_closing() {
                    self.render_closed_fence(fence.kind, &text, builder, at_line_start)?;
                } else {
                    self.push_literal_text(&text, builder, at_line_start)?;
                }
            }
            BlockState::FencePlain(_) => {}
            BlockState::TableCandidate(table) => {
                self.flush_table_candidate_plain(table, builder, at_line_start)?;
            }
            BlockState::TableOpen(table) => {
                if !table.pending.is_empty() {
                    if !self.ensure_held_output_budget(&[&table.pending], builder, at_line_start)? {
                        *self = Self::default();
                        return Ok(());
                    }
                    let valid = authoritative
                        && table.body_rows < MAX_TABLE_BODY_ROWS
                        && table
                            .source_bytes
                            .checked_add(table.pending.len())
                            .is_some_and(|bytes| bytes <= MAX_TABLE_BYTES)
                        && table_row_columns(&table.pending) == Some(table.columns);
                    if valid {
                        self.render_table_row(
                            &table.pending,
                            false,
                            TableRowKind::Body,
                            builder,
                            at_line_start,
                        )?;
                    } else {
                        self.push_literal_text(&table.pending, builder, at_line_start)?;
                    }
                }
            }
            BlockState::TablePlain => {}
        }
        *self = Self::default();
        Ok(())
    }

    fn push_markdown_content(
        &mut self,
        content: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        let mut offset = 0;
        while offset < content.len() {
            if self.degraded {
                self.emit(
                    TextStyle::Assistant,
                    &content[offset..],
                    builder,
                    at_line_start,
                )?;
                return Ok(());
            }
            if matches!(self.line, LineState::Body(_)) {
                let format = match self.line {
                    LineState::Body(format) => format,
                    LineState::Prefix(_) => unreachable!(),
                };
                self.push_inline(
                    format.body_style(),
                    &content[offset..],
                    builder,
                    at_line_start,
                )?;
                return Ok(());
            }

            let character = content[offset..]
                .chars()
                .next()
                .ok_or(PresentationError::InvalidText)?;
            let character_bytes = character.len_utf8();
            let prefix_len = match &self.line {
                LineState::Prefix(prefix) => prefix.len(),
                LineState::Body(_) => 0,
            };
            if prefix_len
                .checked_add(character_bytes)
                .is_none_or(|next| next > MAX_LINE_PREFIX_BYTES)
            {
                let literal = matches!(
                    &self.line,
                    LineState::Prefix(prefix) if prefix.starts_with("```")
                );
                if literal {
                    self.resolve_literal_prefix(builder, at_line_start)?;
                } else {
                    self.resolve_prefix(
                        PrefixDecision::Body {
                            format: LineFormat::Paragraph,
                            marker_bytes: 0,
                        },
                        builder,
                        at_line_start,
                    )?;
                }
                continue;
            }
            let prefix = match &mut self.line {
                LineState::Prefix(prefix) => prefix,
                LineState::Body(_) => unreachable!(),
            };
            prefix
                .try_reserve(character_bytes)
                .map_err(|_| PresentationError::Capacity)?;
            prefix.push(character);
            offset += character_bytes;

            let decision = match &self.line {
                LineState::Prefix(prefix) => classify_prefix(prefix, false),
                LineState::Body(_) => unreachable!(),
            };
            if !matches!(decision, PrefixDecision::NeedMore) {
                if matches!(decision, PrefixDecision::Literal) {
                    self.resolve_literal_prefix(builder, at_line_start)?;
                } else {
                    self.resolve_prefix(decision, builder, at_line_start)?;
                }
            }
        }
        Ok(())
    }

    fn resolve_prefix(
        &mut self,
        decision: PrefixDecision,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        let PrefixDecision::Body {
            format,
            marker_bytes,
        } = decision
        else {
            return Err(PresentationError::InvalidText);
        };
        let prefix = match std::mem::replace(&mut self.line, LineState::Body(format)) {
            LineState::Prefix(prefix) => prefix,
            LineState::Body(_) => return Err(PresentationError::InvalidText),
        };
        if marker_bytes > prefix.len() || !prefix.is_char_boundary(marker_bytes) {
            return Err(PresentationError::InvalidText);
        }
        self.emit(
            format.marker_style(),
            &prefix[..marker_bytes],
            builder,
            at_line_start,
        )?;
        self.push_inline(
            format.body_style(),
            &prefix[marker_bytes..],
            builder,
            at_line_start,
        )
    }

    fn resolve_literal_prefix(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        let prefix = match std::mem::replace(&mut self.line, LineState::Body(LineFormat::Paragraph))
        {
            LineState::Prefix(prefix) => prefix,
            LineState::Body(_) => return Err(PresentationError::InvalidText),
        };
        self.inline_disabled = true;
        self.emit(TextStyle::Assistant, &prefix, builder, at_line_start)
    }

    fn end_markdown_line(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if let LineState::Prefix(prefix) = &self.line {
            let decision = classify_prefix(prefix, true);
            if let PrefixDecision::FenceOpen(kind) = decision {
                let prefix = match std::mem::replace(&mut self.line, LineState::prefix()) {
                    LineState::Prefix(prefix) => prefix,
                    LineState::Body(_) => return Err(PresentationError::InvalidText),
                };
                let mut opener = prefix;
                opener
                    .try_reserve(1)
                    .map_err(|_| PresentationError::Capacity)?;
                opener.push('\n');
                let mut buffer = ChunkBuffer::default();
                buffer.append(&opener).map_err(map_buffer_error)?;
                self.inline = InlineState::Text;
                self.inline_disabled = false;
                self.block = BlockState::FenceHeld(FenceHeld {
                    kind,
                    buffer,
                    closer: CloserTracker::new(),
                });
                return Ok(());
            }
            if matches!(decision, PrefixDecision::Literal) {
                self.resolve_literal_prefix(builder, at_line_start)?;
            } else {
                self.resolve_prefix(decision, builder, at_line_start)?;
            }
        }
        let format = match self.line {
            LineState::Body(format) => format,
            LineState::Prefix(_) => return Err(PresentationError::InvalidText),
        };
        self.flush_inline(format.body_style(), builder, at_line_start)?;
        self.emit_line_feed(builder, at_line_start)?;
        self.line = LineState::prefix();
        self.inline_disabled = false;
        Ok(())
    }

    fn finish_markdown_line(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if let LineState::Prefix(prefix) = &self.line {
            let decision = classify_prefix(prefix, true);
            if matches!(
                decision,
                PrefixDecision::FenceOpen(_) | PrefixDecision::Literal
            ) {
                self.resolve_literal_prefix(builder, at_line_start)?;
            } else {
                self.resolve_prefix(decision, builder, at_line_start)?;
            }
        }
        let format = match self.line {
            LineState::Body(format) => format,
            LineState::Prefix(_) => return Err(PresentationError::InvalidText),
        };
        self.flush_inline(format.body_style(), builder, at_line_start)
    }

    fn push_inline(
        &mut self,
        base_style: TextStyle,
        text: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if text.is_empty() {
            return Ok(());
        }
        if self.inline_disabled || self.degraded {
            return self.emit(base_style, text, builder, at_line_start);
        }
        let mut remaining = text;
        while !remaining.is_empty() {
            let state = std::mem::replace(&mut self.inline, InlineState::Text);
            match state {
                InlineState::Text => {
                    if let Some(index) = remaining.find('`') {
                        self.emit(base_style, &remaining[..index], builder, at_line_start)?;
                        let mut pending = String::new();
                        pending
                            .try_reserve_exact(1)
                            .map_err(|_| PresentationError::Capacity)?;
                        pending.push('`');
                        self.inline = InlineState::Pending(pending);
                        remaining = &remaining[index + 1..];
                    } else {
                        self.emit(base_style, remaining, builder, at_line_start)?;
                        return Ok(());
                    }
                }
                InlineState::Pending(mut pending) => {
                    if let Some(index) = remaining.find('`') {
                        let needed = index.saturating_add(1);
                        if pending
                            .len()
                            .checked_add(needed)
                            .is_some_and(|next| next <= MAX_INLINE_CODE_BYTES)
                        {
                            pending
                                .try_reserve(needed)
                                .map_err(|_| PresentationError::Capacity)?;
                            pending.push_str(&remaining[..=index]);
                            self.emit(TextStyle::Code, &pending, builder, at_line_start)?;
                            remaining = &remaining[index + 1..];
                        } else {
                            self.emit(base_style, &pending, builder, at_line_start)?;
                            self.emit(base_style, remaining, builder, at_line_start)?;
                            self.inline_disabled = true;
                            return Ok(());
                        }
                    } else if pending
                        .len()
                        .checked_add(remaining.len())
                        .is_some_and(|next| next <= MAX_INLINE_CODE_BYTES)
                    {
                        pending
                            .try_reserve(remaining.len())
                            .map_err(|_| PresentationError::Capacity)?;
                        pending.push_str(remaining);
                        self.inline = InlineState::Pending(pending);
                        return Ok(());
                    } else {
                        self.emit(base_style, &pending, builder, at_line_start)?;
                        self.emit(base_style, remaining, builder, at_line_start)?;
                        self.inline_disabled = true;
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn flush_inline(
        &mut self,
        base_style: TextStyle,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if let InlineState::Pending(pending) =
            std::mem::replace(&mut self.inline, InlineState::Text)
        {
            self.emit(base_style, &pending, builder, at_line_start)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_table_candidate_segment(
        &mut self,
        candidate: TableCandidate,
        segment: &str,
        _content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<BlockState, PresentationError> {
        match candidate {
            TableCandidate::Header(mut header) => {
                if append_table_source(&mut header, segment).is_err() {
                    self.push_table_literal_parts(&[&header, segment], builder, at_line_start)?;
                    return Ok(table_plain_after(line_feed));
                }
                if !line_feed {
                    return Ok(BlockState::TableCandidate(TableCandidate::Header(header)));
                }
                let header_content = header
                    .strip_suffix('\n')
                    .ok_or(PresentationError::InvalidText)?;
                let Some(columns) = table_header_columns(header_content) else {
                    self.push_table_literal_parts(&[&header], builder, at_line_start)?;
                    return Ok(BlockState::Markdown);
                };
                Ok(BlockState::TableCandidate(TableCandidate::Delimiter {
                    header,
                    columns,
                    pending: String::new(),
                }))
            }
            TableCandidate::Delimiter {
                header,
                columns,
                mut pending,
            } => {
                if append_table_source(&mut pending, segment).is_err() {
                    self.push_table_literal_parts(
                        &[&header, &pending, segment],
                        builder,
                        at_line_start,
                    )?;
                    return Ok(table_plain_after(line_feed));
                }
                if header
                    .len()
                    .checked_add(pending.len())
                    .is_none_or(|bytes| bytes > MAX_TABLE_BYTES)
                {
                    self.push_table_literal_parts(&[&header, &pending], builder, at_line_start)?;
                    return Ok(table_plain_after(line_feed));
                }
                if !line_feed {
                    return Ok(BlockState::TableCandidate(TableCandidate::Delimiter {
                        header,
                        columns,
                        pending,
                    }));
                }
                let delimiter = pending
                    .strip_suffix('\n')
                    .ok_or(PresentationError::InvalidText)?;
                if !is_table_delimiter(delimiter, columns) {
                    self.push_table_literal_parts(&[&header, &pending], builder, at_line_start)?;
                    return Ok(BlockState::Markdown);
                }
                if !self.ensure_held_output_budget(&[&header, &pending], builder, at_line_start)? {
                    return Ok(BlockState::Markdown);
                }
                let header_content = header
                    .strip_suffix('\n')
                    .ok_or(PresentationError::InvalidText)?;
                self.render_table_row(
                    header_content,
                    true,
                    TableRowKind::Header,
                    builder,
                    at_line_start,
                )?;
                self.render_table_row(
                    delimiter,
                    true,
                    TableRowKind::Delimiter,
                    builder,
                    at_line_start,
                )?;
                Ok(BlockState::TableOpen(TableOpen {
                    columns,
                    body_rows: 0,
                    source_bytes: header.len() + pending.len(),
                    pending: String::new(),
                }))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_open_table_segment(
        &mut self,
        mut table: TableOpen,
        segment: &str,
        content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<BlockState, PresentationError> {
        if table.pending.is_empty() && !content.starts_with('|') {
            return self.push_markdown_segment(content, line_feed, builder, at_line_start);
        }
        let next_source = table
            .source_bytes
            .checked_add(table.pending.len())
            .and_then(|bytes| bytes.checked_add(segment.len()));
        if next_source.is_none_or(|bytes| bytes > MAX_TABLE_BYTES) {
            self.push_table_literal_parts(&[&table.pending, segment], builder, at_line_start)?;
            return Ok(table_plain_after(line_feed));
        }
        if append_table_source(&mut table.pending, segment).is_err() {
            self.push_table_literal_parts(&[&table.pending, segment], builder, at_line_start)?;
            return Ok(table_plain_after(line_feed));
        }
        if !line_feed {
            return Ok(BlockState::TableOpen(table));
        }
        let row = table
            .pending
            .strip_suffix('\n')
            .ok_or(PresentationError::InvalidText)?;
        if table.body_rows == MAX_TABLE_BODY_ROWS || table_row_columns(row) != Some(table.columns) {
            self.push_table_literal_parts(&[&table.pending], builder, at_line_start)?;
            return Ok(BlockState::Markdown);
        }
        if !self.ensure_held_output_budget(&[&table.pending], builder, at_line_start)? {
            return Ok(BlockState::Markdown);
        }
        self.render_table_row(row, true, TableRowKind::Body, builder, at_line_start)?;
        table.body_rows += 1;
        table.source_bytes = next_source.ok_or(PresentationError::Limit)?;
        table.pending.clear();
        Ok(BlockState::TableOpen(table))
    }

    fn flush_table_candidate_plain(
        &mut self,
        candidate: TableCandidate,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        match candidate {
            TableCandidate::Header(header) => self
                .push_table_literal_parts(&[&header], builder, at_line_start)
                .map(|_| ()),
            TableCandidate::Delimiter {
                header, pending, ..
            } => self
                .push_table_literal_parts(&[&header, &pending], builder, at_line_start)
                .map(|_| ()),
        }
    }

    fn push_table_literal_parts(
        &mut self,
        parts: &[&str],
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<bool, PresentationError> {
        if !self.ensure_held_output_budget(parts, builder, at_line_start)? {
            return Ok(false);
        }
        for part in parts {
            self.push_literal_text(part, builder, at_line_start)?;
        }
        Ok(true)
    }

    fn ensure_held_output_budget(
        &mut self,
        parts: &[&str],
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<bool, PresentationError> {
        let mut bytes = 0_usize;
        let mut item_state = (0_usize, false);
        for part in parts {
            let Some(next_bytes) = bytes.checked_add(estimated_literal_text_bytes(part)) else {
                self.omit_output(builder, at_line_start)?;
                return Ok(false);
            };
            let Some(next_item_state) = append_literal_items_from(item_state, part) else {
                self.omit_output(builder, at_line_start)?;
                return Ok(false);
            };
            bytes = next_bytes;
            item_state = next_item_state;
        }
        if !self.has_output_budget(item_state.0, bytes, builder) {
            self.omit_output(builder, at_line_start)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn render_table_row(
        &mut self,
        row: &str,
        line_feed: bool,
        kind: TableRowKind,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        let cell_style = match kind {
            TableRowKind::Header => TextStyle::Heading,
            TableRowKind::Delimiter => TextStyle::Border,
            TableRowKind::Body => TextStyle::Assistant,
        };
        let mut start = 0_usize;
        for (offset, character) in row.char_indices() {
            if character != '|' {
                continue;
            }
            self.emit(cell_style, &row[start..offset], builder, at_line_start)?;
            self.emit(TextStyle::Border, "|", builder, at_line_start)?;
            start = offset + 1;
        }
        self.emit(cell_style, &row[start..], builder, at_line_start)?;
        if line_feed {
            self.emit_line_feed(builder, at_line_start)?;
        }
        Ok(())
    }

    fn push_held_fence_segment(
        &mut self,
        mut fence: FenceHeld,
        segment: &str,
        content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<BlockState, PresentationError> {
        match fence.buffer.append(segment) {
            Ok(()) => {
                fence.closer.observe(content)?;
                if line_feed && fence.closer.finish_line() {
                    let text = fence.buffer.collect()?;
                    self.render_closed_fence(fence.kind, &text, builder, at_line_start)?;
                    Ok(BlockState::Markdown)
                } else {
                    Ok(BlockState::FenceHeld(fence))
                }
            }
            Err(BufferError::Capacity) => Err(PresentationError::Capacity),
            Err(BufferError::Limit) => {
                let buffered = fence.buffer.collect()?;
                self.push_literal_text(&buffered, builder, at_line_start)?;
                self.push_literal_segment(content, line_feed, builder, at_line_start)?;
                fence.closer.observe(content)?;
                if line_feed && fence.closer.finish_line() {
                    Ok(BlockState::Markdown)
                } else {
                    Ok(BlockState::FencePlain(FencePlain {
                        closer: fence.closer,
                    }))
                }
            }
        }
    }

    fn push_plain_fence_segment(
        &mut self,
        mut fence: FencePlain,
        content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<BlockState, PresentationError> {
        self.push_literal_segment(content, line_feed, builder, at_line_start)?;
        fence.closer.observe(content)?;
        if line_feed && fence.closer.finish_line() {
            Ok(BlockState::Markdown)
        } else {
            Ok(BlockState::FencePlain(fence))
        }
    }

    fn render_closed_fence(
        &mut self,
        kind: FenceKind,
        text: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if !self.has_output_budget(
            estimated_literal_items(text),
            estimated_literal_text_bytes(text),
            builder,
        ) {
            self.omit_output(builder, at_line_start)?;
            return Ok(());
        }
        let mut lines = text.split_inclusive('\n').peekable();
        let mut first = true;
        while let Some(segment) = lines.next() {
            let (content, line_feed) = segment
                .strip_suffix('\n')
                .map_or((segment, false), |content| (content, true));
            let style = if first || lines.peek().is_none() {
                TextStyle::Muted
            } else {
                match kind {
                    FenceKind::Code => TextStyle::Code,
                    FenceKind::Diff => diff_style(content),
                }
            };
            self.emit(style, content, builder, at_line_start)?;
            if line_feed {
                self.emit_line_feed(builder, at_line_start)?;
            }
            first = false;
        }
        Ok(())
    }

    fn push_literal_text(
        &mut self,
        text: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        for segment in text.split_inclusive('\n') {
            let (content, line_feed) = segment
                .strip_suffix('\n')
                .map_or((segment, false), |content| (content, true));
            self.push_literal_segment(content, line_feed, builder, at_line_start)?;
        }
        Ok(())
    }

    fn push_literal_segment(
        &mut self,
        content: &str,
        line_feed: bool,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        self.emit(TextStyle::Assistant, content, builder, at_line_start)?;
        if line_feed {
            self.emit_line_feed(builder, at_line_start)?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        requested: TextStyle,
        text: &str,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if text.is_empty() {
            return Ok(());
        }
        let mut style = if self.degraded {
            TextStyle::Assistant
        } else {
            requested
        };
        if style != TextStyle::Assistant && self.last_style != Some(style) {
            if self.style_runs == MAX_STYLE_RUNS {
                self.degraded = true;
                style = TextStyle::Assistant;
            } else {
                self.style_runs += 1;
            }
        }
        builder.push_text(style, text)?;
        self.last_style = Some(style);
        *at_line_start = false;
        Ok(())
    }

    fn emit_line_feed(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        builder.push_line_feed()?;
        self.last_style = None;
        *at_line_start = true;
        Ok(())
    }

    fn has_output_budget(
        &self,
        source_items: usize,
        source_bytes: usize,
        builder: &PresentedChunkBuilder,
    ) -> bool {
        let items_fit = builder
            .item_count()
            .checked_add(source_items)
            .and_then(|items| items.checked_add(MARKUP_ITEM_HEADROOM))
            .is_some_and(|items| items <= MAX_MARKUP_FRAME_ITEMS);
        let bytes_fit = builder
            .text_bytes()
            .checked_add(source_bytes)
            .is_some_and(|bytes| bytes <= MAX_MARKUP_FRAME_TEXT_BYTES);
        items_fit && bytes_fit
    }

    fn omit_output(
        &mut self,
        builder: &mut PresentedChunkBuilder,
        at_line_start: &mut bool,
    ) -> Result<(), PresentationError> {
        if !*at_line_start {
            builder.push_line_feed()?;
        }
        builder.push_text(TextStyle::Muted, DISPLAY_OMITTED)?;
        builder.push_line_feed()?;
        self.block = BlockState::Markdown;
        self.line = LineState::prefix();
        self.inline = InlineState::Text;
        self.inline_disabled = false;
        self.last_style = None;
        self.output_omitted = true;
        *at_line_start = true;
        Ok(())
    }
}

fn estimated_literal_items(text: &str) -> usize {
    let mut items = 0usize;
    for segment in text.split_inclusive('\n') {
        let (content, line_feed) = segment
            .strip_suffix('\n')
            .map_or((segment, false), |content| (content, true));
        if !content.is_empty() {
            items = items.saturating_add(1);
        }
        if line_feed {
            items = items.saturating_add(1);
        }
    }
    items
}

fn append_literal_items(
    mut items: usize,
    mut line_has_text: bool,
    text: &str,
) -> Option<(usize, bool)> {
    for segment in text.split_inclusive('\n') {
        let (content, line_feed) = segment
            .strip_suffix('\n')
            .map_or((segment, false), |content| (content, true));
        if !content.is_empty() && !line_has_text {
            items = items.checked_add(1)?;
            line_has_text = true;
        }
        if line_feed {
            items = items.checked_add(1)?;
            line_has_text = false;
        }
    }
    Some((items, line_has_text))
}

fn literal_item_state(text: &str) -> (usize, bool) {
    append_literal_items(0, false, text).unwrap_or((usize::MAX, true))
}

fn append_literal_items_from(state: (usize, bool), text: &str) -> Option<(usize, bool)> {
    append_literal_items(state.0, state.1, text)
}

fn estimated_literal_text_bytes(text: &str) -> usize {
    text.len().saturating_sub(
        text.as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
    )
}

fn map_buffer_error(error: BufferError) -> PresentationError {
    match error {
        BufferError::Capacity => PresentationError::Capacity,
        BufferError::Limit => PresentationError::Limit,
    }
}

fn append_table_source(buffer: &mut String, text: &str) -> Result<(), BufferError> {
    let next = buffer
        .len()
        .checked_add(text.len())
        .ok_or(BufferError::Limit)?;
    if next > MAX_TABLE_ROW_BYTES {
        return Err(BufferError::Limit);
    }
    buffer
        .try_reserve(text.len())
        .map_err(|_| BufferError::Capacity)?;
    buffer.push_str(text);
    Ok(())
}

const fn table_plain_after(line_feed: bool) -> BlockState {
    if line_feed {
        BlockState::Markdown
    } else {
        BlockState::TablePlain
    }
}

fn table_inner(line: &str) -> Option<&str> {
    if line.len() > MAX_TABLE_ROW_BYTES
        || !line.starts_with('|')
        || !line.ends_with('|')
        || line.as_bytes().windows(2).any(|pair| pair == b"\\|")
    {
        return None;
    }
    line.get(1..line.len().checked_sub(1)?)
}

fn table_row_columns(line: &str) -> Option<usize> {
    let columns = table_inner(line)?.split('|').count();
    (2..=MAX_TABLE_COLUMNS)
        .contains(&columns)
        .then_some(columns)
}

fn table_header_columns(line: &str) -> Option<usize> {
    let inner = table_inner(line)?;
    let columns = inner.split('|').count();
    ((2..=MAX_TABLE_COLUMNS).contains(&columns)
        && inner
            .split('|')
            .all(|cell| !cell.trim_matches([' ', '\t']).is_empty()))
    .then_some(columns)
}

fn is_table_delimiter(line: &str, expected_columns: usize) -> bool {
    let Some(inner) = table_inner(line) else {
        return false;
    };
    let mut columns = 0_usize;
    for cell in inner.split('|') {
        columns += 1;
        let mut marker = cell.trim_matches([' ', '\t']);
        marker = marker.strip_prefix(':').unwrap_or(marker);
        marker = marker.strip_suffix(':').unwrap_or(marker);
        if marker.len() < 3 || !marker.bytes().all(|byte| byte == b'-') {
            return false;
        }
    }
    columns == expected_columns
}

fn classify_prefix(prefix: &str, end_of_line: bool) -> PrefixDecision {
    let bytes = prefix.as_bytes();
    if bytes.is_empty() {
        return if end_of_line {
            PrefixDecision::Body {
                format: LineFormat::Paragraph,
                marker_bytes: 0,
            }
        } else {
            PrefixDecision::NeedMore
        };
    }
    match bytes[0] {
        b'#' => classify_heading(bytes, end_of_line),
        b'-' | b'*' | b'+' => classify_two_byte_marker(bytes, LineFormat::List, end_of_line),
        b'>' => classify_two_byte_marker(bytes, LineFormat::Quote, end_of_line),
        b'0'..=b'9' => classify_ordered_marker(bytes, end_of_line),
        b'`' => classify_fence(prefix, end_of_line),
        _ => PrefixDecision::Body {
            format: LineFormat::Paragraph,
            marker_bytes: 0,
        },
    }
}

fn classify_heading(bytes: &[u8], end_of_line: bool) -> PrefixDecision {
    let count = bytes.iter().take_while(|byte| **byte == b'#').count();
    if count > 3 {
        return paragraph();
    }
    match bytes.get(count) {
        Some(b' ') => PrefixDecision::Body {
            format: LineFormat::Heading,
            marker_bytes: count + 1,
        },
        Some(_) => paragraph(),
        None if end_of_line => paragraph(),
        None => PrefixDecision::NeedMore,
    }
}

fn classify_two_byte_marker(bytes: &[u8], format: LineFormat, end_of_line: bool) -> PrefixDecision {
    match bytes.get(1) {
        Some(b' ') => PrefixDecision::Body {
            format,
            marker_bytes: 2,
        },
        Some(_) => paragraph(),
        None if end_of_line => paragraph(),
        None => PrefixDecision::NeedMore,
    }
}

fn classify_ordered_marker(bytes: &[u8], end_of_line: bool) -> PrefixDecision {
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits > 3 {
        return paragraph();
    }
    match bytes.get(digits) {
        Some(b'.') => match bytes.get(digits + 1) {
            Some(b' ') => PrefixDecision::Body {
                format: LineFormat::List,
                marker_bytes: digits + 2,
            },
            Some(_) => paragraph(),
            None if end_of_line => paragraph(),
            None => PrefixDecision::NeedMore,
        },
        Some(_) => paragraph(),
        None if end_of_line => paragraph(),
        None => PrefixDecision::NeedMore,
    }
}

fn classify_fence(prefix: &str, end_of_line: bool) -> PrefixDecision {
    let bytes = prefix.as_bytes();
    if bytes.len() < 3 && bytes.iter().all(|byte| *byte == b'`') {
        return if end_of_line {
            paragraph()
        } else {
            PrefixDecision::NeedMore
        };
    }
    if !bytes.starts_with(b"```") {
        return paragraph();
    }
    if !end_of_line {
        return PrefixDecision::NeedMore;
    }
    let label = prefix[3..].trim_matches(' ');
    if label.len() > 32
        || !label.is_ascii()
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'.' | b'-'))
    {
        return PrefixDecision::Literal;
    }
    PrefixDecision::FenceOpen(
        if label.eq_ignore_ascii_case("diff") || label.eq_ignore_ascii_case("patch") {
            FenceKind::Diff
        } else {
            FenceKind::Code
        },
    )
}

fn paragraph() -> PrefixDecision {
    PrefixDecision::Body {
        format: LineFormat::Paragraph,
        marker_bytes: 0,
    }
}

fn diff_style(line: &str) -> TextStyle {
    if line.starts_with("--- ") || line.starts_with("+++ ") {
        TextStyle::DiffHeader
    } else if is_hunk_header(line) {
        TextStyle::DiffHunk
    } else if line.starts_with('+') {
        TextStyle::DiffAdd
    } else if line.starts_with('-') {
        TextStyle::DiffRemove
    } else if line == "\\ No newline at end of file" {
        TextStyle::Muted
    } else {
        TextStyle::Code
    }
}

fn is_hunk_header(line: &str) -> bool {
    let Some(mut rest) = line.strip_prefix("@@ -") else {
        return false;
    };
    let Some(after_old) = consume_range(rest) else {
        return false;
    };
    rest = after_old;
    let Some(after_separator) = rest.strip_prefix(" +") else {
        return false;
    };
    let Some(after_new) = consume_range(after_separator) else {
        return false;
    };
    after_new.starts_with(" @@")
}

fn consume_range(input: &str) -> Option<&str> {
    let digits = input.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let mut rest = &input[digits..];
    if let Some(after_comma) = rest.strip_prefix(',') {
        let count = after_comma.bytes().take_while(u8::is_ascii_digit).count();
        if count == 0 {
            return None;
        }
        rest = &after_comma[count..];
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::{
        DISPLAY_OMITTED, MARKUP_ITEM_HEADROOM, MAX_FENCE_BYTES, MAX_INLINE_CODE_BYTES,
        MAX_MARKUP_FRAME_ITEMS, MAX_MARKUP_FRAME_TEXT_BYTES, MAX_STYLE_RUNS, MAX_TABLE_BODY_ROWS,
        MAX_TABLE_BYTES, MAX_TABLE_COLUMNS, MAX_TABLE_ROW_BYTES, MarkupState,
        append_literal_items_from, estimated_literal_items, estimated_literal_text_bytes,
        literal_item_state,
    };
    use crate::tui::presentation::{PresentedChunk, PresentedItem, TextStyle};

    fn render(chunks: &[&str]) -> PresentedChunk {
        let mut state = MarkupState::default();
        let mut builder = PresentedChunk::builder();
        let mut at_line_start = true;
        for chunk in chunks {
            state.push(chunk, &mut builder, &mut at_line_start).unwrap();
        }
        state
            .finish_authoritative(&mut builder, &mut at_line_start)
            .unwrap();
        builder.finish()
    }

    fn render_aborted(chunks: &[&str]) -> PresentedChunk {
        let mut state = MarkupState::default();
        let mut builder = PresentedChunk::builder();
        let mut at_line_start = true;
        for chunk in chunks {
            state.push(chunk, &mut builder, &mut at_line_start).unwrap();
        }
        state.abort_plain(&mut builder, &mut at_line_start).unwrap();
        builder.finish()
    }

    fn plain_text(chunk: &PresentedChunk) -> String {
        let mut output = String::new();
        for item in chunk.items() {
            match item {
                PresentedItem::Text { text, .. } => output.push_str(text),
                PresentedItem::LineFeed => output.push('\n'),
            }
        }
        output
    }

    fn styled_text(chunk: &PresentedChunk, style: TextStyle) -> String {
        let mut output = String::new();
        for item in chunk.items() {
            if let PresentedItem::Text {
                style: item_style,
                text,
            } = item
            {
                if *item_style == style {
                    output.push_str(text);
                }
            }
        }
        output
    }

    fn semantic_shape(chunk: &PresentedChunk) -> Vec<(Option<TextStyle>, usize)> {
        chunk
            .items()
            .iter()
            .map(|item| match item {
                PresentedItem::Text { style, text } => (Some(*style), text.len()),
                PresentedItem::LineFeed => (None, 1),
            })
            .collect()
    }

    #[test]
    fn semantic_blocks_keep_every_source_byte_and_use_closed_styles() {
        let source = concat!(
            "# Heading\n",
            "- item with `inline` code\n",
            "> quoted text\n",
            "```rust\n",
            "fn main() {}\n",
            "```\n",
            "```diff\n",
            "--- a/file\n",
            "+++ b/file\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "```\n",
        );
        let chunk = render(&[source]);
        assert_eq!(plain_text(&chunk), source);
        assert!(styled_text(&chunk, TextStyle::Heading).contains("# Heading"));
        assert!(styled_text(&chunk, TextStyle::Code).contains("`inline`"));
        assert!(styled_text(&chunk, TextStyle::Quote).contains("quoted text"));
        assert!(styled_text(&chunk, TextStyle::DiffHeader).contains("--- a/file"));
        assert!(styled_text(&chunk, TextStyle::DiffHunk).contains("@@ -1 +1 @@"));
        assert!(styled_text(&chunk, TextStyle::DiffRemove).contains("-old"));
        assert!(styled_text(&chunk, TextStyle::DiffAdd).contains("+new"));
    }

    #[test]
    fn every_two_chunk_split_has_the_same_text_and_styles() {
        let source = "## 标题\n- `值`\n```diff\n-old\n+新\n```\n";
        let whole = render(&[source]);
        let expected_shape = semantic_shape(&whole);
        for split in (0..=source.len()).filter(|index| source.is_char_boundary(*index)) {
            let split_render = render(&[&source[..split], &source[split..]]);
            assert_eq!(plain_text(&split_render), source, "split {split}");
            assert_eq!(
                semantic_shape(&split_render),
                expected_shape,
                "split {split}"
            );
        }
        let mut boundaries = source
            .char_indices()
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        boundaries.push(source.len());
        let fragments = boundaries
            .windows(2)
            .map(|pair| &source[pair[0]..pair[1]])
            .collect::<Vec<_>>();
        let fragmented = render(&fragments);
        assert_eq!(plain_text(&fragmented), source);
        assert_eq!(semantic_shape(&fragmented), expected_shape);
    }

    #[test]
    fn unclosed_inline_and_fence_finish_as_plain_without_losing_text() {
        for source in ["before `secret", "```rust\nsecret\n"] {
            let chunk = render(&[source]);
            assert_eq!(plain_text(&chunk), source);
            assert!(chunk.items().iter().all(|item| matches!(
                item,
                PresentedItem::LineFeed
                    | PresentedItem::Text {
                        style: TextStyle::Assistant,
                        ..
                    }
            )));
            assert!(styled_text(&chunk, TextStyle::Code).is_empty());
        }
    }

    #[test]
    fn a_closing_fence_at_the_authoritative_eof_is_still_closed() {
        let source = "```rust\ncode-at-eof\n```";
        let chunk = render(&["``", "`rust\ncode-at-eof\n``", "`"]);
        assert_eq!(plain_text(&chunk), source);
        assert_eq!(styled_text(&chunk, TextStyle::Code), "code-at-eof");
    }

    #[test]
    fn an_aborted_eof_closer_never_promotes_partial_output_to_code() {
        let source = "```rust\npartial\n```";
        let chunk = render_aborted(&["```rust\npartial\n``", "`"]);
        assert_eq!(plain_text(&chunk), source);
        assert!(chunk.items().iter().all(|item| matches!(
            item,
            PresentedItem::LineFeed
                | PresentedItem::Text {
                    style: TextStyle::Assistant,
                    ..
                }
        )));
    }

    #[test]
    fn fence_shaped_but_invalid_openers_remain_entirely_literal() {
        let eof_only = render(&["```rust"]);
        assert_eq!(plain_text(&eof_only), "```rust");
        assert!(styled_text(&eof_only, TextStyle::Code).is_empty());

        let long_label = "r".repeat(33);
        let source = format!("```{long_label}\nbody\n```\n");
        let chunk = render(&[&source]);
        assert_eq!(plain_text(&chunk), source);
        assert!(chunk.items().iter().all(|item| matches!(
            item,
            PresentedItem::LineFeed
                | PresentedItem::Text {
                    style: TextStyle::Assistant,
                    ..
                }
        )));
    }

    #[test]
    fn inline_limit_exact_styles_and_one_over_degrades_atomically() {
        let exact = format!("`{}`", "x".repeat(MAX_INLINE_CODE_BYTES - 2));
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);
        assert_eq!(styled_text(&exact_chunk, TextStyle::Code), exact);

        let over = format!("`{}` after", "s".repeat(MAX_INLINE_CODE_BYTES - 1));
        let over_chunk = render(&[&over]);
        assert_eq!(plain_text(&over_chunk), over);
        assert!(styled_text(&over_chunk, TextStyle::Code).is_empty());
    }

    #[test]
    fn language_label_limit_accepts_32_ascii_bytes_and_rejects_one_more() {
        let exact_label = "r".repeat(32);
        let exact = format!("```{exact_label}\ncode-exact\n```\n");
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);
        assert_eq!(styled_text(&exact_chunk, TextStyle::Code), "code-exact");

        let over_label = "r".repeat(33);
        let over = format!("```{over_label}\ncode-over\n```\n");
        let over_chunk = render(&[&over]);
        assert_eq!(plain_text(&over_chunk), over);
        assert!(!styled_text(&over_chunk, TextStyle::Code).contains("code-over"));
    }

    #[test]
    fn fence_limit_exact_styles_and_one_over_degrades_independently_of_chunks() {
        let exact_body = "x".repeat(MAX_FENCE_BYTES - 9);
        let exact = format!("```\n{exact_body}\n```\n");
        assert_eq!(exact.len(), MAX_FENCE_BYTES);
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);
        assert_eq!(styled_text(&exact_chunk, TextStyle::Code), exact_body);

        let over_body = "y".repeat(MAX_FENCE_BYTES - 8);
        let over = format!("```\n{over_body}\n```\n");
        assert_eq!(over.len(), MAX_FENCE_BYTES + 1);
        let whole = render(&[&over]);
        let split = render(&[&over[..1024], &over[1024..]]);
        assert_eq!(plain_text(&whole), over);
        assert_eq!(plain_text(&split), over);
        assert_eq!(semantic_shape(&whole), semantic_shape(&split));
        assert!(styled_text(&whole, TextStyle::Code).is_empty());
    }

    #[test]
    fn exact_fence_budget_with_maximum_short_lines_stays_below_item_capacity() {
        let body = "x\n".repeat((MAX_FENCE_BYTES - 8) / 2);
        let source = format!("```\n{body}```\n");
        assert_eq!(source.len(), MAX_FENCE_BYTES);
        let chunk = render(&[&source]);
        assert_eq!(plain_text(&chunk), source);
        assert_eq!(
            styled_text(&chunk, TextStyle::Code).len(),
            MAX_STYLE_RUNS - 1
        );
        assert!(chunk.items().len() < 128 * 1024);
    }

    #[test]
    fn retained_fence_and_same_push_tail_obey_the_frame_soft_item_budget() {
        let retained = format!("```\n{}x", "\n".repeat(60_000));
        let continuation_and_closing = "y\n```\n";
        assert!(retained.len() + continuation_and_closing.len() < MAX_FENCE_BYTES);
        let retained_state = literal_item_state(&retained);
        assert!(retained_state.1, "the retained fence must end mid-line");
        let committed_state =
            append_literal_items_from(retained_state, continuation_and_closing).unwrap();
        let available_tail_items =
            MAX_MARKUP_FRAME_ITEMS - MARKUP_ITEM_HEADROOM - committed_state.0;
        let mut exact_tail = "x\n".repeat(available_tail_items / 2);
        if available_tail_items % 2 != 0 {
            exact_tail.push('x');
        }

        let render_cross_frame = |current: &str| {
            let mut state = MarkupState::default();
            let mut at_line_start = true;
            let mut first_frame = PresentedChunk::builder();
            state
                .push(&retained, &mut first_frame, &mut at_line_start)
                .unwrap();
            assert!(first_frame.finish().items().is_empty());

            let mut second_frame = PresentedChunk::builder();
            state
                .push(current, &mut second_frame, &mut at_line_start)
                .unwrap();
            state
                .finish_authoritative(&mut second_frame, &mut at_line_start)
                .unwrap();
            second_frame.finish()
        };

        let exact_current = format!("{continuation_and_closing}{exact_tail}");
        let exact = render_cross_frame(&exact_current);
        assert_eq!(plain_text(&exact), format!("{retained}{exact_current}"));
        assert!(exact.items().len() < MAX_MARKUP_FRAME_ITEMS);

        let over = render_cross_frame(&format!("{exact_current}\n"));
        assert_eq!(plain_text(&over), format!("{DISPLAY_OMITTED}\n"));
    }

    #[test]
    fn ordinary_line_item_budget_exact_and_one_over_degrade_without_error() {
        let exact_lines = (MAX_MARKUP_FRAME_ITEMS - MARKUP_ITEM_HEADROOM) / 2;
        let exact = "x\n".repeat(exact_lines);
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);

        let over = "x\n".repeat(exact_lines + 1);
        let over_chunk = render(&[&over]);
        assert_eq!(plain_text(&over_chunk), format!("{DISPLAY_OMITTED}\n"));

        let allowed_items = MAX_MARKUP_FRAME_ITEMS - MARKUP_ITEM_HEADROOM;
        let same_line_prefix = format!("{}\nx", "x\n".repeat((allowed_items - 2) / 2));
        assert_eq!(estimated_literal_items(&same_line_prefix), allowed_items);
        let same_line = render(&[&same_line_prefix, "y"]);
        assert_eq!(plain_text(&same_line), format!("{same_line_prefix}y"));

        let mut state = MarkupState::default();
        let mut builder = PresentedChunk::builder();
        let mut at_line_start = true;
        state
            .push(&same_line_prefix, &mut builder, &mut at_line_start)
            .unwrap();
        state.push("\n", &mut builder, &mut at_line_start).unwrap();
        state
            .finish_authoritative(&mut builder, &mut at_line_start)
            .unwrap();
        assert_eq!(
            plain_text(&builder.finish()),
            format!("{same_line_prefix}\n{DISPLAY_OMITTED}\n")
        );
    }

    #[test]
    fn ordinary_text_byte_budget_exact_and_one_over_degrade_without_error() {
        let exact = "x".repeat(MAX_MARKUP_FRAME_TEXT_BYTES);
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);

        let over = "x".repeat(MAX_MARKUP_FRAME_TEXT_BYTES + 1);
        let over_chunk = render(&[&over]);
        assert_eq!(plain_text(&over_chunk), format!("{DISPLAY_OMITTED}\n"));
    }

    #[test]
    fn style_run_limit_is_deterministic_and_one_over_becomes_plain() {
        let exact = "`x`a".repeat(MAX_STYLE_RUNS);
        let exact_chunk = render(&[&exact]);
        assert_eq!(plain_text(&exact_chunk), exact);
        assert_eq!(
            styled_text(&exact_chunk, TextStyle::Code)
                .matches("`x`")
                .count(),
            MAX_STYLE_RUNS
        );

        let over = "`x`a".repeat(MAX_STYLE_RUNS + 1);
        let over_chunk = render(&[&over[..over.len() / 2], &over[over.len() / 2..]]);
        assert_eq!(plain_text(&over_chunk), over);
        assert_eq!(
            styled_text(&over_chunk, TextStyle::Code)
                .matches("`x`")
                .count(),
            MAX_STYLE_RUNS
        );
    }

    #[test]
    fn diff_headers_precede_add_remove_and_malformed_hunks_stay_code() {
        let source = "```diff\n--- a\n+++ b\n@@ nope @@\n-list\n+item\n```\n";
        let chunk = render(&[source]);
        assert_eq!(styled_text(&chunk, TextStyle::DiffHeader), "--- a+++ b");
        assert!(styled_text(&chunk, TextStyle::Code).contains("@@ nope @@"));
        assert_eq!(styled_text(&chunk, TextStyle::DiffRemove), "-list");
        assert_eq!(styled_text(&chunk, TextStyle::DiffAdd), "+item");
    }

    #[test]
    fn source_preserving_tables_are_fragment_independent_and_semantically_styled() {
        let source = concat!(
            "before\n",
            "| Name | Value |\n",
            "| :--- | ---: |\n",
            "| α | `1` |\n",
            "| beta | 2 |\n",
            "after\n",
        );
        let whole = render(&[source]);
        assert_eq!(plain_text(&whole), source);
        assert!(styled_text(&whole, TextStyle::Heading).contains(" Name "));
        assert!(styled_text(&whole, TextStyle::Heading).contains(" Value "));
        assert!(styled_text(&whole, TextStyle::Border).contains("| :--- | ---: |"));
        assert!(styled_text(&whole, TextStyle::Border).matches('|').count() >= 12);

        let expected = semantic_shape(&whole);
        for split in (0..=source.len()).filter(|index| source.is_char_boundary(*index)) {
            let split_chunk = render(&[&source[..split], &source[split..]]);
            assert_eq!(plain_text(&split_chunk), source, "split {split}");
            assert_eq!(semantic_shape(&split_chunk), expected, "split {split}");
        }
    }

    #[test]
    fn false_incomplete_and_aborted_table_candidates_remain_plain() {
        for source in [
            "| not | a table |\nordinary\n",
            "| A | B |\n| -- | --- |\n| x | y |\n",
            "| A\\|B | C |\n| --- | --- |\n| x | y |\n",
            "| A | B |\n| --- | --- | --- |\n| x | y |\n",
        ] {
            let chunk = render(&[source]);
            assert_eq!(plain_text(&chunk), source);
            assert!(styled_text(&chunk, TextStyle::Border).is_empty());
            assert!(styled_text(&chunk, TextStyle::Heading).is_empty());
        }

        let wrong_body_columns = "| A | B |\n| --- | --- |\n| wrong columns |\n";
        let wrong_body_chunk = render(&[wrong_body_columns]);
        assert_eq!(plain_text(&wrong_body_chunk), wrong_body_columns);
        assert!(styled_text(&wrong_body_chunk, TextStyle::Heading).contains(" A "));

        let held = "| SECRET_HEADER | value |";
        let aborted = render_aborted(&[held]);
        assert_eq!(plain_text(&aborted), held);
        assert!(styled_text(&aborted, TextStyle::Border).is_empty());
        assert!(styled_text(&aborted, TextStyle::Heading).is_empty());

        let table_with_final_row = "| A | B |\n| --- | --- |\n| final | row |";
        let authoritative = render(&[table_with_final_row]);
        let aborted = render_aborted(&[table_with_final_row]);
        assert_eq!(plain_text(&authoritative), table_with_final_row);
        assert_eq!(plain_text(&aborted), table_with_final_row);
        assert_eq!(
            styled_text(&authoritative, TextStyle::Border)
                .matches('|')
                .count(),
            styled_text(&aborted, TextStyle::Border)
                .matches('|')
                .count()
                + 3
        );
    }

    #[test]
    fn table_column_row_and_physical_line_limits_degrade_without_losing_source() {
        let header8 = format!(
            "| {} |\n",
            (0..MAX_TABLE_COLUMNS)
                .map(|n| format!("h{n}"))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let delimiter8 = format!(
            "| {} |\n",
            std::iter::repeat_n("---", MAX_TABLE_COLUMNS)
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let body8 = format!(
            "| {} |\n",
            std::iter::repeat_n("x", MAX_TABLE_COLUMNS)
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let exact_rows = format!("{header8}{delimiter8}{}", body8.repeat(MAX_TABLE_BODY_ROWS));
        let exact_chunk = render(&[&exact_rows]);
        assert_eq!(plain_text(&exact_chunk), exact_rows);
        assert!(styled_text(&exact_chunk, TextStyle::Heading).contains("h7"));

        let one_over_row = format!("{exact_rows}{body8}");
        let over_row_chunk = render(&[&one_over_row]);
        assert_eq!(plain_text(&over_row_chunk), one_over_row);
        assert_eq!(
            styled_text(&over_row_chunk, TextStyle::Border)
                .matches('|')
                .count(),
            styled_text(&exact_chunk, TextStyle::Border)
                .matches('|')
                .count()
        );

        let header9 = format!(
            "| {} |\n",
            (0..=MAX_TABLE_COLUMNS)
                .map(|n| format!("h{n}"))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let delimiter9 = format!(
            "| {} |\n",
            std::iter::repeat_n("---", MAX_TABLE_COLUMNS + 1)
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let columns9 = format!("{header9}{delimiter9}");
        let columns9_chunk = render(&[&columns9]);
        assert_eq!(plain_text(&columns9_chunk), columns9);
        assert!(styled_text(&columns9_chunk, TextStyle::Border).is_empty());
        assert!(styled_text(&columns9_chunk, TextStyle::Heading).is_empty());

        let exact_cell = "x".repeat(MAX_TABLE_ROW_BYTES - "|  | y |\n".len());
        let exact_line = format!("| {exact_cell} | y |\n");
        assert_eq!(exact_line.len(), MAX_TABLE_ROW_BYTES);
        let exact_source = format!("{exact_line}| --- | --- |\n");
        let exact_line_chunk = render(&[&exact_source]);
        assert_eq!(plain_text(&exact_line_chunk), exact_source);
        assert!(styled_text(&exact_line_chunk, TextStyle::Heading).contains(" y "));
        assert!(!styled_text(&exact_line_chunk, TextStyle::Border).is_empty());

        let over_cell = "x".repeat(MAX_TABLE_ROW_BYTES - "|  | y |\n".len() + 1);
        let over_line = format!("| {over_cell} | y |\n| --- | --- |\n");
        let over_line_chunk = render(&[&over_line]);
        assert_eq!(plain_text(&over_line_chunk), over_line);
        assert!(styled_text(&over_line_chunk, TextStyle::Border).is_empty());

        let prefix = "| h | v |\n| --- | --- |\n";
        let remaining = MAX_TABLE_BYTES - prefix.len();
        let mut aggregate_exact = String::from(prefix);
        for index in 0..4 {
            let row_bytes = remaining / 4 + usize::from(index < remaining % 4);
            let cell = "z".repeat(row_bytes - "|  | y |\n".len());
            aggregate_exact.push_str(&format!("| {cell} | y |\n"));
        }
        assert_eq!(aggregate_exact.len(), MAX_TABLE_BYTES);
        let aggregate_chunk = render(&[&aggregate_exact]);
        assert_eq!(plain_text(&aggregate_chunk), aggregate_exact);
        assert!(styled_text(&aggregate_chunk, TextStyle::Heading).contains(" h "));
        let aggregate_border_count = styled_text(&aggregate_chunk, TextStyle::Border)
            .matches('|')
            .count();

        let aggregate_over = format!("{aggregate_exact}| x | y |\n");
        let aggregate_over_chunk = render(&[&aggregate_over]);
        assert_eq!(plain_text(&aggregate_over_chunk), aggregate_over);
        assert_eq!(
            styled_text(&aggregate_over_chunk, TextStyle::Border)
                .matches('|')
                .count(),
            aggregate_border_count
        );

        let same_line_prefix = format!("|{}", "q".repeat(MAX_TABLE_ROW_BYTES - 1));
        let fragmented_overflow = format!("{same_line_prefix}x# forged heading\n");
        let split_overflow = render(&[&same_line_prefix, "x", "# forged heading\n"]);
        let whole_overflow = render(&[&fragmented_overflow]);
        assert_eq!(plain_text(&split_overflow), fragmented_overflow);
        assert_eq!(
            semantic_shape(&split_overflow),
            semantic_shape(&whole_overflow)
        );
        assert!(!styled_text(&split_overflow, TextStyle::Heading).contains("forged"));
        assert!(styled_text(&split_overflow, TextStyle::Border).is_empty());

        let aggregate_tail = "| over aggregate# forged heading\n";
        let split_aggregate_overflow =
            render(&[&aggregate_exact, "| over aggregate", "# forged heading\n"]);
        let whole_aggregate_overflow = render(&[&format!("{aggregate_exact}{aggregate_tail}")]);
        assert_eq!(
            plain_text(&split_aggregate_overflow),
            format!("{aggregate_exact}{aggregate_tail}")
        );
        assert_eq!(
            semantic_shape(&split_aggregate_overflow),
            semantic_shape(&whole_aggregate_overflow)
        );
        assert!(!styled_text(&split_aggregate_overflow, TextStyle::Heading).contains("forged"));

        let allowed_items = MAX_MARKUP_FRAME_ITEMS - MARKUP_ITEM_HEADROOM;
        let soft_base = format!("{}\n", "x\n".repeat((allowed_items - 2) / 2));
        assert_eq!(estimated_literal_items(&soft_base), allowed_items - 1);
        let soft_header = format!("|{}", "q".repeat(MAX_TABLE_ROW_BYTES - 1));
        let soft_whole_source = format!("{soft_base}{soft_header}x");
        let soft_whole = render(&[&soft_whole_source]);
        let soft_split = render(&[&soft_base, &soft_header, "x"]);
        assert_eq!(plain_text(&soft_whole), soft_whole_source);
        assert_eq!(plain_text(&soft_split), soft_whole_source);
        assert_eq!(semantic_shape(&soft_split), semantic_shape(&soft_whole));

        let soft_over = render(&[&soft_base, &soft_header, "x\n"]);
        assert_eq!(
            plain_text(&soft_over),
            format!("{soft_base}{DISPLAY_OMITTED}\n")
        );
    }

    #[test]
    fn retained_table_source_obeys_the_frame_soft_byte_budget() {
        let table = "| A | B |\n| --- | --- |\n";
        let render_fragmented = |base_text_bytes: usize| {
            let mut base = "x".repeat(base_text_bytes);
            base.push('\n');
            let mut boundaries = table
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            boundaries.push(table.len());
            let mut fragments = Vec::with_capacity(boundaries.len());
            fragments.push(base.as_str());
            fragments.extend(boundaries.windows(2).map(|pair| &table[pair[0]..pair[1]]));
            let rendered = render(&fragments);
            drop(fragments);
            (base, rendered)
        };

        let table_text_bytes = table.len() - table.matches('\n').count();
        let (exact_base, exact) = render_fragmented(MAX_MARKUP_FRAME_TEXT_BYTES - table_text_bytes);
        assert_eq!(plain_text(&exact), format!("{exact_base}{table}"));
        assert!(styled_text(&exact, TextStyle::Heading).contains(" A "));

        let (over_base, over) =
            render_fragmented(MAX_MARKUP_FRAME_TEXT_BYTES - table_text_bytes + 1);
        let over_text = plain_text(&over);
        let over_tail = over_text
            .strip_prefix(&over_base)
            .expect("the accepted prefix must remain exact");
        assert_eq!(over_tail, format!("{DISPLAY_OMITTED}\n"));

        let final_row = "| final | row |";
        let mut finish_base =
            "x".repeat(MAX_MARKUP_FRAME_TEXT_BYTES - table_text_bytes - final_row.len() + 1);
        finish_base.push('\n');
        let body_fragments = final_row
            .char_indices()
            .map(|(index, character)| &final_row[index..index + character.len_utf8()])
            .collect::<Vec<_>>();
        let mut fragments = vec![finish_base.as_str(), table];
        fragments.extend(body_fragments);
        let finished = render(&fragments);
        let aborted = render_aborted(&fragments);
        let expected = format!("{finish_base}{table}{DISPLAY_OMITTED}\n");
        assert_eq!(plain_text(&finished), expected);
        assert_eq!(plain_text(&aborted), expected);

        let header = "| retained header | value |\n";
        let delimiter = "| --- | --- |\n";
        let cross_frame = |tail_bytes: usize| {
            let mut state = MarkupState::default();
            let mut at_line_start = true;
            let mut first_frame = PresentedChunk::builder();
            state
                .push(header, &mut first_frame, &mut at_line_start)
                .unwrap();
            assert!(first_frame.finish().items().is_empty());

            let tail = "x".repeat(tail_bytes);
            let current = format!("{delimiter}{tail}");
            let mut second_frame = PresentedChunk::builder();
            state
                .push(&current, &mut second_frame, &mut at_line_start)
                .unwrap();
            state
                .finish_authoritative(&mut second_frame, &mut at_line_start)
                .unwrap();
            (tail, second_frame.finish())
        };
        let held_table_text_bytes =
            estimated_literal_text_bytes(header) + estimated_literal_text_bytes(delimiter);
        let (exact_tail, exact_cross_frame) =
            cross_frame(MAX_MARKUP_FRAME_TEXT_BYTES - held_table_text_bytes);
        assert_eq!(
            plain_text(&exact_cross_frame),
            format!("{header}{delimiter}{exact_tail}")
        );
        assert!(styled_text(&exact_cross_frame, TextStyle::Heading).contains(" retained header "));

        let (_, over_cross_frame) =
            cross_frame(MAX_MARKUP_FRAME_TEXT_BYTES - held_table_text_bytes + 1);
        assert_eq!(
            plain_text(&over_cross_frame),
            format!("{DISPLAY_OMITTED}\n")
        );
    }

    #[test]
    fn entities_are_literal_and_cannot_create_terminal_or_line_controls() {
        let source = "# &#27; &#x202e; &#10;\n";
        let chunk = render(&[source]);
        assert_eq!(plain_text(&chunk), source);
        assert!(!plain_text(&chunk).contains('\u{1b}'));
        assert_eq!(plain_text(&chunk).matches('\n').count(), 1);
    }

    #[test]
    fn pending_source_is_redacted_from_debug() {
        let mut state = MarkupState::default();
        let mut builder = PresentedChunk::builder();
        let mut at_line_start = true;
        state
            .push("`secret-token", &mut builder, &mut at_line_start)
            .unwrap();
        assert!(!format!("{state:?}").contains("secret-token"));

        let mut table = MarkupState::default();
        table
            .push(
                "| SECRET_TABLE_CELL | value |",
                &mut builder,
                &mut at_line_start,
            )
            .unwrap();
        assert!(!format!("{table:?}").contains("SECRET_TABLE_CELL"));
    }
}
