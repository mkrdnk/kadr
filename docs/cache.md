# Disk cache and permanent saves

`kadr-cache` implements SPEC §3.5, §7 and the cache/save parts of §8 without
GPUI or an application dependency. The disk manager is synchronous and must
run on a background worker. Saving is async and offloads blocking file work.
This crate does not implement torrent verification, file selection, the UI,
saved-file library records, or source descriptors.

## Public API

```rust,ignore
CacheManager::open(root: PathBuf, limit: u64) -> Result<CacheManager>
manager.reserve(content_key: &str, total_bytes: u64) -> Result<CacheLease>
manager.set_limit(limit: u64) -> Result<()>
manager.usage() -> Result<CacheUsage>
manager.clear_inactive() -> Result<CacheUsage>
manager.touch(content_key: &str) -> Result<()>
manager.check_save_space(destination_parent: &Path, total_bytes: u64) -> Result<()>

lease.path() -> &Path
lease.token() -> &str

pub struct CacheUsage {
    pub limit_bytes: u64,
    pub reserved_bytes: u64,
    pub actual_bytes: u64,
    pub active_entries: u64,
    pub active_leases: u64,
}

pub async fn save_source(
    source: Arc<dyn kadr_media::ByteSource>,
    destination: PathBuf,
    cancel: &tokio_util::sync::CancellationToken,
    progress: impl FnMut(SaveProgress) + Send,
) -> Result<()>

pub async fn save_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    total_bytes: u64,
    destination: PathBuf,
    cancel: &tokio_util::sync::CancellationToken,
    progress: impl FnMut(SaveProgress) + Send,
) -> Result<()>

pub struct SaveProgress {
    pub written_bytes: u64,
    pub total_bytes: u64,
}
```

`CacheManager` and `CacheLease` are `Send + Sync`. Wrap the manager in an `Arc`
when sharing ownership. A lease is deliberately not `Clone`; a second
`reserve` for the same key creates another counted pin. Reuse requires the
identical whole-torrent length. Keys are **1..=128 ASCII alphanumeric bytes**,
typically a hex infohash, and are **case-insensitive**, normalized to lowercase
on all platforms. This prevents distinct active pins from aliasing the same
directory on default macOS filesystems. Noncanonical persisted keys are
rejected. No titles, URLs, relative paths, separators, dots, Unicode, or
torrent-provided filenames are accepted as keys.

Use the standard owned application cache root:

```rust,ignore
// Entire block on a worker, never in GPUI render/event callbacks.
let paths = kadr_storage::AppPaths::discover()?;
let root = paths.cache_dir().join("torrents-v1");
let manager = Arc::new(CacheManager::open(root, settings.cache_limit_bytes)?);
let lease = manager.reserve("0123456789abcdef", whole_torrent_bytes)?;
// Give lease.path() as the engine's dedicated output directory.
// Retain lease until engine + HTTP + player + save readers have all stopped.
```

A configured cache-directory override replaces `paths.cache_dir()`, not the
`torrents-v1` ownership boundary. Do not open the application's general cache
directory, a user's Downloads directory, or a torrent-chosen directory as the
manager root. A root is exclusively managed, user-owned and private (Unix mode
0700). New ancestors/root use 0700; existing root permissions are validated,
not silently changed.

## Reservations, accounting and active protection

1. Obtain and validate metadata without creating torrent payload files.
2. Reserve the sum of **all files in the torrent**, not just the selected video.
   This covers unselected files and shared-piece overhead.
3. Only after `reserve` returns may the engine create data in `lease.path()`.
4. Keep at least one lease alive for every writer and reader, including an
   in-progress permanent save. A lease pins the **entire directory tree**.
5. Stop/join the engine and all reads before dropping their final lease.

Every manager operation and **lease destruction** may perform blocking I/O.
Move final lease destruction to the worker too. Dropping the manager before a
lease does not release the root lock or permit a second manager to evict data.

Reservations are charged even if zero bytes have been downloaded. Quota
decisions charge `max(reserved, actual)` per entry. `actual_bytes` is the sum
of logical regular-file lengths, not allocated blocks. Directory/index/lock
overhead is not video quota. Free-space preflight uses filesystem space
available to the current user, with **all outstanding reservations** charged
against it. Allocated blocks, rather than logical lengths, are subtracted
from that outstanding amount so sparse files do not hide future disk demand.
Credited allocation is capped at each individual file's logical length so
padding/preallocation in one file cannot fund missing bytes in another.
Physical-space availability is not a filesystem reservation: another program
can consume space later, and ENOSPC/EDQUOT remain ordinary handled errors.

An external engine with a raw output path cannot be physically sandboxed to a
byte quota by this manager. The engine must obey its validated whole-torrent
reservation and reject path traversal/symlink extraction. If any entry's actual
length exceeds its reservation, `usage` reports it and **every new `reserve`,
including reuse, fails with `ReservationExceeded`**. Stop the faulty writer;
inactive data can then be explicitly cleared. Never interpret this detection
as permission to continue writing. A hard kernel-enforced quota would require
filesystem/project quotas or a mediated torrent write API.

