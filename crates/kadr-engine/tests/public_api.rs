//! Public API smoke tests: no UI, network requests, players or storage imports.
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use kadr_engine::{Command, Engine, EngineDirectories, Event, Settings};

fn directories(root: &std::path::Path) -> EngineDirectories {
    EngineDirectories {
        data_directory: root.join("data"),
        config_directory: root.join("config"),
        cache_directory: root.join("cache"),
    }
}

#[test]
fn starts_updates_preferences_and_restarts_using_only_public_api() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    let wait = |events: &mut futures_channel::mpsc::UnboundedReceiver<Event>,
                predicate: &dyn Fn(&Event) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            assert!(Instant::now() < deadline, "engine event timeout");
            if let Ok(event) = events.try_recv() {
                if predicate(&event) {
                    return event;
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let initial = wait(&mut events, &|event| matches!(event, Event::Ready { .. }));
    let Event::Ready { mut settings, cache } = initial else {
        unreachable!()
    };
    assert_eq!(settings, Settings::default());
    assert_eq!(cache.unwrap().active_leases, 0);
    settings.catalog_enabled = false;
    settings.network.max_connections = 7;
    assert!(engine.dispatch(Command::UpdateSettings(settings.clone())));
    wait(&mut events, &|event| {
        matches!(event, Event::PreferencesSaved(value) if value == &settings)
    });
    engine.cancel();
    assert!(engine.join());
    assert!(engine.join(), "join must be idempotent");
    assert!(root.path().join("data/kadr.sqlite3").is_file());
    assert!(root.path().join("config").is_dir());
    assert!(root.path().join("cache").is_dir());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, &|event| {
        matches!(event, Event::Ready { settings: value, .. } if value == &settings)
    });
    assert!(engine.join());
}

#[test]
fn rejects_invalid_directories_before_starting_worker() {
    let root = tempfile::tempdir().unwrap();
    for invalid in [
        PathBuf::from("relative"),
        root.path().join("parent/../child"),
        root.path().join("bad\0directory"),
    ] {
        let mut paths = directories(root.path());
        paths.data_directory = invalid;
        let Err(error) = Engine::start_with_directories(paths) else {
            panic!("invalid application directory accepted");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(!root.path().join("data").exists());
}
