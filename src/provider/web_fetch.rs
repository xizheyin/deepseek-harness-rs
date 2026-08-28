//! Anonymous HTTP(S) retrieval with public-address validation and DNS pinning.

use std::{
    collections::HashSet,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{StatusCode, Url, header};
use tokio::net::lookup_host;
use tokio_util::sync::CancellationToken;

use crate::tools::{
    WEB_FETCH_MAX_URL_BYTES, WebFetchBodyKind, WebFetchFuture, WebFetchProvider,
    WebFetchProviderError, WebFetchResult,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;
const MAX_RESPONSE_BYTES: usize = 5_000_000;
const MAX_BODY_CHARS: usize = 100_000;
const USER_AGENT: &str = "dsh-rs/0.1 (+https://github.com/xizheyin/deepseek-harness-rs)";
const ACCEPT: &str = "text/html,application/xhtml+xml,text/*;q=0.9,application/json;q=0.8";
const DNS64_DISCOVERY_HOST: &str = "ipv4only.arpa";
const DNS64_PREFIX_LENGTHS: [usize; 6] = [32, 40, 48, 56, 64, 96];

type AddressFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, ()>> + Send + 'a>>;

trait AddressResolver: Send + Sync {
    fn resolve(&self, hostname: String, cancellation: CancellationToken) -> AddressFuture<'_>;
}

#[derive(Debug)]
struct SystemResolver;

impl AddressResolver for SystemResolver {
    fn resolve(&self, hostname: String, cancellation: CancellationToken) -> AddressFuture<'_> {
        Box::pin(async move {
            let lookup = async move {
                let addresses = lookup_host((hostname.as_str(), 0)).await.map_err(|_| ())?;
                let mut seen = HashSet::new();
                Ok(addresses
                    .filter_map(|address| seen.insert(address.ip()).then_some(address.ip()))
                    .collect())
            };
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(()),
                result = lookup => result,
            }
        })
    }
}

/// Production anonymous fetch provider used by the standard local registry.
pub(crate) struct HttpWebFetchProvider {
    resolver: Arc<dyn AddressResolver>,
    timeout: Duration,
}

impl std::fmt::Debug for HttpWebFetchProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpWebFetchProvider")
            .field("timeout", &self.timeout)
            .field("max_redirects", &MAX_REDIRECTS)
            .field("max_response_bytes", &MAX_RESPONSE_BYTES)
            .field("max_body_chars", &MAX_BODY_CHARS)
            .finish_non_exhaustive()
    }
}

impl HttpWebFetchProvider {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            resolver: Arc::new(SystemResolver),
            timeout: REQUEST_TIMEOUT,
        }
    }

    async fn fetch_inner(
        &self,
        raw_url: String,
        cancellation: CancellationToken,
    ) -> Result<WebFetchResult, WebFetchProviderError> {
        let mut current = validate_url(&raw_url)?;
        let mut redirects = 0_usize;
        loop {
            if cancellation.is_cancelled() {
                return Err(WebFetchProviderError::Cancelled);
            }
            let host = current
                .host_str()
                .ok_or(WebFetchProviderError::InvalidUrl)?
                .to_owned();
            let addresses =
                resolve_public_addresses(&host, self.resolver.as_ref(), cancellation.clone())
                    .await?;
            let response = request_pinned(&current, &host, &addresses).await?;
            if is_redirect(response.status()) {
                if redirects >= MAX_REDIRECTS {
                    return Err(WebFetchProviderError::RedirectBlocked);
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(WebFetchProviderError::Provider)?;
                let target = current
                    .join(location)
                    .map_err(|_| WebFetchProviderError::InvalidUrl)?;
                let target = validate_url(target.as_str())?;
                if !same_origin(&current, &target) {
                    return Err(WebFetchProviderError::RedirectBlocked);
                }
                current = target;
                redirects += 1;
                continue;
            }
            return read_response(response, current, cancellation).await;
        }
    }
}

impl Default for HttpWebFetchProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchProvider for HttpWebFetchProvider {
    fn fetch(&self, url: String, cancellation: CancellationToken) -> WebFetchFuture<'_> {
        Box::pin(async move {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(WebFetchProviderError::Cancelled),
                result = tokio::time::timeout(
                    self.timeout,
                    self.fetch_inner(url, cancellation.clone()),
                ) => result.unwrap_or(Err(WebFetchProviderError::Timeout)),
            }
        })
    }
}

