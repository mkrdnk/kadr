use super::*;
use kadr_core::{
    catalog::MediaKind,
    sources::{Locator, MediaSource},
};

/// Real desktop worker, SQLite, resolver and HTTP transport; the explicit
/// loopback proxy rejects CONNECT before TLS, so no origin or peer is reached.
#[test]
fn opening_a_movie_checks_a_cached_topic_before_find_and_reuses_the_lookup() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut db = Storage::open_default(&paths).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut settings = db.load_settings().unwrap();
    settings.catalog_enabled = true;
    settings.network.torrents_enabled = true;
    settings.catalog_proxy = Some(format!("http://{}", listener.local_addr().unwrap()));
    db.save_settings(&settings).unwrap();
    let id = MediaId::new("trakt:movie:99").unwrap();
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
    let row = MediaSource::new(
        "rutracker",
        Locator::RuTracker(42),
        "Fixture [2020]",
        MediaKind::Movie,
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    db.source_cache_put(
        "resolve-v2:trakt:movie:99:rt2",
        "rutracker",
        now,
        false,
        &serde_json::to_vec(&[row]).unwrap(),
    )
    .unwrap();
    let (backend, events) = Engine::start_with(Some(paths), false).unwrap();
    let mut app = App { backend, events };
    app.ready();
    app.command(Command::OpenLibrary(library_id(&PlaybackTarget {
        media_id: id.clone(),
        episode: None,
    })));
    app.wait("cached movie opened", |e| {
        matches!(
            e,
            Event::LibraryOpened {
                source_available: false
            }
        )
    });
    app.command(Command::PrefetchMovieSources(id.as_str().into()));
    let deadline = Instant::now() + WAIT;
    let mut socket = loop {
        match listener.accept() {
            Ok((socket, _)) => break socket,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("expected HTTP topic prefetch: {e}"),
        }
    };
    socket.set_read_timeout(Some(WAIT)).unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 4096);
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    assert!(headers.starts_with(b"CONNECT rutracker.org:443 "));
    app.command(Command::FindSources);
    socket
        .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    drop(socket);
    let Event::SourceDiscovery {
        rows,
        notices,
        failures,
        ..
    } = app.wait("foreground joined topic prefetch", |e| {
        assert!(!matches!(
            e,
            Event::PlaybackActive(true)
                | Event::RuTrackerLogin {
                    message: Some(_),
                    ..
                }
        ));
        matches!(e, Event::SourceDiscovery { pending: 0, .. })
    })
    else {
        unreachable!()
    };
    assert_eq!(rows.len(), 1);
    assert!(rows[0].rutracker);
    assert_eq!(
        notices.len(),
        1,
        "the joined topic error must reach Sources"
    );
    assert!(failures.is_empty());
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
