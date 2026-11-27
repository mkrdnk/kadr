# Trakt catalog and source discovery

Trakt is the only provider for new metadata. The source resolver runs on the
desktop engine worker, not inside Trakt or a separate HTTP service. Requests are user-initiated.
There are no automatic startup queries or fallback demo records.

## Metadata

Use the public Trakt application **Client ID** (not a `client_secret`). A developer
run reads `KADR_TRAKT_CLIENT_ID` from its environment. A configured release sets
the approved read-only Production Client ID in that variable at build time, so
users do not need to register, sign in or set an environment variable. The client
sends the ID as `trakt-api-key` only to `https://api.trakt.tv`, without OAuth.
There is no ID in request URLs or diagnostics, and no OAuth secret in the app.
Missing configuration leaves manual playback and the library available.

Search and browse include movies and TV shows, not general video uploads.
The shelves use Trakt's trending endpoints, not rankings computed by kadr.
Metadata retains the original title when Trakt provides it.
Posters and wide artwork come from approved Trakt image hosts without API
headers. The desktop stores accepted raster image bytes in a bounded per-user
`catalog-images-v1` cache before rendering them; no direct browser/CDN hotlink is
used. This directory is separate from the torrent payload cache and its quota.
The displayed rating is Trakt's ten-point rating, not IMDb's; missing values
remain missing. Details load numbered episodes for shows, including specials
when the provider supplies season zero. Partial season data is not passed off
as a complete episode list.

New provider IDs use `trakt:movie:<id>` and `trakt:show:<id>`. Archive and
TVmaze are no longer contacted for search, details, posters or file discovery.
Existing local snapshots, progress and linked sources are retained; do not
match old and new records by similar titles. An explicitly reopened saved
HTTP source can still contact its original host.

```rust,ignore
use kadr_catalog::{CancellationToken, Client, Options};
let client = Client::new(Options::default())?;
let cancel = CancellationToken::new();
let shelves = client.browse(&cancel).await?;
let cards = client.search("Sherlock", &cancel).await?;
if let Some(card) = cards.first() {
    let media = client.details(&card.id, &cancel).await?;
}
```

Trakt receives explicit search terms or selected title IDs plus normal connection
metadata. Requests use fixed HTTPS origins, bounded JSON bodies and timeouts.
Cancellation drops in-flight metadata work; rate limits and auth failures are
reported rather than silently retried. Descriptions are bounded text, not HTML.

