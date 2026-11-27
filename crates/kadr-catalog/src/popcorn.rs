//! Independent Popcorn and YTS indexes with fixed, ordered mirror sets. Requests
//! use an IMDb ID; the episode number is matched locally against show details.
//! Offers and complete magnet URIs are memory-only: callers must not persist
//! them, log them, or treat provider metadata as a rights grant.

use std::{fmt, time::Duration};

use kadr_core::{
    catalog::MediaKind,
    sources::{Locator, MediaQuery, MediaSource, match_source},
};
use serde_json::Value;
use url::Url;

use crate::{CancellationToken, rutracker, torznab::movie_release_metadata};

pub const ID: &str = "popcorn";
pub const YTS_ID: &str = "yts";
mod episode;
mod yts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Index {
    Popcorn,
    Yts,
}

impl Index {
    pub fn id(self) -> &'static str {
        match self {
            Self::Popcorn => ID,
            Self::Yts => YTS_ID,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Popcorn => "Popcorn API",
            Self::Yts => "YTS",
        }
    }

    fn hosts(self) -> &'static [&'static str] {
        match self {
            Self::Popcorn => &["fusme.link", "jfper.link", "uxert.link", "yrkde.link"],
            Self::Yts => &["yts.lt", "yts.gg"],
        }
    }

    pub fn supports(self, query: &MediaQuery) -> bool {
        supports(query) && (self == Self::Popcorn || query.kind == MediaKind::Movie)
    }

    pub fn accepts(self, source: &MediaSource) -> bool {
        match self {
            Self::Popcorn => accepts(source),
            Self::Yts => yts::accepts(source),
        }
    }

    fn url(self, host: &str, query: &MediaQuery) -> Result<Url, Error> {
        let imdb = query
            .external
            .imdb_id
            .as_deref()
            .ok_or(Error::InvalidQuery)?;
        let mut url = Url::parse(&format!("https://{host}/")).map_err(|_| Error::InvalidQuery)?;
        match self {
            Self::Popcorn => {
                let path = match query.episode {
                    Some(_) => format!("/show/{imdb}"),
                    None => format!("/movie/{imdb}/torrents"),
                };
                url.set_path(&path);
                url.set_query(Some("contentLocale=ru&locale=ru"));
            }
            Self::Yts => {
                url.set_path("/api/v2/movie_details.json");
                url.query_pairs_mut().append_pair("imdb_id", imdb);
            }
        }
        Ok(url)
    }
}
const TIMEOUT: Duration = Duration::from_secs(20);
const BODY_LIMIT: usize = 4 * 1024 * 1024;
const RESULT_LIMIT: usize = 1000;

/// Static, redacted errors: no URLs, proxy details, bodies or nested errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidProxy,
    InvalidQuery,
    Cancelled,
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
            Self::InvalidProxy => f.write_str("Invalid Popcorn proxy endpoint."),
            Self::InvalidQuery => f.write_str("Invalid Popcorn request."),
            Self::Cancelled => f.write_str("Popcorn request cancelled."),
            Self::Network => f.write_str("Popcorn connection failed."),
            Self::Timeout => f.write_str("Popcorn request timed out."),
            Self::RateLimited => f.write_str("Popcorn rate limit reached; retry later."),
            Self::Http(status) => write!(f, "Popcorn returned HTTP {status}."),
            Self::TooLarge => f.write_str("Popcorn response exceeds the safety limit."),
            Self::InvalidResponse => f.write_str("Popcorn returned unsupported data."),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    #[cfg(test)]
    fixture_origins: Option<Vec<Url>>,
    #[cfg(test)]
    timeout: Duration,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PopcornClient").finish_non_exhaustive()
    }
}

impl Client {
    /// Offline. An explicit unauthenticated proxy replaces system defaults with
    /// no direct fallback. No cookie jar, authentication, redirects or retries.
    pub fn new(proxy: Option<&str>) -> Result<Self, Error> {
        let mut http = http_builder();
        if let Some(proxy) = proxy {
            if !kadr_core::proxy::is_valid_endpoint(proxy) {
                return Err(Error::InvalidProxy);
            }
            http = http
                .no_proxy()
                .proxy(reqwest::Proxy::all(proxy).map_err(|_| Error::InvalidProxy)?);
        }
        Ok(Self {
            http: http.build().map_err(|_| Error::Network)?,
            #[cfg(test)]
            fixture_origins: None,
            #[cfg(test)]
            timeout: TIMEOUT,
        })
    }

