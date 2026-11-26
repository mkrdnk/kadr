# Catalog and permitted source adapter

`kadr-catalog` is an account-free, API-key-free client for two real public APIs.
It does not ship a curated/demo catalog, recommendations, a crawler, or a private
backend. It runs no background network tasks. Callers explicitly initiate search,
details, and source discovery. `Options { enabled: false }` disables all three.

## Providers and coverage

* **Internet Archive**: dynamic title search through
  `https://archive.org/advancedsearch.php`; details and file metadata through
  `https://archive.org/metadata/{identifier}`. This covers films and other video
  items, not a comprehensive commercial movie database. Video items are shown as
  movies; the adapter does not invent series grouping from upload filenames.
* **TVmaze**: dynamic series search through
  `https://api.tvmaze.com/search/shows?q=...`; show details with embedded
  episodes through `/shows/{id}?embed=episodes`. Episodes expose season and episode
  numbers in airing order; unnumbered specials are omitted rather than assigned
  made-up numbers. There is no TVmaze playback adapter.

These services have different namespaces (`archive:identifier`, `tvmaze:335`).
IDs contain only storage-safe ASCII. Never match a film/episode to files by a
similar title alone. TVmaze source discovery returns an empty list: the app can
link a user's permitted manual source to the selected catalog ID and episode.
Original titles are optional and currently unavailable from these provider fields.
Missing dates, descriptions, images, or license claims remain missing, not invented.

## API and integration

```rust,ignore
use kadr_catalog::{CancellationToken, Client, Options};
let client = Client::new(Options::default())?;
let cancel = CancellationToken::new();
let cards = client.search("Sherlock", &cancel).await?;
let media = client.details(&cards[0].id, &cancel).await?;
let candidates = client.find_sources(&media, None, &cancel).await?;
```

Domain models are in `kadr_core::catalog`. Search returns up to 20 results per
provider. A failure in one provider preserves nonempty results from the other;
if neither provides usable results, the error is returned. `details` populates
series episodes; search cards do not. Group seasons by `Episode::season`.
Cancellation drops the in-flight HTTP operation. The client is cloneable and
reuses pooled connections. UI debounce/cancelling superseded searches should avoid
exceeding TVmaze's published rate limits. HTTP 429 returns an actionable
`RateLimited` error; the client deliberately does not silently retry user searches.

`SourceCandidate` is not automatic playback permission. It includes a URL,
display title, provider-reported format and language where available, normalized
license URL, and origin/creator attribution. After explicit user selection,
validate with `ManualSource::parse_link`, then use the normal session/playback
pipeline. Do not strip attribution or the license on display or export.

## Rights gate, provenance, and limits

Archive search metadata is **not** permission to play. Discovery fetches fresh
item metadata and requires an exact `licenseurl` on the item:

* CC0 1.0 or Public Domain Mark 1.0;
* CC BY or CC BY-SA, versions 1.0, 2.0, 2.5, 3.0, or 4.0.

Only HTTP(S) `creativecommons.org` URLs with exact paths and no credentials,
query, fragment or nonstandard port are accepted. HTTP license URLs normalize to
HTTPS. Missing, repeated/ambiguous, unsupported, NC/ND, or deceptive host claims
are rejected. Public-domain words in titles/descriptions are **never** enough.
Restricted/dark items and private files are rejected. Conflicting explicit
file-level licenses are rejected. Only MP4, M4V, WebM and Ogg video files are
offered (up to 40); arbitrary uploader links, torrents, archives, audio-only files,
and path traversal names are not followed.

**This gate filters uploader claims; it cannot verify ownership or jurisdiction.**
Internet Archive explicitly makes no guarantee about the accuracy of rights
information. Public-domain status varies by country; a restoration, soundtrack,
or embedded work can have separate rights. CC licenses require attribution and
can require ShareAlike on adaptations. Check the item page, creator and applicable
license before use. Availability and player codec support are not guaranteed.
No files are fetched until playback/explicit live-test probing; source discovery
only retrieves metadata.

Display `Media.attribution` together with a link to `Media.source_page`, and its
license when present. TVmaze data is CC BY-SA 4.0; credit TVmaze and preserve
ShareAlike obligations when redistributing adapted metadata. TVmaze permits
hotlinking its image CDN, but images can have underlying third-party rights.
Archive metadata and imagery likewise are not given a blanket rights guarantee.
Descriptions are bounded literal plain display text, never webview HTML.

## Network and privacy

On search, the user's query and ordinary connection information (IP address,
user-agent) go directly to **both** providers. Details and file discovery disclose
the selected provider ID. Images, if loaded by the UI, disclose requests to
`archive.org` or `static.tvmaze.com`. Actual Archive file playback may redirect to
Archive-operated download servers. No telemetry or private query/URL logging is
implemented here. Errors do not embed request URLs or response bodies;
`SourceCandidate` debug formatting redacts its file URL. Serialized candidates
do contain explicit URLs and must not be logged indiscriminately.

Production metadata endpoint origins are fixed; no user endpoint overrides,
credentials, arbitrary image hosts, or metadata redirects. Connect timeout is
5 seconds; whole request/body deadline is 20 seconds; each JSON body is capped at
4 MiB, even without Content-Length. Text fields and result counts are bounded.

## Provider documentation and terms consulted

* [Archive metadata read API](https://archive.org/developers/md-read.html)
* [Archive advanced search](https://archive.org/advancedsearch.php)
* [Archive metadata schema](https://archive.org/developers/metadata-schema/index.html)
* [Archive rights guidance](https://help.archive.org/help/rights/) explicitly says
  to ensure use is non-infringing; uploader rights claims are not guaranteed.
* [Archive Terms of Use](https://archive.org/about/terms.php) (the current page
  requires JavaScript; the rights guidance quotes the relevant risk limitation).
* [TVmaze API, rate limits, images and CC BY-SA terms](https://www.tvmaze.com/api)
* [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/)

## Validation

Deterministic fixture and localhost transport tests:

```sh
cargo test -p kadr-catalog
```

Explicit live smoke test (network required, no account or secrets):

```sh
cargo test -p kadr-catalog live_catalog -- --ignored --nocapture
```

The opt-in smoke test uses real film and series searches, loads real show
episodes, fetches fresh explicit-license Archive file metadata, and probes the
first bytes of a real video with a Range request. Its one stable Archive test
target is test data only, not a static application catalog. It allows redirects
only to HTTPS Archive hosts. Outages, rate limits, rights changes or removed files
can fail this opt-in check without making deterministic tests flaky.
