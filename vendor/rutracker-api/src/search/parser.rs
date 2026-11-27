//! HTML parser for `/forum/tracker.php` results.
//!
//! Targets the `#tor-tbl` table layout used by rutracker.org as of 2024.
//! Schema reverse-engineered from real responses; tolerant of missing/empty
//! cells (unknown size/seed values remain absent).

use chrono::{DateTime, TimeZone, Utc};
use scraper::{ElementRef, Html};
use url::Url;

use crate::error::{Error, Result};
use crate::models::{TopicId, TopicState};
use crate::parse_util::{element_text, has_challenge, has_login_form, selector};
use crate::search::{PAGE_SIZE, SearchResults, TorrentSummary};

pub(crate) fn parse(html: &str, page: u32, base_url: &Url) -> Result<SearchResults> {
    let doc = Html::parse_document(html);
    if has_challenge(&doc) {
        return Err(Error::parse("search", "unsupported challenge"));
    }

    // Anonymous search can return the credential form instead of results.
    if has_login_form(&doc) {
        return Err(Error::NotAuthenticated);
    }

    let total_count = parse_total_count(&doc);
    let table = doc.select(selector!("table#tor-tbl")).next();
    let mut results = Vec::new();
    let mut malformed_rows = false;
    let mut empty_marker = false;
    if let Some(table) = table {
        for (i, row) in table.select(selector!("tbody tr")).enumerate() {
            match parse_row(row, base_url, i) {
                Ok(Some(t)) => results.push(t),
                Ok(None) => {
                    empty_marker |= row.select(selector!("td")).any(|cell| {
                        matches!(
                            element_text(cell).to_lowercase().trim_end_matches('.'),
                            "ничего не найдено" | "по вашему запросу ничего не найдено"
                        )
                    });
                }
                Err(_) => malformed_rows = true,
            }
        }
    }

    // Keep usable partial results, but never turn an unreadable response into a
    // successful empty search (which callers may cache). An empty table alone
    // is not evidence that the query matched nothing.
    if results.is_empty()
        && (malformed_rows
            || total_count.is_some_and(|count| count > 0)
            || !(empty_marker || (table.is_some() && total_count == Some(0))))
    {
        return Err(Error::parse(
            "search",
            "no usable results or reliable empty marker",
        ));
    }

    let total_count = total_count.unwrap_or(0);
    let total_pages = total_count.div_ceil(PAGE_SIZE);
    Ok(SearchResults {
        total_count,
        page,
        total_pages,
        results,
    })
}

fn parse_total_count(doc: &Html) -> Option<u32> {
    // The "Результатов поиска: 1234" string (literal HTML marker on
    // rutracker's results page) — extract the trailing integer.
    doc.select(selector!("p.med.bold")).find_map(|counter| {
        let text = element_text(counter);
        let count = text.strip_prefix("Результатов поиска:")?.trim();
        count.replace([',', ' ', '\u{a0}'], "").parse().ok()
    })
}

fn parse_row(
    row: ElementRef<'_>,
    base_url: &Url,
    row_index: usize,
) -> Result<Option<TorrentSummary>> {
    let Some(title_link) = row
        .select(selector!("a.tLink, a.med.tLink, a[data-topic_id]"))
        .next()
    else {
        // A recognized result row with a missing title is malformed, not a
        // placeholder. Header and genuine no-results rows may be skipped.
        if row.value().classes().any(|class| class == "hl-tr")
            || row.value().id().is_some_and(|id| id.starts_with("tr-"))
            || row
                .select(selector!("td.t-title, td.t-title-col, td.tor-size"))
                .next()
                .is_some()
        {
            return Err(Error::parse(
                "search/parser::title",
                "result row without title link",
            ));
        }
        return Ok(None);
    };
    let id = extract_topic_id(title_link, row_index)?;
    let title = element_text(title_link);
    if id.get() == 0 || title.is_empty() {
        return Err(Error::parse(
            "search/parser::title",
            "result row without usable id or title",
        ));
    }

    Ok(Some(TorrentSummary {
        id,
        title,
        author: extract_author(row),
        category: extract_category(row),
        size: extract_size(row),
        seeds: extract_seeds(row),
        leeches: extract_leeches(row),
        downloads: extract_downloads(row),
        registered: extract_registered(row),
        state: extract_state(row),
        url: build_topic_url(base_url, id)?,
    }))
}

fn extract_topic_id(title_link: ElementRef<'_>, row_index: usize) -> Result<TopicId> {
    let id_str = title_link.value().attr("data-topic_id").ok_or_else(|| {
        Error::parse(
            "search/parser::topic_id",
            format!("title link without data-topic_id at row {row_index}"),
        )
    })?;
    id_str.parse()
}

fn extract_state(row: ElementRef<'_>) -> Option<TopicState> {
    row.select(selector!("td.t-ico, td.tor-icon"))
        .next()
        .and_then(|td| td.value().attr("title"))
        .filter(|s| !s.is_empty())
        .map(TopicState::from_label)
}

fn extract_category(row: ElementRef<'_>) -> String {
    row.select(selector!(".f-name a, td.f-name-col a"))
        .next()
        .map(element_text)
        .unwrap_or_default()
}

fn extract_author(row: ElementRef<'_>) -> Option<String> {
    row.select(selector!(".u-name a, td.u-name a, .vf-col-author a"))
        .next()
        .map(element_text)
        .filter(|s| !s.is_empty())
}

fn extract_size(row: ElementRef<'_>) -> Option<u64> {
    row.select(selector!("td.tor-size"))
        .find_map(|td| td.value().attr("data-ts_text").map(str::to_owned))
        .and_then(|s| s.parse::<u64>().ok())
}

fn extract_seeds(row: ElementRef<'_>) -> Option<i64> {
    row.select(selector!("td.seedmed b, b.seedmed"))
        .next()
        .map(element_text)
        .and_then(|s| s.parse::<i64>().ok())
}

fn extract_leeches(row: ElementRef<'_>) -> u32 {
    row.select(selector!("td.leechmed b, b.leechmed"))
        .next()
        .map(element_text)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}

fn extract_downloads(row: ElementRef<'_>) -> u32 {
    // Downloads is the third sibling after size, or carries class `dl-stub`.
    row.select(selector!("td.dl-stub, td.tor-size + td + td + td"))
        .next()
        .map(element_text)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
}

fn extract_registered(row: ElementRef<'_>) -> DateTime<Utc> {
    row.select(selector!("td.row4.small.nowrap.tCenter, td[data-ts_text]"))
        .filter_map(|td| td.value().attr("data-ts_text"))
        .filter_map(|s| s.parse::<i64>().ok())
        // Rows may carry several `data-ts_text` attributes (size, registered);
        // the registered timestamp is the last one in document order.
        .last()
        .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn build_topic_url(base_url: &Url, id: TopicId) -> Result<String> {
    let mut viewtopic = base_url.join("/forum/viewtopic.php").map_err(|e| {
        Error::parse(
            "search/parser::topic_url",
            format!("failed to build topic URL: {e}"),
        )
    })?;
    viewtopic
        .query_pairs_mut()
        .append_pair("t", &id.to_string());
    Ok(viewtopic.into())
}
