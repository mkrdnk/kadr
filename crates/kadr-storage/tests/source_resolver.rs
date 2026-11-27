use kadr_storage::{
    Error, MediaId, SCHEMA_VERSION, Settings, SourcePreferences, Storage, TorznabConfig,
};
use rusqlite::{Connection, params};

fn config(endpoint: &str) -> TorznabConfig {
    TorznabConfig {
        name: "Private indexer".into(),
        base_url: endpoint.into(),
        api_key_env: Some("KADR_TORZNAB_PRIVATE_KEY".into()),
        enabled: true,
    }
}

fn preferences() -> SourcePreferences {
    SourcePreferences {
        rutracker_enabled: false,
        commons_enabled: false,
        torznab: vec![
            config("https://private.example/indexer/api"),
            TorznabConfig {
                enabled: false,
                api_key_env: None,
                ..config("http://[::1]:9117/api/v2")
            },
        ],
        preferred_resolution: Some("1080p".into()),
        preferred_language: Some("ru".into()),
    }
}

fn downgrade_to_v6(raw: &Connection) {
    raw.execute_batch(
        "DROP TABLE source_cache;
         DROP TABLE torznab_configs;
         DROP TABLE source_preferences;
         PRAGMA user_version = 6;",
    )
    .unwrap();
}

fn downgrade_to_v7(raw: &Connection) {
    raw.execute_batch(
        "ALTER TABLE source_preferences DROP COLUMN commons_enabled;
         PRAGMA user_version = 7;",
    )
    .unwrap();
}

#[test]
fn clean_install_commons_opt_out_and_opt_in_survive_restart_independently() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let mut expected = store.load_source_preferences().unwrap();
    assert!(expected.commons_enabled);
    let settings = store.load_settings().unwrap();
    for enabled in [false, true, false] {
        expected.commons_enabled = enabled;
        store.save_source_preferences(&expected).unwrap();
        drop(store);
        store = Storage::open(&path).unwrap();
        assert_eq!(store.load_source_preferences().unwrap(), expected);
        assert_eq!(store.load_settings().unwrap(), settings);
        let raw = Connection::open(&path).unwrap();
        assert_eq!(
            raw.query_row(
                "SELECT commons_enabled FROM source_preferences",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            i64::from(enabled)
        );
    }
}

#[test]
fn v7_upgrade_keeps_existing_preferences_settings_and_cache_without_commons_consent() {
    // Neither catalog nor BitTorrent/RuTracker opt-in implies Commons consent.
    for catalog_enabled in [false, true] {
        for torrents_enabled in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db");
            let mut store = Storage::open(&path).unwrap();
            let mut settings = Settings {
                catalog_enabled,
                ..Settings::default()
            };
            settings.network.torrents_enabled = torrents_enabled;
            store.save_settings(&settings).unwrap();
            let mut expected = preferences();
            expected.rutracker_enabled = torrents_enabled;
            store.save_source_preferences(&expected).unwrap();
            store
                .source_cache_put("media:1", "provider:1", 100, false, b"v7 result")
                .unwrap();
            let media = MediaId::new("media:favorite").unwrap();
            store.set_favorite(&media, true).unwrap();
            drop(store);
            let raw = Connection::open(&path).unwrap();
            downgrade_to_v7(&raw);
            drop(raw);

            let mut store = Storage::open(&path).unwrap();
            assert_eq!(store.load_source_preferences().unwrap(), expected);
            assert!(!store.load_source_preferences().unwrap().commons_enabled);
            assert_eq!(store.load_settings().unwrap(), settings);
            assert!(store.library_entry(&media).unwrap().unwrap().favorite);
            assert_eq!(
                store
                    .source_cache_get("media:1", "provider:1", 101)
                    .unwrap(),
                Some(b"v7 result".to_vec())
            );
            drop(store);
            store = Storage::open(&path).unwrap();
            assert_eq!(store.load_source_preferences().unwrap(), expected);
            let raw = Connection::open(&path).unwrap();
            assert_eq!(
                raw.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                8
            );
            drop(raw);
            for enabled in [true, false] {
                expected.commons_enabled = enabled;
                store.save_source_preferences(&expected).unwrap();
                drop(store);
                store = Storage::open(&path).unwrap();
                assert_eq!(store.load_source_preferences().unwrap(), expected);
            }
        }
    }
}