    /// Unsupported queries and HTTP 404 yield no offers. Only the IMDb ID and
    /// fixed locale parameters leave the process. Returned URIs are memory-only.
    pub async fn search(
        &self,
        query: &MediaQuery,
        fetched_at: i64,
        cancel: &CancellationToken,
    ) -> Result<Vec<MediaSource>, Error> {
        self.search_index(Index::Popcorn, query, fetched_at, cancel)
            .await
    }

    pub async fn search_index(
        &self,
        index: Index,
        query: &MediaQuery,
        fetched_at: i64,
        cancel: &CancellationToken,
    ) -> Result<Vec<MediaSource>, Error> {
        ready(cancel)?;
        if fetched_at < 0 {
            return Err(Error::InvalidQuery);
        }
        if !index.supports(query) {
            return Ok(Vec::new());
        }
        #[cfg(test)]
        let timeout = self.timeout;
        #[cfg(not(test))]
        let timeout = TIMEOUT;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(timeout, async {
                self.search_mirrors(index, query, fetched_at, cancel).await
            }) => result.map_err(|_| Error::Timeout)?,
        }
    }

    async fn search_mirrors(
        &self,
        index: Index,
        query: &MediaQuery,
        fetched_at: i64,
        cancel: &CancellationToken,
    ) -> Result<Vec<MediaSource>, Error> {
        let mut last_error = Error::Network;
        let mut used_canonical = false;
        #[cfg(test)]
        let mut fixture_origins = self.fixture_origins.as_ref().map(|v| v.iter());
        for host in index.hosts() {
            ready(cancel)?;
            let url = index.url(host, query)?;
            #[cfg(test)]
            let url = if let Some(origins) = &mut fixture_origins {
                let Some(mut local) = origins.next().cloned() else {
                    break;
                };
                local.set_path(url.path());
                local.set_query(url.query());
                local
            } else {
                url
            };
            let attempt = async {
                let mut response = self
                    .http
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)?;
                if index == Index::Yts && response.status().is_redirection() {
                    let target = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|s| s.to_str().ok());
                    if let Some(canonical) = canonical_yts_redirect(&url, target) {
                        if used_canonical {
                            return Err(last_error);
                        }
                        used_canonical = true;
                        response = self
                            .http
                            .get(canonical)
                            .send()
                            .await
                            .map_err(network_error)?;
                    }
                }
                read_body(response).await
            };
            let outcome = tokio::time::timeout(Duration::from_secs(5), attempt)
                .await
                .unwrap_or(Err(Error::Timeout));
            match outcome {
                Err(err @ (Error::Network | Error::Timeout | Error::Http(500..=599))) => {
                    last_error = err
                }
                Err(err) => return Err(err),
                Ok(None) => return Ok(Vec::new()),
                Ok(Some(body)) => {
                    ready(cancel)?;
                    let rows = match (index, query.kind) {
                        (Index::Yts, _) => yts::parse(&body, query, fetched_at),
                        (Index::Popcorn, MediaKind::Series) => {
                            episode::parse(&body, query, fetched_at)
                        }
                        (Index::Popcorn, MediaKind::Movie) => parse(&body, query, fetched_at),
                    }?;
                    ready(cancel)?;
                    return Ok(rows);
                }
            }
        }
        Err(last_error)
    }
}

// YTS currently redirects both of its public names to the same canonical JSON
// service. Do not use the redirect URL as authority for a new host or path.
fn canonical_yts_redirect(request: &Url, location: Option<&str>) -> Option<Url> {
    if !Index::Yts.hosts().contains(&request.host_str()?) {
        return None;
    }
    let mut expected = request.clone();
    expected.set_host(Some("yts.pw")).ok()?;
    (location? == expected.as_str()).then_some(expected)
}

fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent("Kadr/0.1 (explicit read-only Popcorn requests)")
        .connect_timeout(Duration::from_secs(5))
        .timeout(TIMEOUT)
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
}

/// Never search a shelf, whole series, or unnumbered episode.
pub fn supports(query: &MediaQuery) -> bool {
    (query.kind == MediaKind::Series) == query.episode.is_some()
        && query.external.clone().normalized().imdb_id.is_some()
        && query.year.is_none_or(|year| (1800..2200).contains(&year))
}

