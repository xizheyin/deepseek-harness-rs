//! Provider-neutral messages to DeepSeek chat-completions JSON.

use std::{
    io::{self, Write},
    sync::Arc,
};

use serde::Serialize;
use serde_json::Value;
#[cfg(test)]
use serde_json::{Map, json};
use thiserror::Error;

use crate::{
    attachment::{AttachmentRuntime, retained_request_images},
    model::{
        ContentBlock, ContentBlockKind, ImageAttachmentRef, ImageMediaType, Message, MessageRole,
    },
    provider::{MAX_PROVIDER_REQUEST_BYTES, ProviderRequest, ProviderRequestDraft, RequestPurpose},
};

use super::config::{
    DEEPSEEK_PROVIDER, DEEPSEEK_VISION_MODEL, DeepSeekConfig, DeepSeekReasoningEffort,
    DeepSeekThinking,
};

pub(super) fn serialize_request(
    config: &DeepSeekConfig,
    attachments: Option<&AttachmentRuntime>,
    request: &ProviderRequest,
) -> Result<Vec<u8>, RequestBuildError> {
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(MAX_PROVIDER_REQUEST_BYTES)
        .map_err(|_| RequestBuildError::Capacity)?;
    let encoded_bytes = encode_request(
        config,
        WireRequest {
            config: request.config(),
            system: request.system(),
            messages: request.messages(),
            tools: request.tools(),
            purpose: request.purpose(),
        },
        attachments,
        &mut encoded,
    )?;
    if request
        .preflight_encoded_bytes()
        .is_some_and(|expected| expected != encoded_bytes)
    {
        return Err(RequestBuildError::PreflightMismatch);
    }
    Ok(encoded)
}

pub(super) fn preflight_request_len(
    config: &DeepSeekConfig,
    attachments: Option<&AttachmentRuntime>,
    effective_config: &crate::model::LlmCallConfig,
    draft: ProviderRequestDraft<'_>,
) -> Result<usize, RequestBuildError> {
    encode_request(
        config,
        WireRequest {
            config: effective_config,
            system: draft.system(),
            messages: draft.messages(),
            tools: draft.tools(),
            purpose: draft.purpose(),
        },
        attachments,
        &mut io::sink(),
    )
}

#[derive(Clone, Copy)]
struct WireRequest<'a> {
    config: &'a crate::model::LlmCallConfig,
    system: Option<&'a str>,
    messages: &'a [Message],
    tools: &'a [crate::model::ToolSchema],
    purpose: RequestPurpose,
}

