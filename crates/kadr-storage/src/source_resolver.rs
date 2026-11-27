//! Resolver persistence, separate from playback settings and the media file cache.
//!
//! Payloads are opaque, externally encoded normalized results, never raw provider
//! responses. Call `source_cache_put` only on successful resolution (including a
//! successful empty result), not on network/authentication/decoding errors.

use std::{collections::HashSet, fmt};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{Error, MediaId, Result, Storage, models::validate_timestamp};

const MAX_CONFIGS: usize = 16;
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: i64 = 16 * 1024 * 1024;
const MAX_CACHE_ENTRIES: i64 = 1024;

/// Independent resolver configuration; does not change [`crate::Settings`].
/// Defaults preserve RuTracker availability; the caller must still enforce the
/// separate BitTorrent/catalog permissions. No Torznab endpoints are configured.
/// Commons defaults on for clean installs, but v8 upgrades preserve prior privacy
/// choices by starting it off. Commons requires catalog consent, not BitTorrent.
/// Public fields are private user data; `Debug` and validation errors redact them.
#[derive(Clone, PartialEq, Eq)]
pub struct SourcePreferences {
    pub rutracker_enabled: bool,
    /// Built-in Wikimedia Commons provider for a limited reviewed Blender film set.
    /// Requires no credentials; callers must enforce the global catalog permission.
    pub commons_enabled: bool,
    pub popcorn_enabled: bool,
    /// At most 16 configs, in user order, with distinct normalized endpoints.
    pub torznab: Vec<TorznabConfig>,
    /// Optional nonblank, control-free labels, at most 64 UTF-8 bytes each.
    pub preferred_resolution: Option<String>,
    pub preferred_language: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct TorznabConfig {
    /// Nonblank, control-free display name, at most 128 UTF-8 bytes.
    pub name: String,
    /// At most 2048 bytes. HTTPS, or HTTP to localhost/a literal loopback IP.
    /// Paths are allowed; credentials, queries and fragments are forbidden.
    pub base_url: String,
    /// Runtime environment variable NAME, never an API key value. Must start
    /// with KADR_TORZNAB_ and have a nonempty [A-Z0-9_] suffix; max 128 bytes.
    /// Storage never reads the process environment.
    pub api_key_env: Option<String>,
    pub enabled: bool,
}

impl Default for SourcePreferences {
    fn default() -> Self {
        Self {
            rutracker_enabled: true,
            commons_enabled: true,
            popcorn_enabled: false,
            torznab: Vec::new(),
            preferred_resolution: None,
            preferred_language: None,
        }
    }
}

impl fmt::Debug for SourcePreferences {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SourcePreferences([redacted])")
    }
}

impl fmt::Debug for TorznabConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TorznabConfig([redacted])")
    }
}

impl SourcePreferences {
    /// Validates disabled configs too; invalid writes leave preferences/cache intact.
    pub fn validate(&self) -> Result<()> {
        if self.torznab.len() > MAX_CONFIGS {
            return Err(Error::InvalidInput("Torznab config count"));
        }
        for label in [&self.preferred_resolution, &self.preferred_language]
            .into_iter()
            .flatten()
        {
            validate_label(label, 64)?;
        }
        let mut endpoints = HashSet::new();
        for config in &self.torznab {
            config.validate()?;
            if !endpoints.insert(config.endpoint_identity()?) {
                return Err(Error::InvalidInput("duplicate Torznab endpoint"));
            }
        }
        Ok(())
    }
}

impl TorznabConfig {
    /// Canonical identity for duplicate detection and provider cache keys.
    /// The request itself uses `base_url`, preserving the configured API path.
    pub fn endpoint_identity(&self) -> Result<String> {
        let mut url = kadr_core::sources::torznab_endpoint(&self.base_url)
            .ok_or(Error::InvalidInput("Torznab endpoint"))?;
        let path = normalize_path(url.path());
        url.set_path(path.trim_end_matches('/'));
        Ok(url.to_string())
    }

    pub fn validate(&self) -> Result<()> {
        validate_label(&self.name, 128)?;
        self.endpoint_identity()?;
        if let Some(name) = &self.api_key_env
            && (name.len() > 128
                || !name.strip_prefix("KADR_TORZNAB_").is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                }))
        {
            return Err(Error::InvalidInput("Torznab API key environment name"));
        }
        Ok(())
    }
}

fn validate_label(value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(Error::InvalidInput("source preference label"));
    }
    Ok(())
}

