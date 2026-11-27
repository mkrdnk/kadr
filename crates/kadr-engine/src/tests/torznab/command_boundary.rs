//! E2E-04/05 at the desktop's actual Engine command/event boundary, not GUI clicks.
//! Cards are cached Trakt library snapshots; no catalog server or public network.
use super::*;
use std::sync::mpsc;

mod recommendation;

/// One owned HTTP worker can retain A's socket while serving B. Channels mark
/// request receipt and the late reply attempt; no timing-dependent sleep releases.
struct Origin {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    stall_a: Arc<AtomicBool>,
    held: mpsc::Receiver<()>,
    client_closed: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    replied: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

fn reply(socket: &mut TcpStream, body: &[u8]) {
    // Cancellation may already have closed A's client socket. Attempting the
    // complete reply is required; TCP acceptance does not prove client delivery.
    let _ = write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .and_then(|()| socket.write_all(body));
}

impl Origin {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let endpoint = format!("{base}/api");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stall_a = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (held_tx, held) = mpsc::channel();
        let (closed_tx, client_closed) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (replied_tx, replied) = mpsc::channel();
        let (captured, stall, done) = (requests.clone(), stall_a.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let a = metainfo(&[("Fixture.mp4", vec![3; 1024])], true);
            let b = metainfo(&[("Other.mp4", vec![7; 1024])], true);
            let mut delayed: Option<(TcpStream, Vec<u8>)> = None;
            let mut closed = false;
            while !done.load(Ordering::SeqCst) {
                if let Some((socket, _)) = &delayed
                    && !closed
                {
                    match socket.peek(&mut [0]) {
                        Ok(0) => {
                            closed = true;
                            closed_tx.send(()).unwrap();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                            closed = true;
                            closed_tx.send(()).unwrap();
                        }
                        other => panic!("unexpected data on held request: {other:?}"),
                    }
                }
                if release_rx.try_recv().is_ok() {
                    let (mut socket, body): (TcpStream, Vec<u8>) = delayed
                        .take()
                        .expect("release requires an observed A request");
                    socket.set_nonblocking(false).unwrap();
                    reply(&mut socket, &body);
                    replied_tx.send(()).unwrap();
                }
                let (mut socket, _) = match accept_http(&listener) {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                socket.set_write_timeout(Some(WAIT)).unwrap();
                let deadline = Instant::now() + WAIT;
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 16_384);
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(!remaining.is_zero(), "fixture header deadline exceeded");
                    socket.set_read_timeout(Some(remaining)).unwrap();
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                let request = header.lines().next().unwrap();
                captured.lock().unwrap().push(request.to_owned());
                let target = request.split_whitespace().nth(1).unwrap();
                let url = reqwest::Url::parse(&base).unwrap().join(target).unwrap();
                assert_eq!(url.host_str(), Some("127.0.0.1"));
                assert_eq!(url.port(), reqwest::Url::parse(&base).unwrap().port());
                let params: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
                assert_eq!(
                    params.get("apikey").map(|s| s.as_ref()),
                    Some(PRIVATE_MARKER)
                );
                let mut hold = false;
                let body = match url.path() {
                    "/download" => match params.get("id").map(|s| s.as_ref()) {
                        Some("a") => a.clone(),
                        Some("b") => b.clone(),
                        _ => panic!("unknown fixture download"),
                    },
                    "/api" if params.get("t").map(|s| s.as_ref()) == Some("caps") => {
                        b"<caps><searching><movie-search available='yes' supportedParams='q'/></searching></caps>".to_vec()
                    }
                    "/api" => {
                        let (id, title, metadata) = match params.get("q").map(|s| s.as_ref()) {
                            Some("Fixture 2020") => {
                                hold = stall.load(Ordering::SeqCst);
                                ("a", "Fixture", &a)
                            }
                            Some("Other 2020") => ("b", "Other", &b),
                            _ => panic!("unexpected fixture query"),
                        };
                        let hash = PreparedTorrent::from_bytes(metadata.clone().into())
                            .unwrap()
                            .info_hash;
                        format!(
                            "<rss version='2.0' xmlns:t='http://torznab.com/schemas/2015/feed'><channel><title>Owned</title><link>{base}/</link><description>Owned fixture</description><item><title>{title} 2020 1080p</title><guid>{id}</guid><t:attr name='category' value='2000'/><t:attr name='infohash' value='{hash}'/><enclosure type='application/x-bittorrent' url='{base}/download?id={id}&amp;apikey={PRIVATE_MARKER}'/></item></channel></rss>"
                        ).into_bytes()
                    }
                    _ => panic!("unexpected request path: {}", url.path()),
                };
                if hold {
                    socket.set_nonblocking(true).unwrap();
                    assert!(delayed.replace((socket, body)).is_none());
                    held_tx.send(()).unwrap();
                } else {
                    reply(&mut socket, &body);
                }
            }
        });
        Self {
            endpoint,
            requests,
            stall_a,
            held,
            client_closed,
            release,
            replied,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn preferences(&self) -> SourcePreferences {
        SourcePreferences {
            rutracker_enabled: false,
            torznab: vec![TorznabConfig {
                name: "Owned boundary fixture".into(),
                base_url: self.endpoint.clone(),
                api_key_env: Some("KADR_TORZNAB_TEST_KEY".into()),
                enabled: true,
            }],
            ..Default::default()
        }
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            assert!(
                joined.is_ok() || thread::panicking(),
                "HTTP fixture panicked"
            );
        }
    }
}

