//! Command/event acceptance tests. Ordinary tests never launch a player or browser.
//! Run real-media tests explicitly with `cargo test -p kadr-engine
//! tests::real_ -- --ignored --test-threads=1`.
//!
//! All media is generated locally; all peers and HTTP origins are loopback-only.
//! No public catalogs, trackers, DHT, downloaded fixtures, or user directories.

use super::*;
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Child, Command as ProcessCommand, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use kadr_storage::{
    EpisodeKey, MediaId, PlaybackProgress, PlaybackTarget, ProgressOrigin, SavedFile, SourceId,
    Storage, StoredSource,
};
use sha1::{Digest, Sha1};

mod commons;
mod episodes;
mod torznab;

const WAIT: Duration = Duration::from_secs(8);
const PRIVATE_MARKER: &str = "acceptance-private-query-never-display";

fn paths(root: &Path) -> AppPaths {
    AppPaths::from_roots(root.join("data"), root.join("config"), root.join("cache"))
        .expect("isolated absolute application paths")
}

/// Own the worker even when an assertion unwinds, before its tempdir is deleted.
struct App {
    backend: Engine,
    events: UnboundedReceiver<Event>,
}

impl App {
    fn start(paths: &AppPaths) -> Self {
        let (backend, events) =
            Engine::start_with(Some(paths.clone()), true).expect("start isolated engine worker");
        Self { backend, events }
    }

    fn command(&mut self, command: Command) {
        assert!(
            self.backend.dispatch(command),
            "backend command queue unexpectedly closed/full"
        );
    }

    fn wait(&mut self, description: &str, predicate: impl Fn(&Event) -> bool) -> Event {
        self.wait_for(WAIT, description, predicate)
    }

    fn wait_for(
        &mut self,
        timeout: Duration,
        description: &str,
        predicate: impl Fn(&Event) -> bool,
    ) -> Event {
        let deadline = Instant::now() + timeout;
        let mut observed = Vec::new();
        while Instant::now() < deadline {
            if let Ok(event) = self.events.try_recv() {
                assert_public_feedback_is_private(&event);
                match &event {
                    Event::Status(level, message) => observed.push(format!("{level:?}: {message}")),
                    Event::SourceSelected { playable, kind, .. } => {
                        observed.push(format!("SourceSelected({kind:?}, playable={playable})"))
                    }
                    Event::Library(items) => {
                        observed.push(format!("Library({} rows)", items.len()))
                    }
                    Event::PreferencesRejected { message, .. } => {
                        observed.push(format!("PreferencesRejected: {message}"))
                    }
                    Event::Transfer(Some(transfer)) => observed.push(format!(
                        "Transfer({}, done={:?}/{:?}, verified={:?})",
                        transfer.stage, transfer.done, transfer.total, transfer.buffered,
                    )),
                    _ => {}
                }
                if predicate(&event) {
                    return event;
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        panic!("timed out waiting for {description}; observed {observed:?}");
    }

    fn until(&mut self, timeout: Duration, description: &str, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        let mut statuses = Vec::new();
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            if let Ok(event) = self.events.try_recv() {
                assert_public_feedback_is_private(&event);
                if let Event::Status(level, message) = event {
                    statuses.push(format!("{level:?}: {message}"));
                }
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
        panic!("timed out waiting for {description}; observed statuses {statuses:?}");
    }

    fn ready(&mut self) -> Settings {
        match self.wait("Ready", |event| matches!(event, Event::Ready { .. })) {
            Event::Ready { settings, cache } => {
                assert!(
                    cache.is_some(),
                    "Ready must include available cache accounting"
                );
                settings
            }
            _ => unreachable!(),
        }
    }

    fn selected(&mut self) {
        self.wait("playable source selection", |event| {
            matches!(event, Event::SourceSelected { playable: true, .. })
        });
    }

    fn stopped(&mut self) {
        self.command(Command::Stop);
        self.wait("PlaybackActive(false) after Stop", |event| {
            matches!(event, Event::PlaybackActive(false))
        });
        // This event follows player reaping, HTTP shutdown and lease release.
        self.wait("Transfer(None) after stopping an active session", |event| {
            matches!(event, Event::Transfer(None))
        });
    }

    fn finish(&mut self) {
        let before = Instant::now();
        assert!(
            self.backend.join(),
            "backend worker panicked while shutting down"
        );
        assert!(
            before.elapsed() < WAIT,
            "backend shutdown exceeded its bounded cleanup budget"
        );
        while let Ok(event) = self.events.try_recv() {
            assert_public_feedback_is_private(&event);
        }
    }

    fn startup_failed(&mut self) {
        // The first event must be a failure, never a misleading Ready/default reset.
        let event = self.wait("startup storage error", |_| true);
        assert!(
            matches!(event, Event::Status(Level::Error, _)),
            "startup must fail closed"
        );
        assert!(
            self.backend.join(),
            "failed-startup worker must shut down cleanly"
        );
        while let Ok(event) = self.events.try_recv() {
            assert!(
                !matches!(event, Event::Ready { .. }),
                "failed startup must never publish Ready"
            );
            assert_public_feedback_is_private(&event);
        }
        assert!(
            !self.backend.dispatch(Command::ClearCache),
            "failed startup must close its command receiver"
        );
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.backend.cancel();
        // join is idempotent. Keep the runtime alive until its owned resources stop.
        let joined = self.backend.join();
        assert!(
            joined || thread::panicking(),
            "backend worker cleanup failed"
        );
    }
}

fn assert_public_feedback_is_private(event: &Event) {
    let check = |value: &str| {
        assert!(
            !value.contains(PRIVATE_MARKER),
            "private source query leaked in public feedback"
        );
    };
    match event {
        Event::Status(_, message) => check(message),
        Event::PreferencesRejected { message, .. } => check(message),
        Event::SourceSelected {
            title,
            suggested_filename,
            ..
        } => {
            check(title);
            check(suggested_filename);
        }
        Event::Transfer(Some(transfer)) => {
            check(&transfer.stage);
            check(&transfer.saving);
        }
        Event::SearchState(AsyncState::Error(message))
        | Event::BrowseState(AsyncState::Error(message))
        | Event::SourcesState(AsyncState::Error(message)) => check(message),
        // Library snapshots contain no source descriptors and redact private titles.
        Event::Library(items) => check(&format!("{items:?}")),
        _ => {}
    }
}

#[test]
fn browse_cancellation_is_independent_from_search_and_source_discovery() {
    // Inspect queue tokens without running requests or contacting providers.
    let (commands, mut receiver) = mpsc::channel(32);
    let shutdown = CancellationToken::new();
    let mut engine = Engine {
        commands,
        browse_stop: shutdown.child_token(),
        catalog_stop: shutdown.child_token(),
        source_stop: shutdown.child_token(),
        playback_stop: shutdown.child_token(),
        save_stop: shutdown.child_token(),
        shutdown,
        thread: None,
    };
    assert!(engine.dispatch(Command::Browse));
    let first_browse = receiver.try_recv().unwrap().cancel;
    let mut current_browse = first_browse.clone();
    for command in [Command::Search("query".into()), Command::FindSources] {
        assert!(engine.dispatch(command));
        let catalog = receiver.try_recv().unwrap().cancel;
        assert!(!current_browse.is_cancelled());
        let source = engine.source_stop.clone();
        let source_cancelled = source.is_cancelled();
        assert!(engine.dispatch(Command::Browse));
        let next_browse = receiver.try_recv().unwrap().cancel;
        assert!(
            !catalog.is_cancelled(),
            "browse must not cancel catalog work"
        );
        assert_eq!(source.is_cancelled(), source_cancelled);
        assert!(current_browse.is_cancelled());
        assert!(!next_browse.is_cancelled());
        current_browse = next_browse;
    }
    assert!(
        first_browse.is_cancelled(),
        "new Browse supersedes old Browse"
    );
    assert!(engine.dispatch(Command::Browse));
    let browse = receiver.try_recv().unwrap().cancel;
    for command in [Command::Cancel, Command::Stop] {
        assert!(engine.dispatch(command));
        receiver.try_recv().unwrap();
        assert!(
            !browse.is_cancelled(),
            "an unrelated dialog must not cancel Browse"
        );
    }
    assert!(engine.dispatch(Command::UpdateSettings(Settings {
        catalog_enabled: false,
        ..Settings::default()
    })));
    receiver.try_recv().unwrap();
    assert!(
        browse.is_cancelled(),
        "disabling the catalog cancels Browse"
    );
    assert!(engine.dispatch(Command::Browse));
    let browse = receiver.try_recv().unwrap().cancel;
    engine.cancel();
    assert!(browse.is_cancelled(), "shutdown cancels Browse too");
}

#[test]
fn settings_round_trip_rejects_invalid_updates_and_unusable_cache_without_reset() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let mut app = App::start(&paths);
    let initial = app.ready();
    assert!(
        !initial.network.torrents_enabled,
        "BitTorrent requires explicit consent"
    );

    let mut changed = initial.clone();
    changed.default_player = PlayerPreference::Browser;
    changed.catalog_enabled = false;
    changed.network.download = RateLimit::Paused;
    changed.network.upload = RateLimit::BytesPerSecond(32 * 1024);
    changed.network.max_connections = 7;
    changed.cache_limit_bytes = 2 * 1024 * 1024;
    changed.cache_directory = Some(directory.path().join("chosen-cache"));
    app.command(Command::UpdateSettings(changed.clone()));
    app.wait(
        "all settings saved",
        |event| matches!(event, Event::PreferencesSaved(value) if value == &changed),
    );

    let mut invalid = changed.clone();
    invalid.network.max_connections = 0;
    app.command(Command::UpdateSettings(invalid));
    app.wait("invalid settings rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    let blocked_cache = directory.path().join("not-a-cache-directory");
    fs::write(&blocked_cache, b"preserve cache-path sentinel").unwrap();
    let mut unusable = changed.clone();
    unusable.cache_directory = Some(blocked_cache.clone());
    app.command(Command::UpdateSettings(unusable));
    app.wait("unusable cache rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER acceptance_refuse_settings BEFORE INSERT ON settings
         BEGIN SELECT RAISE(FAIL, 'acceptance injected write failure'); END;",
        )
        .unwrap();
    let mut unwritable = changed.clone();
    unwritable.default_player = PlayerPreference::Vlc;
    app.command(Command::UpdateSettings(unwritable));
    app.wait(
        "SQLite write failure reported without acknowledgement",
        |event| matches!(event, Event::Status(Level::Error, _)),
    );
    connection
        .execute_batch("DROP TRIGGER acceptance_refuse_settings;")
        .unwrap();
    drop(connection);
    app.finish();

    assert_eq!(
        fs::read(blocked_cache).unwrap(),
        b"preserve cache-path sentinel"
    );
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert_eq!(
        storage.load_settings().unwrap(),
        changed.clone().into_storage(),
        "failed edits must not persist"
    );
    drop(storage);
    let mut restarted = App::start(&paths);
    assert_eq!(
        restarted.ready(),
        changed,
        "new worker must load every persisted setting"
    );
    restarted.command(Command::Search("offline acceptance query".into()));
    restarted.wait("disabled catalog error without network", |event| {
        matches!(event, Event::SearchState(AsyncState::Error(_)))
    });
    restarted.finish();
}

#[test]
fn storage_open_failure_has_no_ready_and_preserves_existing_data() {
    let directory = tempfile::tempdir().unwrap();
    let data_file = directory.path().join("not-a-directory");
    fs::write(&data_file, b"preserve existing data").unwrap();
    let paths = AppPaths::from_roots(
        data_file.clone(),
        directory.path().join("config"),
        directory.path().join("cache"),
    )
    .unwrap();
    let mut app = App::start(&paths);
    app.startup_failed();
    assert_eq!(fs::read(data_file).unwrap(), b"preserve existing data");
}

#[test]
fn future_schema_and_failed_migration_have_no_ready_and_preserve_schema() {
    for future in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        paths.create_dirs().unwrap();
        let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
        // A v0 table collision fails *after* migration started creating settings.
        connection
            .execute_batch(
                "CREATE TABLE library(sentinel TEXT NOT NULL);
             INSERT INTO library VALUES ('preserve migration sentinel');",
            )
            .unwrap();
        let version = if future { 999 } else { 0 };
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let mut app = App::start(&paths);
        app.startup_failed();

        let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
        let actual: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(
            actual, version,
            "failed startup must not change migration version"
        );
        let sentinel: String = connection
            .query_row("SELECT sentinel FROM library", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sentinel, "preserve migration sentinel");
        let created: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='settings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            created, 0,
            "failed migration must roll back earlier schema changes"
        );
    }
}

#[test]
fn corrupt_settings_have_no_ready_and_are_not_replaced_with_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    drop(Storage::open_default(&paths).unwrap());
    let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints=ON;
         UPDATE settings SET max_connections=0 WHERE singleton=1;",
        )
        .unwrap();
    drop(connection);
    let mut app = App::start(&paths);
    app.startup_failed();
    let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
    let connections: i64 = connection
        .query_row(
            "SELECT max_connections FROM settings WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        connections, 0,
        "corruption must remain diagnosable, not silently reset"
    );
}

