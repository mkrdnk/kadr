//! Explicit, cancellable metadata and permitted-source discovery.
//! No requests run until a caller invokes search, details, or find_sources.
#![forbid(unsafe_code)]

use kadr_core::{
    catalog::{Episode, Media, MediaKind, SourceCandidate},
    source::ManualSource,
};
use serde_json::Value;
use std::{fmt, time::Duration};
pub use tokio_util::sync::CancellationToken;
use url::Url;

const BODY_LIMIT: usize = 4 * 1024 * 1024;
const SEARCH_LIMIT: usize = 20;
const ARCHIVE: &str = "https://archive.org";
const TVMAZE: &str = "https://api.tvmaze.com";
const TV_LICENSE: &str = "https://creativecommons.org/licenses/by-sa/4.0/";

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// A privacy switch: disabled clients never issue metadata or source requests.
    pub enabled: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Errors intentionally contain no URLs, queries, response bodies, or reqwest errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Disabled,
    Cancelled,
    InvalidQuery,
    InvalidId,
    NotFound,
    Network,
    Timeout,
    RateLimited,
    Http(u16),
    TooLarge,
    InvalidResponse,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("Online catalog is disabled in settings."),
            Self::Cancelled => f.write_str("Catalog request cancelled."),
            Self::InvalidQuery => f.write_str("Enter a search of 1–200 characters."),
            Self::InvalidId => f.write_str("Unsupported catalog identifier."),
            Self::NotFound => f.write_str("The catalog item is unavailable."),
            Self::Network => {
                f.write_str("Catalog connection failed. Check your connection and retry.")
            }
            Self::Timeout => {
                f.write_str("Catalog request timed out. Retry when the service is available.")
            }
            Self::RateLimited => {
                f.write_str("Catalog rate limit reached. Wait a few seconds and retry.")
            }
            Self::Http(status) => write!(f, "Catalog service returned HTTP {status}. Retry later."),
            Self::TooLarge => f.write_str("Catalog response exceeds the safety limit."),
            Self::InvalidResponse => f.write_str("Catalog service returned unsupported data."),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    options: Options,
}

