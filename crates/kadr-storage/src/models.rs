use std::fmt;

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
