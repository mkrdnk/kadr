use kadr_storage::{
    EpisodeKey, Error, MediaId, PlaybackProgress, PlaybackTarget, ProgressOrigin, SCHEMA_VERSION,
    Settings, Storage, StoredSource,
};
use rusqlite::Connection;

fn target(episode: Option<u16>) -> PlaybackTarget {
    PlaybackTarget {
        media_id: MediaId::new("series:1").unwrap(),
        episode: episode.map(|episode| EpisodeKey::new(1, episode).unwrap()),
    }
}

fn descriptor() -> StoredSource {
    StoredSource::Link("https://private.example/video?token=secret".into())
}

fn version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn version_three_migrates_only_recorded_media_opens_to_item_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let source = descriptor();
    let id = source.source_id().unwrap();
    store.save_source(&id, &source).unwrap();
    store
        .record_opened(&target(None).media_id, &id, 123)
        .unwrap();
    let progress = PlaybackProgress {
        target: target(Some(2)),
        source_id: id.clone(),
        position_seconds: 42.0,
        duration_seconds: None,
        updated_at: 999,
        completed: false,
        origin: ProgressOrigin::Mpv,
    };
    store.save_progress_for_source(&progress, &source).unwrap();
    let settings = Settings {
        catalog_enabled: false,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE target_history; PRAGMA user_version = 3;")
        .unwrap();
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), settings);
    assert_eq!(store.load_source(&id).unwrap(), Some(source.clone()));
    let item = store.library_target(&target(None)).unwrap().unwrap();
    assert_eq!(item.opened_at, Some(123));
    assert_eq!(item.source, Some(source));
    let episode = store.library_target(&target(Some(2))).unwrap().unwrap();
    assert_eq!(episode.progress, Some(progress));
    assert_eq!(
        episode.opened_at, None,
        "progress must not invent an episode open"
    );
    assert_eq!(store.library(10).unwrap()[0].opened_at, None);
    assert_eq!(version(&Connection::open(&path).unwrap()), SCHEMA_VERSION);
}

#[test]
fn failed_v3_upgrade_preserves_all_prior_rows_version_and_database_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let id = descriptor().source_id().unwrap();
    store.save_source(&id, &descriptor()).unwrap();
    store
        .record_opened(&target(None).media_id, &id, 123)
        .unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE target_history; PRAGMA user_version = 3;
         CREATE TABLE target_history(sentinel TEXT);
         INSERT INTO target_history VALUES ('preserve');",
    )
    .unwrap();
    drop(raw);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let raw = Connection::open(&path).unwrap();
    assert_eq!(version(&raw), 3);
    assert_eq!(
        raw.query_row("SELECT sentinel FROM target_history", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
    assert_eq!(
        raw.query_row("SELECT last_opened_at FROM library", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        123
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM sources", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn future_v5_is_refused_without_erasing_history_or_touching_database_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let id = descriptor().source_id().unwrap();
    store
        .record_opened_target(&target(Some(1)), &id, 123)
        .unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.pragma_update(None, "user_version", 5).unwrap();
    drop(raw);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Storage::open(&path),
        Err(Error::FutureSchema {
            found: 5,
            supported: 4
        })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn corrupt_target_history_is_rejected_not_silently_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let id = descriptor().source_id().unwrap();
    store
        .record_opened_target(&target(Some(1)), &id, 123)
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
        UPDATE target_history SET opened_at = -1;",
    )
    .unwrap();
    assert!(matches!(
        store.library_target(&target(Some(1))),
        Err(Error::CorruptData(_))
    ));
    assert!(matches!(
        store.library_targets(10),
        Err(Error::CorruptData(_))
    ));
    assert_eq!(
        raw.query_row("SELECT opened_at FROM target_history", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        -1
    );
}
