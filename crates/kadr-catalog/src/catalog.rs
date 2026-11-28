//! Explicit, server-ranked catalog pages, separate from the unchanged Home shelves.
//!
//! Trakt documents filters at <https://developer.trakt.tv/docs/filters> and
//! pagination at <https://developer.trakt.tv/docs/pagination>.
//! Popular returns bare media objects; trending wraps them in `movie` / `show`.
//! Only `X-Pagination-*` headers establish pagination, never the number of cards
//! left after validation. A missing page-count header disables forward navigation.
//! Popular/trending do not document placeholder counts. Still stop on an empty
//! raw array, never on a short page; report the effective (possibly clamped) limit.

use super::*;
use reqwest::header::HeaderMap;
use std::collections::BTreeSet;

const MAX_PAGE: u32 = 1_000;
const MAX_LIMIT: u16 = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CatalogSort {
    #[default]
    Popular,
    Trending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct YearRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogQuery {
    pub kind: MediaKind,
    pub sort: CatalogSort,
    /// Trakt genre slugs, not translated labels. At most 10.
    pub genres: Vec<String>,
    /// Inclusive server-side range, or `None` for all years.
    pub years: Option<YearRange>,
    /// One-based, at most 1,000. Requests never crawl subsequent pages.
    pub page: u32,
    /// 1–100 raw provider rows, before invalid cards and duplicates are removed.
    pub limit: u16,
}

impl Default for CatalogQuery {
    fn default() -> Self {
        Self {
            kind: MediaKind::Movie,
            sort: CatalogSort::Popular,
            genres: Vec::new(),
            years: None,
            page: 1,
            limit: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPage {
    pub items: Vec<Media>,
    pub page: u32,
    pub limit: u16,
    pub page_count: Option<u32>,
    pub total_items: Option<u32>,
    /// Absent for the last page, missing pagination headers, or the safety cap.
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Genre {
    pub name: String,
    pub slug: String,
}

impl Client {
    /// One real Trakt popular/trending request. Filtering and ranking are remote;
    /// this never sorts a Home shelf or silently replaces failed data.
    pub async fn catalog(
        &self,
        query: &CatalogQuery,
        cancel: &CancellationToken,
    ) -> Result<CatalogPage, Error> {
        self.ready(cancel)?;
        let response = self.json_response(catalog_url(query)?, cancel).await?;
        catalog_page(response, query)
    }

    /// Genre labels and filter slugs from the official per-kind genre list.
    pub async fn genres(
        &self,
        kind: MediaKind,
        cancel: &CancellationToken,
    ) -> Result<Vec<Genre>, Error> {
        self.ready(cancel)?;
        let mut url = trakt::endpoint(&["genres", trakt::kind_path(kind)])?;
        url.set_query(None);
        let data = self.json(url, cancel).await?;
        genre_list(&data)
    }
}

pub(super) fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

fn catalog_url(query: &CatalogQuery) -> Result<Url, Error> {
    if !(1..=MAX_PAGE).contains(&query.page)
        || !(1..=MAX_LIMIT).contains(&query.limit)
        || query.genres.len() > 10
        || query.genres.iter().any(|slug| !valid_slug(slug))
        || query
            .years
            .is_some_and(|range| range.start < 1800 || range.end > 2200 || range.start > range.end)
    {
        return Err(Error::InvalidCatalogQuery);
    }
    let operation = match query.sort {
        CatalogSort::Popular => "popular",
        CatalogSort::Trending => "trending",
    };
    let mut url = trakt::endpoint(&[trakt::kind_path(query.kind), operation])?;
    url.query_pairs_mut()
        .append_pair("page", &query.page.to_string())
        .append_pair("limit", &query.limit.to_string());
    if !query.genres.is_empty() {
        url.query_pairs_mut()
            .append_pair("genres", &query.genres.join(","));
    }
    if let Some(years) = query.years {
        let years = if years.start == years.end {
            years.start.to_string()
        } else {
            format!("{}-{}", years.start, years.end)
        };
        url.query_pairs_mut().append_pair("years", &years);
    }
    Ok(url)
}

fn header_number(headers: &HeaderMap, name: &str) -> Result<Option<u32>, Error> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(Error::InvalidResponse);
    }
    let raw = value.to_str().map_err(|_| Error::InvalidResponse)?;
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidResponse);
    }
    raw.parse().map(Some).map_err(|_| Error::InvalidResponse)
}

fn catalog_page(response: JsonResponse, query: &CatalogQuery) -> Result<CatalogPage, Error> {
    let page = header_number(&response.headers, "x-pagination-page")?.unwrap_or(query.page);
    let limit =
        header_number(&response.headers, "x-pagination-limit")?.unwrap_or(u32::from(query.limit));
    let page_count = header_number(&response.headers, "x-pagination-page-count")?;
    let total_items = header_number(&response.headers, "x-pagination-item-count")?;
    let rows = response.data.as_array().ok_or(Error::InvalidResponse)?;
    if page != query.page
        || limit == 0
        || limit > u32::from(query.limit)
        || page_count.is_some_and(|count| page > count && !rows.is_empty())
    {
        return Err(Error::InvalidResponse);
    }
    if rows.len() > limit as usize {
        return Err(Error::InvalidResponse);
    }
    let mut seen = BTreeSet::new();
    let items = rows
        .iter()
        .filter_map(|row| {
            let data = match query.sort {
                CatalogSort::Popular => row,
                CatalogSort::Trending => match query.kind {
                    MediaKind::Movie => &row["movie"],
                    MediaKind::Series => &row["show"],
                },
            };
            trakt::media(data, query.kind)
        })
        .filter(|media| seen.insert(media.id.clone()))
        .collect();
    Ok(CatalogPage {
        items,
        page,
        limit: limit as u16,
        page_count,
        total_items,
        next_page: page_count
            .filter(|count| !rows.is_empty() && page < *count && page < MAX_PAGE)
            .map(|_| page + 1),
    })
}

fn genre_list(data: &Value) -> Result<Vec<Genre>, Error> {
    let rows = data.as_array().ok_or(Error::InvalidResponse)?;
    if rows.len() > 100 {
        return Err(Error::TooLarge);
    }
    let mut seen = BTreeSet::new();
    Ok(rows
        .iter()
        .filter_map(|row| {
            let slug = row["slug"].as_str().filter(|slug| valid_slug(slug))?;
            let name = row["name"].as_str().filter(|name| {
                !name.trim().is_empty()
                    && name.chars().count() <= 100
                    && !name.chars().any(char::is_control)
            })?;
            seen.insert(slug).then(|| Genre {
                name: name.trim().to_owned(),
                slug: slug.to_owned(),
            })
        })
        .collect())
}
