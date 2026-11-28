use kadr_storage::{Error, PlayerPreference, RateLimit, SCHEMA_VERSION, Settings, Storage};
use rusqlite::Connection;

#[test]
fn proxy_round_trips_unchanged_and_can_be_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap().catalog_proxy, None);

    for endpoint in [
        "http://localhost",
        "https://proxy.example:443/",
        "socks5://127.0.0.1:1080",
        "socks5h://[::1]:1080/",
    ] {
        let settings = Settings {
            catalog_proxy: Some(endpoint.into()),
            ..Settings::default()
        };
        store.save_settings(&settings).unwrap();
        drop(store);
        store = Storage::open(&path).unwrap();
        assert_eq!(store.load_settings().unwrap(), settings);
        assert!(!format!("{settings:?}").contains(endpoint));
    }
    store.save_settings(&Settings::default()).unwrap();
    drop(store);
    assert_eq!(
        Storage::open(&path).unwrap().load_settings().unwrap(),
        Settings::default()
    );
}

#[test]
fn invalid_proxy_does_not_replace_valid_settings_or_leak_the_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let good = Settings {
        catalog_proxy: Some("socks5h://localhost:1080".into()),
        ..Settings::default()
    };
    store.save_settings(&good).unwrap();

    for endpoint in [
        "",
        "ftp://private.example:1080",
        "http://private.example:0",
        "http://private.example:",
        "http://private.example/path",
        "http://private.example?secret",
        "http://private.example#secret",
        "http://@private.example",
        "http://user:secret@private.example:1080",
        "socks5h://private.example\n",
    ] {
        assert!(!kadr_core::proxy::is_valid_endpoint(endpoint));
        let invalid = Settings {
            catalog_proxy: Some(endpoint.into()),
            ..good.clone()
        };
        let error = invalid.validate().unwrap_err();
        assert!(matches!(error, Error::InvalidInput("catalog proxy")));
        if !endpoint.is_empty() {
            assert!(!format!("{error} {error:?} {invalid:?}").contains(endpoint));
        }
        assert!(store.save_settings(&invalid).is_err());
        assert_eq!(store.load_settings().unwrap(), good);
    }
}

#[test]
fn corrupt_stored_proxy_is_rejected_without_reset_or_disclosure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Storage::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    let endpoint = "http://user:secret@private.example:1080";
    raw.execute("UPDATE settings SET catalog_proxy = ?1", [endpoint])
        .unwrap();
    let error = store.load_settings().unwrap_err();
    assert!(matches!(error, Error::CorruptData("settings")));
    assert!(!format!("{error} {error:?}").contains(endpoint));
    let stored: String = raw
        .query_row("SELECT catalog_proxy FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, endpoint);
}

#[test]
fn v5_migration_preserves_existing_settings_and_has_no_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut settings = Settings {
        default_player: PlayerPreference::Vlc,
        catalog_enabled: false,
        cache_limit_bytes: 3 * 1024 * 1024,
        cache_directory: Some(dir.path().join("custom-cache")),
        ..Settings::default()
    };
    settings.network.torrents_enabled = true;
    settings.network.download = RateLimit::Unlimited;
    settings.network.upload = RateLimit::BytesPerSecond(12345);
    settings.network.max_connections = 123;
    let mut store = Storage::open(&path).unwrap();
    store.save_settings(&settings).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE source_cache;
         DROP TABLE torznab_configs;
         DROP TABLE source_preferences;
         ALTER TABLE settings DROP COLUMN catalog_proxy;
         ALTER TABLE settings DROP COLUMN sidebar_collapsed;
         PRAGMA user_version = 5;",
    )
    .unwrap();
    drop(raw);

    let store = Storage::open(&path).unwrap();
    assert_eq!(store.load_settings().unwrap(), settings);
    drop(store);
    let raw = Connection::open(&path).unwrap();
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let proxy: Option<String> = raw
        .query_row("SELECT catalog_proxy FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(proxy, None);
}

#[test]
fn v6_migration_collision_rolls_back_without_replacing_existing_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let settings = Settings {
        catalog_proxy: Some("http://localhost:8080".into()),
        ..Settings::default()
    };
    let mut store = Storage::open(&path).unwrap();
    store.save_settings(&settings).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE source_cache;
         DROP TABLE torznab_configs;
         DROP TABLE source_preferences;
         ALTER TABLE settings DROP COLUMN sidebar_collapsed;
         PRAGMA user_version = 5;",
    )
    .unwrap();
    drop(raw);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(Storage::open(&path), Err(Error::Database)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let raw = Connection::open(&path).unwrap();
    let version: i64 = raw
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    let endpoint: String = raw
        .query_row("SELECT catalog_proxy FROM settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(Some(endpoint), settings.catalog_proxy);
}
