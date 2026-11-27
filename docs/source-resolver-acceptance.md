# Source Resolver — acceptance record

Scope: the **current working tree** on Linux x86_64. Historical tests of the
old Archive/TVmaze catalog and macOS CI for other revisions are not evidence
for this change. `PASS` below describes the named check at its stated boundary.
`NOT VERIFIED` means the end result was not observed; a narrower success is
not promoted to an end-to-end pass. **Overall acceptance remains open.**

## Criteria

| AC | Result and evidence |
| --- | --- |
| 01 · Architecture | **PASS.** Desktop → engine commands/events; no discovery HTTP server, daemon, external indexer installation, or plugin loader. Trakt is still the metadata client. `scripts/check-architecture.py` and its tests verify the presentation dependency boundary. Commons was registered in `Worker::registry` using `SourceProvider` without changing the concurrent scheduler in `resolver.rs`. The existing loopback HTTP server is for playback, not search. |
| 02 · Out of the box | **PASS for the built-in provider, NOT VERIFIED for the clean-install user journey.** Commons resolves two reviewed full films through a real public API. It needs neither credentials nor BitTorrent. The credit, file-specific license and permitted API/reuse model are documented in [catalog.md](catalog.md#built-in-open-films-wikimedia-commons). Trakt catalog access is a separate prerequisite: see E2E-01. |
| 03 · Providers | **PASS at engine/settings boundaries.** Commons, RuTracker, independent named Torznab instances, and explicitly attached HTTP/magnet/local `.torrent` use `SourceProvider`. The settings editor supports adding, toggling and removing indexers. `kadr-storage/tests/source_resolver.rs`, UI editor tests and `tests/torznab.rs` cover validation, persistence, opt-out, credential references, authenticated RSS and selected metainfo. The manual path also works without running a search. |
| 04 · Resolution | **PASS at controller/transport boundaries.** Explicit `FindSources` targets one Trakt movie or a numbered show episode. No shelf enumeration. The matcher consumes primary/original/available alternative titles and IDs; no extra per-card alias request. `resolver::tests` uses fast, stalled, failed and queued providers to check incremental completion, the concurrency cap, timeouts and cancellation. The Torznab request counters distinguish a normal cache hit from force refresh. `DiscoveryStamp` and target checks discard old snapshots; see E2E-05 for the scope of navigation testing. |
| 05 · Identity and provenance | **PASS for tested formats.** `kadr-core/tests/media_sources.rs` tests ID conflicts, full normalized titles, aliases, remake years, exact season/episode and specials. Torznab tests reject conflicting or ambiguous scope. RuTracker may return a *candidate pack*, not a verified episode. The engine tests reject positive file/target contradictions and require confirmation for an inconclusive file when an episode was explicitly selected. Unknown resolution/size/seed count remain unknown. `origins` survives merging. |
| 06 · Ranking and deduplication | **PASS.** Domain tests cover v1/v2/hybrid info hashes, canonical HTTP URIs (queries remain significant), provider-scoped item IDs, merged origins and permutation invariance. Matching movie torrents use descending known seed counts, with deterministic ties; other sources retain their previous ranking. Only an explicit movie Watch can launch the final recommendation. Browsing or selecting a row alone does not start playback. |
| 07 · Cache | **PASS at SQLite/engine boundaries.** One normalized batch per provider and Trakt movie or episode. Storage tests reopen the database, check exact TTL boundaries (1,800/300 seconds), prune expired entries, enforce row/byte limits and roll back failed writes. A provider settings change clears the resolver cache transactionally (all provider batches, not just the edited one); errors do not create negative entries. Force refresh skips the cache. `kadr-cache` contains media payloads, not resolver metadata. See E2E-04. |
| 08 · UI | **PASS on Linux.** Film/episode actions open the existing Sources dialog. GPUI does not wait on network or SQLite. UI tests cover idle/loading/partial/complete/empty/partial failure/all failed, stable handles, refresh, manual input and persisted preferences. Rows show available resolution, codecs, language, size, provider, seeds/peers and license; unknowns are not zeros. The private Xorg runs below also exercised actual rendered rows, explicit selection and playback, not just presentation models. |
| 09 · Playback | **PASS at engine/player boundaries on Linux.** Selected HTTP inputs use the existing HTTP path; selected torrent enclosures are parsed as metainfo, not streamed as video. Tests exercise multi-file selection, actual mpv position before torrent completion, stop/teardown, save, failure messages, and separate episode progress. Real mpv and VLC tests use generated media. VLC does not claim position telemetry. |
| 10 · Failure handling | **PASS in controlled failure/offline scenarios.** Independent deadlines, max four concurrent providers, streaming snapshots and cancellation. A failing localhost Torznab API does not erase a peer's results; recovery retries just failed providers. Changed metainfo hash and malformed responses fail without launch. Cache-only lookup does not require a live provider (privacy/config gates still apply); manual sources remain independent. Not a claim about every public service or network failure. |
| 11 · Security | **PASS for implemented boundaries and tested cases.** Core URI/scheme validation; default TLS verification; fixed Commons API with exact approved file locations/rights; bounded JSON/XML and DTDs disabled. Separate clients, credential-free DTOs, sanitized cached locators, no cross-origin authenticated redirects. On a shared Torznab origin, only the configured API path receives the instance key. Tests inspect outbound mock requests, diagnostics and stored bytes. Player launches use fixed arguments and `--` plus one media argument, not shell interpretation. No real credential was added; pattern inspection is not a guarantee about all historical secrets. |
| 12 · Automated coverage | **PASS for the ordinary offline suite.** `kadr-catalog` tests Trakt → query, Torznab XML, Commons JSON/rights and local HTTP errors. `kadr-core` tests matching/dedup/ranking. Storage tests TTL/config invalidation; resolver/worker tests partial failures, timeout/cancel and stale work; UI/controller tests queued snapshots. Public-network/player acceptance has explicit opt-in tests, not a dependency of `cargo test --locked --workspace`. |
| 13 · User scenarios | See the six individual results below. Component and engine-command coverage are deliberately identified separately from interactive desktop evidence. |
| 14 · Platforms | **PASS: Linux build and runtime. NOT VERIFIED: macOS build, discovery, cache and native UI for this revision.** Shared Rust implementation; OS-specific window/paths/keychain/player code stays in existing adapters. Re-run the project macOS CI matrix on this revision. |
| 15 · Definition of Done | **NOT VERIFIED as a complete DoD** until the remaining platform/user scenarios pass. [Provider extension guide and settings](catalog.md#adding-a-built-in-provider), [storage/migrations](storage.md), and known limitations are documented. No `todo!()`/`unimplemented!()` were found in the resolver crates. Build/lint, regular tests and opt-in evidence are listed below; no assertion of universal regression coverage. |

## Movie Watch follow-up (Linux)

A private Xorg session used a saved `Fixture` (2020) card and two owned Torznab
indexers. Opening the card made no indexer request. Leaving while the second
response was held returned to the library; the late response triggered no
metainfo download. With zero seeds, Watch opened Sources without choosing a
release. After refresh, the [captured Sources window](evidence/movie-watch-recommendation.webp)
showed the 720p release with 27 seeds above the 1080p release with 3; only
the former was marked recommended.

Selecting the 3-seed alternate, closing Sources and clicking Watch requested
no replacement. On a freshly reopened card, Watch requested only the 27-seed
release's metainfo. This UI check intentionally used a cache smaller than the
torrent: both playback attempts reached the cache quota check, not a public
swarm or a player.

The complementary engine test `real_watch_release_plays_video_from_the_final_winner`
uses an owned loopback seed, generated video, the actual default mpv, and its
measured position persisted against the winner's info hash. To run it together
with the late-answer, wrong-year, manual choice, fallback and cancellation checks:

```sh
cargo test --locked -p kadr-engine tests::torznab::command_boundary::recommendation:: -- --include-ignored
```

Two existing `kadr-media` tests failed intermittently on different workspace
runs: `dropping_server_requests_cleanup_of_pending_waits` (a TCP connect still
succeeded immediately after dropping the server) and `startup_timeout_and_cancel_join_the_child`
(timed out waiting for the fake player's startup record). Both passed on
isolated reruns; the final `cargo test --locked --workspace` passed. Neither
test was changed for the movie Watch feature.

## User scenarios and evidence boundary

1. **E2E-01 — NOT VERIFIED, blocked before source search.** The opt-in
   `tests::commons::live_clean_install_trakt_commons_playback` starts with an
   empty database. In this build `Search` returned `TRAKT_MISSING_CLIENT_ID`.
   No catalog card was substituted in this test. An authorized build-time
   production ID (or a valid runtime development ID for testing) is needed to
   exercise live Trakt → select movie → live Commons → player. The separate
   saved-card test starts with labeled fixture metadata; it is **not** E2E-01.
2. **E2E-02 — PASS for a saved series in the native Linux client.**
   From two episode rows for the same series, select S01E02, refresh Sources,
   choose its HTTP source and play. Only `/ep2.mp4` received Range requests,
   mpv measured progress for S01E02 (15-second generated video), and S01E01
   (different 8-second video) had no progress row. These are deliberately
   prepared library records, not a claim about live Trakt access.
   `real_episode_search_selects_correct_file_and_mpv_playback` uses an
   episode-specific saved target, actual discovery, distinguishable generated
   episodes in a multi-file torrent, a loopback seed and mpv IPC. It checks
   that a contradictory file cannot be played/saved and compares the saved
   bytes for the correct episode. Interactive selection in a live Trakt
   series is **NOT VERIFIED**.
3. **E2E-03 — PASS, native Linux desktop with controlled providers.** The `chosen_offer_and_partial_force_cache`
   test uses two local HTTP APIs and a missing-key config. One returns a good
   release, another fails; both the row and the failure survive in the same
   `SourceDiscovery` event. This also exercises recovery without restarting.
   On a private Xorg dummy display the actual Sources dialog for `Fixture`
   showed `BadLocal: Torznab returned HTTP 503`, one success, one failure and
   a selectable `Fixture.2020.1080p` row from `GoodLocal` with seven seeds.
   This was read from the rendered client window, not inferred from RSS or
   a passed model test. The test card and both API servers were controlled.
4. **E2E-04 — PASS, native application restart and persistent cache.** The `e2e_04_restart`
   test shuts down the worker, opens a new engine/SQLite connection on the
   same directories and reopens the card. Its HTTP request counter must not
   change (not even `caps`). It also checks that disabling permissions cannot
   expose a cached network result. Separately, the rendered Linux client was
   closed, relaunched with the same private HOME, and the same card reopened.
   Its source row returned and the two fixture servers' request log did not
   grow, including capabilities requests.
5. **E2E-05 — PASS, native rapid navigation with a delayed HTTP reply.**
   `e2e_05_late_a` holds A's request while B is opened, searched and selected.
   It observes A's socket close, attempts the late reply, rejects A's row
   handle, and reads back B's file. The app's presentation tests check the
   target/stamp filter. In the private Xorg UI run, A's force-refresh request
   was held at the server. The dialog was dismissed, B opened and searched
   before releasing A. The rendered dialog showed B's `Other` release and no
   `Fixture` row both before and after the server's late reply attempt.
6. **E2E-06 — PASS, native Linux desktop and real mpv.**
   `real_http_mpv_atomic_save_clear_cache_library_resume_and_restart` disables
   Commons and RuTracker, configures no indexers, dispatches the manual HTTP
   input and plays the generated movie through Kadr/real mpv. It saves and
   reopens the result. In a separate private Xorg session, Commons, RuTracker
   and both indexers were disabled in the persisted settings before launch.
   Pasting an owned loopback MP4 **through the Sources UI** and clicking Play
   produced HTTP range reads and mpv progress in the application's SQLite
   database (at least 1.1 seconds), with zero indexer requests. A real mpv
   used X11 video and null audio in this disposable display. This did not
   exercise a native torrent file picker.

The live saved-card Commons test also measured mpv position and a full-film
duration (not just process startup or a HEAD request). This was repeated
through the native GUI: the library card was prepared locally; the credited
renditions were live, and a user-selected 240p rendition reported more than
one second of playback out of 634.57 seconds via real mpv. For *Big Buck Bunny* a
separate public range probe returned `206`, `video/webm`, and WebM bytes. An
older Commons upload can have file-history continuation even though its
current revision is complete; the provider requests one current revision and
never follows the cursor. No film was uploaded, modified or bundled.

**Captured client windows:** [live Commons renditions and credit](evidence/sources-commons.webp)
· [one available, one failed indexer](evidence/sources-partial.webp)
· [episode-specific results](evidence/sources-episode.webp). These are lossless
captures from the disposable Linux display; none is a picture of a user's
desktop. Playback assertions use the engine's persisted IPC measurements,
not the mere presence of a button in a screenshot.

## Reproduce

### Popcorn movie index

With the six-domain provider configuration, `indexes_live` returned three YTS
releases for *The Matrix* (1999), two Popcorn releases for *The Simpsons*
S01E01, and one for *Cowboy Bebop* S01E01. All three episode offers included
a mapped video filename. This checked live index responses, not playback of
those titles. The separate engine acceptance below also passed with the two
independent movie indexes registered.

The opt-in `tests::popcorn::live_engine_bbb_metadata_only` was run with a local
*Big Buck Bunny* (2008) card containing IMDb `tt1254207`, only Popcorn enabled,
and public BitTorrent metadata discovery. The real `/movie/tt1254207/torrents`
lookup yielded five candidate rows. Choosing a 1080p row returned one selectable
MKV (648,602,823 bytes in metainfo). No film payload was downloaded and no
player was started. This verifies discovery through file selection at the
engine command/event boundary, not a live Trakt login, GUI run, or continuous
availability of the third-party endpoint or peers.

```sh
cargo test --locked -p kadr-engine tests::popcorn::live_engine_bbb_metadata_only -- --ignored --exact --nocapture --test-threads=1
```

### Original resolver checks

**Linux run, 2026-11-27:** formatting, all-features Clippy with warnings denied,
all-targets build, architecture checks and the ordinary **parallel** workspace
suite passed. Summing Rust's printed summaries including the explicitly
re-executed subprocess tests: **518 passed, 0 failed, 14 ignored**. Ignored
live/platform tests are not counted as successes. Separately the six real
engine playback scenarios, two real player smoke tests, generated MP4 mpv/VLC
test, and live lookup of both Commons films passed. The clean-install Trakt
check was executed separately and failed at its documented prerequisite.

Two initially intermittent tests involved test processes inheriting open file
descriptors: the abrupt-exit cache test now runs in its own test executable,
and player IPC tests launch an immutable fixture instead of rewriting the
executable while sibling tests fork. No cache lock, validation, test assertion
or production player timeout was relaxed to pass the suite.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --locked --workspace --all-targets
cargo test --locked --workspace
python3 scripts/check-architecture.py
python3 scripts/check-architecture-test.py
cargo test --locked -p kadr-engine tests::real_ -- --ignored --test-threads=1
cargo test --locked -p kadr-media --test players_smoke -- --ignored --test-threads=1
cargo test --locked -p kadr-media --test video_smoke -- --ignored --test-threads=1
```

Explicit live checks, separate from the ordinary suite:

```sh
cargo test --locked -p kadr-catalog commons::tests::live_verified_files -- --exact --ignored
cargo test --locked -p kadr-engine tests::commons::live_commons_from_saved_card_plays_in_mpv -- --exact --ignored
cargo test --locked -p kadr-engine tests::commons::live_clean_install_trakt_commons_playback -- --exact --ignored
```

Use the [documented release/Trakt configuration](catalog.md#metadata) for the
last check. Do not embed a personal token, publish credentials or replace a
failing live lookup with fixtures.
