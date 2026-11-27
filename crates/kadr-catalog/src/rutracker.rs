//! Opt-in local desktop source discovery on the authorized RuTracker domain.
//!
//! Results are offers for explicit selection, not exact episode matches or rights
//! guarantees. The original title retains pack/episode scope. Looking up a
//! topic attachment is HTTP-only; login and `.torrent` download are separate.
//! No credentials, cookies or downloaded files are persisted by this module.

use std::{collections::HashSet, fmt, future::Future, time::Duration};

use kadr_core::{
    catalog::{Episode, Media, MediaKind},
    source::{ManualSource, SourceKind},
    sources::{MediaQuery, contains_movie_pack_label, movie_release_has_pack_suffix},
    torrent::{EpisodeNumber, EpisodeRecognition, recognize_episode},
};
use rutracker_api as api;

use crate::CancellationToken;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const OFFER_LIMIT: usize = 50;
const QUERY_LIMIT: usize = 4;
const SEARCH_CALL_LIMIT: usize = 6;
const SEARCH_PAGE_LIMIT: u32 = 2;

/// Strength of the release's title, production-year and scope evidence.
/// `Possible` requires explicit selection, never automatic movie playback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseMatch {
    Exact,
    Possible,
}

/// Distinguishes a genuinely empty catalog from topics rejected by matching.
#[derive(Debug, Eq, PartialEq)]
pub struct SearchOutcome {
    pub offers: Vec<Offer>,
    pub had_topics: bool,
}

struct SearchPage {
    offers: Vec<Offer>,
    had_topics: bool,
    total_pages: u32,
}

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

    /// Compatibility wrapper; new callers should use `search_outcome` to distinguish
    /// no topics from topics which did not match. No per-topic requests are made.
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
        let query = MediaQuery {
            media_id: media.id.clone(),
            kind: media.kind,
            title: media.title.clone(),
            original_title: media.original_title.clone(),
            year: media.year,
            external: media.external.clone(),
            episode: episode
                .map(|e| EpisodeNumber::new(e.season, e.number).ok_or(Error::InvalidQuery))
                .transpose()?,
        };
        Ok(self.search_catalog(&query, cancel).await?.offers)
    }

    /// Search the primary title first; only without confirmed hits try distinct
    /// alternatives and advertised extra pages. At most six calls, four titles,
    /// two pages per title and 50 offers. Login/cancellation are never hidden.
    pub async fn search_outcome(
        &self,
        query: &MediaQuery,
        cancel: &CancellationToken,
    ) -> Result<SearchOutcome, Error> {
        ready(cancel)?;
        if (query.kind == MediaKind::Series) != query.episode.is_some() {
            return Err(Error::InvalidQuery);
        }
        self.search_catalog(query, cancel).await
    }

    async fn search_catalog(
        &self,
        query: &MediaQuery,
        cancel: &CancellationToken,
    ) -> Result<SearchOutcome, Error> {
        search_with(query, cancel, |title, page| async move {
            let results = request(cancel, self.session().search(title).page(page).send()).await?;
            Ok(SearchPage {
                had_topics: results.total_count > 0 || !results.results.is_empty(),
                total_pages: results.total_pages,
                offers: results
                    .results
                    .into_iter()
                    .take(OFFER_LIMIT)
                    .map(|row| Offer {
                        topic_id: row.id.get(),
                        quality: quality(&row.title),
                        title: row.title,
                        size: row.size,
                        seeds: row.seeds.and_then(|n| u32::try_from(n).ok()),
                    })
                    .collect(),
            })
        })
        .await
    }

    /// HTTP lookup of a topic's attachment. No authentication prompt or
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

