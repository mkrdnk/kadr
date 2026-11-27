//! Bounded, read-only Torznab 1.3 discovery. This module never starts a transfer.
//!
//! Capabilities are read for each explicit search; there is no background IO.
//! Only the first page is searched. Provider metadata is untrusted evidence for
//! the shared matcher, never an exact-match assertion.
//!
//! Limitations: custom indexer category mappings and TVDb queries are not
//! supported. Untyped/ambiguous categories are omitted. Release parsing accepts
//! only explicit year/episode boundaries; the shared matcher may reject other
//! naming conventions. Credential-bearing signed URLs are rejected. Magnets
//! retain identity only, so private trackers should expose same-origin metainfo.
//! Metainfo validation here only rejects obvious non-bencode; the torrent engine
//! must validate structure, hashes and resource limits before any use.

use std::{collections::HashSet, fmt, time::Duration};

use kadr_core::{
    catalog::MediaKind,
    source::ManualSource,
    sources::{
        Locator, MediaQuery, MediaSource, movie_release_has_pack_suffix, provider_item_digest,
    },
    torrent::{EpisodeNumber, EpisodeRecognition, recognize_episode},
};
use roxmltree::{Document, Node, ParsingOptions};
use url::Url;

use crate::CancellationToken;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const BODY_LIMIT: usize = 4 * 1024 * 1024;
const NODE_LIMIT: u32 = 20_000;
const DEPTH_LIMIT: usize = 32;
const RESULT_LIMIT: usize = 100;
const TORZNAB_NS: &str = "http://torznab.com/schemas/2015/feed";

