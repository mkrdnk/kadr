//! Bounded HTTP reads for an app-approved source, never an incoming request URL.

use std::{
    fmt,
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use kadr_core::source::ManualSource;
use reqwest::{
    Client, Method, Response, StatusCode, Url,
    header::{
        ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
        HeaderMap, HeaderValue, IF_MATCH, IF_UNMODIFIED_SINCE, LAST_MODIFIED, LOCATION, RANGE,
    },
};
use tokio_util::sync::CancellationToken;

use crate::{ByteSource, ReadFuture};

pub(super) const MAX_READ: usize = 64 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_URL: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

/// A single approved HTTP(S) resource with a stable length and validators.
///
/// Construct this in the application, not from a loopback streaming request.
/// No persistent cache or unbounded full-file buffering is used. A nonempty
/// resource must supply a strong ETag or a valid Last-Modified date.
pub struct RemoteSource {
    endpoint: Endpoint,
    length: u64,
    validators: Validators,
    content_type: Option<String>,
    cancel: CancellationToken,
    changed: AtomicBool,
}

impl fmt::Debug for RemoteSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteSource")
            .field("resource", &"[redacted]")
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl RemoteSource {
    /// Open an app-approved URL. Lexical validation is repeated defensively;
    /// credentials are rejected and fragments removed by `kadr-core`.
    ///
    /// Cancelling the token aborts opening and all outstanding/future reads.
    /// Errors intentionally contain neither URLs nor upstream error strings.
    pub async fn open(url: impl AsRef<str>, cancel: CancellationToken) -> io::Result<Self> {
        let url = validated_url(url.as_ref())?;
        let (endpoint, length, validators, content_type) = bounded(&cancel, async {
            let endpoint = Endpoint::resolve(url, false).await?;
            let mut redirects = 0;
            let (response, endpoint) =
                request(endpoint, Method::HEAD, None, &mut redirects).await?;
            let status = response.status();
            let head_length = length_header(response.headers())?;
            if status == StatusCode::OK
                && let Some(length) = head_length
            {
                check_identity(response.headers())?;
                return Ok((
                    endpoint,
                    length,
                    Validators::from_headers(response.headers())?,
                    media_type(response.headers()),
                ));
            }
            if status != StatusCode::OK
                && status != StatusCode::METHOD_NOT_ALLOWED
                && status != StatusCode::NOT_IMPLEMENTED
            {
                return Err(invalid("remote metadata request failed"));
            }
            drop(response);
            let (mut response, endpoint) =
                request(endpoint, Method::GET, Some("bytes=0-0"), &mut redirects).await?;
            check_identity(response.headers())?;
            let validators = Validators::from_headers(response.headers())?;
            let content_type = media_type(response.headers());
            let length = match response.status() {
                StatusCode::PARTIAL_CONTENT => {
                    let (start, end, total) = content_range(response.headers())?;
                    if start != 0 || end != 0 || total == 0 {
                        return Err(invalid("remote probe returned an invalid range"));
                    }
                    check_body_length(response.headers(), 1)?;
                    read_body(&mut response, 1).await?;
                    total
                }
                StatusCode::OK => {
                    // A range-ignoring server can advertise a length, but its
                    // unrequested body is dropped, never downloaded as a cache.
                    if response.headers().contains_key(CONTENT_RANGE) {
                        return Err(invalid("unexpected remote content range"));
                    }
                    length_header(response.headers())?
                        .ok_or_else(|| unsupported("remote length is unknown"))?
                }
                StatusCode::RANGE_NOT_SATISFIABLE
                    if single_header(response.headers(), CONTENT_RANGE)? == Some("bytes */0") =>
                {
                    0
                }
                _ => return Err(invalid("remote range probe failed")),
            };
            Ok((endpoint, length, validators, content_type))
        })
        .await?;
        if length != 0 && !validators.can_condition() {
            return Err(unsupported(
                "remote resource needs a strong ETag or Last-Modified",
            ));
        }
        Ok(Self {
            endpoint,
            length,
            validators,
            content_type,
            cancel,
            changed: AtomicBool::new(false),
        })
    }

    /// Lowercase MIME essence (parameters removed), or `None` for invalid MIME.
    /// The loopback server must explicitly opt in to using this metadata.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    fn resource_changed(&self) -> io::Error {
        self.changed.store(true, Ordering::Release);
        invalid("remote resource changed; reopen the source")
    }

    async fn read(&self, offset: u64, max_len: usize) -> io::Result<Bytes> {
        if self.changed.load(Ordering::Acquire) {
            return Err(self.resource_changed());
        }
        if offset >= self.length {
            return Ok(Bytes::new());
        }
        if max_len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero-sized remote read",
            ));
        }
        let size = (self.length - offset).min(max_len.min(MAX_READ) as u64) as usize;
        let end = offset + size as u64 - 1;
        let mut request = self
            .endpoint
            .client
            .get(self.endpoint.url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes={offset}-{end}"));
        if let Some(etag) = self.validators.strong_etag() {
            request = request.header(IF_MATCH, etag);
        } else if let Some(date) = &self.validators.modified {
            request = request.header(IF_UNMODIFIED_SINCE, date);
        }
        let mut response = request.send().await.map_err(network_error)?;
        check_headers(response.headers())?;
        if matches!(
            response.status(),
            StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE
        ) || response.status().is_redirection()
        {
            return Err(self.resource_changed());
        }
        if response.status() != StatusCode::PARTIAL_CONTENT && response.status() != StatusCode::OK {
            return Err(invalid("remote read request failed"));
        }
        check_identity(response.headers())?;
        if !self.validators.matches(response.headers())? {
            return Err(self.resource_changed());
        }
        if response.status() == StatusCode::PARTIAL_CONTENT {
            let (actual_start, actual_end, total) = content_range(response.headers())?;
            if total != self.length {
                return Err(self.resource_changed());
            }
            if actual_start != offset || actual_end != end {
                return Err(invalid("remote response does not match requested range"));
            }
        } else {
            let length = length_header(response.headers())?
                .ok_or_else(|| unsupported("range-ignoring remote has unknown length"))?;
            if length != self.length {
                return Err(self.resource_changed());
            }
            if offset != 0 || size as u64 != self.length {
                return Err(unsupported(
                    "remote server does not support positional ranges",
                ));
            }
            if response.headers().contains_key(CONTENT_RANGE) {
                return Err(invalid("unexpected remote content range"));
            }
        }
        check_body_length(response.headers(), size as u64)?;
        let bytes = read_body(&mut response, size).await?;
        if self.changed.load(Ordering::Acquire) {
            return Err(self.resource_changed());
        }
        Ok(bytes)
    }
}

