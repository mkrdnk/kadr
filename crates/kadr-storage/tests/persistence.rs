use std::fs;

use kadr_storage::{
    AppPaths, EpisodeKey, Error, MediaId, PlaybackProgress, PlaybackTarget, PlayerPreference,
    ProgressOrigin, RateLimit, SCHEMA_VERSION, Settings, SourceId, Storage,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn database() -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.sqlite3");
    (dir, path)
}

fn media() -> MediaId {
    MediaId::new("local:stable-item-1").unwrap()
}

fn source() -> SourceId {
    SourceId::new("source:opaque-1").unwrap()
}

fn target(episode: Option<EpisodeKey>) -> PlaybackTarget {
    PlaybackTarget {
        media_id: media(),
        episode,
    }
}

fn progress(episode: Option<EpisodeKey>) -> PlaybackProgress {
    PlaybackProgress {
        target: target(episode),
        source_id: source(),
        position_seconds: 15.5,
        duration_seconds: Some(100.0),
        updated_at: 1000,
        completed: false,
        origin: ProgressOrigin::Mpv,
    }
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn defaults_and_settings_persist_across_reopen() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), Settings::default());
    let mut settings = Settings {
        default_player: PlayerPreference::Vlc,
        ..Settings::default()
    };
    settings.network.torrents_enabled = true;
    settings.network.download = RateLimit::Unlimited;
    settings.network.upload = RateLimit::BytesPerSecond(128 * 1024);
    settings.network.max_connections = 128;
    settings.cache_limit_bytes = 2 * 1024 * 1024 * 1024;
    store.save_settings(&settings).unwrap();
    drop(store);
    let mut store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), settings);
    settings.default_player = PlayerPreference::Browser;
    settings.network.download = RateLimit::Paused;
    store.save_settings(&settings).unwrap();
    drop(store);
    assert_eq!(
        Storage::open(&path).unwrap().load_settings().unwrap(),
        settings
    );
    assert_eq!(
        schema_version(&Connection::open(&path).unwrap()),
        SCHEMA_VERSION
    );
}

#[test]
fn defaults_are_explicit_private_and_bounded() {
    let settings = Settings::default();
    assert_eq!(settings.default_player, PlayerPreference::Mpv);
    assert!(!settings.network.torrents_enabled);
    assert!(matches!(
        settings.network.download,
        RateLimit::BytesPerSecond(_)
    ));
    assert_eq!(settings.network.upload, RateLimit::Paused);
    settings.validate().unwrap();
}

#[test]
fn invalid_settings_do_not_replace_previous_values() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let good = Settings::default();
    let mut invalid = Vec::new();
    for rate in [
        RateLimit::BytesPerSecond(0),
        RateLimit::BytesPerSecond(u64::MAX),
    ] {
        let mut settings = good.clone();
        settings.network.download = rate;
        invalid.push(settings);
        let mut settings = good.clone();
        settings.network.upload = rate;
        invalid.push(settings);
    }
    for limit in [0, 4097, u16::MAX] {
        let mut settings = good.clone();
        settings.network.max_connections = limit;
        invalid.push(settings);
    }
    for limit in [0, 1024 * 1024 - 1, i64::MAX as u64 + 1, u64::MAX] {
        let mut settings = good.clone();
        settings.cache_limit_bytes = limit;
        invalid.push(settings);
    }
    for settings in invalid {
        assert!(store.save_settings(&settings).is_err());
        assert_eq!(store.load_settings().unwrap(), good);
    }
}

#[test]
fn stored_settings_are_validated_and_never_silently_reset() {
    let (_dir, path) = database();
    drop(Storage::open(&path).unwrap());
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE settings SET download_kind = 'limited', download_bytes = 0;",
    )
    .unwrap();
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert!(matches!(store.load_settings(), Err(Error::CorruptData(_))));
    drop(store);
    let raw = Connection::open(&path).unwrap();
    let bytes: i64 = raw
        .query_row("SELECT download_bytes FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(bytes, 0);
}