#[test]
fn command_validation_rejects_missing_selection_bad_torrents_and_network_without_consent() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let mut app = App::start(&paths);
    app.ready();
    app.command(Command::Play { resume: true });
    app.wait("Play without selection rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::ChooseSource(999));
    app.wait("stale source index rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::ChooseTorrentFile(999));
    app.wait("invalid video index rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::AssignEpisode(1, 0));
    app.wait("zero episode rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });

    let invalid = directory.path().join("bad.torrent");
    fs::write(&invalid, b"not bencoded metadata").unwrap();
    app.command(Command::SelectSource(
        ManualSource::torrent_file(invalid).unwrap(),
    ));
    app.wait("malformed metadata rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::SelectSource(
        ManualSource::torrent_file(directory.path().join("missing.torrent")).unwrap(),
    ));
    app.wait("missing torrent rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::SelectSource(
        ManualSource::parse_link("magnet:?xt=urn:btih:0123456789012345678901234567890123456789")
            .unwrap(),
    ));
    app.wait("magnet blocked before network consent", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::ClearCache);
    let usage = app.wait("cache accounting without payload reservation", |event| {
        matches!(event, Event::CacheUsage(_))
    });
    if let Event::CacheUsage(usage) = usage {
        assert_eq!(usage.reserved_bytes, 0);
        assert_eq!(usage.actual_bytes, 0);
        assert_eq!(usage.active_leases, 0);
    }
    app.finish();
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert!(storage.history(10).unwrap().is_empty());
    assert!(storage.library(10).unwrap().is_empty());
}

#[test]
fn torrent_file_choices_are_offline_episode_aware_and_require_consent_to_play() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let torrent = directory.path().join("own-series.torrent");
    fs::write(
        &torrent,
        metainfo(
            &[
                ("Own.S01E01.mp4", vec![1; 32 * 1024]),
                ("Own.S01E02.mp4", vec![2; 32 * 1024]),
                ("README.txt", b"owned fixture".to_vec()),
            ],
            true,
        ),
    )
    .unwrap();
    let mut app = App::start(&paths);
    let settings = app.ready();
    app.command(Command::SelectSource(
        ManualSource::torrent_file(torrent).unwrap(),
    ));
    let choices = app.wait(
        "two recognized video choices",
        |event| matches!(event, Event::TorrentFiles(files) if files.len() == 2),
    );
    if let Event::TorrentFiles(files) = choices {
        assert_eq!(
            (files[0].index, files[0].season, files[0].episode),
            (0, Some(1), Some(1))
        );
        assert_eq!(
            (files[1].index, files[1].season, files[1].episode),
            (1, Some(1), Some(2))
        );
    }
    app.command(Command::ChooseTorrentFile(1));
    app.wait(
        "selected video remains unplayable before consent",
        |event| {
            matches!(event, Event::SourceSelected { playable: false, suggested_filename, .. }
            if suggested_filename == "Own.S01E02.mp4")
        },
    );
    app.command(Command::Play { resume: true });
    app.wait("payload download blocked before consent", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::ClearCache);
    app.wait("failed start releases its reservation", |event| {
        matches!(event, Event::CacheUsage(usage) if usage.active_leases == 0 && usage.reserved_bytes == 0)
    });
    app.finish();
    assert_eq!(
        Storage::open(&paths.database_path())
            .unwrap()
            .load_settings()
            .unwrap(),
        settings.into_storage()
    );
    let payload = paths.cache_dir().join("torrents-v1/data");
    assert!(
        !payload.exists() || fs::read_dir(payload).unwrap().count() == 0,
        "consent failure must not allocate payload files"
    );
}

