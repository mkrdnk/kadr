use std::{fs, path::PathBuf};

use kadr_storage::{
    EpisodeKey, Error, MediaId, MediaKind, MediaRecord, PlaybackProgress, PlaybackTarget,
    ProgressOrigin, SCHEMA_VERSION, SavedFile, Settings, SourceId, Storage, StoredSource,
};
use rusqlite::Connection;

fn target(episode: Option<u16>) -> PlaybackTarget {
    PlaybackTarget {
        media_id: MediaId::new("catalog:series:1").unwrap(),
        episode: episode.map(|episode| EpisodeKey::new(1, episode).unwrap()),
    }
}

fn link() -> StoredSource {
    StoredSource::Link("https://private.example/movie?token=secret".into())
}

fn magnet(file_id: Option<usize>) -> StoredSource {
    StoredSource::TorrentLink {
        uri: "magnet:?xt=urn:btih:0123456789012345678901234567890123456789&tr=https%3A%2F%2Fprivate.example%2Fsecret".into(),
        file_id,
    }
}

fn media() -> MediaRecord {
    MediaRecord {
        id: target(None).media_id,
        title: "private-user-title".into(),
        original_title: Some("Original title".into()),
        year: Some(2026),
        kind: MediaKind::Series,
        description: "A locally stored catalog snapshot.".into(),
        poster_url: Some("https://images.example/poster?secret=poster-token".into()),
        attribution: "Catalog provider".into(),
    }
}

fn progress(target: PlaybackTarget, source: &StoredSource, time: i64) -> PlaybackProgress {
    PlaybackProgress {
        target,
        source_id: source.source_id().unwrap(),
        position_seconds: 42.5,
        duration_seconds: Some(100.0),
        updated_at: time,
        completed: false,
        origin: ProgressOrigin::Mpv,
    }
}

#[test]
fn persisted_library_can_reconstruct_a_real_source_and_resume_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("storage.db");
    let source = link();
    let id = source.source_id().unwrap();
    let observation = progress(target(None), &source, 100);
    let mut store = Storage::open(&path).unwrap();
    store.upsert_media(&media()).unwrap();
    store.save_source(&id, &source).unwrap();
    store
        .record_opened(&target(None).media_id, &id, 99)
        .unwrap();
    store.save_progress(&observation).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    drop(store);

    let store = Storage::open(&path).unwrap();
    let items = store.library(100).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media, Some(media()));
    assert_eq!(items[0].target, target(None));
    assert_eq!(items[0].progress, Some(observation));
    let reopened = items[0].source.as_ref().unwrap().manual_source().unwrap();
    assert_eq!(reopened.id().as_str(), id.as_str());
    assert_eq!(
        reopened.http_url().unwrap().as_str(),
        "https://private.example/movie?token=secret",
    );
    assert_eq!(
        store.source_for_target(&target(None)).unwrap(),
        Some(source)
    );
    assert_eq!(store.media_record(&media().id).unwrap(), Some(media()));
    assert!(store.library(0).is_err());
    assert!(store.library(1001).is_err());
}

#[test]
fn sources_validate_core_identity_inputs_and_selection_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let good = link();
    let id = good.source_id().unwrap();
    store.save_source(&id, &good).unwrap();
    let fake = SourceId::new("arbitrary-but-syntactically-valid").unwrap();
    assert!(store.save_source(&fake, &good).is_err());
    let another = StoredSource::Link("https://other.example/video".into());
    assert!(store.save_source(&id, &another).is_err());
    assert_eq!(store.load_source(&id).unwrap(), Some(good.clone()));
    for invalid in [
        StoredSource::Link("file:///private/secret".into()),
        StoredSource::Link("https://user:secret@private.example/video".into()),
        StoredSource::Link("magnet:?xt=bad".into()),
        StoredSource::TorrentLink {
            uri: "https://private.example/video".into(),
            file_id: Some(1),
        },
        StoredSource::TorrentFile {
            path: PathBuf::from("relative.torrent"),
            file_id: None,
        },
        StoredSource::TorrentFile {
            path: dir.path().join("../escape.torrent"),
            file_id: None,
        },
        StoredSource::TorrentFile {
            path: dir.path().join("not-a-torrent.mp4"),
            file_id: None,
        },
        StoredSource::TorrentFile {
            path: dir.path().join("nul\0.torrent"),
            file_id: None,
        },
    ] {
        assert!(invalid.source_id().is_err());
        assert!(store.save_source(&id, &invalid).is_err());
        assert_eq!(store.load_source(&id).unwrap(), Some(good.clone()));
    }
    assert!(store.link_source(&target(None), &fake).is_err());
    assert!(store.library(10).unwrap().is_empty());
}