Only inactive indexed entries are eviction candidates. Monotonic persisted
access counters give deterministic LRU; successful reserve, explicit touch,
and lease release update recency. `set_limit` first checks the protected total:
an impossibly low request fails without evicting inactive data or changing
the old limit. Otherwise it evicts inactive LRU entries before accepting the
new limit. Zero disables nonempty reservations and clears inactive entries.
`clear_inactive` never removes an active entry. Eviction may already have
freed disposable entries if a later filesystem or space check fails; it is not
a transaction that can restore deleted video.

## Durable index and restart

The owned root contains:

```text
torrents-v1/
  manager.lock
  index.json
  index.pending       # only while replacing the index / after interruption
  data/
    <opaque key>/     # dedicated whole-torrent output tree
```

`index.json` is strict, versioned JSON (version 1). It contains only opaque
keys, whole-torrent reservations and LRU counters; it never contains titles,
source URLs, saved destinations, or video bytes. Updates create a private
exclusive temporary file, write and fsync it, rename it atomically over the
index, and fsync the root. The reservation is committed **before** creating
an output directory or returning its path. Index size is bounded to 16 MiB.
Unsupported/corrupt indexes fail closed; they are not reset or migrated
destructively. A failed index write/fsync poisons that manager until reopen.
Lease-drop failures cannot be returned, but the next operation reports them.

An exclusive OS file lock permits one manager per root. Active counts are
deliberately in-memory, never persisted: after an actual process exit, the OS
releases the lock and surviving bytes can be reused as inactive entries. A
second process cannot "recover" a still-live manager's active entries.
Restart removes only the known `index.pending` artifact after validating the
committed index version. Missing indexed directories are safe: they represent
interruption after reservation/before creation or after deletion/before index
update. Their reservation remains charged and a subsequent reserve recreates
the directory. Unknown/unindexed output directories fail closed and are never
automatically adopted or deleted.

Reuse preserves bytes; **the torrent engine must revalidate/recheck pieces**.
Cache presence is not evidence that bytes are complete or verified. There is
no persistent BitTorrent resume-state format in this crate.

## Filesystem boundary and privacy

The implementation targets Linux/macOS. Root/destination ancestor walking
opens directories relative to pinned handles with `O_NOFOLLOW`; `..` and
symlink ancestors are rejected, even if a symlink would point within the root.
The sole exception is macOS's fixed first-component system aliases: `/tmp`
and `/var` are substituted with `/private/tmp` and `/private/var` only when
the root entry is a root-owned symlink with exactly that expected absolute
target or its root-relative spelling (`private/tmp` / `private/var`).
Unexpected targets/owners are rejected. No descendant is canonicalized or
followed, `/tmpfoo` is not an alias, and other platforms are unchanged.
The manager stores this physical prefix so `lease.path()` is suitable for an
external torrent writer that also rejects symlink ancestors. Save/preflight
opening uses the same rule while callers may retain their original destination
spelling. Any other intentionally symlinked user location requires an explicit
trusted path-selection workflow to supply an approved physical path.

Accounting rejects symlinks and special files. Recursive eviction uses
fd-relative no-follow opens and unlink operations; a symlink is only unlinked,
never traversed. Only indexed names within the dedicated `data` directory
are removed. No cleanup walks an arbitrary saved-media directory. The index
and lock must be regular, single-link files, not symlinks/FIFOs/hardlink aliases.
This is not a sandbox against a malicious process with the same user identity
that can rename the user's owned/private root, tamper with their torrent
writer, or change private directory contents concurrently.

Errors have static sanitized `Display`/`Debug` messages and no underlying
source error. Manager/lease `Debug` redact paths, keys and tokens. Explicit
`path()`/`token()` getters are sensitive capabilities: **do not log them**.
The local index/media are plaintext under OS permissions, not encrypted.

## Permanent save

The caller selects an **absolute destination outside every managed cache
root**, with an existing user-owned parent that is not group/other-writable.
The independent save functions have no manager/root argument: the app
must enforce that separation using the approved canonical destination parent,
including configured root overrides. Never select a destination by combining
an untrusted torrent filename with an unchecked parent. The final name may
contain non-UTF-8 OS bytes.

Before starting a save, run `manager.check_save_space(approved_parent,
total_bytes)` on the cache worker (`source.len()` for a `ByteSource`). It opens
every parent component without following symlinks and compares the destination
and cache devices. On the
**same filesystem**, free space must cover both the saved copy and **all**
outstanding whole-torrent reservations, using the same per-file capped
physical-allocation credit as torrent reservation. On another filesystem,
only the destination's free space is charged. This does not alter video quota,
evict data, create output, or record a durable save reservation.

The application must serialize saves and prevent new torrent reservations or
cache-root switches from the check until the save job completes. Existing
torrent downloads may continue: their remaining space was included. Otherwise
concurrent saves/new reservations could each pass the same preflight and
overcommit disk. External processes can still consume free space, so the save
also rechecks remaining space and handles write failures. Destination safety
must account for **historical cache roots**, not just the current override:
switching A → B must not permit a permanent save inside A that a later return
to A could clear.

