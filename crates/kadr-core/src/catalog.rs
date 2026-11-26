//! Provider-neutral catalog data. Descriptions are plain display text, never HTML.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MediaKind {
    Movie,
    Series,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Media {
    /// Stable, storage-safe, namespaced provider identifier, not a title.
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    /// Provider-reported score out of ten, rounded to one decimal (0–100).
    /// None means absent or unsupported; never inferred from popularity or search rank.
    #[serde(default)]
    pub rating_tenths: Option<u8>,
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
