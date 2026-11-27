//! Opt-in local desktop source discovery on the authorized RuTracker domain.
//!
//! Results are offers for explicit selection, not exact episode matches or rights
//! guarantees. The original title retains pack/episode scope. Call `magnet` only
//! after selection; login and `.torrent` download are a separate fallback.
//! No credentials, cookies or downloaded files are persisted by this module.

use std::{collections::HashSet, fmt, future::Future, time::Duration};

use kadr_core::{
    catalog::{Episode, Media, MediaKind},
    source::{ManualSource, SourceKind},
    sources::{MediaQuery, movie_release_has_pack_suffix},
    torrent::{EpisodeRecognition, recognize_episode},
};
use rutracker_api as api;

use crate::CancellationToken;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const OFFER_LIMIT: usize = 50;

/// Provider-reported data only. Missing metadata is not replaced with zero.
#[derive(Clone, Eq, PartialEq)]
pub struct Offer {
    pub topic_id: u64,
    /// Full source title, including season/episode/pack information.
    pub title: String,
    pub quality: Option<String>,
    pub size: Option<u64>,
    pub seeds: Option<u32>,
}

impl fmt::Debug for Offer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Offer")
            .field("topic_id", &"[redacted]")
            .field("title", &"[redacted]")
            .field("quality", &self.quality)
            .field("size", &self.size)
            .field("seeds", &self.seeds)
            .finish()
    }
}

/// Redacted errors: no nested transport errors, URLs, account data or HTML.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Cancelled,
    InvalidQuery,
    InvalidTopic,
    AuthenticationRequired,
    Authorization,
    Network,
    Timeout,
    RateLimited,
    Http(u16),
    TooLarge,
    InvalidResponse,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "RuTracker request cancelled.",
            Self::InvalidQuery => "Unsupported RuTracker search title.",
            Self::InvalidTopic => "Invalid RuTracker topic identifier.",
            Self::AuthenticationRequired => "RuTracker login is required.",
            Self::Authorization => {
                "RuTracker login was not accepted; check the account or captcha."
            }
            Self::Network => "RuTracker connection failed.",
            Self::Timeout => "RuTracker request timed out.",
            Self::RateLimited => "RuTracker rate limit or captcha requires a later retry.",
            Self::Http(_) => "RuTracker returned an unsuccessful HTTP status.",
            Self::TooLarge => "RuTracker response exceeds the safety limit.",
            Self::InvalidResponse => "RuTracker returned unsupported data.",
        })
    }
}

impl std::error::Error for Error {}

impl From<api::Error> for Error {
    fn from(error: api::Error) -> Self {
        match error {
            api::Error::NotAuthenticated => Self::AuthenticationRequired,
            api::Error::Authorization(_) => Self::Authorization,
            api::Error::RateLimited => Self::RateLimited,
            api::Error::TooLarge => Self::TooLarge,
            api::Error::InvalidArgument(_) => Self::InvalidQuery,
            api::Error::Server(status) => Self::Http(status),
            api::Error::Http(error) => {
                if error
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.is_timeout())
                {
                    Self::Timeout
                } else {
                    Self::Network
                }
            }
            _ => Self::InvalidResponse,
        }
    }
}

/// Single-owner session. Deliberately not `Clone` or `Debug`.
pub struct Client {
    anonymous: api::Client,
    authenticated: Option<api::AuthenticatedClient>,
    session_builder: api::ClientBuilder,
}

impl Client {
    /// Construct without network activity. Only `https://rutracker.org:443` is allowed.
    pub fn new() -> Result<Self, Error> {
        Self::new_with_proxy(None)
    }

    /// Use the same explicit, unauthenticated proxy for anonymous requests,
    /// fresh login sessions and authenticated downloads, with no direct fallback.
    /// `None` retains the explicitly direct RuTracker transport.
    pub fn new_with_proxy(proxy: Option<&str>) -> Result<Self, Error> {
        let mut session_builder = api::Client::builder();
        if let Some(endpoint) = proxy {
            session_builder =
                session_builder.proxy(crate::explicit_proxy(endpoint).map_err(|_| Error::Network)?);
        }
        Ok(Self {
            anonymous: session_builder.clone().build().map_err(Error::from)?,
            authenticated: None,
            session_builder,
        })
    }

