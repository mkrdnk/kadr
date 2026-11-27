//! IO-free discovery domain. Provider metadata is a claim, not verified media metadata.
//!
//! Generic adapters use [`select_sources`]. Adapters which already conservatively
//! matched releases (including packs) use [`rank_and_dedup`] directly instead of
//! inventing exact episode/title metadata to satisfy [`matches_query`].
//! Serialized public fields are not trusted: normalize again after deserialization,
//! and revalidate locators at the playback/network boundary. URL validation is not
//! an SSRF policy; adapters must authorize destinations, DNS and redirects.

use crate::{
    catalog::{Episode, ExternalMetadata, Media, MediaKind},
    source::{ManualSource, SourceKind, decode_btih},
    torrent::EpisodeNumber,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{cmp::Ordering, collections::BTreeSet, fmt, path::PathBuf};
use url::Url;

/// User-specified indexer API: never put a key in configuration or send it
/// over plaintext to a remote host. Loopback HTTP supports local indexers.
pub fn torznab_endpoint(raw: &str) -> Option<Url> {
    if raw.len() > 2048 || raw.contains(['?', '#']) {
        return None;
    }
    let input = ManualSource::parse_link(raw).ok()?;
    let mut url = input.http_url()?.clone();
    let host = url
        .host_str()?
        .strip_suffix('.')
        .unwrap_or(url.host_str()?)
        .to_owned();
    if host.ends_with('.') || url.port() == Some(0) {
        return None;
    }
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if url.scheme() != "https" && !loopback {
        return None;
    }
    url.set_host(Some(&host)).ok()?;
    Some(url)
}

/// A movie or a concrete episode of a show, never a bare series.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct MediaQuery {
    pub media_id: String,
    pub kind: MediaKind,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub external: ExternalMetadata,
    pub episode: Option<EpisodeNumber>,
}

impl MediaQuery {
    pub fn from_media(media: &Media, episode: Option<&Episode>) -> Option<Self> {
        let (kind, number) = match media.kind {
            MediaKind::Movie if episode.is_none() => ("movie", None),
            MediaKind::Series => {
                let episode = episode?;
                (
                    "show",
                    Some(EpisodeNumber::new(episode.season, episode.number)?),
                )
            }
            _ => return None,
        };
        let id = media.id.strip_prefix(&format!("trakt:{kind}:"))?;
        if id.starts_with('0')
            || !id.bytes().all(|b| b.is_ascii_digit())
            || !id.parse::<u64>().is_ok_and(|id| id > 0)
            || !valid_text(&media.title, 500)
        {
            return None;
        }
        Some(Self {
            media_id: media.id.clone(),
            kind: media.kind,
            title: display_text(&media.title),
            original_title: media.original_title.clone(),
            year: media.year,
            external: media.external.clone().normalized(),
            episode: number,
        })
    }
}

impl fmt::Debug for MediaQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaQuery([redacted])")
    }
}

/// Discovery descriptors. Local paths are only authorized by an explicit user
/// attachment, never by a network provider or its cached response.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub enum Locator {
    RuTracker(u64),
    Magnet(String),
    Http(String),
    /// Remote HTTP(S) metainfo, not a local file or a direct video URL.
    /// The extension need not be `.torrent`; content validation belongs to IO.
    TorrentUrl(String),
    /// Validated local user attachment; normalization does not grant file access.
    TorrentFile(PathBuf),
}

impl fmt::Debug for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::RuTracker(_) => "RuTracker",
            Self::Magnet(_) => "Magnet",
            Self::Http(_) => "Http",
            Self::TorrentUrl(_) => "TorrentUrl",
            Self::TorrentFile(_) => "TorrentFile",
        };
        write!(f, "{kind}([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceError {
    InvalidLocator,
    InvalidMetadata,
    InvalidInfoHash,
    ConflictingInfoHashes,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLocator => "The discovery source locator is invalid.",
            Self::InvalidMetadata => "The discovery source metadata is invalid.",
            Self::InvalidInfoHash => "The discovery source info hash is invalid.",
            Self::ConflictingInfoHashes => "The discovery source has conflicting info hashes.",
        })
    }
}

impl std::error::Error for SourceError {}

