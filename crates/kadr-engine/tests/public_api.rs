//! Public API smoke tests: no UI, network requests, players or storage imports.
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use kadr_engine::{
    AsyncState, Command, Engine, EngineDirectories, Event, Settings, SourcePreferences,
};

fn directories(root: &std::path::Path) -> EngineDirectories {
    EngineDirectories {
        data_directory: root.join("data"),
        config_directory: root.join("config"),
        cache_directory: root.join("cache"),
    }
}

fn wait(
    events: &mut futures_channel::mpsc::UnboundedReceiver<Event>,
    predicate: impl Fn(&Event) -> bool,
) -> Event {
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
}

#[test]
fn starts_updates_preferences_and_restarts_using_only_public_api() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    let initial = wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    let Event::Ready {
        mut settings,
        cache,
    } = initial
    else {
        unreachable!()
    };
    assert_eq!(settings, Settings::default());
    assert_eq!(cache.unwrap().active_leases, 0);
    settings.catalog_enabled = false;
    settings.network.max_connections = 7;
    assert!(engine.dispatch(Command::UpdateSettings(settings.clone())));
    wait(
        &mut events,
        |event| matches!(event, Event::PreferencesSaved(value) if value == &settings),
    );
    engine.cancel();
    assert!(engine.join());
    assert!(engine.join(), "join must be idempotent");
    assert!(root.path().join("data/kadr.sqlite3").is_file());
    assert!(root.path().join("config").is_dir());
    assert!(root.path().join("cache").is_dir());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(
        &mut events,
        |event| matches!(event, Event::Ready { settings: value, .. } if value == &settings),
    );
    assert!(engine.join());
}

#[test]
fn sidebar_and_automatic_discovery_preferences_persist_without_enabling_network() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    let Event::Ready { mut settings, .. } =
        wait(&mut events, |event| matches!(event, Event::Ready { .. }))
    else {
        unreachable!()
    };
    assert!(!settings.sidebar_collapsed);
    assert!(!settings.network.torrents_enabled);
    let Event::SourcePreferences(mut preferences) = wait(&mut events, |event| {
        matches!(event, Event::SourcePreferences(_))
    }) else {
        unreachable!()
    };
    assert!(!preferences.auto_discover);
    preferences.auto_discover = true;
    assert!(engine.dispatch(Command::SetSourcePreferences(preferences.clone())));
    wait(
        &mut events,
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &preferences),
    );
    assert!(engine.dispatch(Command::SetSidebarCollapsed(true)));
    wait(&mut events, |event| {
        matches!(event, Event::SidebarCollapsed(true))
    });
    // A preferences form opened before the toggle must not restore its stale
    // sidebar value when saving otherwise unchanged settings.
    assert!(engine.dispatch(Command::UpdateSettings(settings.clone())));
    settings.sidebar_collapsed = true;
    wait(
        &mut events,
        |event| matches!(event, Event::PreferencesSaved(saved) if saved == &settings),
    );
    assert!(engine.join());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(
        &mut events,
        |event| matches!(event, Event::Ready { settings: saved, .. } if saved == &settings),
    );
    wait(
        &mut events,
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &preferences),
    );
    assert!(engine.dispatch(Command::SetSidebarCollapsed(false)));
    wait(&mut events, |event| {
        matches!(event, Event::SidebarCollapsed(false))
    });
    assert!(engine.dispatch(Command::SetSourcePreferences(SourcePreferences::default())));
    wait(
        &mut events,
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &SourcePreferences::default()),
    );
    assert!(engine.join());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    settings.sidebar_collapsed = false;
    wait(
        &mut events,
        |event| matches!(event, Event::Ready { settings: saved, .. } if saved == &settings),
    );
    wait(
        &mut events,
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &SourcePreferences::default()),
    );
    assert!(engine.join());
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event,
            Event::BrowseState(_) | Event::SourceDiscovery { .. } | Event::PlaybackActive(true)
        ));
    }
}

#[test]
fn credentials_cannot_start_login_without_a_selected_rutracker_offer() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    assert!(engine.dispatch(Command::SignInRuTracker {
        username: "test user".into(),
        password: "test credential".into(),
    }));
    wait(&mut events, |event| {
        matches!(
            event,
            Event::RuTrackerLogin {
                message: None,
                pending: false
            }
        )
    });
    assert!(engine.join());
}

