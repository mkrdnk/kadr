# MVP acceptance evidence

This is an evidence index, not a declaration that every target platform or a
publicly signed release has passed. `DONE.md` records exact final commands and
platform results; historical P0 evidence is not substituted for runtime tests.

| SPEC §10 | Linux evidence | macOS evidence |
|---|---|---|
| 1. Search/details/errors | Real Archive and TVmaze requests; native Xorg search, cards, posters, Sherlock seasons/episodes; fixture/cancellation tests | Provider/worker tests and native Sources-screen pixels passed; native interactive search not claimed |
| 2. Permitted source search | Archive rights re-fetch and allowlist, exact video `206` probe; actual catalogue → source → decoded mpv frame | Shared adapter fixture tests passed; no separate live Mac catalogue/UI claim |
| 3. Manual HTTP | Real mpv/VLC tests, fallback for missing validators/ignored Range, atomic whole-response save | Stable and fallback HTTP → actual mpv/save passed, including three additional required fallback runs; real VLC WAV/MP4 passed |
| 4. Magnet/`.torrent` | Real local BEP9 metadata exchange; native file picker and recognized file choices; real torrent playback measured before completion | BEP9 engine tests and actual torrent → mpv before completion passed; native picker not claimed |
| 5. Range/seek | GET/HEAD/206/416, byte equality, unavailable/corrupt pieces, concurrency, disconnect/timeout; actual mpv forward/backward IPC seek and continued playback | Range tests and real torrent/mpv forward/backward seek passed |
| 6. Player selection/browser | Preferences survive restart, actual mpv/VLC, installed Flatpak host playback; real Firefox decoded owned video from the local tokenized endpoint | Actual mpv/VLC WAV and generated MP4 passed; two VLC sessions retain separate ownership; native browser acceptance unrun |
| 7. Resume | Real IPC → SQLite → restart/resume; genuine EOF only, no clock-based completion; browser history without invented progress | Real mpv → SQLite → saved-file restart/resume and explicit restart passed |
| 8. Episodes | S01E01/S01E02 parser, native file-picker choice, ambiguous manual mapping, exact per-target source/file/history/progress isolation | Shared parser/storage tests; native episodes UI not yet claimed |
| 9. Bounded cache | Durable whole-torrent reservations, physical outstanding demand, inactive LRU, counted active leases, no-follow paths; actual active ClearCache protection | Cache tests including actual Darwin aliases and real torrent active-lease protection passed |
| 10. Permanent save | Native GTK save dialog, exact MP4 bytes, SQL saved record, clear-cache preservation; reader/verified-source atomic cancellation/no-clobber tests | Atomic-save tests and real stable/fallback HTTP and torrent save/clear/reopen passed; native save dialog unrun |
| 11. Security | IPv4 loopback media server, per-session OS-random capability, wrong tokens/traversal rejected; fixed URI source authority; pinned remote resolution/redirect checks; unsafe torrent paths and Unicode aliases rejected | Shared HTTP/path tests; no macOS packet-level claim |
| 12. Teardown | Owned player stop/reap, HTTP listener release, torrent cancellation, no abandoned save publication, source lifetime retained across player switching | Actual mpv/torrent teardown and native window/backend-ready/quit passed |
| 13. Distribution | AppImage/Flatpak built and installed, rendered UI, host mpv/VLC; installed-policy permanent-save helper | Strict-notice unsigned debug DMG build/mount and earlier native pixels/quit passed; optimized CI artifact result recorded in DONE; signing/notarization require credentials |

## Deliberate boundaries

- Metadata and playback permission are separate. TVmaze does not magically
  supply permission to stream copyrighted television; its adapter can return
  no sources. Manual linking remains available.
- The Archive adapter relies on explicit provider declarations, not an
  independent legal adjudication of every upload or jurisdiction.
- Browser opens are history events, not measured viewing. VLC's unsupported
  telemetry is explicitly unavailable. Process exit zero is not media EOF.
- Cache capacity is conservatively reserved for the whole torrent. The engine
  downloads selected content and required neighboring pieces, not the whole
  torrent merely to satisfy that reservation.
- A known finite HTTP representation can be saved atomically without Range or
  validators because it comes from one response. Unknown-length/live streams
  are not silently converted into a complete finite video file.
- Root/peer socket configuration checks are not a full hostile-peer audit or
  an OS packet-capture proof. Upstream cancellation cannot preempt blocked
  kernel filesystem IO.
- Local QA artifacts, unsigned candidates and public distribution are different
  outcomes. Package license provenance and signing prerequisites must remain
  visible; no generated certificate or invented copyright notice is accepted.

## Reproduce the functional suite

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo build --locked --workspace --all-targets
cargo test --locked --workspace -- --ignored --test-threads=1
```

The final command requires installed mpv, VLC and FFmpeg; the catalogue smoke
also requires Internet access. The torrent fixtures and generated media are
owned test data. None of these tests needs a public torrent swarm.