impl Locator {
    /// Uses the same strict parser as manual input, without opening any resource.
    pub fn validate(&self) -> Result<Self, SourceError> {
        let input = match self {
            Self::RuTracker(id) => {
                return if *id > 0 {
                    Ok(self.clone())
                } else {
                    Err(SourceError::InvalidLocator)
                };
            }
            Self::TorrentFile(path) => {
                let source = ManualSource::torrent_file(path.clone())
                    .map_err(|_| SourceError::InvalidLocator)?;
                return Ok(Self::TorrentFile(
                    source
                        .torrent_path()
                        .expect("validated torrent path")
                        .to_owned(),
                ));
            }
            Self::Magnet(input) | Self::Http(input) | Self::TorrentUrl(input) => input,
        };
        if input.len() > 16 * 1024 {
            return Err(SourceError::InvalidLocator);
        }
        let parsed = ManualSource::parse_link(input).map_err(|_| SourceError::InvalidLocator)?;
        match self {
            Self::Magnet(_) if parsed.kind() == SourceKind::Magnet => Ok(self.clone()),
            Self::Http(_) | Self::TorrentUrl(_) if parsed.kind() == SourceKind::Http => {
                let url = parsed
                    .http_url()
                    .ok_or(SourceError::InvalidLocator)?
                    .to_string();
                Ok(if matches!(self, Self::Http(_)) {
                    Self::Http(url)
                } else {
                    Self::TorrentUrl(url)
                })
            }
            _ => Err(SourceError::InvalidLocator),
        }
    }

    fn identity(&self) -> String {
        match self {
            Self::RuTracker(id) => digest_id("rutracker", &id.to_string()),
            Self::TorrentUrl(url) => digest_id("torrent-url", url),
            Self::TorrentFile(path) => ManualSource::torrent_file(path.clone())
                .expect("normalized torrent path")
                .id()
                .as_str()
                .to_owned(),
            Self::Http(input) | Self::Magnet(input) => ManualSource::parse_link(input)
                .expect("normalized locator")
                .id()
                .as_str()
                .to_owned(),
        }
    }

    fn hashes(&self) -> BTreeSet<String> {
        let Self::Magnet(input) = self else {
            return BTreeSet::new();
        };
        // ManualSource has already checked conflicts, encodings and namespaces.
        Url::parse(input)
            .expect("normalized magnet")
            .query_pairs()
            .filter(|(key, _)| key == "xt")
            .filter_map(|(_, value)| {
                let value = value.to_ascii_lowercase();
                if let Some(value) = value.strip_prefix("urn:btih:") {
                    normalize_hash(value).ok()
                } else {
                    value
                        .strip_prefix("urn:btmh:1220")
                        .and_then(|value| normalize_hash(value).ok())
                }
            })
            .collect()
    }
}

/// Normalized, serializable discovery offer. `None`/empty languages mean unknown;
/// zero seeds/size remain known zero, not missing. No metadata is parsed from a
/// release title. `id` is recomputed by normalization, never trusted from storage.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct MediaSource {
    pub id: String,
    /// Stable provider-instance key, not a display name shared by several servers.
    pub provider: String,
    /// Opaque provider GUID, scoped by `provider`, not globally unique.
    pub provider_item_id: Option<String>,
    /// Provenance labels/pages; normalization also retains the provider label.
    pub origins: Vec<String>,
    /// Verified credit and license notice, including links if the license requires them.
    /// Absence means the provider has not established rights to the work.
    #[serde(default)]
    pub license: Option<String>,
    pub locator: Locator,
    pub title: String,
    /// Original release label for display; never canonical title evidence.
    pub release_title: Option<String>,
    pub kind: MediaKind,
    pub year: Option<u16>,
    pub episode: Option<EpisodeNumber>,
    #[serde(default)]
    pub external: ExternalMetadata,
    pub quality: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// Provider-reported pixel height, not a measurement.
    pub resolution: Option<u16>,
    /// Language tags, not inferred from the provider's country or release title.
    pub languages: Vec<String>,
    pub size: Option<u64>,
    pub seeds: Option<u32>,
    pub peers: Option<u32>,
    /// Caller-supplied Unix seconds; zero means not supplied. Core reads no clock.
    pub fetched_at: i64,
    /// Computed by `match_source`; prefiltered adapters may assign their accuracy.
    pub match_score: u8,
    /// v1 hex/base32 or v2 SHA-256 hex input; normalized to lowercase hex.
    /// A hybrid magnet's additional hash is retained in its locator for dedup.
    pub info_hash: Option<String>,
}