#[test]
fn browse_is_explicit_and_uses_its_own_gated_event_stream() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    assert!(engine.join());
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, Event::BrowseState(_)),
            "startup must not browse"
        );
    }

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    assert!(engine.dispatch(Command::UpdateSettings(Settings {
        catalog_enabled: false,
        ..Settings::default()
    })));
    // Settings disablement also clears browse presentation with an error.
    wait(&mut events, |event| {
        matches!(event, Event::BrowseState(AsyncState::Error(_)))
    });
    // Drain all earlier settings feedback before testing request event isolation.
    assert!(engine.join());
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    assert!(engine.dispatch(Command::Browse));
    wait(&mut events, |event| {
        matches!(event, Event::BrowseState(AsyncState::Loading))
    });
    let result = wait(&mut events, |event| {
        assert!(!matches!(
            event,
            Event::SearchState(_) | Event::SourcesState(_)
        ));
        matches!(event, Event::BrowseState(AsyncState::Error(_)))
    });
    assert!(
        matches!(result, Event::BrowseState(AsyncState::Error(message)) if !message.is_empty())
    );
    assert!(engine.dispatch(Command::Cancel));
    wait(&mut events, |event| {
        assert!(!matches!(event, Event::BrowseState(_)));
        matches!(event, Event::SearchState(AsyncState::Ready(_)))
    });
    assert!(engine.join());
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, Event::BrowseState(_)));
    }
}

#[test]
fn rejected_catalog_disable_still_finishes_cancelled_browse_presentation() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    let defaults = Settings::default();
    let invalid = Settings {
        catalog_enabled: false,
        network: kadr_engine::NetworkLimits {
            max_connections: 0,
            ..defaults.network
        },
        ..defaults
    };
    assert!(engine.dispatch(Command::UpdateSettings(invalid)));
    // No network request is needed: this update cancels any Browse token in
    // dispatch, so a terminal event must be guaranteed even on rejection.
    wait(&mut events, |event| {
        matches!(event, Event::BrowseState(AsyncState::Error(_)))
    });
    let rejected = wait(&mut events, |event| {
        matches!(event, Event::PreferencesRejected { .. })
    });
    assert!(matches!(
        rejected,
        Event::PreferencesRejected { settings, .. } if settings.catalog_enabled
    ));
    assert!(engine.join());
}

#[test]
fn catalog_proxy_can_be_saved_restored_and_cleared_without_network_access() {
    let root = tempfile::tempdir().unwrap();
    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(&mut events, |event| matches!(event, Event::Ready { .. }));
    let configured = Settings {
        catalog_proxy: Some("socks5h://127.0.0.1:1080".into()),
        ..Settings::default()
    };
    assert!(engine.dispatch(Command::UpdateSettings(configured.clone())));
    wait(
        &mut events,
        |event| matches!(event, Event::PreferencesSaved(settings) if settings == &configured),
    );
    let invalid = Settings {
        catalog_proxy: Some("http://user:password@proxy.example:8080".into()),
        ..configured.clone()
    };
    assert!(engine.dispatch(Command::UpdateSettings(invalid)));
    let rejected = wait(&mut events, |event| {
        matches!(event, Event::PreferencesRejected { .. })
    });
    let Event::PreferencesRejected { settings, message } = rejected else {
        unreachable!();
    };
    assert_eq!(settings, configured);
    assert!(message.contains("прокси"));
    assert!(!message.contains("proxy.example") && !message.contains("password"));
    assert!(engine.join());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(
        &mut events,
        |event| matches!(event, Event::Ready { settings, .. } if settings == &configured),
    );
    assert!(engine.dispatch(Command::UpdateSettings(Settings::default())));
    wait(
        &mut events,
        |event| matches!(event, Event::PreferencesSaved(settings) if settings == &Settings::default()),
    );
    assert!(engine.join());

    let (mut engine, mut events) =
        Engine::start_with_directories(directories(root.path())).unwrap();
    wait(
        &mut events,
        |event| matches!(event, Event::Ready { settings, .. } if settings == &Settings::default()),
    );
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
