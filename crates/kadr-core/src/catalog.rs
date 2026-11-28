//! Provider-neutral catalog data. Descriptions are plain display text, never HTML.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MediaKind {
    Movie,
    Series,
}

/// Optional provider-reported identifiers and aliases, never inferred from a title.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ExternalMetadata {
    pub imdb_id: Option<String>,
    pub tmdb_id: Option<u64>,
    pub alternative_titles: Vec<String>,
}

impl ExternalMetadata {
    /// Bound untrusted aliases and discard malformed identifiers. Does not fetch metadata.
    pub fn normalized(mut self) -> Self {
        self.imdb_id = self.imdb_id.filter(|id| valid_imdb_id(id));
        self.tmdb_id = self.tmdb_id.filter(|id| *id > 0);
        let mut seen = std::collections::BTreeSet::new();
        self.alternative_titles = self
            .alternative_titles
            .into_iter()
            .take(100)
            .filter(|title| title.chars().count() <= 500 && !title.chars().any(char::is_control))
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|title| !title.is_empty() && seen.insert(title.to_lowercase()))
            .take(20)
            .collect();
        self
    }
}

pub(crate) fn valid_imdb_id(id: &str) -> bool {
    id.strip_prefix("tt").is_some_and(|digits| {
        (7..=10).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The original rating service, even when the score is delivered by Trakt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RatingSource {
    Imdb,
    Tmdb,
    Metascore,
    RottenTomatoes,
    RottenTomatoesAudience,
    Letterboxd,
    MyAnimeList,
}

impl RatingSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Imdb => "IMDb",
            Self::Tmdb => "TMDB",
            Self::Metascore => "Metascore",
            Self::RottenTomatoes => "Rotten Tomatoes",
            Self::RottenTomatoesAudience => "Rotten Tomatoes Audience",
            Self::Letterboxd => "Letterboxd",
            Self::MyAnimeList => "MyAnimeList",
        }
    }
}

/// A provider-reported score on its original scale, not an inferred Trakt score.
/// Both values use fixed-point tenths: IMDb 8.3/10 is `83 / 100`, while
/// Rotten Tomatoes 83/100 is `830 / 1000`. No external links are persisted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalRating {
    pub source: RatingSource,
    pub rating_tenths: u16,
    pub max_tenths: u16,
    pub votes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Media {
    /// Stable, storage-safe, namespaced provider identifier, not a title.
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    /// Missing in older snapshots; absence does not mean a negative ID match.
    #[serde(default)]
    pub external: ExternalMetadata,
    /// Provider-reported score out of ten, rounded to one decimal (0–100).
    /// None means absent or unsupported; never inferred from popularity or search rank.
    #[serde(default)]
    pub rating_tenths: Option<u8>,
    #[serde(default)]
    pub runtime_minutes: Option<u16>,
    /// Provider genre slugs, not localized display labels.
    #[serde(default)]
    pub genres: Vec<String>,
    /// Provider certification (for example PG-13), not a local age assessment.
    #[serde(default)]
    pub certification: Option<String>,
    /// Optional detail enrichment; an empty list never means zero scores.
    #[serde(default)]
    pub external_ratings: Vec<ExternalRating>,
    pub description: String,
    pub poster_url: Option<String>,
    /// Public provider image, independent of the poster; absent in older records.
    #[serde(default)]
    pub backdrop_url: Option<String>,
    pub kind: MediaKind,
    /// Populated by series details; search cards need not contain episodes.
    pub episodes: Vec<Episode>,
    /// Display with a link to `source_page`; do not silently discard on export.
    pub attribution: String,
    /// Provider metadata license or uploader-declared work license, not a rights guarantee.
    pub license: Option<String>,
    pub source_page: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Episode {
    pub season: u16,
    pub number: u16,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub runtime_minutes: Option<u16>,
    /// Only accepted public provider images, never a URL inferred from an ID.
    #[serde(default)]
    pub screenshot_url: Option<String>,
}

/// A provider-offered file, not an instruction to start playback or a download.
/// Revalidate `url` with `ManualSource::parse_link` at the playback boundary.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceCandidate {
    pub url: String,
    pub title: String,
    /// Provider-reported quality/format, not a measured resolution.
    pub quality: Option<String>,
    pub language: Option<String>,
    pub license: String,
    /// Attribution and provenance, including the public item page and creator.
    pub origin: String,
}

impl fmt::Debug for SourceCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceCandidate")
            .field("url", &"[redacted]")
            .field("title", &self.title)
            .field("quality", &self.quality)
            .field("language", &self.language)
            .field("license", &self.license)
            .field("origin", &self.origin)
            .finish()
    }
}
