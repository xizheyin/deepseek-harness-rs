use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LspOperation {
    GoToDefinition,
    FindReferences,
    GoToImplementation,
    Hover,
}

impl LspOperation {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "goToDefinition" => Some(Self::GoToDefinition),
            "findReferences" => Some(Self::FindReferences),
            "goToImplementation" => Some(Self::GoToImplementation),
            "hover" => Some(Self::Hover),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::GoToDefinition => "goToDefinition",
            Self::FindReferences => "findReferences",
            Self::GoToImplementation => "goToImplementation",
            Self::Hover => "hover",
        }
    }

    pub(super) fn method(self) -> &'static str {
        match self {
            Self::GoToDefinition => "textDocument/definition",
            Self::FindReferences => "textDocument/references",
            Self::GoToImplementation => "textDocument/implementation",
            Self::Hover => "textDocument/hover",
        }
    }

    fn capability(self) -> &'static str {
        match self {
            Self::GoToDefinition => "definitionProvider",
            Self::FindReferences => "referencesProvider",
            Self::GoToImplementation => "implementationProvider",
            Self::Hover => "hoverProvider",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LspPosition {
    pub(super) line: u32,
    pub(super) character: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LspRange {
    pub(super) start: LspPosition,
    pub(super) end: LspPosition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LspLocation {
    pub(super) uri: String,
    pub(super) range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LspHover {
    pub(super) contents: String,
    pub(super) range: Option<LspRange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum LspResult {
    Locations(Vec<LspLocation>),
    Hover(Option<LspHover>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProtocolError {
    UnsupportedEncoding,
    UnsupportedOperation,
    UnsupportedSynchronization,
    MalformedResponse,
}

pub(super) fn validate_capabilities(
    initialize_result: &Value,
    operation: LspOperation,
) -> Result<(), ProtocolError> {
    let capabilities = initialize_result
        .as_object()
        .and_then(|fields| fields.get("capabilities"))
        .and_then(Value::as_object)
        .ok_or(ProtocolError::MalformedResponse)?;
    match capabilities.get("positionEncoding") {
        None => {}
        Some(Value::String(value)) if value == "utf-16" => {}
        Some(Value::String(_)) => return Err(ProtocolError::UnsupportedEncoding),
        Some(_) => return Err(ProtocolError::MalformedResponse),
    }
    let supported = match capabilities.get(operation.capability()) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Object(_)) => true,
        None | Some(Value::Null) => false,
        Some(_) => return Err(ProtocolError::MalformedResponse),
    };
    if !supported {
        return Err(ProtocolError::UnsupportedOperation);
    }
    let sync = capabilities
        .get("textDocumentSync")
        .ok_or(ProtocolError::UnsupportedSynchronization)?;
    let open_close = match sync {
        Value::Number(value) => matches!(value.as_u64(), Some(1 | 2)),
        Value::Object(fields) => fields.get("openClose").and_then(Value::as_bool) == Some(true),
        _ => false,
    };
    if !open_close {
        return Err(ProtocolError::UnsupportedSynchronization);
    }
    Ok(())
}

pub(super) fn normalize_result(
    operation: LspOperation,
    payload: &Value,
) -> Result<LspResult, ProtocolError> {
    if operation == LspOperation::Hover {
        normalize_hover(payload).map(LspResult::Hover)
    } else {
        normalize_locations(payload).map(LspResult::Locations)
    }
}

fn normalize_locations(payload: &Value) -> Result<Vec<LspLocation>, ProtocolError> {
    if payload.is_null() {
        return Ok(Vec::new());
    }
    let elements = payload
        .as_array()
        .map_or_else(|| vec![payload], |values| values.iter().collect());
    let mut locations = Vec::new();
    locations
        .try_reserve_exact(elements.len())
        .map_err(|_| ProtocolError::MalformedResponse)?;
    for value in elements {
        let fields = value.as_object().ok_or(ProtocolError::MalformedResponse)?;
        let (uri, range) = if let (Some(uri), Some(range)) = (
            fields.get("targetUri").and_then(Value::as_str),
            fields.get("targetSelectionRange"),
        ) {
            (uri, range)
        } else if let (Some(uri), Some(range)) = (
            fields.get("uri").and_then(Value::as_str),
            fields.get("range"),
        ) {
            (uri, range)
        } else {
            return Err(ProtocolError::MalformedResponse);
        };
        if uri.len() > 16 * 1024 || uri.contains('\0') {
            return Err(ProtocolError::MalformedResponse);
        }
        locations.push(LspLocation {
            uri: uri.to_owned(),
            range: parse_range(range)?,
        });
    }
    Ok(locations)
}

fn normalize_hover(payload: &Value) -> Result<Option<LspHover>, ProtocolError> {
    if payload.is_null() {
        return Ok(None);
    }
    let fields = payload
        .as_object()
        .ok_or(ProtocolError::MalformedResponse)?;
    let contents = fields
        .get("contents")
        .ok_or(ProtocolError::MalformedResponse)
        .and_then(render_hover_contents)?;
    if contents.is_empty() {
        return Ok(None);
    }
    let range = fields.get("range").map(parse_range).transpose()?;
    Ok(Some(LspHover { contents, range }))
}

fn render_hover_contents(value: &Value) -> Result<String, ProtocolError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Array(values) => {
            let mut rendered = Vec::new();
            rendered
                .try_reserve_exact(values.len())
                .map_err(|_| ProtocolError::MalformedResponse)?;
            for value in values {
                rendered.push(render_marked_string(value)?);
            }
            Ok(rendered.join("\n\n"))
        }
        Value::Object(fields)
            if matches!(
                fields.get("kind").and_then(Value::as_str),
                Some("markdown" | "plaintext")
            ) =>
        {
            fields
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(ProtocolError::MalformedResponse)
        }
        Value::Object(_) => render_marked_string(value),
        _ => Err(ProtocolError::MalformedResponse),
    }
}

fn render_marked_string(value: &Value) -> Result<String, ProtocolError> {
    if let Some(value) = value.as_str() {
        return Ok(value.to_owned());
    }
    let fields = value.as_object().ok_or(ProtocolError::MalformedResponse)?;
    let language = fields
        .get("language")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MalformedResponse)?;
    let value = fields
        .get("value")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MalformedResponse)?;
    Ok(format!("```{language}\n{value}\n```"))
}

fn parse_range(value: &Value) -> Result<LspRange, ProtocolError> {
    let fields = value.as_object().ok_or(ProtocolError::MalformedResponse)?;
    Ok(LspRange {
        start: parse_position(
            fields
                .get("start")
                .ok_or(ProtocolError::MalformedResponse)?,
        )?,
        end: parse_position(fields.get("end").ok_or(ProtocolError::MalformedResponse)?)?,
    })
}

fn parse_position(value: &Value) -> Result<LspPosition, ProtocolError> {
    let fields = value.as_object().ok_or(ProtocolError::MalformedResponse)?;
    let line = fields
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ProtocolError::MalformedResponse)?;
    let character = fields
        .get("character")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(ProtocolError::MalformedResponse)?;
    Ok(LspPosition { line, character })
}

