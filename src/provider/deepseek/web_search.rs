//! DeepSeek native web-search provider over its Anthropic-compatible endpoint.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::tools::{
    WEB_SEARCH_MAX_RESULTS, WebSearchFuture, WebSearchProvider, WebSearchProviderError,
    WebSearchResult, normalize_source,
};

use super::{
    config::DEFAULT_API_KEY_ENV,
    credentials::{
        ApiKey, CredentialLookup, CredentialRef, CredentialSource, EnvironmentCredentials,
    },
};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic/v1";
const BASE_URL_ENV: &str = "DEEPSEEK_SEARCH_BASE_URL";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const API_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u64 = 4_096;
const MAX_USES: u64 = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_BLOCKS: usize = 256;
const MAX_RESULT_ITEMS: usize = 512;
const MAX_CITATIONS: usize = 512;

#[derive(Clone, Debug)]
struct SearchConfig {
    endpoint: reqwest::Url,
    credential_ref: CredentialRef,
    timeout: Duration,
}

impl SearchConfig {
    fn from_process_environment() -> Result<Self, DeepSeekSearchBuildError> {
        let base = match std::env::var_os(BASE_URL_ENV) {
            None => DEFAULT_BASE_URL.to_owned(),
            Some(value) => value
                .into_string()
                .map_err(|_| DeepSeekSearchBuildError::EndpointEnvironmentNotUnicode)?,
        };
        Self::new(
            base,
            CredentialRef::new(DEFAULT_API_KEY_ENV)
                .map_err(|_| DeepSeekSearchBuildError::InvalidCredentialReference)?,
        )
    }

    fn new(
        base_url: impl AsRef<str>,
        credential_ref: CredentialRef,
    ) -> Result<Self, DeepSeekSearchBuildError> {
        let mut base = reqwest::Url::parse(base_url.as_ref())
            .map_err(|_| DeepSeekSearchBuildError::InvalidEndpoint)?;
        if !(matches!(base.scheme(), "https") || base.scheme() == "http" && is_loopback(&base))
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(DeepSeekSearchBuildError::InvalidEndpoint);
        }
        let path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        let endpoint = base
            .join("messages")
            .map_err(|_| DeepSeekSearchBuildError::InvalidEndpoint)?;
        Ok(Self {
            endpoint,
            credential_ref,
            timeout: REQUEST_TIMEOUT,
        })
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum DeepSeekSearchBuildError {
    #[error("DEEPSEEK_SEARCH_BASE_URL is not valid Unicode")]
    EndpointEnvironmentNotUnicode,
    #[error(
        "DEEPSEEK_SEARCH_BASE_URL must use HTTPS, or loopback HTTP for offline testing, without credentials, query, or fragment"
    )]
    InvalidEndpoint,
    #[error("the web-search credential reference is invalid")]
    InvalidCredentialReference,
    #[error("failed to initialize the DeepSeek web-search HTTPS client")]
    Client,
}

pub(crate) struct DeepSeekSearchProvider {
    config: SearchConfig,
    credentials: Arc<dyn CredentialSource>,
    client: reqwest::Client,
}

impl std::fmt::Debug for DeepSeekSearchProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeepSeekSearchProvider")
            .field("endpoint", &self.config.endpoint)
            .field("credential_ref", &self.config.credential_ref)
            .field("timeout", &self.config.timeout)
            .finish_non_exhaustive()
    }
}

impl DeepSeekSearchProvider {
    pub(crate) fn from_process_environment() -> Result<Self, DeepSeekSearchBuildError> {
        Self::new(
            SearchConfig::from_process_environment()?,
            Arc::new(EnvironmentCredentials),
        )
    }

    fn new(
        config: SearchConfig,
        credentials: Arc<dyn CredentialSource>,
    ) -> Result<Self, DeepSeekSearchBuildError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| DeepSeekSearchBuildError::Client)?;
        Ok(Self {
            config,
            credentials,
            client,
        })
    }

    async fn search_inner(
        &self,
        query: String,
        cancellation: CancellationToken,
    ) -> Result<WebSearchResult, WebSearchProviderError> {
        if cancellation.is_cancelled() {
            return Err(WebSearchProviderError::Cancelled);
        }
        let key = match self.credentials.resolve(&self.config.credential_ref) {
            CredentialLookup::Missing => return Err(WebSearchProviderError::CredentialMissing),
            CredentialLookup::InvalidEncoding => {
                return Err(WebSearchProviderError::CredentialInvalid);
            }
            CredentialLookup::Present(value) => {
                ApiKey::normalize(value).map_err(|_| WebSearchProviderError::CredentialInvalid)?
            }
        };
        if cancellation.is_cancelled() {
            return Err(WebSearchProviderError::Cancelled);
        }
        let body = serde_json::to_vec(&json!({
            "model": DEFAULT_MODEL,
            "max_tokens": MAX_TOKENS,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": format!("Perform a web search for the query: {query}")
                }]
            }],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": MAX_USES
            }]
        }))
        .map_err(|_| WebSearchProviderError::Provider)?;
        let request = self
            .client
            .post(self.config.endpoint.clone())
            .header("x-api-key", key.expose())
            .header("authorization", format!("Bearer {}", key.expose()))
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("user-agent", "dsh-rs/0.1")
            .body(body);

        let operation = async {
            let response = request
                .send()
                .await
                .map_err(|_| WebSearchProviderError::Provider)?;
            if !response.status().is_success() {
                return Err(WebSearchProviderError::Provider);
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| WebSearchProviderError::Provider)?;
                if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(WebSearchProviderError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            map_response(&bytes)
        };

        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(WebSearchProviderError::Cancelled),
            result = tokio::time::timeout(self.config.timeout, operation) => {
                result.unwrap_or(Err(WebSearchProviderError::Timeout))
            }
        }
    }
}

