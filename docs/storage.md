# Storage foundation (P1 / P4)

`kadr-storage` is a standalone synchronous persistence adapter. It depends on
`rusqlite` with bundled SQLite and `directories`, not GPUI, `kadr-core`, a network
runtime, or a player. It implements the persistence part of SPEC §7 and §8,
not a complete library/cache or UI feature.

## Background-worker contract

Own one `Storage` on a dedicated background worker and send it commands.
Opening, directory creation, migrations, reads, writes and closing a connection
may block. **Never call them from GPUI render or on the UI event thread.**
Return snapshots/results to the UI. SQLite has a five-second lock wait; report
errors to the user rather than resetting the database. The connection is not
shared concurrently, and transactions do not span UI/network/player work.

```rust
use kadr_storage::{
    AppPaths, MediaId, PlaybackProgress, PlaybackTarget, ProgressOrigin,
    SourceId, Storage,
};

// Run this entire block on the storage worker, not inside a GPUI callback.
fn worker_example() -> kadr_storage::Result<()> {
    let paths = AppPaths::discover()?;
    let mut storage = Storage::open_default(&paths)?;
    let mut settings = storage.load_settings()?;
    settings.cache_limit_bytes = 2 * 1024 * 1024 * 1024;
    storage.save_settings(&settings)?;

    // IDs come from the application. Never substitute a source URL or path.
    let media = MediaId::new("local:stable-item-1")?;
    let source = SourceId::new("source:stable-opaque-1")?;
    let target = PlaybackTarget { media_id: media.clone(), episode: None };
    storage.set_favorite(&media, true)?;
    storage.record_opened(&media, &source, 1_700_000_000)?;
    let _resume = storage.load_progress(&target)?;

    // Only when a managed player actually reports this observation:
    storage.save_progress(&PlaybackProgress {
        target,
        source_id: source,
        position_seconds: 42.0,
        duration_seconds: Some(120.0),
        updated_at: 1_700_000_042,
        completed: false,
        origin: ProgressOrigin::Mpv,
    })?;
    Ok(())
}
```

The API does not generate stable IDs: catalog IDs should be provider-namespaced,
and local items need a persistent application-generated identity. Reuse the same
ID after restart. Reusing an opaque source ID as the media ID is sufficient for
an initially uncatalogued local/manual item. IDs allow 1..=256 ASCII bytes from
`A-Z a-z 0-9 . _ : -`. They are keys, not torrent filenames, URLs, titles, paths,
or a content-addressing implementation.

## Directories and files

`AppPaths::discover()` uses `directories::ProjectDirs::from("", "", "kadr")`:

| Platform | Data / SQLite | Configuration | Disposable cache |
|---|---|---|---|
| Linux | `$XDG_DATA_HOME/kadr`, fallback `~/.local/share/kadr` | `$XDG_CONFIG_HOME/kadr`, fallback `~/.config/kadr` | `$XDG_CACHE_HOME/kadr`, fallback `~/.cache/kadr` |
| macOS | `~/Library/Application Support/kadr` | `~/Library/Application Support/kadr` | `~/Library/Caches/kadr` |

SQLite is `kadr.sqlite3` in the data directory. Settings are in SQLite, not a
second configuration file; the configuration directory is reserved for future
file-based configuration. Video bytes never belong in SQLite.

Discovery does not create directories. `Storage::open_default(&paths)` creates
the app directories and opens the DB. `Storage::open(&absolute_path)` opens a
specific DB but requires its parent to exist.
`AppPaths::from_roots(data, config, cache)` accepts explicit app-specific
absolute roots; it rejects relative paths, parent traversal and NULs. This is
the integration point for future UI-controlled path overrides; this crate does
not persist overrides or relocate existing files.

New directories use Unix mode 0700 and new DB files 0600, further restricted by
the process umask. Existing permissions are not silently changed. Roots are
trusted user choices in user-owned directories: this is not a symlink sandbox.
Never use torrent-provided paths as directory roots or database paths. No API
here extracts torrent files, copies media, or creates saved-media destinations.

## Settings and validation

`Settings` has public editable fields and `validate()`. `save_settings` validates
before an atomic singleton write; `load_settings` validates persisted values.
Invalid/corrupt values produce errors rather than silently resetting defaults.

| Setting | Default | Accepted values |
|---|---|---|
| `default_player` | `PlayerPreference::Mpv` | `Mpv`, `Vlc`, `Browser` |
| `network.torrents_enabled` | `false` | explicit bool |
| `network.download` | 10 MiB/s | `Paused`, `BytesPerSecond(1..=i64::MAX)`, `Unlimited` |
| `network.upload` | `Paused` | same explicit rate-limit enum |
| `network.max_connections` | 64 | 1..=4096 |
| `cache_limit_bytes` | 10 GiB | 1 MiB..=i64::MAX bytes |

