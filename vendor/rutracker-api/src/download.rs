//! `.torrent` file downloads (`/forum/dl.php`). Requires login.

use reqwest::{Method, header};
use scraper::Html;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::http;
use crate::models::TopicId;
use crate::parse_util::{has_challenge, has_login_form};

/// Per-request cap on `.torrent` file downloads. Real torrents are tens to
/// hundreds of KiB; megatorrents over 4 MiB are vanishingly rare.
const MAX_TORRENT_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) async fn download_torrent(client: &Client, id: TopicId) -> Result<Vec<u8>> {
    let mut url = client.url("/forum/dl.php")?;
    url.query_pairs_mut().append_pair("t", &id.to_string());

    let resp = http::send(client.http(), Method::POST, &url, None).await?;
    read_response(resp).await
}

async fn read_response(resp: reqwest::Response) -> Result<Vec<u8>> {
    let resp = http::check_status(resp).await?;

    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !ct
        .split(';')
        .next()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/x-bittorrent"))
    {
        // A non-torrent response can be a login page, challenge or a changed
        // layout. Only the credential form is evidence of an expired session.
        let html = http::read_html(resp).await?;
        let doc = Html::parse_document(&html);
        if has_challenge(&doc) {
            return Err(Error::parse("download", "unsupported challenge"));
        }
        if has_login_form(&doc) {
            return Err(Error::NotAuthenticated);
        }
        return Err(Error::parse("download", "expected torrent response"));
    }

    let bytes = http::read_bytes_capped(resp, MAX_TORRENT_BYTES).await?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_torrent_responses_require_a_genuine_login_form_for_auth_errors() {
        for (body, is_login) in [
            (include_str!("fixtures/search/login.html"), true),
            (
                "<form id=login-form-full><input name=login_username><input type=password name=login_password></form>",
                true,
            ),
            ("<h1>Checking your browser</h1>", false),
            ("<form id=login-form-full>Challenge</form>", false),
            ("<h1>Unavailable</h1>", false),
            ("", false),
        ] {
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
            let result = read_response(response).await;
            drop(server);
            if is_login {
                assert!(matches!(result, Err(Error::NotAuthenticated)), "{body}");
            } else {
                assert!(matches!(result, Err(Error::Parse { .. })), "{body}");
            }
        }
    }

    #[tokio::test]
    async fn torrent_content_type_is_required_and_parameters_are_allowed() {
        let body = "d4:infodee";
        for content_type in [
            Some("application/x-bittorrent"),
            Some("Application/X-Bittorrent; charset=binary"),
            Some("application/octet-stream"),
            None,
        ] {
            let header = content_type
                .map(|value| format!("Content-Type: {value}\r\n"))
                .unwrap_or_default();
            let wire = format!(
                "HTTP/1.1 200 OK\r\n{header}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
            let result = read_response(response).await;
            drop(server);
            if content_type.is_some_and(|ct| {
                ct.to_ascii_lowercase()
                    .starts_with("application/x-bittorrent")
            }) {
                assert_eq!(result.unwrap(), body.as_bytes());
            } else {
                assert!(matches!(result, Err(Error::Parse { .. })));
            }
        }
    }

    #[tokio::test]
    async fn download_checks_status_before_login_or_torrent_body() {
        for (content_type, body) in [
            ("text/html", include_str!("fixtures/search/login.html")),
            ("application/x-bittorrent", "d4:infodee"),
        ] {
            for status in [401, 403, 429, 503] {
                let wire = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
                let result = read_response(response).await;
                drop(server);
                match status {
                    401 => assert!(matches!(result, Err(Error::NotAuthenticated))),
                    429 => assert!(matches!(result, Err(Error::RateLimited))),
                    _ => assert!(matches!(result, Err(Error::Server(code)) if code == status)),
                }
            }
        }
    }

    #[tokio::test]
    async fn non_torrent_responses_are_bounded_before_parsing() {
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n",
            http::MAX_RESPONSE_BYTES + 1
        );
        let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), read_response(response)).await;
        drop(server);
        assert!(matches!(result, Ok(Err(Error::TooLarge))), "{result:?}");
    }
}