`save_source` captures the source's stable length and loops over `read_at`
offsets with requests of at most **64 KiB**. Short verified prefixes are
supported; early empty or oversized reads are rejected. Local files and torrent
sources share exactly this path. Torrent `ByteSource` implementations must
wait asynchronously for verified pieces, and release waits when their read
future is dropped. Cancellation selects against those waits.

`save_reader` supports **one coherent, known-length sequential response**,
including direct HTTP origins without Range support or stable validators.
The transport adapter supplies an `AsyncRead` and the actual response's known
byte length; it must not stitch separate responses or invent a length. Unknown
Content-Length responses are explicitly unsupported by this MVP save path.
An adapter exposing chunks can use `tokio_util::io::StreamReader`; this crate
does not depend on an HTTP download implementation.

The sequential path also reads at most 64 KiB per request. An early EOF is a
source error. After exactly the declared length, a cancellable **one-byte EOF
probe** must return empty before publication; excess bytes or transport errors
reject the save. This probe also runs for an advertised empty response, unlike
an empty `ByteSource`, which needs no reads. Cancellation interrupts body and
EOF waits. Reaching 100% byte progress does **not** imply save completion:
the EOF probe, fsync and final publication must still succeed.

Both entry points share one bounded streaming loop and the exact same `Writer`
temporary/space/fsync/no-clobber publication implementation.

The destination is never partially visible under its final name:

1. Check final-name absence and physical free space.
2. Create a random private (0600) `.kadr-save-<128-bit token>.part` sibling with
   exclusive creation. Recheck remaining space before each bounded write.
3. Report initial and per-write progress; an empty file reports `(0, 0)`.
   Callbacks must be fast, must not block the async
   executor, and must not log private data.
4. Fsync the complete file, then check cancellation again.
5. Publish with an atomic same-directory hard link, which **cannot replace any
   existing final entry**, including a concurrent file or a dangling symlink.
6. Fsync the parent, remove the temporary name, and fsync the parent again.

The RAII temporary guard removes its own sibling on source failure, explicit
cancellation, or async task/future interruption. A blocking filesystem task
already in flight finishes its bounded operation and then drops its guard.
Cancellation cannot roll back an already published final file. Likewise an
fsync error after publication may return an error while leaving a complete
final file; never respond by overwriting/removing that final name blindly.
The filesystem must support hard links and directory fsync.

Abrupt process termination/power loss may leave an unreferenced `.part` sibling;
it cannot expose a partial final file. This crate does not scan arbitrary user
directories and delete matching files on restart, since they could belong to
another live save. Such crash artifacts require explicit user-directed cleanup.
Normal cancellation/future-drop cleanup is covered by tests. Saved final files
are outside the index and remain untouched by cache eviction/clearing.

## Validation

From the integrated workspace, with the Rust environment loaded:

```sh
. ~/.cargo/env
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --locked -p kadr-cache
CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo clippy --locked -p kadr-cache --all-targets -- -D warnings
cargo fmt --all -- --check
```

No GPUI build is needed. Implementation validation initially uses a standalone
temporary workspace containing only `kadr-cache` and `kadr-media` to avoid
editing the parent's root membership/lockfile.

On Linux, that standalone workspace passed **37 tests**, `cargo clippy
--locked -p kadr-cache --all-targets -- -D warnings`, and `cargo fmt -p
kadr-cache -- --check`, using two jobs and disabled debug information. The
integrated-root commands above are for the parent integration; they were not
substituted with a GPUI build.

Tests cover real-directory inactive LRU/touch eviction, entire-tree active
pins and same-key reference counts, process locking while leases outlive the
manager, persisted byte/LRU reuse, missing directories and pending-index
recovery, abrupt process exit with stale active state, invalid/future indexes,
private permissions/diagnostics, quota and active-limit refusal, overrun
detection, simulated out-of-space including sparse/all-outstanding demand,
same-filesystem save headroom, allocated-byte credit, different-filesystem
budget branching and overflow, injected Darwin alias allowlist/ownership
decisions and unchanged Linux path handling, symlink escapes, bounded/short/empty saves,
actual free-space refusal, local
file byte equality, progress, saved-file survival after cache clear, existing
and racing final names, source-contract failure, cancellation during piece
waits, and task-abort cleanup. A macOS-only integration test additionally
exercises real `/tmp` and `/var/tmp` aliases, physical lease paths accepted by
an independent strict walker, save/preflight, and descendant-symlink rejection.
Runtime checks here are Linux; that macOS test and real hardware/disk-exhaustion
fault injection remain separate validation.

Sequential-reader tests additionally cover bounded duplex streaming, exact/
empty/truncated/excess bodies, transport errors during the body and EOF probe,
existing/racing destination preservation, cancellation before start and final
publication, cancellation during empty/nonempty EOF waits, body-wait aborts,
and deterministic abort with a blocking write queued (cleanup after that
bounded operation completes). They do not require networking or fill a disk.