#[test]
fn cache_quota_rejects_whole_torrent_before_network_or_player_launch() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let torrent = directory.path().join("oversized.torrent");
    fs::write(
        &torrent,
        metainfo(&[("Own.S01E01.mp4", vec![3; 2 * 1024 * 1024])], true),
    )
    .unwrap();
    let mut app = App::start(&paths);
    let mut settings = app.ready();
    settings.network.torrents_enabled = true;
    settings.cache_limit_bytes = 1024 * 1024;
    app.command(Command::UpdateSettings(settings.clone()));
    app.wait("small cache quota configured", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    app.command(Command::SelectSource(
        ManualSource::torrent_file(torrent).unwrap(),
    ));
    app.selected();
    app.command(Command::Play { resume: false });
    app.wait(
        "whole torrent reservation rejected before starting payload",
        |event| matches!(event, Event::Status(Level::Error, message) if message.contains("quota")),
    );
    app.command(Command::ClearCache);
    app.wait("failed reservation creates no active cache data", |event| {
        matches!(event, Event::CacheUsage(usage) if usage.reserved_bytes == 0
            && usage.actual_bytes == 0 && usage.active_leases == 0)
    });
    app.finish();
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert_eq!(storage.load_settings().unwrap(), settings.into_storage());
    assert!(
        storage.history(10).unwrap().is_empty(),
        "quota failure must not pretend playback started"
    );
}

#[test]
fn browser_rejects_unstable_http_without_opening_system_browser_or_inventing_progress() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let origin = HttpOrigin::new(vec![0; 1024], HttpBehavior::NoValidator);
    let source = ManualSource::parse_link(&origin.url()).unwrap();
    let target = target(&source, None);
    let mut app = App::start(&paths);
    let mut settings = app.ready();
    settings.default_player = PlayerPreference::Browser;
    app.command(Command::UpdateSettings(settings));
    app.wait("browser preference persisted", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    app.command(Command::SelectSource(source));
    app.selected();
    app.command(Command::Play { resume: true });
    app.wait(
        "browser refused before launching for unstable HTTP",
        |event| matches!(event, Event::Status(Level::Error, message) if message.contains("Range")),
    );
    app.finish();
    assert!(
        origin.heads.load(Ordering::Relaxed) > 0,
        "must probe actual HTTP metadata"
    );
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert!(
        storage.load_progress(&target).unwrap().is_none(),
        "browser has no position contract"
    );
    assert!(
        storage.history(10).unwrap().is_empty(),
        "failed browser launch is not history"
    );
    assert_eq!(
        storage.load_settings().unwrap().default_player,
        PlayerPreference::Browser.into_storage()
    );
}

#[test]
fn stop_interrupts_pending_http_probe_before_player_launch() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let origin = HttpOrigin::new(vec![0; 1024], HttpBehavior::StallHead);
    let source = ManualSource::parse_link(&origin.url()).unwrap();
    let target = target(&source, None);
    let mut app = App::start(&paths);
    app.ready();
    app.command(Command::SelectSource(source));
    app.selected();
    app.command(Command::Play { resume: false });
    // Like Ready/source selection, reaching HTTP includes durable storage work.
    // This is a readiness wait, not the cancellation-latency measurement below.
    app.until(WAIT, "origin must receive pending HEAD", || {
        origin.heads.load(Ordering::Relaxed) > 0
    });
    let before = Instant::now();
    app.command(Command::Stop);
    app.wait("pending start cancelled without player", |event| {
        matches!(event, Event::PlaybackActive(false))
    });
    app.finish();
    assert!(
        before.elapsed() < Duration::from_secs(3),
        "Stop must bypass queued/probing work"
    );
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert!(storage.load_progress(&target).unwrap().is_none());
    assert!(storage.history(10).unwrap().is_empty());
}

#[test]
fn library_open_restores_descriptor_and_episode_without_mutating_37_second_progress() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let origin = HttpOrigin::new(vec![0; 1024], HttpBehavior::NoValidator);
    let descriptor = StoredSource::Link(origin.url());
    let source = descriptor.manual_source().unwrap();
    let target = target(
        &source,
        Some(EpisodeKey {
            season: 2,
            episode: 7,
        }),
    );
    let other = PlaybackTarget {
        media_id: target.media_id.clone(),
        episode: Some(EpisodeKey {
            season: 2,
            episode: 8,
        }),
    };
    let mut storage = Storage::open_default(&paths).unwrap();
    let first = progress(&source, target.clone(), 37.0, 200);
    let second = progress(&source, other.clone(), 11.0, 100);
    storage
        .save_source(&descriptor.source_id().unwrap(), &descriptor)
        .unwrap();
    storage
        .save_progress_for_source(&second, &descriptor)
        .unwrap();
    storage
        .save_progress_for_source(&first, &descriptor)
        .unwrap();
    storage.set_favorite(&target.media_id, true).unwrap();
    drop(storage);

    let mut app = App::start(&paths);
    app.ready();
    app.wait("startup library resumes latest episode", |event| {
        matches!(event, Event::Library(items) if items.iter().any(|item|
            item.id == library_id(&target)
                && item.episode == Some(LibraryEpisode { season: 2, number: 7 })
                && item.progress == Some(LibraryProgress {
                    position_seconds: first.position_seconds,
                    duration_seconds: first.duration_seconds,
                    completed: first.completed,
                })
                && item.favorite))
    });
    app.command(Command::OpenLibrary(library_id(&target)));
    app.selected();
    // Invalid browser fallback exercises the reopened descriptor without a player.
    app.command(Command::OpenIn(PlayerPreference::Browser));
    app.wait(
        "reopened private HTTP descriptor probed then browser refused",
        |event| matches!(event, Event::Status(Level::Error, message) if message.contains("Range")),
    );
    app.finish();
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert_eq!(storage.load_progress(&target).unwrap(), Some(first));
    assert_eq!(storage.load_progress(&other).unwrap(), Some(second));
    assert_eq!(storage.library(10).unwrap()[0].source, Some(descriptor));
}

#[test]
fn clearing_history_preserves_favorites_saved_files_and_reopen_identity() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let source = ManualSource::parse_link("http://127.0.0.1:1/own.mp4").unwrap();
    let target = target(
        &source,
        Some(EpisodeKey {
            season: 1,
            episode: 3,
        }),
    );
    let descriptor = StoredSource::Link(source.http_url().unwrap().as_str().to_owned());
    let saved_path = directory.path().join("user-owned.mp4");
    fs::write(&saved_path, b"owned saved-file sentinel").unwrap();
    let mut storage = Storage::open_default(&paths).unwrap();
    storage
        .save_source(&descriptor.source_id().unwrap(), &descriptor)
        .unwrap();
    storage
        .save_progress_for_source(&progress(&source, target.clone(), 37.0, 200), &descriptor)
        .unwrap();
    storage
        .record_opened(
            &target.media_id,
            &SourceId::new(source.id().as_str()).unwrap(),
            200,
        )
        .unwrap();
    storage.set_favorite(&target.media_id, true).unwrap();
    storage
        .save_saved_file(&SavedFile {
            target: target.clone(),
            path: saved_path.clone(),
            saved_at: 200,
        })
        .unwrap();
    drop(storage);
    let mut app = App::start(&paths);
    app.ready();
    app.wait("initial library snapshot", |event| {
        matches!(event, Event::Library(_))
    });
    app.command(Command::ClearHistory);
    app.wait("library cleared except favorite/saved identity", |event| {
        matches!(event, Event::Library(items) if items.iter().all(|item| item.progress.is_none() && item.favorite)
            && items.iter().any(|item| item.id == library_id(&target)))
    });
    app.command(Command::ClearCache);
    app.wait("cache cleared", |event| {
        matches!(event, Event::CacheUsage(_))
    });
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("saved file is reopenable without remote network", |event| {
        matches!(event, Event::SourceSelected { kind: None, playable: true, suggested_filename, .. }
            if suggested_filename == "user-owned.mp4")
    });
    app.finish();
    assert_eq!(fs::read(saved_path).unwrap(), b"owned saved-file sentinel");
    let storage = Storage::open(&paths.database_path()).unwrap();
    assert!(storage.load_progress(&target).unwrap().is_none());
    assert!(storage.history(10).unwrap().is_empty());
    assert!(storage.saved_file(&target).unwrap().is_some());
}

