use kadr_storage::{
    EpisodeKey, Error, MediaId, PlaybackProgress, PlaybackTarget, PlayerPreference, ProgressOrigin,
    SCHEMA_VERSION, Settings, SourceId, SourcePreferences, Storage, TorznabConfig,
};
use rusqlite::Connection;

fn downgrade_to_v10(raw: &Connection) {
    raw.execute_batch(
        "ALTER TABLE settings DROP COLUMN sidebar_collapsed;
         ALTER TABLE source_preferences DROP COLUMN auto_discover;
         PRAGMA user_version = 10;",
    )
    .unwrap();
}

#[test]
fn ux_preferences_default_off_and_round_trip_independently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let mut settings = store.load_settings().unwrap();
    let mut preferences = store.load_source_preferences().unwrap();
    assert!(!settings.sidebar_collapsed);
    assert!(!preferences.auto_discover);
    assert!(!Settings::default().sidebar_collapsed);
    assert!(!SourcePreferences::default().auto_discover);

    for (sidebar, discovery) in [(true, false), (true, true), (false, true), (false, false)] {
        settings.sidebar_collapsed = sidebar;
        preferences.auto_discover = discovery;
        store.save_settings(&settings).unwrap();
        store.save_source_preferences(&preferences).unwrap();
        drop(store);
        store = Storage::open(&path).unwrap();
        assert_eq!(store.load_settings().unwrap(), settings);
        assert_eq!(store.load_source_preferences().unwrap(), preferences);
    }
}

#[test]
fn automatic_discovery_consent_never_enables_providers_or_global_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let settings = Settings {
        catalog_enabled: false,
        ..Settings::default()
    };
    assert!(!settings.network.torrents_enabled);
    store.save_settings(&settings).unwrap();
    let mut preferences = SourcePreferences {
        rutracker_enabled: false,
        commons_enabled: false,
        popcorn_enabled: false,
        torznab: vec![TorznabConfig {
            name: "Disabled indexer".into(),
            base_url: "http://localhost:9117/api".into(),
            api_key_env: None,
            enabled: false,
        }],
        ..SourcePreferences::default()
    };
    store.save_source_preferences(&preferences).unwrap();
    preferences.auto_discover = true;
    store.save_source_preferences(&preferences).unwrap();
    drop(store);

    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_source_preferences().unwrap(), preferences);
    assert_eq!(store.load_settings().unwrap(), settings);
}

#[test]
fn changing_auto_discover_follows_existing_cache_invalidation_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let mut preferences = store.load_source_preferences().unwrap();
    for auto_discover in [true, false] {
        store
            .source_cache_put("media:1", "provider:1", 100, false, b"result")
            .unwrap();
        let settings = Settings {
            sidebar_collapsed: auto_discover,
            ..store.load_settings().unwrap()
        };
        store.save_settings(&settings).unwrap();
        store.save_source_preferences(&preferences).unwrap();
        assert_eq!(
            store
                .source_cache_get("media:1", "provider:1", 101)
                .unwrap(),
            Some(b"result".to_vec())
        );
        preferences.auto_discover = auto_discover;
        store.save_source_preferences(&preferences).unwrap();
        assert_eq!(
            store
                .source_cache_get("media:1", "provider:1", 101)
                .unwrap(),
            None
        );
        assert_eq!(store.load_settings().unwrap(), settings);
        assert_eq!(store.load_source_preferences().unwrap(), preferences);
    }
}