#[test]
fn migration_failure_rolls_back_whole_chain_and_preserves_data() {
    let (_dir, path) = database();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TABLE playback_progress (sentinel TEXT);
         INSERT INTO playback_progress VALUES ('preserve-me');",
    )
    .unwrap();
    drop(raw);
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    let raw = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&raw), 0);
    let tables: Vec<String> = raw
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(tables, ["playback_progress"]);
    let value: String = raw
        .query_row("SELECT sentinel FROM playback_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(value, "preserve-me");
}

#[test]
fn version_one_upgrades_without_replacing_existing_settings() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let settings = Settings {
        default_player: PlayerPreference::Vlc,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    drop(store);
    // Reconstruct a genuine v1 schema by removing the v2 tables.
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE playback_progress; DROP TABLE library; PRAGMA user_version = 1;")
        .unwrap();
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), settings);
    assert!(store.history(10).unwrap().is_empty());
    drop(store);
    assert_eq!(
        schema_version(&Connection::open(&path).unwrap()),
        SCHEMA_VERSION
    );
}

#[test]
fn failed_version_one_upgrade_preserves_settings_and_schema_version() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let settings = Settings {
        default_player: PlayerPreference::Browser,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE playback_progress; DROP TABLE library;
         CREATE TABLE playback_progress (sentinel INTEGER);
         INSERT INTO playback_progress VALUES (42);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    drop(raw);
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    let raw = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&raw), 1);
    let player: String = raw
        .query_row("SELECT player FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(player, "browser");
    let library_count: i64 = raw
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'library'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(library_count, 0);
}

#[test]
fn future_schema_is_refused_without_changing_any_database_bytes() {
    let (_dir, path) = database();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TABLE future_data (payload TEXT);
         INSERT INTO future_data VALUES ('do-not-touch');
         PRAGMA user_version = 999;",
    )
    .unwrap();
    drop(raw);
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        Storage::open(&path),
        Err(Error::FutureSchema {
            found: 999,
            supported: SCHEMA_VERSION
        })
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    let raw = Connection::open(&path).unwrap();
    assert_eq!(schema_version(&raw), 999);
    let payload: String = raw
        .query_row("SELECT payload FROM future_data", [], |row| row.get(0))
        .unwrap();
    assert_eq!(payload, "do-not-touch");
}

#[test]
fn negative_schema_version_is_not_treated_as_empty_database() {
    let (_dir, path) = database();
    let raw = Connection::open(&path).unwrap();
    raw.pragma_update(None, "user_version", -1).unwrap();
    drop(raw);
    assert!(matches!(
        Storage::open(&path),
        Err(Error::InvalidSchemaVersion)
    ));
}

#[test]
fn favorite_and_history_updates_are_independent_and_persistent() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let other = MediaId::new("provider:item2").unwrap();
    store.set_favorite(&media(), true).unwrap();
    assert!(store.history(10).unwrap().is_empty());
    store.record_opened(&media(), &source(), 100).unwrap();
    store.record_opened(&other, &source(), 200).unwrap();
    store
        .record_opened(&media(), &SourceId::new("older-source").unwrap(), 99)
        .unwrap();
    assert_eq!(store.history(1).unwrap()[0].media_id, other);
    assert_eq!(store.favorites().unwrap()[0].media_id, media());
    assert!(store.load_progress(&target(None)).unwrap().is_none());
    drop(store);
    let mut store = Storage::open(&path).unwrap();
    let entry = store.library_entry(&media()).unwrap().unwrap();
    assert!(entry.favorite);
    assert_eq!(entry.last_opened_at, Some(100));
    assert_eq!(entry.last_source_id, Some(source()));
    store.set_favorite(&media(), false).unwrap();
    assert!(store.favorites().unwrap().is_empty());
    assert_eq!(store.history(10).unwrap().len(), 2);
}