fn ready(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

async fn read_body(mut response: reqwest::Response) -> Result<Option<Vec<u8>>, Error> {
    match response.status().as_u16() {
        200 => {}
        404 => return Ok(None),
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
    Ok(Some(body))
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

fn display_text(raw: &str, limit: usize) -> bool {
    !raw.trim().is_empty()
        && raw.chars().count() <= limit
        && !raw.chars().any(char::is_control)
        && !raw.contains(['?', '='])
        && !["//", "magnet:", "www."]
            .iter()
            .any(|marker| raw.to_ascii_lowercase().contains(marker))
}

fn upstream(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let origin = format!("{ID}:{raw}");
    if upstream_label(&origin).is_some() {
        Some(origin)
    } else {
        None
    }
}

/// Recognize only canonical, short display provenance, never a page URL.
pub fn upstream_label(origin: &str) -> Option<&str> {
    let raw = origin.strip_prefix("popcorn:")?;
    (!raw.is_empty()
        && raw == raw.trim()
        && raw.len() <= 64
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_ .".contains(&byte))
        && !raw.contains(".."))
    .then_some(raw)
}

// Decode the observed API's double HTML separators, not percent-encoded tracker
// queries. Retain original query segments verbatim except unapproved trackers.
fn magnet(raw: &str) -> Option<String> {
    if raw.len() > 16 * 1024 || raw.chars().any(char::is_control) {
        return None;
    }
    let decoded = raw.replace("&amp;", "&").replace("&amp;", "&");
    let url = Url::parse(&decoded).ok()?;
    if url.scheme() != "magnet"
        || url.host().is_some()
        || !url.path().is_empty()
        || url.fragment().is_some()
    {
        return None;
    }
    let (prefix, query) = decoded.split_once('?')?;
    let mut segments = Vec::new();
    for segment in query.split('&') {
        let (key, value) = url::form_urlencoded::parse(segment.as_bytes()).next()?;
        match key.as_ref() {
            "xt" if value.starts_with("urn:btih:") || value.starts_with("urn:btmh:") => {}
            "dn" if display_text(&value, 2000) => {}
            "xl" if value.parse::<u64>().is_ok() => {}
            "tr" => {
                if !safe_tracker(&value) {
                    continue;
                }
            }
            // Web seeds, exact sources, explicit peers and unknown URL
            // entrypoints are not part of this capability.
            _ => return None,
        }
        segments.push(segment);
    }
    let result = format!("{prefix}?{}", segments.join("&"));
    Locator::Magnet(result.clone()).validate().ok()?;
    // Playback currently supports v1 and hybrid torrents, not pure v2.
    if !Url::parse(&result)
        .ok()?
        .query_pairs()
        .any(|(key, value)| key == "xt" && value.starts_with("urn:btih:"))
    {
        return None;
    }
    Some(result)
}

fn safe_magnet(raw: &str) -> bool {
    if Locator::Magnet(raw.to_owned()).validate().is_err() {
        return false;
    }
    magnet(raw).as_deref() == Some(raw)
}

fn safe_tracker(raw: &str) -> bool {
    if raw.chars().any(char::is_control) {
        return false;
    }
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some_and(|q| q != "magnet")
    {
        return false;
    }
    // Audited public announce endpoints. DNS/IP heuristics cannot make an
    // arbitrary provider-controlled hostname safe against local resolution.
    (url.scheme() == "http"
        && url.port_or_known_default() == Some(80)
        && matches!(
            url.host_str(),
            Some("bt.t-ru.org" | "bt2.t-ru.org" | "bt3.t-ru.org" | "bt4.t-ru.org")
        )
        && url.path() == "/ann")
        || (url.scheme() == "udp"
            && url.host_str() == Some("opentor.net")
            && url.port() == Some(6969)
            && url.path().is_empty())
}

fn parse(body: &[u8], query: &MediaQuery, fetched_at: i64) -> Result<Vec<MediaSource>, Error> {
    if body.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    let data: Value = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    let rows = data.as_array().ok_or(Error::InvalidResponse)?;
    if rows.len() > RESULT_LIMIT {
        return Err(Error::TooLarge);
    }
    Ok(rows
        .iter()
        .filter_map(|row| source(row, query, fetched_at))
        .collect())
}

fn source(row: &Value, query: &MediaQuery, fetched_at: i64) -> Option<MediaSource> {
    let raw = row.get("title")?.as_str()?;
    if !display_text(raw, 2000) || non_movie(raw) {
        return None;
    }
    // Retain the particular catalog title supported by the raw release. A
    // restored offer must not require the request's discarded aliases to pass.
    let (title, confidence) = std::iter::once(query.title.as_str())
        .chain(query.original_title.as_deref())
        .chain(
            query
                .external
                .alternative_titles
                .iter()
                .take(16)
                .map(String::as_str),
        )
        .filter(|title| display_text(title, 2000))
        .find_map(|title| {
            let mut identity = query.clone();
            identity.title = title.into();
            identity.original_title = None;
            identity.external.alternative_titles.clear();
            rutracker::classify_release(raw, &identity).map(|confidence| (title, confidence))
        })?;
    let (_, year) = movie_release_metadata(raw)?;
    // Optional future explicit identity/scope fields cannot contradict the
    // endpoint and raw title. Current live rows contain none of these.
    for field in ["imdb", "imdb_id"] {
        if row
            .get(field)
            .is_some_and(|id| id.as_str() != query.external.imdb_id.as_deref())
        {
            return None;
        }
    }
    if row
        .get("kind")
        .is_some_and(|kind| kind.as_str() != Some("movie"))
        || ["season", "episode", "pack"].iter().any(|key| {
            row.get(key)
                .is_some_and(|value| !value.is_null() && value != false)
        })
    {
        return None;
    }
    let declared_year = match row.get("year") {
        None | Some(Value::Null) => None,
        Some(value) => Some(u16::try_from(unsigned(value)?).ok()?),
    };
    if declared_year.zip(year).is_some_and(|(a, b)| a != b) {
        return None;
    }
    let uri = magnet(row.get("url")?.as_str()?)?;
    let mut source = MediaSource::new(ID, Locator::Magnet(uri), title, MediaKind::Movie).ok()?;
    source.release_title = Some(raw.into());
    source.year = declared_year.or(year);
    source.external.imdb_id = query.external.imdb_id.clone();
    source.quality = row
        .get("quality")
        .and_then(Value::as_str)
        .filter(|text| display_text(text, 100) && !text.eq_ignore_ascii_case("unknown"))
        .map(str::to_owned);
    source.resolution = source.quality.as_deref().and_then(|q| match q {
        "480p" => Some(480),
        "576p" => Some(576),
        "720p" => Some(720),
        "1080p" | "1080i" => Some(1080),
        "2160p" => Some(2160),
        _ => None,
    });
    source.seeds = row
        .get("seed")
        .and_then(unsigned)
        .and_then(|n| u32::try_from(n).ok());
    source.peers = row
        .get("peer")
        .and_then(unsigned)
        .and_then(|n| u32::try_from(n).ok());
    source.size = row.get("size").and_then(unsigned);
    source.fetched_at = fetched_at;
    // The upstream page URL and human-formatted size are intentionally unused.
    if let Some(origin) = row
        .get("provider")
        .and_then(Value::as_str)
        .and_then(upstream)
    {
        source.origins.push(origin);
    }
    let mut source = match_source(query, source)?;
    source.possible_match = confidence == rutracker::ReleaseMatch::Possible;
    accepts(&source).then_some(source)
}

fn unsigned(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            text.parse().ok()
        }
        _ => None,
    }
}

fn non_movie(raw: &str) -> bool {
    let metadata = raw.find(['[', '(']).map_or(raw, |index| &raw[index..]);
    let words: Vec<_> = metadata
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "soundtrack"
                | "ost"
                | "flac"
                | "mp3"
                | "audiobook"
                | "аудиокнига"
                | "саундтрек"
                | "аудиоспектакль"
                | "трейлер"
                | "trailer"
                | "аудиодорожка"
                | "аудиодорожки"
                | "сериал"
                | "сериалы"
        )
    }) || words.windows(2).any(|pair| {
        matches!(
            (pair[0].as_str(), pair[1].as_str()),
            ("audio", "only" | "track" | "tracks") | ("только", "аудио") | ("tv", "series")
        )
    })
}

