//! Top-level client types.
//!
//! - [`Client`] performs anonymous operations (search, topic fetch, magnet,
//!   API v1 calls).
//! - [`AuthenticatedClient`] wraps a logged-in session and unlocks
//!   [`download_torrent`](AuthenticatedClient::download_torrent). It derefs to
//!   `Client`, so all anonymous methods remain available.
//!
//! Neither type implements [`Clone`]: cloning the underlying `reqwest::Client`
//! would silently share the cookie jar with another typed handle, defeating
//! the type-state guarantee around login. Wrap a client in [`std::sync::Arc`]
//! if you need to share it across tasks.

pub(crate) mod builder;
mod cookies;

use std::{fmt, ops::Deref};

use url::Url;

pub use self::builder::ClientBuilder;

use crate::error::Result;
use crate::models::TopicId;
use crate::search::SearchRequest;
use crate::topic::Topic;

/// Anonymous rutracker client.
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Returns a new [`ClientBuilder`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    pub(crate) fn from_parts(http: reqwest::Client, base_url: Url) -> Self {
        Self { http, base_url }
    }

    /// The rutracker base URL this client points at, as a string slice.
    ///
    /// Returns a `&str` rather than `&Url` to avoid leaking the `url` crate
    /// into this library's public API.
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Borrow the base URL as a [`Url`] for crate-internal use.
    pub(crate) fn base_url_internal(&self) -> &Url {
        &self.base_url
    }

    /// Borrow the underlying HTTP client (for crate-internal use).
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Resolve `path` against the base URL.
    ///
    /// # Errors
    /// Returns [`crate::Error::Url`] when the path cannot be joined.
    pub(crate) fn url(&self, path: &str) -> Result<Url> {
        Ok(self.base_url.join(path)?)
    }

    /// Start a search query. See [`SearchRequest`].
    pub fn search(&self, query: impl Into<String>) -> SearchRequest<'_> {
        SearchRequest::new(self, query)
    }

    /// Fetch a topic page (`/forum/viewtopic.php?t=<id>`) and parse it.
    ///
    /// # Errors
    /// See [`SearchRequest::send`](crate::SearchRequest::send) — the same
    /// transport / parse errors apply.
    pub async fn get_topic(&self, id: impl Into<TopicId>) -> Result<Topic> {
        crate::topic::fetch(self, id.into()).await
    }

    /// Convenience: fetch only the magnet link from a topic page.
    ///
    /// # Errors
    /// Same as [`Client::get_topic`].
    pub async fn get_magnet_link(&self, id: impl Into<TopicId>) -> Result<Option<String>> {
        let topic = self.get_topic(id).await?;
        Ok(topic.magnet)
    }

    /// Authenticate with username/password. Consumes the anonymous client and
    /// returns an [`AuthenticatedClient`] that owns the same session.
    ///
    /// # Cancellation
    /// If the future is dropped after the request was sent but before this
    /// method returns, rutracker may have already set a session cookie that
    /// is now lost (the consumed `Client` is gone). Build a fresh `Client`
    /// via [`Client::builder`] to retry.
    ///
    /// # Errors
    /// - [`crate::Error::InvalidArgument`] for empty username or password.
    /// - [`crate::Error::Authorization`] when the server rejects the credentials.
    /// - Transport / parse errors as in [`Client::get_topic`].
    pub async fn login(self, username: &str, password: &str) -> Result<AuthenticatedClient> {
        crate::auth::login(&self, username, password).await?;
        Ok(AuthenticatedClient { inner: self })
    }
}

/// Logged-in client. Derefs to [`Client`] so all anonymous methods are
/// available.
#[derive(Debug)]
pub struct AuthenticatedClient {
    inner: Client,
}

impl AuthenticatedClient {
    /// Download a `.torrent` file by topic id. Requires login.
    ///
    /// # Errors
    /// - [`crate::Error::NotAuthenticated`] when the server returns the
    ///   anonymous login page instead of a torrent file (typically means the
    ///   session has expired).
    /// - Transport errors as in other methods.
    pub async fn download_torrent(&self, id: impl Into<TopicId>) -> Result<Vec<u8>> {
        crate::download::download_torrent(&self.inner, id.into()).await
    }

    /// Unwrap the underlying anonymous [`Client`].
    ///
    /// This does **not** invalidate the session: the rutracker cookies remain
    /// in `reqwest`'s internal jar and authenticated endpoints will still
    /// behave as if you are logged in. To truly start fresh, build a new
    /// `Client` via [`Client::builder`].
    pub fn into_inner(self) -> Client {
        self.inner
    }
}

impl Deref for AuthenticatedClient {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