fn encode_request(
    adapter: &DeepSeekConfig,
    request: WireRequest<'_>,
    attachments: Option<&AttachmentRuntime>,
    output: &mut impl Write,
) -> Result<usize, RequestBuildError> {
    if request.config.provider() != DEEPSEEK_PROVIDER {
        return Err(RequestBuildError::WrongProvider);
    }
    let has_images = request
        .messages
        .iter()
        .any(|message| content_has_image_without_allocation(message.content()));
    if has_images && request.config.model() != DEEPSEEK_VISION_MODEL {
        return Err(RequestBuildError::UnsupportedContent);
    }
    if has_images && attachments.is_none() {
        return Err(RequestBuildError::AttachmentUnavailable);
    }
    if request.messages.iter().any(|message| {
        message.role() != MessageRole::User
            && content_has_image_without_allocation(message.content())
    }) {
        return Err(RequestBuildError::UnsupportedContent);
    }
    let (thinking, effort) = resolve_thinking_parts(adapter, request.config, request.purpose)?;
    let max_tokens = request
        .config
        .max_tokens()
        .ok_or(RequestBuildError::UnpreparedConfig)?
        .get();
    if max_tokens == 0 {
        return Err(RequestBuildError::InvalidMaxTokens);
    }

    let mut writer = WireWriter::new(output);
    writer.raw(b"{")?;
    writer.raw(b"\"max_tokens\":")?;
    writer.json(&max_tokens)?;
    writer.raw(b",\"messages\":[")?;
    write_messages(&mut writer, request.system, request.messages, attachments)?;
    writer.raw(b"],\"model\":")?;
    writer.json(&request.config.model())?;
    if let Some(effort) = effort {
        writer.raw(b",\"reasoning_effort\":")?;
        writer.json(&match effort {
            DeepSeekReasoningEffort::High => "high",
            DeepSeekReasoningEffort::Max => "max",
            DeepSeekReasoningEffort::Off => {
                return Err(RequestBuildError::InvalidResolvedThinking);
            }
        })?;
    }
    if let Some(stop) = request.config.stop() {
        writer.raw(b",\"stop\":")?;
        writer.json(&stop)?;
    }
    writer.raw(b",\"stream\":true,\"stream_options\":{\"include_usage\":true}")?;
    if let Some(temperature) = request.config.temperature() {
        writer.raw(b",\"temperature\":")?;
        writer.json(&temperature)?;
    }
    writer.raw(b",\"thinking\":{\"type\":")?;
    writer.json(&match thinking {
        DeepSeekThinking::Enabled => "enabled",
        DeepSeekThinking::Disabled => "disabled",
    })?;
    writer.raw(b"}")?;
    if !request.tools.is_empty() {
        writer.raw(b",\"tools\":[")?;
        for (index, tool) in request.tools.iter().enumerate() {
            if index > 0 {
                writer.raw(b",")?;
            }
            writer.raw(b"{\"function\":{\"description\":")?;
            writer.json(&tool.description())?;
            writer.raw(b",\"name\":")?;
            writer.json(&tool.name())?;
            writer.raw(b",\"parameters\":")?;
            writer.json(tool.parameters().as_value())?;
            writer.raw(b"},\"type\":\"function\"}")?;
        }
        writer.raw(b"]")?;
    }
    writer.raw(b"}")?;
    let written = writer.written();
    if written > MAX_PROVIDER_REQUEST_BYTES {
        return Err(RequestBuildError::TooLarge {
            maximum: MAX_PROVIDER_REQUEST_BYTES,
            actual: written,
        });
    }
    Ok(written)
}

fn write_messages(
    writer: &mut WireWriter<'_, impl Write>,
    system: Option<&str>,
    messages: &[Message],
    attachments: Option<&AttachmentRuntime>,
) -> Result<(), RequestBuildError> {
    let mut first = true;
    let mut scratch = String::new();
    let mut images = attachments.map(|runtime| ImageWireState::new(runtime, messages));
    if let Some(system) = system {
        write_simple_message(writer, &mut first, "system", system)?;
    }
    for message in messages {
        match message.role() {
            MessageRole::System => {
                concat_strings_into(
                    &mut scratch,
                    message
                        .content()
                        .iter()
                        .filter_map(|block| match block.kind() {
                            ContentBlockKind::Text { text } => Some(text.as_str()),
                            _ => None,
                        }),
                )?;
                write_simple_message(writer, &mut first, "system", &scratch)?;
            }
            MessageRole::Assistant => {
                write_assistant_message(writer, &mut first, message, &mut scratch)?;
            }
            MessageRole::User => {
                write_user_messages(writer, &mut first, message, &mut scratch, &mut images)?;
            }
        }
    }
    Ok(())
}

fn write_separator(
    writer: &mut WireWriter<'_, impl Write>,
    first: &mut bool,
) -> Result<(), RequestBuildError> {
    if *first {
        *first = false;
        Ok(())
    } else {
        writer.raw(b",")
    }
}

fn write_simple_message(
    writer: &mut WireWriter<'_, impl Write>,
    first: &mut bool,
    role: &str,
    content: &str,
) -> Result<(), RequestBuildError> {
    write_separator(writer, first)?;
    writer.raw(b"{\"content\":")?;
    writer.json(&content)?;
    writer.raw(b",\"role\":")?;
    writer.json(&role)?;
    writer.raw(b"}")
}