fn validate_url(input: &str) -> Result<Url, WebFetchProviderError> {
    if input.len() > WEB_FETCH_MAX_URL_BYTES {
        return Err(WebFetchProviderError::InvalidUrl);
    }
    let url = Url::parse(input).map_err(|_| WebFetchProviderError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(WebFetchProviderError::InvalidUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebFetchProviderError::BlockedUrl);
    }
    Ok(url)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

async fn request_pinned(
    url: &Url,
    hostname: &str,
    addresses: &[IpAddr],
) -> Result<reqwest::Response, WebFetchProviderError> {
    let port = url
        .port_or_known_default()
        .ok_or(WebFetchProviderError::InvalidUrl)?;
    let socket_addresses: Vec<_> = addresses
        .iter()
        .copied()
        .map(|address| SocketAddr::new(address, port))
        .collect();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .resolve_to_addrs(hostname, &socket_addresses)
        .build()
        .map_err(|_| WebFetchProviderError::Provider)?;
    client
        .get(url.clone())
        .header(header::USER_AGENT, USER_AGENT)
        .header(header::ACCEPT, ACCEPT)
        .send()
        .await
        .map_err(|_| WebFetchProviderError::Provider)
}

async fn resolve_public_addresses(
    hostname: &str,
    resolver: &dyn AddressResolver,
    cancellation: CancellationToken,
) -> Result<Vec<IpAddr>, WebFetchProviderError> {
    let addresses = match hostname.parse::<IpAddr>() {
        Ok(address) => vec![address],
        Err(_) => resolver
            .resolve(hostname.to_owned(), cancellation.clone())
            .await
            .map_err(|_| WebFetchProviderError::Provider)?,
    };
    if addresses.is_empty() {
        return Err(WebFetchProviderError::Provider);
    }
    let nat64_prefixes = if addresses.iter().any(IpAddr::is_ipv6) {
        discover_nat64_prefixes(resolver, cancellation).await?
    } else {
        Vec::new()
    };
    for address in &addresses {
        if !is_public_ip(*address) {
            return Err(WebFetchProviderError::BlockedUrl);
        }
        if let IpAddr::V6(address) = address {
            if translated_ipv4(*address, &nat64_prefixes)
                .is_some_and(|ipv4| !is_public_ip(IpAddr::V4(ipv4)))
            {
                return Err(WebFetchProviderError::BlockedUrl);
            }
        }
    }
    Ok(addresses)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Nat64Prefix {
    bytes: Vec<u8>,
    length: usize,
}

async fn discover_nat64_prefixes(
    resolver: &dyn AddressResolver,
    cancellation: CancellationToken,
) -> Result<Vec<Nat64Prefix>, WebFetchProviderError> {
    let addresses = resolver
        .resolve(DNS64_DISCOVERY_HOST.to_owned(), cancellation)
        .await
        .map_err(|_| WebFetchProviderError::Provider)?;
    let mut prefixes = Vec::new();
    let mut seen = HashSet::new();
    for address in addresses {
        let IpAddr::V6(address) = address else {
            continue;
        };
        let bytes = address.octets();
        for length in DNS64_PREFIX_LENGTHS {
            let Some(embedded) = embedded_ipv4(&bytes, length) else {
                continue;
            };
            if !matches!(embedded.octets(), [192, 0, 0, 170] | [192, 0, 0, 171]) {
                continue;
            }
            let prefix = Nat64Prefix {
                bytes: bytes[..length / 8].to_vec(),
                length,
            };
            if seen.insert((prefix.length, prefix.bytes.clone())) {
                prefixes.push(prefix);
            }
        }
    }
    Ok(prefixes)
}

fn translated_ipv4(address: Ipv6Addr, prefixes: &[Nat64Prefix]) -> Option<Ipv4Addr> {
    let bytes = address.octets();
    for prefix in prefixes {
        if bytes.get(..prefix.bytes.len()) != Some(prefix.bytes.as_slice()) {
            continue;
        }
        if let Some(address) = embedded_ipv4(&bytes, prefix.length) {
            return Some(address);
        }
    }
    None
}

fn embedded_ipv4(bytes: &[u8; 16], prefix_length: usize) -> Option<Ipv4Addr> {
    if prefix_length == 96 {
        return Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    if !DNS64_PREFIX_LENGTHS.contains(&prefix_length) || bytes[8] != 0 {
        return None;
    }
    let prefix_bytes = prefix_length / 8;
    let before_reserved = 8_usize.checked_sub(prefix_bytes)?;
    let mut ipv4 = [0_u8; 4];
    ipv4[..before_reserved].copy_from_slice(&bytes[prefix_bytes..prefix_bytes + before_reserved]);
    ipv4[before_reserved..].copy_from_slice(&bytes[9..9 + 4 - before_reserved]);
    Some(Ipv4Addr::from(ipv4))
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or_else(|| is_public_ipv6(address), is_public_ipv4),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let blocked = [
        (Ipv4Addr::new(0, 0, 0, 0), 8),
        (Ipv4Addr::new(10, 0, 0, 0), 8),
        (Ipv4Addr::new(100, 64, 0, 0), 10),
        (Ipv4Addr::new(127, 0, 0, 0), 8),
        (Ipv4Addr::new(169, 254, 0, 0), 16),
        (Ipv4Addr::new(172, 16, 0, 0), 12),
        (Ipv4Addr::new(192, 0, 0, 0), 24),
        (Ipv4Addr::new(192, 0, 2, 0), 24),
        (Ipv4Addr::new(192, 88, 99, 0), 24),
        (Ipv4Addr::new(192, 168, 0, 0), 16),
        (Ipv4Addr::new(198, 18, 0, 0), 15),
        (Ipv4Addr::new(198, 51, 100, 0), 24),
        (Ipv4Addr::new(203, 0, 113, 0), 24),
        (Ipv4Addr::new(224, 0, 0, 0), 4),
        (Ipv4Addr::new(240, 0, 0, 0), 4),
    ];
    blocked
        .iter()
        .all(|(network, prefix)| !ipv4_in_prefix(address, *network, *prefix))
}

fn ipv4_in_prefix(address: Ipv4Addr, network: Ipv4Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(address) & mask == u32::from(network) & mask
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    // Current globally routable unicast allocation, minus protocol and
    // documentation ranges whose destination cannot be treated as public.
    ipv6_in_prefix(address, "2000::".parse().expect("literal IPv6"), 3)
        && !ipv6_in_prefix(address, "2001::".parse().expect("literal IPv6"), 23)
        && !ipv6_in_prefix(address, "2001:db8::".parse().expect("literal IPv6"), 32)
        && !ipv6_in_prefix(address, "2002::".parse().expect("literal IPv6"), 16)
        && !ipv6_in_prefix(address, "3fff::".parse().expect("literal IPv6"), 20)
}

fn ipv6_in_prefix(address: Ipv6Addr, network: Ipv6Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    u128::from(address) & mask == u128::from(network) & mask
}

async fn read_response(
    response: reqwest::Response,
    final_url: Url,
    cancellation: CancellationToken,
) -> Result<WebFetchResult, WebFetchProviderError> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(WebFetchProviderError::UnsupportedContentType)?
        .to_owned();
    let body_kind = classify_content_type(&content_type)
        .ok_or(WebFetchProviderError::UnsupportedContentType)?;
    let charset = parse_charset(&content_type);
    validate_charset(charset.as_deref())?;
    if response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
        return Err(WebFetchProviderError::ResponseTooLarge);
    }
    let status_code = response.status().as_u16();
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut truncated_by_bytes = false;
    loop {
        let next = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(WebFetchProviderError::Cancelled),
            next = stream.next() => next,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|_| WebFetchProviderError::Provider)?;
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(bytes.len());
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated_by_bytes = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let decoded = decode_body(&bytes, charset.as_deref())?;
    let (content, truncated_by_chars) = truncate_chars(decoded, MAX_BODY_CHARS);
    Ok(WebFetchResult {
        url: final_url.to_string(),
        status_code,
        body_kind,
        content,
        truncated: truncated_by_bytes || truncated_by_chars,
    })
}

fn classify_content_type(content_type: &str) -> Option<WebFetchBodyKind> {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if matches!(mime.as_str(), "text/html" | "application/xhtml+xml") {
        return Some(WebFetchBodyKind::Html);
    }
    if mime.starts_with("text/")
        || matches!(mime.as_str(), "application/json" | "application/xml")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
    {
        return Some(WebFetchBodyKind::Text);
    }
    None
}

fn parse_charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['"', '\'']).to_ascii_lowercase())
    })
}