impl WebSearchProvider for DeepSeekSearchProvider {
    fn search(&self, query: String, cancellation: CancellationToken) -> WebSearchFuture<'_> {
        Box::pin(self.search_inner(query, cancellation))
    }
}

fn map_response(bytes: &[u8]) -> Result<WebSearchResult, WebSearchProviderError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| WebSearchProviderError::InvalidResponse)?;
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or(WebSearchProviderError::InvalidResponse)?;
    if blocks.len() > MAX_RESPONSE_BLOCKS {
        return Err(WebSearchProviderError::InvalidResponse);
    }

    let mut citations = HashMap::new();
    let mut citation_count = 0_usize;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(items) = block.get("citations").and_then(Value::as_array) else {
            continue;
        };
        citation_count = citation_count.saturating_add(items.len());
        if citation_count > MAX_CITATIONS {
            return Err(WebSearchProviderError::InvalidResponse);
        }
        for citation in items {
            let Some(url) = citation.get("url").and_then(Value::as_str) else {
                continue;
            };
            let Some(snippet) = citation.get("cited_text").and_then(Value::as_str) else {
                continue;
            };
            citations
                .entry(url.to_owned())
                .or_insert_with(|| snippet.to_owned());
        }
    }

    let mut found_result_block = false;
    let mut item_count = 0_usize;
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    let mut truncated = false;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("web_search_tool_result") {
            continue;
        }
        found_result_block = true;
        let Some(items) = block.get("content").and_then(Value::as_array) else {
            continue;
        };
        item_count = item_count.saturating_add(items.len());
        if item_count > MAX_RESULT_ITEMS {
            return Err(WebSearchProviderError::InvalidResponse);
        }
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("web_search_result") {
                continue;
            }
            let Some(raw_url) = item.get("url").and_then(Value::as_str) else {
                continue;
            };
            if seen.contains(raw_url) {
                continue;
            }
            let source = normalize_source(
                raw_url.to_owned(),
                item.get("title").and_then(Value::as_str).map(str::to_owned),
                citations.get(raw_url).cloned(),
                item.get("page_age")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            let Some(source) = source else {
                continue;
            };
            seen.insert(raw_url.to_owned());
            if sources.len() == WEB_SEARCH_MAX_RESULTS {
                truncated = true;
                continue;
            }
            sources.push(source);
        }
    }
    if !found_result_block {
        return Err(WebSearchProviderError::NoNativeResults);
    }
    Ok(WebSearchResult { sources, truncated })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
        time::Duration,
    };

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::{DeepSeekSearchProvider, SearchConfig, map_response};
    use crate::{
        provider::deepseek::{CredentialRef, SecretValue, StaticCredentials},
        tools::{WebSearchProvider, WebSearchProviderError},
    };

    const TEST_KEY: &str = "test-key-for-loopback-only";

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let mut expected = None;
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if expected.is_none() {
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    expected = Some(header_end + 4 + length);
                }
            }
            if expected.is_some_and(|expected| bytes.len() >= expected) {
                break;
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    fn spawn_server(body: String) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
            request
        });
        (format!("http://{address}/anthropic/v1"), handle)
    }

    fn provider(base: &str) -> DeepSeekSearchProvider {
        provider_with_timeout(base, super::REQUEST_TIMEOUT)
    }

    fn provider_with_timeout(base: &str, timeout: Duration) -> DeepSeekSearchProvider {
        let reference = CredentialRef::new("DEEPSEEK_API_KEY").unwrap();
        let mut config = SearchConfig::new(base, reference.clone()).unwrap();
        config.timeout = timeout;
        DeepSeekSearchProvider::new(
            config,
            Arc::new(StaticCredentials::new(
                reference,
                SecretValue::new(TEST_KEY),
            )),
        )
        .unwrap()
    }

    fn spawn_partial_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{",
                )
                .unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(100));
        });
        (format!("http://{address}/anthropic/v1"), server)
    }

    #[test]
    fn response_mapping_joins_citations_deduplicates_and_caps_sources() {
        let sources = (0..10)
            .map(|index| {
                json!({
                    "type":"web_search_result",
                    "url":format!("https://example.test/{index}"),
                    "title":format!("Title {index}"),
                    "page_age":"2026-08-29"
                })
            })
            .collect::<Vec<_>>();
        let body = serde_json::to_vec(&json!({
            "content":[
                {"type":"text","citations":[{"url":"https://example.test/0","cited_text":"snippet"}]},
                {"type":"web_search_tool_result","content":sources},
                {"type":"web_search_tool_result","content":[{"type":"web_search_result","url":"https://example.test/0"}]}
            ]
        }))
        .unwrap();
        let result = map_response(&body).unwrap();
        assert_eq!(result.sources.len(), 8);
        assert!(result.truncated);
        assert_eq!(result.sources[0].snippet.as_deref(), Some("snippet"));
    }

    #[test]
    fn prose_without_native_result_blocks_is_rejected() {
        let body =
            serde_json::to_vec(&json!({"content":[{"type":"text","text":"answer"}]})).unwrap();
        assert_eq!(
            map_response(&body),
            Err(WebSearchProviderError::NoNativeResults)
        );
    }

    #[test]
    fn an_empty_native_result_block_is_a_successful_empty_search() {
        let body = serde_json::to_vec(&json!({
            "content":[{"type":"web_search_tool_result","content":[]}]
        }))
        .unwrap();
        let result = map_response(&body).unwrap();
        assert!(result.sources.is_empty());
        assert!(!result.truncated);
    }

    #[test]
    fn endpoint_policy_requires_https_except_for_loopback_tests() {
        let reference = CredentialRef::new("DEEPSEEK_API_KEY").unwrap();
        assert!(SearchConfig::new("https://example.test/v1", reference.clone()).is_ok());
        assert!(SearchConfig::new("http://127.0.0.1:8080/v1", reference.clone()).is_ok());
        assert!(SearchConfig::new("http://example.test/v1", reference.clone()).is_err());
        assert!(SearchConfig::new("https://user@example.test/v1", reference).is_err());
    }

    #[tokio::test]
    async fn loopback_request_uses_native_search_shape_and_keeps_the_key_out_of_results() {
        let body = json!({
            "content":[
                {"type":"text","citations":[{"url":"https://example.test/a","cited_text":"bounded excerpt"}]},
                {"type":"web_search_tool_result","content":[{
                    "type":"web_search_result",
                    "url":"https://example.test/a",
                    "title":"Example",
                    "page_age":"2026-08-29"
                }]}
            ]
        })
        .to_string();
        let (base, server) = spawn_server(body);
        let provider = provider(&base);
        let result = provider
            .search("latest Rust".to_owned(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.sources.len(), 1);
        assert_eq!(
            result.sources[0].snippet.as_deref(),
            Some("bounded excerpt")
        );
        assert!(!format!("{provider:?}").contains(TEST_KEY));

        let request = server.join().unwrap();
        let lower = request.to_ascii_lowercase();
        assert!(lower.starts_with("post /anthropic/v1/messages http/1.1\r\n"));
        assert!(lower.contains("x-api-key: test-key-for-loopback-only\r\n"));
        assert!(lower.contains("authorization: bearer test-key-for-loopback-only\r\n"));
        let payload: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["model"], "deepseek-v4-flash");
        assert_eq!(payload["tools"][0]["type"], "web_search_20250305");
        assert_eq!(
            payload["messages"][0]["content"][0]["text"],
            "Perform a web search for the query: latest Rust"
        );
        assert!(!payload.to_string().contains(TEST_KEY));
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_partial_response_body() {
        let (base, server) = spawn_partial_server();
        let provider = provider(&base);
        let cancellation = CancellationToken::new();
        let child = cancellation.clone();
        let task = tokio::spawn(async move { provider.search("q".to_owned(), child).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancellation.cancel();
        assert_eq!(task.await.unwrap(), Err(WebSearchProviderError::Cancelled));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn one_deadline_covers_a_partial_response_body() {
        let (base, server) = spawn_partial_server();
        let provider = provider_with_timeout(&base, Duration::from_millis(10));
        assert_eq!(
            provider
                .search("q".to_owned(), CancellationToken::new())
                .await,
            Err(WebSearchProviderError::Timeout)
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn response_body_stops_at_the_fixed_byte_limit() {
        let (base, server) = spawn_server("x".repeat(super::MAX_RESPONSE_BYTES + 1));
        let provider = provider(&base);
        assert_eq!(
            provider
                .search("q".to_owned(), CancellationToken::new())
                .await,
            Err(WebSearchProviderError::ResponseTooLarge)
        );
        let _ = server.join().unwrap();
    }
}
