//! Search torrents on `/forum/tracker.php`.

pub(crate) mod parser;

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use reqwest::Method;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::http;
use crate::models::{TopicId, TopicState};

/// Field by which results are sorted.
///
/// Numeric values map to the `o=` query parameter on rutracker (sort *field*).
/// The set of supported fields is fixed by the upstream HTML form; gaps in
/// the numeric encoding (`3`, `5`, `6`, `9`) correspond to options that
/// rutracker has retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Sort {
    /// Sort by registration timestamp.
    Registered,
    /// Sort alphabetically by title.
    Title,
    /// Sort by total downloads.
    Downloads,
    /// Sort by torrent size.
    Size,
    /// Sort by last-message timestamp in the topic.
    LastMessage,
    /// Sort by current seeders.
    Seeds,
    /// Sort by current leechers.
    Leeches,
}

impl Sort {
    pub(crate) fn as_param(self) -> &'static str {
        // rutracker `o=N` mapping (positions 3, 5, 6, 9 are retired).
        match self {
            Sort::Registered => "1",
            Sort::Title => "2",
            Sort::Downloads => "4",
            Sort::Size => "7",
            Sort::LastMessage => "8",
            Sort::Seeds => "10",
            Sort::Leeches => "11",
        }
    }
}

/// Sort direction.
///
/// Maps to the `s=` query parameter on rutracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Order {
    /// Ascending order (smallest / oldest first).
    Asc,
    /// Descending order (largest / newest first).
    Desc,
}

impl Order {
    pub(crate) fn as_param(self) -> &'static str {
        match self {
            Order::Asc => "1",
            Order::Desc => "2",
        }
    }
}

/// Page size assumed by this client. rutracker has historically returned 50
/// rows per `tracker.php` page; if upstream changes this, both
/// [`SearchResults::total_pages`] and the `start=` offset will drift — please
/// file a bug.
pub const PAGE_SIZE: u32 = 50;

/// Maximum search query length we accept. Beyond this rutracker rejects the
/// request anyway; we cap client-side to keep error reporting obvious.
pub const MAX_QUERY_LEN: usize = 1024;

/// Builder returned by [`Client::search`](crate::Client::search).
///
/// Configure with [`sort`](Self::sort)/[`order`](Self::order)/[`page`](Self::page),
/// then call [`send`](Self::send) to execute.
pub struct SearchRequest<'a> {
    client: &'a Client,
    query: String,
    sort: Option<Sort>,
    order: Option<Order>,
    page: u32,
    forum: Option<u64>,
}

impl<'a> SearchRequest<'a> {
    pub(crate) fn new(client: &'a Client, query: impl Into<String>) -> Self {
        Self {
            client,
            query: query.into(),
            sort: None,
            order: None,
            page: 1,
            forum: None,
        }
    }

    /// Restrict the search to a specific subforum.
    ///
    /// Maps to the `f=<id>` query parameter on `tracker.php`.
    pub fn forum(mut self, forum_id: u64) -> Self {
        self.forum = Some(forum_id);
        self
    }

    /// Set the sort field.
    pub fn sort(mut self, s: Sort) -> Self {
        self.sort = Some(s);
        self
    }

    /// Set the sort direction.
    pub fn order(mut self, o: Order) -> Self {
        self.order = Some(o);
        self
    }

    /// Set the 1-based page number (default `1`). Page size is fixed at
    /// [`PAGE_SIZE`] by rutracker.
    ///
    /// Setting `0` is a programming error. To keep the builder chainable we
    /// store the value as-is; [`Self::send`] will reject it with
    /// [`Error::InvalidArgument`].
    pub fn page(mut self, p: u32) -> Self {
        self.page = p;
        self
    }