#[test]
fn session_progress_rejects_mismatched_source_without_mutating_previous_observation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let source = link();
    let observation = progress(target(None), &source, 1);
    store
        .save_source(&source.source_id().unwrap(), &source)
        .unwrap();
    store
        .save_progress_for_source(&observation, &source)
        .unwrap();
    let different = StoredSource::Link("https://other.example/movie".into());
    assert!(
        store
            .save_progress_for_source(&observation, &different)
            .is_err()
    );
    assert_eq!(
        store.load_progress(&observation.target).unwrap(),
        Some(observation)
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn torrent_selections_must_fit_sqlite_integers() {
    let invalid = magnet(Some(usize::MAX));
    assert!(invalid.source_id().is_err());
    let dir = tempfile::tempdir().unwrap();
    let invalid = StoredSource::TorrentFile {
        path: dir.path().join("valid.torrent"),
        file_id: Some(usize::MAX),
    };
    assert!(invalid.source_id().is_err());
}

#[test]
fn magnet_and_torrent_selections_are_isolated_per_episode_and_progress_snapshot() {
    for use_file in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let selected = |file_id| {
            if use_file {
                StoredSource::TorrentFile {
                    path: dir.path().join("private.torrent"),
                    file_id,
                }
            } else {
                magnet(file_id)
            }
        };
        let first = selected(Some(7));
        let second = selected(Some(9));
        let id = first.source_id().unwrap();
        assert_eq!(id, second.source_id().unwrap());
        let mut store = Storage::open(&path).unwrap();
        store.save_source(&id, &first).unwrap();
        store.link_source(&target(Some(1)), &id).unwrap();
        let earlier = progress(target(Some(1)), &first, 100);
        store.save_progress(&earlier).unwrap();
        store.save_source(&id, &second).unwrap();
        store.link_source(&target(Some(2)), &id).unwrap();
        store
            .save_progress(&progress(target(Some(2)), &second, 101))
            .unwrap();
        // More telemetry must preserve episode one's selection, not take the
        // global descriptor most recently used for episode two.
        let latest = progress(target(Some(1)), &first, 102);
        store.save_progress(&latest).unwrap();
        drop(store);
        let mut store = Storage::open(&path).unwrap();
        assert_eq!(
            store.source_for_target(&target(Some(1))).unwrap(),
            Some(first.clone())
        );
        assert_eq!(
            store.source_for_target(&target(Some(2))).unwrap(),
            Some(second.clone())
        );
        assert!(store.source_for_target(&target(None)).unwrap().is_none());
        let item = store.library(10).unwrap().remove(0);
        assert_eq!(item.progress, Some(latest.clone()));
        assert_eq!(item.source, Some(first.clone()));
        // Explicitly selecting another file does not reinterpret earlier telemetry.
        store.link_source(&target(Some(1)), &id).unwrap();
        assert_eq!(
            store.source_for_target(&target(Some(1))).unwrap(),
            Some(second)
        );
        let item = store.library(10).unwrap().remove(0);
        assert_eq!(item.progress, Some(latest));
        assert_eq!(item.source, Some(first.clone()));
        // A late observation must identify the file the old player opened,
        // even though the same target has since selected another file.
        let late = progress(target(Some(1)), &first, 103);
        store.save_progress_for_source(&late, &first).unwrap();
        let item = store.library(10).unwrap().remove(0);
        assert_eq!(item.progress, Some(late));
        assert_eq!(item.source, Some(first));
    }
}

#[test]
fn browser_history_does_not_pair_old_progress_with_a_different_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let original = link();
    store
        .save_source(&original.source_id().unwrap(), &original)
        .unwrap();
    store
        .save_progress(&progress(target(None), &original, 100))
        .unwrap();
    let browser = StoredSource::Link("https://browser.example/new-video".into());
    let id = browser.source_id().unwrap();
    store.save_source(&id, &browser).unwrap();
    store
        .record_opened(&target(None).media_id, &id, 101)
        .unwrap();
    let item = store.library(10).unwrap().remove(0);
    assert_eq!(item.entry.last_source_id, Some(id));
    assert_eq!(item.source, Some(original));
}