async fn search_with<F, Fut>(
    query: &MediaQuery,
    cancel: &CancellationToken,
    mut fetch: F,
) -> Result<SearchOutcome, Error>
where
    F: FnMut(String, u32) -> Fut,
    Fut: Future<Output = Result<SearchPage, Error>>,
{
    ready(cancel)?;
    let titles = queries(query)?;
    let mut pages = vec![1; titles.len()];
    let mut outcome = SearchOutcome {
        offers: Vec::new(),
        had_topics: false,
    };
    let mut seen = HashSet::new();
    let mut possible_indices = Vec::new();
    let mut calls = 0;
    let mut searched = false;
    // First pages of alternative titles precede deeper pages of broad searches.
    for page in 1..=SEARCH_PAGE_LIMIT {
        for (index, title) in titles.iter().enumerate() {
            if calls == SEARCH_CALL_LIMIT || page > pages[index] {
                continue;
            }
            ready(cancel)?;
            calls += 1;
            let result = fetch(title.clone(), page).await;
            ready(cancel)?;
            let result = match result {
                Ok(result) => result,
                // Windows-1251 cannot represent every alias.
                Err(Error::InvalidQuery) => continue,
                // A failed optional fallback must not discard usable choices.
                Err(Error::Network | Error::Timeout | Error::Http(500..=599))
                    if !outcome.offers.is_empty() =>
                {
                    return Ok(outcome);
                }
                Err(error) => return Err(error),
            };
            searched = true;
            pages[index] = result.total_pages.min(SEARCH_PAGE_LIMIT);
            outcome.had_topics |= result.had_topics || !result.offers.is_empty();
            let mut confirmed = false;
            for offer in result.offers.into_iter().take(OFFER_LIMIT) {
                ready(cancel)?;
                let Some(class) = classify_topic(&offer.title, query) else {
                    continue;
                };
                if offer.topic_id == 0 || !seen.insert(offer.topic_id) {
                    continue;
                }
                confirmed |= class == ReleaseMatch::Exact;
                if outcome.offers.len() < OFFER_LIMIT {
                    if class == ReleaseMatch::Possible {
                        possible_indices.push(outcome.offers.len());
                    }
                    outcome.offers.push(offer);
                } else if class == ReleaseMatch::Exact
                    && let Some(index) = possible_indices.pop()
                {
                    outcome.offers[index] = offer;
                }
            }
            if confirmed {
                return Ok(outcome);
            }
        }
    }
    ready(cancel)?;
    if !searched {
        return Err(Error::InvalidQuery);
    }
    Ok(outcome)
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

fn queries(query: &MediaQuery) -> Result<Vec<String>, Error> {
    let mut queries: Vec<String> = Vec::new();
    for title in [Some(query.title.as_str()), query.original_title.as_deref()]
        .into_iter()
        .flatten()
        .chain(
            query
                .external
                .alternative_titles
                .iter()
                .take(16)
                .map(String::as_str),
        )
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
            if queries.len() == QUERY_LIMIT {
                break;
            }
        }
    }
    if queries.is_empty() {
        Err(Error::InvalidQuery)
    } else {
        Ok(queries)
    }
}

/// Exact-only compatibility rule for automatic movie playback.
pub fn matches_movie_release(title: &str, query: &MediaQuery) -> bool {
    query.kind == MediaKind::Movie && classify_release(title, query) == Some(ReleaseMatch::Exact)
}

/// Revalidate live or cached topic titles without assuming external IDs in topics.
/// Unknown year/format can only be possible evidence; known contradictions veto.
pub fn classify_release(title: &str, query: &MediaQuery) -> Option<ReleaseMatch> {
    if query.kind == MediaKind::Series && query.episode.is_none() {
        return None;
    }
    classify_topic(title, query)
}

fn classify_topic(title: &str, query: &MediaQuery) -> Option<ReleaseMatch> {
    if title.chars().count() > 2_000
        || title.chars().any(char::is_control)
        || (query.kind == MediaKind::Movie && query.episode.is_some())
    {
        return None;
    }
    let titles = queries(query).ok()?;
    let heading = title.split(['[', '(']).next().unwrap_or("");
    let strict_title = heading.split(['/', '|']).any(|part| {
        let normalized = normalize(strip_scope(part));
        titles.iter().any(|query| normalized == normalize(query))
    });
    let title_matches = strict_title
        || heading.split(['/', '|']).any(|part| {
            let normalized = normalize(strip_scope(part));
            titles.iter().any(|query| {
                normalized
                    .strip_prefix(&format!("{} ", normalize(query)))
                    .is_some_and(release_tail)
            })
        });
    if !title_matches {
        return None;
    }
    let bracket = production_years(title).ok()?;
    let loose_years = unbracketed_years(title, &titles);
    let scope = query.episode.map(|episode| episode_scope(title, episode));
    if scope == Some(EpisodeScope::Conflict) {
        return None;
    }
    let mut year_known = false;
    let mut season_year = false;
    let mut movie_year = None;
    for (first, last) in bracket.range.into_iter().chain(loose_years) {
        if query.kind == MediaKind::Movie {
            if first != last || movie_year.is_some_and(|year| year != first) {
                return None;
            }
            movie_year = Some(first);
        }
        if let Some(year) = query.year {
            let agrees = match query.kind {
                MediaKind::Movie => first == year,
                MediaKind::Series => {
                    if (first..=last).contains(&year) {
                        true
                    } else if first > year
                        && query.episode.is_some_and(|e| e.season() > 1)
                        && matches!(scope, Some(EpisodeScope::Single | EpisodeScope::Pack))
                    {
                        // Without a season-year field this is plausible, not
                        // proof against a same-title series remake.
                        season_year = true;
                        true
                    } else {
                        false
                    }
                }
            };
            if !agrees {
                return None;
            }
            year_known = true;
        }
    }
    let exact_year = year_known && bracket.exact && strict_title && !season_year;
    match (query.kind, query.episode) {
        (MediaKind::Movie, _) => {
            if has_scope(title)
                || movie_release_has_pack_suffix(title)
                || heading
                    .split(['/', '|'])
                    .any(|part| contains_movie_pack_label(strip_title(part, &titles)))
                || !matches!(recognize_episode(title), EpisodeRecognition::Unrecognized)
            {
                return None;
            }
            Some(if exact_year {
                ReleaseMatch::Exact
            } else {
                ReleaseMatch::Possible
            })
        }
        (MediaKind::Series, Some(_)) => {
            Some(if exact_year && scope == Some(EpisodeScope::Single) {
                ReleaseMatch::Exact
            } else {
                ReleaseMatch::Possible
            })
        }
        // Only the legacy Media-based search supports a bare series.
        (MediaKind::Series, None) => has_scope(title).then_some(ReleaseMatch::Possible),
    }
}