/// No nested errors, URLs, response bodies, queries or credentials are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidEndpoint,
    InvalidApiKey,
    InvalidProxy,
    InvalidQuery,
    InvalidLocator,
    UnsupportedSearch,
    Cancelled,
    Network,
    Timeout,
    RateLimited,
    Http(u16),
    Api(u32),
    TooLarge,
    InvalidResponse,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("Invalid Torznab API endpoint."),
            Self::InvalidApiKey => f.write_str("Invalid Torznab API key."),
            Self::InvalidProxy => f.write_str("Invalid Torznab proxy endpoint."),
            Self::InvalidQuery => f.write_str("Invalid Torznab search query."),
            Self::InvalidLocator => f.write_str("Unsafe Torznab metainfo locator."),
            Self::UnsupportedSearch => f.write_str("Torznab does not advertise a usable search."),
            Self::Cancelled => f.write_str("Torznab request cancelled."),
            Self::Network => f.write_str("Torznab connection failed."),
            Self::Timeout => f.write_str("Torznab request timed out."),
            Self::RateLimited => f.write_str("Torznab rate limit reached."),
            Self::Http(status) => write!(f, "Torznab returned HTTP {status}."),
            Self::Api(code) => write!(f, "Torznab returned API error {code}."),
            Self::TooLarge => f.write_str("Torznab response exceeds the safety limit."),
            Self::InvalidResponse => f.write_str("Torznab returned unsupported data."),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Url,
    api_key: Option<String>,
    shared_origin: bool,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// Offline construction. The endpoint must be an HTTP(S) API URL without
    /// userinfo, query or fragment; supply the API key separately.
    /// Explicit proxies replace system proxies without a direct fallback.
    pub fn new(
        endpoint: &str,
        api_key: Option<String>,
        proxy: Option<&str>,
    ) -> Result<Self, Error> {
        let endpoint =
            kadr_core::sources::torznab_endpoint(endpoint).ok_or(Error::InvalidEndpoint)?;
        if api_key.as_deref().is_some_and(|key| {
            key.is_empty()
                || key.len() > 256
                || key.chars().any(|c| c.is_control() || c.is_whitespace())
        }) {
            return Err(Error::InvalidApiKey);
        }
        let mut http = reqwest::Client::builder()
            .user_agent("kadr/0.1 (Torznab; explicit user requests)")
            .connect_timeout(Duration::from_secs(5))
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
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
            endpoint,
            api_key,
            shared_origin: false,
        })
    }

    /// Several configured API instances can share a host. A same-origin
    /// download path outside this API then has no unambiguous key owner.
    pub fn restrict_enclosure_key_to_api(&mut self) {
        self.shared_origin = true;
    }

    /// Read capabilities, then search one bounded page using only advertised
    /// specialized parameters or an explicitly advertised free-text fallback.
    pub async fn search(
        &self,
        query: &MediaQuery,
        provider_id: &str,
        now: i64,
        cancel: &CancellationToken,
    ) -> Result<Vec<MediaSource>, Error> {
        ready(cancel)?;
        if !self.safe_text(provider_id, 100) {
            return Err(Error::InvalidQuery);
        }
        // Validate before issuing even the capabilities request.
        search_title(query)?;
        let mut url = self.endpoint.clone();
        url.query_pairs_mut().append_pair("t", "caps");
        let body = self.fetch(self.authenticated(url), cancel).await?;
        let document = xml(&body)?;
        let capabilities = caps(&document)?;
        let url = self.search_url(query, &capabilities)?;
        let body = self.fetch(self.authenticated(url), cancel).await?;
        let document = xml(&body)?;
        let sources = self.sources(&document, provider_id, now)?;
        ready(cancel)?;
        Ok(sources)
    }

    fn search_url(&self, query: &MediaQuery, caps: &Caps) -> Result<Url, Error> {
        let title = search_title(query)?;
        let imdb = query.external.imdb_id.as_deref().and_then(imdb_id);
        let (function, mode) = match query.kind {
            MediaKind::Movie
                if caps.movie.supports("q")
                    || (imdb.is_some() && caps.movie.supports("imdbid")) =>
            {
                ("movie", &caps.movie)
            }
            MediaKind::Series if caps.tv.supports("q") => ("tvsearch", &caps.tv),
            _ if caps.search.supports("q") => ("search", &caps.search),
            _ => return Err(Error::UnsupportedSearch),
        };
        let mut url = self.endpoint.clone();
        let mut params = url.query_pairs_mut();
        params
            .append_pair("t", function)
            .append_pair("limit", &caps.limit.to_string())
            .append_pair("offset", "0")
            .append_pair("extended", "1");
        if let Some(imdb) = imdb.filter(|_| function == "movie" && mode.supports("imdbid")) {
            // Torznab/Newznab imdbid search uses the numeric IMDb identifier.
            params.append_pair("imdbid", &imdb[2..]);
        } else {
            let mut text = title.to_owned();
            if query.kind == MediaKind::Movie
                && let Some(year) = query.year
            {
                text.push_str(&format!(" {year}"));
            }
            if let Some(episode) = query.episode
                && (function != "tvsearch" || !mode.supports("season") || !mode.supports("ep"))
            {
                text.push_str(&format!(
                    " S{:02}E{:02}",
                    episode.season(),
                    episode.episode()
                ));
            }
            params.append_pair("q", &text);
        }
        if function == "tvsearch"
            && let Some(episode) = query.episode
        {
            if mode.supports("season") {
                params.append_pair("season", &episode.season().to_string());
            }
            if mode.supports("ep") {
                params.append_pair("ep", &episode.episode().to_string());
            }
        }
        drop(params);
        Ok(url)
    }

    fn sources(
        &self,
        document: &Document<'_>,
        provider_id: &str,
        now: i64,
    ) -> Result<Vec<MediaSource>, Error> {
        let root = document.root_element();
        if !named(root, "rss") || root.attribute("version") != Some("2.0") {
            return Err(Error::InvalidResponse);
        }
        let channels: Vec<_> = root.children().filter(|n| named(*n, "channel")).collect();
        if channels.len() != 1
            || root
                .children()
                .any(|n| n.is_element() && !named(n, "channel"))
        {
            return Err(Error::InvalidResponse);
        }
        let channel = channels[0];
        if child(channel, "title")
            .and_then(|n| n.text())
            .is_none_or(|v| v.trim().is_empty())
            || child(channel, "link")
                .and_then(|n| n.text())
                .and_then(http_url)
                .is_none()
            || child(channel, "description").is_none()
        {
            return Err(Error::InvalidResponse);
        }
        let items: Vec<_> = channel.children().filter(|n| named(*n, "item")).collect();
        if items.len() > RESULT_LIMIT {
            return Err(Error::TooLarge);
        }
        let mut seen = HashSet::new();
        let mut sources = Vec::new();
        for item in items {
            let Some(title) = child(item, "title")
                .filter(|n| !n.children().any(|child| child.is_element()))
                .and_then(|n| n.text())
            else {
                continue;
            };
            // Do not rewrite, strip or replace scope-bearing release titles.
            if !self.safe_text(title, 2000) {
                continue;
            }
            let Some(kind) = item_kind(item) else {
                continue;
            };
            let parsed = release_title(title);
            if kind == MediaKind::Movie && (parsed.scoped || movie_release_has_pack_suffix(title)) {
                continue;
            }
            let canonical = attr(
                item,
                if kind == MediaKind::Movie {
                    "imdbtitle"
                } else {
                    "tvtitle"
                },
            )
            .filter(|value| self.safe_text(value, 1000))
            .unwrap_or(&parsed.title);
            let magnet_uri = attr(item, "magneturl")
                .and_then(magnet)
                .or_else(|| {
                    item.children()
                        .filter(|n| named(*n, "enclosure"))
                        .filter_map(|n| n.attribute("url"))
                        .find_map(magnet)
                })
                .filter(|value| !self.contains_key(value));
            // Keep the declared content identity even when the actual handoff
            // is a metainfo enclosure. Never associate two contradictory claims.
            let magnet_hash = magnet_uri.as_ref().and_then(|uri| {
                MediaSource::new(provider_id, Locator::Magnet(uri.clone()), canonical, kind)
                    .ok()?
                    .info_hash
            });
            let declared = attr(item, "infohash").and_then(infohash);
            if declared
                .as_ref()
                .zip(magnet_hash.as_ref())
                .is_some_and(|(a, b)| a != b)
            {
                continue;
            }
            let hash = declared.or(magnet_hash);
            // Metainfo preserves private tracker configuration without caching
            // its credentials in a magnet's tracker URLs.
            let locator = item
                .children()
                .filter(|n| named(*n, "enclosure"))
                .filter(|n| n.attribute("type") == Some("application/x-bittorrent"))
                .filter_map(|n| n.attribute("url"))
                .find_map(|url| self.clean_locator(url))
                .map(|url| Locator::TorrentUrl(url.into()))
                .or_else(|| magnet_uri.map(Locator::Magnet))
                .or_else(|| {
                    hash.as_ref()
                        .filter(|value| !self.contains_key(value))
                        .and_then(|hash| magnet(&format!("magnet:?xt=urn:btih:{hash}")))
                        .map(Locator::Magnet)
                });
            let Some(locator) = locator else {
                continue;
            };
            let Ok(mut source) = MediaSource::new(provider_id, locator, canonical, kind) else {
                continue;
            };
            source.release_title = Some(title.to_owned());
            source.fetched_at = now;
            source.provider_item_id = child(item, "guid")
                .and_then(|n| n.text())
                .or_else(|| attr(item, "guid"))
                .filter(|value| !value.is_empty() && value.len() <= 4096)
                .map(provider_item_digest);
            let declared_year = number::<u16>(attr(item, "year"));
            let imdb_year = number::<u16>(attr(item, "imdbyear"));
            if kind == MediaKind::Movie && declared_year.zip(imdb_year).is_some_and(|(a, b)| a != b)
            {
                continue;
            }
            source.year = declared_year
                .or(imdb_year)
                .or(parsed.year)
                .filter(|year| (1800..=2200).contains(year));
            if parsed.year.is_some() && source.year != parsed.year {
                continue;
            }
            if kind == MediaKind::Series {
                // A missing/ambiguous raw scope must not be narrowed to one
                // episode by a misleading singleton extended attribute.
                let seasons = attrs(item, "season");
                let episodes = attrs(item, "episode");
                source.episode = parsed.episode.filter(|episode| {
                    (seasons.is_empty()
                        || (seasons.len() == 1
                            && number(Some(seasons[0])) == Some(episode.season())))
                        && (episodes.is_empty()
                            || (episodes.len() == 1
                                && number(Some(episodes[0])) == Some(episode.episode())))
                });
            }
            source.external.imdb_id = attr(item, "imdb")
                .and_then(imdb_id)
                .filter(|value| !self.contains_key(value));
            source.external.tmdb_id = number(attr(item, "tmdbid"));
            source.resolution = resolution(item, title);
            source.size = number(attr(item, "size"));
            source.seeds = number(attr(item, "seeders"));
            source.peers = number(attr(item, "peers"));
            source.video_codec = attr(item, "video")
                .filter(|value| self.safe_text(value, 100))
                .map(str::to_owned);
            source.audio_codec = attr(item, "audio")
                .filter(|value| self.safe_text(value, 100))
                .map(str::to_owned);
            source.info_hash = hash.filter(|value| !self.contains_key(value));
            source.languages = attrs(item, "language")
                .into_iter()
                .flat_map(|value| value.split(','))
                .map(str::trim)
                .filter(|value| self.safe_text(value, 50))
                .take(10)
                .map(str::to_owned)
                .collect();
            source.languages.sort();
            source.languages.dedup();
            let Ok(source) = source.normalize() else {
                continue;
            };
            // Raw GUIDs, URLs and their authentication data are never retained.
            if seen.insert(source.id.clone()) {
                sources.push(source);
            }
        }
        Ok(sources)
    }

    /// Reauthorize a deserialized offer against this provider before showing or
    /// selecting it. A syntactically valid URL alone is not this capability.
    pub fn accepts(&self, source: &MediaSource) -> bool {
        let safe_locator = match &source.locator {
            Locator::TorrentUrl(raw) => self
                .clean_locator(raw)
                .is_some_and(|url| url.as_str() == raw),
            Locator::Magnet(raw) => magnet(raw).as_ref() == Some(raw) && !self.contains_key(raw),
            _ => false,
        };
        safe_locator
            && source.license.is_none() // Indexer metadata is not a rights grant.
            && self.safe_text(&source.title, 2000)
            && source
                .release_title
                .as_ref()
                .is_none_or(|s| self.safe_text(s, 2000))
            && movie_release_is_suitable(source)
            && source
                .quality
                .as_ref()
                .is_none_or(|s| self.safe_text(s, 100))
            && [&source.video_codec, &source.audio_codec].into_iter().flatten().all(|s| self.safe_text(s, 100))
            && source.languages.iter().all(|s| self.safe_text(s, 50))
    }

    /// Fetch only same-origin HTTP(S) metainfo. On a shared origin, send the
    /// instance's key only to its own API path, never to a sibling's enclosure.
    /// Full bencode and torrent validation belongs to the torrent engine.
    pub async fn metainfo(&self, url: &str, cancel: &CancellationToken) -> Result<Vec<u8>, Error> {
        ready(cancel)?;
        let url = self.clean_locator(url).ok_or(Error::InvalidLocator)?;
        let url = if !self.shared_origin || url.path() == self.endpoint.path() {
            self.authenticated(url)
        } else {
            url
        };
        let body = self.fetch(url, cancel).await?;
        if body.first() != Some(&b'd') || body.last() != Some(&b'e') {
            return Err(Error::InvalidResponse);
        }
        Ok(body)
    }

    fn authenticated(&self, mut url: Url) -> Url {
        if let Some(key) = &self.api_key {
            url.query_pairs_mut().append_pair("apikey", key);
        }
        url
    }

    async fn fetch(&self, url: Url, cancel: &CancellationToken) -> Result<Vec<u8>, Error> {
        ready(cancel)?;
        // This check guards every credential-bearing request, not just enclosures.
        if url.origin() != self.endpoint.origin() {
            return Err(Error::InvalidLocator);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(REQUEST_TIMEOUT, read_body(self.http.get(url))) => {
                ready(cancel)?;
                result.map_err(|_| Error::Timeout)?
            }
        }
    }

    // Strip only the Torznab key parameter. Other credential-like parameters
    // are rejected rather than silently mutating opaque signed download URLs.
    fn clean_locator(&self, raw: &str) -> Option<Url> {
        if raw.len() > 8192 {
            return None;
        }
        let mut url = http_url(raw)?;
        if url.origin() != self.endpoint.origin() {
            return None;
        }
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(name, _)| !name.eq_ignore_ascii_case("apikey"))
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        if pairs.iter().any(|(name, value)| {
            !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || credential_name(name)
                || self.contains_key(name)
                || self.contains_key(value)
        }) || self.contains_key(url.path())
        {
            return None;
        }
        url.set_query(None);
        url.set_fragment(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
        Some(url)
    }

    fn contains_key(&self, value: &str) -> bool {
        let Some(key) = &self.api_key else {
            return false;
        };
        // Inspect URL/form escaped variants as well as XML-decoded strings.
        let mut decoded = value.to_owned();
        for _ in 0..4 {
            if decoded.contains(key) {
                return true;
            }
            let escaped = decoded.replace('&', "%26");
            let next = url::form_urlencoded::parse(format!("v={escaped}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            if next == decoded {
                return false;
            }
            decoded = next;
        }
        // Deeply nested escaping is not a useful display field or locator.
        true
    }

    fn safe_text(&self, text: &str, max: usize) -> bool {
        !text.trim().is_empty()
            && text.len() <= max
            && !text.chars().any(char::is_control)
            && !self.contains_key(text)
            && !text.contains("://")
            && !["apikey", "api_key", "password", "passkey", "access_token"]
                .iter()
                .any(|name| text.to_ascii_lowercase().contains(name))
    }
}