impl fmt::Debug for MediaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaSource([redacted])")
    }
}

impl MediaSource {
    /// Minimal offer with unknown metadata. The caller can supply metadata and
    /// normalize again, or let `rank_and_dedup` normalize the completed offer.
    pub fn new(
        provider: impl Into<String>,
        locator: Locator,
        title: impl Into<String>,
        kind: MediaKind,
    ) -> Result<Self, SourceError> {
        Self {
            id: String::new(),
            provider: provider.into(),
            provider_item_id: None,
            origins: Vec::new(),
            license: None,
            locator,
            title: title.into(),
            release_title: None,
            kind,
            year: None,
            episode: None,
            external: ExternalMetadata::default(),
            quality: None,
            video_codec: None,
            audio_codec: None,
            resolution: None,
            languages: Vec::new(),
            size: None,
            seeds: None,
            peers: None,
            fetched_at: 0,
            match_score: 0,
            info_hash: None,
        }
        .normalize()
    }

    pub fn normalize(mut self) -> Result<Self, SourceError> {
        self.locator = self.locator.validate()?;
        if !valid_text(&self.provider, 100)
            || !valid_text(&self.title, 2_000)
            || self.origins.iter().any(|origin| !valid_text(origin, 4_096))
            || self
                .license
                .as_ref()
                .is_some_and(|notice| !valid_text(notice, 2048))
            || self
                .quality
                .as_ref()
                .is_some_and(|text| !valid_text(text, 100))
            || self
                .provider_item_id
                .as_ref()
                .is_some_and(|text| !valid_text(text, 4_096))
            || self
                .release_title
                .as_ref()
                .is_some_and(|text| !valid_text(text, 2_000))
            || self
                .video_codec
                .as_ref()
                .is_some_and(|text| !valid_text(text, 100))
            || self
                .audio_codec
                .as_ref()
                .is_some_and(|text| !valid_text(text, 100))
            || self.fetched_at < 0
            || self.match_score > 100
            || self.languages.len() > 100
            || self.languages.iter().any(|tag| !valid_language(tag))
            || self.resolution == Some(0)
            || self.year.is_some_and(|year| !(1800..2200).contains(&year))
            || (self.kind == MediaKind::Movie && self.episode.is_some())
        {
            return Err(SourceError::InvalidMetadata);
        }
        self.provider = display_text(&self.provider);
        self.title = display_text(&self.title);
        self.quality = self.quality.map(|text| display_text(&text));
        self.release_title = self.release_title.map(|text| display_text(&text));
        self.video_codec = self.video_codec.map(|text| display_text(&text));
        self.audio_codec = self.audio_codec.map(|text| display_text(&text));
        self.external = self.external.normalized();
        self.languages = self
            .languages
            .into_iter()
            .map(|tag| tag.to_ascii_lowercase())
            .collect();
        self.languages.sort();
        self.languages.dedup();
        self.origins = self
            .origins
            .into_iter()
            .map(|text| display_text(&text))
            .collect();
        self.origins.push(self.provider.clone());
        self.origins.sort();
        self.origins.dedup();

        let mut hashes = self.locator.hashes();
        self.info_hash = self.info_hash.as_deref().map(normalize_hash).transpose()?;
        if let Some(hash) = &self.info_hash {
            if matches!(self.locator, Locator::Http(_)) {
                return Err(SourceError::InvalidMetadata);
            }
            if hashes
                .iter()
                .any(|other| other.len() == hash.len() && other != hash)
            {
                return Err(SourceError::ConflictingInfoHashes);
            }
            hashes.insert(hash.clone());
        } else {
            self.info_hash = preferred_hash(&hashes);
        }
        // V1 identity wins when both versions were reported, as in ManualSource.
        // Keep the original locator and explicit hash rather than rewrite a
        // provider's descriptor or lose its additional observed hash version.
        self.id = preferred_hash(&hashes).as_ref().map_or_else(
            || {
                self.provider_identity()
                    .unwrap_or_else(|| self.locator.identity())
            },
            |hash| hash_identity(hash),
        );
        Ok(self)
    }

    /// Automatic film torrents use seed-first ordering. Explicit user bindings,
    /// direct HTTP resources and episodic offers retain their existing policy.
    pub fn is_discovered_movie_torrent(&self) -> bool {
        self.kind == MediaKind::Movie
            && !matches!(self.locator, Locator::Http(_))
            && !matches!(self.provider.as_str(), "manual-http" | "manual-torrent")
    }

