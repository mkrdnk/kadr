//! Local persistence for kadr. All operations are synchronous.
//!
//! Own a [`Storage`] on a dedicated background worker; never open it or call it
//! from a GPUI render callback. IDs are stable opaque keys, not source URLs or
//! file paths. No network, playback inference, or telemetry is performed here.
//!
//! # Source resolver (schema v8)
//!
//! [`SourcePreferences`] is independent of [`Settings`]. RuTracker defaults to
//! enabled to preserve availability, but the caller must still enforce explicit
//! BitTorrent/catalog permissions. Torznab defaults to no endpoints. Saving a
//! changed preference snapshot clears resolver results in the same transaction;
//! an unchanged save preserves them. Only API-key environment **names** belong
//! in configurations; resolving their values is the network adapter's job.
//!
//! v7 adds strict `source_preferences`, ordered `torznab_configs` and
//! `source_cache` tables. v8 adds independent `commons_enabled`: true on clean
//! installs, false on all pre-v8 upgrades to avoid expanding prior consent.
//! Explicit opt-in/out survives restarts. Commons needs catalog consent, not
//! BitTorrent or credentials. The migration chain remains one transaction,
//! preserving settings/history and refusing newer schemas without changing them.
//! Resolver results live in SQLite, not the media file cache; no media files are
//! moved or created by resolver preference/cache writes.
//!
//! Cache payloads are normalized results encoded outside storage. Cache only
//! successful responses, never network/authentication/decoding failures. Positive
//! results expire after 1800 seconds and successful empty results after 300.
//! Reads/writes remove expired rows; writes enforce a 1 MiB payload limit, 1024
//! rows and 16 MiB total payload bytes with oldest-write eviction. These limits
//! do not bound SQLite file size or securely erase private data from old pages.
//!
//! ```no_run
//! use kadr_storage::{SourcePreferences, Storage, TorznabConfig};
//!
//! fn configure_on_worker(storage: &mut Storage) -> kadr_storage::Result<()> {
//!     let mut preferences = storage.load_source_preferences()?;
//!     preferences.torznab.push(TorznabConfig {
//!         name: "Local indexer".into(),
//!         base_url: "http://127.0.0.1:9117/api/v2".into(),
//!         api_key_env: Some("KADR_TORZNAB_LOCAL_KEY".into()),
//!         enabled: true,
//!     });
//!     storage.save_source_preferences(&preferences)?;
//!     // After successful resolution, encode normalized results outside storage:
//!     let encoded_empty_result = b"[]";
//!     storage.source_cache_put(
//!         "catalog:movie:42", "torznab:local", 1_700_000_000, true, encoded_empty_result,
//!     )?;
//!     let _encoded = storage.source_cache_get(
//!         "catalog:movie:42", "torznab:local", 1_700_000_001,
//!     )?;
//!     Ok(())
//! }
//! ```

mod error;
mod models;
mod paths;
pub mod rutracker_credentials;
mod settings;
mod snapshots;
mod source_resolver;
mod storage;

pub use error::{Error, Result};
pub use models::{
    EpisodeKey, LibraryEntry, LibraryItem, MediaId, MediaKind, MediaRecord, PlaybackProgress,
    PlaybackTarget, ProgressOrigin, SavedFile, SourceId, StoredSource,
};
pub use paths::AppPaths;
pub use settings::{NetworkLimits, PlayerPreference, RateLimit, Settings};
pub use source_resolver::{SourcePreferences, TorznabConfig};
pub use storage::{SCHEMA_VERSION, Storage};