#[test]
fn library_item_without_source_cannot_revive_the_previously_selected_input() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let mut storage = Storage::open_default(&paths).unwrap();
    let favorite = MediaId::new("local:favorite-without-source").unwrap();
    storage.set_favorite(&favorite, true).unwrap();
    drop(storage);
    let mut app = App::start(&paths);
    let mut settings = app.ready();
    app.command(Command::SelectSource(
        ManualSource::parse_link("https://example.invalid/old.mp4").unwrap(),
    ));
    app.selected();
    app.command(Command::OpenLibrary(favorite.as_str().to_owned()));
    app.wait("library explicitly reports no source", |event| {
        matches!(
            event,
            Event::LibraryOpened {
                source_available: false
            }
        )
    });
    settings.default_player = PlayerPreference::Vlc;
    app.command(Command::UpdateSettings(settings));
    app.wait(
        "settings cannot re-enable the unrelated old input",
        |event| {
            assert!(!matches!(
                event,
                Event::SourceSelected { playable: true, .. }
            ));
            matches!(event, Event::CacheUsage(_))
        },
    );
    app.finish();
}

#[test]
fn selecting_another_library_torrent_file_recomputes_its_episode_target() {
    use kadr_torrent::engine::EngineOptions;
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let torrent = directory.path().join("owned.torrent");
    fs::write(
        &torrent,
        metainfo(
            &[
                ("Own.S01E01.mp4", vec![1; 2 * 1024 * 1024]),
                ("Own.S01E02.mp4", vec![2; 2 * 1024 * 1024]),
            ],
            true,
        ),
    )
    .unwrap();
    let descriptor = StoredSource::TorrentFile {
        path: torrent,
        file_id: Some(0),
    };
    let source = descriptor.manual_source().unwrap();
    let id = MediaId::new("tvmaze:999").unwrap();
    let first = PlaybackTarget {
        media_id: id.clone(),
        episode: Some(EpisodeKey::new(1, 1).unwrap()),
    };
    let second = PlaybackTarget {
        media_id: id,
        episode: Some(EpisodeKey::new(1, 2).unwrap()),
    };
    let mut storage = Storage::open_default(&paths).unwrap();
    let mut settings = storage.load_settings().unwrap();
    settings.network.torrents_enabled = true;
    settings.cache_limit_bytes = 1024 * 1024; // Never reach network/player startup.
    storage.save_settings(&settings).unwrap();
    storage
        .save_source(&descriptor.source_id().unwrap(), &descriptor)
        .unwrap();
    storage
        .upsert_media(&kadr_storage::MediaRecord {
            id: first.media_id.clone(),
            title: "Owned series".into(),
            original_title: None,
            year: None,
            kind: kadr_storage::MediaKind::Series,
            description: String::new(),
            poster_url: None,
            attribution: String::new(),
        })
        .unwrap();
    let previous = progress(&source, first.clone(), 37.0, 100);
    storage
        .save_progress_for_source(&previous, &descriptor)
        .unwrap();
    storage.set_favorite(&first.media_id, true).unwrap();
    let options = EngineOptions {
        discovery_enabled: false,
        ..EngineOptions::default()
    };
    let (backend, events) = Engine::start_with_network(Some(paths), true, Some(options)).unwrap();
    let mut app = App { backend, events };
    app.ready();
    app.command(Command::OpenLibrary(library_id(&first)));
    app.selected();
    app.command(Command::ChooseTorrentFile(1));
    app.wait("second filename selected", |event| matches!(event,
        Event::SourceSelected { suggested_filename, playable: true, .. } if suggested_filename == "Own.S01E02.mp4"));
    app.command(Command::Play { resume: true });
    app.wait("quota stops before payload or process", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    assert_eq!(storage.load_progress(&first).unwrap(), Some(previous));
    assert_eq!(
        storage.source_for_target(&first).unwrap(),
        Some(descriptor.clone())
    );
    assert!(matches!(
        storage.source_for_target(&second).unwrap(),
        Some(StoredSource::TorrentFile {
            file_id: Some(1),
            ..
        })
    ));
    app.finish();
}

#[test]
fn reopening_rutracker_history_without_consent_does_not_request_login_or_download() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let mut storage = Storage::open_default(&paths).unwrap();
    let descriptor = StoredSource::RuTracker {
        topic_id: 42,
        info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
        file_id: Some(2),
    };
    let source_id = descriptor.source_id().unwrap();
    let target = PlaybackTarget {
        media_id: MediaId::new("test:tracker-history").unwrap(),
        episode: None,
    };
    storage.save_source(&source_id, &descriptor).unwrap();
    storage.link_source(&target, &source_id).unwrap();
    storage
        .record_opened_target(&target, &source_id, 1)
        .unwrap();
    let (backend, events) = Engine::start_with(Some(paths), false).unwrap();
    let mut app = App { backend, events };
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("the stored topic is gated before resolution", |event| {
        assert!(!matches!(event, Event::RuTrackerLogin { message: Some(_), .. }));
        assert!(!matches!(event, Event::SourceSelected { playable: true, .. }));
        matches!(event, Event::SourcesState(crate::AsyncState::Error(message)) if message.contains("BitTorrent"))
    });
    assert_eq!(
        storage.source_for_target(&target).unwrap(),
        Some(descriptor)
    );
    app.finish();
}

#[test]
fn legacy_library_can_reopen_a_saved_link_without_enabling_online_discovery() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let mut storage = Storage::open_default(&paths).unwrap();
    let media_id = MediaId::new("archive:legacy-fixture").unwrap();
    let target = PlaybackTarget {
        media_id: media_id.clone(),
        episode: None,
    };
    storage
        .upsert_media(&kadr_storage::MediaRecord {
            id: media_id,
            title: "Legacy film".into(),
            original_title: None,
            year: None,
            kind: kadr_storage::MediaKind::Movie,
            description: String::new(),
            poster_url: Some("https://archive.org/services/img/legacy-fixture".into()),
            attribution: "Saved local snapshot".into(),
        })
        .unwrap();
    let descriptor =
        StoredSource::Link("https://archive.org/download/legacy-fixture/owned.mp4".into());
    let source_id = descriptor.source_id().unwrap();
    storage.save_source(&source_id, &descriptor).unwrap();
    storage.link_source(&target, &source_id).unwrap();
    storage
        .record_opened_target(&target, &source_id, 1)
        .unwrap();

    let (backend, events) = Engine::start_with(Some(paths), false).unwrap();
    let mut app = App { backend, events };
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait(
        "legacy snapshot without an Archive poster request",
        |event| {
            matches!(event, Event::MediaSelected { media: Some(media), .. }
            if media.id == "archive:legacy-fixture" && media.poster_url.is_none())
        },
    );
    app.wait("saved HTTP link still available", |event| {
        matches!(event, Event::SourceSelected { playable: true, .. })
    });
    app.command(Command::FindSources);
    app.wait(
        "explicit notice that a legacy snapshot cannot trigger discovery",
        |event| {
            assert!(!matches!(
                event,
                Event::SourcesState(crate::AsyncState::Loading)
            ));
            matches!(event, Event::SourcesState(crate::AsyncState::Error(message))
            if message.contains("Trakt"))
        },
    );
    assert_eq!(
        storage.source_for_target(&target).unwrap(),
        Some(descriptor)
    );
    app.finish();
}

