use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    EpisodeKey, Error, LibraryItem, MediaId, MediaKind, MediaRecord, PlaybackProgress,
    PlaybackTarget, Result, SavedFile, SourceId, Storage, StoredSource,
    paths::{decode_path, encode_path},
};

// One definition of target membership for both listing and exact lookup.
// A favorite without concrete targets stays an item-level library entry.
const LIBRARY_TARGETS: &str = "
    WITH concrete_targets AS (
        SELECT media_id, season, episode FROM playback_progress
        UNION SELECT media_id, season, episode FROM target_sources
        UNION SELECT media_id, season, episode FROM saved_files
        UNION SELECT media_id, season, episode FROM target_history
    ), library_targets AS (
        SELECT media_id, season, episode FROM concrete_targets
        UNION SELECT media_id, -1, -1 FROM library AS l
            WHERE favorite = 1 AND NOT EXISTS (
                SELECT 1 FROM concrete_targets AS t WHERE t.media_id = l.media_id
            )
    )";

struct TargetHistory {
    source_id: SourceId,
    file_id: Option<i64>,
    opened_at: i64,
}

impl Storage {
    /// Persist only user-approved inputs. The ID must match core validation.
    /// Re-saving a magnet may update tracker parameters without changing identity.
    pub fn save_source(&mut self, id: &SourceId, source: &StoredSource) -> Result<()> {
        if source.source_id()? != *id {
            return Err(Error::InvalidInput("source identity mismatch"));
        }
        let (kind, link, path, file_id) = match source {
            StoredSource::Link(link) => ("link", Some(link.as_str()), None, None),
            StoredSource::TorrentLink { uri, file_id } => (
                "magnet",
                Some(uri.as_str()),
                None,
                file_id.map(|id| id as i64),
            ),
            StoredSource::TorrentFile { path, file_id } => (
                "torrent",
                None,
                Some(encode_path(path)),
                file_id.map(|id| id as i64),
            ),
        };
        self.connection.execute(
            "INSERT INTO sources(source_id, kind, link, path, file_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source_id) DO UPDATE SET
                 kind = excluded.kind, link = excluded.link,
                 path = excluded.path, file_id = excluded.file_id",
            params![id.as_str(), kind, link, path, file_id],
        )?;
        Ok(())
    }

