# Pure core models

`crates/kadr-core` implements the first IO-independent part of SPEC §3, §6 and
§8. It has no GPUI, async runtime, filesystem access, network requests, catalogue
provider, torrent engine, or player process. Its only dependencies are `url`,
`sha2` and `regex`; these versions already occur in the workspace lockfile.
The crate is included in the root workspace and lockfile.

## Manual sources and privacy

```rust
use kadr_core::source::ManualSource;

let source = ManualSource::parse_link("https://example.org/movie.mp4")?;
let storage_key = source.id();
// Pass to an IO adapter, never routine logging:
let url = source.http_url().unwrap();
# Ok::<(), kadr_core::source::SourceError>(())
```

- `ManualSource::parse_link` accepts only HTTP(S) and magnet links. HTTP requires
  an explicit `://` authority with a host. Credentials (even empty `@`),
  unescaped whitespace/control characters, backslashes, malformed percent
  escapes, invalid host/port and inputs the WHATWG parser would repair by adding
  authority slashes are rejected. HTTP fragments are removed. The parser is
  deliberately stricter than a browser address bar; UI may offer explicit
  correction, but core does not silently trim user input.
- Valid HTTP path/query escapes and international domain names are supported.
  Local/private IP addresses are **not** blocked: a direct, user-selected
  external-player link is not an SSRF authorization policy. Any future proxy
  adapter must validate destinations, redirects and DNS resolution separately.
  A valid link does not prove reachability or a supported video format.
- Magnets require a valid `xt=urn:btih:` (40 hex or 32 unpadded base32 digits) or
  `xt=urn:btmh:1220` plus 64 hex digits (BEP 52 SHA-256 multihash). Equivalent
  duplicate hashes are accepted; conflicting hashes of the same version fail.
  Hybrid v1/v2 magnets are accepted. Unsupported `xt` namespaces and optional
  parameters are retained but not interpreted, validated as trackers, or fetched.
  Magnet paths, authorities and fragments are rejected.
- `ManualSource::torrent_file(PathBuf)` requires an explicit absolute path with
  a case-insensitive `.torrent` extension, no parent component/NUL/trailing `/`.
  It does **not** read bencode, check existence, resolve symlinks, or verify that
  the entry is a regular file. Those checks belong to `kadr-media`. A relative
  path is not accepted because its identity would depend on an implicit cwd.
- `Debug` for sources and torrent paths is redacted, including nested
  candidates/selections. Errors have static descriptions with no input or
  lower-level error chains. The explicit `http_url`, `magnet_uri`, `torrent_path`,
  `TorrentRelativePath::as_str/as_path/file_name` accessors expose sensitive data:
  adapters/UI must not log them. `url::Url` itself is not a redacting wrapper.

## Storage identity

`SourceId::as_str()` is an opaque deterministic key; `FromStr` validates stored
version-1 keys. The serialization is:

```text
source:v1:{domain}:{lowercase hex SHA-256(domain bytes || NUL || identity bytes)}
```

Domains and identity bytes:

| Domain | Identity |
|---|---|
| `http` | `url`-normalized HTTP(S) URL without fragment; full query retained |
| `btih` | decoded 20-byte v1 info hash, independent of case/base32/trackers/name |
| `btmh` | decoded 32-byte v2 SHA-256 info hash, only when v1 is absent |
| `torrent-path` | absolute component-normalized `PathBuf::as_encoded_bytes()` |

Hashing uses SHA-256, **not** Rust's unstable `DefaultHasher`. Frozen HTTP/magnet
vectors guard accidental format changes. Changes to normalization/identity need
a new version and storage migration. Queries are not sorted or discarded:
signed URLs and distinct query resources must not be conflated. Different
signed URLs may consequently produce different IDs for the same video.

A hybrid magnet uses the v1 identity, so it matches its v1-only spelling. A
v2-only spelling of that hybrid cannot be known to be equivalent without
metadata; resolving that alias belongs to the engine/storage layer.

The local path ID survives reopening the same spelling on the same platform,
not moves or replacement of its contents. Symlink aliases can have different
IDs. Non-UTF-8 Unix paths are accepted without lossy conversion; their keys are
not promised portable across operating systems. A `.torrent` path and a magnet
are not deduplicated until an IO adapter obtains their common content hash.

An unkeyed digest is **not** encryption, a session capability, or protection
against dictionary attacks. Store original source descriptors privately if
reopening is needed; a key alone cannot reconstruct a source.

## Torrent paths, video candidates and selection

`TorrentRelativePath::new` validates UTF-8 metadata names using `/` explicitly,
independent of the host's path parser. It rejects absolute/empty paths, empty
components, `.`/`..`, backslashes, drive/stream colons, control characters,
reserved punctuation/device stems, and trailing dots/spaces in components.
Valid percent sequences are literal filename characters: **never URL-decode
these paths**. Engines with component arrays must validate the entire joined
path; non-UTF-8 torrent metadata needs an explicit engine policy.

This prevents lexical traversal, not filesystem attacks. The IO adapter must
use a trusted root and symlink-safe opens (including pre-existing ancestor
symlinks), verify regular files, and handle duplicate/colliding metadata paths.
It must not join arbitrary endpoint input or decoded URL paths to that root.
No guarantee is made about Windows filesystem semantics; Windows is outside
SPEC v1.0.

