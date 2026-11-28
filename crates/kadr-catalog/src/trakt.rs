use super::*;
use kadr_core::catalog::{ExternalMetadata, ExternalRating, RatingSource};
use std::collections::BTreeSet;

const SEASON_LIMIT: usize = 100;
const EPISODES_PER_SEASON: usize = 1_000;
const EPISODE_LIMIT: usize = 20_000;
const ALTERNATIVE_TITLE_SCAN_LIMIT: usize = 100;
const ALTERNATIVE_TITLE_LIMIT: usize = 20;

pub(super) fn kind_path(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movies",
        MediaKind::Series => "shows",
    }
}

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "movie",
        MediaKind::Series => "show",
    }
}

pub(super) fn provider_id(id: &str) -> Result<(MediaKind, u64), Error> {
    let mut parts = id.split(':');
    if parts.next() != Some("trakt") {
        return Err(Error::InvalidId);
    }
    let kind = match parts.next() {
        Some("movie") => MediaKind::Movie,
        Some("show") => MediaKind::Series,
        _ => return Err(Error::InvalidId),
    };
    let number = parts.next().ok_or(Error::InvalidId)?;
    if number.is_empty()
        || number.len() > 20
        || number.starts_with('0')
        || !number.bytes().all(|b| b.is_ascii_digit())
        || parts.next().is_some()
    {
        return Err(Error::InvalidId);
    }
    Ok((kind, number.parse().map_err(|_| Error::InvalidId)?))
}

pub(super) fn api_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("api.trakt.tv")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

pub(super) fn endpoint(segments: &[&str]) -> Result<Url, Error> {
    let mut url = Url::parse(TRAKT).map_err(|_| Error::InvalidResponse)?;
    url.path_segments_mut()
        .map_err(|_| Error::InvalidResponse)?
        .extend(segments);
    url.query_pairs_mut().append_pair("extended", "full");
    Ok(url)
}

pub(super) fn list_url(
    kind: MediaKind,
    operation: &str,
    query: Option<&str>,
) -> Result<Url, Error> {
    let mut url = match operation {
        "search" => endpoint(&["search", kind_name(kind)])?,
        "trending" => endpoint(&[kind_path(kind), "trending"])?,
        _ => return Err(Error::InvalidResponse),
    };
    url.query_pairs_mut()
        .append_pair("limit", &SEARCH_LIMIT.to_string());
    if let Some(query) = query {
        url.query_pairs_mut().append_pair("query", query);
    }
    Ok(url)
}

pub(super) fn list(data: &Value, kind: MediaKind) -> Result<Vec<Media>, Error> {
    let mut seen = BTreeSet::new();
    Ok(data
        .as_array()
        .ok_or(Error::InvalidResponse)?
        .iter()
        .take(SEARCH_LIMIT)
        .filter(|row| {
            row.get("type")
                .is_none_or(|value| value.as_str() == Some(kind_name(kind)))
        })
        .filter_map(|row| media(&row[kind_name(kind)], kind))
        .filter(|media| seen.insert(media.id.clone()))
        .collect())
}

