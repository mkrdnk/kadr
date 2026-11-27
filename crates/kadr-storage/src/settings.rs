use std::{fmt, path::PathBuf};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerPreference {
    Mpv,
    Vlc,
    Browser,
}

/// Explicit semantics: zero is not silently interpreted as unlimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimit {
    Paused,
    BytesPerSecond(u64),
    Unlimited,
}

impl RateLimit {
    pub(crate) fn validate(self) -> Result<()> {
        if let Self::BytesPerSecond(bytes) = self
            && (bytes == 0 || bytes > i64::MAX as u64)
        {
            return Err(Error::InvalidInput("network rate limit"));
        }
        Ok(())
    }

    pub(crate) fn encode(self) -> (&'static str, Option<i64>) {
        match self {
            Self::Paused => ("paused", None),
            Self::BytesPerSecond(bytes) => ("limited", Some(bytes as i64)),
            Self::Unlimited => ("unlimited", None),
        }
    }

    pub(crate) fn decode(kind: &str, bytes: Option<i64>) -> Result<Self> {
        match (kind, bytes) {
            ("paused", None) => Ok(Self::Paused),
            ("unlimited", None) => Ok(Self::Unlimited),
            ("limited", Some(n)) if n > 0 => Ok(Self::BytesPerSecond(n as u64)),
            _ => Err(Error::CorruptData("network rate limit")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkLimits {
    /// Disabled until the user is informed about BitTorrent peer uploads.
    pub torrents_enabled: bool,
    pub download: RateLimit,
    pub upload: RateLimit,
    pub max_connections: u16,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    pub default_player: PlayerPreference,
    pub network: NetworkLimits,
    pub cache_limit_bytes: u64,
    /// Catalog requests are allowed only in response to user action.
    pub catalog_enabled: bool,
    /// Optional unauthenticated proxy for catalog providers and images.
    pub catalog_proxy: Option<String>,
    /// None uses AppPaths::cache_dir(). Persisting this does not move files.
    pub cache_directory: Option<PathBuf>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Settings([redacted])")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_player: PlayerPreference::Mpv,
            network: NetworkLimits {
                torrents_enabled: false,
                download: RateLimit::BytesPerSecond(10 * 1024 * 1024),
                upload: RateLimit::Paused,
                max_connections: 64,
            },
            cache_limit_bytes: 10 * 1024 * 1024 * 1024,
            catalog_enabled: true,
            catalog_proxy: None,
            cache_directory: None,
        }
    }
}

impl Settings {
    /// Limits fit SQLite signed integers. Cache minimum is 1 MiB; connection
    /// range is 1..=4096. Validation does not enforce limits on an engine.
    pub fn validate(&self) -> Result<()> {
        self.network.download.validate()?;
        self.network.upload.validate()?;
        if !(1..=4096).contains(&self.network.max_connections) {
            return Err(Error::InvalidInput("connection limit"));
        }
        if !(1024 * 1024..=i64::MAX as u64).contains(&self.cache_limit_bytes) {
            return Err(Error::InvalidInput("cache limit"));
        }
        if let Some(endpoint) = &self.catalog_proxy
            && !kadr_core::proxy::is_valid_endpoint(endpoint)
        {
            return Err(Error::InvalidInput("catalog proxy"));
        }
        if let Some(path) = &self.cache_directory {
            crate::paths::validate_private_path(path)?;
        }
        Ok(())
    }
}
