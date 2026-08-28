//! Deterministic stdio language server used by Phase 37 offline acceptance.

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use serde_json::{Value, json};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 16_000_000;

fn main() -> io::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "normal".to_owned());
    let marker = std::env::args().nth(2).map(PathBuf::from);
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut facts = Vec::new();
    let initialize = read_message(&mut input)?;
    let initialize_id = request_id(&initialize)?;
    facts.push("initialize".to_owned());

    write_message(
        &mut output,
        &json!({
            "jsonrpc":"2.0",
            "id":"fixture-configuration",
            "method":"workspace/configuration",
            "params":{"items":[{"section":"fixture"}]}
        }),
    )?;
    let configuration = read_message(&mut input)?;
    if configuration["id"] != "fixture-configuration"
        || configuration["result"] != json!([{"fixture":"configured"}])
    {
        return Err(invalid_data("host configuration response was invalid"));
    }
    facts.push("configuration".to_owned());

    let mut capabilities = json!({
        "positionEncoding":"utf-16",
        "textDocumentSync":{"openClose":true,"change":1},
        "definitionProvider":true,
        "referencesProvider":true,
        "implementationProvider":true,
        "hoverProvider":true
    });
    if mode == "unsupported" {
        capabilities["definitionProvider"] = Value::Bool(false);
    }
    write_message(
        &mut output,
        &json!({"jsonrpc":"2.0","id":initialize_id,"result":{"capabilities":capabilities}}),
    )?;

    let initialized = read_message(&mut input)?;
    if initialized["method"] != "initialized" {
        return Err(invalid_data("initialized notification was missing"));
    }
    facts.push("initialized".to_owned());
    let mut opened_uri = None;

    loop {
        let message = read_message(&mut input)?;
        match message.get("method").and_then(Value::as_str) {
            Some("textDocument/didOpen") => {
                let document = &message["params"]["textDocument"];
                let uri = document["uri"]
                    .as_str()
                    .ok_or_else(|| invalid_data("didOpen URI was missing"))?;
                if document["languageId"] != "rust"
                    || !document["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("fn target"))
                {
                    return Err(invalid_data("didOpen source was invalid"));
                }
                opened_uri = Some(uri.to_owned());
                facts.push("didOpen".to_owned());
            }
            Some(
                method @ ("textDocument/definition"
                | "textDocument/references"
                | "textDocument/implementation"
                | "textDocument/hover"),
            ) => {
                let id = request_id(&message)?;
                let uri = opened_uri
                    .as_deref()
                    .ok_or_else(|| invalid_data("query arrived before didOpen"))?;
                if message["params"]["position"] != json!({"line":2,"character":4}) {
                    return Err(invalid_data("query coordinate was not converted"));
                }
                if method == "textDocument/references"
                    && message["params"]["context"]["includeDeclaration"] != true
                {
                    return Err(invalid_data("references omitted the declaration"));
                }
                facts.push(method.to_owned());
                if mode == "stall-query" {
                    let child = Command::new("/bin/sleep").arg("30").spawn()?;
                    append_marker(marker.as_deref(), &format!("child={}\n", child.id()))?;
                    let cancel = read_message(&mut input)?;
                    append_marker(marker.as_deref(), &format!("cancel={}\n", cancel["method"]))?;
                    std::thread::sleep(Duration::from_secs(30));
                    return Ok(());
                }
                if mode == "crash-once" {
                    let crash_marker = marker
                        .as_ref()
                        .map(|path| path.with_extension("first-crash"))
                        .ok_or_else(|| invalid_data("crash-once requires a marker"))?;
                    if !crash_marker.exists() {
                        fs::write(crash_marker, b"crashed\n")?;
                        std::process::exit(17);
                    }
                }
                let result = match (mode.as_str(), method) {
                    ("malformed", _) => json!({"uri":uri,"range":{"start":{"line":-1}}}),
                    (_, "textDocument/definition") => json!({
                        "targetUri":uri,
                        "targetRange":{"start":{"line":4,"character":2},"end":{"line":4,"character":8}},
                        "targetSelectionRange":{"start":{"line":4,"character":2},"end":{"line":4,"character":8}}
                    }),
                    (_, "textDocument/references") => json!([
                        {"uri":uri,"range":{"start":{"line":4,"character":2},"end":{"line":4,"character":8}}},
                        {"uri":uri,"range":{"start":{"line":8,"character":1},"end":{"line":8,"character":7}}}
                    ]),
                    (_, "textDocument/implementation") => Value::Null,
                    (_, "textDocument/hover") => json!({
                        "contents":{"kind":"markdown","value":"`fn target() -> usize`"},
                        "range":{"start":{"line":2,"character":3},"end":{"line":2,"character":9}}
                    }),
                    _ => return Err(invalid_data("unexpected fixture mode")),
                };
                write_message(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":id,"result":result}),
                )?;
            }
            Some("textDocument/didClose") => {
                facts.push("didClose".to_owned());
                opened_uri = None;
            }
            Some("shutdown") => {
                facts.push("shutdown".to_owned());
                write_message(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":request_id(&message)?,"result":null}),
                )?;
            }
            Some("exit") => {
                facts.push("exit".to_owned());
                write_marker(marker.as_deref(), &facts.join("\n"))?;
                return Ok(());
            }
            Some(other) => {
                if let Some(id) = message.get("id") {
                    write_message(
                        &mut output,
                        &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unsupported {other}")}}),
                    )?;
                }
            }
            None => {}
        }
    }
}

fn request_id(message: &Value) -> io::Result<Value> {
    message
        .get("id")
        .cloned()
        .ok_or_else(|| invalid_data("request id was missing"))
}

fn read_message(input: &mut impl BufRead) -> io::Result<Value> {
    let mut content_length = None;
    let mut header_bytes = 0_usize;
    loop {
        let mut line = String::new();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return Err(invalid_data("protocol stream ended"));
        }
        header_bytes = header_bytes.saturating_add(read);
        if header_bytes > MAX_HEADER_BYTES {
            return Err(invalid_data("protocol header exceeded its limit"));
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line
            .strip_suffix("\r\n")
            .and_then(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim())
        {
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| invalid_data("invalid Content-Length"))?,
            );
        }
    }
    let length = content_length.ok_or_else(|| invalid_data("Content-Length was missing"))?;
    if length > MAX_BODY_BYTES {
        return Err(invalid_data("protocol body exceeded its limit"));
    }
    let mut body = vec![0_u8; length];
    input.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|_| invalid_data("protocol body was invalid JSON"))
}

fn write_message(output: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(|_| invalid_data("response could not encode"))?;
    write!(output, "Content-Length: {}\r\n\r\n", body.len())?;
    output.write_all(&body)?;
    output.flush()
}

fn write_marker(path: Option<&Path>, text: &str) -> io::Result<()> {
    if let Some(path) = path {
        fs::write(path, format!("{text}\n"))?;
    }
    Ok(())
}

fn append_marker(path: Option<&Path>, text: &str) -> io::Result<()> {
    if let Some(path) = path {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(text.as_bytes())?;
    }
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