    /// Execute the search.
    ///
    /// # Errors
    /// - [`Error::InvalidArgument`] when both the query and forum are
    ///   missing (rutracker rejects such requests), when the query exceeds
    ///   [`MAX_QUERY_LEN`] bytes, or when `page == 0`.
    /// - [`Error::Http`] / [`Error::Server`] / [`Error::RateLimited`] for
    ///   transport failures.
    /// - [`Error::Parse`] when the response HTML doesn't match the expected
    ///   layout.
    ///
    /// # Browsing a forum without a query
    /// When [`forum`](Self::forum) is set, the query may be empty —
    /// rutracker then returns the most-recent topics inside that subforum
    /// (`tracker.php?f=<id>`). Pagination works as usual.
    pub async fn send(self) -> Result<SearchResults> {
        let query = self.query.trim();
        let has_query = !query.is_empty();
        if !has_query && self.forum.is_none() {
            return Err(Error::InvalidArgument(
                "either a non-empty query or a forum filter is required".into(),
            ));
        }
        if query.len() > MAX_QUERY_LEN {
            return Err(Error::InvalidArgument(format!(
                "search query length {} exceeds MAX_QUERY_LEN={MAX_QUERY_LEN}",
                query.len()
            )));
        }
        if self.page == 0 {
            return Err(Error::InvalidArgument(
                "page must be >= 1 (rutracker uses 1-based pagination)".into(),
            ));
        }

        // Note the rutracker quirk: `o` = sort field, `s` = direction.
        let mut fields: Vec<(&'static str, Cow<'_, str>)> = Vec::new();
        if has_query {
            fields.push(("nm", Cow::Borrowed(query)));
        }
        if let Some(sort) = self.sort {
            fields.push(("o", Cow::Borrowed(sort.as_param())));
        }
        if let Some(order) = self.order {
            fields.push(("s", Cow::Borrowed(order.as_param())));
        }
        if let Some(forum) = self.forum {
            fields.push(("f", Cow::Owned(forum.to_string())));
        }
        let start = (self.page - 1) * PAGE_SIZE;
        if start > 0 {
            fields.push(("start", Cow::Owned(start.to_string())));
        }

        let body = http::cp1251_form_body(fields.iter().map(|(k, v)| (*k, v.as_ref())))?;

        let url = self.client.url("/forum/tracker.php")?;

        let resp = http::send(
            self.client.http(),
            Method::POST,
            &url,
            Some(http::Body::Form(&body)),
        )
        .await?;
        let resp = http::check_status(resp).await?;
        let html = http::read_html(resp).await?;

        let parsed = parser::parse(&html, self.page, self.client.base_url_internal())?;
        Ok(parsed)
    }
}

/// Paginated search response.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchResults {
    /// Total number of matching results across all pages (may be `0` when the
    /// counter cannot be parsed).
    pub total_count: u32,
    /// Current page number (1-based, matches the request).
    pub page: u32,
    /// Total number of pages, derived from `total_count` and [`PAGE_SIZE`].
    pub total_pages: u32,
    /// Up to [`PAGE_SIZE`] entries on the requested page.
    pub results: Vec<TorrentSummary>,
}

/// One row from the search results table.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TorrentSummary {
    /// Topic id (use with [`Client::get_topic`](crate::Client::get_topic) /
    /// [`AuthenticatedClient::download_torrent`](crate::AuthenticatedClient::download_torrent)).
    pub id: TopicId,
    /// Title as shown on the search page.
    pub title: String,
    /// Original uploader's display name, when available.
    pub author: Option<String>,
    /// Forum / category breadcrumb leaf (e.g. `"Дистрибутивы Linux"`).
    pub category: String,
    /// Torrent size in bytes.
    pub size: Option<u64>,
    /// Active seeders. Can be negative on rutracker when the counter is
    /// adjusted (rare); kept as `i64` to preserve the upstream value.
    pub seeds: Option<i64>,
    /// Active leechers.
    pub leeches: u32,
    /// Total downloads since the topic was registered.
    pub downloads: u32,
    /// Topic registration timestamp (UTC).
    pub registered: DateTime<Utc>,
    /// Approval / moderation state. `None` when the page does not include a
    /// state icon (e.g. legacy rows).
    pub state: Option<TopicState>,
    /// Canonical URL to the topic page.
    pub url: String,
}