    fn hashes(&self) -> BTreeSet<String> {
        let mut hashes = self.locator.hashes();
        hashes.extend(self.info_hash.clone());
        hashes
    }

    fn identities(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::from([self.locator.identity()]);
        keys.extend(self.provider_identity());
        keys.extend(self.hashes().iter().map(|hash| hash_identity(hash)));
        keys
    }

    fn provider_identity(&self) -> Option<String> {
        self.provider_item_id.as_ref().map(|id| {
            digest_id(
                if matches!(self.locator, Locator::Http(_)) {
                    "http-provider-item"
                } else {
                    "torrent-provider-item"
                },
                &format!("{}\0{id}", self.provider),
            )
        })
    }
}

/// Requested values in descending preference order. Empty lists use descending
/// known resolution and no language preference. Unknown values never become zero.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct SourcePreferences {
    pub resolutions: Vec<u16>,
    pub languages: Vec<String>,
}

impl fmt::Debug for SourcePreferences {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SourcePreferences([redacted])")
    }
}

/// Exact ID match outranks textual evidence, but any comparable ID conflict or
/// contradictory movie year is a veto. Kind and exact episode scope are mandatory.
/// Without an ID match, compare full normalized title tokens (including aliases);
/// movies additionally require a known, identical year. No substring/fuzzy match.
pub fn matches_query(query: &MediaQuery, source: &MediaSource) -> bool {
    match_score(query, source).is_some()
}

/// Explicit multi-film labels in release metadata are not a single-film offer.
/// Inspect only the suffix beginning with a year/metadata group, not the literal
/// film name: titles such as "The Collection" must remain eligible.
pub fn movie_release_has_pack_suffix(release: &str) -> bool {
    let year = release.char_indices().find_map(|(index, _)| {
        let token = release.get(index..index + 4)?;
        (index > 0
            && token.bytes().all(|byte| byte.is_ascii_digit())
            && token
                .parse::<u16>()
                .is_ok_and(|year| (1800..=2200).contains(&year))
            && !release[..index].ends_with(char::is_alphanumeric)
            && !release[index + 4..].starts_with(char::is_alphanumeric))
        .then_some(index)
    });
    let start = release.find(['[', '(']).into_iter().chain(year).min();
    let Some(start) = start else {
        return false;
    };
    let tokens: Vec<_> = release[start..]
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect();
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "pack"
                | "collection"
                | "collections"
                | "anthology"
                | "trilogy"
                | "duology"
                | "tetralogy"
                | "filmography"
                | "сборник"
                | "коллекция"
                | "антология"
                | "трилогия"
                | "дилогия"
                | "фильмография"
        )
    }) || tokens.windows(2).any(|pair| {
        matches!(
            (pair[0].as_str(), pair[1].as_str()),
            ("complete" | "all", "films" | "movies") | ("все", "фильмы")
        )
    })
}

/// Accuracy: 100 for external IDs, 90 for primary/original title, 80 for an
/// alias on either side. Every textual movie match requires the exact year.
pub fn match_score(query: &MediaQuery, source: &MediaSource) -> Option<u8> {
    if query.kind != source.kind
        || query.episode != source.episode
        || (query.kind == MediaKind::Series && query.episode.is_none())
        || (query.kind == MediaKind::Movie && query.episode.is_some())
    {
        return None;
    }
    if query.kind == MediaKind::Movie && query.year.zip(source.year).is_some_and(|(a, b)| a != b) {
        return None;
    }
    let query_external = query.external.clone().normalized();
    let source_external = source.external.clone().normalized();
    let mut id_match = false;
    if let (Some(left), Some(right)) = (&query_external.imdb_id, &source_external.imdb_id) {
        if left != right {
            return None;
        }
        id_match = true;
    }
    if let (Some(left), Some(right)) = (query_external.tmdb_id, source_external.tmdb_id) {
        if left != right {
            return None;
        }
        id_match = true;
    }
    if id_match {
        return Some(100);
    }
    if query.kind == MediaKind::Movie && (query.year.is_none() || query.year != source.year) {
        return None;
    }
    if query.year.zip(source.year).is_some_and(|(a, b)| a != b) {
        return None;
    }
    let primary_titles: BTreeSet<_> = std::iter::once(query.title.as_str())
        .chain(query.original_title.as_deref())
        .map(title_key)
        .filter(|key| !key.is_empty())
        .collect();
    if primary_titles.contains(&title_key(&source.title)) {
        return Some(90);
    }
    let query_titles: BTreeSet<_> = std::iter::once(query.title.as_str())
        .chain(query.original_title.as_deref())
        .chain(query_external.alternative_titles.iter().map(String::as_str))
        .map(title_key)
        .filter(|key| !key.is_empty())
        .collect();
    std::iter::once(source.title.as_str())
        .chain(
            source_external
                .alternative_titles
                .iter()
                .map(String::as_str),
        )
        .map(title_key)
        .any(|key| !key.is_empty() && query_titles.contains(&key))
        .then_some(80)
}