fn write_assistant_message(
    writer: &mut WireWriter<'_, impl Write>,
    first: &mut bool,
    message: &Message,
    scratch: &mut String,
) -> Result<(), RequestBuildError> {
    let has_tool_calls = message
        .content()
        .iter()
        .any(|block| matches!(block.kind(), ContentBlockKind::ToolCall { .. }));
    concat_strings_into(
        scratch,
        message
            .content()
            .iter()
            .filter_map(|block| match block.kind() {
                ContentBlockKind::Text { text } => Some(text.as_str()),
                _ => None,
            }),
    )?;

    write_separator(writer, first)?;
    writer.raw(b"{\"content\":")?;
    writer.json(&scratch.as_str())?;
    if has_tool_calls {
        concat_strings_into(
            scratch,
            message
                .content()
                .iter()
                .filter_map(|block| match block.kind() {
                    ContentBlockKind::Reasoning { text } => Some(text.as_str()),
                    _ => None,
                }),
        )?;
        if !scratch.is_empty() {
            writer.raw(b",\"reasoning_content\":")?;
            writer.json(&scratch.as_str())?;
        }
    }
    writer.raw(b",\"role\":\"assistant\"")?;
    if has_tool_calls {
        writer.raw(b",\"tool_calls\":[")?;
        for (index, block) in message
            .content()
            .iter()
            .filter(|block| matches!(block.kind(), ContentBlockKind::ToolCall { .. }))
            .enumerate()
        {
            let ContentBlockKind::ToolCall {
                id,
                name,
                arguments,
            } = block.kind()
            else {
                continue;
            };
            if index > 0 {
                writer.raw(b",")?;
            }
            writer.raw(b"{\"function\":{\"arguments\":")?;
            writer.json(&arguments.as_str())?;
            writer.raw(b",\"name\":")?;
            writer.json(&name.as_str())?;
            writer.raw(b"},\"id\":")?;
            writer.json(&id.as_str())?;
            writer.raw(b",\"type\":\"function\"}")?;
        }
        writer.raw(b"]")?;
    }
    writer.raw(b"}")
}

fn write_user_messages(
    writer: &mut WireWriter<'_, impl Write>,
    first: &mut bool,
    message: &Message,
    scratch: &mut String,
    images: &mut Option<ImageWireState<'_>>,
) -> Result<(), RequestBuildError> {
    let has_tool_results = message
        .content()
        .iter()
        .any(|block| matches!(block.kind(), ContentBlockKind::ToolResult { .. }));
    let regular_has_images = message.content().iter().any(|block| {
        !matches!(block.kind(), ContentBlockKind::ToolResult { .. })
            && raw_value_has_image(block.raw().as_value())
    });
    if regular_has_images {
        let image_state = images
            .as_mut()
            .ok_or(RequestBuildError::AttachmentUnavailable)?;
        write_separator(writer, first)?;
        writer.raw(b"{\"content\":[")?;
        let mut first_part = true;
        for block in message
            .content()
            .iter()
            .filter(|block| !matches!(block.kind(), ContentBlockKind::ToolResult { .. }))
        {
            match block.kind() {
                ContentBlockKind::Text { text } if !text.is_empty() => {
                    write_text_part(writer, &mut first_part, text)?;
                }
                ContentBlockKind::Image { attachment } => {
                    write_image_occurrence(writer, &mut first_part, image_state, attachment)?;
                }
                _ if raw_value_has_image(block.raw().as_value()) => {
                    return Err(RequestBuildError::UnsupportedContent);
                }
                _ => {}
            }
        }
        writer.raw(b"],\"role\":\"user\"}")?;
    } else {
        concat_strings_into(
            scratch,
            message
                .content()
                .iter()
                .filter_map(|block| match block.kind() {
                    ContentBlockKind::Text { text } => Some(text.as_str()),
                    _ => None,
                }),
        )?;
        if !scratch.is_empty() || !has_tool_results {
            write_simple_message(writer, first, "user", scratch)?;
        }
    }
    let mut pending_tool_images = Vec::new();
    for result in message
        .content()
        .iter()
        .filter(|block| matches!(block.kind(), ContentBlockKind::ToolResult { .. }))
    {
        let ContentBlockKind::ToolResult { tool_call_id, .. } = result.kind() else {
            continue;
        };
        scratch.clear();
        for block in result.tool_result_content().unwrap_or_default() {
            collect_tool_content(block, scratch, &mut pending_tool_images, images)?;
        }
        write_separator(writer, first)?;
        writer.raw(b"{\"content\":")?;
        writer.json(&if scratch.is_empty() {
            "(no output)"
        } else {
            scratch.as_str()
        })?;
        writer.raw(b",\"role\":\"tool\",\"tool_call_id\":")?;
        writer.json(&tool_call_id.as_str())?;
        writer.raw(b"}")?;
    }
    if !pending_tool_images.is_empty() {
        write_separator(writer, first)?;
        writer.raw(
            b"{\"content\":[{\"text\":\"Attached image(s) from tool result:\",\"type\":\"text\"}",
        )?;
        for image in pending_tool_images {
            writer.raw(b",")?;
            write_image_url_part(writer, image.media_type, &image.bytes)?;
        }
        writer.raw(b"],\"role\":\"user\"}")?;
    }
    Ok(())
}