#[test]
fn commons_boolean_is_strict_and_corruption_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    for value in ["2", "-1", "NULL", "'true'"] {
        assert!(
            raw.execute_batch(&format!(
                "UPDATE source_preferences SET commons_enabled = {value};"
            ))
            .is_err()
        );
    }
    raw.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE source_preferences SET commons_enabled = 2;",
    )
    .unwrap();
    assert_eq!(
        store.load_source_preferences(),
        Err(Error::CorruptData("source preferences"))
    );
    assert!(
        store
            .save_source_preferences(&SourcePreferences::default())
            .is_err()
    );
}

#[test]
fn v8_column_collision_preserves_v7_data_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.save_source_preferences(&preferences()).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    downgrade_to_v7(&raw);
    raw.execute_batch(
        "ALTER TABLE source_preferences ADD COLUMN commons_enabled TEXT;
         UPDATE source_preferences SET commons_enabled = 'preserve';",
    )
    .unwrap();
    drop(raw);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let raw = Connection::open(&path).unwrap();
    assert_eq!(
        raw.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    assert_eq!(
        raw.query_row(
            "SELECT commons_enabled FROM source_preferences",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "preserve"
    );
}

#[test]
fn preferences_defaults_round_trip_and_do_not_change_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    assert_eq!(
        store.load_source_preferences().unwrap(),
        SourcePreferences::default()
    );
    assert!(SourcePreferences::default().rutracker_enabled);
    assert!(SourcePreferences::default().commons_enabled);
    let settings = Settings {
        catalog_enabled: false,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    store.save_source_preferences(&preferences()).unwrap();
    drop(store);
    let mut store = Storage::open(&path).unwrap();
    assert_eq!(store.load_source_preferences().unwrap(), preferences());
    assert_eq!(store.load_settings().unwrap(), settings);
    store
        .save_source_preferences(&SourcePreferences::default())
        .unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    assert_eq!(
        store.load_source_preferences().unwrap(),
        SourcePreferences::default()
    );
    assert_eq!(store.load_settings().unwrap(), settings);
}

#[test]
fn only_environment_names_are_stored_and_private_config_is_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let prefs = preferences();
    store.save_source_preferences(&prefs).unwrap();
    let raw = Connection::open(&path).unwrap();
    let name: String = raw
        .query_row(
            "SELECT api_key_env FROM torznab_configs WHERE position = 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "KADR_TORZNAB_PRIVATE_KEY");
    let debug = format!("{prefs:?} {:?}", prefs.torznab[0]);
    for private in [
        "Private indexer",
        "private.example",
        "KADR_TORZNAB_PRIVATE_KEY",
        "1080p",
    ] {
        assert!(!debug.contains(private));
    }
}

#[test]
fn endpoint_policy_accepts_https_and_loopback_http_with_paths() {
    for endpoint in [
        "https://indexer.example",
        "https://INDEXER.example:443/api/",
        "https://indexer.example:8443/a%20b/api",
        "http://localhost:9117/api",
        "http://LOCALHOST./api",
        "http://127.0.0.1:9117/api",
        "http://127.42.0.2/api",
        "http://[::1]:9117/api",
    ] {
        assert!(config(endpoint).validate().is_ok(), "{endpoint}");
    }
}

#[test]
fn unsafe_endpoints_and_env_names_never_replace_preferences_or_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let good = preferences();
    store.save_source_preferences(&good).unwrap();
    store
        .source_cache_put("media:1", "provider:1", 0, false, b"good")
        .unwrap();
    for endpoint in [
        "",
        "ftp://private.example/api",
        "http://private.example/api",
        "http://192.168.1.1/api",
        "http://localhost.evil.example/api",
        "http://localhost../api",
        "http://[::ffff:127.0.0.1]/api",
        "https://user:secret@private.example/api",
        "https://@private.example/api",
        "https://private.example/api?apikey=secret",
        "https://private.example/api?",
        "https://private.example/api#secret",
        "https://private.example/api#",
        "https://private.example:0/api",
        "https://private.example:/api",
        "https:///api",
        "https://private.example/api%zz",
        "https://private.example\\api",
        "https://private.example/api\n",
    ] {
        let invalid = SourcePreferences {
            torznab: vec![config(endpoint)],
            ..good.clone()
        };
        let err = store.save_source_preferences(&invalid).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{endpoint}");
        assert!(!format!("{err:?} {err} {invalid:?}").contains("private.example"));
        assert_eq!(store.load_source_preferences().unwrap(), good);
        assert_eq!(
            store.source_cache_get("media:1", "provider:1", 1).unwrap(),
            Some(b"good".to_vec())
        );
    }
    for name in [
        "",
        "API_KEY",
        "KADR_TORZNAB_",
        "KADR_TORZNAB_private",
        "KADR_TORZNAB_A-B",
        "KADR_TORZNAB_A=B",
        "KADR_TORZNAB_A\n",
        "KADR_TORZNAB_$SECRET",
    ] {
        let mut invalid = good.clone();
        invalid.torznab[0].api_key_env = Some(name.into());
        assert!(store.save_source_preferences(&invalid).is_err());
        assert_eq!(store.load_source_preferences().unwrap(), good);
    }
}

