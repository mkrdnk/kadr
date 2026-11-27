//! Internal HTTP helpers: Windows-1251 encoding and bounded response reading.

use encoding_rs::WINDOWS_1251;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::{Method, Response, StatusCode};
use url::Url;

use crate::error::{Error, Result};

/// Hard ceiling on response bodies that callers consume into RAM.
///
/// rutracker pages are at most a few hundred KiB; `.torrent` files
/// (metadata-only) almost always stay under 1 MiB. The 4 MiB cap defends
/// against accidentally pulling a malicious or malformed response.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Characters that must be percent-encoded in form values / query strings.
const FORM_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'&')
    .add(b'=')
    .add(b'+')
    .add(b'%');

/// Percent-encode `s` using its **Windows-1251** byte representation.
///
/// Rutracker's forms (login, search) treat form data as cp1251; sending UTF-8
/// percent-encoded values garbles Cyrillic queries.
pub(crate) fn cp1251_percent_encode(s: &str) -> Result<String> {
    let (encoded, _, invalid) = WINDOWS_1251.encode(s);
    if invalid {
        return Err(Error::InvalidArgument(
            "text cannot be encoded as Windows-1251".into(),
        ));
    }
    let mut out = String::with_capacity(encoded.len() * 3);
    for &byte in encoded.iter() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            // Writing to a String is infallible.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    Ok(out)
}

/// Build an `application/x-www-form-urlencoded` body where field names are
/// ASCII (safe) and values are encoded via [`cp1251_percent_encode`].
pub(crate) fn cp1251_form_body<I, K, V>(fields: I) -> Result<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut out = String::new();
    for (i, (name, value)) in fields.into_iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&utf8_percent_encode(name.as_ref(), FORM_ENCODE_SET).to_string());
        out.push('=');
        out.push_str(&cp1251_percent_encode(value.as_ref())?);
    }
    Ok(out)
}

/// Decode a response body that *might* be Windows-1251.
pub(crate) fn decode_body(body: &[u8], content_type: Option<&str>) -> String {
    let charset = content_type
        .and_then(|ct| {
            ct.split(';')
                .map(str::trim)
                .find_map(|p| p.strip_prefix("charset="))
        })
        .map(|s| s.trim_matches('"').to_ascii_lowercase());

    let encoding = match charset.as_deref() {
        Some("utf-8") | Some("utf8") => encoding_rs::UTF_8,
        Some("windows-1251") | Some("cp1251") | None => WINDOWS_1251,
        Some(other) => encoding_rs::Encoding::for_label(other.as_bytes()).unwrap_or(WINDOWS_1251),
    };

    let (decoded, _, _) = encoding.decode(body);
    decoded.into_owned()
}

/// Map a response status code to a domain-specific error, *consuming* the
/// response. 2xx returns `Ok(response)` unchanged.
pub(crate) async fn check_status(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(Error::RateLimited);
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(Error::NotAuthenticated);
    }
    Err(Error::Server(status.as_u16()))
}

/// Issue an HTTP request to the single authorized HTTPS origin.
pub(crate) async fn send(
    http: &reqwest::Client,
    method: Method,
    url: &Url,
    body: Option<Body<'_>>,
) -> Result<Response> {
    crate::client::builder::check_url(url)?;
    let mut req = http.request(method.clone(), url.clone());
    if let Some(b) = body {
        match b {
            Body::Form(s) => {
                req = req
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(s.to_owned());
            }
        }
    }
    let resp = req.send().await?;
    Ok(resp)
}

/// Body variants supported by [`send`].
#[derive(Debug)]
pub(crate) enum Body<'a> {
    /// Pre-encoded `application/x-www-form-urlencoded` payload.
    Form(&'a str),
}

/// Read up to `max_bytes` from a response body, rejecting larger ones early.
///
/// If the server advertises a `Content-Length` greater than `max_bytes`, the
/// request is rejected before any body bytes are read.
pub(crate) async fn read_bytes_capped(mut resp: Response, max_bytes: u64) -> Result<Vec<u8>> {
    if let Some(len) = resp.content_length() {
        if len > max_bytes {
            return Err(Error::TooLarge);
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if chunk.len() as u64 > max_bytes.saturating_sub(bytes.len() as u64) {
            return Err(Error::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Read a response body and decode it as text using the response's charset
/// (with cp1251 as fallback). Caps body at [`MAX_RESPONSE_BYTES`].
pub(crate) async fn read_html(resp: Response) -> Result<String> {
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);
    let bytes = read_bytes_capped(resp, MAX_RESPONSE_BYTES).await?;
    Ok(decode_body(&bytes, content_type.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_unauthorized_is_an_authentication_status() {
        for status in [200, 204, 401, 403, 429, 503] {
            // Even success-like or login-like content cannot override status.
            let body = "<span id=logged-in-username>user</span><form id=login-form-full>\
                        <input name=login_username><input name=login_password type=password></form>";
            let wire = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
            let result = check_status(response).await;
            drop(server);
            match status {
                200 | 204 => assert_eq!(result.unwrap().status().as_u16(), status),
                401 => assert!(matches!(result, Err(Error::NotAuthenticated))),
                429 => assert!(matches!(result, Err(Error::RateLimited))),
                _ => assert!(matches!(result, Err(Error::Server(code)) if code == status)),
            }
        }
    }

    #[test]
    fn cp1251_encodes_cyrillic() {
        // "Вход" — Cyrillic for "Login", used as the rutracker login button value.
        assert_eq!(cp1251_percent_encode("Вход").unwrap(), "%C2%F5%EE%E4");
    }

    #[test]
    fn cp1251_passes_ascii() {
        assert_eq!(cp1251_percent_encode("user_42").unwrap(), "user_42");
        assert_eq!(cp1251_percent_encode("a-b.c~d").unwrap(), "a-b.c~d");
    }

    #[test]
    fn cp1251_rejects_unrepresentable_text_without_replacement() {
        assert!(matches!(
            cp1251_percent_encode("東京"),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            cp1251_form_body([("nm", "🦀")]),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn cp1251_form_body_combines() {
        let body = cp1251_form_body([("login_username", "user"), ("login", "Вход")]).unwrap();
        assert_eq!(body, "login_username=user&login=%C2%F5%EE%E4");
    }

    #[test]
    fn decode_body_uses_cp1251_by_default() {
        let bytes: &[u8] = &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]; // "Привет"
        assert_eq!(decode_body(bytes, None), "Привет");
    }

    #[test]
    fn decode_body_respects_utf8_header() {
        let bytes = "Привет".as_bytes();
        assert_eq!(
            decode_body(bytes, Some("text/html; charset=utf-8")),
            "Привет"
        );
    }
}