#[test]
fn progress_is_isolated_by_media_season_episode_and_item() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let keys = [
        None,
        Some(EpisodeKey::new(0, 1).unwrap()),
        Some(EpisodeKey::new(1, 1).unwrap()),
        Some(EpisodeKey::new(1, 2).unwrap()),
        Some(EpisodeKey::new(2, 1).unwrap()),
    ];
    for (index, episode) in keys.iter().enumerate() {
        let mut observation = progress(*episode);
        observation.position_seconds = index as f64;
        store.save_progress(&observation).unwrap();
    }
    let mut other = progress(None);
    other.target.media_id = MediaId::new("another-series").unwrap();
    other.origin = ProgressOrigin::Vlc;
    store.save_progress(&other).unwrap();
    assert!(store.history(10).unwrap().is_empty());
    drop(store);
    let store = Storage::open(&path).unwrap();
    for (index, episode) in keys.iter().enumerate() {
        let observation = store.load_progress(&target(*episode)).unwrap().unwrap();
        assert_eq!(observation.position_seconds, index as f64);
        assert_eq!(observation.origin, ProgressOrigin::Mpv);
    }
    assert_eq!(store.load_progress(&other.target).unwrap(), Some(other));
    assert!(
        store
            .load_progress(&target(Some(EpisodeKey::new(3, 1).unwrap())))
            .unwrap()
            .is_none()
    );
}

#[test]
fn latest_progress_wins_and_unknown_duration_is_preserved() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let mut observation = progress(None);
    observation.duration_seconds = None;
    store.save_progress(&observation).unwrap();
    let mut stale = observation.clone();
    stale.position_seconds = 1.0;
    stale.updated_at -= 1;
    store.save_progress(&stale).unwrap();
    assert_eq!(
        store.load_progress(&observation.target).unwrap(),
        Some(observation.clone())
    );
    observation.updated_at += 1;
    observation.duration_seconds = Some(100.0);
    observation.position_seconds = 100.0;
    observation.completed = true;
    store.save_progress(&observation).unwrap();
    assert_eq!(
        store.load_progress(&observation.target).unwrap(),
        Some(observation)
    );
}

#[test]
fn invalid_progress_is_rejected_without_mutating_existing_progress() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    let good = progress(None);
    store.save_progress(&good).unwrap();
    let mut invalid = Vec::new();
    for position in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 101.0] {
        let mut value = good.clone();
        value.position_seconds = position;
        invalid.push(value);
    }
    for duration in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0, 1.0] {
        let mut value = good.clone();
        value.duration_seconds = Some(duration);
        invalid.push(value);
    }
    let mut value = good.clone();
    value.updated_at = -1;
    invalid.push(value);
    let mut value = good.clone();
    value.target.episode = Some(EpisodeKey {
        season: 1,
        episode: 0,
    });
    invalid.push(value);
    for value in invalid {
        assert!(store.save_progress(&value).is_err());
        assert_eq!(
            store.load_progress(&good.target).unwrap(),
            Some(good.clone())
        );
    }
    assert!(EpisodeKey::new(1, 0).is_err());
    assert!(store.history(0).is_err());
    assert!(store.history(1001).is_err());
    assert!(store.record_opened(&media(), &source(), -1).is_err());
}

#[test]
fn browser_open_is_history_only_and_telemetry_is_rejected() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    store.record_opened(&media(), &source(), 1000).unwrap();
    let mut observation = progress(None);
    observation.origin = ProgressOrigin::Browser;
    assert!(store.save_progress(&observation).is_err());
    assert!(store.load_progress(&target(None)).unwrap().is_none());
    assert_eq!(store.history(10).unwrap().len(), 1);
}

#[test]
fn malformed_persisted_progress_is_not_exposed_as_valid_telemetry() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    store.save_progress(&progress(None)).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    // Positive infinity passes SQLite's nonnegative check, but not Rust's
    // finite-value validation. Exercise the read boundary too.
    raw.execute(
        "UPDATE playback_progress SET position_seconds = ?1, duration_seconds = NULL",
        [f64::INFINITY],
    )
    .unwrap();
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert!(matches!(
        store.load_progress(&target(None)),
        Err(Error::CorruptData(_))
    ));
}

