# Production torrent engine

`kadr_torrent::engine` is the production-facing adapter over pinned
`librqbit 9.1.0`. It is separate from the original, unchanged local-only
`LocalSession` P0 API. The engine does not own the cache budget, media HTTP
server, player, UI, or persistent library.

## Prepare → reserve → start

1. `.torrent`: `PreparedTorrent::from_bytes(Bytes)` parses and validates
   synchronously, **offline**, without creating a session, directory, or file.
2. Magnet: `PreparedTorrent::from_magnet(&str, EngineOptions,
   CancellationToken).await` resolves BEP 9 metadata using upstream
   `list_only: true`. Metadata exchange requires explicit network consent,
   but no payload storage is initialized. The temporary session is stopped.
   Only `magnet:?` input is accepted here, never HTTP/file URLs.
3. Display `prepared.files` (`TorrentFile { id, relative_filename, len }`) for
   selection. `info_hash` is the lowercase 40-character BTv1 hex cache key;
   `total_bytes` is the **whole torrent** size, not selected-file size.
4. The cache manager must reserve that whole size **before** `start`. This
   conservative reservation includes unselected files and shared boundary
   pieces. Give the engine a dedicated, private, exclusively owned directory
   for this hash. Never start two sessions concurrently in the same root.
5. `prepared.start(file_id, cache_output, options, cancel).await` opens storage
   under the fixed `cache_output/payload` subfolder, rehashes existing blocks,
   and registers a selected-file stream **while paused**, before allowing any
   peer requests. It then starts downloading only the selected file and its
   boundary pieces, unless the download limit explicitly remains paused.
   `PreparedTorrent` is `Clone`; `start` borrows it for reopening/reselection.
6. Give `playback.source.clone(): Arc<dyn ByteSource>` to the existing
   loopback-only `kadr_media::Server`. The torrent library's own HTTP/API server
   is never started.

The original metainfo is private and immutable. `start` reparses it rather
than trusting caller-mutable display paths or lengths. Paths are validated
component by component with `kadr_core::torrent::TorrentRelativePath`; traversal,
absolute paths, separators embedded in components, reserved names, symlink
attributes, duplicate case-insensitive names, and file/directory collisions
are rejected before opening payload storage. The torrent's display `name` is
never used as an output directory.

File collision keys use **NFKC → full default Unicode case folding → NFD**
per component, deterministically on every platform. Both exact-path duplicates
and file/directory prefix collisions use those keys. This rejects aliases such
as `é.mp4` / `e` + combining acute + `.mp4`, `Straße.mp4` / `STRASSE.mp4`,
and Greek sigma variants before two metadata files can share one physical
file on a normalizing or case-insensitive filesystem. Otherwise later writes
could invalidate bytes already marked hash-verified. Normalized components
must also pass the same safe-component checks: compatibility characters must
not introduce separators, traversal or reserved names. This deliberately
conservative policy can reject distinct Linux filenames that would alias
elsewhere. Original display/storage names, including distinct Cyrillic names,
are preserved; the normalized strings are validation keys only.

Existing symlink components in the cache root, its ancestors, and all payload
paths are rejected. This is **preflight**, not a race-free sandbox: the cache
manager must prevent other processes/users from modifying this private tree.
External edits of already verified files are unsupported; initialization errors
are reported without private paths.

## Options, limits and consent

`EngineOptions::default()` has `network_enabled = false`, so all session
creation fails closed until the application explicitly grants consent.
Preparing `.torrent` metadata offline does not require consent.

| Option | Behavior |
|---|---|
| `network_enabled` | Consent for BitTorrent sockets and metadata exchange |
| `discovery_enabled` | Production DHT/trackers; defaults true, but gated by consent |
| `initial_peers` | Optional explicit peers, also usable for metadata resolution |
| `peer_limit` | Positive maximum connected payload peers (default 40) |
| `download_limit` | `RateLimit::{Unlimited, Bps(u32), Paused}` |
| `upload_limit` | Same enum; defaults to **Paused**, actual upload disabled |

`Bps(0)` is rejected, never treated as unlimited. The pinned upstream rate
limiter operates on 16 KiB requests and cannot represent rates below one
request per second: `Bps(1..16384)` is rejected explicitly instead of silently
exceeding the requested limit or stalling a peer. Unlimited and numeric rates
use upstream session-wide byte rate limiters.

`download_limit = Paused` creates a paused torrent after initialization;
rehashing existing cache files still happens. `upload_limit = Paused` enables
the upstream **`disable-upload`** feature and `SessionOptions::disable_upload`;
peers cannot receive pieces, while downloads remain possible. This is not a
tiny nonzero upload rate. Metadata/control handshakes and tracker/DHT traffic
are not payload uploads and are not governed by payload byte limits.

