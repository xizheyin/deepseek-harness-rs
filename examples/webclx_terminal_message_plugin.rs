mod plugin_support;

use std::{path::PathBuf, time::Duration};

use futures_util::StreamExt as _;
use plugin_support::{PluginError, ToolDeclaration};
use reqwest::{Client, Url};
use serde_json::{Map, Value, json};

const DEFAULT_URL: &str = "http://127.0.0.1:11111";
const TOKEN_HEADER: &str = "X-WebClx-Local-Token";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
struct Config {
    base_url: Url,
    reply_url: Option<Url>,
    sender: Option<String>,
    token_file: Option<PathBuf>,
}

#[derive(Clone)]
struct WebClxClient {
    config: Config,
    http: Client,
}

fn main() -> std::io::Result<()> {
    let config = parse_config(std::env::args().skip(1)).map_err(std::io::Error::other)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = WebClxClient::new(config).map_err(std::io::Error::other)?;
    plugin_support::run(
        "webclx-terminal-message",
        declarations(),
        move |tool, arguments| match tool {
            "webclx_list_terminals" => runtime.block_on(client.list_terminals(arguments)),
            "webclx_send_terminal_message" => runtime.block_on(client.send_message(arguments)),
            _ => Err(error("UNKNOWN_TOOL", "tool is not declared by this plugin")),
        },
    )
}

fn declarations() -> Vec<ToolDeclaration> {
    let terminal = json!({
        "type":"object",
        "properties":{
            "id":{"type":"string"}, "name":{"type":"string"},
            "path":{"type":"string"}, "display_path":{"type":"string"},
            "alive":{"type":"boolean"}, "busy":{"type":"boolean"},
            "activity_state":{"type":"string"}, "activity_agent":{"type":"string"}
        },
        "required":["id","name","path","display_path","alive","busy","activity_state","activity_agent"],
        "additionalProperties":false
    });
    vec![
        ToolDeclaration {
            name: "webclx_list_terminals",
            description: "List live webClx terminals with optional agent and workspace filters so an exact cross-terminal target can be selected",
            parameters: json!({
                "type":"object",
                "properties":{
                    "agent":{"type":"string","enum":["codex","claude","deepseek"]},
                    "path":{"type":"string"}, "alive_only":{"type":"boolean"}
                },
                "additionalProperties":false
            }),
            output: json!({"type":"array","items":terminal}),
        },
        ToolDeclaration {
            name: "webclx_send_terminal_message",
            description: "Send a tagged message to one exact webClx terminal and optionally include a native return route",
            parameters: json!({
                "type":"object",
                "properties":{
                    "target":{"type":"string"}, "message":{"type":"string"},
                    "request_reply":{"type":"boolean"}, "wait_ready":{"type":"boolean"},
                    "wait_ready_timeout_seconds":{"type":"integer"}
                },
                "required":["target","message"],
                "additionalProperties":false
            }),
            output: json!({
                "type":"object",
                "properties":{
                    "target_session_id":{"type":"string"}, "terminal_name":{"type":"string"},
                    "accepted":{"type":"boolean"}, "submitted":{"type":"boolean"},
                    "verification":{"type":"string","enum":["confirmed","not-supported"]}
                },
                "required":["target_session_id","terminal_name","accepted","submitted","verification"],
                "additionalProperties":false
            }),
        },
    ]
}