struct PendingWireImage {
    media_type: ImageMediaType,
    bytes: Arc<[u8]>,
}

struct ImageWireState<'a> {
    runtime: &'a AttachmentRuntime,
    offload_before: usize,
    seen: usize,
}

impl<'a> ImageWireState<'a> {
    fn new(runtime: &'a AttachmentRuntime, messages: &[Message]) -> Self {
        let total = messages
            .iter()
            .map(|message| count_images(message.content()))
            .sum::<usize>();
        let retained = retained_request_images(messages).len();
        Self {
            runtime,
            offload_before: total.saturating_sub(retained),
            seen: 0,
        }
    }

    fn next(
        &mut self,
        reference: &ImageAttachmentRef,
        preceded_by_content: bool,
    ) -> Result<ImageOccurrence, RequestBuildError> {
        let offloaded = self.seen < self.offload_before;
        self.seen = self.seen.saturating_add(1);
        if offloaded {
            return Ok(ImageOccurrence::Offloaded(offloaded_image_text(reference)));
        }
        let bytes = self
            .runtime
            .resolve_cached(reference)
            .map_err(|_| RequestBuildError::AttachmentUnavailable)?;
        Ok(ImageOccurrence::Retained {
            handle: image_handle_text(reference, preceded_by_content),
            image: PendingWireImage {
                media_type: reference.media_type(),
                bytes,
            },
        })
    }
}

enum ImageOccurrence {
    Offloaded(String),
    Retained {
        handle: String,
        image: PendingWireImage,
    },
}

fn write_image_occurrence(
    writer: &mut WireWriter<'_, impl Write>,
    first_part: &mut bool,
    images: &mut ImageWireState<'_>,
    reference: &ImageAttachmentRef,
) -> Result<(), RequestBuildError> {
    match images.next(reference, !*first_part)? {
        ImageOccurrence::Offloaded(text) => write_text_part(writer, first_part, &text),
        ImageOccurrence::Retained { handle, image } => {
            write_text_part(writer, first_part, &handle)?;
            write_part_separator(writer, first_part)?;
            write_image_url_part(writer, image.media_type, &image.bytes)
        }
    }
}

fn write_text_part(
    writer: &mut WireWriter<'_, impl Write>,
    first_part: &mut bool,
    text: &str,
) -> Result<(), RequestBuildError> {
    write_part_separator(writer, first_part)?;
    writer.raw(b"{\"text\":")?;
    writer.json(&text)?;
    writer.raw(b",\"type\":\"text\"}")
}