/// Normalize a provider row and assign its accuracy before persistence.
pub fn match_source(query: &MediaQuery, source: MediaSource) -> Option<MediaSource> {
    let mut source = source.normalize().ok()?;
    source.match_score = match_score(query, &source)?;
    Some(source)
}

/// Validate, match, then rank/deduplicate. Does not do IO or resolve locators.
pub fn select_sources(
    query: &MediaQuery,
    sources: Vec<MediaSource>,
    preferences: &SourcePreferences,
) -> Vec<MediaSource> {
    rank_and_dedup(
        sources
            .into_iter()
            .filter_map(|source| match_source(query, source))
            .collect(),
        preferences,
    )
}

/// Also serves already-matched provider offers: no generic title/episode matcher.
/// Invalid offers are omitted. Equal locators and observed content hashes dedup,
/// including hybrid hash aliases. Different HTTP queries and HTTP/metainfo roles
/// stay distinct. All origins survive; metadata comes from one deterministic
/// representative, not a fabricated mixture of contradictory metadata. Dedup is
/// intentionally batch-local: persist provider batches, not merged identity aliases.
/// Discovered movie torrents use descending known seeds (positive, zero, unknown)
/// before accuracy/quality preferences, both for dedup and their display order.
/// Discovered film torrents precede other rows. Direct HTTP/manual and episodic
/// offers retain their legacy comparison within the remaining rows.
pub fn rank_and_dedup(
    sources: Vec<MediaSource>,
    preferences: &SourcePreferences,
) -> Vec<MediaSource> {
    let mut sources: Vec<_> = sources
        .into_iter()
        .filter_map(|source| source.normalize().ok())
        .collect();
    let representative_order = |left: &MediaSource, right: &MediaSource| {
        // Partitioning keeps this a transitive mixed comparator. An automatic
        // provider's known availability must survive a duplicate manual binding.
        right
            .is_discovered_movie_torrent()
            .cmp(&left.is_discovered_movie_torrent())
            .then_with(|| {
                if left.is_discovered_movie_torrent() {
                    right.seeds.cmp(&left.seeds)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| compare_sources(left, right, preferences))
    };
    sources.sort_by(&representative_order);
    let mut groups: Vec<Group> = Vec::new();
    for source in sources {
        let mut group = Group {
            keys: source.identities(),
            hashes: source.hashes(),
            source,
        };
        // Restart after a merge: one hybrid/locator may connect earlier groups.
        let mut index = 0;
        while index < groups.len() {
            if !group.keys.is_disjoint(&groups[index].keys)
                && compatible_hashes(&group.hashes, &groups[index].hashes)
            {
                let mut other = groups.remove(index);
                group.keys.extend(other.keys);
                group.hashes.extend(other.hashes);
                let mut origins = other.source.origins.clone();
                origins.extend(group.source.origins.clone());
                if representative_order(&other.source, &group.source).is_lt() {
                    std::mem::swap(&mut group.source, &mut other.source);
                }
                // A manual attachment can win the ranking for the same HTTP
                // resource. Choosing it must not remove the licensor's credit.
                if group.source.license.is_none() && group.source.locator == other.source.locator {
                    group.source.license = other.source.license;
                }
                origins.sort();
                origins.dedup();
                group.source.origins = origins;
                index = 0;
            } else {
                index += 1;
            }
        }
        groups.push(group);
    }
    let mut result: Vec<_> = groups
        .into_iter()
        .map(|mut group| {
            // Preserve the complete ranked representative, including its locator,
            // hash and identity. Other provider evidence is not credential authority.
            group.source.origins.sort();
            group.source
        })
        .collect();
    result.sort_by(&representative_order);
    let mut unique: Vec<MediaSource> = Vec::new();
    for source in result {
        // Conflicting secondary hash claims can prevent grouping while sharing
        // a primary stable identity. Keep the best complete offer, never expose
        // duplicate public IDs or transfer conflicting hash aliases.
        if let Some(existing) = unique.iter_mut().find(|existing| existing.id == source.id) {
            existing.origins.extend(source.origins);
            existing.origins.sort();
            existing.origins.dedup();
        } else {
            unique.push(source);
        }
    }
    unique
}

struct Group {
    source: MediaSource,
    keys: BTreeSet<String>,
    hashes: BTreeSet<String>,
}

fn compatible_hashes(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    !left.iter().any(|left| {
        right
            .iter()
            .any(|right| left.len() == right.len() && left != right)
    })
}

fn preferred_hash(hashes: &BTreeSet<String>) -> Option<String> {
    hashes
        .iter()
        .min_by_key(|hash| (hash.len(), *hash))
        .cloned()
}

fn compare_sources(left: &MediaSource, right: &MediaSource, prefs: &SourcePreferences) -> Ordering {
    let resolution = |source: &MediaSource| {
        if prefs.resolutions.is_empty() {
            return 0;
        }
        source
            .resolution
            .map(|value| {
                prefs
                    .resolutions
                    .iter()
                    .position(|v| *v == value)
                    .unwrap_or(prefs.resolutions.len())
            })
            .unwrap_or(prefs.resolutions.len() + 1)
    };
    let language = |source: &MediaSource| {
        if prefs.languages.is_empty() {
            return 0;
        }
        prefs
            .languages
            .iter()
            .position(|wanted| {
                source
                    .languages
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(prefs.languages.len() + usize::from(source.languages.is_empty()))
    };
    let availability = |source: &MediaSource| {
        if source.seeds.is_some_and(|n| n > 0) || source.peers.is_some_and(|n| n > 0) {
            2
        } else if source.seeds.is_none() || source.peers.is_none() {
            1
        } else {
            0
        }
    };
    right
        .match_score
        .cmp(&left.match_score)
        .then_with(|| availability(right).cmp(&availability(left)))
        .then_with(|| resolution(left).cmp(&resolution(right)))
        .then_with(|| language(left).cmp(&language(right)))
        .then_with(|| right.resolution.cmp(&left.resolution))
        .then_with(|| right.seeds.cmp(&left.seeds))
        .then_with(|| left.id.cmp(&right.id))
        .then_with(|| stable_key(left).cmp(&stable_key(right)))
}

// Every field participates in the tie-break, including origins. A structural
// tuple avoids unstable hashes or concatenation ambiguities. Debug is never used.
fn stable_key(source: &MediaSource) -> impl Ord + '_ {
    let locator = match &source.locator {
        Locator::RuTracker(id) => (0, id.to_string().into_bytes()),
        Locator::Magnet(value) => (1, value.as_bytes().to_vec()),
        Locator::Http(value) => (2, value.as_bytes().to_vec()),
        Locator::TorrentUrl(value) => (3, value.as_bytes().to_vec()),
        Locator::TorrentFile(path) => (4, path.as_os_str().as_encoded_bytes().to_vec()),
    };
    (
        (
            &source.provider,
            locator,
            &source.title,
            source.kind == MediaKind::Series,
        ),
        (
            source.year,
            source.episode,
            &source.external.imdb_id,
            source.external.tmdb_id,
        ),
        (
            &source.external.alternative_titles,
            &source.quality,
            &source.languages,
        ),
        (source.size, &source.info_hash, &source.origins),
        (
            &source.provider_item_id,
            &source.release_title,
            &source.video_codec,
            &source.audio_codec,
        ),
        (source.peers, source.fetched_at, &source.license),
    )
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= limit
        && !value.chars().any(char::is_control)
}

fn display_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn valid_language(value: &str) -> bool {
    value.len() <= 35
        && !value.is_empty()
        && value
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn title_key(value: &str) -> String {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_hash(value: &str) -> Result<String, SourceError> {
    let value = value.to_ascii_lowercase();
    if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(value);
    }
    decode_btih(&value)
        .map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect())
        .ok_or(SourceError::InvalidInfoHash)
}

fn hash_identity(hash: &str) -> String {
    let prefix = if hash.len() == 40 {
        "urn:btih:"
    } else {
        "urn:btmh:1220"
    };
    ManualSource::parse_link(&format!("magnet:?xt={prefix}{hash}"))
        .expect("normalized info hash")
        .id()
        .as_str()
        .to_owned()
}

fn digest_id(domain: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("media-source:v1:{domain}:{:x}", digest.finalize())
}

/// Hash opaque provider GUIDs before persistence; the resulting label is not a credential.
pub fn provider_item_digest(value: &str) -> String {
    digest_id("provider-guid", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_file_locators_validate_without_io_and_hide_paths() {
        let path = std::env::temp_dir()
            .join("private-user")
            .join("not-present.torrent");
        let manual = ManualSource::torrent_file(path.clone()).unwrap();
        let locator = Locator::TorrentFile(path.clone()).validate().unwrap();
        let source =
            MediaSource::new("manual-torrent", locator.clone(), "Movie", MediaKind::Movie).unwrap();
        assert_eq!(source.id, manual.id().as_str());
        assert!(source.id.starts_with("source:v1:torrent-path:"));
        assert_eq!(format!("{locator:?}"), "TorrentFile([redacted])");
        assert!(!format!("{source:?}").contains("private-user"));
        assert_eq!(source.clone().normalize().unwrap(), source);
        assert_eq!(Locator::TorrentFile(path).validate().unwrap(), locator);
    }

    #[test]
    fn torrent_file_locators_reject_invalid_paths() {
        for path in [
            PathBuf::from("relative.torrent"),
            std::env::temp_dir().join("video.mp4"),
            std::env::temp_dir().join("..").join("private.torrent"),
        ] {
            assert_eq!(
                Locator::TorrentFile(path).validate(),
                Err(SourceError::InvalidLocator)
            );
        }
        for input in ["file:///private.torrent", "/private.torrent"] {
            assert_eq!(
                Locator::Http(input.into()).validate(),
                Err(SourceError::InvalidLocator)
            );
            assert_eq!(
                Locator::TorrentUrl(input.into()).validate(),
                Err(SourceError::InvalidLocator)
            );
        }
    }

    #[test]
    fn torrent_file_paths_normalize_and_dedup_by_attachment_or_observed_hash() {
        let path = std::env::temp_dir().join("private-user").join("a.torrent");
        let mut a = MediaSource::new(
            "manual-torrent",
            Locator::TorrentFile(path.clone()),
            "Movie",
            MediaKind::Movie,
        )
        .unwrap();
        let b = MediaSource::new(
            "manual-torrent",
            Locator::TorrentFile(path.parent().unwrap().join(".").join("a.torrent")),
            "Movie",
            MediaKind::Movie,
        )
        .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(
            rank_and_dedup(vec![a.clone(), b], &SourcePreferences::default()).len(),
            1
        );
        let c = MediaSource::new(
            "manual-torrent",
            Locator::TorrentFile(path.with_file_name("b.torrent")),
            "Movie",
            MediaKind::Movie,
        )
        .unwrap();
        assert_ne!(a.id, c.id);
        assert_eq!(
            rank_and_dedup(vec![a.clone(), c], &SourcePreferences::default()).len(),
            2
        );
        let hash = "a".repeat(40);
        a.info_hash = Some(hash.clone());
        let magnet = MediaSource::new(
            "network",
            Locator::Magnet(format!("magnet:?xt=urn:btih:{hash}")),
            "Movie",
            MediaKind::Movie,
        )
        .unwrap();
        assert_eq!(
            rank_and_dedup(vec![a, magnet], &SourcePreferences::default()).len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn torrent_file_identity_preserves_non_utf8_path_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let path = |byte| {
            std::env::temp_dir().join(OsString::from_vec(vec![
                byte, b'.', b't', b'o', b'r', b'r', b'e', b'n', b't',
            ]))
        };
        let a = Locator::TorrentFile(path(0xfe)).validate().unwrap();
        let b = Locator::TorrentFile(path(0xff)).validate().unwrap();
        assert_ne!(a.identity(), b.identity());
        assert_eq!(
            a.identity(),
            ManualSource::torrent_file(path(0xfe))
                .unwrap()
                .id()
                .as_str()
        );
    }
}