#[test]
fn local_discovery_round_trip_rejects_old_handles_and_reuses_http_preparation() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let input = StoredSource::Link(format!(
        "http://127.0.0.1:9/owned.mp4?marker={PRIVATE_MARKER}"
    ));
    discovered_http(&paths, input).finish();
}

fn discovered_http(paths: &AppPaths, input: StoredSource) -> App {
    let mut db = Storage::open_default(paths).unwrap();
    let id = MediaId::new("trakt:movie:52").unwrap();
    let target = PlaybackTarget {
        media_id: id.clone(),
        episode: None,
    };
    db.upsert_media(&kadr_storage::MediaRecord {
        id,
        title: "Local fixture".into(),
        original_title: None,
        year: Some(2020),
        kind: kadr_storage::MediaKind::Movie,
        description: String::new(),
        poster_url: None,
        attribution: String::new(),
    })
    .unwrap();
    let src_id = input.source_id().unwrap();
    db.save_source(&src_id, &input).unwrap();
    db.link_source(&target, &src_id).unwrap();
    let mut app = App::start(paths);
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.selected();
    let result = |app: &mut App| {
        let Event::SourceDiscovery {
            stamp,
            rows,
            failures,
            ..
        } = app.wait("completed local discovery", |event| {
            matches!(
                event,
                Event::SourceDiscovery {
                    pending: 0,
                    succeeded: 1,
                    ..
                }
            )
        })
        else {
            unreachable!()
        };
        assert!(failures.is_empty());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Local fixture");
        assert_eq!(rows[0].kind, SourceKind::Http);
        (stamp, rows[0].index)
    };
    app.command(Command::FindSources);
    let (stamp, old) = result(&mut app);
    assert!(stamp.is_current());
    app.command(Command::RetrySources);
    assert!(
        !stamp.is_current(),
        "invalidate even snapshots already on the UI queue"
    );
    let (_, current) = result(&mut app);
    assert_ne!(old, current);
    app.command(Command::ChooseSource(old));
    app.wait("reject old row", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    app.command(Command::ChooseSource(current));
    app.selected();
    assert_eq!(db.source_for_target(&target).unwrap(), Some(input));
    app
}

#[test]
#[ignore = "requires ffmpeg and mpv; Torznab enclosure and verified loopback torrent"]
fn real_torznab_selection_plays_generated_video() {
    torznab::play_owned_torrent();
}

#[test]
#[ignore = "requires ffmpeg and mpv; episode search, multi-file torrent, measured playback and exact save"]
fn real_episode_search_selects_correct_file_and_mpv_playback() {
    episodes::play_selected_episode();
}

#[test]
#[ignore = "requires ffmpeg and mpv; owned local video through a resolver selection"]
fn real_discovered_source_plays_generated_video() {
    let root = tempfile::tempdir().unwrap();
    let origin = HttpOrigin::new(
        generate_video(&root.path().join("owned.mp4")),
        HttpBehavior::Stable,
    );
    let mut app = discovered_http(&paths(root.path()), StoredSource::Link(origin.url()));
    app.command(Command::Play { resume: false });
    app.wait("mpv started after explicit choice", |e| {
        matches!(e, Event::PlaybackActive(true))
    });
    app.until(WAIT, "player range read", || {
        origin.ranges.load(Ordering::Relaxed) > 0
    });
    app.stopped();
    app.finish();
}

fn target(source: &ManualSource, episode: Option<EpisodeKey>) -> PlaybackTarget {
    PlaybackTarget {
        media_id: MediaId::new(source.id().as_str()).unwrap(),
        episode,
    }
}

fn library_id(target: &PlaybackTarget) -> String {
    match target.episode {
        Some(episode) => format!(
            "{}@{}:{}",
            target.media_id.as_str(),
            episode.season,
            episode.episode
        ),
        None => target.media_id.as_str().to_owned(),
    }
}

fn progress(
    source: &ManualSource,
    target: PlaybackTarget,
    position: f64,
    updated_at: i64,
) -> PlaybackProgress {
    PlaybackProgress {
        target,
        source_id: SourceId::new(source.id().as_str()).unwrap(),
        position_seconds: position,
        duration_seconds: Some(120.0),
        updated_at,
        completed: false,
        origin: ProgressOrigin::Mpv,
    }
}

fn eventually(timeout: Duration, description: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {description}");
}

fn bstring(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(format!("{}:", value.len()).as_bytes());
    out.extend_from_slice(value);
}

/// Canonical multi-file bencode, with SHA-1 over the exact concatenated payload.
fn metainfo(files: &[(&str, Vec<u8>)], private: bool) -> Vec<u8> {
    let all: Vec<u8> = files
        .iter()
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect();
    let hashes: Vec<u8> = all
        .chunks(16 * 1024)
        .flat_map(|piece| Sha1::digest(piece).to_vec())
        .collect();
    let mut out = b"d4:infod5:filesl".to_vec();
    for (name, bytes) in files {
        out.extend_from_slice(format!("d6:lengthi{}e4:pathl", bytes.len()).as_bytes());
        bstring(&mut out, name.as_bytes());
        out.extend_from_slice(b"ee");
    }
    out.extend_from_slice(b"e4:name12:owned-series12:piece lengthi16384e6:pieces");
    bstring(&mut out, &hashes);
    if private {
        out.extend_from_slice(b"7:privatei1e");
    }
    out.extend_from_slice(b"ee");
    out
}

#[derive(Clone, Copy, Debug)]
enum HttpBehavior {
    Stable,
    IgnoresRanges,
    NoValidator,
    StallHead,
}

/// Minimal owned origin, not kadr-media::Server: independently implements strong
/// ETag + conditional ranges, so the HTTP -> RemoteSource boundary is exercised.
struct HttpOrigin {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    heads: Arc<AtomicUsize>,
    ranges: Arc<AtomicUsize>,
    conditional: Arc<AtomicUsize>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HttpOrigin {
    fn new(bytes: Vec<u8>, behavior: HttpBehavior) -> Self {
        let bytes: Arc<[u8]> = bytes.into();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let heads = Arc::new(AtomicUsize::new(0));
        let ranges = Arc::new(AtomicUsize::new(0));
        let conditional = Arc::new(AtomicUsize::new(0));
        let (done, head_count, range_count, conditional_count) = (
            stop.clone(),
            heads.clone(),
            ranges.clone(),
            conditional.clone(),
        );
        let worker = thread::spawn(move || {
            let mut connections: Vec<thread::JoinHandle<()>> = Vec::new();
            while !done.load(Ordering::Relaxed) {
                for index in (0..connections.len()).rev() {
                    if connections[index].is_finished() {
                        connections.swap_remove(index).join().unwrap();
                    }
                }
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        if connections.len() == 16 {
                            continue;
                        }
                        let (bytes, done, heads, ranges, conditional) = (
                            bytes.clone(),
                            done.clone(),
                            head_count.clone(),
                            range_count.clone(),
                            conditional_count.clone(),
                        );
                        // Playback and permanent saving use independent GETs.
                        // A slow player must not monopolize the fixture origin.
                        connections.push(thread::spawn(move || {
                            socket
                                .set_read_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            socket
                                .set_write_timeout(Some(Duration::from_secs(1)))
                                .unwrap();
                            if let Err(error) = serve_origin(
                                &mut socket,
                                &bytes,
                                behavior,
                                &done,
                                &heads,
                                &ranges,
                                &conditional,
                            ) {
                                eprintln!("owned HTTP origin ({behavior:?}): {:?}", error.kind());
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
            for connection in connections {
                connection.join().unwrap();
            }
        });
        Self {
            address,
            stop,
            heads,
            ranges,
            conditional,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/own.mp4?token={PRIVATE_MARKER}", self.address)
    }
}

impl Drop for HttpOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("owned HTTP origin cleanup");
        }
    }
}

#[test]
fn owned_origin_serves_a_second_request_while_head_is_pending() {
    let bytes = vec![42; 1024];
    let origin = HttpOrigin::new(bytes.clone(), HttpBehavior::StallHead);
    let mut head = TcpStream::connect(origin.address).unwrap();
    head.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    head.write_all(b"HEAD /own.mp4 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    eventually(WAIT, "owned HEAD request is pending", || {
        origin.heads.load(Ordering::Relaxed) == 1
    });
    let mut get = TcpStream::connect(origin.address).unwrap();
    get.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    get.write_all(b"GET /own.mp4 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut response = Vec::new();
    get.read_to_end(&mut response).unwrap();
    let split = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert_eq!(&response[split + 4..], bytes);

    let before = Instant::now();
    drop(origin);
    assert!(before.elapsed() < Duration::from_secs(3));
    assert_eq!(head.read(&mut [0; 1]).unwrap(), 0);
}

fn serve_origin(
    socket: &mut TcpStream,
    bytes: &[u8],
    behavior: HttpBehavior,
    stop: &AtomicBool,
    heads: &AtomicUsize,
    ranges: &AtomicUsize,
    conditional: &AtomicUsize,
) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let size = socket.read(&mut chunk)?;
        if size == 0 || request.len() + size > 16 * 1024 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..size]);
    }
    let request = String::from_utf8_lossy(&request);
    let head = request.starts_with("HEAD ");
    if head {
        heads.fetch_add(1, Ordering::Relaxed);
    }
    if head && matches!(behavior, HttpBehavior::StallHead) {
        // Bounded independently of the caller; Drop can interrupt this wait.
        let deadline = Instant::now() + WAIT;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        return Ok(());
    }
    let header = |key: &str| {
        request.lines().skip(1).find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(key).then(|| value.trim())
        })
    };
    if let Some(value) = header("If-Match") {
        if value != "\"owned-etag-v1\"" {
            socket.write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
        conditional.fetch_add(1, Ordering::Relaxed);
    }
    let range = header("Range")
        .filter(|_| !matches!(behavior, HttpBehavior::IgnoresRanges))
        .and_then(|value| value.strip_prefix("bytes="))
        .and_then(|value| value.split_once('-'))
        .and_then(|(start, end)| {
            Some((
                start.parse::<usize>().ok()?,
                end.parse::<usize>().ok().unwrap_or(bytes.len() - 1),
            ))
        });
    let (start, end, status) = match range {
        Some((start, end)) if start < bytes.len() && start <= end => {
            ranges.fetch_add(1, Ordering::Relaxed);
            (start, end.min(bytes.len() - 1), "206 Partial Content")
        }
        Some(_) => {
            write!(
                socket,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                bytes.len()
            )?;
            return Ok(());
        }
        None => (0, bytes.len() - 1, "200 OK"),
    };
    write!(
        socket,
        "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        end - start + 1
    )?;
    if matches!(behavior, HttpBehavior::Stable | HttpBehavior::IgnoresRanges) {
        socket.write_all(b"ETag: \"owned-etag-v1\"\r\n")?;
    }
    if range.is_some() {
        write!(
            socket,
            "Content-Range: bytes {start}-{end}/{}\r\n",
            bytes.len()
        )?;
    }
    socket.write_all(b"\r\n")?;
    if !head {
        socket.set_nonblocking(true)?;
        write_origin_body(socket, &bytes[start..=end], stop)?;
    }
    Ok(())
}