#[test]
fn duplicate_endpoint_aliases_are_rejected_even_when_disabled() {
    for (left, right) in [
        (
            "https://indexer.example/api",
            "https://INDEXER.example:443/api/",
        ),
        ("https://indexer.example", "https://indexer.example/"),
        (
            "https://indexer.example./api",
            "https://indexer.example/api",
        ),
        (
            "https://indexer.example/a/../api",
            "https://indexer.example/api",
        ),
        (
            "https://indexer.example/%61pi",
            "https://indexer.example/api",
        ),
        ("http://127.0.0.1/api", "http://127.1:80/api/"),
        ("http://[0:0:0:0:0:0:0:1]/api", "http://[::1]:80/api"),
    ] {
        let prefs = SourcePreferences {
            torznab: vec![
                config(left),
                TorznabConfig {
                    enabled: false,
                    ..config(right)
                },
            ],
            ..SourcePreferences::default()
        };
        assert!(
            matches!(
                prefs.validate(),
                Err(Error::InvalidInput("duplicate Torznab endpoint"))
            ),
            "{left} {right}"
        );
    }
}

#[test]
fn preference_size_and_label_bounds_are_validated() {
    let mut prefs = SourcePreferences {
        torznab: (0..16)
            .map(|n| config(&format!("https://indexer.example/{n}")))
            .collect(),
        ..SourcePreferences::default()
    };
    prefs.validate().unwrap();
    prefs.torznab.push(config("https://extra.example/api"));
    assert!(prefs.validate().is_err());
    for label in [
        "".to_owned(),
        "  ".into(),
        "private\nlabel".into(),
        "a".repeat(65),
    ] {
        assert!(
            SourcePreferences {
                preferred_resolution: Some(label.clone()),
                ..SourcePreferences::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            SourcePreferences {
                preferred_language: Some(label),
                ..SourcePreferences::default()
            }
            .validate()
            .is_err()
        );
    }
    for name in ["".to_owned(), " ".into(), "a\0b".into(), "x".repeat(129)] {
        assert!(
            TorznabConfig {
                name,
                ..config("https://indexer.example")
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        config(&format!("https://indexer.example/{}", "x".repeat(2048)))
            .validate()
            .is_err()
    );
    assert!(
        TorznabConfig {
            api_key_env: Some(format!("KADR_TORZNAB_{}", "A".repeat(128))),
            ..config("https://indexer.example")
        }
        .validate()
        .is_err()
    );
}

#[test]
fn config_changes_clear_cache_atomically_but_identical_saves_retain_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.save_source_preferences(&preferences()).unwrap();
    store
        .source_cache_put("media:1", "provider:1", 0, false, b"data")
        .unwrap();
    store.save_source_preferences(&preferences()).unwrap();
    assert!(
        store
            .source_cache_get("media:1", "provider:1", 1)
            .unwrap()
            .is_some()
    );

    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER fail_clear BEFORE DELETE ON source_cache BEGIN
             SELECT RAISE(ABORT, 'private trigger details');
         END;",
    )
    .unwrap();
    let err = store
        .save_source_preferences(&SourcePreferences::default())
        .unwrap_err();
    assert_eq!(err, Error::Database);
    assert_eq!(store.load_source_preferences().unwrap(), preferences());
    assert!(
        store
            .source_cache_get("media:1", "provider:1", 1)
            .unwrap()
            .is_some()
    );
    raw.execute_batch("DROP TRIGGER fail_clear").unwrap();
    store
        .save_source_preferences(&SourcePreferences::default())
        .unwrap();
    assert_eq!(
        store.source_cache_get("media:1", "provider:1", 1).unwrap(),
        None
    );
}

#[test]
fn every_preference_field_change_invalidates_cache() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let good = preferences();
    let mut changes = Vec::new();
    let mut changed = good.clone();
    changed.commons_enabled = true;
    changes.push(changed);
    let mut changed = good.clone();
    changed.rutracker_enabled = true;
    changes.push(changed);
    let mut changed = good.clone();
    changed.preferred_language = Some("en".into());
    changes.push(changed);
    let mut changed = good.clone();
    changed.preferred_resolution = None;
    changes.push(changed);
    let mut changed = good.clone();
    changed.torznab[0].api_key_env = Some("KADR_TORZNAB_OTHER".into());
    changes.push(changed);
    let mut changed = good.clone();
    changed.torznab[0].base_url = "https://other.example/api".into();
    changes.push(changed);
    let mut changed = good.clone();
    changed.torznab[0].enabled = false;
    changes.push(changed);
    let mut changed = good.clone();
    changed.torznab[0].name = "Renamed".into();
    changes.push(changed);
    let mut changed = good.clone();
    changed.torznab.reverse();
    changes.push(changed);
    for changed in changes {
        store.save_source_preferences(&good).unwrap();
        store
            .source_cache_put("media:1", "provider:1", 0, false, b"data")
            .unwrap();
        store.save_source_preferences(&changed).unwrap();
        assert_eq!(
            store.source_cache_get("media:1", "provider:1", 1).unwrap(),
            None
        );
    }
}