impl ByteSource for RemoteSource {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        Box::pin(bounded(&self.cancel, self.read(offset, max_len)))
    }
}

pub(super) async fn bounded<T>(
    cancel: &CancellationToken,
    future: impl Future<Output = io::Result<T>>,
) -> io::Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "remote operation cancelled")),
        result = tokio::time::timeout(DEADLINE, future) =>
            result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "remote operation timed out"))?,
    }
}

// Reuse a client with pinned DNS answers for the final URL across all reads.
// New redirect endpoints get a fresh checked/pinned client during opening only.
pub(super) struct Endpoint {
    client: Client,
    url: Url,
    public: bool,
}

impl Endpoint {
    pub(super) async fn resolve(url: Url, public_only: bool) -> io::Result<Self> {
        let host = url
            .host_str()
            .ok_or_else(|| invalid("invalid remote host"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = url
            .port_or_known_default()
            .ok_or_else(|| invalid("invalid remote port"))?;
        let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| invalid("remote host lookup failed"))?
                .take(33)
                .collect()
        };
        if addresses.is_empty() || addresses.len() > 32 {
            return Err(invalid("remote host has no bounded address set"));
        }
        let public = addresses.iter().all(|a| public_ip(a.ip()));
        if (public_only && !public) || (!public && addresses.iter().any(|a| public_ip(a.ip()))) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe remote redirect or address set",
            ));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .http1_only()
            .http1_max_headers(64)
            .connect_timeout(Duration::from_secs(10))
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            url,
            public,
        })
    }
}

