# GPUI presentation layer

`kadr-ui` implements the MVP screens on GPUI `=0.2.2`. `KadrView` owns display
state and editable drafts; the application controller owns validation of actual
sources, persistence, asynchronous tasks, native dialogs and player lifecycle.
The UI never performs filesystem, SQL, process or application-network IO in
render. Public poster images use GPUI's asynchronous image loader and require
the application to configure a GPUI HTTP client.

## Screens

- **Home** (initial screen) and **Movies/Shows**: a measured-resume hero or
  a real catalog spotlight, series and movie shelves, adaptive poster grid,
  and sort by provider order, title, year or rating. A user action loads
  the shelves; there are no startup requests or placeholder films.
- **Search overlay** (`Ctrl+K` / `⌘K`): explicit Enter/button submission, poster
  results, Arrow key selection and Escape to close. Detail view shows
  available title, year, rating, description, attribution, favorite control
  and series season/episode selection.
- **Sources dialog** (also a page for manual input): quality tabs, optional
  seed sorting, and found sources with quality, language, origin and
  license; manual clipboard attachment and `.torrent` picker; explicit unlink
  from the current media; player selection and playback controls. Private URLs
  and stream tokens have no fields in the presentation DTOs and are not shown.
  A discovery snapshot reports pending/successful/failed providers. Fast
  results are selectable while others run, and manual input works before
  the first result. Refresh bypasses the discovery cache; dismissing the
  dialog invalidates its search, not an already prepared manual input.
  RuTracker rows also show available size and seed count, without asserting
  playback rights. If the selected topic has no magnet, Sources shows a masked
  sign-in form only when requested by the engine.
- **Torrent files**, within Sources: video-file names, byte sizes, recognized
  season/episode and ambiguity indication; file selection and editable numeric
  season/episode correction. The controller decides which files are videos.
- **Library and History**: all/favorites/resume/history filters, favorite controls, measured
  position/duration and open/resume actions. Unknown position stays unknown:
  neither browser launches nor downloaded byte counts imply playback progress.
- **Downloads and preparation overlay**: stage/player summary, downloaded/total bytes and percentage,
  buffered bytes, download/upload rates, peers, save summary; Save, Open in and
  Stop controls. Every unknown statistic is explicitly unknown, not zero.
- **Settings**: default player; editable cache bytes/path, download/upload
  limits, peer cap, BitTorrent and catalogue toggles, directory picker, save,
  clear-cache and clear-history requests. Source preferences have a separate
  editor/acknowledgement: provider switches, endpoints, optional names of
  key environment variables (not secrets), preferred resolution and language.

Sections support `SectionState::{Unavailable,Loading,Empty,Error}`.
Source/search work is cancellable; retry repeats the corresponding request.
Real DTOs represent success,
so the default Empty card is hidden when there is data. Errors and Loading remain
visible alongside previous data. Controllers should set an Empty state after
successful completion (including a legitimate empty result), replacing any prior
Loading/Error state. No fake rows or progress values are generated.

## Controller API

Every public presentation type is exported at the crate root.
Instantiate with `cx.new(|_| KadrView::new())` and subscribe to
`UiEvent` using GPUI `cx.subscribe`. Retain the returned `Subscription`.

New DTOs (all derive Clone, Debug, PartialEq, Eq):

```rust,ignore
MediaCard {
    id: String, title: String, original_title: Option<String>, year: Option<u16>,
    description: Option<String>, poster_url: Option<String>, kind: MediaKind,
    rating_tenths: Option<u8>,
    episodes: Vec<EpisodeRow>, attribution: String,
}
EpisodeRow { season: u16, number: u16, title: String }
SourceRow {
    index: usize, title: String, kind: SourceKind, quality: Option<String>,
    language: Option<String>, origin: String, license: Option<String>,
    size: Option<u64>, seeds: Option<u32>, rutracker: bool, recommended: bool,
}
TorrentFileRow {
    index: usize, name: String, size: u64, season: Option<u16>,
    episode: Option<u16>, ambiguous: bool,
}
LibraryRow {
    id: String, title: String, poster_url: Option<String>, details: String, favorite: bool,
    opened_at: Option<i64>, completed: bool,
    position: Option<u64>, duration: Option<u64>, // seconds
}
TransferView {
    stage: String, download_bps: Option<u64>, upload_bps: Option<u64>,
    peers: Option<usize>, done: Option<u64>, total: Option<u64>,
    buffered: Option<u64>, saving: String,
}
PreferencesDraft {
    cache_limit_bytes: u64, download_limit: RateLimit, upload_limit: RateLimit,
    peer_limit: u32, torrents_enabled: bool, catalog_enabled: bool,
    catalog_proxy: Option<String>, cache_directory: String,
}
```