`VideoCandidate::from_file` filters a small case-insensitive container-extension
allowlist and retains engine `FileIndex`, validated path and byte size. It does
not sniff content, guarantee codecs, discard zero-byte entries, or hide
unrecognized video names. Subtitles, artwork, executables and `.nfo` are not
video candidates.

Episode recognition examines the **basename only**, with delimited,
case-insensitive `S01E02` and `1x02` patterns. Season zero supports specials;
episode zero and integer overflow become ambiguous rather than valid matches.
The `x` form accepts at most two season digits to avoid common resolution
markers such as `1280x720`. Larger season values in that spelling, uninformative
names and attached alphanumeric tokens are unrecognized, not guessed.

`EpisodeRecognition` is explicitly `Recognized`, `Ambiguous` or `Unrecognized`.
Multiple markers (even repeated identical markers), episode chains and ranges
are ambiguous. Ambiguous lists contain successfully parsed markers/endpoints,
not an assertion about every episode in a range; no range expansion or title
inference is performed. `VideoSelection::match_episode` reports duplicate
encodes and matching multi-episode files as ambiguous. It is only advisory and
does not modify selection. It may report no match for an interior episode of
an ambiguous range; the full candidate list is always available.

`VideoSelection::select` accepts any candidate, including unrecognized and
ambiguous files. Invalid choices do not erase an earlier selection. Duplicate
engine indices are rejected. Engine indices must remain stable for a given
source's metadata, and must not be replaced with filtered-vector offsets.

## Small session model

`PlaybackSession` separates source readiness from external player lifetime:

```text
HTTP:                         Buffering -> Ready
Torrent: Metadata -> Connecting -> Buffering -> Ready
                                     ^           |
                                     +-----------+ (rebuffer)
Active source -> Failed or Stopped
```

`SessionEvent` records facts supplied by adapters, not IO commands. Invalid
transitions leave state unchanged; stop is idempotent. Failure/stop are terminal
(a retry creates a new session). Player exit/switch does not restart source
acquisition. Applying stop/failure does **not itself** kill a process or free
resources: the IO orchestration layer still owns those obligations.

`PlaybackKey` includes source identity plus optional selected file and explicit
episode assignment, without requiring a catalogue ID. The caller supplies a
source kind consistent with that key. Use a fresh session/key when changing the
selected file or episode; progress must never migrate silently between them.

mpv/VLC adapters declare telemetry capability rather than the model assuming it
from a player name. Browser telemetry cannot be enabled. `record_progress`
requires a running telemetry-capable player, accepts unknown duration and
backward seeks, and rejects positions beyond known duration. `last_progress`
is the last observed snapshot, not live progress after player exit/switch.
Storage/adapters add timestamps and completion policy; clock reads, automatic
completion detection, IPC, player detection, resume commands and persistence
are intentionally not implemented here.

## Validation and remaining integration

Tests cover malformed/normalized URLs, redaction, credential rejection,
hex/base32/v2/hybrid/conflicting magnets, source-ID storage vectors, explicit
local paths, traversal and reserved components, case-insensitive video filters,
episode ambiguity/overflow/resolution markers, manual selection, session
transitions and unavailable telemetry.

The implementation is not end-to-end acceptance of CAT-04, NET-01, PLAY-03/04/09
or SPEC §10: no source is opened, metadata fetched, torrent parsed or player
launched by this crate. Linux is tested; macOS runtime/CI remains unverified.
No large provider/plugin/async trait framework is
introduced before real IO adapters establish the required interfaces.

### Checks performed for this change

On Linux with Rust/Cargo 1.100.0, copied the crate to
`$DELTA_SCRATCH_DIR/core-check` as a standalone package and seeded its lockfile
from the existing workspace lock. Initial standalone resolution modified only
that scratch lock. Compared the resulting package/version set with the root:
the only additional entry is `kadr-core 0.1.0`; no dependency versions changed.
This initial isolated check did not edit root `Cargo.toml`/`Cargo.lock`;
`libc 0.2.186` and `mio 1.2.4` pins were preserved during later integration too.

Final checks (both Cargo commands also used `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_BUILD_JOBS=2`):

| Command | Result |
|---|---|
| `cargo test --locked --manifest-path "$DELTA_SCRATCH_DIR/core-check/Cargo.toml"` | 21 integration tests pass: 10 source, 7 torrent/video, 4 session; no unit/doc tests |
| `cargo clippy --locked --manifest-path "$DELTA_SCRATCH_DIR/core-check/Cargo.toml" --all-targets -- -D warnings` | Pass |
| `rustfmt --edition 2024 --check crates/kadr-core/src/*.rs crates/kadr-core/tests/*.rs` | Pass on original worktree files |

Whole-workspace tests/builds and `cargo fmt --all` subsequently passed after
integration; see [DONE.md](../DONE.md) for exact final commands/results.
The isolated core checks above did not exercise macOS, actual bencoded files,
network requests or external-player runtime. IO evidence belongs to the
media/torrent/desktop checks, not these pure model tests.
