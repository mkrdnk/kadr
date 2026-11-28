//! The live test is explicit opt-in, with one API lookup and one selected magnet.
//! Run only this probe, serially:
//! `cargo test -p kadr-engine tests::popcorn::live_engine_bbb_metadata_only -- --ignored --exact --nocapture --test-threads=1`
//!
//! The card is a test fixture with the real film's IMDb ID, not a live Trakt
//! response. SQLite snapshots omit external IDs, so OpenLibrary cannot supply it.
//! No Trakt credentials, player, payload download, or production URL override.
use super::*;
use kadr_core::catalog::{ExternalMetadata, MediaKind};

const TITLE: &str = "Big Buck Bunny";
const IMDB: &str = "tt1254207";
const FIXTURE_ID: &str = "trakt:movie:1";
const API_WAIT: Duration = Duration::from_secs(45);
const METADATA_WAIT: Duration = Duration::from_secs(60);

fn movie() -> Media {
    Media {
        // Intentionally a local fixture ID, not the film's claimed Trakt ID.
        id: FIXTURE_ID.into(),
        title: TITLE.into(),
        original_title: None,
        year: Some(2008),
        external: ExternalMetadata {
            imdb_id: Some(IMDB.into()),
            ..Default::default()
        },
        rating_tenths: None,
        runtime_minutes: None,
        genres: Vec::new(),
        certification: None,
        external_ratings: Vec::new(),
        description: String::new(),
        poster_url: None,
        backdrop_url: None,
        kind: MediaKind::Movie,
        episodes: Vec::new(),
        attribution: "Local acceptance fixture; IMDb tt1254207".into(),
        license: None,
        source_page: String::new(),
    }
}

fn preferences(enabled: bool) -> SourcePreferences {
    SourcePreferences {
        popcorn_enabled: enabled,
        rutracker_enabled: false,
        commons_enabled: false,
        torznab: Vec::new(),
        ..Default::default()
    }
}

fn settings(catalog: bool, torrents: bool) -> kadr_storage::Settings {
    let mut settings = kadr_storage::Settings {
        catalog_enabled: catalog,
        ..Default::default()
    };
    settings.network.torrents_enabled = torrents;
    settings.network.download = kadr_storage::RateLimit::BytesPerSecond(64 * 1024);
    settings.network.upload = kadr_storage::RateLimit::Paused;
    settings.network.max_connections = 8;
    settings
}

fn start(
    paths: &AppPaths,
    settings: &kadr_storage::Settings,
    preferences: &SourcePreferences,
    media: Media,
) -> App {
    let mut db = Storage::open_default(paths).unwrap();
    db.save_settings(settings).unwrap();
    db.save_source_preferences(preferences).unwrap();
    drop(db);
    let mut app = App::start(paths);
    assert_eq!(app.ready(), Settings::from_storage(settings.clone()));
    // Uses the ordinary details-result handler, preserving the fixture's IMDb
    // ID in memory without pretending it survives the storage snapshot.
    app.command(Command::OpenFixtureMovie(Box::new(media.clone())));
    app.wait("fixture movie with external metadata", |event| {
        metadata_only(event);
        matches!(event, Event::MediaSelected { media: Some(opened), .. } if opened.as_ref() == &media)
    });
    app
}

fn metadata_only(event: &Event) {
    assert!(
        !matches!(event, Event::PlaybackActive(true)),
        "metadata-only acceptance must never launch a player"
    );
    assert!(
        !matches!(event, Event::Transfer(Some(_))),
        "metadata-only acceptance must never start a payload transfer"
    );
    if let Event::CacheUsage(usage) = event {
        assert_eq!(usage.reserved_bytes, 0, "no payload reservation");
        assert_eq!(usage.actual_bytes, 0, "no payload bytes");
        assert_eq!(usage.active_leases, 0, "no payload session");
    }
}

fn no_payload(paths: &AppPaths) {
    let payload = paths.cache_dir().join("torrents-v1/data");
    assert!(
        !payload.exists() || fs::read_dir(payload).unwrap().next().is_none(),
        "metadata preparation must not allocate even an empty payload directory"
    );
}

#[test]
fn popcorn_requires_catalog_torrents_opt_in_and_imdb() {
    for (catalog, torrents, enabled, imdb) in [
        (false, true, true, true),
        (true, false, true, true),
        (true, true, false, true),
        (true, true, true, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut settings = settings(catalog, torrents);
        settings.catalog_proxy = Some(format!("http://{}", listener.local_addr().unwrap()));
        let mut media = movie();
        if !imdb {
            media.external.imdb_id = None;
        }
        let mut app = start(&paths, &settings, &preferences(enabled), media);
        app.command(Command::FindSources);
        app.wait("disabled or unsupported Popcorn provider", |event| {
            metadata_only(event);
            assert!(
                !matches!(event, Event::SourceDiscovery { .. }),
                "a disabled/unsupported provider must not enter discovery"
            );
            matches!(event, Event::Status(Level::Warning, message)
                if if imdb { message.starts_with("Нет включённых поставщиков") }
                   else { message.contains("нет IMDb ID") })
        });
        app.finish();
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "disabled/unsupported provider contacted the proxy: catalog={catalog}, torrents={torrents}, enabled={enabled}, imdb={imdb}"
        );
        no_payload(&paths);
    }
}

