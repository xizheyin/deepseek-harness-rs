use serde_json::Value;

pub(super) const MAX_LSP_HEADER_BYTES: usize = 64 * 1024;
pub(super) const MAX_LSP_MESSAGE_BYTES: usize = 16_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FramingError {
    Capacity,
    Header,
    MessageTooLarge,
    Json,
}

pub(super) fn encode_message(value: &Value) -> Result<Vec<u8>, FramingError> {
    let body = serde_json::to_vec(value).map_err(|_| FramingError::Json)?;
    if body.len() > MAX_LSP_MESSAGE_BYTES {
        return Err(FramingError::MessageTooLarge);
    }
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(header.len().saturating_add(body.len()))
        .map_err(|_| FramingError::Capacity)?;
    encoded.extend_from_slice(header.as_bytes());
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

#[derive(Default)]
pub(super) struct MessageDecoder {
    buffer: Vec<u8>,
}

impl MessageDecoder {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, FramingError> {
        let maximum = MAX_LSP_HEADER_BYTES
            .saturating_add(4)
            .saturating_add(MAX_LSP_MESSAGE_BYTES);
        if self.buffer.len().saturating_add(bytes.len()) > maximum {
            return Err(FramingError::MessageTooLarge);
        }
        self.buffer
            .try_reserve_exact(bytes.len())
            .map_err(|_| FramingError::Capacity)?;
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        loop {
            let Some(separator) = find_separator(&self.buffer) else {
                if self.buffer.len() > MAX_LSP_HEADER_BYTES {
                    return Err(FramingError::Header);
                }
                break;
            };
            if separator > MAX_LSP_HEADER_BYTES {
                return Err(FramingError::Header);
            }
            let content_length = parse_content_length(&self.buffer[..separator])?;
            if content_length > MAX_LSP_MESSAGE_BYTES {
                return Err(FramingError::MessageTooLarge);
            }
            let body_start = separator.saturating_add(4);
            let body_end = body_start
                .checked_add(content_length)
                .ok_or(FramingError::MessageTooLarge)?;
            if self.buffer.len() < body_end {
                break;
            }
            let value = serde_json::from_slice(&self.buffer[body_start..body_end])
                .map_err(|_| FramingError::Json)?;
            messages.push(value);
            self.buffer.drain(..body_end);
        }
        Ok(messages)
    }
}

fn find_separator(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(header: &[u8]) -> Result<usize, FramingError> {
    let header = std::str::from_utf8(header).map_err(|_| FramingError::Header)?;
    let mut found = None;
    for line in header.split("\r\n") {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if found.is_some() || value.trim().is_empty() {
                return Err(FramingError::Header);
            }
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| FramingError::Header)?;
            found = Some(length);
        }
    }
    found.ok_or(FramingError::Header)
}

#[cfg(test)]
mod tests {
    use super::{FramingError, MAX_LSP_HEADER_BYTES, MessageDecoder, encode_message};

    #[test]
    fn framing_uses_utf8_byte_length_and_survives_every_split() {
        let value = serde_json::json!({"jsonrpc":"2.0","id":1,"result":"é"});
        let encoded = encode_message(&value).unwrap();
        let body = serde_json::to_vec(&value).unwrap();
        assert!(encoded.starts_with(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes()));
        for split in 0..encoded.len() {
            let mut decoder = MessageDecoder::default();
            assert!(decoder.push(&encoded[..split]).unwrap().is_empty());
            assert_eq!(decoder.push(&encoded[split..]).unwrap(), [value.clone()]);
        }
    }

    #[test]
    fn framing_accepts_multiple_and_case_insensitive_headers() {
        let first = encode_message(&serde_json::json!({"a":1})).unwrap();
        let second = b"content-length: 7\r\nContent-Type: x\r\n\r\n{\"b\":2}";
        let mut bytes = first;
        bytes.extend_from_slice(second);
        let mut decoder = MessageDecoder::default();
        assert_eq!(
            decoder.push(&bytes).unwrap(),
            [serde_json::json!({"a":1}), serde_json::json!({"b":2})]
        );
    }

    #[test]
    fn framing_rejects_missing_duplicate_invalid_and_overlong_headers() {
        for value in [
            b"X: 1\r\n\r\n{}".as_slice(),
            b"Content-Length: x\r\n\r\n{}".as_slice(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        ] {
            let mut decoder = MessageDecoder::default();
            assert_eq!(decoder.push(value), Err(FramingError::Header));
        }
        let mut decoder = MessageDecoder::default();
        assert_eq!(
            decoder.push(&vec![b'x'; MAX_LSP_HEADER_BYTES + 1]),
            Err(FramingError::Header)
        );
    }
}