The settings credit Trakt: “kadr uses Trakt but is not endorsed or certified by
Trakt.” Review the [API use policy](https://developer.trakt.tv/docs/api-use-policy),
[Production access requirements](https://developer.trakt.tv/docs/create-an-app),
and [image cache requirements](https://developer.trakt.tv/docs/images) before
distribution. Success with a development ID does not imply Production approval.
Metadata and imagery do not confer playback or distribution rights.

## Validation

```sh
cargo test --locked -p kadr-catalog
```

The ordinary tests use fixtures and localhost HTTP, not a personal token.
A live Trakt check needs a configured application Client ID and network access:

```sh
cargo test --locked -p kadr-catalog live_catalog -- --ignored --nocapture
```

## On-demand source resolution

**Find sources** targets one Trakt movie or one numbered episode, never a
catalog shelf. Text, IDs and aliases come from metadata already in the card;
there is no background title crawl. Local library snapshots predating external
IDs can still match by title/year. A local input can always be opened directly,
including when no provider returned anything.

- `kadr-core::sources` validates locators and holds the query, normalized offer,
  match score and deterministic ranking. A known ID conflict is not resolved
  by a similar title. Torrent aliases use info hashes when known; HTTP uses
  normalized resource identity (the query remains significant). Aggregation
  preserves provider origins. Ranking never starts playback.
- `kadr-engine::resolver` runs enabled, compatible providers concurrently
  (up to four), with a deadline per provider. A timeout or login failure does
  not remove other providers' rows. Local providers project the selected or
  previously linked HTTP, magnet or `.torrent` input for this exact target.
  They never probe the network or open a file during discovery.
- The worker reuses fresh per-provider SQLite results; **Refresh** bypasses
  them. A force refresh or target change invalidates in-flight results before
  they can be written to the cache. [Storage](storage.md) describes limits.
- GPUI receives cumulative snapshots tagged with the target and cancellation
  stamp, plus stable, opaque selection handles. Re-ranking cannot silently
  redirect a click to a different release. On explicit selection the existing
  HTTP/torrent preparation and player session paths take over.
- **Watch** on a movie is an explicit request for playback. Without an explicitly
  selected source, the desktop waits for the final snapshot of that card. Only a
  matching torrent with a known positive seed count can be recommended; the
  highest count wins, with deterministic ties. The worker rechecks the winning
  handle before preparing it. **Sources** shows the same results without launching
  anything. HTTP results and torrents with zero or unknown seeds remain available
  for explicit selection. Episode playback retains its explicit source/file flow.

### Optional Torznab

The [Torznab specification](https://torznab.github.io/spec-1.3-draft/torznab/Specification-v1.3.html)
defines `caps`, movie/TV/free-text queries and RSS enclosures; a semantic API
error can have HTTP status 200. Endpoint settings use an explicit API URL
and, if needed, a provider-specific `KADR_TORZNAB_…` environment variable
**name**. Set its value in the environment of the running desktop client.
Use HTTPS (loopback HTTP is also accepted). Configure and enable each indexer
under **Settings → Source search**; there are no preconfigured endpoints or
mandatory companion services. The catalog and BitTorrent permissions must
both be enabled.

On a cache miss the adapter reads capabilities, then requests one bounded
page using supported search parameters. It rejects DTDs, oversized responses
and cross-origin enclosures. Discovery does not download metainfo. Selecting
an enclosure authorizes a same-origin fetch. When multiple configured API
instances share an origin Kadr sends an instance's key only to its own API
path; other download paths on that host must work anonymously. The engine
validates metainfo and checks the advertised hash, if supplied, before offering
video files. The cache contains sanitized descriptors, not the key or metainfo
bytes. If only a magnet is available, only its hash identity is retained.
Private releases may therefore need a same-origin `.torrent` enclosure.
An enclosure is not a durable library link: after restart, select it again
unless you saved a permanent copy.

### Built-in open films: Wikimedia Commons

This is a file provider, not an alternative Trakt catalog or a demo list of
video URLs. **Find sources** on a supported movie resolves its reviewed Commons
file through the public [MediaWiki Action API](https://www.mediawiki.org/wiki/API:Main_page).
Kadr queries current `videoinfo` metadata, including rights and progressive
renditions. If the work, file identity, rights, or playable rendition no
longer qualifies, it does not offer an old hardcoded download. No API key,
account, tracker, BitTorrent, or companion server is required. Unknown
titles, episodes, and catalog grids do not trigger Commons requests.

| Movie | Reviewed file and rights (unchanged complete film, credits retained) |
| --- | --- |
| *Big Buck Bunny* (2008), `tt1254207` | [Big Buck Bunny 4K.webm](https://commons.wikimedia.org/wiki/File:Big_Buck_Bunny_4K.webm), Blender Foundation, [CC BY 3.0](https://creativecommons.org/licenses/by/3.0/); see the [project's attribution instructions](https://peach.blender.org/about/). |
| *Elephants Dream* (2006), `tt0807840` | [Elephants Dream.ogv](https://commons.wikimedia.org/wiki/File:Elephants_Dream.ogv), Blender Foundation / Netherlands Media Art Institute, [CC BY-SA 2.5](https://creativecommons.org/licenses/by-sa/2.5/) for this Commons file. |

The result row includes the applicable credit, file page, and license link;
no film bytes ship with Kadr. [Commons reuse guidance](https://commons.wikimedia.org/wiki/Commons:Reusing_content_outside_Wikimedia)
allows linking/downloading subject to file-specific rights. Do not infer
permission from a Wikimedia domain, filename, or search hit alone. Kadr follows
[API etiquette](https://www.mediawiki.org/wiki/API:Etiquette): identifying
User-Agent, bounded read-only request, cache and no bulk downloads or automatic
rate-limit retries. The public service has no availability guarantee. The
catalog permission and the independent Commons toggle control discovery;
playback is a separate user action. The toggle is on for fresh installations
and off when upgrading an existing database; upgrading does not opt a user
into a new network service.

### Adding a built-in provider

1. Document the data source, rights, supported movie/episode identities and
   credential boundary. For works requiring credit, populate `MediaSource.license`
   with the attribution and license evidence presented to the user.
2. Implement the provider's protocol separately from playback. Map bounded,
   validated results to `MediaSource`; use an existing `Locator` kind. No
   player calls, torrent sessions, catalog-wide polling, or dynamic plugins.
3. Implement `SourceProvider` in `kadr-engine`: stable instance ID, display
   name, `supports`, offline `validate` and `accepts`, per-provider timeout,
   cancellable `search`. `accepts` reauthorizes both network and cached
   locators; do not grant permission to a local path from a network result.
4. Register it in `Worker::registry` behind explicit network/settings gates.
   The generic `SourceResolver` does not change. Only introduce a different
   playback handoff if none of the existing locator kinds has the right
   semantics.
5. Test valid/empty/error/cancelled replies, exact media scope, reauthorization
   and cache behavior using fixtures. Keep any live acceptance test opt-in.

### Other video APIs considered (not contacted)

| Documented API | Catalog and rights | Desktop suitability and limits |
| --- | --- | --- |
| [Internet Archive Search](https://archive.org/help/aboutsearch.htm) and [Metadata](https://archive.org/developers/md-read.html) | Feature films and archival video; no reliable Trakt episode mapping. [Rights vary by item](https://help.archive.org/help/movies-and-videos-a-basic-guide/); an uploader's label alone is insufficient. | Public reads need no key or backend. Established APIs, no availability guarantee; identify the client and respect [bot limits](https://archive.org/developers/bots.html) and 429. |
| [Library of Congress JSON API](https://www.loc.gov/apis/json-and-yaml/) | Historical film and newsreels, not a modern TV catalog. Check [rights and restrictions](https://loc.gov/legal/understanding-copyright) on individual resources. | Public, documented, no key. A desktop adapter could follow item resources; throttle, handle rate limiting and availability of the digitized file. |

These catalogs are technically accessible without a server, but neither the
metadata license nor a title match proves the right to distribute a particular
video. They are not implicit fallbacks. Trakt remains the metadata catalog.

## RuTracker release discovery

RuTracker is a separate source provider, not a metadata catalog or a rights
authority. When the catalog and BitTorrent are enabled, **Find sources** sends
the selected title to `rutracker.org`; result metadata is parsed on the device.
No periodic indexing or requests go through a kadr server. Titles can be
ambiguous and season packs may contain multiple episodes; verify the release
and the selected video file. Quality, size and seeds reflect available source
data, not independent verification. Use only content you have rights to.

The engine requests the selected topic's magnet, if present. If none is
available, it downloads and validates the actual `.torrent`, using a local
RuTracker account. It also offers sign-in if the tracker requires it for search.
Login goes only to RuTracker, not to Trakt or any kadr service.
Passwords are stored in the system vault, not in SQLite.
The login session and authenticated metainfo are in memory; reopening an entry
resolves the topic again and verifies its info hash before reusing the saved
file selection. The app does not attempt to bypass CAPTCHA.

### Catalog proxy

The optional **Network → Proxy** setting covers HTTP(S) requests to RuTracker
(search, sign-in, magnet lookup and `.torrent` download), Wikimedia Commons metadata, Torznab APIs, Trakt and allowed
poster hosts. Enter an `http://`, `https://`, `socks5://` (local DNS) or
`socks5h://` (proxy DNS) endpoint without credentials. Saving applies it to
new operations and discards the previous RuTracker session. A configured
proxy never falls back to a direct request; TLS verification and destination
restrictions still apply. Use a proxy you trust. An empty field restores the
default transports. BitTorrent, manual HTTP(S) media, loopback playback and
external players do not use this setting.