fn release_tail(tail: &str) -> bool {
    // Only recognizable metadata may follow a full title. Never turn
    // "The Thing Returns" into a match for "The Thing".
    tail.split_whitespace().all(|token| {
        token
            .parse::<u16>()
            .is_ok_and(|n| (1800..=2200).contains(&n))
            || matches!(
                token,
                "2160p"
                    | "1080p"
                    | "1080i"
                    | "720p"
                    | "576p"
                    | "480p"
                    | "bdrip"
                    | "brrip"
                    | "bluray"
                    | "blu"
                    | "ray"
                    | "web"
                    | "dl"
                    | "webrip"
                    | "hdtv"
                    | "dvdrip"
                    | "remux"
                    | "x264"
                    | "x265"
                    | "h264"
                    | "h265"
                    | "hevc"
                    | "hdr"
                    | "hdr10"
                    | "sdr"
                    | "remastered"
                    | "remaster"
            )
    })
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

#[derive(Default)]
struct ProductionYears {
    range: Option<(u16, u16)>,
    exact: bool,
}

fn production_years(title: &str) -> Result<ProductionYears, ()> {
    let mut found = ProductionYears::default();
    for group in title.split(['[', '(']).skip(1) {
        let group = group.split([']', ')']).next().ok_or(())?.trim();
        let lower = group.to_lowercase();
        let remaster = ["remaster", "re-release", "переиздан", "реставрац"]
            .iter()
            .any(|label| lower.contains(label));
        let bytes = group.as_bytes();
        if bytes.len() < 4 || !bytes[..4].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let first: u16 = group[..4].parse().map_err(|_| ())?;
        if !(1800..=2200).contains(&first) {
            continue;
        }
        let rest = group[4..].trim_start();
        let (last, rest) = if let Some(rest) = rest.strip_prefix(['-', '–']) {
            let rest = rest.trim_start();
            let last = rest.get(..4).ok_or(())?.parse::<u16>().map_err(|_| ())?;
            (last, rest.get(4..).ok_or(())?.trim_start())
        } else {
            (first, rest)
        };
        // Standard production-year slots: "[2020, ...]", "(2020)", or ranges.
        // "[2024 remaster]" is not evidence for a film's production year.
        if !rest.is_empty() && !rest.starts_with([',', ';']) {
            continue;
        }
        if last < first || last > 2200 {
            return Err(());
        }
        if found
            .range
            .is_some_and(|previous| previous != (first, last))
        {
            return Err(());
        }
        found.range = Some((first, last));
        // A production-year slot remains contradiction evidence even if later
        // fields mention remastering. Preserve the old stricter Exact rule.
        found.exact |= !remaster;
    }
    Ok(found)
}

fn unbracketed_years(title: &str, titles: &[String]) -> Vec<(u16, u16)> {
    let mut outside = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '[' | '(' => {
                depth += 1;
                outside.push(' ');
            }
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    let mut years = Vec::new();
    for part in outside.split(['/', '|']) {
        // A numeric film title (1917, 1984, ...) is not a production year.
        let suffix = strip_title(part, titles).to_lowercase();
        let words: Vec<_> = suffix
            .split(|c: char| !c.is_alphanumeric() && !matches!(c, '-' | '–' | '*'))
            .filter(|word| !word.is_empty())
            .collect();
        for (i, word) in words.iter().enumerate() {
            let (first, last) = word.split_once(['-', '–']).unwrap_or((word, word));
            let (Ok(first), Ok(last)) = (first.parse::<u16>(), last.parse::<u16>()) else {
                continue;
            };
            if !(1800..=2200).contains(&first) || !(first..=2200).contains(&last) {
                continue;
            }
            if words.get(i + 1).is_some_and(|word| {
                matches!(
                    *word,
                    "x" | "*"
                        | "kbps"
                        | "mbps"
                        | "кбит"
                        | "мбит"
                        | "kb"
                        | "mb"
                        | "gb"
                        | "кб"
                        | "мб"
                        | "гб"
                        | "mhz"
                        | "мгц"
                )
            }) || i > 0 && matches!(words[i - 1], "x" | "*")
            {
                continue;
            }
            if words[..i].iter().any(|word| {
                word.starts_with("remaster")
                    || word.starts_with("реставрац")
                    || word.starts_with("переиздан")
                    || *word == "release"
            }) || (words
                .get(i + 1)
                .is_some_and(|word| word.starts_with("remaster"))
                && words
                    .get(i + 2)
                    .is_none_or(|word| word.parse::<u16>().is_err()))
            {
                continue;
            }
            years.push((first, last));
        }
    }
    years
}

fn strip_title<'a>(part: &'a str, titles: &[String]) -> &'a str {
    let normalized = normalize(part);
    let count = titles
        .iter()
        .map(|title| normalize(title))
        .filter(|title| normalized == *title || normalized.starts_with(&format!("{title} ")))
        .map(|title| title.split_whitespace().count())
        .max();
    let Some(count) = count else {
        return part;
    };
    let mut words = 0;
    let mut in_word = false;
    for (index, c) in part.char_indices() {
        if c.is_alphanumeric() {
            if !in_word {
                words += 1;
            }
            in_word = true;
        } else {
            if in_word && words == count {
                return &part[index..];
            }
            in_word = false;
        }
    }
    ""
}