#[test]
fn deleting_playback_data_preserves_favorites_and_settings() {
    let (_dir, path) = database();
    let mut store = Storage::open(&path).unwrap();
    store.set_favorite(&media(), true).unwrap();
    store.record_opened(&media(), &source(), 100).unwrap();
    store.save_progress(&progress(None)).unwrap();
    let other = MediaId::new("not-favorite").unwrap();
    store.record_opened(&other, &source(), 200).unwrap();
    store.clear_playback_data().unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    assert!(store.history(10).unwrap().is_empty());
    assert!(store.load_progress(&target(None)).unwrap().is_none());
    assert!(store.library_entry(&other).unwrap().is_none());
    assert_eq!(store.favorites().unwrap().len(), 1);
    assert_eq!(
        store
            .library_entry(&media())
            .unwrap()
            .unwrap()
            .last_source_id,
        None
    );
    assert_eq!(store.load_settings().unwrap(), Settings::default());
}

#[test]
fn opaque_ids_reject_urls_paths_and_unbounded_or_empty_values() {
    for id in [
        "",
        "https://private.example/video?token=secret",
        "/private/user/video",
        "../escape",
        "contains space",
        "control\n",
        "юникод",
        &"a".repeat(257),
    ] {
        assert!(MediaId::new(id).is_err());
        assert!(SourceId::new(id).is_err());
    }
    assert!(MediaId::new("catalog:movie_123-v2.0").is_ok());
}

#[test]
fn debug_and_errors_do_not_leak_private_paths_urls_or_ids() {
    let dir = tempfile::tempdir().unwrap();
    let private = dir.path().join("secret-user-token");
    let paths = AppPaths::from_roots(&private, &private, &private).unwrap();
    let store = Storage::open_default(&paths).unwrap();
    let id = MediaId::new("private-opaque-id").unwrap();
    let output = format!("{paths:?} {store:?} {id:?} {:?}", progress(None));
    for secret in [
        "secret-user-token",
        "private-opaque-id",
        "local:stable-item-1",
    ] {
        assert!(!output.contains(secret));
    }
    let error = Storage::open(&dir.path().join("private-missing-parent").join("db")).unwrap_err();
    let url_error = SourceId::new("https://private.example/?token=secret").unwrap_err();
    let output = format!("{error} {error:?} {url_error} {url_error:?}");
    assert!(!output.contains("private-missing-parent"));
    assert!(!output.contains("private.example"));
    assert!(!output.contains("token=secret"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn custom_directories_are_validated_and_created_only_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let config = dir.path().join("config");
    let cache = dir.path().join("cache");
    let paths = AppPaths::from_roots(&data, &config, &cache).unwrap();
    assert!(!data.exists());
    assert_eq!(paths.data_dir(), data);
    assert_eq!(paths.config_dir(), config);
    assert_eq!(paths.cache_dir(), cache);
    drop(Storage::open_default(&paths).unwrap());
    assert!(paths.database_path().is_file());
    assert!(config.is_dir());
    assert!(cache.is_dir());
    assert!(AppPaths::from_roots("relative", &config, &cache).is_err());
    assert!(AppPaths::from_roots(data.join("../escape"), &config, &cache).is_err());
    assert!(Storage::open(std::path::Path::new("relative.sqlite")).is_err());
}

#[cfg(unix)]
#[test]
fn new_files_and_directories_have_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_roots(
        dir.path().join("data"),
        dir.path().join("config"),
        dir.path().join("cache"),
    )
    .unwrap();
    drop(Storage::open_default(&paths).unwrap());
    for path in [paths.data_dir(), paths.config_dir(), paths.cache_dir()] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    assert_eq!(
        fs::metadata(paths.database_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[cfg(target_os = "linux")]
#[test]
fn discovered_linux_paths_follow_xdg_conventions() {
    // Do not mutate process environment: tests may run concurrently.
    let paths = AppPaths::discover().unwrap();
    for (variable, fallback, actual) in [
        ("XDG_DATA_HOME", ".local/share", paths.data_dir()),
        ("XDG_CONFIG_HOME", ".config", paths.config_dir()),
        ("XDG_CACHE_HOME", ".cache", paths.cache_dir()),
    ] {
        let base = std::env::var_os(variable)
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(fallback)
            });
        assert_eq!(actual, base.join("kadr"));
    }
}