pub(super) async fn request(
    mut endpoint: Endpoint,
    method: Method,
    range: Option<&str>,
    redirects: &mut usize,
) -> io::Result<(Response, Endpoint)> {
    let mut public_only = endpoint.public;
    loop {
        let mut request = endpoint
            .client
            .request(method.clone(), endpoint.url.clone())
            .header(ACCEPT_ENCODING, "identity");
        if let Some(range) = range {
            request = request.header(RANGE, range);
        }
        let response = request.send().await.map_err(network_error)?;
        check_headers(response.headers())?;
        if !response.status().is_redirection() {
            return Ok((response, endpoint));
        }
        if *redirects == 5 || !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Err(invalid("remote redirect limit or unsupported redirect"));
        }
        let location = single_header(response.headers(), LOCATION)?
            .ok_or_else(|| invalid("remote redirect has no location"))?;
        // Validate before WHATWG join can repair unsafe whitespace/escapes.
        if !safe_reference(location) {
            return Err(invalid("invalid remote redirect"));
        }
        let url = if location.starts_with("//") {
            validated_url(&format!("{}:{location}", endpoint.url.scheme()))?
        } else if Url::parse(location).is_ok() {
            validated_url(location)?
        } else {
            endpoint
                .url
                .join(location)
                .map_err(|_| invalid("invalid remote redirect"))?
        };
        let url = validated_url(url.as_str())?;
        drop(response);
        endpoint = Endpoint::resolve(url, public_only).await?;
        public_only |= endpoint.public;
        *redirects += 1;
    }
}

pub(super) fn validated_url(input: &str) -> io::Result<Url> {
    if input.len() > MAX_URL {
        return Err(invalid("remote URL exceeds limit"));
    }
    ManualSource::parse_link(input)
        .ok()
        .and_then(|source| source.http_url().cloned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid HTTP source"))
}

fn safe_reference(input: &str) -> bool {
    !input.is_empty()
        && input.len() <= MAX_URL
        && !input.chars().any(|c| c.is_whitespace() || c.is_control())
        && !input.contains('\\')
        && input.as_bytes().iter().enumerate().all(|(i, byte)| {
            *byte != b'%'
                || input
                    .as_bytes()
                    .get(i + 1..i + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        })
}

// Conservative non-global exclusions, including mapped IPv4 and transition
// networks that could otherwise route to local IPv4 addresses.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 || b == 168))
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113)
                || ip == Ipv4Addr::BROADCAST)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = ip.segments();
            // Only global unicast, excluding special 2001::/23, documentation,
            // 6to4 and documentation 3fff::/20. Other spaces fail closed.
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

#[derive(Default)]
struct Validators {
    etag: Option<HeaderValue>,
    modified: Option<HeaderValue>,
}

impl Validators {
    fn from_headers(headers: &HeaderMap) -> io::Result<Self> {
        let etag = single_header(headers, ETAG)?
            .map(|value| {
                let opaque = value.strip_prefix("W/").unwrap_or(value);
                if opaque.len() > 1024
                    || opaque.len() < 2
                    || !opaque.starts_with('"')
                    || !opaque.ends_with('"')
                    || !opaque.as_bytes()[1..opaque.len() - 1]
                        .iter()
                        .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b))
                {
                    return Err(invalid("invalid remote ETag"));
                }
                HeaderValue::from_str(value).map_err(|_| invalid("invalid remote ETag"))
            })
            .transpose()?;
        let modified = single_header(headers, LAST_MODIFIED)?
            .map(|value| {
                httpdate::parse_http_date(value)
                    .map_err(|_| invalid("invalid remote modification date"))?;
                HeaderValue::from_str(value)
                    .map_err(|_| invalid("invalid remote modification date"))
            })
            .transpose()?;
        Ok(Self { etag, modified })
    }

    fn strong_etag(&self) -> Option<&HeaderValue> {
        self.etag
            .as_ref()
            .filter(|v| !v.as_bytes().starts_with(b"W/"))
    }

    fn can_condition(&self) -> bool {
        self.strong_etag().is_some() || self.modified.is_some()
    }

    fn matches(&self, headers: &HeaderMap) -> io::Result<bool> {
        let other = Self::from_headers(headers)?;
        Ok(self.etag == other.etag && self.modified == other.modified)
    }
}

fn check_headers(headers: &HeaderMap) -> io::Result<()> {
    if headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum::<usize>()
        > MAX_HEADERS
    {
        return Err(invalid("remote headers exceed limit"));
    }
    Ok(())
}

