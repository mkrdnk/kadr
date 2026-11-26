//! Parser for `/forum/viewtopic.php` pages.

use chrono::{TimeZone, Utc};
use scraper::Html;

use crate::error::{Error, Result};
use crate::models::{InfoHash, TopicId, TopicState};
use crate::parse_util::{element_text, selector};
use crate::topic::Topic;

pub(crate) fn parse(html: &str, id: TopicId, url: &str) -> Result<Topic> {
    let doc = Html::parse_document(html);
    if doc
        .select(selector!("form#login-form-full"))
        .next()
        .is_some()
    {
        return Err(Error::NotAuthenticated);
    }
    // A maintenance/error page is not a real topic without a magnet.
    // Recognize the topic layout before offering authenticated fallback.
    if doc
        .select(selector!("h1.maintitle, h1#topic-title"))
        .next()
        .is_none()
        || doc
            .select(selector!(".post_body, td.message, a.magnet-link"))
            .next()
            .is_none()
    {
        return Err(Error::parse("topic", "unrecognized response"));
    }

    let title = doc
        .select(selector!("h1.maintitle, h1#topic-title"))
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Fallback: take everything before the first "::" separator.
            // The rutracker <title> looks like `<title> :: RuTracker.org</title>`.
            doc.select(selector!("title"))
                .next()
                .map(element_text)
                .map(|t| t.split("::").next().unwrap_or(&t).trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default();

    let author = doc
        .select(selector!(".nick.nick-author, .topicAuthor a, p.nick a"))
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty());

    let category_path: Vec<String> = doc
        .select(selector!("table.navTitle a, td.nav a"))
        .map(element_text)
        .filter(|s| !s.is_empty())
        .collect();

    let info_hash = doc
        .select(selector!("#tor-hash, span.tor-hash, .post_body .tor-hash"))
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<InfoHash>().ok());

    let magnet = doc
        .select(selector!("a.magnet-link, a[href^=\"magnet:\"]"))
        .find_map(|a| a.value().attr("href").map(str::to_owned));

    // Topic pages render size as a human label (`"4.5 GB"`); the byte-precise
    // `data-ts_text` attribute appears on the search results page, not here.
    //
    // Authenticated layout exposes `#tor-size-humn` (and the byte-precise
    // value in its `title` attribute). Anonymous viewers get a stripped-down
    // layout where the size sits in an unmarked `<li>` next to the magnet
    // link. We try the precise selectors first, then the title-attribute
    // form, and finally fall back to scanning `<li>` text for the first
    // entry that parses as a size.
    let size = extract_size(&doc).unwrap_or(0);

    let registered = doc
        .select(selector!("[data-ts_text]"))
        .filter_map(|el| el.value().attr("data-ts_text"))
        .filter_map(|s| s.parse::<i64>().ok())
        .next()
        .and_then(|ts| Utc.timestamp_opt(ts, 0).single());

    let state = doc
        .select(selector!(".tor-icon, .t-status"))
        .next()
        .and_then(|el| el.value().attr("title"))
        .filter(|s| !s.is_empty())
        .map(TopicState::from_label);

    let description_html = doc
        .select(selector!(".post_body, td.message"))
        .next()
        .map(|el| el.inner_html())
        .unwrap_or_default();

    Ok(Topic {
        id,
        title,
        author,
        category_path,
        size,
        info_hash,
        magnet,
        registered,
        state,
        description_html,
        url: url.to_owned(),
    })
}

/// Walk the topic page looking for the torrent size, in order of decreasing
/// precision: byte-exact `title` attribute (authenticated layout), the
/// `#tor-size-humn` text, the legacy `span.tor-size` / attachment block, and
/// finally any `<li>` text that parses as a size (anonymous layout).
fn extract_size(doc: &Html) -> Option<u64> {
    // 1. Byte-exact value lives in `title="<bytes>"` on `#tor-size-humn`.
    if let Some(el) = doc.select(selector!("#tor-size-humn")).next() {
        if let Some(bytes) = el
            .value()
            .attr("title")
            .and_then(|s| s.trim().parse::<u64>().ok())
        {
            return Some(bytes);
        }
        let text = element_text(el);
        if !text.is_empty() {
            if let Some(parsed) = parse_size(&text) {
                return Some(parsed);
            }
        }
    }
    // 2. Legacy / variant selectors (some older skins).
    if let Some(parsed) = doc
        .select(selector!("span.tor-size, .attach_link .post-b"))
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty())
        .and_then(|s| parse_size(&s))
    {
        return Some(parsed);
    }
    // 3. Anonymous layout: the size lands in an unmarked `<li>` next to the
    //    magnet link. We don't know which one, so scan and pick the first
    //    entry whose text parses as a size.
    for li in doc.select(selector!("ul.inlined li, ul.middot-separated li")) {
        let text = element_text(li);
        if text.is_empty() {
            continue;
        }
        if let Some(parsed) = parse_size(&text) {
            return Some(parsed);
        }
    }
    None
}

