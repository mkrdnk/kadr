# P0: local-only librqbit integration

**Status:** runnable Linux experiment, not a production torrent engine or an
approved replacement for the requirements in [SPEC](../SPEC.md#33-bittorrent-и-локальный-потоковый-сервер).
This implements part of the experiment proposed in
[bittorrent-spike.md](bittorrent-spike.md); that document's original research
status is not a claim about the code now present in `crates/kadr-torrent`.

## Boundary and integration

`kadr-torrent` is separate from `kadr-media`, so it does not couple player work
or HTTP parsing to librqbit. It depends on `kadr-media` by path, and uses its
existing `ByteSource`/`ReadFuture` contract and token-authorized `Server`.
No rqbit HTTP/control API is started.

```rust,no_run
use std::{sync::Arc, time::Duration};
use kadr_media::{Server, ServerOptions};
use kadr_torrent::LocalSession;

# async fn example() -> anyhow::Result<()> {
let session = LocalSession::new("dedicated-experiment-directory".into()).await?;
let torrent_bytes = tokio::fs::read("own-private-fixture.torrent").await?;
let selected = tokio::time::timeout(
    Duration::from_secs(30),
    session.add_selected(
        torrent_bytes.into(),
        1, // file ID from this torrent's metadata, not a path
        vec!["127.0.0.1:41000".parse()?], // an explicitly started local peer
        false, // do not overwrite existing files
    ),
).await??;
// selected.files contains id, relative_filename and len for all files.
// selected.source is fixed to file ID 1; it does not accept paths or URLs.
let source: Arc<dyn kadr_media::ByteSource> = selected.source;
let http = Server::start(source, ServerOptions::default()).await?;
// Pass http.private_url() deliberately to a local player; never ordinary logs.
tokio::time::timeout(Duration::from_secs(3), http.shutdown()).await??;
tokio::time::timeout(Duration::from_secs(3), session.stop()).await?;
# Ok(())
# }
```

Applications should put HTTP and torrent cleanup on error paths too. The
executable example does this for torrent stop; dropping HTTP requests
cancellation even if explicit HTTP shutdown was skipped by an error.
`LocalSession::Drop` requests cancellation, but async `stop` remains the
preferred lifecycle path. Shut down HTTP first, then stop the torrent session.
Duplicate adds are rejected: upstream `AlreadyManaged` ignores changed peers
and selection, so the wrapper never presents it as successful reconfiguration.
Pure input rejections leave the session usable. Once the real add starts,
failure or dropping its initialization future cancels the **whole** session
via a drop guard: call `stop`, discard it and create another session. This
intentionally coarse P0 rule avoids orphaned initialization/download work;
there is no per-torrent rollback API in this wrapper.

### Selected-file reads and seek

Each `read_at` creates a separate `handle.clone().stream(file_id).await`, seeks
to the absolute file offset with `AsyncSeek`, then performs one `AsyncRead`.
It returns at most 64 KiB, possibly a shorter verified prefix ending at a piece
boundary. The existing HTTP server already repeats short source reads.
Concurrent requests therefore never share a seek cursor or hold a mutex while
waiting for a piece.

Source inspection of the **published 9.1.0 crate**, not just docs.rs signatures:

- `torrent_state/streaming.rs`: reads check `get_have_pieces()` before
  `pread_exact`; unavailable pieces register a waker and return `Pending`.
  `FileStream::Drop` removes its stream state/waker.
- The same file maintains a **32 MiB per-stream lookahead**. Seek changes the
  stream position used by `iter_next_pieces`, and active streams' queues are
  interleaved. This adapter uses that built-in policy, not a custom piece
  deadline/prioritization API. A completed positional read drops that lookahead;
  normal background downloading continues independently.
- `session.rs`: `wait_until_initialized` is required before metadata/storage
  use. `Session::stop` pauses torrents, cancels its token and sleeps one second.
  It does **not** join every task. Tests separately probe listener release.
- The adapter also selects on the session cancellation token, so stopping a
  session interrupts a pending read rather than relying on a piece wakeup.

No claim is made about long-lived playback throughput, exact piece request
order or seek latency. Recreating a stream per short read trades allocation and
registration overhead for simple cancellation/concurrency semantics.

## Network constraints

The wrapper deliberately accepts less than NET-01:

- Only supplied `.torrent` bytes; **no magnet or HTTP URL resolution**.
- `private=1` is mandatory. Metainfo is parsed first with `list_only`, before
  starting a managed torrent. In 9.1.0 `torrent_state/live/mod.rs`, private
  torrents neither initiate PEX nor process incoming `UtPex` peers. Private
  torrents also do not serve metadata via the extension protocol; magnet
  metadata exchange is therefore outside this experiment.
- Initial peers must be explicit IPv4 loopback socket addresses with nonzero
  ports. IPv6, unspecified and non-loopback peers are rejected before adding.
- DHT is `None`; trackers and local service discovery are disabled at session
  level, and trackers are disabled again per torrent. Persistence is `None`;
  no configured global trackers, blocklist/allowlist URLs or proxy.
- Listener mode is TCP-only at `127.0.0.1:0`, IPv4-only, UPnP port forwarding
  off. No uTP UDP listener is configured. Actual listener addresses are asserted
  in tests. HTTP uses the existing independently tokenized loopback server.

**Important upstream detail:** disabling trackers does not prevent librqbit
9.1.0 from constructing its UDP tracker client. It creates a wildcard IPv6
UDP socket and a receive/parser task unconditionally. `ipv4_only` and
`ListenerMode::TcpOnly` do not remove that socket. The wrapper therefore also
sets `bind_device_name` to `lo` on Linux and `lo0` on macOS. Interface lookup
and socket-binding failures abort construction, without an unrestricted
fallback. Other platforms are refused. On Linux `SO_BINDTODEVICE` confines
the wildcard socket as well as peer sockets to loopback; a dedicated test
process inspects actual initial TCP/UDP descriptors with `getsockopt` and
requires both socket kinds, avoiding a vacuous pass. This passed as an
unprivileged user on this runner. Systems that deny interface binding fail
closed. macOS uses upstream's `IP_BOUND_IF`/`IPV6_BOUND_IF` implementation,
but its runtime behavior has **not** been verified.

This is a configuration boundary backed by published-source inspection and
loopback/interface-binding tests, not a general OS network sandbox or a
hostile-peer security audit.
Tests never join a public swarm or access external media. The generated
metainfo contains a disabled loopback tracker URL; no live public tracker is
used. Native syscall/network tracing has not been run.

The output directory must be dedicated to this experiment and owned by the
caller. `overwrite=true` explicitly permits rehashing existing files for
seeding/reopening; do not use it on unrelated user data. Torrent metadata paths
are handled by librqbit, not HTTP endpoints. The wrapper rejects an existing
symlink at the configured output root, then preflights every metadata file
path, rejecting existing symlink components under that root before starting
the torrent. A Unix test attempts an overwrite through a metadata leaf symlink
and confirms the outside sentinel is unchanged.

**This is not race-free symlink confinement.** A local process can swap paths
after these checks, and ancestors outside the configured root are not checked.
Never use an attacker-writable output directory. Production confinement needs
appropriate filesystem capabilities/sandboxing, not this preflight alone.
Disk quotas and malicious metadata fuzzing are not established by this spike.
Downloads may include adjacent file bytes needed to verify a shared piece.
Dropping a read cancels its availability wait/lookahead, **not** the torrent's
background download or uploads to other allowed local peers.

## Reproduce

The crate is included in the root workspace and lockfile:

```sh
. ~/.cargo/env
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo test --locked -p kadr-torrent --all-targets -- --nocapture
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo clippy --locked -p kadr-torrent --all-targets -- -D warnings
rustfmt --edition 2024 --check \
  crates/kadr-torrent/src/lib.rs \
  crates/kadr-torrent/tests/loopback.rs \
  crates/kadr-torrent/tests/support/mod.rs \
  crates/kadr-torrent/examples/local_torrent.rs
```

These commands select only the torrent crate and its media dependency; they do
not compile the GPUI application. The dependency is exactly
`librqbit = "=9.1.0", default-features = false, features = ["rust-tls"]`.
Neither `http-api` nor `http-api-client` is enabled. Some HTTP, discovery and
UPnP dependencies still compile upstream even though their services are not
started. `rust-tls` avoids OpenSSL here, but is **not** a native-build-free
promise: the resolved SHA-1/TLS stack includes `aws-lc-sys`, CMake and C code.

For a manually created private torrent containing only your own test bytes,
with a local seed already running:

```sh
mkdir -p dedicated-download
CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo run --locked -p kadr-torrent --example local_torrent -- \
  own-private-fixture.torrent dedicated-download 1 127.0.0.1:41000
```

The example prints the BitTorrent listener and file IDs/lengths, then deliberately
prints the private HTTP bearer URL to stdout for use with a local player.
Ctrl-C shuts down HTTP and stops the torrent session with bounded waits.
Its overwrite/rehash setting is enabled to permit using an existing local
fixture as a seed. Do not point it at an ordinary downloads directory.
Upstream filesystem/metainfo/stream error chains are replaced with safe
stage messages at the adapter boundary; `TorrentFile::Debug` redacts names.
The explicit public `relative_filename` field is for deliberate selection UI,
not ordinary logging. A directory-collision regression checks that Display,
Debug and chained formatting do not leak an absolute output path or filename.
`stats()` returns kadr's numeric-only `TorrentCounters` with a `has_error`
flag, not upstream `TorrentStats.error`: fatal disk-write errors can include
filenames in that upstream diagnostic string too.

### Deterministic integration tests

`tests/support` generates canonical multi-file bencode and correct SHA-1 piece
hashes from a byte formula. Pieces are 16 KiB; the selected file is 1,048,693
bytes, starts after a 777-byte prefix and ends inside a piece shared with the
tail file. No checked-in media fixture or external copyrighted content is used.

The seed deliberately has two corrupt interior pieces on disk. Initialization
rehashes them; neither peer can finish the selected file. That is stronger
than timing a small download and hoping a Range request beats completion.
The tests verify:

1. All file IDs, relative filenames and lengths; exactly file ID 1 is exposed.
2. Actual loopback ephemeral listener addresses for both peers.
3. HTTP `206`, `Content-Length`, `Content-Range`, `Accept-Ranges`, and exact
   beginning/middle/end bytes, end-to-start seek and cross-piece reads while
   the selected torrent remains unfinished.
4. Three concurrent HTTP ranges, each compared to the original selected bytes.
5. Corrupt/missing-piece bytes remain unavailable; no zero-filled disk data is
   exposed. Dropping a timed-out real adapter read releases its pending future.
6. HTTP disconnect, HTTP read timeout and HTTP shutdown release pending real
   adapter reads. The observer wraps the real source, not a mock byte source.
   Stream deregistration itself is established from upstream Drop code, not a
   public upstream stream-count API.
7. Torrent stop interrupts a pending direct source read. Both torrent listeners
   and HTTP listener reject new connections and can be rebound, even with
   torrent source handles still alive.
8. Non-private metainfo, invalid file IDs, external/IPv6 peers and port zero
   are rejected. Duplicate adds are rejected without disrupting the original
   source. EOF and zero-length reads return empty buffers.
9. A real failed storage initialization invalidates the session. A unit test
   separately proves the interrupted-add guard cancels, while its successful
   path disarms; this is not a timing-dependent interrupted-rehash test.
10. On Unix, output-root and metadata-file symlinks are rejected before torrent
    writes, with an outside sentinel checked unchanged.
11. Malformed metainfo and real filesystem errors do not reveal private input
    or output paths in diagnostics.
12. On Linux, initial IPv4/IPv6 TCP/UDP sockets are kernel-bound to `lo`,
    including the otherwise wildcard upstream UDP tracker socket.

Every asynchronous test operation has a bounded deadline (15 seconds overall
per helper; explicit missing-piece waits are 150 ms). Shutdown includes the
upstream one-second best-effort delay. Listener release is not proof that every
internal upstream task has joined.

## Evidence and remaining work

Initial evidence, Linux x86_64 Cloud Runner, kernel 6.1.189, rustc 1.100.0:
real loopback peer
integration ran in a disposable workspace seeded from the repository lockfile,
with the GPUI app included in dependency resolution but not built.
`libc=0.2.186` and `mio=1.2.4` were retained. That initial isolated run did not
change the repository root or build GPUI. The subsequent workspace integration
and additional privacy/network regressions are recorded in `DONE.md`.

Checks actually run:

- `cargo test -p kadr-torrent --all-targets -- --nocapture`: **4 integration
  tests + 1 unit test passed**, with the example also compiled.
- The original three-test integration suite was also repeated three more times;
  all passed. The final four-test suite took about 2.4 seconds on this runner,
  not a latency/SLO claim.
- `cargo clippy -p kadr-torrent --all-targets -- -D warnings`: passed.
- `rustfmt --edition 2024 --check` on all four Rust files: passed.
- `cargo run -p kadr-torrent --example local_torrent -- --help`: passed.
- Executed the built example with a separately generated private three-file
  seed, read and compared a 28,000-byte HTTP range, sent SIGINT, observed exit
  code 0, and verified the HTTP listener was closed. Its bearer URL was not
  included in the test report.

The Cargo commands above were run with
`--manifest-path /tmp/kadr-torrent-validation/Cargo.toml` and the documented
debug-zero/jobs-two environment. That disposable copy contained the app
workspace member for resolution, plus current media and torrent sources.
Those initial commands are distinct from the root workspace checks subsequently
run after integration; see `DONE.md` for the latter's exact commands/results.

### Observed Linux timing sample

One successful integrated debug-zero run after interface/privacy hardening
(`cargo test --locked -p kadr-torrent real_partial_peer_ranges_seek_concurrency_and_cancellation -- --nocapture`),
with both peers on
loopback, a 1,098,622-byte torrent, a 1,048,693-byte selected file and 16 KiB
pieces. Two interior selected-file pieces remain unavailable throughout.
Session construction is excluded from the metadata/init timers.

| Operation | Observed elapsed |
|---|---:|
| Partial seed metainfo validation + existing-data rehash/init | 2.412 ms |
| Downloader metainfo validation + new-storage init | 1.234 ms |
| First verified source read, 1,024-byte prefix after init | 43.304 ms |
| Forward HTTP seek to tail, complete 1,000-byte response | 9.837 ms |
| Backward HTTP seek to start, complete 40,001-byte response | 0.754 ms |
| Forward HTTP seek to middle, complete 30,001-byte response | 0.526 ms |
| HTTP shutdown with pending read | 0.289 ms |
| Downloader stop with pending direct read | 1.0017 s |
| Partial-seed stop | 1.0029 s |

Stop includes upstream's deliberate one-second sleep; listener closure/rebind
was separately asserted. These are one-run elapsed observations, not SLOs,
benchmarks or evidence that each seek downloaded uncached pieces. Background
download and the 32 MiB lookahead can populate the small fixture before later
requests; HTTP times include connection setup and full range-body collection,
not just time to first byte.

**macOS has not been tested.** Neither has playback in an external player,
magnet metadata, public networking, pause/resume, reopening/reuse measurement,
cache eviction/budget enforcement, package distribution, or resource/latency
benchmarking. Read cancellation does not stop unrelated torrent background
work. There is no claim that the SPEC's cache, limits UI, privacy disclosures,
production lifecycle guarantees or complete NET acceptance criteria are done.
The fixture is smaller than the 32 MiB lookahead; seek correctness is tested,
but seek-driven piece scheduling beyond that window is not measured.

### Provisional decision: librqbit for the Linux P0

Use librqbit 9.1.0 for the next measured **Linux P0** player integration: local
verified-piece reads, seek correctness, HTTP concurrency and cancellation now
have runnable evidence. Retain the byte-source boundary and the libtorrent
alternative. This is not a final production-engine selection.

Prove macOS embedding and cache/lifecycle behavior before promoting this
experiment. If precise piece deadlines or bounded cache behavior cannot be
provided, revisit libtorrent rather than inventing guarantees around
librqbit's streaming API.
