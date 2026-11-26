use super::*;
use std::collections::BTreeSet;

const SEASON_LIMIT: usize = 100;
const EPISODES_PER_SEASON: usize = 1_000;
const EPISODE_LIMIT: usize = 20_000;

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
        year: data["year"]
            .as_u64()
            .filter(|year| (1800..2200).contains(year))
            .map(|year| year as u16),
        rating_tenths: rating,
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
            });
        }
    }
    episodes.sort_by_key(|episode| (episode.season, episode.number));
    Ok(episodes)
}
