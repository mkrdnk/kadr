//! Error types returned by this crate.

use std::borrow::Cow;

/// Errors returned by this crate.
///
/// Marked `#[non_exhaustive]`: new variants may be added without breaking
/// callers. External library types (`reqwest::Error`, `url::ParseError`, etc.)
/// are wrapped behind `Box<dyn Error + Send + Sync>` so changing dependency
/// versions is not a breaking change for downstream code.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Underlying HTTP/transport failure (connect, TLS, timeout, body read…).
    #[error("HTTP error: {0}")]
    Http(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// URL parsing or composition failed.
    #[error("invalid URL: {0}")]
    Url(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Login was rejected by the server (bad credentials, captcha, malformed
    /// response…).
    #[error("authorization failed: {0}")]
    Authorization(String),

    /// An operation that requires login was called on an anonymous client,
    /// or the session was not accepted by the server.
    #[error("not authenticated — login required")]
    NotAuthenticated,

    /// Server returned a non-success status code.
    #[error("server returned status {0}")]
    Server(u16),

    /// HTML did not match the expected layout. Includes a structured
    /// description of where the failure happened.
    #[error("HTML parsing failed at {location}: {detail}")]
    Parse {
        /// Where the failure occurred (e.g. `"search/parser.rs:row.title_link"`,
        /// `"topic/parser.rs:info_hash"`).
        location: Cow<'static, str>,
        /// Human-readable detail.
        detail: String,
    },

    /// Rate limited or captcha required.
    #[error("rate limited / captcha required")]
    RateLimited,

    /// Response body exceeded the streaming memory bound.
    #[error("response exceeds safety limit")]
    TooLarge,

    /// Official `api.rutracker.org/v1/` returned an error message.
    #[error("API v1 error: {0}")]
    ApiError(String),

    /// Argument validation failure (e.g. empty query, page=0, batch too large).
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// JSON deserialization failure.
    #[error("JSON parsing failed: {0}")]
    Json(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// Build an `Error::Parse` with a static location and a free-form detail.
    pub(crate) fn parse(location: impl Into<Cow<'static, str>>, detail: impl Into<String>) -> Self {
        Self::Parse {
            location: location.into(),
            detail: detail.into(),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(Box::new(e))
    }
}

impl From<url::ParseError> for Error {
    fn from(e: url::ParseError) -> Self {
        Self::Url(Box::new(e))
    }
}

/// Crate-wide [`Result`](std::result::Result) alias.
pub type Result<T> = std::result::Result<T, Error>;