fn write_origin_body(
    writer: &mut impl Write,
    bytes: &[u8],
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let deadline = Instant::now() + WAIT;
    let mut written = 0;
    while written < bytes.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let end = bytes.len().min(written + 64 * 1024);
        match writer.write(&bytes[written..end]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // Socket backpressure is not EOF. Retry partial writes within
                // the existing whole-response budget, observing Stop each time.
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[test]
fn owned_origin_body_retries_backpressure_without_losing_bytes_and_observes_stop() {
    struct Partial {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            assert!(bytes.len() <= 64 * 1024);
            if self.calls % 3 == 1 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let count = bytes.len().min(137);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let expected = vec![37; 1024];
    let mut writer = Partial {
        bytes: Vec::new(),
        calls: 0,
    };
    let stop = AtomicBool::new(false);
    write_origin_body(&mut writer, &expected, &stop).unwrap();
    assert_eq!(writer.bytes, expected);
    stop.store(true, Ordering::Relaxed);
    assert_eq!(
        write_origin_body(&mut writer, &expected, &stop)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(writer.bytes, expected, "Stop must not write any new bytes");
}

/// Kill/reap ffmpeg on timeout or an unwinding test.
struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn generate_video(path: &Path) -> Vec<u8> {
    generate_video_from(path, "testsrc2=size=160x90:rate=10")
}

fn generate_video_from(path: &Path, video_source: &str) -> Vec<u8> {
    let mut child = OwnedChild(
        ProcessCommand::new("ffmpeg")
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                video_source,
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=16000",
                "-t",
                "24",
                "-c:v",
                "mpeg4",
                "-b:v",
                "192k",
                "-g",
                "10",
                "-c:a",
                "aac",
                "-b:a",
                "24k",
                "-movflags",
                "+faststart",
                "-threads",
                "1",
            ])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("opt-in real-media test requires ffmpeg on PATH"),
    );
    eventually(
        WAIT,
        "ffmpeg to generate our own MPEG4/AAC fixture",
        || match child.0.try_wait().expect("poll owned ffmpeg") {
            Some(status) => {
                assert!(status.success(), "ffmpeg fixture generation failed");
                true
            }
            None => false,
        },
    );
    fs::read(path).expect("read generated media fixture")
}

fn measured(storage: &Storage, target: &PlaybackTarget, minimum: f64) -> PlaybackProgress {
    let mut result = None;
    eventually(
        WAIT,
        "mpv IPC measured position persisted in SQLite",
        || {
            result = storage
                .load_progress(target)
                .expect("read backend progress");
            result
                .as_ref()
                .is_some_and(|value| value.position_seconds >= minimum)
        },
    );
    let result = result.unwrap();
    assert_eq!(
        result.origin,
        ProgressOrigin::Mpv,
        "progress must come from actual mpv IPC"
    );
    assert!(
        !result.completed,
        "process state alone is not proof of completion"
    );
    assert!((result.duration_seconds.expect("real fixture duration") - 24.0).abs() < 0.5);
    result
}

#[test]
#[ignore = "requires installed ffmpeg and mpv; real MPEG4/AAC HTTP/IPC/SQLite/save/resume acceptance"]
fn real_http_mpv_atomic_save_clear_cache_library_resume_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let bytes = generate_video(&directory.path().join("generated.mp4"));
    let origin = HttpOrigin::new(bytes.clone(), HttpBehavior::Stable);
    let source = ManualSource::parse_link(&origin.url()).unwrap();
    let target = target(
        &source,
        Some(EpisodeKey {
            season: 1,
            episode: 1,
        }),
    );
    let other = PlaybackTarget {
        media_id: target.media_id.clone(),
        episode: Some(EpisodeKey {
            season: 1,
            episode: 2,
        }),
    };
    let descriptor = StoredSource::Link(origin.url());
    let untouched = progress(&source, other.clone(), 37.0, 1);
    let mut storage = Storage::open_default(&paths).unwrap();
    storage
        .save_progress_for_source(&untouched, &descriptor)
        .unwrap();

    let mut app = App::start(&paths);
    app.ready();
    let disabled = SourcePreferences {
        commons_enabled: false,
        rutracker_enabled: false,
        torznab: Vec::new(),
        ..SourcePreferences::default()
    };
    app.command(Command::SetSourcePreferences(disabled.clone()));
    app.wait(
        "all automatic providers disabled",
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &disabled),
    );
    app.command(Command::SelectSource(source));
    app.selected();
    app.command(Command::AssignEpisode(1, 1));
    app.wait(
        "episode assignment",
        |event| matches!(event, Event::Status(Level::Info, message) if message.contains("эпизода")),
    );
    app.command(Command::Play { resume: false });
    app.wait("real headless mpv active", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    measured(&storage, &target, 3.0);
    assert!(
        origin.ranges.load(Ordering::Relaxed) > 0,
        "mpv must read through RemoteSource ranges"
    );
    assert!(
        origin.conditional.load(Ordering::Relaxed) > 0,
        "RemoteSource must condition reads on ETag"
    );

    let inside_cache = paths.cache_dir().join("torrents-v1/forbidden.mp4");
    app.command(Command::SaveTo(inside_cache.clone()));
    app.wait("save inside disposable cache rejected", |event| {
        matches!(event, Event::Status(Level::Error, _))
    });
    assert!(!inside_cache.exists());
    let saved = directory.path().join("saved-owned.mp4");
    app.command(Command::SaveTo(saved.clone()));
    eventually(WAIT, "atomic saved file and durable library path", || {
        if saved.exists() {
            assert_eq!(
                fs::read(&saved).unwrap(),
                bytes,
                "final destination must never expose partial bytes"
            );
        }
        storage.saved_file(&target).unwrap().is_some()
    });
    assert_eq!(storage.saved_file(&target).unwrap().unwrap().path, saved);
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    assert_eq!(
        fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".part"))
            .count(),
        0
    );
    app.stopped();
    let saved_position = storage.load_progress(&target).unwrap().unwrap();
    app.command(Command::ClearCache);
    app.wait(
        "inactive cache cleared after save",
        |event| matches!(event, Event::CacheUsage(usage) if usage.active_leases == 0),
    );
    app.finish();
    assert_eq!(
        storage.load_progress(&other).unwrap(),
        Some(untouched.clone()),
        "episodes must have isolated progress"
    );
    drop(origin); // Saved-file reopen must not depend on the original URL still working.

    let mut app = App::start(&paths);
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("library prefers saved file after restart", |event| {
        matches!(
            event,
            Event::SourceSelected {
                kind: None,
                playable: true,
                ..
            }
        )
    });
    app.command(Command::Play { resume: true });
    app.wait("saved copy opened in real mpv", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    let resumed = measured(&storage, &target, saved_position.position_seconds + 1.0);
    assert!(
        resumed.position_seconds < saved_position.position_seconds + 6.0,
        "resume must use saved position, not a wall-clock estimate"
    );
    app.stopped();
    app.command(Command::Play { resume: false });
    app.wait("explicit restart opened real mpv", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    eventually(
        Duration::from_secs(4),
        "restart overwrites progress with measured near-zero position",
        || {
            storage
                .load_progress(&target)
                .unwrap()
                .is_some_and(|value| {
                    value.updated_at >= resumed.updated_at
                        && value.position_seconds < saved_position.position_seconds
                })
        },
    );
    // Preserve the old terminal-feedback contract using an actual SQLite write
    // failure, not a synthetic player snapshot or an invented elapsed clock.
    let connection = rusqlite::Connection::open(paths.database_path()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER acceptance_refuse_progress BEFORE INSERT ON playback_progress
         BEGIN SELECT RAISE(FAIL, 'acceptance injected write failure'); END;",
        )
        .unwrap();
    let before_failure = storage.load_progress(&target).unwrap();
    while let Ok(event) = app.events.try_recv() {
        assert_public_feedback_is_private(&event);
    }
    for _ in 0..2 {
        app.wait(
            "real telemetry tick while SQLite refuses progress",
            |event| matches!(event, Event::Transfer(Some(_))),
        );
    }
    app.stopped();
    app.wait(
        "terminal feedback must expose failed measured-progress write",
        |event| {
            matches!(event, Event::Status(Level::Warning, message)
            if message.contains("не удалось сохранить") && !message.contains("позиция сохранена"))
        },
    );
    assert_eq!(
        storage.load_progress(&target).unwrap(),
        before_failure,
        "failed writes preserve prior measured progress"
    );
    connection
        .execute_batch("DROP TRIGGER acceptance_refuse_progress;")
        .unwrap();
    drop(connection);

    // Quit while playing: worker must stop/reap its owned player and HTTP server.
    app.command(Command::Play { resume: true });
    app.wait("player active before application shutdown", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    app.finish();
    assert_eq!(
        fs::read(saved).unwrap(),
        bytes,
        "cache cleanup must never delete user-owned copy"
    );
    assert_eq!(storage.load_progress(&other).unwrap(), Some(untouched));
}

#[test]
#[ignore = "requires ffmpeg and mpv; direct HTTP without validators/Range still supports atomic whole-response save"]
fn real_http_fallback_saves_origins_without_validators_or_range_support() {
    let directory = tempfile::tempdir().unwrap();
    let bytes = generate_video(&directory.path().join("generated.mp4"));
    for (index, behavior) in [HttpBehavior::NoValidator, HttpBehavior::IgnoresRanges]
        .into_iter()
        .enumerate()
    {
        let root = directory.path().join(format!("profile-{index}"));
        fs::create_dir(&root).unwrap();
        let paths = paths(&root);
        let origin = HttpOrigin::new(bytes.clone(), behavior);
        let source = ManualSource::parse_link(&origin.url()).unwrap();
        let target = target(&source, None);
        let mut app = App::start(&paths);
        app.ready();
        app.command(Command::SelectSource(source));
        app.selected();
        app.command(Command::Play { resume: false });
        app.wait("direct HTTP player launched", |event| {
            matches!(event, Event::PlaybackActive(true))
        });
        let storage = Storage::open(&paths.database_path()).unwrap();
        measured(&storage, &target, 1.0);
        let destination = root.join("permanent.mp4");
        app.command(Command::SaveTo(destination.clone()));
        app.until(
            WAIT,
            &format!("whole-response save commits without Range or ETag ({behavior:?})"),
            || storage.saved_file(&target).unwrap().is_some(),
        );
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        app.stopped();
        app.command(Command::ClearCache);
        app.wait("cache cleanup leaves direct HTTP permanent copy", |event| {
            matches!(event, Event::CacheUsage(_))
        });
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        app.finish();
    }
}

/// Control only this test's direct child. Never log private process arguments.
fn seek_owned_mpv(seconds: f64) {
    let children = ProcessCommand::new("pgrep")
        .args(["-P", &std::process::id().to_string(), "-x", "mpv"])
        .output()
        .expect("query owned mpv process");
    assert!(children.status.success(), "our mpv child must be alive");
    let pids = String::from_utf8(children.stdout).unwrap();
    let pid = pids.split_whitespace().next().expect("owned player PID");
    let process = ProcessCommand::new("ps")
        .args(["-ww", "-p", pid, "-o", "command="])
        .output()
        .expect("query owned player's IPC argument");
    let command = String::from_utf8(process.stdout).unwrap();
    let endpoint = command
        .split_whitespace()
        .find_map(|argument| argument.strip_prefix("--input-ipc-server="))
        .expect("owned player exposes its managed IPC endpoint");
    let mut socket = std::os::unix::net::UnixStream::connect(endpoint)
        .expect("connect a second player-control IPC client");
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let request =
        serde_json::json!({"command":["seek",seconds,"absolute+exact"],"request_id":4242});
    socket.write_all(request.to_string().as_bytes()).unwrap();
    socket.write_all(b"\n").unwrap();
    let mut reply = Vec::new();
    loop {
        let mut byte = [0];
        socket
            .read_exact(&mut byte)
            .expect("bounded player seek reply");
        if byte[0] == b'\n' {
            let value: serde_json::Value =
                serde_json::from_slice(&reply).expect("valid player JSON");
            if value["request_id"].as_u64() == Some(4242) {
                assert_eq!(
                    value["error"].as_str(),
                    Some("success"),
                    "player accepted seek"
                );
                return;
            }
            reply.clear();
        } else {
            assert!(reply.len() < 64 * 1024, "bounded seek/event message");
            reply.push(byte[0]);
        }
    }
}

struct LocalSeed {
    runtime: tokio::runtime::Runtime,
    session: Option<kadr_torrent::LocalSession>,
    _selected: kadr_torrent::SelectedTorrent,
}

impl LocalSeed {
    fn start(output: &Path, metadata: Vec<u8>) -> Self {
        let output = output
            .canonicalize()
            .expect("resolve owned local seed fixture directory");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (session, selected) = runtime.block_on(async {
            tokio::time::timeout(WAIT, async {
                let session = kadr_torrent::LocalSession::new(output.clone())
                    .await
                    .expect("start discovery-disabled loopback-only seed");
                let selected = session
                    .add_selected(metadata.into(), 0, vec![], true)
                    .await
                    .expect("rehash our generated seed video");
                assert!(selected.source.stats().finished,
                    "the generated seed must contain all verified pieces before starting the downloader: {:?}",
                    selected.source.stats());
                (session, selected)
            })
            .await
            .expect("seed initialization must be bounded")
        });
        Self {
            runtime,
            session: Some(session),
            _selected: selected,
        }
    }

    fn address(&self) -> SocketAddr {
        self.session.as_ref().unwrap().listen_addr()
    }
}

impl Drop for LocalSeed {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let result = self.runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(3), session.stop()).await
            });
            assert!(
                result.is_ok() || thread::panicking(),
                "local seed shutdown exceeded deadline"
            );
        }
    }
}