fn single_header(
    headers: &HeaderMap,
    name: reqwest::header::HeaderName,
) -> io::Result<Option<&str>> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|v| v.to_str().map_err(|_| invalid("invalid remote header")))
        .transpose()?;
    if values.next().is_some() {
        return Err(invalid("duplicate remote metadata header"));
    }
    Ok(value)
}

pub(super) fn check_identity(headers: &HeaderMap) -> io::Result<()> {
    if single_header(headers, CONTENT_ENCODING)?
        .is_some_and(|s| !s.eq_ignore_ascii_case("identity"))
    {
        return Err(unsupported("encoded remote content is not seekable"));
    }
    Ok(())
}

fn decimal(input: &str) -> io::Result<u64> {
    if input.is_empty() || !input.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid remote byte count"));
    }
    input
        .parse()
        .map_err(|_| invalid("remote byte count overflows"))
}

pub(super) fn length_header(headers: &HeaderMap) -> io::Result<Option<u64>> {
    single_header(headers, CONTENT_LENGTH)?
        .map(decimal)
        .transpose()
}

fn check_body_length(headers: &HeaderMap, expected: u64) -> io::Result<()> {
    if length_header(headers)?.is_some_and(|len| len != expected) {
        return Err(invalid("remote body length does not match range"));
    }
    Ok(())
}

fn content_range(headers: &HeaderMap) -> io::Result<(u64, u64, u64)> {
    if single_header(headers, CONTENT_TYPE)?.is_some_and(|value| {
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("multipart/byteranges")
    }) {
        return Err(unsupported("multipart remote ranges are not supported"));
    }
    let value = single_header(headers, CONTENT_RANGE)?
        .and_then(|s| s.strip_prefix("bytes "))
        .ok_or_else(|| invalid("missing or invalid remote content range"))?;
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| invalid("invalid remote content range"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| invalid("invalid remote content range"))?;
    let (start, end, total) = (decimal(start)?, decimal(end)?, decimal(total)?);
    if start > end || end >= total {
        return Err(invalid("invalid remote content range bounds"));
    }
    Ok((start, end, total))
}

async fn read_body(response: &mut Response, expected: usize) -> io::Result<Bytes> {
    let mut buffer = BytesMut::with_capacity(expected);
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if chunk.len() > expected - buffer.len() {
            return Err(invalid("remote body exceeds requested range"));
        }
        buffer.extend_from_slice(&chunk);
    }
    if buffer.len() != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated remote body",
        ));
    }
    Ok(buffer.freeze())
}

fn media_type(headers: &HeaderMap) -> Option<String> {
    let raw = single_header(headers, CONTENT_TYPE).ok()??;
    let essence = raw.split(';').next()?.trim();
    let (kind, subtype) = essence.split_once('/')?;
    fn token(s: &str) -> bool {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    }
    (essence.len() <= 127 && token(kind) && token(subtype)).then(|| essence.to_ascii_lowercase())
}

pub(super) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

pub(super) fn network_error(error: reqwest::Error) -> io::Error {
    // Never wrap reqwest::Error: its Display/Debug can include private URLs.
    if error.is_timeout() {
        io::Error::new(io::ErrorKind::TimedOut, "remote operation timed out")
    } else {
        io::Error::other("remote transport failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn public_redirect_cannot_resolve_to_local_addresses() {
        for url in [
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://localhost/",
            "http://10.0.0.1/",
        ] {
            let error = Endpoint::resolve(validated_url(url).unwrap(), true)
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
    }

    #[test]
    fn address_classification_is_conservative() {
        for address in [
            "0.0.0.0",
            "127.1.2.3",
            "100.64.0.1",
            "169.254.169.254",
            "192.168.0.1",
            "198.18.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "64:ff9b::7f00:1",
        ] {
            assert!(!public_ip(address.parse().unwrap()), "{address}");
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_ip(address.parse().unwrap()), "{address}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_bounds_the_entire_operation() {
        let cancel = CancellationToken::new();
        let start = tokio::time::Instant::now();
        let result: io::Result<()> = bounded(&cancel, std::future::pending()).await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(start.elapsed(), DEADLINE);
    }
}