/// Recheck the movie/magnet capability before use. This is not authorization to
/// persist the source. Other provider origins can be added by core deduplication.
pub fn accepts(source: &MediaSource) -> bool {
    if source.kind == MediaKind::Series {
        return episode::accepts(source);
    }
    let Locator::Magnet(uri) = &source.locator else {
        return false;
    };
    let Some(raw) = source.release_title.as_deref() else {
        return false;
    };
    let query = MediaQuery {
        media_id: String::new(),
        kind: source.kind,
        title: source.title.clone(),
        original_title: None,
        year: source.year,
        external: source.external.clone(),
        episode: source.episode,
    };
    supports(&query)
        && safe_magnet(uri)
        && source.license.is_none()
        && display_text(&source.title, 2000)
        && display_text(raw, 2000)
        && !non_movie(raw)
        && rutracker::classify_release(raw, &query).is_some_and(|confidence| {
            confidence == rutracker::ReleaseMatch::Exact || source.possible_match
        })
        && movie_release_metadata(raw)
            .is_some_and(|(_, year)| !year.zip(source.year).is_some_and(|(a, b)| a != b))
        && source.provider == ID
        && source
            .quality
            .as_deref()
            .is_none_or(|q| display_text(q, 100))
        && source
            .origins
            .iter()
            .all(|origin| !origin.starts_with("popcorn:") || upstream_label(origin).is_some())
        && source
            .clone()
            .normalize()
            .is_ok_and(|normalized| normalized == *source)
}

#[cfg(test)]
#[path = "popcorn/tests.rs"]
mod tests;
