//! Read-only, explicit Commons discovery for two verified Blender short films.
//! No title/ID search, account, background IO, retries, or media downloads.
//! Rights are checked per file, not inferred from Commons membership. Metadata
//! schema/rights changes fail closed. Media URLs use the existing HTTP player path.
//! Credits apply only to the selected film, not logos, trademarks or DVD art.
//! ED retains this Commons file's BY-SA 2.5 obligations, not BBB's BY 3.0 grant.

use std::{collections::HashSet, fmt, time::Duration};

use kadr_core::{
    catalog::{ExternalMetadata, MediaKind},
    sources::{Locator, MediaQuery, MediaSource, match_score},
};
use serde_json::Value;
use url::Url;

use crate::CancellationToken;

pub const ID: &str = "wikimedia-commons";
const API: &str = "https://commons.wikimedia.org/w/api.php";
const UPLOAD: &str = "https://upload.wikimedia.org/wikipedia/commons";
const TIMEOUT: Duration = Duration::from_secs(20);
const BODY_LIMIT: usize = 512 * 1024;
const NODE_LIMIT: usize = 10_000;
const DERIVATIVE_LIMIT: usize = 64;

struct Work {
    title: &'static str,
    year: u16,
    imdb: &'static str,
    tmdb: Option<u64>,
    pageid: u64,
    file: &'static str,
    path: &'static str,
    artist: &'static str,
    attribution: &'static str,
    license: &'static str,
    short_license: &'static str,
    terms: &'static str,
    license_url: &'static str,
    credit: &'static str,
    mime: &'static str,
    encodings: &'static [&'static str],
}

// File identity and rights evidence: the linked Commons file pages; BBB's
// creator terms are also published at https://peach.blender.org/about/.
// ED deliberately follows this file's BY-SA 2.5 claim, not another edition.
const WORKS: &[Work] = &[
    Work {
        title: "Big Buck Bunny",
        year: 2008,
        imdb: "tt1254207",
        tmdb: Some(10378),
        pageid: 40108642,
        file: "Big Buck Bunny 4K.webm",
        path: "c/c0/Big_Buck_Bunny_4K.webm",
        artist: "Blender Foundation",
        attribution: "(c) copyright Blender Foundation | www.bigbuckbunny.org",
        license: "cc-by-3.0",
        short_license: "CC BY 3.0",
        terms: "Creative Commons Attribution 3.0",
        license_url: "https://creativecommons.org/licenses/by/3.0",
        credit: "(c) copyright Blender Foundation | www.bigbuckbunny.org — Big Buck Bunny; https://commons.wikimedia.org/wiki/File:Big_Buck_Bunny_4K.webm — CC BY 3.0 https://creativecommons.org/licenses/by/3.0",
        mime: "video/webm",
        encodings: &["240p.vp9.webm", "480p.vp9.webm", "1080p.vp9.webm"],
    },
    Work {
        title: "Elephants Dream",
        year: 2006,
        imdb: "tt0807840",
        tmdb: Some(9761),
        pageid: 8819826,
        file: "Elephants Dream.ogv",
        path: "d/d5/Elephants_Dream.ogv",
        artist: "(c) copyright 2006, Blender Foundation / Netherlands Media Art Institute / www.elephantsdream.org",
        attribution: "(c) copyright 2006, Blender Foundation / Netherlands Media Art Institute / www.elephantsdream.org",
        license: "cc-by-sa-2.5",
        short_license: "CC BY-SA 2.5",
        terms: "Creative Commons Attribution-Share Alike 2.5",
        license_url: "https://creativecommons.org/licenses/by-sa/2.5",
        credit: "(c) copyright 2006, Blender Foundation / Netherlands Media Art Institute / www.elephantsdream.org — Elephants Dream; https://commons.wikimedia.org/wiki/File:Elephants_Dream.ogv — CC BY-SA 2.5 https://creativecommons.org/licenses/by-sa/2.5",
        mime: "application/ogg",
        encodings: &["240p.vp9.webm"],
    },
];

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
            Self::InvalidProxy => f.write_str("Invalid Commons proxy endpoint."),
            Self::InvalidQuery => f.write_str("Invalid Commons request."),
            Self::Cancelled => f.write_str("Commons request cancelled."),
            Self::Network => f.write_str("Commons connection failed."),
            Self::Timeout => f.write_str("Commons request timed out."),
            Self::RateLimited => f.write_str("Commons rate limit reached; retry later."),
            Self::Http(status) => write!(f, "Commons returned HTTP {status}."),
            Self::TooLarge => f.write_str("Commons response exceeds the safety limit."),
            Self::InvalidResponse => {
                f.write_str("Commons file identity or rights could not be verified.")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    #[cfg(test)]
    fixture_origin: Option<Url>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommonsClient").finish_non_exhaustive()
    }
}