impl WebClxClient {
    fn new(config: Config) -> Result<Self, String> {
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|failure| format!("could not construct HTTP client: {failure}"))?;
        Ok(Self { config, http })
    }

    async fn list_terminals(&self, arguments: &Value) -> Result<Value, PluginError> {
        let fields = object(arguments)?;
        let agent = optional_string(fields, "agent")?.map(str::to_ascii_lowercase);
        let path = optional_string(fields, "path")?.unwrap_or_default();
        let alive_only = optional_bool(fields, "alive_only")?.unwrap_or(true);
        let sessions = self.all_sessions().await?;
        Ok(Value::Array(
            sessions
                .into_iter()
                .filter(|session| !alive_only || bool_field(session, "alive"))
                .filter(|session| matches_path(session, path))
                .filter(|session| {
                    agent.as_deref().is_none_or(|expected| {
                        string_field(session, "activity_agent").eq_ignore_ascii_case(expected)
                    })
                })
                .map(public_session)
                .collect(),
        ))
    }

    async fn send_message(&self, arguments: &Value) -> Result<Value, PluginError> {
        let fields = object(arguments)?;
        let target = normalize(required_string(fields, "target")?, "target")?;
        let mut message = normalize(required_string(fields, "message")?, "message")?;
        let request_reply = optional_bool(fields, "request_reply")?.unwrap_or(false);
        let wait_ready = optional_bool(fields, "wait_ready")?.unwrap_or(false);
        let wait_seconds = optional_u64(fields, "wait_ready_timeout_seconds")?
            .unwrap_or(120)
            .min(600);
        let sender = self
            .config
            .sender
            .as_deref()
            .ok_or_else(|| error("SENDER_REQUIRED", "configure --sender before sending"))?;
        let sessions = self.all_sessions().await?;
        let selected = exact_target(&sessions, &target)?;
        let _ = exact_target(&sessions, sender)?;
        let target_id = string_field(selected, "id").to_owned();
        let terminal_name = string_field(selected, "name").to_owned();
        let target_agent = string_field(selected, "activity_agent").to_owned();
        if wait_ready {
            self.wait_ready(&target_id, Duration::from_secs(wait_seconds))
                .await?;
        }
        if request_reply {
            let reply_url = self
                .config
                .reply_url
                .as_ref()
                .unwrap_or(&self.config.base_url);
            if !loopback(&self.config.base_url) && loopback(reply_url) {
                return Err(error(
                    "INVALID_REPLY_URL",
                    "a remote destination cannot reply to a loopback URL",
                ));
            }
            message.push_str(&format!(
                "; reply with the native webclx_send_terminal_message tool to {sender} through {}; do not answer only in your own terminal.",
                reply_url.as_str().trim_end_matches('/')
            ));
        }
        let data = format!("[from {sender}] {message}");
        let verify = target_agent.eq_ignore_ascii_case("codex")
            || target_agent.eq_ignore_ascii_case("claude");
        let delivery_id = if verify { data.clone() } else { String::new() };
        let response = self
            .request(
                reqwest::Method::POST,
                "/api/terminal/sessions/message",
                Some(json!({
                    "target":target_id, "data":data, "submit":true,
                    "submit_enters":1, "bracketed_paste":true,
                    "verify_submission":verify,
                    "delivery_id":delivery_id
                })),
            )
            .await?;
        let accepted = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if !accepted {
            return Err(error(
                "DELIVERY_REJECTED",
                "webClx did not accept the terminal message",
            ));
        }
        let submitted = response
            .get("submitted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if verify && !submitted {
            return Err(error(
                "SUBMISSION_UNCONFIRMED",
                "terminal message submission was not confirmed",
            ));
        }
        Ok(json!({
            "target_session_id":target_id, "terminal_name":terminal_name,
            "accepted":accepted,
            "submitted":submitted,
            "verification":if verify { "confirmed" } else { "not-supported" }
        }))
    }

    async fn wait_ready(&self, id: &str, timeout: Duration) -> Result<(), PluginError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let sessions = self.all_sessions().await?;
            let current = sessions
                .iter()
                .find(|session| string_field(session, "id") == id)
                .ok_or_else(|| error("TERMINAL_GONE", "terminal disappeared"))?;
            if !bool_field(current, "busy")
                && !string_field(current, "activity_state").eq_ignore_ascii_case("agent")
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(error("TERMINAL_BUSY", "terminal is still busy"));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn all_sessions(&self) -> Result<Vec<Value>, PluginError> {
        self.request(
            reqwest::Method::GET,
            "/api/terminal/sessions?all=true",
            None,
        )
        .await?
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| error("INVALID_RESPONSE", "webClx sessions response is invalid"))
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Value, PluginError> {
        let url = self
            .config
            .base_url
            .join(path)
            .map_err(|_| error("INVALID_URL", "webClx request URL is invalid"))?;
        let mut request = self.http.request(method, url);
        if loopback(&self.config.base_url) {
            if let Some(path) = &self.config.token_file {
                let token = std::fs::read_to_string(path)
                    .map_err(|_| error("TOKEN_READ_FAILED", "could not read local token file"))?;
                let token = token.trim();
                if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(error(
                        "INVALID_TOKEN",
                        "local token file must contain a 64-character hexadecimal token",
                    ));
                }
                request = request.header(TOKEN_HEADER, token);
            }
        }
        if let Some(payload) = payload {
            let body = serde_json::to_vec(&payload)
                .map_err(|_| error("INVALID_REQUEST", "could not encode request"))?;
            request = request
                .header("Content-Type", "application/json")
                .body(body);
        }
        let response = request.send().await.map_err(|failure| {
            error(
                "REQUEST_FAILED",
                format!("webClx request failed: {failure}"),
            )
        })?;
        let status = response.status();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(error("RESPONSE_TOO_LARGE", "webClx response exceeds 1 MiB"));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|failure| {
                error(
                    "REQUEST_FAILED",
                    format!("webClx response failed: {failure}"),
                )
            })?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(error("RESPONSE_TOO_LARGE", "webClx response exceeds 1 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(error(
                "HTTP_ERROR",
                format!("webClx returned HTTP {status}"),
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| error("INVALID_RESPONSE", "webClx returned invalid JSON"))
    }
}