#[test]
fn clear_history_removes_source_secrets_except_favorite_resume_and_saved_copy_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let favorite_source = magnet(Some(2));
    let favorite_id = favorite_source.source_id().unwrap();
    store.save_source(&favorite_id, &favorite_source).unwrap();
    store.link_source(&target(Some(1)), &favorite_id).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    store.upsert_media(&media()).unwrap();
    store
        .save_progress(&progress(target(Some(1)), &favorite_source, 100))
        .unwrap();

    let mut other = target(None);
    other.media_id = MediaId::new("local:saved").unwrap();
    let source = link();
    let id = source.source_id().unwrap();
    store.save_source(&id, &source).unwrap();
    store.record_opened(&other.media_id, &id, 200).unwrap();
    let saved = SavedFile {
        target: other.clone(),
        path: dir.path().join("user-copy.mp4"),
        saved_at: 201,
    };
    fs::write(&saved.path, b"saved media independent of disposable cache").unwrap();
    store.save_saved_file(&saved).unwrap();
    let orphan = StoredSource::Link("https://orphan.example/secret".into());
    let orphan_id = orphan.source_id().unwrap();
    store.save_source(&orphan_id, &orphan).unwrap();
    store.clear_playback_data().unwrap();
    drop(store);
    let mut store = Storage::open(&path).unwrap();
    assert!(store.history(10).unwrap().is_empty());
    assert!(store.load_progress(&target(Some(1))).unwrap().is_none());
    assert!(store.load_source(&id).unwrap().is_none());
    assert!(store.load_source(&orphan_id).unwrap().is_none());
    assert_eq!(
        store.source_for_target(&target(Some(1))).unwrap(),
        Some(favorite_source)
    );
    let favorite = store
        .library(10)
        .unwrap()
        .into_iter()
        .find(|item| item.entry.favorite)
        .unwrap();
    assert_eq!(favorite.target, target(Some(1)));
    assert!(favorite.progress.is_none());
    assert_eq!(store.media_record(&media().id).unwrap(), Some(media()));
    assert_eq!(store.saved_file(&other).unwrap(), Some(saved.clone()));
    assert_eq!(store.saved_files().unwrap(), vec![saved.clone()]);
    assert!(fs::read(&store.saved_file(&other).unwrap().unwrap().path).is_ok());
    store.remove_saved_file(&other).unwrap();
    assert!(store.saved_file(&other).unwrap().is_none());
    assert!(
        saved.path.exists(),
        "metadata removal must not delete user files"
    );
}

#[test]
fn saved_copy_only_episode_has_its_own_resume_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let saved = SavedFile {
        target: target(Some(4)),
        path: dir.path().join("episode4.mp4"),
        saved_at: 1,
    };
    store.save_saved_file(&saved).unwrap();
    store.clear_playback_data().unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    let item = store.library(10).unwrap().remove(0);
    assert_eq!(item.target, saved.target);
    assert_eq!(store.saved_file(&item.target).unwrap(), Some(saved));
    assert!(item.source.is_none());
}

#[test]
fn removing_last_saved_copy_or_favorite_does_not_leave_empty_library_cards() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let saved = SavedFile {
        target: target(Some(1)),
        path: dir.path().join("ep1.mp4"),
        saved_at: 1,
    };
    let other = SavedFile {
        target: target(Some(2)),
        path: dir.path().join("ep2.mp4"),
        saved_at: 2,
    };
    store.save_saved_file(&saved).unwrap();
    store.save_saved_file(&other).unwrap();
    store.remove_saved_file(&saved.target).unwrap();
    assert_eq!(store.library(10).unwrap().len(), 1);
    store.remove_saved_file(&other.target).unwrap();
    assert!(store.library(10).unwrap().is_empty());
    store.set_favorite(&saved.target.media_id, true).unwrap();
    assert_eq!(store.library(10).unwrap().len(), 1);
    store.set_favorite(&saved.target.media_id, false).unwrap();
    assert!(store.library(10).unwrap().is_empty());
    // Removing a copy must not remove independent history.
    let source = link();
    store
        .save_source(&source.source_id().unwrap(), &source)
        .unwrap();
    store
        .record_opened(&saved.target.media_id, &source.source_id().unwrap(), 3)
        .unwrap();
    store.save_saved_file(&saved).unwrap();
    store.remove_saved_file(&saved.target).unwrap();
    assert_eq!(store.library(10).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_roundtrip_losslessly_for_sources_settings_and_saved_copies() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let torrent = dir
        .path()
        .join(OsString::from_vec(b"secret-\xff.torrent".to_vec()));
    let copy = dir
        .path()
        .join(OsString::from_vec(b"saved-\xfe.mp4".to_vec()));
    let cache = dir.path().join(OsString::from_vec(b"cache-\xfd".to_vec()));
    let source = StoredSource::TorrentFile {
        path: torrent,
        file_id: Some(3),
    };
    let id = source.source_id().unwrap();
    let mut store = Storage::open(&path).unwrap();
    store.save_source(&id, &source).unwrap();
    let saved = SavedFile {
        target: target(None),
        path: copy,
        saved_at: 1,
    };
    store.save_saved_file(&saved).unwrap();
    let settings = Settings {
        catalog_enabled: false,
        cache_directory: Some(cache),
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_source(&id).unwrap(), Some(source));
    assert_eq!(store.saved_file(&target(None)).unwrap(), Some(saved));
    assert_eq!(store.load_settings().unwrap(), settings);
}

