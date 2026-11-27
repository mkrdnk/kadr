//! Numbered episodes from `/show/{imdb}`.
//!
//! The upstream anime client inherits this same show-detail flow. Anime
//! represented as Series needs no guessed ID or absolute-number conversion.
//! Pack offers retain the server's mapped filename as a hint, not a verified
//! torrent index: playback must verify it against actual torrent metadata.

use kadr_core::{
    catalog::MediaKind,
    sources::{Locator, MediaQuery, MediaSource, match_source},
    torrent::{
        EpisodeNumber, EpisodeRecognition, FileIndex, TorrentRelativePath, VideoCandidate,
        recognize_episode,
    },
};
use serde_json::Value;

use super::{BODY_LIMIT, Error, ID, RESULT_LIMIT, display_text, magnet, unsigned, upstream};
use crate::rutracker;

const EPISODE_LIMIT: usize = 10_000;

pub(super) fn parse(
    body: &[u8],
    query: &MediaQuery,
    fetched_at: i64,
) -> Result<Vec<MediaSource>, Error> {
    if body.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    if !supported(query) || fetched_at < 0 {
        return Err(Error::InvalidQuery);
    }
    let data: Value = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    let root = data.as_object().ok_or(Error::InvalidResponse)?;
    if root.get("imdb_id").and_then(Value::as_str) != query.external.imdb_id.as_deref()
        || root
            .get("_id")
            .is_some_and(|id| id.as_str() != query.external.imdb_id.as_deref())
    {
        return Err(Error::InvalidResponse);
    }
    let episodes = root
        .get("episodes")
        .and_then(Value::as_array)
        .ok_or(Error::InvalidResponse)?;
    // Bound the episode list separately: long-running anime can have many
    // more episodes than there are releases for any one of them.
    if episodes.len() > EPISODE_LIMIT {
        return Err(Error::TooLarge);
    }
    let target = query.episode.ok_or(Error::InvalidQuery)?;
    let mut matching = episodes.iter().filter(|episode| {
        episode.get("season").and_then(unsigned) == Some(u64::from(target.season()))
            && episode.get("episode").and_then(unsigned) == Some(u64::from(target.episode()))
    });
    let Some(episode) = matching.next() else {
        return Ok(Vec::new());
    };
    if matching.next().is_some() {
        return Err(Error::InvalidResponse);
    }
    let rows = match episode.get("torrents") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(rows)) if rows.is_empty() => return Ok(Vec::new()),
        Some(Value::Object(rows)) => rows,
        _ => return Err(Error::InvalidResponse),
    };
    if rows.len() > RESULT_LIMIT {
        return Err(Error::TooLarge);
    }
    let mut by_id = std::collections::BTreeMap::<String, Option<MediaSource>>::new();
    for source in rows
        .values()
        .filter_map(|row| source(row, query, fetched_at))
    {
        // The "0" quality is usually a duplicate. If two rows for the same
        // torrent disagree about the file, ranking must not pick one by chance.
        match by_id.entry(source.id.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(source));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry
                    .get()
                    .as_ref()
                    .is_some_and(|prev| prev.episode_file_hint != source.episode_file_hint)
                {
                    entry.insert(None);
                }
            }
        }
    }
    Ok(by_id.into_values().flatten().collect())
}

fn supported(query: &MediaQuery) -> bool {
    query.kind == MediaKind::Series && query.episode.is_some() && super::supports(query)
}