fn parse_config(arguments: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        base_url: base_url(DEFAULT_URL, "--base-url")?,
        reply_url: None,
        sender: None,
        token_file: None,
    };
    let mut arguments = arguments;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--base-url" => config.base_url = base_url(&value, "--base-url")?,
            "--reply-url" => config.reply_url = Some(base_url(&value, "--reply-url")?),
            "--sender" => {
                config.sender =
                    Some(normalize(&value, "sender").map_err(|failure| failure.message)?)
            }
            "--local-token-file" => config.token_file = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown argument: {flag}")),
        }
    }
    Ok(config)
}

fn base_url(value: &str, label: &str) -> Result<Url, String> {
    let mut url = Url::parse(value).map_err(|_| format!("{label} must be an HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{label} must be an HTTP(S) origin without credentials, query, or fragment"
        ));
    }
    url.set_path("/");
    Ok(url)
}

fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
    })
}

fn exact_target<'a>(sessions: &'a [Value], target: &str) -> Result<&'a Value, PluginError> {
    let matches = sessions
        .iter()
        .filter(|session| {
            string_field(session, "id") == target || string_field(session, "name") == target
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [selected] => Ok(*selected),
        [] => Err(error(
            "TARGET_NOT_FOUND",
            "no exact terminal target was found",
        )),
        _ => Err(error(
            "TARGET_AMBIGUOUS",
            "multiple exact terminal targets were found",
        )),
    }
}

fn matches_path(session: &Value, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let expected = clean_path(path);
    let actual = clean_path(string_field(session, "path"));
    let display = clean_path(string_field(session, "display_path"));
    actual == expected || display == expected || display.ends_with(&format!("/{expected}"))
}

fn clean_path(value: &str) -> String {
    value
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn public_session(session: Value) -> Value {
    json!({
        "id":string_field(&session,"id"), "name":string_field(&session,"name"),
        "path":string_field(&session,"path"), "display_path":string_field(&session,"display_path"),
        "alive":bool_field(&session,"alive"), "busy":bool_field(&session,"busy"),
        "activity_state":string_field(&session,"activity_state"),
        "activity_agent":string_field(&session,"activity_agent")
    })
}

fn normalize(value: &str, label: &str) -> Result<String, PluginError> {
    let value = value
        .replace('\0', "")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        Err(error("INVALID_ARGUMENT", format!("{label} is empty")))
    } else {
        Ok(value)
    }
}

fn object(value: &Value) -> Result<&Map<String, Value>, PluginError> {
    value
        .as_object()
        .ok_or_else(|| error("INVALID_ARGUMENT", "arguments must be an object"))
}

fn required_string<'a>(fields: &'a Map<String, Value>, name: &str) -> Result<&'a str, PluginError> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| error("INVALID_ARGUMENT", format!("{name} must be a string")))
}

fn optional_string<'a>(
    fields: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a str>, PluginError> {
    fields
        .get(name)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| error("INVALID_ARGUMENT", format!("{name} must be a string")))
        })
        .transpose()
}

fn optional_bool(fields: &Map<String, Value>, name: &str) -> Result<Option<bool>, PluginError> {
    fields
        .get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| error("INVALID_ARGUMENT", format!("{name} must be a boolean")))
        })
        .transpose()
}

fn optional_u64(fields: &Map<String, Value>, name: &str) -> Result<Option<u64>, PluginError> {
    fields
        .get(name)
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                error(
                    "INVALID_ARGUMENT",
                    format!("{name} must be a non-negative integer"),
                )
            })
        })
        .transpose()
}

fn string_field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or("")
}

