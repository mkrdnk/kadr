use std::{fmt, fs::OpenOptions, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::{
    AppPaths, Error, LibraryEntry, MediaId, NetworkLimits, PlaybackProgress, PlaybackTarget,
    PlayerPreference, ProgressOrigin, RateLimit, Result, Settings, SourceId, StoredSource,
    models::validate_timestamp,
    paths::{decode_path, encode_path},
    snapshots::link_source_row,
};

pub const SCHEMA_VERSION: i64 = 5;

/// Synchronous connection, intended to be owned by one background worker.
/// No raw connection is exposed, so writes always pass validation.
pub struct Storage {
    pub(crate) connection: Connection,
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Storage([redacted])")
    }
}

impl Storage {
    /// Opens a trusted absolute database path. The parent must already exist.
    /// Newly created DB files use 0600 on Unix; existing permissions are kept.
    /// Future schemas are refused before any schema/journal-mode changes.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
            || path.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(Error::InvalidInput("database path"));
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(Error::Io),
        }
        // No SQLITE_OPEN_URI: caller paths are paths, never SQLite URI options.
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut connection)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { connection })
    }

    pub fn open_default(paths: &AppPaths) -> Result<Self> {
        paths.create_dirs()?;
        Self::open(&paths.database_path())
    }

    pub fn load_settings(&self) -> Result<Settings> {
        let settings = self.connection.query_row(
            "SELECT player, torrents_enabled, download_kind, download_bytes,
                    upload_kind, upload_bytes, max_connections, cache_limit_bytes,
                    catalog_enabled, cache_directory
             FROM settings WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                ))
            },
        )?;
        let (
            player,
            torrents,
            download_kind,
            download,
            upload_kind,
            upload,
            connections,
            cache,
            catalog,
            cache_directory,
        ) = settings;
        let settings = Settings {
            default_player: match player.as_str() {
                "mpv" => PlayerPreference::Mpv,
                "vlc" => PlayerPreference::Vlc,
                "browser" => PlayerPreference::Browser,
                _ => return Err(Error::CorruptData("player preference")),
            },
            network: NetworkLimits {
                torrents_enabled: match torrents {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::CorruptData("torrent preference")),
                },
                download: RateLimit::decode(&download_kind, download)?,
                upload: RateLimit::decode(&upload_kind, upload)?,
                max_connections: connections
                    .try_into()
                    .map_err(|_| Error::CorruptData("connection limit"))?,
            },
            cache_limit_bytes: cache
                .try_into()
                .map_err(|_| Error::CorruptData("cache limit"))?,
            catalog_enabled: match catalog {
                0 => false,
                1 => true,
                _ => return Err(Error::CorruptData("catalog preference")),
            },
            cache_directory: cache_directory.map(decode_path).transpose()?,
        };
        settings
            .validate()
            .map_err(|_| Error::CorruptData("settings"))?;
        Ok(settings)
    }

    /// Validation precedes the atomic singleton write. Invalid settings never
    /// replace the previous valid settings; corrupt reads never reset defaults.
    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        write_settings(&self.connection, settings)
    }

    pub fn set_favorite(&mut self, media_id: &MediaId, favorite: bool) -> Result<()> {
        self.connection.execute(
            "INSERT INTO library(media_id, favorite) VALUES (?1, ?2)
             ON CONFLICT(media_id) DO UPDATE SET favorite = excluded.favorite",
            params![media_id.as_str(), favorite],
        )?;
        Ok(())
    }

    /// Record a real open event, not a playback observation. Older events cannot
    /// overwrite the latest source or timestamp. No progress row is created.
    /// Same-ID torrent selections are preserved; explicitly select a new file
    /// with save_source + link_source before opening it.
    pub fn record_opened(
        &mut self,
        media_id: &MediaId,
        source_id: &SourceId,
        opened_at: i64,
    ) -> Result<()> {
        self.record_opened_target(
            &PlaybackTarget {
                media_id: media_id.clone(),
                episode: None,
            },
            source_id,
            opened_at,
        )
    }

    /// Call only after a successful player/browser launch. Records exact target
    /// history and its selected file without inventing measured playback.
    /// Newest timestamp wins independently per target and for media history.
    pub fn record_opened_target(
        &mut self,
        target: &PlaybackTarget,
        source_id: &SourceId,
        opened_at: i64,
    ) -> Result<()> {
        target.validate()?;
        validate_timestamp(opened_at)?;
        let (season, episode) = target.sql_episode();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO library(media_id, favorite, last_opened_at, last_source_id)
             VALUES (?1, 0, ?2, ?3)
             ON CONFLICT(media_id) DO UPDATE SET
                 last_opened_at = excluded.last_opened_at,
                 last_source_id = excluded.last_source_id
             WHERE library.last_opened_at IS NULL
                OR excluded.last_opened_at >= library.last_opened_at",
            params![target.media_id.as_str(), opened_at, source_id.as_str()],
        )?;
        let changed = transaction.execute(
            "INSERT INTO target_history(media_id, season, episode, source_id, file_id, opened_at)
             VALUES (?1, ?2, ?3, ?4,
                 CASE WHEN EXISTS (SELECT 1 FROM target_sources
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3 AND source_id = ?4)
                 THEN (SELECT file_id FROM target_sources
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3 AND source_id = ?4)
                 ELSE (SELECT file_id FROM sources WHERE source_id = ?4) END, ?5)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                 source_id = excluded.source_id, file_id = excluded.file_id,
                 opened_at = excluded.opened_at
             WHERE excluded.opened_at >= target_history.opened_at",
            params![
                target.media_id.as_str(),
                season,
                episode,
                source_id.as_str(),
                opened_at
            ],
        )?;
        if changed != 0 {
            link_source_row(&transaction, target, source_id, false, None)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn library_entry(&self, media_id: &MediaId) -> Result<Option<LibraryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT media_id, favorite, last_opened_at, last_source_id
             FROM library WHERE media_id = ?1",
        )?;
        let mut rows = statement.query([media_id.as_str()])?;
        rows.next()?.map(decode_library_entry).transpose()
    }

    /// Sorted by opaque ID for stable results. A future large-library UI should
    /// introduce paginated queries rather than calling this from render.
    pub fn favorites(&self) -> Result<Vec<LibraryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT media_id, favorite, last_opened_at, last_source_id
             FROM library WHERE favorite = 1 ORDER BY media_id",
        )?;
        let mut rows = statement.query([])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            entries.push(decode_library_entry(row)?);
        }
        Ok(entries)
    }

    /// Newest first, with a stable ID tie-breaker; 1..=1000 rows per call.
    pub fn history(&self, limit: u32) -> Result<Vec<LibraryEntry>> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput("history limit"));
        }
        let mut statement = self.connection.prepare(
            "SELECT media_id, favorite, last_opened_at, last_source_id
             FROM library WHERE last_opened_at IS NOT NULL
             ORDER BY last_opened_at DESC, media_id LIMIT ?1",
        )?;
        let mut rows = statement.query([limit])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            entries.push(decode_library_entry(row)?);
        }
        Ok(entries)
    }

    /// Only genuine adapter telemetry belongs here. Browser is rejected.
    /// Last timestamp wins, independently for each (media, season, episode).
    /// This does not implicitly record a history event or infer completion.
    /// Uses current target selection; asynchronous player sessions should use
    /// `save_progress_for_source` with their immutable launch descriptor instead.
    pub fn save_progress(&mut self, progress: &PlaybackProgress) -> Result<()> {
        self.save_progress_impl(progress, None)
    }

    /// Snapshot selection from the actual player session, not mutable UI state.
    /// Late telemetry after a same-torrent reselection must still refer to the
    /// file the reporting player opened. This does not save the descriptor.
    pub fn save_progress_for_source(
        &mut self,
        progress: &PlaybackProgress,
        source: &StoredSource,
    ) -> Result<()> {
        if source.source_id()? != progress.source_id {
            return Err(Error::InvalidInput("progress source identity"));
        }
        self.save_progress_impl(progress, Some(source.file_id()))
    }

    fn save_progress_impl(
        &mut self,
        progress: &PlaybackProgress,
        selection: Option<Option<usize>>,
    ) -> Result<()> {
        progress.validate()?;
        let (season, episode) = progress.target.sql_episode();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO library(media_id, favorite) VALUES (?1, 0)
             ON CONFLICT(media_id) DO NOTHING",
            [progress.target.media_id.as_str()],
        )?;
        let changed = transaction.execute(
            "INSERT INTO playback_progress
             (media_id, season, episode, source_id, position_seconds,
              duration_seconds, updated_at, completed, origin, file_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                 CASE WHEN ?10 THEN ?11
                 WHEN EXISTS (SELECT 1 FROM target_sources
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3 AND source_id = ?4)
                 THEN (SELECT file_id FROM target_sources
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3 AND source_id = ?4)
                 ELSE (SELECT file_id FROM sources WHERE source_id = ?4) END)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                 source_id = excluded.source_id,
                 position_seconds = excluded.position_seconds,
                 duration_seconds = excluded.duration_seconds,
                 updated_at = excluded.updated_at,
                 completed = excluded.completed,
                 origin = excluded.origin,
                 file_id = excluded.file_id
             WHERE excluded.updated_at >= playback_progress.updated_at",
            params![
                progress.target.media_id.as_str(),
                season,
                episode,
                progress.source_id.as_str(),
                progress.position_seconds,
                progress.duration_seconds,
                progress.updated_at,
                progress.completed,
                match progress.origin {
                    ProgressOrigin::Mpv => "mpv",
                    ProgressOrigin::Vlc => "vlc",
                    ProgressOrigin::Browser => unreachable!("validated above"),
                },
                selection.is_some(),
                selection.flatten().map(|id| id as i64),
            ],
        )?;
        if changed != 0 {
            link_source_row(
                &transaction,
                &progress.target,
                &progress.source_id,
                false,
                selection,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_progress(&self, target: &PlaybackTarget) -> Result<Option<PlaybackProgress>> {
        target.validate()?;
        let (season, episode) = target.sql_episode();
        let progress = self.connection.query_row(
            "SELECT source_id, position_seconds, duration_seconds, updated_at, completed, origin
             FROM playback_progress WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![target.media_id.as_str(), season, episode],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        ).optional()?;
        let Some((source, position, duration, updated_at, completed, origin)) = progress else {
            return Ok(None);
        };
        let progress = PlaybackProgress {
            target: target.clone(),
            source_id: SourceId::new(source).map_err(|_| Error::CorruptData("source ID"))?,
            position_seconds: position,
            duration_seconds: duration,
            updated_at,
            completed: match completed {
                0 => false,
                1 => true,
                _ => return Err(Error::CorruptData("completion flag")),
            },
            origin: match origin.as_str() {
                "mpv" => ProgressOrigin::Mpv,
                "vlc" => ProgressOrigin::Vlc,
                _ => return Err(Error::CorruptData("progress origin")),
            },
        };
        progress
            .validate()
            .map_err(|_| Error::CorruptData("playback progress"))?;
        Ok(Some(progress))
    }

    /// Logical deletion of history/progress, retaining favorites and saved copies.
    /// Favorite source URLs, torrent paths and per-episode selections remain for
    /// reopening. Other original source descriptors are removed.
    /// Not a forensic secure erase of SQLite pages, backups or filesystem data.
    pub fn clear_playback_data(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM playback_progress", [])?;
        transaction.execute("DELETE FROM target_history", [])?;
        // Keep episode selections only for favorites. Saved copies need only
        // their own paths, not private origin URLs or torrent paths.
        transaction.execute(
            "DELETE FROM target_sources WHERE media_id IN
             (SELECT media_id FROM library WHERE favorite = 0)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM library WHERE favorite = 0
             AND media_id NOT IN (SELECT media_id FROM saved_files)",
            [],
        )?;
        transaction.execute(
            "UPDATE library SET last_opened_at = NULL, last_source_id = NULL",
            [],
        )?;
        transaction.execute(
            "DELETE FROM sources WHERE source_id NOT IN (SELECT source_id FROM target_sources)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM media_records WHERE media_id NOT IN (SELECT media_id FROM library)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn decode_library_entry(row: &rusqlite::Row<'_>) -> Result<LibraryEntry> {
    let media: String = row.get(0)?;
    let favorite: i64 = row.get(1)?;
    let opened: Option<i64> = row.get(2)?;
    let source: Option<String> = row.get(3)?;
    if opened.is_some_and(|time| time < 0) || opened.is_some() != source.is_some() {
        return Err(Error::CorruptData("library history"));
    }
    Ok(LibraryEntry {
        media_id: MediaId::new(media).map_err(|_| Error::CorruptData("media ID"))?,
        favorite: match favorite {
            0 => false,
            1 => true,
            _ => return Err(Error::CorruptData("favorite flag")),
        },
        last_opened_at: opened,
        last_source_id: source
            .map(SourceId::new)
            .transpose()
            .map_err(|_| Error::CorruptData("source ID"))?,
    })
}

fn write_settings(connection: &Connection, settings: &Settings) -> Result<()> {
    let (download_kind, download) = settings.network.download.encode();
    let (upload_kind, upload) = settings.network.upload.encode();
    connection.execute(
        "INSERT INTO settings
         (singleton, player, torrents_enabled, download_kind, download_bytes,
          upload_kind, upload_bytes, max_connections, cache_limit_bytes,
          catalog_enabled, cache_directory)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(singleton) DO UPDATE SET
             player = excluded.player,
             torrents_enabled = excluded.torrents_enabled,
             download_kind = excluded.download_kind,
             download_bytes = excluded.download_bytes,
             upload_kind = excluded.upload_kind,
             upload_bytes = excluded.upload_bytes,
             max_connections = excluded.max_connections,
             cache_limit_bytes = excluded.cache_limit_bytes,
             catalog_enabled = excluded.catalog_enabled,
             cache_directory = excluded.cache_directory",
        params![
            match settings.default_player {
                PlayerPreference::Mpv => "mpv",
                PlayerPreference::Vlc => "vlc",
                PlayerPreference::Browser => "browser",
            },
            settings.network.torrents_enabled,
            download_kind,
            download,
            upload_kind,
            upload,
            settings.network.max_connections,
            settings.cache_limit_bytes as i64,
            settings.catalog_enabled,
            settings.cache_directory.as_deref().map(encode_path),
        ],
    )?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<()> {
    // The complete chain and user_version changes share one transaction.
    // Failure (including a collision) leaves prior data/schema/version intact.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(Error::FutureSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if version < 0 {
        return Err(Error::InvalidSchemaVersion);
    }
    if version < 1 {
        transaction.execute_batch(
            "CREATE TABLE settings (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                player TEXT NOT NULL CHECK(player IN ('mpv', 'vlc', 'browser')),
                torrents_enabled INTEGER NOT NULL CHECK(torrents_enabled IN (0, 1)),
                download_kind TEXT NOT NULL,
                download_bytes INTEGER,
                upload_kind TEXT NOT NULL,
                upload_bytes INTEGER,
                max_connections INTEGER NOT NULL CHECK(max_connections BETWEEN 1 AND 4096),
                cache_limit_bytes INTEGER NOT NULL CHECK(cache_limit_bytes >= 1048576),
                CHECK ((download_kind = 'limited' AND download_bytes IS NOT NULL AND download_bytes > 0)
                    OR (download_kind IN ('paused', 'unlimited') AND download_bytes IS NULL)),
                CHECK ((upload_kind = 'limited' AND upload_bytes IS NOT NULL AND upload_bytes > 0)
                    OR (upload_kind IN ('paused', 'unlimited') AND upload_bytes IS NULL))
             ) STRICT;",
        )?;
        transaction.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        transaction.execute_batch(
            "CREATE TABLE library (
                media_id TEXT PRIMARY KEY NOT NULL,
                favorite INTEGER NOT NULL CHECK(favorite IN (0, 1)),
                last_opened_at INTEGER CHECK(last_opened_at >= 0),
                last_source_id TEXT,
                CHECK ((last_opened_at IS NULL) = (last_source_id IS NULL))
             ) STRICT;
             CREATE INDEX library_history ON library(last_opened_at DESC, media_id);
             CREATE TABLE playback_progress (
                media_id TEXT NOT NULL REFERENCES library(media_id) ON DELETE CASCADE,
                season INTEGER NOT NULL,
                episode INTEGER NOT NULL,
                source_id TEXT NOT NULL,
                position_seconds REAL NOT NULL CHECK(position_seconds >= 0),
                duration_seconds REAL CHECK(duration_seconds > 0 AND position_seconds <= duration_seconds),
                updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
                completed INTEGER NOT NULL CHECK(completed IN (0, 1)),
                origin TEXT NOT NULL CHECK(origin IN ('mpv', 'vlc')),
                PRIMARY KEY(media_id, season, episode),
                CHECK ((season = -1 AND episode = -1)
                    OR (season BETWEEN 0 AND 65535 AND episode BETWEEN 1 AND 65535))
             ) STRICT;",
        )?;
        transaction.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        transaction.execute_batch(
            "ALTER TABLE settings ADD COLUMN catalog_enabled INTEGER NOT NULL DEFAULT 1
                 CHECK(catalog_enabled IN (0, 1));
             ALTER TABLE settings ADD COLUMN cache_directory BLOB;
             ALTER TABLE playback_progress ADD COLUMN file_id INTEGER CHECK(file_id >= 0);
             CREATE TABLE sources (
                 source_id TEXT PRIMARY KEY NOT NULL,
                 kind TEXT NOT NULL CHECK(kind IN ('link', 'magnet', 'torrent')),
                 link TEXT,
                 path BLOB,
                 file_id INTEGER CHECK(file_id >= 0),
                 CHECK ((kind = 'link' AND link IS NOT NULL AND path IS NULL AND file_id IS NULL)
                     OR (kind = 'magnet' AND link IS NOT NULL AND path IS NULL)
                     OR (kind = 'torrent' AND link IS NULL AND path IS NOT NULL))
             ) STRICT;
             CREATE TABLE target_sources (
                 media_id TEXT NOT NULL REFERENCES library(media_id) ON DELETE CASCADE,
                 season INTEGER NOT NULL,
                 episode INTEGER NOT NULL,
                 source_id TEXT NOT NULL,
                 file_id INTEGER CHECK(file_id >= 0),
                 PRIMARY KEY(media_id, season, episode),
                 CHECK ((season = -1 AND episode = -1)
                     OR (season BETWEEN 0 AND 65535 AND episode BETWEEN 1 AND 65535))
             ) STRICT;
             CREATE INDEX target_sources_source ON target_sources(source_id);
             CREATE TABLE media_records (
                 media_id TEXT PRIMARY KEY NOT NULL,
                 title TEXT NOT NULL,
                 original_title TEXT,
                 year INTEGER CHECK(year BETWEEN 1 AND 9999),
                 kind TEXT NOT NULL CHECK(kind IN ('movie', 'series')),
                 description TEXT NOT NULL,
                 poster_url TEXT,
                 attribution TEXT NOT NULL
             ) STRICT;
             CREATE TABLE saved_files (
                 media_id TEXT NOT NULL REFERENCES library(media_id) ON DELETE CASCADE,
                 season INTEGER NOT NULL,
                 episode INTEGER NOT NULL,
                 path BLOB NOT NULL,
                 saved_at INTEGER NOT NULL CHECK(saved_at >= 0),
                 PRIMARY KEY(media_id, season, episode),
                 CHECK ((season = -1 AND episode = -1)
                     OR (season BETWEEN 0 AND 65535 AND episode BETWEEN 1 AND 65535))
             ) STRICT;
             INSERT INTO target_sources(media_id, season, episode, source_id)
                 SELECT media_id, -1, -1, last_source_id FROM library
                 WHERE last_source_id IS NOT NULL;
             INSERT INTO target_sources(media_id, season, episode, source_id)
                 SELECT media_id, season, episode, source_id FROM playback_progress
                 WHERE TRUE
                 ON CONFLICT(media_id, season, episode) DO UPDATE SET
                     source_id = excluded.source_id;",
        )?;
        transaction.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        transaction.execute_batch(
            "CREATE TABLE target_history (
                 media_id TEXT NOT NULL REFERENCES library(media_id) ON DELETE CASCADE,
                 season INTEGER NOT NULL,
                 episode INTEGER NOT NULL,
                 source_id TEXT NOT NULL,
                 file_id INTEGER CHECK(file_id >= 0),
                 opened_at INTEGER NOT NULL CHECK(opened_at >= 0),
                 PRIMARY KEY(media_id, season, episode),
                 CHECK ((season = -1 AND episode = -1)
                     OR (season BETWEEN 0 AND 65535 AND episode BETWEEN 1 AND 65535))
             ) STRICT;
             CREATE INDEX target_history_opened ON target_history(opened_at DESC, media_id);
             INSERT INTO target_history(media_id, season, episode, source_id, file_id, opened_at)
                 SELECT l.media_id, -1, -1, l.last_source_id,
                     CASE WHEN t.source_id IS NOT NULL THEN t.file_id ELSE s.file_id END,
                     l.last_opened_at
                 FROM library AS l
                 LEFT JOIN target_sources AS t ON t.media_id = l.media_id
                     AND t.season = -1 AND t.episode = -1 AND t.source_id = l.last_source_id
                 LEFT JOIN sources AS s ON s.source_id = l.last_source_id
                 WHERE l.last_opened_at IS NOT NULL;",
        )?;
        transaction.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        // No foreign key references sources: keep all target/history keys exactly as-is.
        transaction.execute_batch(
            "CREATE TABLE sources_v5 (
                 source_id TEXT PRIMARY KEY NOT NULL,
                 kind TEXT NOT NULL CHECK(kind IN ('link', 'magnet', 'torrent', 'rutracker')),
                 link TEXT,
                 path BLOB,
                 file_id INTEGER CHECK(file_id >= 0),
                 CHECK ((kind = 'link' AND link IS NOT NULL AND path IS NULL AND file_id IS NULL)
                     OR (kind = 'magnet' AND link IS NOT NULL AND path IS NULL)
                     OR (kind = 'rutracker' AND link IS NULL AND path IS NULL)
                     OR (kind = 'torrent' AND link IS NULL AND path IS NOT NULL))
             ) STRICT;
             INSERT INTO sources_v5 SELECT source_id, kind, link, path, file_id FROM sources;
             DROP TABLE sources;
             ALTER TABLE sources_v5 RENAME TO sources;",
        )?;
        transaction.pragma_update(None, "user_version", 5)?;
    }
    if version < 1 {
        write_settings(&transaction, &Settings::default())?;
    }
    transaction.commit()?;
    Ok(())
}