fn isolated(test: &str) -> bool {
    // Never change the parent multithreaded test process's environment.
    if std::env::var("KADR_BOUNDARY_CHILD").ok().as_deref() == Some(test) {
        return false;
    }
    let mut child = ProcessCommand::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--include-ignored", "--nocapture"])
        .env("KADR_BOUNDARY_CHILD", test)
        .env("KADR_TORZNAB_TEST_KEY", PRIVATE_MARKER)
        .env("NO_PROXY", "*")
        .env("no_proxy", "*")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .spawn()
        .unwrap();
    // Hard outer bound also covers cleanup if a worker regresses and stops
    // responding; an elapsed-time assertion after join alone cannot do that.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "isolated boundary test failed");
            return true;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("isolated boundary test exceeded 45 seconds");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn second_card(db: &mut Storage) -> PlaybackTarget {
    let id = MediaId::new("trakt:movie:53").unwrap();
    db.upsert_media(&kadr_storage::MediaRecord {
        id: id.clone(),
        title: "Other".into(),
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

fn save_settings(app: &mut App, settings: &Settings) {
    app.command(Command::UpdateSettings(settings.clone()));
    app.wait(
        "exact settings acknowledged",
        |event| matches!(event, Event::PreferencesSaved(saved) if saved == settings),
    );
}

fn configure(app: &mut App, origin: &Origin) -> Settings {
    let mut settings = app.ready();
    settings.catalog_enabled = true;
    settings.network.torrents_enabled = true;
    // Any accidental catalog request also stays on this owned origin.
    settings.catalog_proxy = Some(origin.endpoint.trim_end_matches("/api").into());
    save_settings(app, &settings);
    let preferences = origin.preferences();
    app.command(Command::SetSourcePreferences(preferences.clone()));
    app.wait(
        "source preferences acknowledged",
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &preferences),
    );
    settings
}

fn open_card(app: &mut App, target: &PlaybackTarget) {
    app.command(Command::OpenLibrary(library_id(target)));
    app.wait("exact cached Trakt card opened", |event| {
        matches!(event, Event::MediaSelected { media: Some(media), episode: None, .. }
            if media.id == target.media_id.as_str())
    });
}

fn discovery(app: &mut App, target: &PlaybackTarget) -> (DiscoveryStamp, SourceCandidate) {
    let event = app.wait("successful discovery for exact target", |event| {
        matches!(event, Event::SourceDiscovery { media_id, pending: 0, .. }
            if media_id == target.media_id.as_str())
    });
    let Event::SourceDiscovery {
        stamp,
        episode,
        rows,
        succeeded,
        failures,
        ..
    } = event
    else {
        unreachable!()
    };
    assert!(stamp.is_current());
    assert_eq!(episode, None);
    assert_eq!(succeeded, 1);
    assert!(failures.is_empty());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, SourceKind::TorrentFile);
    assert_eq!(rows[0].origin, "Owned boundary fixture");
    (stamp, rows.into_iter().next().unwrap())
}

#[test]
fn e2e_04_restart_reads_sqlite_cache_and_respects_privacy_gates() {
    if isolated(
        "tests::torznab::command_boundary::e2e_04_restart_reads_sqlite_cache_and_respects_privacy_gates",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let origin = Origin::new();
    let mut db = Storage::open_default(&paths).unwrap();
    let target = catalog_fixture(&mut db);
    let mut first = App::start(&paths);
    let settings = configure(&mut first, &origin);
    open_card(&mut first, &target);
    first.command(Command::FindSources);
    let (stamp, row) = discovery(&mut first, &target);
    assert_eq!(row.title, "Fixture 2020 1080p");
    let requests = origin.requests();
    assert_eq!(requests.len(), 2, "one caps and one search, no metainfo");
    assert!(requests.iter().any(|r| r.contains("t=caps")));
    assert!(requests.iter().any(|r| r.contains("q=Fixture")));
    first.finish();
    assert!(!stamp.is_current());
    drop(first);
    drop(db);

    // New worker and SQLite connection: no SearchState or provider client survives.
    let mut second = App::start(&paths);
    assert_eq!(second.ready(), settings);
    second.wait(
        "persisted provider preferences loaded",
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &origin.preferences()),
    );
    open_card(&mut second, &target);
    second.command(Command::FindSources);
    let (mut cached_stamp, mut cached) = discovery(&mut second, &target);
    // Handles are engine-local, not persistent identities.
    let mut expected = row.clone();
    expected.index = cached.index;
    assert_eq!(cached, expected);
    assert_eq!(
        origin.requests(),
        requests,
        "restart must not even fetch caps"
    );

    for (catalog_enabled, torrents_enabled) in [(true, false), (false, true)] {
        let mut disabled = settings.clone();
        disabled.catalog_enabled = catalog_enabled;
        disabled.network.torrents_enabled = torrents_enabled;
        save_settings(&mut second, &disabled);
        assert!(!cached_stamp.is_current());
        // Neither a retained UI handle nor force-retry may bypass consent.
        second.command(Command::ChooseSource(cached.index));
        second.wait("retained cached handle rejected", |event| {
            assert!(
                !matches!(event, Event::SourceDiscovery { .. }),
                "disabled provider published a cached discovery"
            );
            matches!(event, Event::Status(Level::Error, message) if message.contains("больше недоступен"))
        });
        for command in [Command::FindSources, Command::RetrySources] {
            second.command(command);
            second.wait("discovery blocked by settings", |event| {
                if let Event::SourceDiscovery { .. } = event {
                    panic!("disabled provider must not publish cached results");
                }
                matches!(event, Event::Status(Level::Warning, message) if message.contains("Нет включённых"))
            });
        }
        assert_eq!(
            origin.requests(),
            requests,
            "privacy gate emitted HTTP traffic"
        );
        save_settings(&mut second, &settings);
        second.command(Command::FindSources);
        (cached_stamp, cached) = discovery(&mut second, &target);
        assert_eq!(
            origin.requests(),
            requests,
            "consent restores SQLite cache access"
        );
    }
    second.finish();
    assert_eq!(origin.requests(), requests);
}

/// Audit every event after B is selected, including events that a predicate-only
/// wait could silently discard and events queued immediately before shutdown.
fn assert_still_b(event: &Event, b: &PlaybackTarget, row: &SourceCandidate) {
    assert_public_feedback_is_private(event);
    match event {
        Event::SourceDiscovery { media_id, rows, .. } => {
            assert_eq!(media_id, b.media_id.as_str());
            assert_eq!(rows, std::slice::from_ref(row));
        }
        Event::SourcesState(AsyncState::Ready(rows)) => {
            assert_eq!(rows, std::slice::from_ref(row));
        }
        Event::SourcesState(AsyncState::Loading) => panic!("late A restarted source loading"),
        Event::MediaSelected { media, .. } => {
            assert_eq!(
                media.as_ref().map(|m| m.id.as_str()),
                Some(b.media_id.as_str())
            );
        }
        Event::SourceSelected {
            kind,
            title,
            playable,
            suggested_filename,
        } => {
            assert_eq!(*kind, Some(SourceKind::TorrentFile));
            assert_eq!(title, "Other.mp4");
            assert!(*playable, "late A cleared B's playable selection");
            assert_eq!(suggested_filename, "Other.mp4");
        }
        Event::TorrentFiles(files) => {
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].name, "Other.mp4");
        }
        Event::SourcesState(AsyncState::Error(message)) => {
            assert!(
                message.contains("больше недоступен"),
                "unexpected source error"
            );
        }
        Event::Status(_, message) => {
            assert!(
                !message.contains("Fixture"),
                "late A feedback leaked into B"
            );
        }
        _ => {}
    }
}

