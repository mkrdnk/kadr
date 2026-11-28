# Local storage and resume APIs

`kadr-storage` is a standalone synchronous persistence adapter. It depends on
`rusqlite` with bundled SQLite, `directories`, and the pure `kadr-core` source
validator/identity implementation, not GPUI, a network runtime, or a player.
It stores settings, catalog/local display snapshots, favorites/history, measured
progress, resumable source inputs and saved-copy paths. Cache ownership,
eviction, copying, playback and UI behavior live in other adapters.

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
    StoredSource, Storage,
};

// Run this entire block on the storage worker, not inside a GPUI callback.
fn worker_example() -> kadr_storage::Result<()> {
    let paths = AppPaths::discover()?;
    let mut storage = Storage::open_default(&paths)?;
    let mut settings = storage.load_settings()?;
    settings.cache_limit_bytes = 2 * 1024 * 1024 * 1024;
    storage.save_settings(&settings)?;

    // Source descriptors are sensitive user-approved inputs, not log messages.
    let descriptor = StoredSource::Link("https://example.org/video.mp4".into());
    let source = descriptor.source_id()?; // Same deterministic ID as kadr-core.
    storage.save_source(&source, &descriptor)?;
    let media = MediaId::new("local:stable-item-1")?;
    let target = PlaybackTarget { media_id: media.clone(), episode: None };
    storage.set_favorite(&media, true)?;
    storage.link_source(&target, &source)?;
    // Only after the player/browser launch actually succeeds:
    storage.record_opened_target(&target, &source, 1_700_000_000)?;
    let _resume = storage.load_progress(&target)?;

    // Only when a managed player actually reports this observation:
    storage.save_progress_for_source(&PlaybackProgress {
        target,
        source_id: source,
        position_seconds: 42.0,
        duration_seconds: Some(120.0),
        updated_at: 1_700_000_042,
        completed: false,
        origin: ProgressOrigin::Mpv,
    }, &descriptor)?;
    // After restart, on the worker:
    let items = storage.library(100)?;
    // items[n].source.manual_source() reconstructs validated adapter input.
    // items[n].target retains its media/episode identity even without progress.
    // items[n].progress retains its measured position.
    // Never log descriptors, explicit URL/path getters, or their field values.
    let _ = items;
    Ok(())
}
```

Catalog media IDs should be provider-namespaced; local items need a persistent
application identity. Reusing the deterministic core source ID as a media ID is
sufficient for an initially uncatalogued manual item. Storage ID wrappers allow
1..=256 ASCII bytes from `A-Z a-z 0-9 . _ : -`. Legacy opaque source IDs remain
readable for v1/v2 compatibility, but **`save_source` rejects an ID that does not
equal the validated descriptor's ID**. Use `StoredSource::source_id()`. For
manual sources it derives from the core source identity. Keys never contain
source URLs or paths.

`StoredSource::RuTracker` uses a provider-specific identity consisting of a
topic ID and v1 info hash. The engine resolves it again, checks the returned
hash and only then reuses the selected file. It does not store magnet links or
downloaded metainfo for these descriptors. Account credentials use the platform
vault (macOS Keychain or Linux Secret Service), never SQLite.

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
the integration point for explicit roots. `Settings::cache_directory` separately
persists an optional absolute cache override; `None` means `AppPaths::cache_dir()`.
The cache adapter must select that root, create it privately and implement any
relocation policy. Saving settings neither creates an override nor moves files.

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
| `sidebar_collapsed` | `false` | bool; persists the compact sidebar without changing playback or network settings |
| `network.torrents_enabled` | `false` | explicit bool |
| `network.download` | 10 MiB/s | `Paused`, `BytesPerSecond(1..=i64::MAX)`, `Unlimited` |
| `network.upload` | `Paused` | same explicit rate-limit enum |
| `network.max_connections` | 64 | 1..=4096 |
| `cache_limit_bytes` | 10 GiB | 1 MiB..=i64::MAX bytes |
| `catalog_enabled` | `true` | explicit bool; permits user-initiated catalog actions only |
| `cache_directory` | `None` | platform cache default, or absolute user-selected path without `..` or NUL |

These are conservative, **provisional policy defaults**, not measured capacities,
performance SLOs, or enforcement by this crate. BitTorrent remains disabled until
the application explains peer uploading and the user enables it. An eventual
engine must honor explicit pause/limit/unlimited semantics (zero must not turn
into unlimited). No telemetry, network request, source resolution, or background
download is started by opening storage.
Catalog enabled does not authorize startup/background fetching: the application
must issue a request only after a user action. Source-provider preferences never
override the catalog or BitTorrent permission. The separate `auto_discover`
source preference is off by default, including after migration, and permits
card-scoped source lookup but not automatic playback. Settings writes
preserve all fields in one atomic singleton
upsert; editors should load, clone/edit, and save the full snapshot.

## Schema, migrations and failure behavior

SQLite uses the bundled SQLite build, parameter-bound values and strict tables.
The public connection is opaque; callers cannot bypass validation via it.
`PRAGMA user_version` records the schema:

1. Validated singleton settings, initialized from `Settings::default()` only
   for a new v0 database.
2. Library favorites/history and per-item/per-episode measured playback progress.
3. Durable source descriptors; per-target source and torrent-file selections;
   catalog/local media records; saved-copy paths; catalog enablement and an
   optional cache directory. Existing history/progress IDs and settings survive.
   Old IDs cannot recover previously discarded URLs: legacy items show without
   resumable input until the user selects a source again.
4. Exact per-target actual-open history with immutable source/file selections.
   Existing media-wide v3 open timestamps migrate to item-level history only.
   Neither measured progress nor a source link invents an episode open timestamp.
5. RuTracker source descriptors; copies all existing source rows and keeps
   playback targets, selections and history keys intact.
6. Optional catalog proxy endpoint without credentials. Existing settings keep
   their values; no proxy is configured by the migration.
7. Source-provider preferences, Torznab endpoint configurations (environment
   **names**, not key values), and disposable source-discovery results. A cache
   row belongs to one Trakt movie or show/season/episode and one provider.
   At most 1 MiB per row, 1,024 rows and 16 MiB of result payloads. Reads and
   writes prune expired rows; positive results live for 30 minutes, successful
   empty responses for 5. Timeouts and provider errors do not write rows.
   Local attachments use the user's selected or linked input, not this cache.
   Changing source preferences invalidates results in the same transaction.
   The media payload quota and permanent saved files are unaffected.
8. Independent Commons provider permission. On a new database it is enabled;
   existing databases migrate with it disabled, so an upgrade never expands
   the user's previous network permissions. The global catalog switch still
   gates all network discovery. No source files or video bytes are migrated.
9. Independent `popcorn_enabled` preference, off on both new and upgraded
   databases. Popcorn API results are held only in memory; the migration does
   not add a release index or store magnet links.
10. Reset `popcorn_enabled` to false on upgrade. The v9 opt-in authorized only
    movie lookups by IMDb ID through `fusme.link`, not additional mirrors or
    numbered episodes. Keep a single switch, default off, with no new fields;
    a fresh opt-in is required for the expanded provider. Reopening a v10
    database preserves any newly enabled choice. All other source preferences,
    ordered Torznab configurations, settings and library/playback data remain
    unchanged. Resolver cache is retained: Popcorn results are memory-only, so
    no persistent Popcorn results need invalidation. A subsequent explicit
    preference change still clears resolver cache through the normal save path.
11. Persist `sidebar_collapsed` and the separate `auto_discover` permission.
    Both default to false; no existing provider permission is expanded.

Opening uses `BEGIN IMMEDIATE`, checks the version, applies the entire missing
migration chain and updates `user_version` in **one transaction**. Any failure
rolls back DDL, data and version changes. Existing settings survive upgrades.
Negative versions are invalid. A future version is refused before any
schema/journal-mode changes; there is no downgrade, drop/recreate, fallback
database or automatic reset. Opening does not request WAL or vacuum.

Paths use tagged native BLOBs (Unix bytes or Windows UTF-16LE), not lossy display
strings. Unix non-UTF-8 paths round-trip exactly. A database copied between
incompatible path-encoding platforms reports an error rather than reinterpreting
private paths. Paths are trusted absolute user choices, not a symlink sandbox.

The journal/synchronization policy is SQLite's default rollback journal with
full synchronization. The adapter enables foreign keys. Each settings/history/
favorite write is a single atomic statement. Open/progress writes atomically
ensure the owning library row and update target linkage; progress also snapshots
the selected torrent file. `link_source`, saved-copy writes, library snapshot
reads and `clear_playback_data` are transactional. Lock contention, unavailable
directories, permissions, full disk
and SQLite failures return sanitized errors. No errors expose raw SQL or SQLite/
OS error strings. Fine-grained retry/recovery UX is left to the application.

## Library, history and progress

- `set_favorite` changes only the favorite flag; `library_entry` looks up one
  item and `favorites` returns favorites in stable ID order.
- `record_opened_target(&PlaybackTarget, &SourceId, opened_at)` records an actual
  successful player/browser launch for that exact item/episode, with a snapshot
  of its linked source/file selection. Call it **after** launch success, never
  during source selection or after a failed launch. Same-ID selection requires
  `save_source` + `link_source` before launch. No progress row is created.
  `record_opened(media, source, time)` remains an item-level compatibility wrapper.
  Target history and the media-wide history summary update atomically; newest
  timestamp wins independently per target, so an older episode's first open
  can be retained without overwriting a newer media-wide open. `history`
  returns 1..=1000 entries, newest first with an ID tie-breaker. Older open
  events cannot overwrite newer history. Recording an open links that exact
  target but does not itself save the source descriptor.
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
  favorites/settings and saved-copy records. Only favorite source links survive;
  descriptors not referenced by those links are deleted, including unlinked
  inputs and the original URLs of non-favorite saved copies. Favorite source
  inputs/selections remain so the user can reopen them, but measured positions
  and all target-history timestamps/selections are removed. Unretained media
  snapshots are deleted.
  This is **not forensic erasure** of free pages, journals, backups or snapshots.

### Durable sources and episode selection

```rust
pub enum StoredSource {
    Link(String), // Validated HTTP(S), or magnet without a file selection.
    TorrentLink { uri: String, file_id: Option<usize> }, // Magnet only.
    TorrentFile { path: std::path::PathBuf, file_id: Option<usize> },
}
```

- `save_source(&SourceId, &StoredSource)` validates through `kadr-core`, including
  ID equality. `load_source` validates again and rejects mismatched/corrupt data.
  `.torrent` paths must be absolute without `..`/NUL and have a `.torrent`
  extension. No network or file reads occur; missing/relocated files are handled
  as unavailable original inputs by the application, not silently reset.
- `StoredSource::manual_source()` reconstructs validated core input;
  `source_id()` computes its stable identity. File IDs are nonnegative,
  SQLite-representable selection indices, not checked against metainfo here.
- Core torrent identities exclude file selection. After selecting a file,
  save the descriptor and call `link_source(&PlaybackTarget, &SourceId)`.
  That explicit link snapshots selection **per episode**, so selecting another
  file from the same magnet or `.torrent` does not change earlier episodes.
  `source_for_target` returns that target's descriptor and selection. It does
  not fall back from a missing episode to an unrelated item/episode.
- Accepted progress writes snapshot the target's selected file and preserve it
  during subsequent telemetry. They cannot reinterpret an earlier measured
  position after a later selection change. Stale progress/open events do not
  change linkage. Explicit reselection requires `link_source`, even for the
  same core source ID.
- Asynchronous player adapters must call
  `save_progress_for_source(&PlaybackProgress, &StoredSource)` with the immutable
  descriptor captured at that player's launch. A late observation from an old
  player must not acquire a newly selected file from mutable picker state.
  This method validates source-ID equality and snapshots the session selection;
  it does not save the descriptor itself. `save_progress` remains a compatible
  convenience for observations guaranteed to use the current target selection.

### Display snapshots and library listing

`MediaRecord` contains `id: MediaId`, `title: String`,
`original_title: Option<String>`, `year: Option<u16>`, `kind: MediaKind`
(`Movie`/`Series`), `description: String`, `poster_url: Option<String>` and
`attribution: String`. The app maps catalog data or a user-chosen local basename
to this record. Attribution is display text, not code. Poster URLs must be
HTTP(S); storage never downloads them. `upsert_media` and `media_record` write/
read validated snapshots without making a catalog-only result a library item.

`library(limit)` accepts a limit of 1..=1000 `LibraryItem` snapshots, ordered by
newest media open timestamp then media ID. Each has `entry`, `target`, `opened_at`,
optional `media`, optional latest
measured `progress` (including the exact episode target), and optional `source`.
Progress-only items, favorites and saved-copy items are included. Missing
metadata/descriptors remain `None`, not guessed titles/URLs. If progress exists,
its matching source/selection is returned: a newer browser launch must never
pair an old position with a different source. Without progress, the latest
actual-open target or a retained source link is available for reopening from start.
Use `item.target` for playback, not an assumed item-level target: a retained
favorite or saved-copy episode can have no progress after clearing history.
With multiple retained links but no history/progress, the highest season/episode
is the deterministic reopening target; a saved-copy-only item uses its newest
saved target. Opening a different episode uses `source_for_target` explicitly.
`favorites`/`history` remain available as lightweight entry-only APIs.

For an episode-selectable library, use
`library_targets(limit) -> Result<Vec<LibraryItem>>`. It returns **one row per
distinct `PlaybackTarget`**, taking the union of measured progress, source links
and saved copies, plus exact target history. A favorite with no concrete targets
gets one item-level row; favoriting a series never fabricates episode targets.
The favorite flag remains media-level and is shared by that media's target rows.
Source-only and saved-only episodes remain selectable even without telemetry.
Targets present in several tables appear only once.

Rows are ordered by the newest relevant progress/saved/target-open timestamp,
then media ID, season and episode ascending. Source-only links
have no timestamp and sort after dated rows with the same stable tie-breakers.
The accepted limit is 1..=1000 **target rows**, not media items.
`library_target(&PlaybackTarget) -> Result<Option<LibraryItem>>` reloads exactly
one selectable target using the same membership and resume rules. A missing
episode returns `None`, never another episode or the item-level source.
Both calls read consistent transactional snapshots. For progress-bearing rows,
the source and file selection come from that target's immutable progress
snapshot, even if a newer browser open or file selection changed the media's
last source/current link.

**History filters must use `item.opened_at.is_some()`**, not playback position,
progress presence, or `item.entry.last_opened_at`. `opened_at` is the actual-open
timestamp for `item.target`; the entry timestamp is a media-wide summary and
may belong to another episode. Browser opens have real history but no measured
progress. A merely linked source (including a failed launch), a saved copy, or
progress without a recorded launch has no invented history timestamp. Without
progress, an opened target resumes using its actual-open source/file snapshot,
not a later prelaunch selection. `library()` also reports only its chosen
target's exact `opened_at`, even when another episode has newer media history.

The UI may encode a row key from media ID plus season/episode, but must pass the
typed `PlaybackTarget` back to storage. A composite UI key containing `@` is not
a `MediaId` and must not be persisted as one. The older `library(limit)` keeps
its one-row-per-media behavior for summary views; it is not an episode picker.

### Saved copies

`SavedFile { target: PlaybackTarget, path: PathBuf, saved_at: i64 }` records a
completed copy independently from disposable cache and its original descriptor.
Call `save_saved_file` **after** an interruption-safe user-owned copy completes.
`saved_file(target)` and `saved_files()` return paths that the application can
open after cache clearing or history removal. `remove_saved_file(target)` only
forgets metadata; it never deletes the user's file. This adapter does not copy
files, check current existence or implement a file-opening player adapter.

### Privacy

**The local database intentionally stores user-approved HTTP URLs (including
query tokens), full magnets (including private trackers/display names), private
`.torrent` paths, chosen cache/saved-copy paths and display titles.** This is
necessary for restart/resume; SPEC's prohibition on logging URLs is not a
prohibition on explicitly retaining user-approved source input.

The database is plaintext, not encrypted. New DB files use 0600 on Unix and app
directories 0700; existing permissions are not silently changed. OS access
controls, backups and filesystem snapshots still matter. `Debug` redacts source
descriptors, media snapshots, saved files, settings, directories, handles and
opaque IDs, including when nested in library results. Errors expose no raw
SQLite/OS/core input strings or underlying error source. Public fields, explicit
path getters, URL getters and ID `as_str()` intentionally expose values for
actual application work: **never log their output**.

## Tests and remaining scope

From the integrated root workspace:

```sh
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p kadr-storage
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo clippy --locked -p kadr-storage --all-targets -- -D warnings
cargo fmt -p kadr-storage -- --check
```

The crate tests use temporary databases: reopen/settings persistence, invalid
writes, corrupt reads, successful genuine v1/v2 upgrades, full-chain/v1/v2
migration rollback,
future-version byte preservation, favorites/history independence, movie/episode/
series isolation, finite/range validation, browser rejection, stale event
ordering, clearing playback data, private debug/errors and Unix permissions.
Additional resume tests cover reconstructing core input after restart, source/
ID validation, magnet and `.torrent` file-selection isolation, progress/source
pairing, retained favorite secrets versus cleared history, saved-copy reopening,
corrupt descriptor rejection and non-UTF-8 Unix paths across settings/sources/
saved copies. Linux path discovery uses the current XDG environment without
mutating global environment variables. macOS runtime validation remains open.
The documented macOS paths follow the directory library's platform mapping;
they have not been exercised on macOS here.

Not implemented in this crate: media **file** cache entries/LRU/eviction or actual rate
enforcement; active/saved-file cache protection; interruption-safe media copying;
path override UI/moves; migration backup or repair UI; an async storage API
(the desktop owns its worker/command queue); paginated favorites; encryption.
In particular
the configured cache bound alone does **not** complete P4's bounded-cache work.