impl Client {
    pub fn new(options: Options) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .user_agent("kadr/0.1 (desktop catalog; explicit user requests)")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            // Metadata APIs do not require redirects; never follow arbitrary remote locations.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self { http, options })
    }

    fn ready(&self, cancel: &CancellationToken) -> Result<(), Error> {
        if !self.options.enabled {
            return Err(Error::Disabled);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Search Archive video titles and TVmaze shows. No recommendations or background crawling.
    /// If one provider fails but the other supplies results, returns those available results.
    pub async fn search(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Media>, Error> {
        self.ready(cancel)?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 200 || query.chars().any(char::is_control) {
            return Err(Error::InvalidQuery);
        }
        let (films, series) = tokio::join!(
            self.search_archive(query, cancel),
            self.search_tvmaze(query, cancel)
        );
        self.ready(cancel)?;
        match (films, series) {
            (Ok(mut films), Ok(series)) => {
                films.extend(series);
                Ok(films)
            }
            (Ok(results), Err(_)) | (Err(_), Ok(results)) if !results.is_empty() => Ok(results),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    pub async fn details(&self, id: &str, cancel: &CancellationToken) -> Result<Media, Error> {
        self.ready(cancel)?;
        match provider_id(id)? {
            ProviderId::Archive(id) => {
                let data = self.archive_item(id, cancel).await?;
                archive_media(&data["metadata"]).ok_or(Error::NotFound)
            }
            ProviderId::Tvmaze(id) => {
                let mut url = endpoint(TVMAZE, &["shows", id])?;
                url.query_pairs_mut().append_pair("embed", "episodes");
                let data = self.json(url, cancel).await?;
                let mut media = tvmaze_media(&data).ok_or(Error::InvalidResponse)?;
                media.episodes = data["_embedded"]["episodes"]
                    .as_array()
                    .ok_or(Error::InvalidResponse)?
                    .iter()
                    .filter_map(tvmaze_episode)
                    .collect();
                media.episodes.sort_by_key(|e| (e.season, e.number));
                Ok(media)
            }
        }
    }

    /// Re-fetch rights metadata immediately before offering files. Never trusts a
    /// caller-supplied Media license. No title-based cross-provider source matching.
    pub async fn find_sources(
        &self,
        media: &Media,
        episode: Option<&Episode>,
        cancel: &CancellationToken,
    ) -> Result<Vec<SourceCandidate>, Error> {
        self.ready(cancel)?;
        match provider_id(&media.id)? {
            ProviderId::Tvmaze(_) => Ok(Vec::new()),
            ProviderId::Archive(id) => {
                // Archive items are videos, not inferred TV seasons. Do not attach a
                // whole film to a selected episode, even if callers mix their models.
                if episode.is_some() {
                    return Ok(Vec::new());
                }
                let data = self.archive_item(id, cancel).await?;
                Ok(archive_sources(id, &data))
            }
        }
    }

    async fn search_archive(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Media>, Error> {
        let mut url = endpoint(ARCHIVE, &["advancedsearch.php"])?;
        // Quote user input as a literal Lucene phrase, never executable search syntax.
        let phrase = query.replace('\\', "\\\\").replace('"', "\\\"");
        let q = format!("mediatype:movies AND title:(\"{phrase}\")");
        url.query_pairs_mut()
            .append_pair("q", &q)
            .append_pair("output", "json")
            .append_pair("rows", &SEARCH_LIMIT.to_string())
            .append_pair(
                "fl[]",
                "identifier,title,date,year,description,licenseurl,creator",
            );
        let data = self.json(url, cancel).await?;
        let docs = data["response"]["docs"]
            .as_array()
            .ok_or(Error::InvalidResponse)?;
        Ok(docs
            .iter()
            .take(SEARCH_LIMIT)
            .filter_map(archive_media)
            .collect())
    }

    async fn search_tvmaze(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Media>, Error> {
        let mut url = endpoint(TVMAZE, &["search", "shows"])?;
        url.query_pairs_mut().append_pair("q", query);
        let data = self.json(url, cancel).await?;
        let shows = data.as_array().ok_or(Error::InvalidResponse)?;
        Ok(shows
            .iter()
            .take(SEARCH_LIMIT)
            .filter_map(|v| tvmaze_media(&v["show"]))
            .collect())
    }

    async fn archive_item(&self, id: &str, cancel: &CancellationToken) -> Result<Value, Error> {
        let data = self
            .json(endpoint(ARCHIVE, &["metadata", id])?, cancel)
            .await?;
        if text(&data["metadata"]["identifier"]).as_deref() != Some(id)
            || text(&data["metadata"]["mediatype"]).as_deref() != Some("movies")
        {
            return Err(Error::NotFound);
        }
        Ok(data)
    }

    async fn json(&self, url: Url, cancel: &CancellationToken) -> Result<Value, Error> {
        self.ready(cancel)?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(20), self.read_json(url)) =>
                result.map_err(|_| Error::Timeout)?,
        }
    }

    async fn read_json(&self, url: Url) -> Result<Value, Error> {
        let mut response = self.http.get(url).send().await.map_err(network_error)?;
        match response.status().as_u16() {
            200 => {}
            404 => return Err(Error::NotFound),
            429 => return Err(Error::RateLimited),
            status => return Err(Error::Http(status)),
        }
        if response
            .content_length()
            .is_some_and(|size| size > BODY_LIMIT as u64)
        {
            return Err(Error::TooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if chunk.len() > BODY_LIMIT - body.len() {
                return Err(Error::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)
    }
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

enum ProviderId<'a> {
    Archive(&'a str),
    Tvmaze(&'a str),
}

fn provider_id(id: &str) -> Result<ProviderId<'_>, Error> {
    match id.split_once(':') {
        Some(("archive", id)) if archive_id(id) => Ok(ProviderId::Archive(id)),
        Some(("tvmaze", id))
            if !id.is_empty()
                && id.len() <= 12
                && id.bytes().all(|b| b.is_ascii_digit())
                && !id.starts_with('0') =>
        {
            Ok(ProviderId::Tvmaze(id))
        }
        _ => Err(Error::InvalidId),
    }
}

fn archive_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn endpoint(base: &str, segments: &[&str]) -> Result<Url, Error> {
    let mut url = Url::parse(base).map_err(|_| Error::InvalidResponse)?;
    url.path_segments_mut()
        .map_err(|_| Error::InvalidResponse)?
        .extend(segments);
    Ok(url)
}

/// Archive fields can be strings, numbers, or arrays of strings.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(values) => values.iter().find_map(text),
        _ => None,
    }
}

/// Bounded literal display text; never load remote markup, scripts or embedded resources.
fn plain(value: &Value, limit: usize) -> String {
    let raw = text(value).unwrap_or_default();
    let mut inside_tag = false;
    let mut tag = String::new();
    let mut output = String::new();
    for c in raw.chars().take(64 * 1024) {
        match c {
            '<' => {
                inside_tag = true;
                tag.clear();
            }
            '>' if inside_tag => {
                inside_tag = false;
                // Preserve block boundaries without inserting spaces before punctuation
                // after inline tags (e.g. "<b>detective</b>.").
                let name = tag.split_whitespace().next().unwrap_or("");
                if matches!(name, "br" | "br/" | "/p" | "/div" | "/li") {
                    output.push(' ');
                }
            }
            _ if inside_tag && tag.len() < 32 => tag.push(c.to_ascii_lowercase()),
            _ if !inside_tag && (!c.is_control() || c.is_whitespace()) => output.push(c),
            _ => {}
        }
    }
    let output = output
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn year(value: &Value) -> Option<u16> {
    let raw = text(value)?;
    let year = raw.get(..4)?.parse::<u16>().ok()?;
    (year > 1800 && year < 2200).then_some(year)
}

fn archive_media(data: &Value) -> Option<Media> {
    let id = text(&data["identifier"])?;
    if !archive_id(&id) {
        return None;
    }
    let title = plain(&data["title"], 500);
    if title.is_empty() {
        return None;
    }
    let creator = plain(&data["creator"], 500);
    Some(Media {
        id: format!("archive:{id}"),
        title,
        original_title: None,
        year: year(&data["year"]).or_else(|| year(&data["date"])),
        description: plain(&data["description"], 16_000),
        poster_url: Some(format!("{ARCHIVE}/services/img/{id}")),
        kind: MediaKind::Movie,
        episodes: Vec::new(),
        attribution: if creator.is_empty() {
            "Internet Archive · uploader-provided metadata".into()
        } else {
            format!("Internet Archive · {creator} · uploader-provided metadata")
        },
        license: text(&data["licenseurl"]).map(|s| s.chars().take(500).collect()),
        source_page: format!("{ARCHIVE}/details/{id}"),
    })
}

fn tvmaze_media(data: &Value) -> Option<Media> {
    let id = data["id"].as_u64()?;
    if id == 0 {
        return None;
    }
    let title = plain(&data["name"], 500);
    if title.is_empty() {
        return None;
    }
    Some(Media {
        id: format!("tvmaze:{id}"),
        title,
        original_title: None,
        year: year(&data["premiered"]),
        description: plain(&data["summary"], 16_000),
        poster_url: text(&data["image"]["medium"]).filter(|s| {
            Url::parse(s).is_ok_and(|u| {
                u.scheme() == "https"
                    && u.host_str() == Some("static.tvmaze.com")
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.port().is_none()
            })
        }),
        kind: MediaKind::Series,
        episodes: Vec::new(),
        attribution: "TVmaze · CC BY-SA · https://www.tvmaze.com/api".into(),
        license: Some(TV_LICENSE.into()),
        source_page: format!("https://www.tvmaze.com/shows/{id}"),
    })
}

fn tvmaze_episode(data: &Value) -> Option<Episode> {
    Some(Episode {
        season: u16::try_from(data["season"].as_u64()?).ok()?,
        number: u16::try_from(data["number"].as_u64()?).ok()?,
        title: plain(&data["name"], 500),
        description: plain(&data["summary"], 8_000),
    })
}

/// Only explicit, exact CC BY / BY-SA and public-domain URLs. No substring
/// matching or free-text "public domain" claims; no NC/ND ambiguity by default.
fn permitted_license(value: &Value) -> Option<String> {
    // A repeated or conflicting license field is ambiguous: fail closed.
    let raw = value.as_str()?;
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str() != Some("creativecommons.org")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path();
    let allowed = matches!(path, "/publicdomain/zero/1.0/" | "/publicdomain/mark/1.0/")
        || ["by", "by-sa"].iter().any(|kind| {
            ["1.0", "2.0", "2.5", "3.0", "4.0"]
                .iter()
                .any(|version| path == format!("/licenses/{kind}/{version}/"))
        });
    allowed.then(|| format!("https://creativecommons.org{path}"))
}

fn restricted(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) if s == "false" || s == "0" => false,
        Value::Number(n) if n.as_u64() == Some(0) => false,
        _ => true,
    }
}

fn archive_sources(id: &str, data: &Value) -> Vec<SourceCandidate> {
    let metadata = &data["metadata"];
    if restricted(&data["is_dark"])
        || restricted(&metadata["access-restricted-item"])
        || text(&metadata["identifier"]).as_deref() != Some(id)
        || text(&metadata["mediatype"]).as_deref() != Some("movies")
    {
        return Vec::new();
    }
    let Some(license) = permitted_license(&metadata["licenseurl"]) else {
        return Vec::new();
    };
    let Some(media) = archive_media(metadata) else {
        return Vec::new();
    };
    let Some(files) = data["files"].as_array() else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(|file| {
            if restricted(&file["private"]) {
                return None;
            }
            let name = file["name"].as_str()?;
            if name.len() > 1024
                || name.chars().any(char::is_control)
                || name.contains('\\')
                || name
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return None;
            }
            let lower = name.to_ascii_lowercase();
            if ![".mp4", ".m4v", ".webm", ".ogv"]
                .iter()
                .any(|ext| lower.ends_with(ext))
            {
                return None;
            }
            // Honor any explicit file-level license: conflicting/unknown claims deny.
            if !file["licenseurl"].is_null()
                && permitted_license(&file["licenseurl"]).as_ref() != Some(&license)
            {
                return None;
            }
            let mut url = endpoint(ARCHIVE, &["download", id]).ok()?;
            url.path_segments_mut().ok()?.extend(name.split('/'));
            let url = url.to_string();
            ManualSource::parse_link(&url).ok()?;
            Some(SourceCandidate {
                url,
                title: plain(&Value::String(name.to_owned()), 500),
                quality: text(&file["format"]).map(|s| s.chars().take(100).collect()),
                language: text(&metadata["language"]).map(|s| s.chars().take(100).collect()),
                license: license.clone(),
                origin: format!(
                    "{} · {} · uploader-declared license; verify rights locally",
                    media.attribution, media.source_page
                ),
            })
        })
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests;