impl Client {
    /// Offline. Explicit unauthenticated proxies replace system defaults with
    /// no direct fallback. TLS validation remains enabled; redirects are denied.
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
            fixture_origin: None,
        })
    }

    /// Only a verified movie can cause IO. The request contains a fixed page ID,
    /// never the caller's title, catalog ID, aliases, or external identifiers.
    pub async fn search(
        &self,
        query: &MediaQuery,
        now: i64,
        cancel: &CancellationToken,
    ) -> Result<Vec<MediaSource>, Error> {
        ready(cancel)?;
        if now < 0 {
            return Err(Error::InvalidQuery);
        }
        let Some(work) = matching_work(query) else {
            return Ok(Vec::new());
        };
        let mut url = Url::parse(API).map_err(|_| Error::InvalidResponse)?;
        url.query_pairs_mut()
            .append_pair("action", "query")
            .append_pair("prop", "videoinfo")
            .append_pair("format", "json")
            .append_pair("formatversion", "2")
            .append_pair("pageids", &work.pageid.to_string())
            .append_pair("vilimit", "1")
            .append_pair("viprop", "url|size|mime|extmetadata|derivatives");
        // Only tests can replace the fixed public API with a loopback server.
        #[cfg(test)]
        if let Some(origin) = &self.fixture_origin {
            let query = url.query().map(str::to_owned);
            url = origin
                .join("/w/api.php")
                .map_err(|_| Error::InvalidResponse)?;
            url.set_query(query.as_deref());
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(TIMEOUT, async {
                let body = read_body(self.http.get(url)).await?;
                let sources = parse(&body, work, now)?;
                ready(cancel)?;
                Ok(sources.into_iter().filter_map(|mut source| {
                    source.match_score = match_score(query, &source)?;
                    Some(source)
                }).collect())
            }) => result.map_err(|_| Error::Timeout)?,
        }
    }
}

fn http_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(
            "Kadr/0.1 (https://github.com/mkrdnk/kadr; explicit read-only Commons requests)",
        )
        .connect_timeout(Duration::from_secs(5))
        .timeout(TIMEOUT)
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
}

pub fn supports(query: &MediaQuery) -> bool {
    matching_work(query).is_some()
}

fn matching_work(query: &MediaQuery) -> Option<&'static Work> {
    WORKS
        .iter()
        .find(|work| match_score(query, &identity(work)).is_some())
}

fn identity(work: &Work) -> MediaSource {
    let mut source = MediaSource::new(
        ID,
        Locator::Http(format!("{UPLOAD}/{}", work.path)),
        work.title,
        MediaKind::Movie,
    )
    .expect("constant Commons identity");
    source.year = Some(work.year);
    source.external = ExternalMetadata {
        imdb_id: Some(work.imdb.into()),
        tmdb_id: work.tmdb,
        alternative_titles: Vec::new(),
    };
    source
}

fn page(work: &Work) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/File:{}",
        work.file.replace(' ', "_")
    )
}

// Only encodings verified for these files, never a network-chosen suffix/path.
// New transcodes require an explicit allowlist update, not a guessed URL.
fn encoding(key: &str) -> Option<(u16, &'static str, &'static str, &'static str)> {
    let height = match key {
        "240p.vp9.webm" => 240,
        "480p.vp9.webm" => 480,
        "1080p.vp9.webm" => 1080,
        _ => return None,
    };
    Some((height, "VP9", "Opus", "video/webm; codecs=\"vp9, opus\""))
}

fn derivative_url(work: &Work, key: &str) -> String {
    let filename = work.path.rsplit('/').next().expect("constant filename");
    format!("{UPLOAD}/transcoded/{}/{filename}.{key}", work.path)
}