fn write_part_separator(
    writer: &mut WireWriter<'_, impl Write>,
    first_part: &mut bool,
) -> Result<(), RequestBuildError> {
    if *first_part {
        *first_part = false;
        Ok(())
    } else {
        writer.raw(b",")
    }
}

fn write_image_url_part(
    writer: &mut WireWriter<'_, impl Write>,
    media_type: ImageMediaType,
    bytes: &[u8],
) -> Result<(), RequestBuildError> {
    writer.raw(b"{\"image_url\":{\"url\":\"data:")?;
    writer.raw(media_type_name(media_type).as_bytes())?;
    writer.raw(b";base64,")?;
    write_base64(writer, bytes)?;
    writer.raw(b"\"},\"type\":\"image_url\"}")
}

fn collect_tool_content(
    value: &Value,
    text: &mut String,
    pending: &mut Vec<PendingWireImage>,
    images: &mut Option<ImageWireState<'_>>,
) -> Result<(), RequestBuildError> {
    let Some(fields) = value.as_object() else {
        return Ok(());
    };
    match fields.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(value) = fields.get("text").and_then(Value::as_str) {
                push_bounded(text, value)?;
            }
        }
        Some("image") => {
            let reference = fields
                .get("attachment")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
                .ok_or(RequestBuildError::UnsupportedContent)?;
            let image_state = images
                .as_mut()
                .ok_or(RequestBuildError::AttachmentUnavailable)?;
            match image_state.next(&reference, !text.is_empty())? {
                ImageOccurrence::Offloaded(value) => push_bounded(text, &value)?,
                ImageOccurrence::Retained { handle, image } => {
                    push_bounded(text, &handle)?;
                    pending.push(image);
                }
            }
        }
        Some("tool-result") => {
            if let Some(content) = fields.get("content").and_then(Value::as_array) {
                for nested in content {
                    collect_tool_content(nested, text, pending, images)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn push_bounded(output: &mut String, value: &str) -> Result<(), RequestBuildError> {
    if output.len().saturating_add(value.len()) > MAX_PROVIDER_REQUEST_BYTES {
        return Err(RequestBuildError::TooLarge {
            maximum: MAX_PROVIDER_REQUEST_BYTES,
            actual: output.len().saturating_add(value.len()),
        });
    }
    output.push_str(value);
    Ok(())
}

fn count_images(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .map(|block| count_images_in_value(block.raw().as_value()))
        .sum()
}

fn count_images_in_value(value: &Value) -> usize {
    let Some(fields) = value.as_object() else {
        return 0;
    };
    match fields.get("type").and_then(Value::as_str) {
        Some("image") => 1,
        Some("tool-result") => fields
            .get("content")
            .and_then(Value::as_array)
            .map_or(0, |content| content.iter().map(count_images_in_value).sum()),
        _ => 0,
    }
}

fn image_handle_text(reference: &ImageAttachmentRef, preceded_by_content: bool) -> String {
    format!(
        "{}Image {}; request preview {}x{}px.",
        if preceded_by_content { "\n" } else { "" },
        reference.attachment_id(),
        reference.width().get(),
        reference.height().get(),
    )
}

fn offloaded_image_text(reference: &ImageAttachmentRef) -> String {
    format!(
        "[image omitted to fit request image limits; {}. No local normalized image path is available; ask the user to attach it again if needed.]",
        reference.attachment_id()
    )
}

fn media_type_name(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    }
}

fn write_base64(
    writer: &mut WireWriter<'_, impl Write>,
    bytes: &[u8],
) -> Result<(), RequestBuildError> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let encoded = [
            TABLE[usize::from(chunk[0] >> 2)],
            TABLE[usize::from(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4))],
            TABLE[usize::from(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6))],
            TABLE[usize::from(chunk[2] & 0x3f)],
        ];
        writer.raw(&encoded)?;
    }
    match chunks.remainder() {
        [first] => writer.raw(&[
            TABLE[usize::from(first >> 2)],
            TABLE[usize::from((first & 0x03) << 4)],
            b'=',
            b'=',
        ]),
        [first, second] => writer.raw(&[
            TABLE[usize::from(first >> 2)],
            TABLE[usize::from(((first & 0x03) << 4) | (second >> 4))],
            TABLE[usize::from((second & 0x0f) << 2)],
            b'=',
        ]),
        [] => Ok(()),
        _ => Err(RequestBuildError::Encode(
            "invalid base64 remainder".to_owned(),
        )),
    }
}