    pub fn load_source(&self, id: &SourceId) -> Result<Option<StoredSource>> {
        let value = self
            .connection
            .query_row(
                "SELECT kind, link, path, file_id FROM sources WHERE source_id = ?1",
                [id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((kind, link, path, file_id)) = value else {
            return Ok(None);
        };
        let source = match (kind.as_str(), link, path, file_id) {
            ("link", Some(link), None, None) => StoredSource::Link(link),
            ("magnet", Some(uri), None, file_id) => StoredSource::TorrentLink {
                uri,
                file_id: decode_file_id(file_id)?,
            },
            ("torrent", None, Some(path), file_id) => StoredSource::TorrentFile {
                path: decode_path(path)?,
                file_id: decode_file_id(file_id)?,
            },
            _ => return Err(Error::CorruptData("source descriptor")),
        };
        if source.source_id().ok().as_ref() != Some(id) {
            return Err(Error::CorruptData("source identity"));
        }
        Ok(Some(source))
    }

    /// Explicitly associate a saved descriptor with one item/episode. Snapshot
    /// torrent selection here: core source identity intentionally excludes file ID.
    pub fn link_source(&mut self, target: &PlaybackTarget, id: &SourceId) -> Result<()> {
        target.validate()?;
        if self.load_source(id)?.is_none() {
            return Err(Error::InvalidInput("missing source descriptor"));
        }
        let transaction = self.connection.transaction()?;
        link_source_row(&transaction, target, id, true, None)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn source_for_target(&self, target: &PlaybackTarget) -> Result<Option<StoredSource>> {
        target.validate()?;
        let (season, episode) = target.sql_episode();
        let value = self
            .connection
            .query_row(
                "SELECT source_id, file_id FROM target_sources
             WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![target.media_id.as_str(), season, episode],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .optional()?;
        let Some((id, file_id)) = value else {
            return Ok(None);
        };
        let id = SourceId::new(id).map_err(|_| Error::CorruptData("source ID"))?;
        self.source_with_selection(&id, file_id)
    }

    fn source_with_selection(
        &self,
        id: &SourceId,
        file_id: Option<i64>,
    ) -> Result<Option<StoredSource>> {
        let selection = decode_file_id(file_id)?;
        Ok(match self.load_source(id)? {
            Some(StoredSource::TorrentFile { path, .. }) => Some(StoredSource::TorrentFile {
                path,
                file_id: selection,
            }),
            Some(StoredSource::TorrentLink { uri, .. }) => Some(StoredSource::TorrentLink {
                uri,
                file_id: selection,
            }),
            Some(StoredSource::Link(uri)) if selection.is_some() => {
                let source = StoredSource::TorrentLink {
                    uri,
                    file_id: selection,
                };
                source
                    .manual_source()
                    .map_err(|_| Error::CorruptData("source selection"))?;
                Some(source)
            }
            source => source,
        })
    }

    pub fn upsert_media(&mut self, media: &MediaRecord) -> Result<()> {
        media.validate()?;
        self.connection.execute(
            "INSERT INTO media_records
             (media_id, title, original_title, year, kind, description, poster_url, attribution)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(media_id) DO UPDATE SET
                 title = excluded.title, original_title = excluded.original_title,
                 year = excluded.year, kind = excluded.kind,
                 description = excluded.description, poster_url = excluded.poster_url,
                 attribution = excluded.attribution",
            params![
                media.id.as_str(),
                media.title,
                media.original_title,
                media.year,
                match media.kind {
                    MediaKind::Movie => "movie",
                    MediaKind::Series => "series",
                },
                media.description,
                media.poster_url,
                media.attribution,
            ],
        )?;
        Ok(())
    }

    pub fn media_record(&self, id: &MediaId) -> Result<Option<MediaRecord>> {
        let value = self
            .connection
            .query_row(
                "SELECT title, original_title, year, kind, description, poster_url, attribution
             FROM media_records WHERE media_id = ?1",
                [id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((title, original_title, year, kind, description, poster_url, attribution)) = value
        else {
            return Ok(None);
        };
        let media = MediaRecord {
            id: id.clone(),
            title,
            original_title,
            year: year
                .map(u16::try_from)
                .transpose()
                .map_err(|_| Error::CorruptData("media year"))?,
            kind: match kind.as_str() {
                "movie" => MediaKind::Movie,
                "series" => MediaKind::Series,
                _ => return Err(Error::CorruptData("media kind")),
            },
            description,
            poster_url,
            attribution,
        };
        media
            .validate()
            .map_err(|_| Error::CorruptData("media snapshot"))?;
        Ok(Some(media))
    }

    /// Consistent display/resume snapshots, newest history first then stable ID.
    /// Includes favorites, progress-only items and saved copies; 1..=1000 items.
    /// Legacy rows without descriptors remain visible but cannot be resumed.
    pub fn library(&self, limit: u32) -> Result<Vec<LibraryItem>> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput("library limit"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let entries = {
            let mut statement = transaction.prepare(
                "SELECT media_id, favorite, last_opened_at, last_source_id FROM library
                 WHERE favorite = 1 OR last_opened_at IS NOT NULL
                    OR EXISTS (SELECT 1 FROM playback_progress p WHERE p.media_id = library.media_id)
                    OR EXISTS (SELECT 1 FROM saved_files s WHERE s.media_id = library.media_id)
                 ORDER BY last_opened_at DESC, media_id LIMIT ?1",
            )?;
            let mut rows = statement.query([limit])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                entries.push(crate::storage::decode_library_entry(row)?);
            }
            entries
        };
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            let latest = self
                .connection
                .query_row(
                    "SELECT season, episode FROM (
                         SELECT season, episode, 0 AS priority, updated_at AS activity_at
                             FROM playback_progress WHERE media_id = ?1
                         UNION ALL SELECT season, episode, 1, opened_at
                             FROM target_history WHERE media_id = ?1
                         UNION ALL SELECT season, episode, 2, 0
                             FROM target_sources WHERE media_id = ?1
                         UNION ALL SELECT season, episode, 3, saved_at
                             FROM saved_files WHERE media_id = ?1
                     ) ORDER BY priority, activity_at DESC, season DESC, episode DESC LIMIT 1",
                    [entry.media_id.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let (season, episode) = latest.unwrap_or((-1, -1));
            let target = decode_target(entry.media_id, season, episode)?;
            items.push(self.load_library_target(&target)?);
        }
        transaction.commit()?;
        Ok(items)
    }

    /// One consistent row per distinct stored playback target, not just the
    /// latest episode of each media item. Includes source-only and saved-only
    /// targets, legacy item history, and item-level favorites with no targets.
    /// Favorite state remains media-level; no episodes are fabricated for it.
    /// Accepts 1..=1000 rows, ordered by activity then media/season/episode.
    pub fn library_targets(&self, limit: u32) -> Result<Vec<LibraryItem>> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::InvalidInput("library limit"));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let targets = {
            let mut statement = transaction.prepare(&format!(
                "{LIBRARY_TARGETS}
                 SELECT t.media_id, t.season, t.episode FROM library_targets AS t
                 JOIN library AS l ON l.media_id = t.media_id
                 LEFT JOIN playback_progress AS p ON p.media_id = t.media_id
                     AND p.season = t.season AND p.episode = t.episode
                 LEFT JOIN saved_files AS s ON s.media_id = t.media_id
                     AND s.season = t.season AND s.episode = t.episode
                 LEFT JOIN target_history AS h ON h.media_id = t.media_id
                     AND h.season = t.season AND h.episode = t.episode
                 ORDER BY MAX(COALESCE(p.updated_at, -1), COALESCE(s.saved_at, -1),
                     COALESCE(h.opened_at, -1))
                     DESC, t.media_id, t.season, t.episode
                 LIMIT ?1"
            ))?;
            let mut rows = statement.query([limit])?;
            let mut targets = Vec::new();
            while let Some(row) = rows.next()? {
                let id = MediaId::new(row.get::<_, String>(0)?)
                    .map_err(|_| Error::CorruptData("media ID"))?;
                targets.push(decode_target(id, row.get(1)?, row.get(2)?)?);
            }
            targets
        };
        let items = targets
            .iter()
            .map(|target| self.load_library_target(target))
            .collect::<Result<Vec<_>>>()?;
        transaction.commit()?;
        Ok(items)
    }

    /// Look up exactly the same target snapshot used by library_targets.
    /// Missing episodes never fall back to item-level or another episode.
    pub fn library_target(&self, target: &PlaybackTarget) -> Result<Option<LibraryItem>> {
        target.validate()?;
        let transaction = self.connection.unchecked_transaction()?;
        let (season, episode) = target.sql_episode();
        let exists = transaction
            .query_row(
                &format!(
                    "{LIBRARY_TARGETS}
                     SELECT 1 FROM library_targets
                     WHERE media_id = ?1 AND season = ?2 AND episode = ?3"
                ),
                params![target.media_id.as_str(), season, episode],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let item = exists
            .then(|| self.load_library_target(target))
            .transpose()?;
        transaction.commit()?;
        Ok(item)
    }

    fn load_library_target(&self, target: &PlaybackTarget) -> Result<LibraryItem> {
        let entry = self
            .library_entry(&target.media_id)?
            .ok_or(Error::CorruptData("library target"))?;
        let history = self.target_history(target)?;
        let progress = self.load_progress(target)?;
        let source = if let Some(progress) = &progress {
            self.source_for_progress(progress)?
        } else if let Some(history) = &history {
            // A prelaunch selection must not reinterpret a previous browser open.
            self.source_with_selection(&history.source_id, history.file_id)?
        } else {
            self.source_for_target(target)?
        };
        Ok(LibraryItem {
            media: self.media_record(&target.media_id)?,
            entry,
            target: target.clone(),
            opened_at: history.map(|history| history.opened_at),
            progress,
            source,
        })
    }

    fn target_history(&self, target: &PlaybackTarget) -> Result<Option<TargetHistory>> {
        let (season, episode) = target.sql_episode();
        let value = self
            .connection
            .query_row(
                "SELECT source_id, file_id, opened_at FROM target_history
             WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![target.media_id.as_str(), season, episode],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        value
            .map(|(source_id, file_id, opened_at)| {
                crate::models::validate_timestamp(opened_at)
                    .map_err(|_| Error::CorruptData("target history"))?;
                decode_file_id(file_id)?;
                Ok(TargetHistory {
                    source_id: SourceId::new(source_id)
                        .map_err(|_| Error::CorruptData("source ID"))?,
                    file_id,
                    opened_at,
                })
            })
            .transpose()
    }

    fn source_for_progress(&self, progress: &PlaybackProgress) -> Result<Option<StoredSource>> {
        let (season, episode) = progress.target.sql_episode();
        let file_id: Option<i64> = self.connection.query_row(
            "SELECT file_id FROM playback_progress
             WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![progress.target.media_id.as_str(), season, episode],
            |row| row.get(0),
        )?;
        self.source_with_selection(&progress.source_id, file_id)
    }

    /// Call after an interruption-safe copy has completed. Does not copy/check
    /// the file: it may be unavailable later, which the file-opening adapter handles.
    pub fn save_saved_file(&mut self, saved: &SavedFile) -> Result<()> {
        saved.validate()?;
        let (season, episode) = saved.target.sql_episode();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO library(media_id, favorite) VALUES (?1, 0)
             ON CONFLICT(media_id) DO NOTHING",
            [saved.target.media_id.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO saved_files(media_id, season, episode, path, saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(media_id, season, episode) DO UPDATE SET
                 path = excluded.path, saved_at = excluded.saved_at",
            params![
                saved.target.media_id.as_str(),
                season,
                episode,
                encode_path(&saved.path),
                saved.saved_at
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn saved_file(&self, target: &PlaybackTarget) -> Result<Option<SavedFile>> {
        target.validate()?;
        let (season, episode) = target.sql_episode();
        let value = self
            .connection
            .query_row(
                "SELECT path, saved_at FROM saved_files
             WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
                params![target.media_id.as_str(), season, episode],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        value
            .map(|(path, saved_at)| {
                let saved = SavedFile {
                    target: target.clone(),
                    path: decode_path(path)?,
                    saved_at,
                };
                saved
                    .validate()
                    .map_err(|_| Error::CorruptData("saved file"))?;
                Ok(saved)
            })
            .transpose()
    }

    pub fn saved_files(&self) -> Result<Vec<SavedFile>> {
        let mut statement = self.connection.prepare(
            "SELECT media_id, season, episode FROM saved_files ORDER BY media_id, season, episode",
        )?;
        let mut rows = statement.query([])?;
        let mut files = Vec::new();
        while let Some(row) = rows.next()? {
            let id = MediaId::new(row.get::<_, String>(0)?)
                .map_err(|_| Error::CorruptData("media ID"))?;
            let target = decode_target(id, row.get(1)?, row.get(2)?)?;
            files.push(
                self.saved_file(&target)?
                    .ok_or(Error::CorruptData("saved file"))?,
            );
        }
        Ok(files)
    }

    /// Forget only the record, never delete a user-owned file.
    pub fn remove_saved_file(&mut self, target: &PlaybackTarget) -> Result<()> {
        target.validate()?;
        let (season, episode) = target.sql_episode();
        self.connection.execute(
            "DELETE FROM saved_files WHERE media_id = ?1 AND season = ?2 AND episode = ?3",
            params![target.media_id.as_str(), season, episode],
        )?;
        Ok(())
    }
}

pub(crate) fn link_source_row(
    connection: &Connection,
    target: &PlaybackTarget,
    id: &SourceId,
    replace_selection: bool,
    selection: Option<Option<usize>>,
) -> Result<()> {
    let (season, episode) = target.sql_episode();
    connection.execute(
        "INSERT INTO library(media_id, favorite) VALUES (?1, 0)
         ON CONFLICT(media_id) DO NOTHING",
        [target.media_id.as_str()],
    )?;
    connection.execute(
        "INSERT INTO target_sources(media_id, season, episode, source_id, file_id)
         VALUES (?1, ?2, ?3, ?4, CASE WHEN ?6 THEN ?7
             ELSE (SELECT file_id FROM sources WHERE source_id = ?4) END)
         ON CONFLICT(media_id, season, episode) DO UPDATE SET
             source_id = excluded.source_id,
             file_id = CASE WHEN ?5 OR target_sources.source_id != excluded.source_id
                            THEN excluded.file_id ELSE target_sources.file_id END",
        params![
            target.media_id.as_str(),
            season,
            episode,
            id.as_str(),
            replace_selection,
            selection.is_some(),
            selection.flatten().map(|id| id as i64)
        ],
    )?;
    Ok(())
}

fn decode_file_id(id: Option<i64>) -> Result<Option<usize>> {
    id.map(usize::try_from)
        .transpose()
        .map_err(|_| Error::CorruptData("torrent file ID"))
}

fn decode_target(media_id: MediaId, season: i64, episode: i64) -> Result<PlaybackTarget> {
    let episode = match (season, episode) {
        (-1, -1) => None,
        (season, episode) => Some(
            EpisodeKey::new(
                season
                    .try_into()
                    .map_err(|_| Error::CorruptData("season"))?,
                episode
                    .try_into()
                    .map_err(|_| Error::CorruptData("episode"))?,
            )
            .map_err(|_| Error::CorruptData("episode"))?,
        ),
    };
    Ok(PlaybackTarget { media_id, episode })
}
