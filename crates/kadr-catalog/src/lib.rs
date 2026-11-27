//! Explicit, read-only Trakt metadata requests. No automatic media sources.
//! Configure the public app Client ID through `KADR_TRAKT_CLIENT_ID` at runtime,
//! or at compile time for standalone distributions. No OAuth or secret is used.
//! Construction is offline; missing/invalid configuration is reported only after
//! the caller's consent and cancellation gates, on an explicit request.
#![forbid(unsafe_code)]

mod browse;
pub mod commons;
pub mod popcorn;
pub mod rutracker;
pub mod torznab;
mod trakt;

pub use browse::BrowseCatalog;
pub use trakt::is_public_catalog_image;

use kadr_core::catalog::{Episode, Media, MediaKind};
use reqwest::header::HeaderValue;
use serde_json::Value;
use std::{fmt, time::Duration};
pub use tokio_util::sync::CancellationToken;
use url::Url;

const BODY_LIMIT: usize = 4 * 1024 * 1024;
const SEARCH_LIMIT: usize = 20;
const TRAKT: &str = "https://api.trakt.tv";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// A privacy switch: disabled clients never issue metadata requests.
    pub enabled: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Errors contain no Client IDs, URLs, queries, bodies, or nested HTTP errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Disabled,
    Cancelled,
    MissingClientId,
    InvalidQuery,
    InvalidId,
    NotFound,
    Network,
    Timeout,
    RateLimited,
    Http(u16),
    TooLarge,
    InvalidResponse,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("Online catalog is disabled in settings."),
            Self::Cancelled => f.write_str("Catalog request cancelled."),
            Self::MissingClientId => f.write_str(
                "[TRAKT_MISSING_CLIENT_ID] Set KADR_TRAKT_CLIENT_ID to your public Trakt app Client ID, then restart Kadr. Standalone builds can configure this variable at compile time. No OAuth login or Client Secret is needed.",
            ),
            Self::InvalidQuery => f.write_str("Enter a search of 1–200 characters."),
            Self::InvalidId => f.write_str("Unsupported catalog identifier."),
            Self::NotFound => f.write_str("The catalog item is unavailable."),
            Self::Network => {
                f.write_str("Catalog connection failed. Check your connection and retry.")
            }
            Self::Timeout => {
                f.write_str("Catalog request timed out. Retry when the service is available.")
            }
            Self::RateLimited => {
                f.write_str("Catalog rate limit reached. Wait a few seconds and retry.")
            }
            Self::Http(status @ (401 | 403)) => write!(
                f,
                "Trakt denied access (HTTP {status}). Check KADR_TRAKT_CLIENT_ID (public app Client ID) and restart Kadr."
            ),
            Self::Http(status) => write!(f, "Catalog service returned HTTP {status}. Retry later."),
            Self::TooLarge => f.write_str("Catalog response exceeds the safety limit."),
            Self::InvalidResponse => f.write_str("Catalog service returned unsupported data."),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    options: Options,
    client_id: Option<HeaderValue>,
    #[cfg(test)]
    fixture_origin: Option<Url>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("options", &self.options)
            .field("client_id", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl Client {
    /// No network activity. A missing or invalid Client ID does not prevent startup.
    /// A runtime override takes precedence even if invalid; it never silently uses
    /// the distributor's compiled ID instead of the user's explicit configuration.
    pub fn new(options: Options) -> Result<Self, Error> {
        Self::new_with_proxy(options, None)
    }

    /// Construct offline with an optional unauthenticated proxy endpoint.
    /// `None` retains reqwest's implicit proxy defaults. An explicit proxy
    /// replaces those defaults, with no direct fallback if it fails.
    pub fn new_with_proxy(options: Options, proxy: Option<&str>) -> Result<Self, Error> {
        let mut http = reqwest::Client::builder()
            .user_agent("kadr/0.1 (desktop catalog; explicit user requests)")
            .connect_timeout(Duration::from_secs(5))
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(endpoint) = proxy {
            http = http.no_proxy().proxy(explicit_proxy(endpoint)?);
        }
        let http = http.build().map_err(|_| Error::Network)?;
        Ok(Self {
            http,
            options,
            client_id: match std::env::var("KADR_TRAKT_CLIENT_ID") {
                Ok(raw) => configured_client_id(Some(&raw), option_env!("KADR_TRAKT_CLIENT_ID")),
                Err(std::env::VarError::NotPresent) => {
                    configured_client_id(None, option_env!("KADR_TRAKT_CLIENT_ID"))
                }
                Err(std::env::VarError::NotUnicode(_)) => None,
            },
            #[cfg(test)]
            fixture_origin: None,
        })
    }

    fn ready(&self, cancel: &CancellationToken) -> Result<(), Error> {
        if !self.options.enabled {
            return Err(Error::Disabled);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.client_id.is_none() {
            return Err(Error::MissingClientId);
        }
        Ok(())
    }

    /// Search the first movie and show pages, preferring exact titles without
    /// issuing extra requests. Trakt's primary title is retained as supplied.
    /// A failed endpoint may leave nonempty results from the other endpoint.
    pub async fn search(
        &self,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Media>, Error> {
        self.ready(cancel)?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 200 || query.chars().any(char::is_control) {
            return Err(Error::InvalidQuery);
        }
        let (movies, shows) = self.lists("search", Some(query), cancel).await?;
        let mut media: Vec<_> = movies.into_iter().chain(shows).collect();
        let exact = trakt::matching_title(query);
        media.sort_by_key(|media| trakt::matching_title(&media.title) != exact);
        Ok(media)
    }

    /// Complete numbered episodes, including specials (season 0), or an error.
    /// At most 100 seasons, 1,000 episodes per season and 20,000 total, using
    /// one bounded seasons-with-episodes request. Oversized or inconsistent collections
    /// fail rather than silently returning an incomplete successful episode list.
    pub async fn details(&self, id: &str, cancel: &CancellationToken) -> Result<Media, Error> {
        self.ready(cancel)?;
        let (kind, number) = trakt::provider_id(id)?;
        let data = self
            .json(
                trakt::endpoint(&[trakt::kind_path(kind), &number.to_string()])?,
                cancel,
            )
            .await?;
        if data["ids"]["trakt"].as_u64() != Some(number) {
            return Err(Error::InvalidResponse);
        }
        let mut media = trakt::media(&data, kind).ok_or(Error::NotFound)?;
        if kind == MediaKind::Series {
            let mut url = trakt::endpoint(&["shows", &number.to_string(), "seasons"])?;
            url.set_query(Some("extended=episodes,full"));
            media.episodes = trakt::episodes(&self.json(url, cancel).await?)?;
        }
        self.ready(cancel)?;
        Ok(media)
    }

    async fn json(&self, url: Url, cancel: &CancellationToken) -> Result<Value, Error> {
        self.ready(cancel)?;
        if !trakt::api_url(&url) {
            return Err(Error::InvalidResponse);
        }
        // Test-only loopback transport. The production destination is never configurable.
        #[cfg(test)]
        let url = if let Some(origin) = &self.fixture_origin {
            let mut local = origin.clone();
            local.set_path(url.path());
            local.set_query(url.query());
            local
        } else {
            url
        };
        let request = self
            .http
            .get(url)
            .header("content-type", "application/json")
            .header("trakt-api-version", "2")
            .header(
                "trakt-api-key",
                self.client_id.clone().ok_or(Error::MissingClientId)?,
            );
        self.transport(request, cancel).await
    }

    async fn transport(
        &self,
        request: reqwest::RequestBuilder,
        cancel: &CancellationToken,
    ) -> Result<Value, Error> {
        self.ready(cancel)?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout(REQUEST_TIMEOUT, read_json(request)) => {
                self.ready(cancel)?;
                result.map_err(|_| Error::Timeout)?
            }
        }
    }
}

fn explicit_proxy(endpoint: &str) -> Result<reqwest::Proxy, Error> {
    if !kadr_core::proxy::is_valid_endpoint(endpoint) {
        return Err(Error::Network);
    }
    reqwest::Proxy::all(endpoint).map_err(|_| Error::Network)
}

fn configured_client_id(runtime: Option<&str>, compiled: Option<&str>) -> Option<HeaderValue> {
    runtime.or(compiled).and_then(client_id)
}

fn client_id(raw: &str) -> Option<HeaderValue> {
    if raw.is_empty()
        || raw.len() > 256
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return None;
    }
    let mut header = HeaderValue::from_str(raw).ok()?;
    header.set_sensitive(true);
    Some(header)
}

async fn read_json(request: reqwest::RequestBuilder) -> Result<Value, Error> {
    let mut response = request.send().await.map_err(network_error)?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err(Error::NotFound),
        429 => return Err(Error::RateLimited),
        status => return Err(Error::Http(status)),
    }
    if response
        .content_length()
        .is_some_and(|size| size > BODY_LIMIT as u64)
    {
        return Err(Error::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if chunk.len() > BODY_LIMIT - body.len() {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse)
}

fn network_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod proxy_tests;