fn concat_strings_into<'a>(
    output: &mut String,
    values: impl Clone + Iterator<Item = &'a str>,
) -> Result<(), RequestBuildError> {
    let length = values
        .clone()
        .try_fold(0_usize, |total, value| total.checked_add(value.len()))
        .ok_or(RequestBuildError::Capacity)?;
    if length > MAX_PROVIDER_REQUEST_BYTES {
        return Err(RequestBuildError::TooLarge {
            maximum: MAX_PROVIDER_REQUEST_BYTES,
            actual: length,
        });
    }
    output.clear();
    output
        .try_reserve_exact(length)
        .map_err(|_| RequestBuildError::Capacity)?;
    for value in values {
        output.push_str(value);
    }
    Ok(())
}

fn content_has_image_without_allocation(blocks: &[ContentBlock]) -> bool {
    blocks
        .iter()
        .any(|block| raw_value_has_image(block.raw().as_value()))
}

fn raw_value_has_image(value: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    match fields.get("type").and_then(Value::as_str) {
        Some("image") => true,
        Some("tool-result") => fields
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| content.iter().any(raw_value_has_image)),
        _ => false,
    }
}

struct WireWriter<'a, W> {
    output: &'a mut W,
    written: usize,
    stored: usize,
}

impl<'a, W: Write> WireWriter<'a, W> {
    fn new(output: &'a mut W) -> Self {
        Self {
            output,
            written: 0,
            stored: 0,
        }
    }

    fn written(&self) -> usize {
        self.written
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), RequestBuildError> {
        self.write_all(bytes)
            .map_err(|error| RequestBuildError::Encode(error.to_string()))
    }

    fn json(&mut self, value: &impl Serialize) -> Result<(), RequestBuildError> {
        serde_json::to_writer(&mut *self, value)
            .map_err(|error| RequestBuildError::Encode(error.to_string()))
    }
}