fn normalize_path(path: &str) -> String {
    let mut normalized = String::new();
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            // Core already validated every percent escape.
            let hex = [bytes.next().unwrap(), bytes.next().unwrap()];
            let value = u8::from_str_radix(std::str::from_utf8(&hex).unwrap(), 16).unwrap();
            if value.is_ascii_alphanumeric() || b"-._~".contains(&value) {
                normalized.push(value as char);
            } else {
                use std::fmt::Write;
                write!(normalized, "%{value:02X}").unwrap();
            }
        } else {
            normalized.push(byte as char);
        }
    }
    normalized
}

impl Storage {
    /// Forget results associated with a provider session (for example on logout).
    pub fn clear_provider_results(&mut self, provider: &str) -> Result<()> {
        MediaId::new(provider).map_err(|_| Error::InvalidInput("source cache provider key"))?;
        self.connection.execute(
            "DELETE FROM source_cache WHERE provider_key = ?1",
            [provider],
        )?;
        Ok(())
    }

    /// Read one consistent, validated snapshot; corruption never resets defaults.
    pub fn load_source_preferences(&self) -> Result<SourcePreferences> {
        let transaction = self.connection.unchecked_transaction()?;
        let preferences = read_preferences(&transaction)?;
        transaction.commit()?;
        Ok(preferences)
    }

