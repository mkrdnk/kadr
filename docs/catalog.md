# Trakt catalog and source discovery

Trakt is the only provider for new metadata. RuTracker is a separate adapter for
source discovery; neither requires a kadr server. Requests are user-initiated.
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

New provider IDs use `trakt:movie:<id>` and `trakt:show:<id>`. The old providers
are no longer contacted for search, details, posters or file discovery.
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