fn source(row: &Value, query: &MediaQuery, fetched_at: i64) -> Option<MediaSource> {
    let raw = row.get("title")?.as_str()?;
    let episode = query.episode?;
    if !display_text(raw, 2000) || non_video(raw) {
        return None;
    }
    for field in ["imdb", "imdb_id"] {
        if row
            .get(field)
            .is_some_and(|id| id.as_str() != query.external.imdb_id.as_deref())
        {
            return None;
        }
    }
    if row
        .get("kind")
        .is_some_and(|kind| kind.as_str() != Some("series"))
        || row
            .get("season")
            .is_some_and(|n| unsigned(n) != Some(u64::from(episode.season())))
        || row
            .get("episode")
            .is_some_and(|n| unsigned(n) != Some(u64::from(episode.episode())))
    {
        return None;
    }
    let file = match row.get("file") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_str()?),
    };
    if !scope_evidence(raw, file, episode)
        || (file.is_none()
            && row
                .get("pack")
                .is_some_and(|pack| !pack.is_null() && pack != false))
    {
        return None;
    }
    let title = std::iter::once(query.title.as_str())
        .chain(query.original_title.as_deref())
        .chain(
            query
                .external
                .alternative_titles
                .iter()
                .take(16)
                .map(String::as_str),
        )
        .filter(|title| display_text(title, 2000))
        .find(|title| title_matches(raw, title, query))?;
    let uri = magnet(row.get("url")?.as_str()?)?;
    let mut source = MediaSource::new(ID, Locator::Magnet(uri), title, MediaKind::Series).ok()?;
    source.release_title = Some(raw.into());
    // A series' catalog year is its premiere, not the selected season's year.
    // IMDb and explicit episode scope establish identity instead.
    source.year = query.year;
    source.episode = query.episode;
    source.episode_file_hint = file.map(str::to_owned);
    source.external.imdb_id = query.external.imdb_id.clone();
    source.quality = row
        .get("quality")
        .and_then(Value::as_str)
        .filter(|text| display_text(text, 100) && !text.eq_ignore_ascii_case("unknown"))
        .map(str::to_owned);
    source.resolution = source.quality.as_deref().and_then(|q| match q {
        "480p" => Some(480),
        "576p" => Some(576),
        "720p" => Some(720),
        "1080p" | "1080i" => Some(1080),
        "2160p" => Some(2160),
        _ => None,
    });
    source.seeds = row
        .get("seeds")
        .and_then(unsigned)
        .and_then(|n| u32::try_from(n).ok());
    source.peers = row
        .get("peers")
        .and_then(unsigned)
        .and_then(|n| u32::try_from(n).ok());
    source.size = row.get("size").and_then(unsigned);
    source.fetched_at = fetched_at;
    if let Some(origin) = row
        .get("provider")
        .and_then(Value::as_str)
        .and_then(upstream)
    {
        source.origins.push(origin);
    }
    let source = match_source(query, source)?;
    accepts(&source).then_some(source)
}

fn title_matches(raw: &str, title: &str, query: &MediaQuery) -> bool {
    let mut identity = query.clone();
    identity.title = title.into();
    identity.original_title = None;
    identity.external.alternative_titles.clear();
    identity.year = None;
    rutracker::classify_release(raw, &identity).is_some()
}

fn scope_evidence(raw: &str, file: Option<&str>, target: EpisodeNumber) -> bool {
    let Some(file) = file else {
        return single(raw, target);
    };
    if !display_text(file, 2000) {
        return false;
    }
    let Ok(path) = TorrentRelativePath::new(file) else {
        return false;
    };
    // Reuse playback's video allowlist; a subtitle or soundtrack is not an
    // episode even if its name happens to contain an exact marker.
    let Some(candidate) = VideoCandidate::from_file(FileIndex(0), path, 0) else {
        return false;
    };
    match candidate.episode() {
        EpisodeRecognition::Recognized(number) => *number == target,
        EpisodeRecognition::Ambiguous(_) => false,
        // Upstream anime packs often use absolute/bespoke basenames. Do not
        // invent a season conversion: the unique structured episode mapping is
        // the evidence, and the engine must confirm a unique actual file.
        EpisodeRecognition::Unrecognized => {
            labeled_scope_agrees(candidate.path().file_name(), target)
        }
    }
}