#[test]
fn corrupt_preferences_are_rejected_without_reset_or_disclosure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.save_source_preferences(&preferences()).unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE torznab_configs SET api_key_env = 'secret-value' WHERE position = 0",
        [],
    )
    .unwrap();
    let err = store.load_source_preferences().unwrap_err();
    assert_eq!(err, Error::CorruptData("source preferences"));
    assert!(!format!("{err:?} {err}").contains("secret-value"));
    assert!(
        store
            .save_source_preferences(&SourcePreferences::default())
            .is_err()
    );
    let stored: String = raw
        .query_row(
            "SELECT api_key_env FROM torznab_configs WHERE position = 0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "secret-value");
}

#[test]
fn cache_ttls_are_exact_and_results_persist_with_provider_and_media_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store
        .source_cache_put("media:1", "provider:a", 100, false, &[0, 255, 1])
        .unwrap();
    store
        .source_cache_put("media:1", "provider:b", 100, true, b"[]")
        .unwrap();
    store
        .source_cache_put("media:2", "provider:a", 100, false, b"other")
        .unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    assert_eq!(
        store.source_cache_get("media:1", "provider:a", 99).unwrap(),
        None
    );
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:a", 100)
            .unwrap(),
        Some(vec![0, 255, 1])
    );
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:b", 399)
            .unwrap(),
        Some(b"[]".to_vec())
    );
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:b", 400)
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:a", 1899)
            .unwrap(),
        Some(vec![0, 255, 1])
    );
    assert_eq!(
        store
            .source_cache_get("media:2", "provider:a", 1899)
            .unwrap(),
        Some(b"other".to_vec())
    );
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:a", 1900)
            .unwrap(),
        None
    );
    let raw = Connection::open(&path).unwrap();
    let count: i64 = raw
        .query_row("SELECT count(*) FROM source_cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn cache_upsert_replaces_payload_and_ttl_and_put_cleans_unrelated_expired_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store
        .source_cache_put("media:1", "provider:a", 0, false, b"old")
        .unwrap();
    store
        .source_cache_put("media:1", "provider:a", 10, true, b"[]")
        .unwrap();
    assert_eq!(
        store
            .source_cache_get("media:1", "provider:a", 309)
            .unwrap(),
        Some(b"[]".to_vec())
    );
    store
        .source_cache_put("media:2", "provider:b", 310, false, b"new")
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    let count: i64 = raw
        .query_row("SELECT count(*) FROM source_cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    store
        .source_cache_put("media:2", "provider:b", 320, false, b"")
        .unwrap();
    assert_eq!(
        store
            .source_cache_get("media:2", "provider:b", 321)
            .unwrap(),
        Some(vec![])
    );
}

#[test]
fn cache_rejects_invalid_input_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    store
        .source_cache_put("media:1", "provider:a", 0, false, b"good")
        .unwrap();
    for key in [
        "".to_owned(),
        "https://secret.example/".into(),
        "with space".into(),
        "x".repeat(257),
    ] {
        assert!(
            store
                .source_cache_put(&key, "provider:a", 0, false, b"bad")
                .is_err()
        );
        assert!(
            store
                .source_cache_put("media:1", &key, 0, false, b"bad")
                .is_err()
        );
        assert!(store.source_cache_get(&key, "provider:a", 0).is_err());
        assert!(store.source_cache_get("media:1", &key, 0).is_err());
    }
    assert!(
        store
            .source_cache_put("media:1", "provider:a", -1, false, b"bad")
            .is_err()
    );
    assert!(store.source_cache_get("media:1", "provider:a", -1).is_err());
    for empty in [false, true] {
        assert!(
            store
                .source_cache_put("media:1", "provider:a", i64::MAX, empty, b"bad")
                .is_err()
        );
    }
    assert!(
        store
            .source_cache_put("media:1", "provider:a", 0, false, &vec![0; 1024 * 1024 + 1])
            .is_err()
    );
    assert_eq!(
        store.source_cache_get("media:1", "provider:a", 1).unwrap(),
        Some(b"good".to_vec())
    );
}