    fn session(&self) -> &api::Client {
        self.authenticated.as_deref().unwrap_or(&self.anonymous)
    }

    /// Search the first page of each distinct localized/original title, with a
    /// combined limit of 50 offers. No per-topic requests, pagination or crawling.
    ///
    /// RuTracker may require authentication even for listing; this is returned as
    /// `AuthenticationRequired`, not silently converted into an empty result.
    /// Conservative title/year/scope filtering intentionally misses some releases.
    pub async fn search(
        &self,
        media: &Media,
        episode: Option<&Episode>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Offer>, Error> {
        ready(cancel)?;
        if media.kind == MediaKind::Movie && episode.is_some() {
            return Ok(Vec::new());
        }
        let queries = queries(media)?;
        let mut seen = HashSet::new();
        let mut offers = Vec::new();
        let mut searched = false;
        for query in &queries {
            let results = match request(cancel, self.session().search(query).send()).await {
                Ok(results) => results,
                // RuTracker's Windows-1251 forms cannot represent every original
                // title. A usable localized/original alternative may still work.
                Err(Error::InvalidQuery) => continue,
                Err(error) => return Err(error),
            };
            searched = true;
            for row in results.results.into_iter().take(OFFER_LIMIT) {
                ready(cancel)?;
                if row.id.get() == 0
                    || row.title.chars().count() > 2_000
                    || row.title.chars().any(char::is_control)
                    || !matches_release(&row.title, media, episode, &queries)
                    || !seen.insert(row.id.get())
                {
                    continue;
                }
                offers.push(Offer {
                    topic_id: row.id.get(),
                    quality: quality(&row.title),
                    title: row.title,
                    size: row.size,
                    seeds: row.seeds.and_then(|n| u32::try_from(n).ok()),
                });
                if offers.len() == OFFER_LIMIT {
                    return Ok(offers);
                }
            }
        }
        ready(cancel)?;
        if !searched {
            return Err(Error::InvalidQuery);
        }
        Ok(offers)
    }

    /// Lazy lookup for an explicitly selected topic. No authentication prompt or
    /// automatic download is performed when the topic offers no magnet.
    pub async fn magnet(
        &self,
        topic_id: u64,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, Error> {
        ready(cancel)?;
        valid_topic(topic_id)?;
        let magnet = request(cancel, self.session().get_magnet_link(topic_id)).await?;
        magnet.map(validate_magnet).transpose()
    }

    /// Try a fresh session. Failed, cancelled or dropped login attempts cannot
    /// leave their cookie jar attached to this client. Password is never stored.
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        self.logout();
        ready(cancel)?;
        if username.is_empty()
            || password.is_empty()
            || username.len() > 256
            || password.len() > 1_024
            || username.chars().any(char::is_control)
            || password.chars().any(char::is_control)
        {
            return Err(Error::Authorization);
        }
        let candidate = self.session_builder.clone().build().map_err(Error::from)?;
        let authenticated = request(cancel, candidate.login(username, password)).await?;
        ready(cancel)?;
        self.authenticated = Some(authenticated);
        Ok(())
    }

    /// Authenticated `.torrent` fallback, at most 4 MiB. The torrent engine must
    /// validate the metainfo before use; this adapter never starts a transfer.
    pub async fn download(
        &self,
        topic_id: u64,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, Error> {
        ready(cancel)?;
        valid_topic(topic_id)?;
        let session = self
            .authenticated
            .as_ref()
            .ok_or(Error::AuthenticationRequired)?;
        let bytes = request(cancel, session.download_torrent(topic_id)).await?;
        // Reject a mislabeled HTML/login/error response. Full bencode validation
        // belongs to the torrent engine, not a second parser in the adapter.
        if bytes.first() != Some(&b'd') || bytes.last() != Some(&b'e') {
            return Err(Error::InvalidResponse);
        }
        Ok(bytes)
    }

    /// Drop local authenticated cookies. Does not revoke a server-side session.
    pub fn logout(&mut self) {
        self.authenticated = None;
    }
}

fn ready(cancel: &CancellationToken) -> Result<(), Error> {
    if cancel.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn valid_topic(topic_id: u64) -> Result<(), Error> {
    if topic_id == 0 {
        Err(Error::InvalidTopic)
    } else {
        Ok(())
    }
}

async fn request<T>(
    cancel: &CancellationToken,
    future: impl Future<Output = api::Result<T>>,
) -> Result<T, Error> {
    ready(cancel)?;
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout(REQUEST_TIMEOUT, future) => {
            ready(cancel)?;
            result.map_err(|_| Error::Timeout)?.map_err(Error::from)
        }
    }
}

fn validate_magnet(magnet: String) -> Result<String, Error> {
    if magnet.len() > 16_384
        || !ManualSource::parse_link(&magnet)
            .is_ok_and(|source| source.kind() == SourceKind::Magnet)
    {
        return Err(Error::InvalidResponse);
    }
    Ok(magnet)
}

fn normalize(title: &str) -> String {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn queries(media: &Media) -> Result<Vec<String>, Error> {
    let mut queries: Vec<String> = Vec::new();
    for title in [Some(media.title.as_str()), media.original_title.as_deref()]
        .into_iter()
        .flatten()
    {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > 200 || title.chars().any(char::is_control) {
            continue;
        }
        // Literal words only: no RuTracker search operators or quoting syntax.
        let query = title
            .split(|c: char| !c.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !query.is_empty() && !queries.iter().any(|q| normalize(q) == normalize(&query)) {
            queries.push(query);
        }
    }
    if queries.is_empty() {
        Err(Error::InvalidQuery)
    } else {
        Ok(queries)
    }
}

/// Revalidate film release evidence on cache reads as well as live discovery.
/// RuTracker has no external film IDs: only a full title and an unambiguous,
/// identical production year can establish eligibility.
pub fn matches_movie_release(title: &str, query: &MediaQuery) -> bool {
    if query.kind != MediaKind::Movie || query.episode.is_some() {
        return false;
    }
    let titles: Vec<_> = std::iter::once(query.title.clone())
        .chain(query.original_title.clone())
        .collect();
    matches_release_metadata(title, query.kind, query.year, None, &titles)
}

fn matches_release(
    title: &str,
    media: &Media,
    episode: Option<&Episode>,
    queries: &[String],
) -> bool {
    matches_release_metadata(title, media.kind, media.year, episode, queries)
}

fn matches_release_metadata(
    title: &str,
    kind: MediaKind,
    year: Option<u16>,
    episode: Option<&Episode>,
    queries: &[String],
) -> bool {
    if kind == MediaKind::Movie && (year.is_none() || episode.is_some()) {
        return false;
    }
    let heading = title.split(['[', '(']).next().unwrap_or("");
    let title_matches = heading.split(['/', '|']).any(|part| {
        let normalized = normalize(strip_scope(part));
        queries.iter().any(|query| normalized == normalize(query))
    });
    if !title_matches {
        return false;
    }
    // A number anywhere in the release title is NOT a production year: it may
    // be the title itself, resolution, remaster year or unrelated metadata.
    if let Some(year) = year {
        let Some((first, last)) = production_years(title) else {
            return false;
        };
        if match kind {
            MediaKind::Movie => first != year || last != year,
            MediaKind::Series => year < first || year > last,
        } {
            return false;
        }
    }
    match (kind, episode) {
        (MediaKind::Movie, _) => {
            !has_scope(title)
                && !movie_release_has_pack_suffix(title)
                && matches!(recognize_episode(title), EpisodeRecognition::Unrecognized)
        }
        (MediaKind::Series, Some(episode)) => episode_scope(title, episode),
        (MediaKind::Series, None) => has_scope(title),
    }
}

fn strip_scope(part: &str) -> &str {
    // Scope tokens must start at a word boundary. Do not turn "Show sequel"
    // into a match for "Show" by general prefix/substring matching.
    for (index, _) in part.char_indices() {
        if index != 0 && !part[..index].ends_with(|c: char| c.is_whitespace() || c == '-') {
            continue;
        }
        let suffix = part[index..].to_lowercase();
        if ((suffix.starts_with("сезон") || suffix.starts_with("season"))
            && labeled_range(&suffix, &["сезон", "сезоны", "season", "seasons"]).is_some())
            || compact_scope(suffix.split_whitespace().next().unwrap_or("")).is_some()
        {
            return part[..index].trim_end_matches([' ', '-', ':']);
        }
    }
    part
}

fn production_years(title: &str) -> Option<(u16, u16)> {
    let mut found = None;
    for group in title.split(['[', '(']).skip(1) {
        let group = group.split([']', ')']).next()?.trim();
        let lower = group.to_lowercase();
        if ["remaster", "re-release", "переиздан", "реставрац"]
            .iter()
            .any(|label| lower.contains(label))
        {
            continue;
        }
        let bytes = group.as_bytes();
        if bytes.len() < 4 || !bytes[..4].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let first: u16 = group[..4].parse().ok()?;
        if !(1800..=2200).contains(&first) {
            continue;
        }
        let rest = group[4..].trim_start();
        let (last, rest) = if let Some(rest) = rest.strip_prefix(['-', '–']) {
            let rest = rest.trim_start();
            let last = rest.get(..4)?.parse::<u16>().ok()?;
            (last, rest.get(4..)?.trim_start())
        } else {
            (first, rest)
        };
        // Standard production-year slots: "[2020, ...]", "(2020)", or ranges.
        // "[2024 remaster]" is not evidence for a film's production year.
        if !rest.is_empty() && !rest.starts_with([',', ';']) {
            continue;
        }
        if last < first || last > 2200 {
            return None;
        }
        if found.is_some_and(|previous| previous != (first, last)) {
            return None;
        }
        found = Some((first, last));
    }
    found
}

/// (season, optional first/last episode), recognized explicit source labels.
fn compact_scope(token: &str) -> Option<(u16, Option<(u16, u16)>)> {
    let rest = token.strip_prefix('s')?;
    let season_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let season = rest[..season_end].parse().ok()?;
    let rest = &rest[season_end..];
    if rest.is_empty() {
        return Some((season, None));
    }
    let rest = rest.strip_prefix('e')?;
    let (first, last) = rest
        .split_once('-')
        .map_or((rest, rest), |(a, b)| (a, b.trim_start_matches('e')));
    let (first, last) = (first.parse().ok()?, last.parse().ok()?);
    if first > last {
        return None;
    }
    Some((season, Some((first, last))))
}

fn labeled_range(title: &str, labels: &[&str]) -> Option<(u16, u16)> {
    let words: Vec<_> = title
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '–')
        .filter(|s| !s.is_empty())
        .collect();
    words.windows(2).find_map(|pair| {
        if !labels.contains(&pair[0]) {
            return None;
        }
        let (first, last) = pair[1].split_once(['-', '–']).unwrap_or((pair[1], pair[1]));
        let (first, last) = (first.parse().ok()?, last.parse().ok()?);
        if first > last {
            None
        } else {
            Some((first, last))
        }
    })
}

fn scope_tokens(title: &str) -> impl Iterator<Item = &str> {
    title
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|s| !s.is_empty())
}

fn has_scope(title: &str) -> bool {
    let title = title.to_lowercase();
    scope_tokens(&title).any(|s| compact_scope(s).is_some())
        || labeled_range(&title, &["сезон", "сезоны", "season", "seasons"]).is_some()
}

fn episode_scope(title: &str, episode: &Episode) -> bool {
    let title = title.to_lowercase();
    let seasons = labeled_range(&title, &["сезон", "сезоны", "season", "seasons"]);
    let episodes = labeled_range(&title, &["серия", "серии", "episode", "episodes"]);
    if seasons.is_some_and(|(first, last)| !(first..=last).contains(&episode.season))
        || episodes.is_some_and(|(first, last)| !(first..=last).contains(&episode.number))
    {
        return false;
    }
    let compact: Vec<_> = scope_tokens(&title).filter_map(compact_scope).collect();
    if !compact.is_empty() {
        return compact.into_iter().any(|(season, episodes)| {
            season == episode.season
                && episodes.is_none_or(|(first, last)| (first..=last).contains(&episode.number))
        });
    }
    // Multi-season and season-only packs are candidates, not exact episodes.
    seasons.is_some()
}

fn quality(title: &str) -> Option<String> {
    let lower = title.to_lowercase();
    ["2160p", "1080p", "1080i", "720p", "576p", "480p"]
        .into_iter()
        .find(|quality| {
            lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == *quality)
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
