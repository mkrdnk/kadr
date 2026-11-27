# Local rutracker-api 0.2.1 fork

This directory starts from the actual published crates.io sources, not a
replacement implementation presented under the upstream name.

- Upstream: <https://github.com/johnlepikhin/rutracker-api>.
- Published archive: <https://static.crates.io/crates/rutracker-api/rutracker-api-0.2.1.crate>.
- Archive SHA-256: `6e292fc0929b96a65a9796272b01d5eb69c6776a0e3d59697086d3a9738747cf`.
- Published Git SHA: `f1586eb7cb4a895c22903c3ce6972c071cf61324`
  (also retained in `.cargo_vcs_info.json`).
- Upstream author attribution and `MIT OR Apache-2.0` declaration are
  retained in `Cargo.toml`. See `LICENSE-PROVENANCE.md` for the explicitly
  approved canonical-text treatment of upstream's missing license files.

## Intentional local changes

1. Only the desktop-required search, topic/magnet, login and torrent-download
   source modules are retained. Optional API v1, forum browsing, examples,
   mockito integration tests and the upstream lockfile are not vendored.
2. Reuse kadr's already locked reqwest 0.13.5 with normal certificate and
   hostname verification. No invalid-certificate option or custom base URL.
   Only `https://rutracker.org:443` is allowed, including every redirect.
   Credentials in URL authorities, alternate ports, mirrors, cross-origin
   redirects and HTTP downgrades are rejected before transmission.
   At most five redirects; referer forwarding, environment proxies and
   automatic request retries are disabled. Transport is direct by default.
   The desktop adapter can explicitly supply `reqwest::Proxy::all` through
   `ClientBuilder::proxy`; this replaces the direct route without fallback,
   not the authorized origin or TLS policy. No SPKI/certificate pin is
   claimed: the fixed HTTPS origin uses the platform TLS trust store.
3. Timeout defaults: 5-second connection, 15-second whole request. The
   adapter also wraps operations in a 20-second deadline and supports
   cancellation. HTML parsing is synchronous over the bounded input, so it
   cannot be interrupted in the middle of a parser poll. There are at most two first-page searches
   per call and 50 returned offers. No topic-per-result requests.
4. Read both HTML and torrent responses incrementally with a real 4 MiB
   limit. Check advertised length early and every chunk before appending;
   never buffer the full body before enforcing the limit.
5. Remove every upstream tracing event and the direct tracing dependency,
   including events containing usernames, search queries, URLs, IDs and HTML
   parse details. No subscriber/filter trick is used.
   Transitive html5ever and selectors dependencies also debug/trace-log
   complete HTML tokens/elements. The explicitly approved `log/max_level_info`
   feature on the already locked log package compiles these events out.
   **Cargo feature unification makes this a global ceiling for log in the
   executable's dependency graph**, not just this crate. Info/warn/error
   remain available; this does not disable the separate tracing crate.
   A regression test asserts the compile-time ceiling. Audited remaining
   html5ever warning paths contain fixed messages or namespace identifiers,
   not HTML tokens/attributes/account contents; selectors and scraper have no
   info/warn/error events. Keep this audit when updating parser dependencies.
6. reqwest's default cookie jar calls cookie_store's logging convenience
   method, which can log entire cookies. `client/cookies.rs` instead uses
   the same RFC6265 implementation's quiet parse/insert API. This adds a
   direct edge to reqwest's already required cookie_store dependency, not
   another unrelated package. Cookies are memory-only, origin/path scoped,
   expiration-aware, and outgoing Cookie headers are marked sensitive.
   Cookie input is bounded to 4 KiB each and 64 records; exceeding the
   record bound clears the local session rather than growing indefinitely.
7. Keep absent search size/seeds as `None`; do not interpret a registration
   timestamp as a size. Unknown pages fail rather than masquerading as empty
   results. Login-form HTML is recognized in search and topic responses.
   Login HTTP errors are checked before interpreting success markers.
   Unsupported Windows-1251 input fails rather than silently altering
   credentials or search words.
8. Enable reqwest's already available `socks` feature without adding packages.
   The adapter validates proxy endpoints with `kadr_core::proxy::is_valid_endpoint`
   before building an explicit proxy; credentials in proxy addresses are rejected.
   The vendored builder accepts the resulting proxy object rather than adding
   a second endpoint parser. Builders are cloneable, but clients and cookie jars
   are not: every build still creates a fresh, independent in-memory session.
   Client debug output omits the HTTP transport so it cannot expose the proxy.

This is a private path dependency, excluded from the root workspace. It is not
an upstream release or a general-purpose mirror/proxy API. `kadr_catalog`
exposes only its own redacted error and offer types; it does not expose the
fork's raw errors, debug output or HTML.

## Adapter semantics and limitations

`kadr_catalog::rutracker::Client` owns an anonymous client and an optional
authenticated session. Login uses a fresh candidate jar; failure, cancellation
or dropping the login future cannot install its jar. Logout destroys the
authenticated jar locally, not the server-side session. Passwords and usernames
are not saved in the client. The app remains responsible for protecting any
credential-store use and avoiding network wire-level debug logging.
`Client::new()` remains direct; `Client::new_with_proxy(Some(endpoint))` uses
the same route for anonymous operations, every fresh login candidate, and the
authenticated session's subsequent downloads. Logout does not change the route.

Search tries distinct localized and original title words. It conservatively
compares complete slash/pipe-separated title alternatives and explicit
production-year metadata, not a year occurring anywhere in a release title.
Selected episodes require recognizable season/episode scope; season packs
remain candidates and the original title preserves their full scope for explicit
selection. Unsupported labels/layouts can yield no offers. This is not an exact
matching or rights guarantee. RuTracker can require login even for search;
`AuthenticationRequired` is surfaced without an implicit login prompt.

Magnet lookup is lazy and the URI is validated by kadr-core. A missing magnet
is `Ok(None)`; an invalid advertised magnet is an error, not a synthesized link.
The caller decides when to offer authenticated torrent fallback. Download
requires the torrent content type and rejects an obviously mislabeled body;
the torrent engine still must perform full metainfo validation.

No live login or credentialed connectivity checks are part of the tests.
Transport tests use loopback HTTP through test-only clients; production cannot
override or disable the authorized HTTPS origin. Parser tests use local
fixtures; no test data are injected into production search results.
Explicit-proxy tests use loopback HTTP CONNECT and SOCKS5 remote-DNS fixtures
that reject tunnels before TLS. They check the fixed `rutracker.org:443` target
for anonymous, fresh-login and authenticated-download transports without
connecting to RuTracker or disabling certificate verification.

The adapter tests run with `cargo test --locked -p kadr-catalog`. For fork
unit tests without creating a second committed lockfile, copy this directory
to a temporary standalone directory, seed it with the root `Cargo.lock`, and
run `cargo test --manifest-path <temporary-directory>/Cargo.toml --offline`.
Compare resolved package versions with the root lock; do not replace the
application lock with the standalone lock.