fn ready(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn http_url(raw: &str) -> Option<Url> {
    ManualSource::parse_link(raw).ok()?;
    let url = Url::parse(raw).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn credential_name(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    matches!(normalized.as_str(), "user" | "username" | "sig")
        || [
            "key",
            "token",
            "secret",
            "auth",
            "pass",
            "credential",
            "signature",
            "session",
            "cookie",
        ]
        .iter()
        .any(|part| normalized.contains(part))
}

async fn read_body(request: reqwest::RequestBuilder) -> Result<Vec<u8>, Error> {
    let mut response = request.send().await.map_err(network_error)?;
    match response.status().as_u16() {
        200 => {}
        429 => return Err(Error::RateLimited),
        status => return Err(Error::Http(status)),
    }
    if response
        .content_length()
        .is_some_and(|n| n > BODY_LIMIT as u64)
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
    Ok(body)
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

fn xml(body: &[u8]) -> Result<Document<'_>, Error> {
    if body.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    let text = std::str::from_utf8(body).map_err(|_| Error::InvalidResponse)?;
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: NODE_LIMIT,
        },
    )
    .map_err(|_| Error::InvalidResponse)?;
    for node in document.descendants() {
        if node.ancestors().take(DEPTH_LIMIT + 1).count() > DEPTH_LIMIT {
            return Err(Error::TooLarge);
        }
        // Error bodies must never be interpreted as successful empty RSS/caps.
        if node.is_element() && node.tag_name().name() == "error" {
            let code = node.attribute("code").and_then(|v| v.parse().ok());
            return Err(code.map_or(Error::InvalidResponse, Error::Api));
        }
    }
    Ok(document)
}