impl<W: Write> Write for WireWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .written
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("DeepSeek request byte count overflow"))?;
        let remaining = MAX_PROVIDER_REQUEST_BYTES.saturating_sub(self.stored);
        let retained = remaining.min(bytes.len());
        if retained > 0 {
            self.output.write_all(&bytes[..retained])?;
            self.stored = self
                .stored
                .checked_add(retained)
                .ok_or_else(|| io::Error::other("DeepSeek request storage count overflow"))?;
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
pub(super) fn request_value(
    config: &DeepSeekConfig,
    request: &ProviderRequest,
) -> Result<Value, RequestBuildError> {
    if request.config().provider() != DEEPSEEK_PROVIDER {
        return Err(RequestBuildError::WrongProvider);
    }
    for message in request.messages() {
        if content_has_image(message.content()) {
            return Err(RequestBuildError::UnsupportedContent);
        }
    }

    let mut messages = Vec::new();
    if let Some(system) = request.system() {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in request.messages() {
        serialize_message(message, &mut messages);
    }

    let mut root = Map::new();
    root.insert(
        "model".to_owned(),
        Value::String(request.config().model().to_owned()),
    );
    root.insert("messages".to_owned(), Value::Array(messages));
    root.insert("stream".to_owned(), Value::Bool(true));
    root.insert(
        "stream_options".to_owned(),
        json!({ "include_usage": true }),
    );

    let (thinking, effort) = resolve_thinking(config, request)?;
    root.insert(
        "thinking".to_owned(),
        json!({
            "type": match thinking {
                DeepSeekThinking::Enabled => "enabled",
                DeepSeekThinking::Disabled => "disabled",
            }
        }),
    );
    if let Some(effort) = effort {
        root.insert(
            "reasoning_effort".to_owned(),
            Value::String(match effort {
                DeepSeekReasoningEffort::High => "high".to_owned(),
                DeepSeekReasoningEffort::Max => "max".to_owned(),
                DeepSeekReasoningEffort::Off => {
                    return Err(RequestBuildError::InvalidResolvedThinking);
                }
            }),
        );
    }

    if !request.tools().is_empty() {
        root.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools()
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name(),
                                "description": tool.description(),
                                "parameters": tool.parameters().as_value(),
                            },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(temperature) = request.config().temperature() {
        root.insert("temperature".to_owned(), json!(temperature));
    }
    let max_tokens = request
        .config()
        .max_tokens()
        .ok_or(RequestBuildError::UnpreparedConfig)?
        .get();
    if max_tokens == 0 {
        return Err(RequestBuildError::InvalidMaxTokens);
    }
    root.insert("max_tokens".to_owned(), Value::from(max_tokens));
    if let Some(stop) = request.config().stop() {
        root.insert("stop".to_owned(), json!(stop));
    }
    Ok(Value::Object(root))
}

#[cfg(test)]
fn serialize_message(message: &Message, output: &mut Vec<Value>) {
    match message.role() {
        MessageRole::System => output.push(json!({
            "role": "system",
            "content": flatten_text(message.content()),
        })),
        MessageRole::Assistant => serialize_assistant(message, output),
        MessageRole::User => serialize_user(message, output),
    }
}

#[cfg(test)]
fn serialize_assistant(message: &Message, output: &mut Vec<Value>) {
    let text = flatten_text(message.content());
    let reasoning = message
        .content()
        .iter()
        .filter_map(|block| match block.kind() {
            ContentBlockKind::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    let tool_calls = message
        .content()
        .iter()
        .filter_map(|block| match block.kind() {
            ContentBlockKind::ToolCall {
                id,
                name,
                arguments,
            } => Some(json!({
                "id": id.as_str(),
                "type": "function",
                "function": { "name": name, "arguments": arguments },
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut wire = Map::new();
    wire.insert("role".to_owned(), Value::String("assistant".to_owned()));
    wire.insert("content".to_owned(), Value::String(text));
    if !tool_calls.is_empty() && !reasoning.is_empty() {
        wire.insert("reasoning_content".to_owned(), Value::String(reasoning));
    }
    if !tool_calls.is_empty() {
        wire.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    output.push(Value::Object(wire));
}

#[cfg(test)]
fn serialize_user(message: &Message, output: &mut Vec<Value>) {
    let tool_results = message
        .content()
        .iter()
        .filter(|block| matches!(block.kind(), ContentBlockKind::ToolResult { .. }))
        .collect::<Vec<_>>();
    let text = flatten_text(message.content());
    if !text.is_empty() || tool_results.is_empty() {
        output.push(json!({ "role": "user", "content": text }));
    }
    for result in tool_results {
        let ContentBlockKind::ToolResult { tool_call_id, .. } = result.kind() else {
            continue;
        };
        let content = flatten_raw_text(result.tool_result_content().unwrap_or_default());
        output.push(json!({
            "role": "tool",
            "tool_call_id": tool_call_id.as_str(),
            "content": if content.is_empty() { "(no output)" } else { &content },
        }));
    }
}

#[cfg(test)]
fn flatten_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block.kind() {
            ContentBlockKind::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
fn flatten_raw_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(|block| {
            let fields = block.as_object()?;
            (fields.get("type")?.as_str()? == "text").then(|| fields.get("text")?.as_str())?
        })
        .collect()
}

#[cfg(test)]
fn content_has_image(blocks: &[ContentBlock]) -> bool {
    let mut pending = blocks
        .iter()
        .map(|block| block.raw().as_value())
        .collect::<Vec<_>>();
    while let Some(value) = pending.pop() {
        let Some(fields) = value.as_object() else {
            continue;
        };
        match fields.get("type").and_then(Value::as_str) {
            Some("image") => return true,
            Some("tool-result") => {
                if let Some(content) = fields.get("content").and_then(Value::as_array) {
                    pending.extend(content);
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
fn resolve_thinking(
    config: &DeepSeekConfig,
    request: &ProviderRequest,
) -> Result<(DeepSeekThinking, Option<DeepSeekReasoningEffort>), RequestBuildError> {
    resolve_thinking_parts(config, request.config(), request.purpose())
}

fn resolve_thinking_parts(
    config: &DeepSeekConfig,
    call_config: &crate::model::LlmCallConfig,
    purpose: RequestPurpose,
) -> Result<(DeepSeekThinking, Option<DeepSeekReasoningEffort>), RequestBuildError> {
    if purpose == RequestPurpose::SessionTitle {
        return Ok((DeepSeekThinking::Disabled, None));
    }
    let effort = match call_config.reasoning_effort() {
        None => return Err(RequestBuildError::UnpreparedConfig),
        Some(value) => match value.as_str() {
            "off" => DeepSeekReasoningEffort::Off,
            "high" => DeepSeekReasoningEffort::High,
            "max" => DeepSeekReasoningEffort::Max,
            other => {
                return Err(RequestBuildError::UnsupportedReasoningEffort {
                    value: other.chars().take(64).collect(),
                });
            }
        },
    };
    if config.thinking() == Some(DeepSeekThinking::Disabled)
        && effort != DeepSeekReasoningEffort::Off
    {
        return Err(RequestBuildError::ThinkingDisabledWithEffort);
    }
    match effort {
        DeepSeekReasoningEffort::Off => Ok((DeepSeekThinking::Disabled, None)),
        DeepSeekReasoningEffort::High | DeepSeekReasoningEffort::Max => {
            Ok((DeepSeekThinking::Enabled, Some(effort)))
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(super) enum RequestBuildError {
    #[error("request was routed to a provider other than DeepSeek")]
    WrongProvider,
    #[error("DeepSeek chat completions do not support image content")]
    UnsupportedContent,
    #[error("DeepSeek image attachment bytes are unavailable")]
    AttachmentUnavailable,
    #[error("DeepSeek does not support reasoning effort {value:?}")]
    UnsupportedReasoningEffort { value: String },
    #[error("DeepSeek thinking is disabled for this deployment")]
    ThinkingDisabledWithEffort,
    #[error("DeepSeek max tokens must be positive")]
    InvalidMaxTokens,
    #[error("DeepSeek call config was not prepared before dispatch")]
    UnpreparedConfig,
    #[error("resolved DeepSeek thinking state is inconsistent")]
    InvalidResolvedThinking,
    #[error("DeepSeek request is {actual} bytes; maximum is {maximum}")]
    TooLarge { maximum: usize, actual: usize },
    #[error("failed to reserve bounded DeepSeek request capacity")]
    Capacity,
    #[error("DeepSeek request changed after its wire preflight")]
    PreflightMismatch,
    #[error("failed to serialize DeepSeek request: {0}")]
    Encode(String),
}

impl RequestBuildError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedContent => "UNSUPPORTED_CONTENT",
            Self::AttachmentUnavailable => "ATTACHMENT_UNAVAILABLE",
            Self::UnsupportedReasoningEffort { .. } | Self::ThinkingDisabledWithEffort => {
                "UNSUPPORTED_REASONING_EFFORT"
            }
            Self::TooLarge { .. } => "REQUEST_TOO_LARGE",
            Self::WrongProvider
            | Self::InvalidMaxTokens
            | Self::UnpreparedConfig
            | Self::InvalidResolvedThinking
            | Self::Capacity
            | Self::PreflightMismatch
            | Self::Encode(_) => "INVALID_REQUEST",
        }
    }
}