#[cfg(test)]
mod tests {
    use super::{
        LspHover, LspLocation, LspOperation, LspPosition, LspRange, LspResult, ProtocolError,
        normalize_result, validate_capabilities,
    };

    #[test]
    fn operations_map_to_the_fixed_methods() {
        for (name, method) in [
            ("goToDefinition", "textDocument/definition"),
            ("findReferences", "textDocument/references"),
            ("goToImplementation", "textDocument/implementation"),
            ("hover", "textDocument/hover"),
        ] {
            let operation = LspOperation::parse(name).unwrap();
            assert_eq!(operation.as_str(), name);
            assert_eq!(operation.method(), method);
        }
        assert!(LspOperation::parse("rename").is_none());
    }

    #[test]
    fn capabilities_require_utf16_operation_and_transient_open() {
        let valid = serde_json::json!({"capabilities":{
            "positionEncoding":"utf-16",
            "definitionProvider":{},
            "textDocumentSync":{"openClose":true,"change":2}
        }});
        assert_eq!(
            validate_capabilities(&valid, LspOperation::GoToDefinition),
            Ok(())
        );
        let mut invalid = valid.clone();
        invalid["capabilities"]["positionEncoding"] = serde_json::json!("utf-8");
        assert_eq!(
            validate_capabilities(&invalid, LspOperation::GoToDefinition),
            Err(ProtocolError::UnsupportedEncoding)
        );
        assert_eq!(
            validate_capabilities(&valid, LspOperation::Hover),
            Err(ProtocolError::UnsupportedOperation)
        );
    }

    #[test]
    fn navigation_normalizes_locations_and_links() {
        let range =
            serde_json::json!({"start":{"line":1,"character":2},"end":{"line":1,"character":5}});
        let result = normalize_result(
            LspOperation::FindReferences,
            &serde_json::json!([
                {"uri":"file:///a","range":range},
                {"targetUri":"file:///b","targetSelectionRange":range,"targetRange":range}
            ]),
        )
        .unwrap();
        assert_eq!(
            result,
            LspResult::Locations(vec![
                LspLocation {
                    uri: "file:///a".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 2
                        },
                        end: LspPosition {
                            line: 1,
                            character: 5
                        }
                    }
                },
                LspLocation {
                    uri: "file:///b".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 2
                        },
                        end: LspPosition {
                            line: 1,
                            character: 5
                        }
                    }
                },
            ])
        );
        assert_eq!(
            normalize_result(LspOperation::GoToDefinition, &serde_json::Value::Null),
            Ok(LspResult::Locations(Vec::new()))
        );
    }

    #[test]
    fn hover_normalizes_all_fixed_content_forms_and_rejects_bad_ranges() {
        assert_eq!(
            normalize_result(
                LspOperation::Hover,
                &serde_json::json!({"contents":["plain",{"language":"rs","value":"fn x()"}]})
            ),
            Ok(LspResult::Hover(Some(LspHover {
                contents: "plain\n\n```rs\nfn x()\n```".to_owned(),
                range: None,
            })))
        );
        assert_eq!(
            normalize_result(LspOperation::Hover, &serde_json::Value::Null),
            Ok(LspResult::Hover(None))
        );
        assert_eq!(
            normalize_result(
                LspOperation::Hover,
                &serde_json::json!({"contents":"x","range":{"start":{"line":-1,"character":0}}})
            ),
            Err(ProtocolError::MalformedResponse)
        );
    }
}