Enums: `MediaKind::{Movie,Series}`, `RateLimit::{Paused,Unlimited,Bps(u64)}`,
`SourceKind::{Http,Magnet,TorrentFile,LocalFile}`, `PlayerChoice::{Mpv,Vlc,Browser}`.
The shell adds `Home, Movies, Shows, History` to the existing sections. Search
and the detail's Sources open over the current page rather than replacing it.

New events:

```rust,ignore
Search(String), Browse, OpenMedia(String), WatchMovie, FindSources,
SelectEpisode { season: u16, number: u16 }, ChooseSource(usize),
ToggleFavorite, SetFavorite { id: String, favorite: bool }, OpenLibrary(String),
ChooseTorrentFile(usize), AssignEpisode { season: u16, number: u16 },
Save, OpenIn(PlayerChoice), SavePreferences(PreferencesDraft),
ChooseCacheDirectory, ClearCache, ClearHistory, UnlinkMedia,
SignInRuTracker(RuTrackerCredentials), ForgetRuTracker
```

Existing events remain `PasteSource,OpenTorrent,Play,PlayFromStart,Stop`,
`SetDefaultPlayer(PlayerChoice),Retry(Section),Cancel(Section)`.
`WatchMovie` is the movie card's explicit play intent; `FindSources` only
shows options. The view renders the engine's `recommended` marker and never
derives a winner from a partially received or visually filtered list.
The source/torrent indices are controller-provided identifiers, not visual row
positions. `AssignEpisode` applies to the last selected torrent-file index.
The controller must reject stale selections or operations when their underlying
session has changed. `OpenLibrary` loads the saved source and position; mpv can
restore a measured position, while VLC/browser must not promise telemetry.
`Save` opens the native destination picker in the application. Clear actions
request application confirmation, rather than deleting anything in the UI.

Every setter takes `(&mut self, value, &mut Context<KadrView>)` and notifies GPUI:

```rust,ignore
set_search_results(Vec<MediaCard>)
set_browse_catalog(Vec<MediaCard>, Vec<MediaCard>) // movies, shows
set_browse_state(SectionState)
set_media_detail(Option<MediaCard>)
set_catalog_episode(Option<(u16, u16)>)
set_media_favorite(bool)
set_sources(Vec<SourceRow>)
begin_source_discovery()
set_source_discovery(Option<SourceDiscovery>)
set_source_preferences(SourcePreferencesDraft)
set_torrent_files(Vec<TorrentFileRow>)
set_library(Vec<LibraryRow>)
set_transfer(Option<TransferView>)
set_preferences(PreferencesDraft)
set_cache_directory(String)
```

`set_media_detail` resets the favorite display to false; acknowledge favorite
with `set_media_favorite` after setting detail. `set_torrent_files` resets local
file selection. `set_preferences` replaces all editable fields, so call it for
initial load or acknowledged save, not on every telemetry tick.
Returning to search results does not implicitly unlink media from Sources.
`set_search_results` closes detail but retains the displayed source-binding
context; acknowledge an explicit unlink with `set_media_detail(None)`.
`set_cache_directory` changes only the path field and preserves other unsaved
settings edits. Numeric form validation rejects overflow, negative sizes,
non-positive peer caps and invalid rate strings without emitting a save event.
Rate fields accept `paused`, `unlimited`, or a positive integer in bytes/second;
zero is rejected rather than silently interpreted as unlimited. Season zero
supports specials; episode numbers must be positive.