/// (season, optional first/last episode), recognized explicit source labels.
fn compact_scope(token: &str) -> Option<(u16, Option<(u16, u16)>)> {
    if let Some((season, episode)) = token.split_once('x')
        && season.len() <= 2
    {
        let episode = episode.parse().ok()?;
        return Some((season.parse().ok()?, Some((episode, episode))));
    }
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
    labeled_ranges(title, labels).into_iter().next()
}

fn labeled_ranges(title: &str, labels: &[&str]) -> Vec<(u16, u16)> {
    let words: Vec<_> = title
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '–')
        .filter(|s| !s.is_empty())
        .collect();
    words
        .windows(2)
        .filter_map(|pair| {
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
        .collect()
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum EpisodeScope {
    Unknown,
    Conflict,
    Pack,
    Single,
}

fn episode_scope(title: &str, episode: EpisodeNumber) -> EpisodeScope {
    let title = title.to_lowercase();
    let seasons = labeled_ranges(&title, &["сезон", "сезоны", "season", "seasons"]);
    let episodes = labeled_ranges(&title, &["серия", "серии", "episode", "episodes"]);
    if seasons
        .iter()
        .any(|&(first, last)| !(first..=last).contains(&episode.season()))
        || episodes
            .iter()
            .any(|&(first, last)| !(first..=last).contains(&episode.episode()))
    {
        return EpisodeScope::Conflict;
    }
    let compact: Vec<_> = scope_tokens(&title).filter_map(compact_scope).collect();
    if compact.iter().any(|&(season, episodes)| {
        season != episode.season()
            || episodes.is_some_and(|(first, last)| !(first..=last).contains(&episode.episode()))
    }) {
        return EpisodeScope::Conflict;
    }
    let recognized = recognize_episode(&title);
    match &recognized {
        EpisodeRecognition::Recognized(number) if *number != episode => {
            return EpisodeScope::Conflict;
        }
        EpisodeRecognition::Ambiguous(numbers)
            if numbers.iter().any(|number| {
                number.season() != episode.season()
                    || (*number != episode
                        && !compact.iter().any(|&(season, range)| {
                            season == number.season()
                                && range.is_some_and(|(first, last)| {
                                    (first..=last).contains(&number.episode())
                                })
                        }))
            }) =>
        {
            return EpisodeScope::Conflict;
        }
        _ => {}
    }
    // Even a matching single marker does not make an expressly labeled pack
    // an exact episode: the user still has to choose the file.
    if seasons
        .iter()
        .chain(&episodes)
        .any(|(first, last)| first != last)
        || compact
            .iter()
            .any(|(_, range)| range.is_some_and(|(first, last)| first != last))
    {
        return EpisodeScope::Pack;
    }
    if matches!(recognized, EpisodeRecognition::Recognized(_))
        || (!seasons.is_empty() && !episodes.is_empty())
    {
        EpisodeScope::Single
    } else if !seasons.is_empty() || !compact.is_empty() {
        EpisodeScope::Pack
    } else {
        EpisodeScope::Unknown
    }
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