#[test]
fn cache_count_cap_evicts_oldest_writes_and_reads_do_not_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    for n in 0..1024 {
        store
            .source_cache_put(&format!("media:{n}"), "provider:a", n, false, b"data")
            .unwrap();
    }
    assert!(
        store
            .source_cache_get("media:0", "provider:a", 1024)
            .unwrap()
            .is_some()
    );
    store
        .source_cache_put("media:new", "provider:a", 1024, false, b"data")
        .unwrap();
    assert_eq!(
        store
            .source_cache_get("media:0", "provider:a", 1024)
            .unwrap(),
        None
    );
    // Updating an existing row must not evict another row.
    store
        .source_cache_put("media:1", "provider:a", 1025, false, b"updated")
        .unwrap();
    store
        .source_cache_put("media:newer", "provider:a", 1026, false, b"data")
        .unwrap();
    assert!(
        store
            .source_cache_get("media:1", "provider:a", 1026)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .source_cache_get("media:2", "provider:a", 1026)
            .unwrap(),
        None
    );
    let raw = Connection::open(&path).unwrap();
    let count: i64 = raw
        .query_row("SELECT count(*) FROM source_cache", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1024);
}

#[test]
fn cache_byte_cap_is_separate_from_file_cache_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let settings = Settings {
        cache_limit_bytes: 1024 * 1024,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    let payload = vec![42; 1024 * 1024];
    for n in 0..17 {
        store
            .source_cache_put(&format!("media:{n}"), "provider:a", n, false, &payload)
            .unwrap();
    }
    assert_eq!(
        store.source_cache_get("media:0", "provider:a", 17).unwrap(),
        None
    );
    assert_eq!(
        store
            .source_cache_get("media:16", "provider:a", 17)
            .unwrap(),
        Some(payload)
    );
    let raw = Connection::open(&path).unwrap();
    let (count, bytes): (i64, i64) = raw
        .query_row(
            "SELECT count(*), sum(length(data)) FROM source_cache",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, bytes), (16, 16 * 1024 * 1024));
    assert_eq!(store.load_settings().unwrap(), settings);
}