Existing setters remain `set_status(UiStatus),set_default_player(PlayerChoice)`,
`set_source_preview(Option<SourcePreview>),set_playback_available(bool)`,
`set_playback_active(bool),set_section_state(Section,SectionState)`,
`set_preferences_status(impl Into<String>),show_section(Section)`.

Set playback active **before** asynchronous launch to suppress duplicate Play
and source replacement. Set it false after Stop or a failed launch. Play also
requires a source, connected playback and no Sources loading state. Save/Open in
require an active source. Controllers retain the cancellable task and invalidate
stale generation results; setters themselves do not cancel work.

Player choice is acknowledged, not optimistically applied: persist then call
`set_default_player`. Network/catalogue toggle selections are an unsaved local
draft until `SavePreferences`. Default values are presentation defaults, not
proof that settings were loaded or a player is installed.

## Text input and privacy

`src/input.rs` is a dedicated single-line GPUI `EntityInputHandler`, adapted from
GPUI's example. It supports typing, mouse caret/drag selection, selection with
Shift, Home/End, grapheme-aware arrows/delete, Select all, Cut/Copy/Paste,
horizontal caret scrolling and native IME marked text. Internal byte offsets
are converted at the platform UTF-16 boundary; surrogate pairs cannot produce
invalid UTF-8 slicing. IME selection is relative to the replacement text.
Enter during marked composition does not submit Search.
Clipboard shortcuts are Cmd on macOS, Ctrl on Linux. Source paste remains a
separate controller action, so arbitrary magnet or `.torrent` input can be
validated without echoing a private source URI.

Tab/Shift-Tab traverse inputs and enabled buttons; Enter/Space activate buttons.
The root only establishes focus once, preserving focused input across renders.
Search text, user-configured cache paths, media titles and public metadata are
intentional display content. Source previews, errors, transfer stage/save
summaries and all controller messages must already be privacy-safe: never pass
raw network errors containing credentials, tokens or private URLs. Public poster
URLs must not be authenticated playback URLs. Only HTTP(S) poster URLs are used.
Images remain off until `set_preferences` confirms catalogue access is enabled.
Turning the catalogue draft off suppresses image elements immediately; turning
it on does not permit new poster requests until persistence is acknowledged.
Existing GPUI image tasks are owned by its loader; application HTTP policy must
also honor opt-out for already scheduled requests.

## Validation

UI tests exercise honest initial state, playback/source replacement guards,
retry/cancel, identifiers, UTF-16 scalar boundaries, explicit rate states,
invalid/overflow inputs and display units. Run in the integrated workspace:

```sh
cargo test --locked -p kadr-ui
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

When parallel implementation owns the root manifest/lock elsewhere, a standalone
scratch Cargo manifest can point its `[lib] path` at this UI's `src/lib.rs`, use
`gpui = "=0.2.2"` and `unicode-segmentation = "=1.13.3"`, and copy the root
lockfile into scratch before running Cargo. This checks and links the **actual**
GPUI UI without editing the workspace lock. Use the existing shared Cargo target
and `CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2` to reuse dependencies.
This is not a substitute for final integrated workspace checks.

Manual acceptance needs the integrated app: type a non-ASCII query, edit it with
selection/clipboard, submit with Enter; open a real card and episode; attach and
unlink sources; choose a torrent video and correct its episode; exercise library
filters/resume/favorite; observe live transfer values and cancellation; save
settings and verify persistence. Repeat at minimum window size and with keyboard
navigation. Test real IME composition and clipboard on Linux/macOS.

For native Wayland lifecycle, the existing `scripts/smoke-wayland.py` runs Xvfb,
Openbox, Weston/X11-backend and the application as a native Wayland client:

```sh
python3 scripts/smoke-wayland.py --binary target/debug/kadr-app --screenshot
```

It asserts configure/buffer/commit and clean Ctrl+Q shutdown with isolated
HOME/XDG directories and bounded waits. Its screenshot is for visual inspection,
not proof of all screens, inputs, backend workflows or macOS behavior.