fn offer(work: &Work, key: &str, now: i64) -> Option<MediaSource> {
    if !work.encodings.contains(&key) {
        return None;
    }
    let (height, video, audio, _) = encoding(key)?;
    let mut source = identity(work);
    source.locator = Locator::Http(derivative_url(work, key));
    source.provider_item_id = Some(format!("{}:{key}", work.pageid));
    // Origins are provider IDs; the verified file page travels in the credit.
    source.resolution = Some(height);
    source.video_codec = Some(video.into());
    source.audio_codec = Some(audio.into());
    source.fetched_at = now;
    source.license = Some(work.credit.into());
    // No rendition byte size is provided by videoinfo. The original's size and
    // bandwidth * duration are NOT the size of this progressive encoding.
    source.normalize().ok()
}

/// Validated static attribution, file page and CC license URL. Labels from the
/// request, response, or cache are never interpreted as a rights grant.
pub fn credit(source: &MediaSource) -> Option<&'static str> {
    trusted_offer(source).map(|(work, _)| work.credit)
}

fn trusted_offer(source: &MediaSource) -> Option<(&'static Work, &str)> {
    if source.provider != ID {
        return None;
    }
    let Locator::Http(raw) = &source.locator else {
        return None;
    };
    let (pageid, key) = source.provider_item_id.as_deref()?.split_once(':')?;
    let work = WORKS
        .iter()
        .find(|work| pageid == work.pageid.to_string())?;
    if !work.encodings.contains(&key) {
        return None;
    }
    let canonical = identity(work);
    (raw == &derivative_url(work, key)
        && source.title == canonical.title
        && source.kind == canonical.kind
        && source.year == canonical.year
        && source.episode.is_none()
        && source.external == canonical.external)
        .then_some((work, key))
}

/// Cache authority: recheck identity, exact fixed-host path and all generated
/// fields rather than trusting a serialized provider label or normalized URL.
pub fn accepts(source: &MediaSource) -> bool {
    let Some((work, key)) = trusted_offer(source) else {
        return false;
    };
    let Some(mut expected) = offer(work, key, source.fetched_at) else {
        return false;
    };
    expected.match_score = source.match_score;
    source.match_score <= 100 && *source == expected
}

fn parse(body: &[u8], work: &Work, now: i64) -> Result<Vec<MediaSource>, Error> {
    if body.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    let data: Value = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    let mut remaining = NODE_LIMIT;
    bounded(&data, &mut remaining, 0)?;
    unique_keys(body)?;
    // With vilimit=1 the current revision is complete even when the API
    // supplies a `continue.vistart` cursor for older uploads. Never follow it.
    if data.get("error").is_some() || data.get("warnings").is_some() {
        return Err(Error::InvalidResponse);
    }
    let pages = data["query"]["pages"]
        .as_array()
        .ok_or(Error::InvalidResponse)?;
    if pages.len() != 1 {
        return Err(Error::InvalidResponse);
    }
    let item = &pages[0];
    if item["pageid"].as_u64() != Some(work.pageid)
        || item["ns"] != 6
        || item["title"] != format!("File:{}", work.file)
        || item["imagerepository"] != "local"
        || ["missing", "invalid", "redirect"]
            .iter()
            .any(|key| item.get(key).is_some())
    {
        return Err(Error::InvalidResponse);
    }
    let infos = item["videoinfo"].as_array().ok_or(Error::InvalidResponse)?;
    if infos.len() != 1 {
        return Err(Error::InvalidResponse);
    }
    let info = &infos[0];
    let original = format!("{UPLOAD}/{}", work.path);
    let tracked = format!(
        "{original}?utm_source=commons.wikimedia.org&utm_campaign=imageinfo&utm_content=original"
    );
    if ![original.as_str(), tracked.as_str()].contains(&info["url"].as_str().unwrap_or(""))
        || info["descriptionurl"] != page(work)
        || info["mime"] != work.mime
        || info["size"].as_u64().is_none_or(|size| size == 0)
    {
        return Err(Error::InvalidResponse);
    }
    rights(&info["extmetadata"], work)?;
    let derivatives = info["derivatives"]
        .as_array()
        .ok_or(Error::InvalidResponse)?;
    if derivatives.len() > DERIVATIVE_LIMIT {
        return Err(Error::TooLarge);
    }
    let mut sources = Vec::new();
    for derivative in derivatives {
        let Some(key) = derivative["transcodekey"].as_str() else {
            continue;
        };
        if !work.encodings.contains(&key) {
            continue;
        }
        let Some((height, _, _, mime)) = encoding(key) else {
            continue;
        };
        if derivative["src"] != derivative_url(work, key)
            || derivative["type"] != mime
            || derivative["height"].as_u64() != Some(u64::from(height))
            || derivative["width"]
                .as_u64()
                .is_none_or(|width| width == 0 || width > 8192)
        {
            return Err(Error::InvalidResponse);
        }
        let source = offer(work, key, now).ok_or(Error::InvalidResponse)?;
        if sources
            .iter()
            .any(|existing: &MediaSource| existing.id == source.id)
        {
            return Err(Error::InvalidResponse);
        }
        sources.push(source);
    }
    Ok(sources)
}