/// Provider prose is plain display text, never remote markup.
fn plain(value: &Value, limit: usize) -> String {
    let Some(raw) = value.as_str() else {
        return String::new();
    };
    let mut inside_tag = false;
    let mut output = String::new();
    for c in raw.chars().take(64 * 1024) {
        match c {
            '<' => inside_tag = true,
            '>' if inside_tag => {
                inside_tag = false;
                output.push(' ');
            }
            _ if !inside_tag && (!c.is_control() || c.is_whitespace()) => output.push(c),
            _ => {}
        }
    }
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

pub(super) fn matching_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// Titles are literal text, including '<3' or 'A < B', not HTML fragments.
fn plain_title(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(64 * 1024)
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

/// Read only the existing media payload: `ids.imdb`, `ids.tmdb`, and an
/// `alternative_titles` array of strings or objects with a string `title`.
/// Do not guess at aliases/translations layouts or fetch another endpoint.
/// Inspect at most 100 entries, keep at most 20 nonempty 500-character titles,
/// and deduplicate their sanitized forms by whitespace and lowercase.
fn external_metadata(data: &Value) -> ExternalMetadata {
    let imdb_id = data["ids"]["imdb"]
        .as_str()
        .filter(|id| {
            (9..=12).contains(&id.len())
                && id.starts_with("tt")
                && id.as_bytes()[2..].iter().all(u8::is_ascii_digit)
        })
        .map(str::to_owned);
    let tmdb_id = data["ids"]["tmdb"].as_u64().filter(|id| *id > 0);
    let mut seen = BTreeSet::new();
    let alternative_titles = data["alternative_titles"]
        .as_array()
        .into_iter()
        .flatten()
        .take(ALTERNATIVE_TITLE_SCAN_LIMIT)
        .map(|entry| {
            plain_title(if entry.is_string() {
                entry
            } else {
                &entry["title"]
            })
        })
        .filter(|title| !title.is_empty() && seen.insert(matching_title(title)))
        .take(ALTERNATIVE_TITLE_LIMIT)
        .collect();
    ExternalMetadata {
        imdb_id,
        tmdb_id,
        alternative_titles,
    }
}

/// Canonical public image allowlist, also used by image downloaders and storage.
/// Images must not carry credentials, tracking queries, fragments or arbitrary
/// hosts. URL parsing normalizes HTTPS's explicit default port to no port.
/// Accept current `media.trakt.tv/{movies|shows}/{id}/{size}/{poster|fanart}`
/// raster paths and the documented `/images/.../{posters|fanarts}/...` layout
/// on `media.trakt.tv` and `walter-r2.trakt.tv`. Downloaders must cache images,
/// as required by <https://developer.trakt.tv/docs/images>.
pub fn is_public_catalog_image(url: &Url) -> bool {
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("media.trakt.tv" | "walter-r2.trakt.tv")
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    safe_image_path(url.host_str().unwrap_or_default(), url.path())
}

fn safe_image_path(host: &str, path: &str) -> bool {
    if path.len() > 512
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        || ![".jpg", ".jpeg", ".png", ".webp"]
            .iter()
            .any(|ext| path.ends_with(ext))
    {
        return false;
    }
    let segments: Vec<_> = path.split('/').skip(1).collect();
    if segments
        .iter()
        .any(|part| part.is_empty() || part.starts_with('.') || part.contains(".."))
    {
        return false;
    }
    match (host, segments.as_slice()) {
        ("media.trakt.tv", [kind, id, size, file]) => {
            matches!(*kind, "movies" | "shows")
                && positive_decimal(id)
                && image_size(size)
                && matches!(
                    *file,
                    "poster.jpg"
                        | "poster.jpeg"
                        | "poster.png"
                        | "poster.webp"
                        | "fanart.jpg"
                        | "fanart.jpeg"
                        | "fanart.png"
                        | "fanart.webp"
                )
        }
        (
            "media.trakt.tv" | "walter-r2.trakt.tv",
            ["images", kind, first, second, third, category, size, _],
        ) => {
            matches!(*kind, "movies" | "shows")
                && [first, second, third]
                    .iter()
                    .all(|part| part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
                && matches!(*category, "posters" | "fanarts")
                && image_size(size)
        }
        _ => false,
    }
}

fn image_size(value: &str) -> bool {
    matches!(value, "thumb" | "medium" | "original")
}

fn positive_decimal(value: &str) -> bool {
    !value.starts_with('0') && value.parse::<u64>().is_ok_and(|id| id > 0)
}

fn image(value: &Value) -> Option<String> {
    value.as_array()?.iter().take(20).find_map(|value| {
        let raw = value.as_str()?;
        // Validate the raw input before URL parsing can normalize traversal.
        let (host, path) = raw.split_once('/')?;
        if !matches!(host, "media.trakt.tv" | "walter-r2.trakt.tv")
            || !safe_image_path(host, &format!("/{path}"))
        {
            return None;
        }
        let url = Url::parse(&format!("https://{raw}")).ok()?;
        is_public_catalog_image(&url).then(|| url.into())
    })
}

fn runtime(value: &Value) -> Option<u16> {
    value
        .as_u64()
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn genres(value: &Value) -> Vec<String> {
    let mut seen = BTreeSet::new();
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(100)
        .filter_map(Value::as_str)
        .filter(|slug| catalog::valid_slug(slug) && seen.insert(*slug))
        .take(30)
        .map(str::to_owned)
        .collect()
}

/// Only the documented `ratings?extended=all` provider blocks are interpreted.
/// See Trakt's official `movieRatingsResponseSchema.ts`,
/// `showRatingsResponseSchema.ts`, and <https://developer.trakt.tv/docs/filters>
/// for the distinct scales. Links, distributions and unknown sources are ignored.
pub(super) fn external_ratings(data: &Value, kind: MediaKind) -> Vec<ExternalRating> {
    let mut ratings = Vec::new();
    for (key, field, source, maximum) in [
        ("imdb", "rating", RatingSource::Imdb, 10),
        ("tmdb", "rating", RatingSource::Tmdb, 10),
        ("metascore", "rating", RatingSource::Metascore, 100),
        (
            "rotten_tomatoes",
            "rating",
            RatingSource::RottenTomatoes,
            100,
        ),
        (
            "rotten_tomatoes",
            "user_rating",
            RatingSource::RottenTomatoesAudience,
            100,
        ),
        ("letterboxd", "rating", RatingSource::Letterboxd, 5),
        ("mal", "rating", RatingSource::MyAnimeList, 10),
    ] {
        if source == RatingSource::Letterboxd && kind != MediaKind::Movie {
            continue;
        }
        let block = &data[key];
        let Some(rating) = block[field]
            .as_f64()
            .filter(|rating| rating.is_finite() && (0.0..=f64::from(maximum)).contains(rating))
        else {
            continue;
        };
        let votes = matches!(
            source,
            RatingSource::Imdb
                | RatingSource::Tmdb
                | RatingSource::Letterboxd
                | RatingSource::MyAnimeList
        )
        .then(|| block["votes"].as_u64())
        .flatten();
        if votes == Some(0) {
            continue;
        }
        ratings.push(ExternalRating {
            source,
            rating_tenths: (rating * 10.0).round() as u16,
            max_tenths: maximum * 10,
            votes,
        });
    }
    ratings
}

pub(super) fn media(data: &Value, kind: MediaKind) -> Option<Media> {
    let id = data["ids"]["trakt"].as_u64().filter(|id| *id > 0)?;
    let title = plain_title(&data["title"]);
    if title.is_empty() {
        return None;
    }
    let original = plain_title(&data["original_title"]);
    let rating = data["votes"]
        .as_u64()
        .filter(|count| *count > 0)
        .and_then(|_| data["rating"].as_f64())
        .filter(|rating| rating.is_finite() && (0.0..=10.0).contains(rating))
        .map(|rating| (rating * 10.0).round() as u8);
    Some(Media {
        id: format!("trakt:{}:{id}", kind_name(kind)),
        title,
        // Preserve the provider's original title when present, never infer it
        // from the primary title or issue per-card translation requests.
        original_title: (!original.is_empty()).then_some(original),
        external: external_metadata(data),
        year: data["year"]
            .as_u64()
            .filter(|year| (1800..2200).contains(year))
            .map(|year| year as u16),
        rating_tenths: rating,
        runtime_minutes: runtime(&data["runtime"]),
        genres: genres(&data["genres"]),
        certification: data["certification"]
            .as_str()
            .filter(|value| {
                !value.trim().is_empty()
                    && value.chars().count() <= 32
                    && !value.chars().any(char::is_control)
            })
            .map(|value| plain(&Value::String(value.to_owned()), 32))
            .filter(|value| !value.is_empty()),
        external_ratings: Vec::new(),
        description: plain(&data["overview"], 16_000),
        poster_url: image(&data["images"]["poster"]),
        backdrop_url: image(&data["images"]["fanart"]),
        kind,
        episodes: Vec::new(),
        attribution: "Trakt · Metadata provided by Trakt.".into(),
        license: None,
        source_page: format!("https://trakt.tv/{}/{id}", kind_path(kind)),
    })
}

pub(super) fn episodes(data: &Value) -> Result<Vec<Episode>, Error> {
    let seasons = data.as_array().ok_or(Error::InvalidResponse)?;
    if seasons.len() > SEASON_LIMIT {
        return Err(Error::TooLarge);
    }
    let mut seen_seasons = BTreeSet::new();
    let mut episodes = Vec::new();
    for row in seasons {
        let season = row["number"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(Error::InvalidResponse)?;
        if !seen_seasons.insert(season) {
            return Err(Error::InvalidResponse);
        }
        let rows = row["episodes"].as_array().ok_or(Error::InvalidResponse)?;
        if rows.len() > EPISODES_PER_SEASON || episodes.len() + rows.len() > EPISODE_LIMIT {
            return Err(Error::TooLarge);
        }
        let mut seen = BTreeSet::new();
        for row in rows {
            let number = row["number"]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or(Error::InvalidResponse)?;
            if row["season"].as_u64() != Some(u64::from(season)) || !seen.insert(number) {
                return Err(Error::InvalidResponse);
            }
            episodes.push(Episode {
                season,
                number,
                title: plain_title(&row["title"]),
                description: plain(&row["overview"], 8_000),
                runtime_minutes: runtime(&row["runtime"]),
                // The official image guide documents screenshot arrays but no
                // episode URL path. Do not widen the canonical image allowlist
                // or invent URLs; retain only already-approved public layouts.
                screenshot_url: image(&row["images"]["screenshot"]),
            });
        }
    }
    episodes.sort_by_key(|episode| (episode.season, episode.number));
    Ok(episodes)
}