#[test]
fn v10_upgrade_preserves_provider_consent_settings_cache_and_measured_progress() {
    for rutracker_enabled in [false, true] {
        for commons_enabled in [false, true] {
            for popcorn_enabled in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("db");
                let mut store = Storage::open(&path).unwrap();
                let mut settings = Settings {
                    default_player: PlayerPreference::Vlc,
                    catalog_enabled: false,
                    catalog_proxy: Some("http://127.0.0.1:8080".into()),
                    ..Settings::default()
                };
                settings.network.torrents_enabled = true;
                let preferences = SourcePreferences {
                    rutracker_enabled,
                    commons_enabled,
                    popcorn_enabled,
                    torznab: vec![TorznabConfig {
                        name: "Local indexer".into(),
                        base_url: "http://localhost:9117/api".into(),
                        api_key_env: Some("KADR_TORZNAB_LOCAL_KEY".into()),
                        enabled: true,
                    }],
                    preferred_resolution: Some("1080p".into()),
                    preferred_language: Some("ru".into()),
                    ..SourcePreferences::default()
                };
                let progress = PlaybackProgress {
                    target: PlaybackTarget {
                        media_id: MediaId::new("series:1").unwrap(),
                        episode: Some(EpisodeKey::new(1, 2).unwrap()),
                    },
                    source_id: SourceId::new("source:1").unwrap(),
                    position_seconds: 42.0,
                    duration_seconds: Some(100.0),
                    updated_at: 100,
                    completed: false,
                    origin: ProgressOrigin::Mpv,
                };
                store.save_settings(&settings).unwrap();
                store.save_source_preferences(&preferences).unwrap();
                store.set_favorite(&progress.target.media_id, true).unwrap();
                store.save_progress(&progress).unwrap();
                store
                    .source_cache_put("media:1", "provider:1", 100, false, b"v10 result")
                    .unwrap();
                drop(store);
                let raw = Connection::open(&path).unwrap();
                downgrade_to_v10(&raw);
                drop(raw);

                for _ in 0..2 {
                    let store = Storage::open(&path).unwrap();
                    assert_eq!(store.load_settings().unwrap(), settings);
                    assert_eq!(store.load_source_preferences().unwrap(), preferences);
                    assert_eq!(
                        store.load_progress(&progress.target).unwrap(),
                        Some(progress.clone())
                    );
                    assert!(
                        store
                            .library_entry(&progress.target.media_id)
                            .unwrap()
                            .unwrap()
                            .favorite
                    );
                    assert_eq!(
                        store
                            .source_cache_get("media:1", "provider:1", 101)
                            .unwrap(),
                        Some(b"v10 result".to_vec())
                    );
                }
                let raw = Connection::open(&path).unwrap();
                assert_eq!(
                    raw.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                        .unwrap(),
                    SCHEMA_VERSION
                );
            }
        }
    }
}

#[test]
fn v11_second_column_collision_rolls_back_both_schema_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    drop(Storage::open(&path).unwrap());
    let raw = Connection::open(&path).unwrap();
    downgrade_to_v10(&raw);
    raw.execute_batch("ALTER TABLE source_preferences ADD COLUMN auto_discover TEXT;")
        .unwrap();
    drop(raw);
    let before = std::fs::read(&path).unwrap();

    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        raw.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        10
    );
    let sidebar_columns: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('settings') WHERE name = 'sidebar_collapsed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sidebar_columns, 0);
}

#[test]
fn ux_boolean_constraints_and_corruption_are_not_silently_coerced() {
    for (table, column) in [
        ("settings", "sidebar_collapsed"),
        ("source_preferences", "auto_discover"),
    ] {
        for invalid in ["-1", "2", "NULL"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db");
            drop(Storage::open(&path).unwrap());
            let raw = Connection::open(&path).unwrap();
            assert!(
                raw.execute_batch(&format!("UPDATE {table} SET {column} = {invalid};"))
                    .is_err()
            );
            if invalid == "NULL" {
                continue;
            }
            raw.execute_batch(&format!(
                "PRAGMA ignore_check_constraints = ON;
                 UPDATE {table} SET {column} = {invalid};"
            ))
            .unwrap();
            drop(raw);
            let store = Storage::open(&path).unwrap();
            if table == "settings" {
                assert!(matches!(store.load_settings(), Err(Error::CorruptData(_))));
            } else {
                assert!(matches!(
                    store.load_source_preferences(),
                    Err(Error::CorruptData(_))
                ));
            }
            let raw = Connection::open(&path).unwrap();
            assert_eq!(
                raw.query_row(&format!("SELECT {column} FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
                invalid.parse::<i64>().unwrap()
            );
        }
    }
}