#[test]
fn v6_upgrade_preserves_existing_data_and_initializes_only_resolver_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let settings = Settings {
        catalog_enabled: false,
        ..Settings::default()
    };
    store.save_settings(&settings).unwrap();
    let media = MediaId::new("media:favorite").unwrap();
    store.set_favorite(&media, true).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    downgrade_to_v6(&raw);
    drop(raw);
    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), settings);
    assert!(store.library_entry(&media).unwrap().unwrap().favorite);
    assert_eq!(
        store.load_source_preferences().unwrap(),
        SourcePreferences {
            commons_enabled: false,
            ..SourcePreferences::default()
        }
    );
    assert_eq!(
        store.source_cache_get("media:1", "provider:a", 0).unwrap(),
        None
    );
    let raw = Connection::open(&path).unwrap();
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn v7_late_collision_rolls_back_schema_data_and_version() {
    for old_version in [0, 6] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        if old_version == 6 {
            drop(Storage::open(&path).unwrap());
        }
        let raw = Connection::open(&path).unwrap();
        if old_version == 6 {
            downgrade_to_v6(&raw);
        }
        raw.execute_batch("CREATE TABLE source_cache(private_value TEXT) STRICT;")
            .unwrap();
        raw.execute(
            "INSERT INTO source_cache VALUES (?1)",
            params!["private sentinel"],
        )
        .unwrap();
        drop(raw);
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(Storage::open(&path), Err(Error::Database)));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let raw = Connection::open(&path).unwrap();
        let version: i64 = raw
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, old_version);
        let count: i64 = raw
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE name IN ('source_preferences', 'torznab_configs')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        let value: String = raw
            .query_row("SELECT private_value FROM source_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "private sentinel");
    }
}

#[test]
fn failed_cache_write_rolls_back_expiration_cleanup_and_previous_payload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store
        .source_cache_put("media:expired", "provider:a", 0, true, b"[]")
        .unwrap();
    store
        .source_cache_put("media:live", "provider:a", 0, false, b"previous")
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER fail_cache_update BEFORE UPDATE ON source_cache BEGIN
             SELECT RAISE(ABORT, 'private provider details');
         END;",
    )
    .unwrap();
    assert_eq!(
        store
            .source_cache_put("media:live", "provider:a", 301, false, b"replacement")
            .unwrap_err(),
        Error::Database
    );
    let count: i64 = raw
        .query_row("SELECT count(*) FROM source_cache", [], |row| row.get(0))
        .unwrap();
    // Do not use get here: it intentionally removes expired rows.
    assert_eq!(count, 2);
    let payload: Vec<u8> = raw
        .query_row(
            "SELECT data FROM source_cache WHERE media_key = 'media:live'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload, b"previous");
}
