use std::{fmt, path::PathBuf};

use crate::{Error, Result};

macro_rules! opaque_id {
    ($name:ident, $field:literal) => {
        /// Stable opaque application key, not a URL or filesystem path.
        /// Debug hides the value; explicit `as_str` is for intentional use.
        #[derive(Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                if value.is_empty()
                    || value.len() > 256
                    || !value.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')
                    })
                {
                    return Err(Error::InvalidInput($field));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }
    };
}

opaque_id!(MediaId, "media ID");
opaque_id!(SourceId, "source ID");

/// User-approved input retained for reopening. Contains secrets: never log fields.
#[derive(Clone, PartialEq, Eq)]
pub enum StoredSource {
    Link(String),
    /// Magnet input with an optional selected file in a multi-file torrent.
    TorrentLink {
        uri: String,
        file_id: Option<usize>,
    },
    TorrentFile {
        path: PathBuf,
        file_id: Option<usize>,
    },
}

impl StoredSource {
    /// Validates through the same rules and identity implementation as manual input.
    /// Does not read files or contact the network.
    pub fn manual_source(&self) -> Result<kadr_core::source::ManualSource> {
        use kadr_core::source::ManualSource;
        match self {
            Self::Link(link) => ManualSource::parse_link(link),
            Self::TorrentLink { uri, file_id } => {
                if file_id.is_some_and(|id| i64::try_from(id).is_err()) {
                    return Err(Error::InvalidInput("torrent file ID"));
                }
                let source = ManualSource::parse_link(uri)
                    .map_err(|_| Error::InvalidInput("stored source"))?;
                if source.magnet_uri().is_none() {
                    return Err(Error::InvalidInput("torrent link"));
                }
                Ok(source)
            }
            Self::TorrentFile { path, file_id } => {
                if file_id.is_some_and(|id| i64::try_from(id).is_err()) {
                    return Err(Error::InvalidInput("torrent file ID"));
                }
                ManualSource::torrent_file(path.clone())
            }
        }
        .map_err(|_| Error::InvalidInput("stored source"))
    }

    pub fn source_id(&self) -> Result<SourceId> {
        SourceId::new(self.manual_source()?.id().as_str())
    }
}

impl fmt::Debug for StoredSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredSource([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Movie,
    Series,
}

/// Local display snapshot. Titles may contain private filenames; Debug is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaRecord {
    pub id: MediaId,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub kind: MediaKind,
    pub description: String,
    pub poster_url: Option<String>,
    pub attribution: String,
}

impl MediaRecord {
    pub fn validate(&self) -> Result<()> {
        let text_valid = |text: &str, limit: usize| text.len() <= limit && !text.contains('\0');
        if self.title.trim().is_empty()
            || !text_valid(&self.title, 4096)
            || self
                .original_title
                .as_ref()
                .is_some_and(|title| !text_valid(title, 4096))
            || !text_valid(&self.description, 131_072)
            || !text_valid(&self.attribution, 4096)
            || self.year.is_some_and(|year| !(1..=9999).contains(&year))
        {
            return Err(Error::InvalidInput("media snapshot"));
        }
        if let Some(url) = &self.poster_url {
            let source = kadr_core::source::ManualSource::parse_link(url)
                .map_err(|_| Error::InvalidInput("poster URL"))?;
            if url.len() > 16_384 || source.http_url().is_none() {
                return Err(Error::InvalidInput("poster URL"));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for MediaRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaRecord([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryItem {
    pub entry: LibraryEntry,
    pub media: Option<MediaRecord>,
    /// Resume identity even when history clearing removed measured progress.
    pub target: PlaybackTarget,
    /// Actual successful open of this exact target, never inferred from progress.
    pub opened_at: Option<i64>,
    /// Latest measured target, including its episode identity.
    pub progress: Option<PlaybackProgress>,
    /// Descriptor matching progress, otherwise the actual open or unplayed link.
    pub source: Option<StoredSource>,
}

/// A completed user-owned copy, independent of disposable cache and source input.
#[derive(Clone, PartialEq, Eq)]
pub struct SavedFile {
    pub target: PlaybackTarget,
    pub path: PathBuf,
    pub saved_at: i64,
}

impl SavedFile {
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        validate_timestamp(self.saved_at)?;
        crate::paths::validate_private_path(&self.path)
    }
}

impl fmt::Debug for SavedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SavedFile([redacted])")
    }
}

/// Season zero supports specials; episode numbers start at one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EpisodeKey {
    pub season: u16,
    pub episode: u16,
}

impl EpisodeKey {
    pub fn new(season: u16, episode: u16) -> Result<Self> {
        let key = Self { season, episode };
        key.validate()?;
        Ok(key)
    }

    pub(crate) fn validate(self) -> Result<()> {
        if self.episode == 0 {
            return Err(Error::InvalidInput("episode number"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlaybackTarget {
    pub media_id: MediaId,
    /// None is item-level (e.g. movie); Some is isolated per series episode.
    pub episode: Option<EpisodeKey>,
}

impl PlaybackTarget {
    pub(crate) fn validate(&self) -> Result<()> {
        if let Some(episode) = self.episode {
            episode.validate()?;
        }
        Ok(())
    }

    pub(crate) fn sql_episode(&self) -> (i64, i64) {
        self.episode
            .map(|key| (i64::from(key.season), i64::from(key.episode)))
            .unwrap_or((-1, -1))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub media_id: MediaId,
    pub favorite: bool,
    /// Unix seconds. Merely opening an item records history, not progress.
    pub last_opened_at: Option<i64>,
    pub last_source_id: Option<SourceId>,
}

/// The adapter must explicitly identify measured telemetry. Browser playback
/// has no position contract and is rejected by save_progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressOrigin {
    Mpv,
    Vlc,
    Browser,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackProgress {
    pub target: PlaybackTarget,
    pub source_id: SourceId,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub updated_at: i64,
    pub completed: bool,
    pub origin: ProgressOrigin,
}

impl PlaybackProgress {
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        validate_timestamp(self.updated_at)?;
        if self.origin == ProgressOrigin::Browser {
            return Err(Error::InvalidInput("browser playback telemetry"));
        }
        if !self.position_seconds.is_finite() || self.position_seconds < 0.0 {
            return Err(Error::InvalidInput("playback position"));
        }
        if let Some(duration) = self.duration_seconds
            && (!duration.is_finite() || duration <= 0.0 || self.position_seconds > duration)
        {
            return Err(Error::InvalidInput("playback duration"));
        }
        Ok(())
    }
}

pub(crate) fn validate_timestamp(timestamp: i64) -> Result<()> {
    if timestamp < 0 {
        return Err(Error::InvalidInput("timestamp"));
    }
    Ok(())
}