#[test]
fn e2e_05_late_a_cannot_replace_b_or_reuse_its_handles() {
    if isolated(
        "tests::torznab::command_boundary::e2e_05_late_a_cannot_replace_b_or_reuse_its_handles",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let origin = Origin::new();
    let mut db = Storage::open_default(&paths).unwrap();
    let a = catalog_fixture(&mut db);
    let b = second_card(&mut db);
    let mut app = App::start(&paths);
    configure(&mut app, &origin);
    open_card(&mut app, &a);
    app.command(Command::FindSources);
    let (old_stamp, old_row) = discovery(&mut app, &a);

    // Warm A only to obtain a real old UI handle. Force a fresh, held HTTP reply.
    origin.stall_a.store(true, Ordering::SeqCst);
    let a_started = Instant::now();
    app.command(Command::RetrySources);
    origin
        .held
        .recv_timeout(WAIT)
        .expect("A request reached owned server");
    let Event::SourceDiscovery {
        stamp: pending_a,
        rows,
        ..
    } = app.wait("A pending snapshot", |event| {
        matches!(event, Event::SourceDiscovery { media_id, pending: 1, .. }
            if media_id == a.media_id.as_str())
    })
    else {
        unreachable!()
    };
    assert!(pending_a.is_current());
    assert!(rows.is_empty());
    assert!(!old_stamp.is_current());

    // Leave A's reply held throughout B discovery AND metainfo selection.
    open_card(&mut app, &b);
    assert!(!pending_a.is_current());
    app.command(Command::FindSources);
    let (b_stamp, b_row) = discovery(&mut app, &b);
    assert_ne!(old_row.index, b_row.index);
    assert_eq!(b_row.title, "Other 2020 1080p");
    app.command(Command::ChooseSource(b_row.index));
    app.wait("B metainfo exposes only B file", |event| {
        matches!(event, Event::TorrentFiles(files) if files.len() == 1 && files[0].name == "Other.mp4")
    });
    app.wait("B playable selection", |event| {
        matches!(event, Event::SourceSelected { playable: true, suggested_filename, .. }
            if suggested_filename == "Other.mp4")
    });
    // Choosing a row deliberately invalidates even B's discovery stamp.
    assert!(!b_stamp.is_current());
    assert_eq!(
        origin
            .requests()
            .iter()
            .filter(|r| r.contains("/download?"))
            .count(),
        1
    );
    assert!(origin.requests().iter().any(|r| r.contains("id=b")));

    // Observe A's client closing before shutdown, within an overall budget below
    // the adapter's 20s request timeout. This tests real navigation cancellation,
    // not an injected, already-queued worker callback. A reply-write acknowledgement
    // alone would not exclude shutdown masking a still-live HTTP request.
    origin
        .client_closed
        .recv_timeout(WAIT.saturating_sub(a_started.elapsed()))
        .expect("navigation closed held A request before engine shutdown");
    assert!(
        a_started.elapsed() < WAIT,
        "A must close before HTTP timeout"
    );
    origin.release.send(()).unwrap();
    origin
        .replied
        .recv_timeout(WAIT)
        .expect("late A reply attempted");
    let requests = origin.requests();
    app.command(Command::ChooseSource(old_row.index));
    app.wait("A handle rejected without disturbing B", |event| {
        assert_still_b(event, &b, &b_row);
        matches!(event, Event::Status(Level::Error, message) if message.contains("больше недоступен"))
    });
    // Read back the actual current prepared selection, not just a retained event.
    app.command(Command::ChooseTorrentFile(0));
    app.wait(
        "B remains selected after late A and stale handle",
        |event| {
            assert_still_b(event, &b, &b_row);
            matches!(event, Event::SourceSelected { playable: true, suggested_filename, .. }
            if suggested_filename == "Other.mp4")
        },
    );
    let before = Instant::now();
    assert!(app.backend.join());
    assert!(before.elapsed() < WAIT);
    while let Ok(event) = app.events.try_recv() {
        assert_still_b(&event, &b, &b_row);
    }
    assert!(!pending_a.is_current());
    assert_eq!(
        origin.requests(),
        requests,
        "late/stale A must never download metainfo"
    );
    assert!(db.source_for_target(&a).unwrap().is_none());
    assert!(db.source_for_target(&b).unwrap().is_none());
}
