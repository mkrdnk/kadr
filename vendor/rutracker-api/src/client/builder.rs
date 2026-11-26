use std::{sync::Arc, time::Duration};

use url::Url;

use crate::client::Client;
use crate::error::{Error, Result};

const BASE_URL: &str = "https://rutracker.org";

/// Desktop transport builder. The authorized origin cannot be overridden.
pub struct ClientBuilder {
    timeout: Duration,
    connect_timeout: Duration,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Create an HTTPS-only builder for the authorized domain.
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(5),
        }
    }

    /// Set the per-request deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the connection deadline.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Build a fresh, in-memory cookie session with normal TLS verification.
    pub fn build(self) -> Result<Client> {
        let http = reqwest::Client::builder()
            .user_agent("kadr/0.1 (local desktop; explicit user requests)")
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .cookie_provider(Arc::new(super::cookies::SilentJar::default()))
            .https_only(true)
            .referer(false)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("redirect limit")
                } else if authorized_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("redirect outside authorized HTTPS origin")
                }
            }))
            .build()?;
        Ok(Client::from_parts(http, Url::parse(BASE_URL)?))
    }
}

pub(crate) fn authorized_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("rutracker.org")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

pub(crate) fn check_url(url: &Url) -> Result<()> {
    if authorized_url(url) {
        Ok(())
    } else {
        Err(Error::InvalidArgument("unauthorized origin".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_authorized_https_origin_is_allowed() {
        for url in [
            "https://rutracker.org/forum/index.php",
            "https://rutracker.org:443/forum/login.php",
        ] {
            assert!(authorized_url(&Url::parse(url).unwrap()));
        }
        for url in [
            "http://rutracker.org/",
            "https://rutracker.org:444/",
            "https://rutracker.org.evil.example/",
            "https://evil.example/",
            "https://user@rutracker.org/",
            "https://user:password@rutracker.org/",
            "https://api.t-ru.org/",
        ] {
            assert!(!authorized_url(&Url::parse(url).unwrap()));
        }
    }
}
