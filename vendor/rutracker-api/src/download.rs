//! `.torrent` file downloads (`/forum/dl.php`). Requires login.

use reqwest::{Method, header};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::http;
use crate::models::TopicId;

/// Per-request cap on `.torrent` file downloads. Real torrents are tens to
/// hundreds of KiB; megatorrents over 4 MiB are vanishingly rare.
const MAX_TORRENT_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) async fn download_torrent(client: &Client, id: TopicId) -> Result<Vec<u8>> {
    let mut url = client.url("/forum/dl.php")?;
    url.query_pairs_mut().append_pair("t", &id.to_string());

    let resp = http::send(client.http(), Method::POST, &url, None).await?;
    let resp = http::check_status(resp).await?;

    // Rutracker quietly returns the login HTML page for unauthenticated
    // requests instead of a 401 — detect via Content-Type.
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
        return Err(Error::NotAuthenticated);
    }

    let bytes = http::read_bytes_capped(resp, MAX_TORRENT_BYTES).await?;
    Ok(bytes)
}