fn named(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|node| named(*node, name))
}

#[derive(Default)]
struct SearchMode {
    available: bool,
    params: HashSet<String>,
}

impl SearchMode {
    fn supports(&self, param: &str) -> bool {
        self.available && self.params.contains(param)
    }
}

struct Caps {
    search: SearchMode,
    movie: SearchMode,
    tv: SearchMode,
    limit: usize,
}

fn caps(document: &Document<'_>) -> Result<Caps, Error> {
    let root = document.root_element();
    if !named(root, "caps") {
        return Err(Error::InvalidResponse);
    }
    let searching = child(root, "searching").ok_or(Error::InvalidResponse)?;
    let mode = |name| {
        let node = child(searching, name);
        SearchMode {
            available: node.and_then(|n| n.attribute("available")) == Some("yes"),
            params: node
                .and_then(|n| n.attribute("supportedParams"))
                .unwrap_or("")
                .split(',')
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
        }
    };
    let limit = child(root, "limits")
        .and_then(|node| node.attribute("max"))
        .map(|n| n.parse::<usize>().map_err(|_| Error::InvalidResponse))
        .transpose()?
        .unwrap_or(RESULT_LIMIT)
        .min(RESULT_LIMIT);
    if limit == 0 {
        return Err(Error::UnsupportedSearch);
    }
    Ok(Caps {
        search: mode("search"),
        movie: mode("movie-search"),
        tv: mode("tv-search"),
        limit,
    })
}