`playback.pause()`/`resume()` control the whole torrent. `set_limits` can
change numeric rates live. Setting upload `Paused` on an initially
upload-enabled session conservatively pauses **both** directions, preventing
uploads rather than pretending a zero rate is safe. An initially
upload-disabled session must be stopped and restarted to enable uploads
(upstream's disable flag is immutable). The application can expose one simpler
rule: stop playback before applying network settings to the next session.

Discovery-disabled mode is designed for deterministic local tests: DHT,
trackers and LSD are off, explicit peers must be nonzero-port IPv4 loopback,
and **all** underlying sockets are bound to `lo` on Linux or `lo0` on macOS.
There is no unrestricted fallback if binding fails. Even public-marked test
torrents cannot exchange traffic outside that interface.

### Production socket behavior

With consent and discovery enabled, BitTorrent traffic is intentionally not
restricted to loopback. Incoming peers use an ephemeral wildcard TCP listener;
outgoing TCP peers, HTTP(S)/UDP trackers, DHT UDP bootstrap/query traffic and
peer exchange are possible. LSD is always disabled. DHT persistence and torrent
session persistence are disabled. **UPnP/NAT port forwarding is always off.**

In particular, upstream creates a wildcard UDP tracker socket even with TCP
peer transport and trackers disabled. In local test mode that hidden socket is
also interface-bound; in production it is part of the explicitly permitted
BitTorrent network activity. None of these BitTorrent sockets are the media
HTTP server: the latter remains token-authorized and bound to loopback only.

## Verified reads, seek and save readiness

The selected source reuses `TorrentByteSource`, also covered by the P0 HTTP
Range tests. Every `read_at` owns an independent upstream file stream, seeks
to that file-relative offset, and returns at most 64 KiB of **hash-verified**
bytes (possibly a short prefix at a piece boundary). Unavailable pieces wait;
there is no raw filesystem fallback that could expose preallocated/unverified
bytes. Dropping a read removes its stream and availability waker. Concurrent
range requests never share a cursor.

Active seeks update upstream's 32 MiB lookahead window. Each read drops its
lookahead when done, while normal selected-file downloading continues.
Production startup retains a separate stream at offset zero until the first
real read has registered and sought its replacement stream. This installs
contiguous-prefix priority before the initial peer request burst instead of
spending that burst on upstream's ordinary first/last/middle file-piece order.
The startup stream is then dropped even if the first read seeks elsewhere,
so a stale prefix lookahead does not compete with later requests. The original
P0 `LocalSession` does not register this startup stream. This is the upstream
streaming scheduler, not a custom piece deadline algorithm or a throughput/SLO
guarantee.

`stats() -> EngineStats` exposes numeric counters/rates/peer count and
`EngineState::{Initializing, Downloading, Ready, Paused, Stopped, Error}` only.
No upstream error strings cross the boundary. `selected_file_bytes` comes
from verified per-file progress; `selected_file_ready()` and
`wait_until_ready(cancel).await` mean the **entire selected file** is checked.
Use the verified source to copy that file to permanent storage, not an
unchecked guessed filesystem path. Readiness does not imply the other files
are complete.

`total_bytes` remains the full reservation size; upstream `progress_bytes`
counts the wanted pieces, including shared boundaries. Rates describe payload
traffic. Upload counters are upstream live-state counters and can reset across
pause/restart; they are not lifetime accounting or cache disk usage.

Reopening in the same owned root uses `overwrite: true`, no truncate, and
rehashes existing pieces before trusting them. Already checked cached bytes
work without any live peer. There is no fast-resume bitmap that skips integrity
checks.

## Cancellation and shutdown

Callers own metadata/start deadlines (for example `tokio::time::timeout`).
Cancel the supplied token to cancel preparation/start/playback. Dedicated child
tokens and drop guards cancel the session if the caller drops the future after
upstream spawned initialization. Once a session exists, a lease also schedules
best-effort async `Session::stop` reaping when dropped; downloads never remain
intentionally unowned. No new worker task is spawned merely to perform reads.

Shutdown the media HTTP server and player, then call `playback.stop().await`,
then release the cache lease. Stop cancels reads immediately and requests
upstream shutdown with its one-second grace period; repeat calls are harmless.
If explicit shutdown is skipped, dropping the playback lease cancels its token
even if someone still holds `source.clone()`.

Upstream does not expose a join-all-tasks API. Async Drop reaping requires a
live Tokio runtime; cancellation still occurs synchronously without one.
Stopping while blocking file initialization/hashing is in progress does not
promise an immediate join of that blocking operation. Keep the private cache
root owned until explicit teardown has finished and do not reuse it concurrently.

All adapter errors are fixed stage messages, never raw magnet URLs, torrent
filenames, output paths, upstream diagnostics or debug-formatted metainfo.
Applications must also avoid enabling verbose upstream tracing in ordinary
user logs: librqbit itself can trace metainfo and filesystem errors.

## Validation

The engine unit tests generate their own deterministic multi-file torrents.
They cover offline metadata, consent rejection before payload creation,
metadata-only public-marked magnet exchange against an explicit loopback peer,
selected-file first/last reads, full save readiness, cached restart with the
seed stopped, real upload-disable configuration, paused limits, cancellation,
listener release, unsafe paths and symlink rejection. A rate-limited startup
regression also verifies the fourth contiguous prefix piece is present in the
initial four-piece request burst, before a player has registered any reads;
the same assertion fails without paused startup stream registration. No test
joins an external swarm. Existing P0 `LocalSession`, Range, privacy and
socket-confinement tests remain in place.

After the parent workspace integrates dependency lockfile changes:

```sh
. ~/.cargo/env
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo test --locked -p kadr-torrent
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo clippy --locked -p kadr-torrent --all-targets -- -D warnings
```

The engine-only integration does not establish public-network swarm
interoperability, macOS runtime behavior, real video seek throughput, or GUI
save/play flows; those need their respective integration/manual checks.