These are conservative, **provisional policy defaults**, not measured capacities,
performance SLOs, or enforcement by this crate. BitTorrent remains disabled until
the application explains peer uploading and the user enables it. An eventual
engine must honor explicit pause/limit/unlimited semantics (zero must not turn
into unlimited). No telemetry, network request, source resolution, or background
download is started by opening storage.

## Schema, migrations and failure behavior

SQLite uses the bundled SQLite build, parameter-bound values and strict tables.
The public connection is opaque; callers cannot bypass validation via it.
`PRAGMA user_version` records the schema:

1. Validated singleton settings, initialized from `Settings::default()` only
   for a new v0 database.
2. Library favorites/history and per-item/per-episode measured playback progress.

Opening uses `BEGIN IMMEDIATE`, checks the version, applies the entire missing
migration chain and updates `user_version` in **one transaction**. Any failure
rolls back DDL, data and version changes. Existing settings survive upgrades.
Negative versions are invalid. A future version is refused before any
schema/journal-mode changes; there is no downgrade, drop/recreate, fallback
database or automatic reset. Opening does not request WAL or vacuum.

The journal/synchronization policy is SQLite's default rollback journal with
full synchronization. The adapter enables foreign keys. Each settings/history/
favorite write is a single atomic statement. Progress writes atomically ensure
the owning library row and upsert the observation. `clear_playback_data` is also
transactional. Lock contention, unavailable directories, permissions, full disk
and SQLite failures return sanitized errors. No errors expose raw SQL or SQLite/
OS error strings. Fine-grained retry/recovery UX is left to the application.

## Library, history and progress

- `set_favorite` changes only the favorite flag; `library_entry` looks up one
  item and `favorites` returns favorites in stable ID order.
- `record_opened` records only media ID, source ID and nonnegative Unix seconds.
  It cannot turn an open/launch into fabricated playback telemetry. `history`
  returns 1..=1000 entries, newest first with an ID tie-breaker. Older open
  events cannot overwrite newer history. No title, private URL or path is stored.
- `PlaybackTarget` is media ID plus optional `EpisodeKey`. Season 0 is allowed
  for specials; episodes start at 1. Item-level progress and each season/episode
  are independent. The internal `(-1, -1)` item sentinel is never accepted as
  a public episode.
- `save_progress` accepts finite nonnegative seconds; known duration must be
  finite, positive and at least the position. Unknown duration stays `None`.
  Timestamps must be nonnegative Unix seconds. Completion is explicit adapter/
  application input, never inferred from a launch or percentage threshold.
- `ProgressOrigin::Mpv` / `Vlc` assert genuine observations; the application is
  responsible for adapter trust. `Browser` is explicitly rejected, including
  a guessed zero position. Opening a browser may record history but must not
  overwrite an earlier managed-player resume position.
- Newest timestamp wins independently per target (equal timestamps allow an
  update); stale writes are harmless no-ops. Progress does not implicitly update
  history. Persisted progress is validated again when loaded.
- `clear_playback_data` logically removes all history/progress and retains
  favorites/settings. It is not forensic erasure of free pages, journals,
  backups or filesystem snapshots.

`Debug` for directories, storage handles, IDs and nested library/progress models
does not reveal path/URL/opaque ID values. Errors have no raw underlying source.
Explicit path getters and ID `as_str()` are intentionally available for actual
use: **do not log their output**. The database is local plaintext, not encrypted;
OS access controls and backup policy still matter.

## Tests and remaining scope

From the integrated root workspace:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p kadr-storage
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo clippy --locked -p kadr-storage --all-targets -- -D warnings
cargo fmt --all -- --check
```

The crate tests use temporary databases: reopen/settings persistence, invalid
writes, corrupt reads, successful v1 upgrades, full-chain/v1 migration rollback,
future-version byte preservation, favorites/history independence, movie/episode/
series isolation, finite/range validation, browser rejection, stale event
ordering, clearing playback data, private debug/errors and Unix permissions.
Linux path discovery is checked against the current XDG environment without
mutating global environment variables.

Initial implementation checks used a standalone copy on Linux to avoid
concurrent root lockfile edits and a duplicate GPUI build. The crate is now
integrated and the root workspace checks passed; see [DONE.md](../DONE.md).
macOS runtime validation remains open.
The documented macOS paths follow the directory library's platform mapping;
they have not been exercised on macOS here.

Not implemented: cache entries/LRU/eviction or actual rate enforcement; active/
saved-file protection; interruption-safe media copying; catalog metadata/title
storage; persistent source descriptors; path override UI/moves; migration backup
or repair UI; an async storage API (the desktop owns its worker/command queue);
paginated favorites; encryption. In particular
the configured cache bound alone does **not** complete P4's bounded-cache work.