    /// Validate then atomically save the full snapshot and clear resolver results
    /// on any configuration change. Saving an identical snapshot retains cache.
    /// This affects neither playback settings nor media files.
    pub fn save_source_preferences(&mut self, preferences: &SourcePreferences) -> Result<()> {
        preferences.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if read_preferences(&transaction)? != *preferences {
            transaction.execute(
                "UPDATE source_preferences SET rutracker_enabled = ?1,
                 preferred_resolution = ?2, preferred_language = ?3,
                 commons_enabled = ?4, popcorn_enabled = ?5 WHERE singleton = 1",
                params![
                    preferences.rutracker_enabled,
                    preferences.preferred_resolution,
                    preferences.preferred_language,
                    preferences.commons_enabled,
                    preferences.popcorn_enabled,
                ],
            )?;
            transaction.execute("DELETE FROM torznab_configs", [])?;
            for (position, config) in preferences.torznab.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO torznab_configs
                     (position, name, base_url, api_key_env, enabled) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        position as i64,
                        config.name,
                        config.base_url,
                        config.api_key_env,
                        config.enabled,
                    ],
                )?;
            }
            transaction.execute("DELETE FROM source_cache", [])?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Opaque normalized result bytes keyed by media AND provider. Keys follow
    /// MediaId's 1..=256-byte opaque ASCII grammar (no URLs, paths or secrets).
    /// `now` is nonnegative Unix seconds. Expiration is exclusive and reads never
    /// extend TTL. Both reads and writes remove all expired rows.
    pub fn source_cache_get(
        &self,
        media_key: &str,
        provider_key: &str,
        now: i64,
    ) -> Result<Option<Vec<u8>>> {
        validate_cache_input(media_key, provider_key, now)?;
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM source_cache WHERE expires_at <= ?1", [now])?;
        let data = transaction
            .query_row(
                "SELECT data FROM source_cache
                 WHERE media_key = ?1 AND provider_key = ?2
                   AND stored_at <= ?3 AND expires_at > ?3",
                params![media_key, provider_key, now],
                |row| row.get(0),
            )
            .optional()?;
        transaction.commit()?;
        Ok(data)
    }

    /// Invalidate one previous batch when a refreshed result is not cacheable.
    pub fn source_cache_remove(&mut self, media_key: &str, provider_key: &str) -> Result<()> {
        validate_cache_keys(media_key, provider_key)?;
        self.connection.execute(
            "DELETE FROM source_cache WHERE media_key = ?1 AND provider_key = ?2",
            params![media_key, provider_key],
        )?;
        Ok(())
    }

    /// Cache only successful results, encoded by the resolver. `empty` means a
    /// successful zero-result response (300s TTL); otherwise TTL is 1800s.
    /// Network/auth/parse failures must NOT call this method.
    /// The caller must discard in-flight results from an older preference
    /// snapshot after a config change; this API carries no config revision.
    ///
    /// Max payload 1 MiB, max 1024 rows and 16 MiB of payloads in total. Reject
    /// oversize input without mutation; after upsert, evict oldest writes first
    /// (opaque keys break timestamp ties). Reads do not change eviction order.
    /// These bounds are independent of Settings::cache_limit_bytes/file cache.
    pub fn source_cache_put(
        &mut self,
        media_key: &str,
        provider_key: &str,
        now: i64,
        empty: bool,
        data: &[u8],
    ) -> Result<()> {
        validate_cache_input(media_key, provider_key, now)?;
        if data.len() > MAX_ENTRY_BYTES {
            return Err(Error::InvalidInput("source cache payload size"));
        }
        let expires = now
            .checked_add(if empty { 300 } else { 1800 })
            .ok_or(Error::InvalidInput("source cache expiration"))?;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM source_cache WHERE expires_at <= ?1", [now])?;
        transaction.execute(
            "INSERT INTO source_cache(media_key, provider_key, stored_at, expires_at, data)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(media_key, provider_key) DO UPDATE SET
                 stored_at = excluded.stored_at, expires_at = excluded.expires_at,
                 data = excluded.data",
            params![media_key, provider_key, now, expires, data],
        )?;
        transaction.execute(
            "WITH ranked AS (
                 SELECT media_key, provider_key,
                     ROW_NUMBER() OVER newest AS count,
                     SUM(length(data)) OVER newest AS bytes
                 FROM source_cache
                 WINDOW newest AS (ORDER BY stored_at DESC, media_key DESC, provider_key DESC
                     ROWS UNBOUNDED PRECEDING)
             )
             DELETE FROM source_cache WHERE (media_key, provider_key) IN
                 (SELECT media_key, provider_key FROM ranked WHERE count > ?1 OR bytes > ?2)",
            params![MAX_CACHE_ENTRIES, MAX_CACHE_BYTES],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn validate_cache_input(media: &str, provider: &str, now: i64) -> Result<()> {
    validate_cache_keys(media, provider)?;
    validate_timestamp(now)
}

fn validate_cache_keys(media: &str, provider: &str) -> Result<()> {
    MediaId::new(media).map_err(|_| Error::InvalidInput("source cache media key"))?;
    MediaId::new(provider).map_err(|_| Error::InvalidInput("source cache provider key"))?;
    Ok(())
}

fn read_preferences(connection: &Connection) -> Result<SourcePreferences> {
    let (enabled, resolution, language, commons, popcorn) = connection.query_row(
        "SELECT rutracker_enabled, preferred_resolution, preferred_language,
                commons_enabled, popcorn_enabled
         FROM source_preferences WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;
    let mut preferences = SourcePreferences {
        rutracker_enabled: decode_bool(enabled)?,
        commons_enabled: decode_bool(commons)?,
        popcorn_enabled: decode_bool(popcorn)?,
        preferred_resolution: resolution,
        preferred_language: language,
        torznab: Vec::new(),
    };
    // Bound allocation even if a database was modified outside this adapter.
    let mut statement = connection.prepare(
        "SELECT position, name, base_url, api_key_env, enabled
         FROM torznab_configs ORDER BY position LIMIT 17",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, i64>(0)? != preferences.torznab.len() as i64 {
            return Err(Error::CorruptData("source preferences"));
        }
        preferences.torznab.push(TorznabConfig {
            name: row.get(1)?,
            base_url: row.get(2)?,
            api_key_env: row.get(3)?,
            enabled: decode_bool(row.get(4)?)?,
        });
    }
    preferences
        .validate()
        .map_err(|_| Error::CorruptData("source preferences"))?;
    Ok(preferences)
}

fn decode_bool(value: i64) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::CorruptData("source preferences")),
    }
}

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE source_preferences (
             singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
             rutracker_enabled INTEGER NOT NULL CHECK(rutracker_enabled IN (0, 1)),
             preferred_resolution TEXT,
             preferred_language TEXT
         ) STRICT;
         INSERT INTO source_preferences(singleton, rutracker_enabled) VALUES (1, 1);
         CREATE TABLE torznab_configs (
             position INTEGER PRIMARY KEY CHECK(position BETWEEN 0 AND 15),
             name TEXT NOT NULL,
             base_url TEXT NOT NULL,
             api_key_env TEXT,
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1))
         ) STRICT;
         CREATE TABLE source_cache (
             media_key TEXT NOT NULL,
             provider_key TEXT NOT NULL,
             stored_at INTEGER NOT NULL CHECK(stored_at >= 0),
             expires_at INTEGER NOT NULL CHECK(expires_at > stored_at),
             data BLOB NOT NULL CHECK(length(data) <= 1048576),
             PRIMARY KEY(media_key, provider_key)
         ) STRICT;
         CREATE INDEX source_cache_expiration ON source_cache(expires_at);",
    )?;
    Ok(())
}
