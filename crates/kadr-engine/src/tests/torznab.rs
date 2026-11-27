//! Offline command/event acceptance of configured Torznab discovery and selection.
use super::*;
use std::sync::Mutex;

use kadr_core::sources::{Locator, MediaSource};
use kadr_torrent::engine::PreparedTorrent;

mod command_boundary;

struct TorznabOrigin {
    endpoint: String,
    stop: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
    replace: Arc<AtomicBool>,
    caps: Arc<AtomicUsize>,
    searches: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TorznabOrigin {
    fn new(metadata: Vec<u8>, empty: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let fail = Arc::new(AtomicBool::new(false));
        let replace = Arc::new(AtomicBool::new(false));
        let caps = Arc::new(AtomicUsize::new(0));
        let searches = Arc::new(AtomicUsize::new(0));
        let downloads = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let hash = PreparedTorrent::from_bytes(metadata.clone().into())
            .unwrap()
            .info_hash;
        let replacement = metainfo(&[("Fixture.mp4", vec![9; 1024])], true);
        let (done, failed, replaced, cap_count, search_count, download_count, captured) = (
            stop.clone(),
            fail.clone(),
            replace.clone(),
            caps.clone(),
            searches.clone(),
            downloads.clone(),
            requests.clone(),
        );
        let origin = endpoint.trim_end_matches("/api").to_owned();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let (mut socket, _) = match accept_http(&listener) {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => break,
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 16_384);
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                let request = header.lines().next().unwrap().to_owned();
                captured.lock().unwrap().push(request.clone());
                let body = if request.contains("/download?") {
                    download_count.fetch_add(1, Ordering::Relaxed);
                    if replaced.load(Ordering::Relaxed) {
                        replacement.clone()
                    } else {
                        metadata.clone()
                    }
                } else if request.contains("t=caps") {
                    cap_count.fetch_add(1, Ordering::Relaxed);
                    b"<caps><limits max='25' default='10'/><searching><movie-search available='yes' supportedParams='q'/></searching></caps>".to_vec()
                } else {
                    search_count.fetch_add(1, Ordering::Relaxed);
                    let item = if empty {
                        String::new()
                    } else {
                        format!(
                            "<item><title>Fixture 2020 1080p</title><guid>fixture</guid><t:attr name='category' value='2000'/><t:attr name='infohash' value='{hash}'/><enclosure type='application/x-bittorrent' url='{origin}/download?id=1&amp;apikey={PRIVATE_MARKER}'/></item>"
                        )
                    };
                    format!("<rss version='2.0' xmlns:t='http://torznab.com/schemas/2015/feed'><channel><title>Fixture</title><link>{origin}/</link><description>Owned fixture</description>{item}</channel></rss>").into_bytes()
                };
                let status = if failed.load(Ordering::Relaxed) {
                    "503 Unavailable"
                } else {
                    "200 OK"
                };
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                let _ = socket.write_all(&body);
            }
        });
        Self {
            endpoint,
            stop,
            fail,
            replace,
            caps,
            searches,
            downloads,
            requests,
            worker: Some(worker),
        }
    }

    fn config(&self) -> TorznabConfig {
        TorznabConfig {
            name: "Owned mock".into(),
            base_url: self.endpoint.clone(),
            api_key_env: Some("KADR_TORZNAB_TEST_KEY".into()),
            enabled: true,
        }
    }
}

impl Drop for TorznabOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn finished(app: &mut App, succeeded: usize, failures: usize) -> Vec<SourceCandidate> {
    let Event::SourceDiscovery {
        rows,
        failures: errors,
        ..
    } = app.wait("completed Torznab discovery", |event| {
        matches!(event, Event::SourceDiscovery { pending: 0, succeeded: n, failures: errors, .. }
            if *n == succeeded && errors.len() == failures)
    })
    else {
        unreachable!()
    };
    assert!(errors.iter().all(|error| !error.contains(PRIVATE_MARKER)));
    rows
}

fn catalog_fixture(db: &mut Storage) -> PlaybackTarget {
    let id = MediaId::new("trakt:movie:52").unwrap();
    db.upsert_media(&kadr_storage::MediaRecord {
        id: id.clone(),
        title: "Fixture".into(),
        original_title: None,
        year: Some(2020),
        kind: kadr_storage::MediaKind::Movie,
        description: String::new(),
        poster_url: None,
        attribution: String::new(),
    })
    .unwrap();
    db.set_favorite(&id, true).unwrap();
    PlaybackTarget {
        media_id: id,
        episode: None,
    }
}

