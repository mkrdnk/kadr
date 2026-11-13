//! Local persistence for kadr. All operations are synchronous disk I/O.
//!
//! Own a [`Storage`] on a dedicated background worker; never open it or call it
//! from a GPUI render callback. IDs are stable opaque keys, not source URLs or
//! file paths. No network, playback inference, or telemetry is performed here.

mod error;
mod models;
mod paths;
mod settings;
mod storage;

pub use error::{Error, Result};
pub use models::{
    EpisodeKey, LibraryEntry, MediaId, PlaybackProgress, PlaybackTarget, ProgressOrigin, SourceId,
};
pub use paths::AppPaths;
pub use settings::{NetworkLimits, PlayerPreference, RateLimit, Settings};
pub use storage::{SCHEMA_VERSION, Storage};
