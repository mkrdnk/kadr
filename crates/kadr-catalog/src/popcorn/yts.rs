//! Decode the documented YTS v2 movie-details response. Torrent and page URLs
//! are untrusted metadata, never destinations or substitutes for an info hash.

use super::{
    BODY_LIMIT, Error, Index, Locator, MediaKind, MediaQuery, MediaSource, RESULT_LIMIT, YTS_ID,
    display_text, match_source, non_movie, rutracker, unsigned,
};
use serde_json::Value;

pub(super) fn parse(
    body: &[u8],
    query: &MediaQuery,
    fetched_at: i64,
) -> Result<Vec<MediaSource>, Error> {
    if body.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    if fetched_at < 0 {
        return Err(Error::InvalidQuery);
    }
    if !Index::Yts.supports(query) {
        return Ok(Vec::new());
    }
    let data: Value = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    if data.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(Error::InvalidResponse);
    }
    let movie = data
        .get("data")
        .and_then(|data| data.get("movie"))
        .filter(|movie| movie.is_object())
        .ok_or(Error::InvalidResponse)?;
    // The canonical service uses {"imdb_code":"tt"} for a missing movie.
    if movie.get("imdb_code").and_then(Value::as_str) == Some("tt")
        && movie.get("torrents").is_none()
    {
        return Ok(Vec::new());
    }
    let torrents = movie
        .get("torrents")
        .and_then(Value::as_array)
        .ok_or(Error::InvalidResponse)?;
    if torrents.len() > RESULT_LIMIT {
        return Err(Error::TooLarge);
    }
    if movie.get("imdb_code").and_then(Value::as_str) != query.external.imdb_id.as_deref()
        || ["kind", "type"].iter().any(|field| {
            movie
                .get(field)
                .is_some_and(|kind| kind.as_str() != Some("movie"))
        })
        || ["season", "episode", "pack"].iter().any(|field| {
            movie
                .get(field)
                .is_some_and(|value| !value.is_null() && value != false)
        })
    {
        return Ok(Vec::new());
    }
    let Some(title) = movie.get("title").and_then(Value::as_str) else {
        return Ok(Vec::new());
    };
    let Some(year) = movie
        .get("year")
        .and_then(Value::as_u64)
        .and_then(|year| u16::try_from(year).ok())
        .filter(|year| (1800..2200).contains(year))
    else {
        return Ok(Vec::new());
    };
    if !single_movie(title, year) || query.year.is_some_and(|requested| requested != year) {
        return Ok(Vec::new());
    }
    Ok(torrents
        .iter()
        .filter_map(|torrent| source(torrent, title, year, query, fetched_at))
        .collect())
}

fn single_movie(title: &str, year: u16) -> bool {
    let raw = format!("{title} ({year})");
    let identity = MediaQuery {
        media_id: String::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        original_title: None,
        year: Some(year),
        external: Default::default(),
        episode: None,
    };
    display_text(title, 1900)
        && !non_movie(&raw)
        && rutracker::classify_release(&raw, &identity) == Some(rutracker::ReleaseMatch::Exact)
}

fn hash_magnet(hash: &str) -> Option<String> {
    (hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("magnet:?xt=urn:btih:{}", hash.to_ascii_lowercase()))
}

fn source(
    torrent: &Value,
    title: &str,
    year: u16,
    query: &MediaQuery,
    fetched_at: i64,
) -> Option<MediaSource> {
    let uri = hash_magnet(torrent.get("hash")?.as_str()?)?;
    let mut source =
        MediaSource::new(YTS_ID, Locator::Magnet(uri), title, MediaKind::Movie).ok()?;
    source.release_title = Some(format!("{title} ({year})"));
    source.year = Some(year);
    // Check title evidence independently: core matching normally short-circuits
    // on an IMDb match, which must not excuse a contradictory API title.
    let mut identity = query.clone();
    identity.external.imdb_id = None;
    identity.external.tmdb_id = None;
    identity.year = Some(year);
    let mut source = match_source(&identity, source)?;
    source.external.imdb_id = query.external.imdb_id.clone();
    source.quality = torrent
        .get("quality")
        .and_then(Value::as_str)
        .filter(|quality| display_text(quality, 100) && !quality.eq_ignore_ascii_case("unknown"))
        .map(str::to_owned);
    source.resolution = source.quality.as_deref().and_then(|quality| match quality {
        "480p" => Some(480),
        "576p" => Some(576),
        "720p" => Some(720),
        "1080p" => Some(1080),
        "2160p" => Some(2160),
        _ => None,
    });
    source.seeds = torrent
        .get("seeds")
        .and_then(unsigned)
        .and_then(|number| u32::try_from(number).ok());
    source.peers = torrent
        .get("peers")
        .and_then(unsigned)
        .and_then(|number| u32::try_from(number).ok());
    source.size = torrent.get("size_bytes").and_then(unsigned);
    source.fetched_at = fetched_at;
    let source = match_source(query, source)?;
    accepts(&source).then_some(source)
}

/// Recheck the distinct YTS capability after ranking/restoration. No arbitrary
/// URL, tracker, web seed, display name, or alternate exact source is permitted.
pub(super) fn accepts(source: &MediaSource) -> bool {
    let Locator::Magnet(uri) = &source.locator else {
        return false;
    };
    let Some(year) = source.year else {
        return false;
    };
    let Some(hash) = uri.strip_prefix("magnet:?xt=urn:btih:") else {
        return false;
    };
    source.provider == YTS_ID
        && source.kind == MediaKind::Movie
        && source.episode.is_none()
        && source.license.is_none()
        && !source.possible_match
        && source.external.clone().normalized().imdb_id.is_some()
        && hash_magnet(hash).as_deref() == Some(uri)
        && single_movie(&source.title, year)
        && source.release_title.as_deref() == Some(format!("{} ({year})", source.title).as_str())
        && source
            .quality
            .as_deref()
            .is_none_or(|quality| display_text(quality, 100))
        && source
            .clone()
            .normalize()
            .is_ok_and(|normalized| normalized == *source)
}

#[cfg(test)]
#[path = "yts/tests.rs"]
mod tests;