fn bool_field(value: &Value, name: &str) -> bool {
    value.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn error(code: &'static str, message: impl Into<String>) -> PluginError {
    PluginError {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
    };

    use super::*;

    fn success<T>(result: Result<T, PluginError>) -> T {
        match result {
            Ok(value) => value,
            Err(failure) => panic!(
                "unexpected plugin error {}: {}",
                failure.code, failure.message
            ),
        }
    }

    fn session(id: &str, name: &str, path: &str) -> Value {
        json!({
            "id":id, "name":name, "path":path,
            "display_path":format!("/home/codes/{path}"),
            "alive":true, "busy":false, "activity_state":"agent", "activity_agent":"Codex"
        })
    }

    fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request closed before its headers");
            request.extend_from_slice(&chunk[..read]);
            if let Some(index) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request closed before its body");
            request.extend_from_slice(&chunk[..read]);
        }
        request
    }

    fn write_response(stream: &mut std::net::TcpStream, body: &Value) {
        let body = serde_json::to_vec(body).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn normalizes_and_matches_paths() {
        assert_eq!(
            success(normalize(" first\n\n second\0 ", "message")),
            "first second"
        );
        let value = session("1", "rust-agent", "deepseekHarnessRs");
        assert!(matches_path(&value, "deepseekHarnessRs"));
        assert!(matches_path(&value, "/home/codes/deepseekHarnessRs"));
        assert!(!matches_path(&value, "webClx"));
    }

    #[test]
    fn resolves_one_exact_target() {
        let sessions = vec![
            session("1", "origin", "webClx"),
            session("2", "rust", "deepseekHarnessRs"),
        ];
        assert_eq!(
            string_field(success(exact_target(&sessions, "2")), "name"),
            "rust"
        );
        assert_eq!(
            exact_target(&sessions, "missing").unwrap_err().code,
            "TARGET_NOT_FOUND"
        );
    }

    #[test]
    fn rejects_credentialed_or_non_http_urls() {
        assert!(base_url("http://name:secret@127.0.0.1:11111", "url").is_err());
        assert!(base_url("file:///socket", "url").is_err());
        assert!(base_url(DEFAULT_URL, "url").is_ok());
    }

    #[tokio::test]
    async fn rejects_unknown_sender_before_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            write_response(
                &mut stream,
                &json!({"sessions":[session("target-1", "codex-target", "webClx")]}),
            );
            request
        });
        let client = WebClxClient::new(Config {
            base_url: base_url(&format!("http://{address}"), "test URL").unwrap(),
            reply_url: None,
            sender: Some("missing-origin".to_owned()),
            token_file: None,
        })
        .unwrap();

        let failure = client
            .send_message(&json!({"target":"codex-target", "message":"run the check"}))
            .await
            .unwrap_err();

        assert_eq!(failure.code, "TARGET_NOT_FOUND");
        let request = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(request.starts_with("GET /api/terminal/sessions?all=true "));
    }

    #[tokio::test]
    async fn rejects_http_success_when_delivery_is_not_accepted() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sessions = json!({"sessions":[
            session("sender-1", "rust-origin", "deepseekHarnessRs"),
            session("target-1", "codex-target", "webClx")
        ]});
        let server = thread::spawn(move || {
            for response in [sessions, json!({"ok":false,"submitted":false})] {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = read_request(&mut stream);
                write_response(&mut stream, &response);
            }
        });
        let client = WebClxClient::new(Config {
            base_url: base_url(&format!("http://{address}"), "test URL").unwrap(),
            reply_url: None,
            sender: Some("rust-origin".to_owned()),
            token_file: None,
        })
        .unwrap();

        let failure = client
            .send_message(&json!({"target":"codex-target", "message":"run the check"}))
            .await
            .unwrap_err();

        assert_eq!(failure.code, "DELIVERY_REJECTED");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn sends_verified_tagged_message_with_loopback_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let sessions = json!({"sessions":[
            session("sender-1", "rust-origin", "deepseekHarnessRs"),
            session("target-1", "codex-target", "webClx")
        ]});
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [sessions, json!({"ok":true,"submitted":true})] {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(read_request(&mut stream));
                write_response(&mut stream, &response);
            }
            requests
        });
        let token_path =
            std::env::temp_dir().join(format!("dsh-webclx-token-{}", uuid::Uuid::new_v4()));
        fs::write(&token_path, "a".repeat(64)).unwrap();
        let client = WebClxClient::new(Config {
            base_url: base_url(&format!("http://{address}"), "test URL").unwrap(),
            reply_url: None,
            sender: Some("rust-origin".to_owned()),
            token_file: Some(token_path.clone()),
        })
        .unwrap();
        let result = success(
            client
                .send_message(&json!({
                    "target":"codex-target",
                    "message":"run\n the check",
                    "request_reply":true
                }))
                .await,
        );
        assert_eq!(result["verification"], "confirmed");
        assert_eq!(result["submitted"], true);

        let requests = server.join().unwrap();
        fs::remove_file(token_path).unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            let text = String::from_utf8_lossy(request);
            assert!(
                text.to_ascii_lowercase()
                    .contains("x-webclx-local-token: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            );
        }
        let post = String::from_utf8(requests[1].clone()).unwrap();
        let body = post.split_once("\r\n\r\n").unwrap().1;
        let payload: Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["target"], "target-1");
        assert_eq!(payload["submit"], true);
        assert_eq!(payload["submit_enters"], 1);
        assert_eq!(payload["bracketed_paste"], true);
        assert_eq!(payload["verify_submission"], true);
        assert_eq!(payload["delivery_id"], payload["data"]);
        assert!(
            payload["data"]
                .as_str()
                .unwrap()
                .starts_with("[from rust-origin] run the check;")
        );
        assert!(
            payload["data"]
                .as_str()
                .unwrap()
                .contains("webclx_send_terminal_message tool to rust-origin")
        );
    }
}