/// Invoked by the same opt-in `tests::real_` suite as manual torrent playback.
pub(super) fn play_owned_torrent() {
    use kadr_torrent::engine::EngineOptions;

    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let file = "Fixture.mp4";
    let bytes = generate_video(&root.path().join(file));
    let metadata = metainfo(&[(file, bytes)], true);
    let seed = LocalSeed::start(root.path(), metadata.clone());
    let origin = TorznabOrigin::new(metadata, false);
    let mut db = Storage::open_default(&paths).unwrap();
    let target = catalog_fixture(&mut db);
    let mut settings = db.load_settings().unwrap();
    settings.network.torrents_enabled = true;
    // The mock also serves absolute HTTP request targets. Do not inherit a
    // workstation proxy or contact a real catalog, tracker, or swarm.
    settings.catalog_proxy = Some(origin.endpoint.trim_end_matches("/api").to_owned());
    db.save_settings(&settings).unwrap();
    db.save_source_preferences(&SourcePreferences {
        rutracker_enabled: false,
        torznab: vec![TorznabConfig {
            base_url: format!("{}/", origin.endpoint),
            api_key_env: None,
            ..origin.config()
        }],
        ..Default::default()
    })
    .unwrap();
    let options = EngineOptions {
        network_enabled: true,
        discovery_enabled: false,
        initial_peers: vec![seed.address()],
        ..Default::default()
    };
    let (backend, events) = Engine::start_with_network(Some(paths), true, Some(options)).unwrap();
    let mut app = App { backend, events };
    app.ready();
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("fixture card opened", |event| {
        matches!(event, Event::MediaSelected { media: Some(_), .. })
    });
    app.command(Command::FindSources);
    let rows = finished(&mut app, 1, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(origin.downloads.load(Ordering::Relaxed), 0);
    assert!(
        origin
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.contains("/api/?"))
    );
    app.command(Command::ChooseSource(rows[0].index));
    app.selected();
    assert_eq!(origin.downloads.load(Ordering::Relaxed), 1);
    app.command(Command::Play { resume: false });
    app.wait("mpv launched for Torznab release", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    assert_eq!(measured(&db, &target, 1.0).origin, ProgressOrigin::Mpv);
    app.stopped();
    app.finish();
}

#[test]
fn chosen_offer_and_partial_force_cache_use_only_owned_loopback_data() {
    // Do not mutate the environment of a multithreaded Rust test process.
    // Re-exec just this test with an isolated fixture key and proxies disabled.
    if std::env::var("KADR_TORZNAB_TEST_KEY").ok().as_deref() != Some(PRIVATE_MARKER) {
        let status = ProcessCommand::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::torznab::chosen_offer_and_partial_force_cache_use_only_owned_loopback_data",
                "--nocapture",
            ])
            .env("KADR_TORZNAB_TEST_KEY", PRIVATE_MARKER)
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .status()
            .unwrap();
        assert!(status.success(), "isolated Torznab acceptance test failed");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let metadata = metainfo(&[("Fixture.mp4", vec![3; 2 * 1024 * 1024])], true);
    let first = TorznabOrigin::new(metadata.clone(), false);
    let second = TorznabOrigin::new(metadata, true);
    second.fail.store(true, Ordering::Relaxed);
    let mut db = Storage::open_default(&paths).unwrap();
    let target = catalog_fixture(&mut db);
    let mut app = App::start(&paths);
    let mut settings = app.ready();
    settings.catalog_enabled = true;
    settings.network.torrents_enabled = true;
    settings.cache_limit_bytes = 1024 * 1024;
    app.command(Command::UpdateSettings(settings.clone()));
    app.wait("network consent saved", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    let mut missing = first.config();
    missing.base_url.push_str("/missing");
    missing.api_key_env = Some(format!("KADR_TORZNAB_MISSING_TEST_{}", std::process::id()));
    assert!(std::env::var(missing.api_key_env.as_ref().unwrap()).is_err());
    let preferences = SourcePreferences {
        rutracker_enabled: false,
        torznab: vec![first.config(), second.config(), missing],
        ..Default::default()
    };
    app.command(Command::SetSourcePreferences(preferences.clone()));
    app.wait(
        "enabled Torznab preferences accepted",
        |event| matches!(event, Event::SourcePreferences(saved) if *saved == preferences),
    );
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("local catalog card opened", |event| {
        matches!(event, Event::MediaSelected { media: Some(_), .. })
    });

    app.command(Command::FindSources);
    let rows = finished(&mut app, 1, 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, SourceKind::TorrentFile);
    assert_eq!(rows[0].origin, "Owned mock");
    assert_eq!(
        first.downloads.load(Ordering::Relaxed),
        0,
        "discovery must not fetch metainfo"
    );
    assert_eq!(first.caps.load(Ordering::Relaxed), 1);
    assert_eq!(second.caps.load(Ordering::Relaxed), 1);

    // A partial hit keeps the successful provider's batch and retries only failures.
    app.command(Command::FindSources);
    finished(&mut app, 1, 2);
    assert_eq!(first.caps.load(Ordering::Relaxed), 1);
    assert_eq!(second.caps.load(Ordering::Relaxed), 2);
    second.fail.store(false, Ordering::Relaxed);
    app.command(Command::FindSources);
    finished(&mut app, 2, 1);
    assert_eq!(first.caps.load(Ordering::Relaxed), 1);
    assert_eq!(second.caps.load(Ordering::Relaxed), 3);
    app.command(Command::FindSources);
    finished(&mut app, 2, 1);
    assert_eq!(
        second.caps.load(Ordering::Relaxed),
        3,
        "successful empty batch is cached"
    );
    app.command(Command::RetrySources);
    let rows = finished(&mut app, 2, 1);
    assert_eq!(first.caps.load(Ordering::Relaxed), 2);
    assert_eq!(
        second.caps.load(Ordering::Relaxed),
        4,
        "force bypasses every valid provider cache"
    );
    assert_eq!(first.searches.load(Ordering::Relaxed), 2);

    let key = "resolve-v2:trakt:movie:52";
    let provider = format!(
        "torznab:{:x}",
        sha2::Sha256::digest(first.endpoint.as_bytes())
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let data = db.source_cache_get(key, &provider, now).unwrap().unwrap();
    let text = String::from_utf8(data.clone()).unwrap();
    assert!(!text.contains(PRIVATE_MARKER) && !text.contains("apikey"));
    let mut offers: Vec<MediaSource> = serde_json::from_slice(&data).unwrap();
    assert!(
        matches!(&offers[0].locator, Locator::TorrentUrl(url) if url.ends_with("/download?id=1"))
    );

    app.command(Command::ChooseSource(rows[0].index));
    app.selected();
    assert_eq!(first.downloads.load(Ordering::Relaxed), 1);
    // Existing playback consumes PreparedTorrent (and rejects its total size
    // before any peer traffic/player launch); it never streams the enclosure.
    app.command(Command::Play { resume: false });
    app.wait(
        "remote torrent goes through existing whole-torrent reservation",
        |event| matches!(event, Event::Status(Level::Error, message) if message.contains("quota")),
    );
    assert!(
        db.source_for_target(&target).unwrap().is_none(),
        "remote enclosures have no resumable descriptor"
    );

    // Revalidate authority on a cache hit rather than granting cached URLs access.
    offers[0].locator = Locator::TorrentUrl(format!(
        "{}/download?id=1",
        second.endpoint.trim_end_matches("/api")
    ));
    db.source_cache_put(
        key,
        &provider,
        now,
        false,
        &serde_json::to_vec(&offers).unwrap(),
    )
    .unwrap();
    app.command(Command::FindSources);
    let rows = finished(&mut app, 2, 1);
    assert_eq!(
        first.caps.load(Ordering::Relaxed),
        3,
        "foreign cached origin requires a new search"
    );
    assert_eq!(second.downloads.load(Ordering::Relaxed), 0);
    first.replace.store(true, Ordering::Relaxed);
    app.command(Command::ChooseSource(rows[0].index));
    app.wait("replacement metainfo hash rejected", |event| {
        matches!(event, Event::Status(Level::Error, message) if message.contains("не соответствует"))
    });
    assert!(db.source_for_target(&target).unwrap().is_none());

    settings.network.torrents_enabled = false;
    app.command(Command::UpdateSettings(settings.clone()));
    app.wait("BitTorrent disabled", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    app.command(Command::FindSources);
    app.wait("providers require BitTorrent consent", |event| {
        matches!(event, Event::Status(Level::Warning, message) if message.contains("Нет включённых"))
    });
    settings.network.torrents_enabled = true;
    settings.catalog_enabled = false;
    app.command(Command::UpdateSettings(settings));
    app.wait("catalog disabled", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
    app.command(Command::FindSources);
    app.wait("providers require catalog consent", |event| {
        matches!(event, Event::Status(Level::Warning, message) if message.contains("Нет включённых"))
    });
    assert_eq!(first.caps.load(Ordering::Relaxed), 3);
    app.finish();
    assert!(
        first
            .requests
            .lock()
            .unwrap()
            .iter()
            // `/api` and `/api/missing` share an origin. Only configured API
            // requests get the key; the separate download path is anonymous.
            .all(|request| request.contains(PRIVATE_MARKER) != request.contains("/download?"))
    );
    assert!(db.source_for_target(&target).unwrap().is_none());
    assert!(
        !fs::read(paths.database_path())
            .unwrap()
            .windows(PRIVATE_MARKER.len())
            .any(|bytes| bytes == PRIVATE_MARKER.as_bytes())
    );
}
