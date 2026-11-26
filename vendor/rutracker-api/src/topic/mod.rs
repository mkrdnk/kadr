//! Fetch and parse a single topic page (`/forum/viewtopic.php`).

pub(crate) mod parser;

use chrono::{DateTime, Utc};
use reqwest::Method;

use crate::client::Client;
use crate::error::Result;
use crate::http;
use crate::models::{InfoHash, TopicId, TopicState};

/// Detailed information about a single torrent / forum topic.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Topic {
    /// Topic id.
    pub id: TopicId,
    /// Page title (e.g. `"[amd64] Ubuntu 24.04 LTS Desktop"`).
    pub title: String,
    /// Original uploader's display name, when available.
    pub author: Option<String>,
    /// Forum breadcrumb path (e.g. `["Linux", "Дистрибутивы", "Ubuntu"]`).
    pub category_path: Vec<String>,
    /// Torrent size in bytes (best-effort; falls back to `0` when unparseable).
    pub size: u64,
    /// SHA-1 info-hash, when present in the rendered page.
    pub info_hash: Option<InfoHash>,
    /// Magnet URI, when present in the rendered page.
    pub magnet: Option<String>,
    /// Topic registration timestamp (UTC), when extractable.
    pub registered: Option<DateTime<Utc>>,
    /// Approval / moderation state. `None` when the page does not expose an
    /// explicit state icon.
    pub state: Option<TopicState>,
    /// Raw HTML of the original post body. Useful for callers that want to
    /// render the description; not sanitised by this crate.
    pub description_html: String,
    /// Canonical URL to the topic page.
    pub url: String,
}

pub(crate) async fn fetch(client: &Client, id: TopicId) -> Result<Topic> {
    let mut url = client.url("/forum/viewtopic.php")?;
    url.query_pairs_mut().append_pair("t", &id.to_string());

    let resp = http::send(client.http(), Method::GET, &url, None).await?;
    let resp = http::check_status(resp).await?;
    let html = http::read_html(resp).await?;

    let topic = parser::parse(&html, id, url.as_str())?;
    Ok(topic)
}