fn validate_charset(charset: Option<&str>) -> Result<(), WebFetchProviderError> {
    if charset.is_none_or(|value| {
        matches!(
            value,
            "utf-8"
                | "utf8"
                | "us-ascii"
                | "ascii"
                | "iso-8859-1"
                | "latin1"
                | "windows-1252"
                | "cp1252"
        )
    }) {
        Ok(())
    } else {
        Err(WebFetchProviderError::UnsupportedContentType)
    }
}

fn decode_body(bytes: &[u8], charset: Option<&str>) -> Result<String, WebFetchProviderError> {
    validate_charset(charset)?;
    if charset.is_none_or(|value| matches!(value, "utf-8" | "utf8")) {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(bytes
        .iter()
        .map(|byte| decode_windows_1252(*byte))
        .collect())
}

fn decode_windows_1252(byte: u8) -> char {
    const EXTENDED: [char; 32] = [
        '€', '\u{0081}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{008d}', 'Ž',
        '\u{008f}', '\u{0090}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{009d}', 'ž', 'Ÿ',
    ];
    if (0x80..=0x9f).contains(&byte) {
        EXTENDED[usize::from(byte - 0x80)]
    } else {
        char::from(byte)
    }
}

fn truncate_chars(value: String, maximum: usize) -> (String, bool) {
    let mut boundary = value.len();
    for (seen, (offset, _)) in value.char_indices().enumerate() {
        if seen == maximum {
            boundary = offset;
            break;
        }
    }
    let truncated = boundary < value.len();
    if truncated {
        (value[..boundary].to_owned(), true)
    } else {
        (value, false)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, TcpListener},
        sync::Arc,
        thread,
        time::Duration,
    };

    use tokio_util::sync::CancellationToken;

    use super::{
        AddressFuture, AddressResolver, HttpWebFetchProvider, Nat64Prefix, classify_content_type,
        decode_body, embedded_ipv4, is_public_ip, parse_charset, read_response, request_pinned,
        resolve_public_addresses, same_origin, translated_ipv4, validate_url,
    };
    use crate::tools::{WebFetchBodyKind, WebFetchProvider, WebFetchProviderError};

    struct StaticResolver {
        addresses: Vec<IpAddr>,
        dns64: Vec<IpAddr>,
    }

    struct HangingResolver;

    impl AddressResolver for HangingResolver {
        fn resolve(&self, _: String, cancellation: CancellationToken) -> AddressFuture<'_> {
            Box::pin(async move {
                cancellation.cancelled().await;
                Err(())
            })
        }
    }

    impl AddressResolver for StaticResolver {
        fn resolve(&self, hostname: String, _: CancellationToken) -> AddressFuture<'_> {
            Box::pin(async move {
                Ok(if hostname == super::DNS64_DISCOVERY_HOST {
                    self.dns64.clone()
                } else {
                    self.addresses.clone()
                })
            })
        }
    }

    #[test]
    fn url_and_content_policy_are_closed() {
        assert!(validate_url("https://example.test/a").is_ok());
        assert_eq!(
            validate_url("https://user:pass@example.test/a"),
            Err(WebFetchProviderError::BlockedUrl)
        );
        assert_eq!(
            validate_url("file:///etc/passwd"),
            Err(WebFetchProviderError::InvalidUrl)
        );
        assert_eq!(
            classify_content_type("text/html; charset=utf-8"),
            Some(WebFetchBodyKind::Html)
        );
        assert_eq!(
            classify_content_type("application/problem+json"),
            Some(WebFetchBodyKind::Text)
        );
        assert_eq!(classify_content_type("image/png"), None);
        assert_eq!(
            parse_charset("text/plain; charset=\"Windows-1252\""),
            Some("windows-1252".to_owned())
        );
        assert_eq!(decode_body(&[0x80], Some("windows-1252")).unwrap(), "€");
        assert!(decode_body(b"x", Some("shift_jis")).is_err());
        let base = reqwest::Url::parse("https://example.test:443/a").unwrap();
        assert!(same_origin(
            &base,
            &reqwest::Url::parse("https://example.test/b").unwrap()
        ));
        assert!(!same_origin(
            &base,
            &reqwest::Url::parse("https://other.test/b").unwrap()
        ));
        assert!(!same_origin(
            &base,
            &reqwest::Url::parse("http://example.test/b").unwrap()
        ));
    }

    #[test]
    fn representative_private_reserved_and_transition_addresses_are_blocked() {
        for address in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.0.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !is_public_ip(address.parse().unwrap()),
                "accepted {address}"
            );
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public_ip(address.parse().unwrap()), "blocked {address}");
        }
    }

    #[tokio::test]
    async fn one_non_public_dns_answer_blocks_the_whole_set() {
        let resolver = StaticResolver {
            addresses: vec!["8.8.8.8".parse().unwrap(), "127.0.0.1".parse().unwrap()],
            dns64: Vec::new(),
        };
        assert_eq!(
            resolve_public_addresses("example.test", &resolver, CancellationToken::new()).await,
            Err(WebFetchProviderError::BlockedUrl)
        );
    }

    #[tokio::test]
    async fn discovered_nat64_translation_to_private_ipv4_is_blocked() {
        let resolver = StaticResolver {
            addresses: vec!["2606:4700:64::7f00:1".parse().unwrap()],
            dns64: vec!["2606:4700:64::c000:aa".parse().unwrap()],
        };
        assert_eq!(
            resolve_public_addresses("example.test", &resolver, CancellationToken::new()).await,
            Err(WebFetchProviderError::BlockedUrl)
        );
    }

    #[test]
    fn discovered_nat64_prefix_exposes_embedded_private_ipv4() {
        let sentinel: std::net::Ipv6Addr = "2001:db8:64::c000:aa".parse().unwrap();
        let bytes = sentinel.octets();
        assert_eq!(
            embedded_ipv4(&bytes, 96),
            Some(Ipv4Addr::new(192, 0, 0, 170))
        );
        let prefix = Nat64Prefix {
            bytes: bytes[..12].to_vec(),
            length: 96,
        };
        let translated: std::net::Ipv6Addr = "2001:db8:64::7f00:1".parse().unwrap();
        assert_eq!(
            translated_ipv4(translated, &[prefix]),
            Some(Ipv4Addr::new(127, 0, 0, 1))
        );
    }

    #[tokio::test]
    async fn pinned_transport_keeps_hostname_but_connects_only_to_supplied_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let read = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            request
        });
        let url = reqwest::Url::parse(&format!(
            "http://does-not-resolve.test:{}/a",
            address.port()
        ))
        .unwrap();
        let response = request_pinned(
            &url,
            "does-not-resolve.test",
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )
        .await
        .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let request = server.join().unwrap();
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("host: does-not-resolve.test:{}", address.port()))
        );
    }

    #[tokio::test]
    async fn pinned_response_is_classified_decoded_and_returned_with_non_success_status() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer).unwrap();
            let body = "<h1>Not found</h1>";
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let url = reqwest::Url::parse(&format!(
            "http://does-not-resolve.test:{}/missing",
            address.port()
        ))
        .unwrap();
        let response = request_pinned(
            &url,
            "does-not-resolve.test",
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )
        .await
        .unwrap();
        let result = read_response(response, url, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.status_code, 404);
        assert_eq!(result.body_kind, WebFetchBodyKind::Html);
        assert_eq!(result.content, "<h1>Not found</h1>");
        assert!(!result.truncated);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn declared_oversized_body_is_rejected_before_collection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                super::MAX_RESPONSE_BYTES + 1
            )
            .unwrap();
        });
        let url = reqwest::Url::parse(&format!(
            "http://does-not-resolve.test:{}/large",
            address.port()
        ))
        .unwrap();
        let response = request_pinned(
            &url,
            "does-not-resolve.test",
            &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )
        .await
        .unwrap();
        assert_eq!(
            read_response(response, url, CancellationToken::new()).await,
            Err(WebFetchProviderError::ResponseTooLarge)
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn whole_operation_timeout_and_precancel_are_distinct() {
        let provider = HttpWebFetchProvider {
            resolver: Arc::new(HangingResolver),
            timeout: Duration::from_millis(10),
        };
        assert_eq!(
            provider
                .fetch(
                    "https://never-resolves.test/".to_owned(),
                    CancellationToken::new(),
                )
                .await,
            Err(WebFetchProviderError::Timeout)
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            provider
                .fetch("https://never-resolves.test/".to_owned(), cancellation,)
                .await,
            Err(WebFetchProviderError::Cancelled)
        );
    }

    #[tokio::test]
    async fn literal_loopback_is_blocked_before_transport() {
        let provider = HttpWebFetchProvider::new();
        assert_eq!(
            provider
                .fetch("http://127.0.0.1/".to_owned(), CancellationToken::new())
                .await,
            Err(WebFetchProviderError::BlockedUrl)
        );
    }
}