fn labeled_scope_agrees(raw: &str, target: EpisodeNumber) -> bool {
    let lower = raw.to_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric() && !matches!(c, '-' | '–'))
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().enumerate().all(|(index, word)| {
        let expected = match *word {
            "season" | "seasons" | "сезон" | "сезоны" => target.season(),
            "episode" | "episodes" | "серия" | "серии" => target.episode(),
            _ => {
                return word
                    .strip_prefix('s')
                    .and_then(|n| n.parse::<u16>().ok())
                    .is_none_or(|season| season == target.season());
            }
        };
        words.get(index + 1).and_then(|n| n.parse::<u16>().ok()) == Some(expected)
            && !words
                .get(index + 2)
                .is_some_and(|n| n.parse::<u16>().is_ok_and(|number| number < 1800))
    })
}

fn non_video(raw: &str) -> bool {
    raw.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "soundtrack"
                    | "саундтрек"
                    | "ost"
                    | "flac"
                    | "mp3"
                    | "trailer"
                    | "трейлер"
                    | "аудиодорожка"
                    | "аудиодорожки"
            )
        })
}

/// Accept a single compact marker or an explicit season/episode label pair.
/// Additional scope must agree; ranges, absolute anime numbering and pack words
/// are not evidence that a torrent contains only the requested episode.
fn single(raw: &str, target: EpisodeNumber) -> bool {
    let lower = raw.to_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric() && !matches!(c, '-' | '–'))
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| {
        matches!(
            *word,
            "pack"
                | "batch"
                | "complete"
                | "collection"
                | "сборник"
                | "коллекция"
                | "полный"
                | "полностью"
                | "soundtrack"
                | "саундтрек"
                | "ost"
                | "flac"
                | "mp3"
                | "trailer"
                | "трейлер"
                | "аудиодорожка"
                | "аудиодорожки"
        )
    }) {
        return false;
    }
    let compact = match recognize_episode(raw) {
        EpisodeRecognition::Recognized(number) if number == target => true,
        EpisodeRecognition::Unrecognized => false,
        _ => return false,
    };
    let mut season = false;
    let mut episode = false;
    for (index, word) in words.iter().enumerate() {
        let expected = match *word {
            "season" | "seasons" | "сезон" | "сезоны" => {
                season = true;
                target.season()
            }
            "episode" | "episodes" | "серия" | "серии" => {
                episode = true;
                target.episode()
            }
            _ => {
                // A bare S01 is a pack scope, not a single-file marker.
                if word
                    .strip_prefix('s')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    return false;
                }
                continue;
            }
        };
        if words.get(index + 1).and_then(|n| n.parse::<u16>().ok()) != Some(expected)
            || words
                .get(index + 2)
                .is_some_and(|n| n.parse::<u16>().is_ok_and(|number| number < 1800))
        {
            return false;
        }
    }
    compact || (season && episode)
}

pub(super) fn accepts(source: &MediaSource) -> bool {
    let Locator::Magnet(uri) = &source.locator else {
        return false;
    };
    let Some(raw) = source.release_title.as_deref() else {
        return false;
    };
    let query = MediaQuery {
        media_id: String::new(),
        kind: source.kind,
        title: source.title.clone(),
        original_title: None,
        year: source.year,
        external: source.external.clone(),
        episode: source.episode,
    };
    supported(&query)
        && source.provider == ID
        && source.license.is_none()
        && display_text(&source.title, 2000)
        && display_text(raw, 2000)
        && !non_video(raw)
        && query.episode.is_some_and(|episode| {
            scope_evidence(raw, source.episode_file_hint.as_deref(), episode)
        })
        && title_matches(raw, &source.title, &query)
        && magnet(uri).as_deref() == Some(uri.as_str())
        && source
            .quality
            .as_deref()
            .is_none_or(|quality| display_text(quality, 100))
        && source.origins.iter().all(|origin| {
            !origin.starts_with("popcorn:") || super::upstream_label(origin).is_some()
        })
        && source
            .clone()
            .normalize()
            .is_ok_and(|normalized| normalized == *source)
}

#[cfg(test)]
#[path = "episode/tests.rs"]
mod tests;