fn rights(metadata: &Value, work: &Work) -> Result<(), Error> {
    let fields = metadata.as_object().ok_or(Error::InvalidResponse)?;
    // Unknown rights fields (including alternate licenses) cannot silently
    // weaken the exact per-file claim. Non-rights fields are never displayed.
    const KNOWN: &[&str] = &[
        "DateTime",
        "ObjectName",
        "CommonsMetadataExtension",
        "Categories",
        "Assessments",
        "ImageDescription",
        "DateTimeOriginal",
        "Credit",
        "Artist",
        "LicenseShortName",
        "UsageTerms",
        "AttributionRequired",
        "Attribution",
        "LicenseUrl",
        "Copyrighted",
        "Restrictions",
        "License",
    ];
    if fields.keys().any(|key| !KNOWN.contains(&key.as_str())) {
        return Err(Error::InvalidResponse);
    }
    for (key, expected) in [
        ("Artist", work.artist),
        ("Attribution", work.attribution),
        ("LicenseShortName", work.short_license),
        ("UsageTerms", work.terms),
        ("LicenseUrl", work.license_url),
        ("License", work.license),
        ("AttributionRequired", "true"),
        ("Copyrighted", "True"),
        ("Restrictions", ""),
    ] {
        let field = &metadata[key];
        let origin = if key == "License" {
            "commons-templates"
        } else {
            "commons-desc-page"
        };
        if field["value"] != expected || field["source"] != origin {
            return Err(Error::InvalidResponse);
        }
    }
    let categories = match metadata.get("Categories") {
        None => "",
        Some(field) => field["value"].as_str().ok_or(Error::InvalidResponse)?,
    }
    .to_ascii_lowercase();
    if [
        "licens",
        "copyright",
        "restrict",
        "permission",
        "disput",
        "non-free",
    ]
    .iter()
    .any(|term| categories.contains(term))
    {
        return Err(Error::InvalidResponse);
    }
    Ok(())
}

fn bounded(value: &Value, remaining: &mut usize, depth: usize) -> Result<(), Error> {
    if *remaining == 0 || depth > 32 {
        return Err(Error::TooLarge);
    }
    *remaining -= 1;
    match value {
        Value::Array(items) => {
            for item in items {
                bounded(item, remaining, depth + 1)?;
            }
        }
        Value::Object(items) => {
            for item in items.values() {
                bounded(item, remaining, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Value uses last-key-wins. Reject duplicate object keys before interpreting
// rights or identity, including escaped spellings of the same JSON key. Syntax
// has already been checked by serde_json; this only tracks containers/strings.
fn unique_keys(body: &[u8]) -> Result<(), Error> {
    let mut containers: Vec<Option<HashSet<String>>> = Vec::new();
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            b'{' => containers.push(Some(HashSet::new())),
            b'[' => containers.push(None),
            b'}' | b']' => {
                containers.pop();
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < body.len() && body[index] != b'"' {
                    if body[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
                let after = body[index + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace());
                if after == Some(&b':') {
                    let key: String = serde_json::from_slice(&body[start..=index])
                        .map_err(|_| Error::InvalidResponse)?;
                    if let Some(Some(keys)) = containers.last_mut()
                        && !keys.insert(key)
                    {
                        return Err(Error::InvalidResponse);
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

fn ready(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
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
    Ok(body)
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

#[cfg(test)]
#[path = "commons/tests.rs"]
mod tests;