#[test]
#[ignore = "requires ffmpeg and mpv; real loopback torrent -> HTTP -> IPC, active cache lease and atomic save"]
fn real_torrent_plays_before_completion_protects_active_cache_and_saves_verified_bytes() {
    use kadr_torrent::engine::{EngineOptions, PreparedTorrent, RateLimit as EngineRate};

    let directory = tempfile::tempdir().unwrap();
    let paths = paths(directory.path());
    let bytes = generate_video(&directory.path().join("generated.mp4"));
    let filename = "Own.S01E01.mp4";
    let metadata = metainfo(&[(filename, bytes.clone())], true);
    let prepared = PreparedTorrent::from_bytes(metadata.clone().into()).unwrap();
    let seed_root = directory.path().join("seed");
    // librqbit intentionally omits its default subfolder for a single entry,
    // including a one-entry multi-file metainfo. The production engine uses an
    // explicit payload directory; this independent local seed uses defaults.
    fs::create_dir_all(&seed_root).unwrap();
    fs::write(seed_root.join(filename), &bytes).unwrap();
    let seed = LocalSeed::start(&seed_root, metadata.clone());
    let torrent_path = directory.path().join("owned-video.torrent");
    fs::write(&torrent_path, metadata).unwrap();
    let source = ManualSource::torrent_file(torrent_path).unwrap();
    let target = PlaybackTarget {
        media_id: MediaId::new(format!("{}:file:0", source.id().as_str())).unwrap(),
        episode: Some(EpisodeKey {
            season: 1,
            episode: 1,
        }),
    };
    // Limit only loopback payload. No DHT, trackers, public bootstrap or uploads.
    // This rate intentionally leaves enough time for a real measured position
    // while verified selected-file bytes are still strictly below completion.
    let options = EngineOptions {
        network_enabled: true,
        discovery_enabled: false,
        initial_peers: vec![seed.address()],
        download_limit: EngineRate::Bps(64 * 1024),
        upload_limit: EngineRate::Paused,
        peer_limit: 1,
    };
    let (backend, events) = Engine::start_with_network(Some(paths.clone()), true, Some(options))
        .expect("start backend with explicit loopback peer");
    let mut app = App { backend, events };
    let mut settings = app.ready();
    settings.network.torrents_enabled = true; // Override must not bypass consent.
    settings.network.download = RateLimit::BytesPerSecond(64 * 1024);
    settings.network.upload = RateLimit::Paused;
    settings.network.max_connections = 1;
    settings.cache_limit_bytes = 1024 * 1024;
    assert!(
        bytes.len() < settings.cache_limit_bytes as usize,
        "generated fixture must fit the test quota"
    );
    app.command(Command::UpdateSettings(settings.clone()));
    app.wait("explicit BitTorrent consent saved", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    app.command(Command::SelectSource(source));
    app.selected();
    app.command(Command::Play { resume: false });
    app.wait(
        "real mpv launched against verified torrent HTTP stream",
        |event| matches!(event, Event::PlaybackActive(true)),
    );
    let storage = Storage::open(&paths.database_path()).unwrap();
    app.wait_for(
        Duration::from_secs(15),
        "actual mpv progress before selected video completes",
        |event| {
            let Event::Transfer(Some(transfer)) = event else {
                return false;
            };
            transfer.done.is_some_and(|done| done > 0)
                && transfer
                    .done
                    .zip(transfer.total)
                    .is_some_and(|(done, total)| done < total)
                && transfer
                    .buffered
                    .is_some_and(|buffered| buffered < bytes.len() as u64)
                && storage
                    .load_progress(&target)
                    .unwrap()
                    .is_some_and(|value| {
                        value.origin == ProgressOrigin::Mpv && value.position_seconds >= 1.0
                    })
        },
    );
    let first = measured(&storage, &target, 1.0);
    assert!(
        first.position_seconds < 20.0,
        "partial playback must be observed before fixture end"
    );
    seek_owned_mpv(12.0);
    measured(&storage, &target, 13.0);
    seek_owned_mpv(2.0);
    eventually(
        WAIT,
        "player continues after a backward seek through verified bytes",
        || {
            storage
                .load_progress(&target)
                .unwrap()
                .is_some_and(|progress| (3.0..6.0).contains(&progress.position_seconds))
        },
    );

    let cached_video = paths
        .cache_dir()
        .join("torrents-v1/data")
        .join(&prepared.info_hash)
        .join("payload")
        .join(filename);
    assert!(
        cached_video.is_file(),
        "active torrent must own its reserved payload"
    );
    app.command(Command::ClearCache);
    app.wait("active reservation survives ClearCache within quota", |event| {
        matches!(event, Event::CacheUsage(usage) if usage.active_entries == 1
            && usage.active_leases >= 1 && usage.reserved_bytes == prepared.total_bytes
            && usage.actual_bytes <= usage.reserved_bytes && usage.reserved_bytes <= usage.limit_bytes)
    });
    assert!(
        cached_video.is_file(),
        "ClearCache must not unlink active payload"
    );
    let mut forbidden_change = settings.clone();
    forbidden_change.network.max_connections = 2;
    app.command(Command::UpdateSettings(forbidden_change));
    app.wait("active-session network change refused", |event| {
        matches!(event, Event::Status(Level::Warning, message) if message.contains("Остановите"))
    });
    assert_eq!(
        storage.load_settings().unwrap(),
        settings.clone().into_storage(),
        "active cache/network policy remains stable"
    );

    let saved = directory.path().join("saved-torrent.mp4");
    app.command(Command::SaveTo(saved.clone()));
    eventually(
        Duration::from_secs(18),
        "torrent completes verified atomic user-owned save",
        || {
            if saved.exists() {
                assert_eq!(
                    fs::read(&saved).unwrap(),
                    bytes,
                    "published save must contain all verified bytes"
                );
            }
            storage.saved_file(&target).unwrap().is_some()
        },
    );
    app.stopped();
    app.command(Command::ClearCache);
    app.wait("inactive torrent cache removed", |event| {
        matches!(event, Event::CacheUsage(usage) if usage.active_leases == 0
            && usage.active_entries == 0 && usage.reserved_bytes == 0 && usage.actual_bytes == 0)
    });
    assert!(
        !cached_video.exists(),
        "inactive cached payload should now be disposable"
    );
    assert_eq!(
        fs::read(&saved).unwrap(),
        bytes,
        "saved file survives torrent cache deletion"
    );
    app.finish();
    drop(seed);

    // Reopen without a seed or network override. The saved file is authoritative.
    let mut app = App::start(&paths);
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait(
        "torrent library entry now uses permanent saved copy",
        |event| {
            matches!(
                event,
                Event::SourceSelected {
                    kind: None,
                    playable: true,
                    ..
                }
            )
        },
    );
    app.command(Command::Play { resume: true });
    app.wait("saved torrent copy resumed offline in real mpv", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    let before = storage.load_progress(&target).unwrap().unwrap();
    measured(&storage, &target, before.position_seconds + 0.5);
    app.finish();
}