#[test]
fn enabled_popcorn_reaches_only_the_configured_proxy_and_reports_failure() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut settings = settings(true, true);
    settings.catalog_proxy = Some(format!("http://{}", listener.local_addr().unwrap()));
    let mut app = start(&paths, &settings, &preferences(true), movie());
    app.command(Command::FindSources);
    let deadline = Instant::now() + WAIT;
    let mut hosts = Vec::new();
    for _ in 0..6 {
        let mut socket = loop {
            match accept_http(&listener) {
                Ok((socket, _)) => break socket,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("enabled Popcorn did not contact the owned proxy: {error}"),
            }
        };
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 4096);
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let line = std::str::from_utf8(&headers)
            .unwrap()
            .lines()
            .next()
            .unwrap();
        hosts.push(
            line.strip_prefix("CONNECT ")
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_owned(),
        );
        // Refuse the tunnel before TLS. No public API, tracker, DHT, or peer is
        // contacted, and no fixture URL is installed in the production adapter.
        socket
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        drop(socket);
    }
    hosts.sort();
    assert_eq!(
        hosts,
        [
            "fusme.link:443",
            "jfper.link:443",
            "uxert.link:443",
            "yrkde.link:443",
            "yts.gg:443",
            "yts.lt:443"
        ]
    );
    let Event::SourceDiscovery {
        rows,
        succeeded,
        failures,
        ..
    } = app.wait("controlled Popcorn transport failure", |event| {
        metadata_only(event);
        matches!(event, Event::SourceDiscovery { pending: 0, .. })
    })
    else {
        unreachable!()
    };
    assert!(rows.is_empty());
    assert_eq!(succeeded, 0);
    assert_eq!(
        failures.len(),
        2,
        "both independent index failures must reach the client"
    );
    app.finish();
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "one failed lookup must not cause an unbounded retry or fallback"
    );
    no_payload(&paths);
}

#[test]
#[ignore = "opt-in real Popcorn API + public BitTorrent metadata only; no Trakt API, playback, or payload"]
fn live_engine_bbb_metadata_only() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut app = start(&paths, &settings(true, true), &preferences(true), movie());
    eprintln!("Popcorn live acceptance: fixture {TITLE} (2008), IMDb {IMDB}; API discovery");
    app.command(Command::FindSources);
    let Event::SourceDiscovery {
        rows,
        succeeded,
        failures,
        notices,
        ..
    } = app.wait_for(API_WAIT, "live Popcorn API discovery", |event| {
        metadata_only(event);
        matches!(event, Event::SourceDiscovery { media_id, pending: 0, .. } if media_id == FIXTURE_ID)
    })
    else {
        unreachable!()
    };
    assert!(
        succeeded > 0,
        "stage=API discovery: {failures:?}; notices={notices:?}"
    );
    assert_eq!(
        succeeded + failures.len(),
        2,
        "Popcorn and YTS must both finish"
    );
    let row = rows
        .iter()
        .filter(|row| {
            let title: String = row
                .title
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect();
            row.kind == SourceKind::Magnet && row.origin.contains("Popcorn API") && title
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains("big buck bunny")
        })
        .max_by_key(|row| {
            (
                !row.possible_match,
                row.seeds.unwrap_or(0),
                std::cmp::Reverse(row.size.unwrap_or(u64::MAX)),
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "stage=API discovery: {count} rows but no manually selectable title-matching BBB magnet; notices={notices:?}",
                count = rows.len()
            )
        });
    eprintln!(
        "Popcorn live acceptance: API returned {} rows; selecting {:?}, seeds={:?}, bytes={:?}; public metadata deadline={}s",
        rows.len(),
        row.title,
        row.seeds,
        row.size,
        METADATA_WAIT.as_secs(),
    );
    // This is the only command that permits public BitTorrent metadata traffic.
    // Never dispatch Play, WatchRelease, OpenIn, or SaveTo in this acceptance.
    app.command(Command::ChooseSource(row.index));
    let Event::TorrentFiles(files) = app.wait_for(
        METADATA_WAIT,
        "selected BBB magnet metadata (public DHT/trackers/peers; no payload)",
        |event| {
            metadata_only(event);
            if let Event::SourcesState(AsyncState::Error(message)) = event {
                panic!("stage=BitTorrent metadata: {message}");
            }
            matches!(event, Event::TorrentFiles(files) if !files.is_empty())
        },
    ) else {
        unreachable!()
    };
    let file = files
        .iter()
        .filter(|file| file.len > 0 && file.episode.is_none() && !file.ambiguous)
        .max_by_key(|file| file.len)
        .expect("stage=file selection: metadata must offer a nonempty movie video");
    // TorrentFiles already applies the production video-extension allowlist.
    app.command(Command::ChooseTorrentFile(file.index));
    app.wait("video file is selectable without playback", |event| {
        metadata_only(event);
        matches!(event, Event::SourceSelected { playable: true, title, .. }
            if title == &file.name)
    });
    app.finish();
    no_payload(&paths);
    eprintln!(
        "Popcorn live acceptance: PASS; {} video choices; selected {:?} ({} bytes); no playback or payload",
        files.len(),
        file.name,
        file.len,
    );
}