fn attrs<'a, 'input>(item: Node<'a, 'input>, name: &'a str) -> Vec<&'a str> {
    let mut values = Vec::new();
    for node in item.children().filter(|node| {
        node.is_element()
            && node.tag_name().namespace() == Some(TORZNAB_NS)
            && node.tag_name().name() == "attr"
            && node.attribute("name") == Some(name)
    }) {
        if let Some(value) = node.attribute("value")
            && !values.contains(&value)
        {
            values.push(value);
        }
    }
    values
}

// Distinct values are a list, not "first wins". Conflicting singular metadata
// is omitted so that the shared matcher cannot mistake a pack for one episode.
fn attr<'a, 'input>(item: Node<'a, 'input>, name: &'a str) -> Option<&'a str> {
    let values = attrs(item, name);
    (values.len() == 1).then(|| values[0])
}

fn number<T: std::str::FromStr>(value: Option<&str>) -> Option<T> {
    value
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()
}

fn infohash(value: &str) -> Option<String> {
    (value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn search_title(query: &MediaQuery) -> Result<&str, Error> {
    if (query.kind == MediaKind::Series) != query.episode.is_some() {
        return Err(Error::InvalidQuery);
    }
    let title = query
        .original_title
        .as_deref()
        .unwrap_or(&query.title)
        .trim();
    if title.is_empty() || title.len() > 800 || title.chars().any(char::is_control) {
        return Err(Error::InvalidQuery);
    }
    Ok(title)
}

fn imdb_id(raw: &str) -> Option<String> {
    let digits = raw.strip_prefix("tt").unwrap_or(raw);
    ((7..=10).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("tt{digits}"))
}

fn item_kind(item: Node<'_, '_>) -> Option<MediaKind> {
    let mut kinds = HashSet::new();
    for category in attrs(item, "category") {
        match number::<u32>(Some(category)) {
            Some(2000..=2999) => {
                kinds.insert(0);
            }
            Some(5000..=5999) => {
                kinds.insert(1);
            }
            _ => {}
        }
    }
    if kinds.len() != 1 {
        return None;
    }
    Some(if kinds.contains(&0) {
        MediaKind::Movie
    } else {
        MediaKind::Series
    })
}

fn resolution(item: Node<'_, '_>, title: &str) -> Option<u16> {
    let known = |text: &str| match text.to_ascii_lowercase().as_str() {
        "2160p" | "3840x2160" => Some(2160),
        "1080p" | "1080i" | "1920x1080" => Some(1080),
        "720p" | "1280x720" => Some(720),
        "576p" | "720x576" => Some(576),
        "480p" | "720x480" => Some(480),
        _ => None,
    };
    attr(item, "resolution").and_then(known).or_else(|| {
        let mut values: Vec<_> = title
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter_map(known)
            .collect();
        values.sort();
        values.dedup();
        (values.len() == 1).then(|| values[0])
    })
}

struct ReleaseTitle {
    title: String,
    year: Option<u16>,
    scoped: bool,
    episode: Option<EpisodeNumber>,
}

/// Reapply raw movie evidence to restored offers; cached scores and a canonical
/// IMDb title cannot narrow a collection or contradict the release's year.
fn movie_release_is_suitable(source: &MediaSource) -> bool {
    if source.kind != MediaKind::Movie {
        return true;
    }
    source.release_title.as_deref().is_none_or(|raw| {
        let parsed = release_title(raw);
        !parsed.scoped
            && !movie_release_has_pack_suffix(raw)
            && !parsed.year.zip(source.year).is_some_and(|(a, b)| a != b)
    })
}

fn release_title(raw: &str) -> ReleaseTitle {
    let mut parsed = ReleaseTitle {
        title: raw.to_owned(),
        year: None,
        scoped: false,
        episode: None,
    };
    // Recognize only explicit year/episode boundaries. No prefix matching
    // against the request and no inference from arbitrary release numbers.
    let tokens: Vec<_> = raw
        .split(|c: char| c.is_whitespace() || matches!(c, '.' | '_' | '[' | ']' | '(' | ')'))
        .filter(|s| !s.is_empty())
        .collect();
    let mut boundary = None;
    for (index, token) in tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        if ["season", "seasons", "сезон", "сезоны"].contains(&lower.as_str()) {
            boundary.get_or_insert(index);
            parsed.scoped = true;
            parsed.episode = None;
            break;
        }
        if lower.starts_with('s') && lower.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
            let rest = &lower[1..];
            let season_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let season = number::<u16>(Some(&rest[..season_end]));
            let tail = &rest[season_end..];
            if season.is_some()
                && (tail.is_empty() || tail.starts_with('e') || tail.starts_with('-'))
            {
                boundary.get_or_insert(index);
                parsed.scoped = true;
                parsed.episode = tail
                    .strip_prefix('e')
                    .and_then(|ep| number(Some(ep)))
                    .and_then(|ep| EpisodeNumber::new(season.unwrap(), ep));
                // Additional scope tokens (multi-episode/season) are ambiguous.
                if tokens[index + 1..].iter().any(|token| {
                    let token = token.to_ascii_lowercase();
                    (token.starts_with('s') || token.starts_with('e'))
                        && token.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                        || token == "-"
                }) {
                    parsed.episode = None;
                }
                break;
            }
        }
        // Match the core recognizer's narrow x-form, not video dimensions.
        if let Some((season, episode)) = lower.split_once('x')
            && let Some(season) = number::<u16>(Some(season)).filter(|_| season.len() <= 2)
        {
            boundary.get_or_insert(index);
            parsed.scoped = true;
            parsed.episode =
                number(Some(episode)).and_then(|episode| EpisodeNumber::new(season, episode));
            break;
        }
        if index > 0
            && token.len() == 4
            && let Some(year) = number::<u16>(Some(token)).filter(|v| (1800..=2200).contains(v))
        {
            boundary.get_or_insert(index);
            parsed.year.get_or_insert(year);
        }
    }
    if let Some(index) = boundary.filter(|index| *index > 0) {
        parsed.title = tokens[..index].join(" ");
    }
    let pack = tokens.iter().any(|token| {
        [
            "pack",
            "complete",
            "collection",
            "season",
            "seasons",
            "сезон",
            "сезоны",
        ]
        .contains(&token.to_lowercase().as_str())
    });
    parsed.episode =
        parsed
            .episode
            .filter(|_| !pack)
            .and_then(|candidate| match recognize_episode(raw) {
                EpisodeRecognition::Recognized(episode) if episode == candidate => Some(episode),
                _ => None,
            });
    parsed
}

/// Keep only validated identity fields, never trackers, display names, web seeds
/// or arbitrary URL parameters which may contain account-specific passkeys.
fn magnet(raw: &str) -> Option<String> {
    ManualSource::parse_link(raw).ok()?;
    let url = Url::parse(raw).ok()?;
    if url.scheme() != "magnet" || raw.len() > 16_384 {
        return None;
    }
    let mut canonical = Url::parse("magnet:?").ok()?;
    let mut hashes = HashSet::new();
    for (name, value) in url.query_pairs() {
        let value = value.to_ascii_lowercase();
        if name == "xt"
            && (value.starts_with("urn:btih:") || value.starts_with("urn:btmh:"))
            && hashes.insert(value.clone())
        {
            canonical.query_pairs_mut().append_pair("xt", &value);
        }
    }
    ManualSource::parse_link(canonical.as_str()).ok()?;
    Some(canonical.into())
}

#[cfg(test)]
#[path = "torznab/tests.rs"]
mod tests;