/// Best-effort parse for sizes like `"1.23 GB"`, `"4.5 GiB"`, `"123 МБ"`,
/// `"1 234,56 MB"`.
///
/// Supported units (binary, base-1024): `B/Б`, `KB/КБ/KiB`, `MB/МБ/MiB`,
/// `GB/ГБ/GiB`, `TB/ТБ/TiB`. Decimal separator may be `.` or `,`. Group
/// separators are stripped (Russian thousands: `1 234,56` → `1234.56`).
///
/// Returns `None` when the string is empty, the value is non-finite/negative,
/// the unit is unrecognised, or the result overflows `u64`.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Split on the boundary between numeric chars and unit chars.
    let mut num = String::with_capacity(s.len());
    let mut unit = String::with_capacity(8);
    let mut seen_unit = false;
    for ch in s.chars() {
        if !seen_unit && (ch.is_ascii_digit() || ch == '.' || ch == ',') {
            // Treat both `.` and `,` as decimal points; drop ASCII
            // thousand separators (whitespace handled below).
            num.push(if ch == ',' { '.' } else { ch });
        } else if ch.is_whitespace() {
            // Whitespace inside the number (`"1 234,56 MB"`) is a thousand
            // separator while we haven't started reading the unit yet.
            continue;
        } else {
            seen_unit = true;
            unit.push(ch);
        }
    }
    // Multiple `.` (e.g. `"1.234.5"`) is invalid — keep only first decimal.
    if num.matches('.').count() > 1 {
        let mut parts = num.splitn(2, '.');
        let int_part = parts.next().unwrap_or("");
        let frac_part = parts.next().unwrap_or("").replace('.', "");
        num = format!("{int_part}.{frac_part}");
    }
    let value: f64 = num.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }

    let mult: u64 = match unit.to_ascii_uppercase().as_str() {
        "" | "B" | "Б" => 1,
        "KB" | "КБ" | "KIB" => 1_024,
        "MB" | "МБ" | "MIB" => 1_024 * 1_024,
        "GB" | "ГБ" | "GIB" => 1_024 * 1_024 * 1_024,
        "TB" | "ТБ" | "TIB" => 1_024u64 * 1_024 * 1_024 * 1_024,
        _ => return None,
    };

    let bytes = value * mult as f64;
    // u64::MAX is not exactly representable as f64, but `as u64` saturates
    // to u64::MAX for any value >= 2^64; we guard against unrealistic
    // inputs (e.g. `"1e30 GB"`) by checking against an explicit cap.
    if !bytes.is_finite() || bytes < 0.0 || bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn parses_decimal_dot() {
        assert_eq!(parse_size("1.5 GB"), Some(1_610_612_736));
    }

    #[test]
    fn parses_decimal_comma() {
        assert_eq!(parse_size("1,5 GB"), Some(1_610_612_736));
    }

    #[test]
    fn parses_russian_thousand_separator() {
        // "1 234,56 MB" — Russian-locale grouping
        assert_eq!(
            parse_size("1 234,56 MB"),
            Some((1234.56 * 1_048_576.0) as u64)
        );
    }

    #[test]
    fn parses_iec_units() {
        assert_eq!(parse_size("4.5 GiB"), Some(4_831_838_208));
        assert_eq!(parse_size("700 MiB"), Some(734_003_200));
    }

    #[test]
    fn parses_cyrillic_units() {
        assert_eq!(parse_size("123 МБ"), Some(128_974_848));
        assert_eq!(parse_size("4 ГБ"), Some(4_294_967_296));
    }

    #[test]
    fn parses_bytes_no_unit() {
        assert_eq!(parse_size("4096"), Some(4096));
    }

    #[test]
    fn rejects_unknown_unit() {
        assert_eq!(parse_size("1 PB"), None);
        assert_eq!(parse_size("garbage"), None);
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("   "), None);
    }

    #[test]
    fn rejects_negative() {
        assert_eq!(parse_size("-1 GB"), None);
    }

    #[test]
    fn rejects_overflow() {
        assert_eq!(parse_size("1e30 GB"), None);
    }
}