#[test]
fn stored_secrets_are_redacted_and_corrupt_descriptors_are_not_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let source = link();
    let id = source.source_id().unwrap();
    store.save_source(&id, &source).unwrap();
    store.upsert_media(&media()).unwrap();
    store.record_opened(&media().id, &id, 1).unwrap();
    let settings = Settings {
        cache_directory: Some(dir.path().join("private-cache-path")),
        ..Settings::default()
    };
    let saved = SavedFile {
        target: target(None),
        path: dir.path().join("private-copy-path"),
        saved_at: 0,
    };
    let output = format!(
        "{source:?} {:?} {:?} {:?} {settings:?} {saved:?}",
        magnet(Some(9)),
        media(),
        store.library(10).unwrap()
    );
    for secret in [
        "private.example",
        "secret",
        "private-user-title",
        "poster-token",
        "private-cache-path",
        "private-copy-path",
    ] {
        assert!(!output.contains(secret), "Debug leaked sensitive data");
    }
    let error = store
        .save_source(&SourceId::new("wrong-id").unwrap(), &source)
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("private.example"));
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE sources SET link = 'https://different.example/secret'",
        [],
    )
    .unwrap();
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert!(matches!(store.load_source(&id), Err(Error::CorruptData(_))));
    assert!(matches!(store.library(10), Err(Error::CorruptData(_))));
}

#[test]
fn corrupt_new_paths_metadata_and_selections_are_rejected_without_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let source = magnet(Some(1));
    let id = source.source_id().unwrap();
    store.save_source(&id, &source).unwrap();
    store.link_source(&target(None), &id).unwrap();
    store
        .save_progress_for_source(&progress(target(None), &source, 1), &source)
        .unwrap();
    store.upsert_media(&media()).unwrap();
    store
        .save_saved_file(&SavedFile {
            target: target(None),
            path: dir.path().join("copy.mp4"),
            saved_at: 1,
        })
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
        UPDATE target_sources SET file_id = -1;
        UPDATE playback_progress SET file_id = -1;
        UPDATE media_records SET year = 0;
        UPDATE saved_files SET path = X'0001';
        UPDATE settings SET cache_directory = X'0001';",
    )
    .unwrap();
    assert!(matches!(
        store.source_for_target(&target(None)),
        Err(Error::CorruptData(_))
    ));
    assert!(matches!(store.library(10), Err(Error::CorruptData(_))));
    assert!(matches!(
        store.media_record(&media().id),
        Err(Error::CorruptData(_))
    ));
    assert!(matches!(
        store.saved_file(&target(None)),
        Err(Error::CorruptData(_))
    ));
    assert!(matches!(store.load_settings(), Err(Error::CorruptData(_))));
    let year: i64 = raw
        .query_row("SELECT year FROM media_records", [], |row| row.get(0))
        .unwrap();
    assert_eq!(year, 0);
    let file_id: i64 = raw
        .query_row("SELECT file_id FROM playback_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(file_id, -1);
}

#[test]
fn invalid_new_fields_do_not_replace_valid_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let settings = Settings {
        cache_directory: Some(PathBuf::from("../secret")),
        ..Settings::default()
    };
    assert!(store.save_settings(&settings).is_err());
    assert_eq!(store.load_settings().unwrap(), Settings::default());
    let good = media();
    store.upsert_media(&good).unwrap();
    for bad in [
        MediaRecord {
            title: String::new(),
            ..good.clone()
        },
        MediaRecord {
            year: Some(0),
            ..good.clone()
        },
        MediaRecord {
            poster_url: Some("file:///private/secret".into()),
            ..good.clone()
        },
    ] {
        assert!(store.upsert_media(&bad).is_err());
        assert_eq!(store.media_record(&good.id).unwrap(), Some(good.clone()));
    }
    let invalid = SavedFile {
        target: target(None),
        path: PathBuf::from("relative.mp4"),
        saved_at: 0,
    };
    assert!(store.save_saved_file(&invalid).is_err());
    assert!(store.saved_files().unwrap().is_empty());
}

#[test]
fn v3_late_migration_failure_rolls_back_the_entire_new_database_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TABLE saved_files(sentinel TEXT);
        INSERT INTO saved_files VALUES ('preserve');",
    )
    .unwrap();
    drop(raw);
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    let raw = Connection::open(&path).unwrap();
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(tables, ["saved_files"]);
    assert_eq!(
        raw.query_row("SELECT sentinel FROM saved_files", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(SCHEMA_VERSION, 10);
}
