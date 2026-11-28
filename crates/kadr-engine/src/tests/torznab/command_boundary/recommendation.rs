//! Recommendation and explicit watch at the engine boundary. Each provider owns
//! its loopback indexer and metainfo; a channel, not a sleep, releases the loser
//! or winner that finishes last. Ordinary tests stop at the existing cache quota.
//! Run with `cargo test -p kadr-engine command_boundary::recommendation`.
//! Add `-- --ignored --test-threads=1` for the opt-in ffmpeg/mpv case.
use super::*;
use kadr_torrent::engine::EngineOptions;

fn isolated_case(name: &str) -> bool {
    // libtest paths omit the crate-name prefix included by module_path!.
    let module = module_path!().split_once("::").unwrap().1;
    isolated(&format!("{module}::{name}"))
}

struct OfferOrigin {
    base: String,
    downloads: Arc<AtomicUsize>,
    held: mpsc::Receiver<()>,
    closed: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    replied: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl OfferOrigin {
    fn new(year: u16, seeds: Option<u32>, metadata: Vec<u8>, delayed: bool) -> Self {
        Self::release_offer(
            &format!("Fixture {year} 1080p"),
            "2000",
            seeds,
            metadata,
            delayed,
        )
    }

    fn release_offer(
        title: &str,
        category: &str,
        seeds: Option<u32>,
        metadata: Vec<u8>,
        delayed: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hash = PreparedTorrent::from_bytes(metadata.clone().into())
            .unwrap()
            .info_hash;
        let seeds = seeds.map_or_else(String::new, |n| {
            format!("<t:attr name='seeders' value='{n}'/>")
        });
        let feed = format!(
            "<rss version='2.0' xmlns:t='http://torznab.com/schemas/2015/feed'><channel><title>Owned</title><link>{base}/</link><description>Owned fixture</description><item><title>{title}</title><guid>owned</guid><t:attr name='category' value='{category}'/><t:attr name='infohash' value='{hash}'/>{seeds}<enclosure type='application/x-bittorrent' url='{base}/download'/></item></channel></rss>"
        ).into_bytes();
        let stop = Arc::new(AtomicBool::new(false));
        let downloads = Arc::new(AtomicUsize::new(0));
        let (held_tx, held) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (replied_tx, replied) = mpsc::channel();
        let (done, count, origin) = (stop.clone(), downloads.clone(), base.clone());
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let (mut socket, _) = match accept_http(&listener) {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("owned indexer accept: {error}"),
                };
                socket.set_read_timeout(Some(WAIT)).unwrap();
                socket.set_write_timeout(Some(WAIT)).unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 16_384);
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                let header = String::from_utf8(header).unwrap();
                let target = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let url = reqwest::Url::parse(&origin).unwrap().join(target).unwrap();
                assert_eq!(url.origin(), reqwest::Url::parse(&origin).unwrap().origin());
                let caps = url.query_pairs().any(|(k, v)| k == "t" && v == "caps");
                let body = match url.path() {
                    "/download" => {
                        count.fetch_add(1, Ordering::SeqCst);
                        &metadata
                    }
                    "/api" if caps => {
                        reply(&mut socket, b"<caps><searching><movie-search available='yes' supportedParams='q'/><tv-search available='yes' supportedParams='q,season,ep'/></searching></caps>");
                        continue;
                    }
                    "/api" => &feed,
                    _ => panic!("unexpected owned indexer path: {}", url.path()),
                };
                if delayed && url.path() == "/api" {
                    socket.set_nonblocking(true).unwrap();
                    held_tx.send(()).unwrap();
                    let mut observed_close = false;
                    while release_rx.try_recv().is_err() {
                        if done.load(Ordering::SeqCst) {
                            return;
                        }
                        if !observed_close {
                            match socket.peek(&mut [0]) {
                                Ok(0) => observed_close = true,
                                Err(error)
                                    if error.kind() == std::io::ErrorKind::ConnectionReset =>
                                {
                                    observed_close = true;
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                                other => panic!("unexpected held request data: {other:?}"),
                            }
                            if observed_close {
                                closed_tx.send(()).unwrap();
                            }
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    socket.set_nonblocking(false).unwrap();
                    reply(&mut socket, body);
                    replied_tx.send(()).unwrap();
                } else {
                    reply(&mut socket, body);
                }
            }
        });
        Self {
            base,
            downloads,
            held,
            closed,
            release,
            replied,
            stop,
            worker: Some(worker),
        }
    }

    fn config(&self, name: &str) -> TorznabConfig {
        TorznabConfig {
            name: name.into(),
            base_url: format!("{}/api", self.base),
            api_key_env: None,
            enabled: true,
        }
    }

    fn release(&self) {
        self.release.send(()).unwrap();
        self.replied
            .recv_timeout(WAIT)
            .expect("held reply attempted");
    }
}

impl Drop for OfferOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            assert!(
                worker.join().is_ok() || thread::panicking(),
                "owned indexer panicked"
            );
        }
    }
}

fn options(peers: Vec<SocketAddr>) -> EngineOptions {
    EngineOptions {
        network_enabled: true,
        discovery_enabled: false,
        initial_peers: peers,
        ..Default::default()
    }
}

fn start(
    paths: &AppPaths,
    fast: &OfferOrigin,
    slow: &OfferOrigin,
    options: EngineOptions,
) -> (App, Storage, PlaybackTarget) {
    let mut db = Storage::open_default(paths).unwrap();
    let target = catalog_fixture(&mut db);
    let (backend, events) =
        Engine::start_with_network(Some(paths.clone()), true, Some(options)).unwrap();
    let mut app = App { backend, events };
    let mut settings = app.ready();
    settings.default_player = PlayerPreference::Mpv;
    settings.catalog_enabled = true;
    settings.network.torrents_enabled = true;
    // `isolated` removes ambient proxies. The only enabled providers below are
    // these two loopback servers; OpenLibrary reads the cached card from SQLite.
    settings.catalog_proxy = None;
    settings.cache_limit_bytes = 1024 * 1024;
    save_settings(&mut app, &settings);
    let preferences = SourcePreferences {
        rutracker_enabled: false,
        commons_enabled: false,
        torznab: vec![fast.config("Fast fixture"), slow.config("Slow fixture")],
        ..Default::default()
    };
    app.command(Command::SetSourcePreferences(preferences.clone()));
    app.wait(
        "only owned providers configured",
        |event| matches!(event, Event::SourcePreferences(saved) if saved == &preferences),
    );
    open_card(&mut app, &target);
    (app, db, target)
}

fn quiet(event: &Event) {
    assert_public_feedback_is_private(event);
    assert!(
        !matches!(event, Event::PlaybackActive(true)),
        "unexpected player launch"
    );
    assert!(
        !matches!(event, Event::Transfer(Some(_))),
        "unexpected torrent session"
    );
}

fn snapshot(
    app: &mut App,
    target: &PlaybackTarget,
    pending: usize,
) -> (DiscoveryStamp, Vec<SourceCandidate>) {
    let event = app.wait("cumulative target discovery", |event| {
        unprepared(event);
        if let Event::SourceDiscovery {
            stamp,
            media_id,
            episode,
            rows,
            pending: left,
            succeeded,
            failures,
            notices,
        } = event
        {
            assert!(stamp.is_current());
            assert_eq!(media_id, target.media_id.as_str());
            assert_eq!(
                *episode,
                target.episode.map(|key| (key.season, key.episode))
            );
            assert!(failures.is_empty(), "{failures:?}");
            assert!(
                notices.is_empty(),
                "unexpected notice for the Torznab fixtures"
            );
            assert_eq!(left + succeeded, 2);
            if *left > 0 {
                assert!(
                    rows.iter().all(|row| !row.recommended),
                    "provisional recommendation"
                );
            }
            *left == pending
        } else {
            false
        }
    });
    let Event::SourceDiscovery { stamp, rows, .. } = event else {
        unreachable!()
    };
    (stamp, rows)
}

fn begin(
    app: &mut App,
    slow: &OfferOrigin,
    target: &PlaybackTarget,
) -> (DiscoveryStamp, SourceCandidate) {
    app.command(Command::FindSources);
    slow.held
        .recv_timeout(WAIT)
        .expect("slow provider search held");
    let (stamp, rows) = snapshot(app, target, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin, "Fast fixture");
    assert_eq!(rows[0].kind, SourceKind::TorrentFile);
    (stamp, rows.into_iter().next().unwrap())
}

fn finish_quiet(app: &mut App) {
    assert!(app.backend.join());
    while let Ok(event) = app.events.try_recv() {
        quiet(&event);
    }
}

fn quota(app: &mut App) {
    app.wait(
        "existing whole-torrent quota rejects playback before any starts",
        |event| {
            quiet(event);
            matches!(event, Event::Status(Level::Error, message) if message.contains("quota"))
        },
    );
}

fn metadata(name: &str, byte: u8) -> Vec<u8> {
    metainfo(&[(name, vec![byte; 2 * 1024 * 1024])], true)
}

fn unprepared(event: &Event) {
    quiet(event);
    assert!(
        !matches!(event, Event::TorrentFiles(files) if !files.is_empty()),
        "discovery/recommendation/rejected watch prepared metainfo"
    );
    assert!(
        !matches!(event, Event::SourceSelected { playable: true, .. }),
        "discovery/recommendation/rejected watch selected a playable file"
    );
}

fn recommend(app: &mut App, preferences: &PlaybackPreferences) -> (DiscoveryStamp, Option<usize>) {
    app.command(Command::RecommendPlayback(preferences.clone()));
    let Event::PlaybackRecommendation { stamp, index } =
        app.wait("IO-free playback recommendation", |event| {
            unprepared(event);
            matches!(event, Event::PlaybackRecommendation { .. })
        })
    else {
        unreachable!()
    };
    assert!(stamp.is_current());
    (stamp, index)
}

fn rejected_watch(app: &mut App, index: usize, preferences: &PlaybackPreferences) {
    app.command(Command::WatchRecommended {
        index,
        preferences: preferences.clone(),
        resume: false,
    });
    app.wait("watch authority rejected before metainfo", |event| {
        unprepared(event);
        matches!(event, Event::Status(Level::Warning, message)
            if message.contains("Не удалось подтвердить источник"))
    });
    app.wait("rejected watch requests explicit input", |event| {
        unprepared(event);
        matches!(event, Event::MovieWatchNeedsInput(stamp) if stamp.is_current())
    });
}

fn no_downloads(fast: &OfferOrigin, slow: &OfferOrigin) {
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
}

#[test]
fn recommend_playback_is_io_free_and_watch_requires_the_same_preferences() {
    if isolated_case("recommend_playback_is_io_free_and_watch_requires_the_same_preferences") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(3), metadata("Alternate.mp4", 3), false);
    let slow = OfferOrigin::new(2020, Some(27), metadata("Winner.mp4", 7), true);
    let (mut app, db, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let preferences = PlaybackPreferences::default();
    let (partial_stamp, partial) = begin(&mut app, &slow, &target);
    assert_eq!(recommend(&mut app, &preferences).1, None);
    rejected_watch(&mut app, partial.index, &preferences);
    no_downloads(&fast, &slow);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    let winner = rows.iter().find(|row| row.recommended).unwrap();
    assert_eq!(winner.origin, "Slow fixture");
    let (stamp, index) = recommend(&mut app, &preferences);
    assert_eq!(index, Some(winner.index));
    assert!(partial_stamp.is_current());
    no_downloads(&fast, &slow);

    // Each field belongs to the request, even when it cannot change the winner.
    for changed in [
        PlaybackPreferences {
            preferred_resolution: Some("720p".into()),
            ..preferences.clone()
        },
        PlaybackPreferences {
            preferred_language: Some("en".into()),
            ..preferences.clone()
        },
        PlaybackPreferences {
            subtitles_off: true,
            ..preferences.clone()
        },
    ] {
        rejected_watch(&mut app, winner.index, &changed);
        assert!(stamp.is_current());
        no_downloads(&fast, &slow);
    }
    // Recommending new preferences supersedes the old request even with the
    // same winning handle; neither operation may prepare media.
    let changed = PlaybackPreferences {
        subtitles_off: true,
        ..preferences.clone()
    };
    assert_eq!(recommend(&mut app, &changed).1, Some(winner.index));
    rejected_watch(&mut app, winner.index, &preferences);
    no_downloads(&fast, &slow);
    assert_eq!(recommend(&mut app, &preferences).1, Some(winner.index));

    app.command(Command::WatchRecommended {
        index: winner.index,
        preferences,
        resume: false,
    });
    app.wait("only explicit watch prepares the winning movie", |event| {
        quiet(event);
        assert!(!matches!(event, Event::MovieWatchNeedsInput(_)));
        if let Event::SourceSelected {
            playable: true,
            suggested_filename,
            ..
        } = event
        {
            assert_eq!(suggested_filename, "Winner.mp4");
            return true;
        }
        false
    });
    quota(&mut app);
    assert!(!stamp.is_current());
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 1);
    finish_quiet(&mut app);
    assert!(db.history(10).unwrap().is_empty());
    assert!(db.saved_files().unwrap().is_empty());
    assert!(db.load_progress(&target).unwrap().is_none());
}

#[test]
fn watch_recommended_revalidates_handles_settings_and_source_preferences() {
    if isolated_case("watch_recommended_revalidates_handles_settings_and_source_preferences") {
        return;
    }
    for invalidation in [
        "refresh",
        "navigation",
        "cancel",
        "catalog",
        "torrent",
        "sources",
    ] {
        let root = tempfile::tempdir().unwrap();
        let fast = OfferOrigin::new(2020, Some(3), metadata("Alternate.mp4", 3), false);
        let slow = OfferOrigin::new(2020, Some(27), metadata("Winner.mp4", 7), true);
        let (mut app, mut db, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
        begin(&mut app, &slow, &target);
        slow.release();
        snapshot(&mut app, &target, 0);
        let preferences = PlaybackPreferences::default();
        let (stamp, index) = recommend(&mut app, &preferences);
        let index = index.expect("completed positive-seed movie has a recommendation");
        match invalidation {
            "refresh" => {
                app.command(Command::RetrySources);
                slow.held.recv_timeout(WAIT).unwrap();
                slow.release();
                let (_, rows) = snapshot(&mut app, &target, 0);
                assert!(rows.iter().all(|row| row.index != index));
                assert_ne!(recommend(&mut app, &preferences).1, Some(index));
            }
            "navigation" => {
                let other = second_card(&mut db);
                open_card(&mut app, &other);
            }
            "cancel" => {
                app.command(Command::Cancel);
                app.wait("cancellation clears source rows", |event| {
                    unprepared(event);
                    matches!(event, Event::SourcesState(AsyncState::Ready(rows)) if rows.is_empty())
                });
            }
            "catalog" | "torrent" => {
                let mut settings = Settings::from_storage(db.load_settings().unwrap());
                if invalidation == "catalog" {
                    settings.catalog_enabled = false;
                } else {
                    settings.network.torrents_enabled = false;
                }
                save_settings(&mut app, &settings);
            }
            "sources" => {
                let stored = SourcePreferences {
                    rutracker_enabled: false,
                    commons_enabled: false,
                    ..Default::default()
                };
                app.command(Command::SetSourcePreferences(stored.clone()));
                app.wait("provider permissions revoked", |event| {
                    unprepared(event);
                    matches!(event, Event::SourcePreferences(saved) if saved == &stored)
                });
            }
            _ => unreachable!(),
        }
        assert!(!stamp.is_current(), "{invalidation}");
        rejected_watch(&mut app, index, &preferences);
        assert!(app.backend.join());
        while let Ok(event) = app.events.try_recv() {
            unprepared(&event);
        }
        no_downloads(&fast, &slow);
        assert!(db.history(10).unwrap().is_empty());
        assert!(db.saved_files().unwrap().is_empty());
        assert!(db.source_for_target(&target).unwrap().is_none());
    }
}

#[test]
fn watch_recommended_prepares_only_the_exact_episode_after_explicit_watch() {
    if isolated_case("watch_recommended_prepares_only_the_exact_episode_after_explicit_watch") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::release_offer(
        "Fixture 2020 S02E04 1080p",
        "5000",
        Some(999),
        metadata("Wrong.S02E04.mp4", 3),
        false,
    );
    let slow = OfferOrigin::release_offer(
        "Fixture 2020 S02E03 1080p",
        "5000",
        Some(27),
        metainfo(
            &[
                ("README.txt", b"owned fixture".to_vec()),
                ("Fixture.S02E04.mp4", vec![4; 2 * 1024 * 1024]),
                ("Fixture.S02E03.mp4", vec![3; 2 * 1024 * 1024]),
            ],
            true,
        ),
        true,
    );
    let (mut app, mut db, _) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let id = MediaId::new("trakt:show:54").unwrap();
    db.upsert_media(&kadr_storage::MediaRecord {
        id: id.clone(),
        title: "Fixture".into(),
        original_title: None,
        year: Some(2020),
        kind: kadr_storage::MediaKind::Series,
        description: String::new(),
        poster_url: None,
        attribution: String::new(),
    })
    .unwrap();
    db.set_favorite(&id, true).unwrap();
    let mut target = PlaybackTarget {
        media_id: id,
        episode: None,
    };
    open_card(&mut app, &target);
    app.command(Command::AssignEpisode(2, 3));
    app.wait("exact episode assigned to cached series", |event| {
        unprepared(event);
        matches!(event, Event::Status(Level::Info, message)
            if message.contains("Привязка эпизода изменена"))
    });
    target.episode = Some(EpisodeKey::new(2, 3).unwrap());
    app.command(Command::FindSources);
    slow.held.recv_timeout(WAIT).unwrap();
    let preferences = PlaybackPreferences {
        preferred_resolution: Some("1080p".into()),
        preferred_language: Some("en".into()),
        subtitles_off: true,
    };
    assert_eq!(recommend(&mut app, &preferences).1, None);
    no_downloads(&fast, &slow);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    assert_eq!(
        rows.len(),
        1,
        "wrong episode must not compete by seed count"
    );
    assert_eq!(rows[0].origin, "Slow fixture");
    let (stamp, index) = recommend(&mut app, &preferences);
    assert_eq!(index, Some(rows[0].index));
    no_downloads(&fast, &slow);
    app.command(Command::WatchRecommended {
        index: index.unwrap(),
        preferences,
        resume: false,
    });
    let Event::TorrentFiles(files) = app.wait("watch inspects the episode pack", |event| {
        quiet(event);
        assert!(!matches!(event, Event::MovieWatchNeedsInput(_)));
        matches!(event, Event::TorrentFiles(files) if !files.is_empty())
    }) else {
        unreachable!()
    };
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|file| {
        file.index == 2 && file.season == Some(2) && file.episode == Some(3) && !file.ambiguous
    }));
    app.wait(
        "watch selects exact episode rather than the first video",
        |event| {
            quiet(event);
            if let Event::SourceSelected {
                playable: true,
                suggested_filename,
                ..
            } = event
            {
                assert_eq!(suggested_filename, "Fixture.S02E03.mp4");
                return true;
            }
            assert!(!matches!(event, Event::MovieWatchNeedsInput(_)));
            false
        },
    );
    quota(&mut app);
    assert!(!stamp.is_current());
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 1);
    finish_quiet(&mut app);
    assert!(db.history(10).unwrap().is_empty());
    assert!(db.saved_files().unwrap().is_empty());
    assert!(db.load_progress(&target).unwrap().is_none());
}

#[test]
fn slower_positive_seed_winner_is_recommended_only_at_completion() {
    if isolated_case("slower_positive_seed_winner_is_recommended_only_at_completion") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(3), metadata("Fast.mp4", 3), false);
    let slow = OfferOrigin::new(2020, Some(27), metadata("Winner.mp4", 7), true);
    let (mut app, db, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let (partial_stamp, partial) = begin(&mut app, &slow, &target);
    assert_eq!(partial.seeds, Some(3));
    assert!(!partial.recommended);
    app.command(Command::WatchRelease(partial.index));
    app.wait("pending handle cannot prepare or start playback", |event| {
        quiet(event);
        matches!(event, Event::Status(Level::Error | Level::Warning, _))
    });
    assert!(
        partial_stamp.is_current(),
        "rejection must not cancel discovery"
    );
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    slow.release();
    let (stamp, rows) = snapshot(&mut app, &target, 0);
    assert!(partial_stamp.is_current());
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().find(|r| r.index == partial.index),
        Some(&partial),
        "cumulative handle changed"
    );
    let recommended: Vec<_> = rows.iter().filter(|r| r.recommended).collect();
    assert_eq!(recommended.len(), 1);
    assert_eq!(recommended[0].origin, "Slow fixture");
    assert_eq!(recommended[0].seeds, Some(27));
    app.command(Command::WatchRelease(recommended[0].index));
    quota(&mut app);
    assert!(!stamp.is_current());
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 1);
    assert!(db.load_progress(&target).unwrap().is_none());
    finish_quiet(&mut app);
}

#[test]
fn more_seeders_from_another_year_cannot_win() {
    if isolated_case("more_seeders_from_another_year_cannot_win") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(3), metadata("Correct.mp4", 3), false);
    let slow = OfferOrigin::new(2021, Some(999), metadata("Wrong.mp4", 7), true);
    let (mut app, _, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let (_, partial) = begin(&mut app, &slow, &target);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].index, partial.index);
    assert_eq!(rows[0].seeds, Some(3));
    assert!(rows[0].recommended);
    app.command(Command::WatchRelease(rows[0].index));
    quota(&mut app);
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    finish_quiet(&mut app);
}

#[test]
fn zero_and_unknown_seeders_remain_visible_without_a_winner_or_starts() {
    if isolated_case("zero_and_unknown_seeders_remain_visible_without_a_winner_or_starts") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(0), metadata("Zero.mp4", 3), false);
    let slow = OfferOrigin::new(2020, None, metadata("Unknown.mp4", 7), true);
    let (mut app, db, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let (_, partial) = begin(&mut app, &slow, &target);
    assert_eq!(partial.seeds, Some(0));
    slow.release();
    let (stamp, rows) = snapshot(&mut app, &target, 0);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter().find(|r| r.index == partial.index),
        Some(&partial)
    );
    assert!(rows.iter().any(|r| r.seeds.is_none()));
    assert!(rows.iter().all(|r| !r.recommended));
    // The UI owns this decision: no WatchRelease command is sent without a winner.
    finish_quiet(&mut app);
    assert!(!stamp.is_current());
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    assert!(db.load_progress(&target).unwrap().is_none());
}

#[test]
fn movie_watch_requires_input_when_metainfo_only_contains_an_episode() {
    if isolated_case("movie_watch_requires_input_when_metainfo_only_contains_an_episode") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, None, metadata("Alternate.mp4", 1), false);
    let slow = OfferOrigin::new(2020, Some(27), metadata("Fixture.S01E01.mp4", 2), true);
    let (mut app, _, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    begin(&mut app, &slow, &target);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    assert!(rows[0].recommended);
    app.command(Command::WatchRelease(rows[0].index));
    app.wait("episodic file cannot autoplay from a movie card", |event| {
        quiet(event);
        assert!(!matches!(
            event,
            Event::SourceSelected { playable: true, .. }
        ));
        matches!(event, Event::MovieWatchNeedsInput(stamp) if stamp.is_current())
    });
    finish_quiet(&mut app);
}

#[test]
fn explicit_alternate_uses_choose_source_not_watch_release() {
    if isolated_case("explicit_alternate_uses_choose_source_not_watch_release") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(3), metadata("Alternate.mp4", 3), false);
    let slow = OfferOrigin::new(2020, Some(27), metadata("Winner.mp4", 7), true);
    let (mut app, _, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let (_, partial) = begin(&mut app, &slow, &target);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    assert_eq!(rows.iter().filter(|r| r.recommended).count(), 1);
    assert!(
        !rows
            .iter()
            .find(|r| r.index == partial.index)
            .unwrap()
            .recommended
    );
    app.command(Command::WatchRelease(partial.index));
    app.wait("nonrecommended handle rejected", |event| {
        quiet(event);
        matches!(event, Event::Status(Level::Error | Level::Warning, _))
    });
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    app.command(Command::ChooseSource(partial.index));
    app.wait("explicit alternate prepared but not auto-played", |event| {
        quiet(event);
        matches!(event, Event::SourceSelected { playable: true, suggested_filename, .. } if suggested_filename == "Alternate.mp4")
    });
    app.command(Command::Play { resume: false });
    quota(&mut app);
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 1);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    finish_quiet(&mut app);
}

#[test]
fn cancellation_invalidates_partial_stamp_and_late_winner_without_starts() {
    if isolated_case("cancellation_invalidates_partial_stamp_and_late_winner_without_starts") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fast = OfferOrigin::new(2020, Some(3), metadata("Fast.mp4", 3), false);
    let slow = OfferOrigin::new(2020, Some(27), metadata("Late.mp4", 7), true);
    let (mut app, db, target) = start(&paths(root.path()), &fast, &slow, options(vec![]));
    let (stamp, partial) = begin(&mut app, &slow, &target);
    assert!(!partial.recommended);
    app.command(Command::Cancel);
    assert!(
        !stamp.is_current(),
        "cancellation must invalidate queued UI snapshots synchronously"
    );
    app.wait("cancel clears discovery rows", |event| {
        quiet(event);
        matches!(event, Event::SourcesState(AsyncState::Ready(rows)) if rows.is_empty())
    });
    slow.closed
        .recv_timeout(WAIT)
        .expect("cancel closed held search before shutdown");
    slow.release();
    // No WatchRelease on cancellation. Drain even shutdown-queued events so a
    // late response cannot hide behind a predicate-only wait.
    assert!(app.backend.join());
    while let Ok(event) = app.events.try_recv() {
        quiet(&event);
        if let Event::SourceDiscovery { stamp, rows, .. } = event {
            assert!(!stamp.is_current());
            assert!(rows.iter().all(|r| !r.recommended));
        }
    }
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 0);
    assert!(db.load_progress(&target).unwrap().is_none());
}

#[test]
#[ignore = "requires ffmpeg and mpv; generated winner torrent -> default player -> measured IPC progress"]
fn real_watch_release_plays_video_from_the_final_winner() {
    if isolated_case("real_watch_release_plays_video_from_the_final_winner") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let filename = "Winner.mp4";
    let bytes = generate_video(&root.path().join(filename));
    let winner = metainfo(&[(filename, bytes.clone())], true);
    let prepared = PreparedTorrent::from_bytes(winner.clone().into()).unwrap();
    let seed = LocalSeed::start(root.path(), winner.clone());
    let fast = OfferOrigin::new(2020, Some(3), metadata("Loser.mp4", 3), false);
    let slow = OfferOrigin::new(2020, Some(27), winner, true);
    let paths = paths(root.path());
    let (mut app, db, target) = start(&paths, &fast, &slow, options(vec![seed.address()]));
    let mut settings = Settings::from_storage(db.load_settings().unwrap());
    settings.cache_limit_bytes = bytes.len() as u64 + 1024 * 1024;
    save_settings(&mut app, &settings);
    begin(&mut app, &slow, &target);
    slow.release();
    let (_, rows) = snapshot(&mut app, &target, 0);
    let recommended: Vec<_> = rows.iter().filter(|r| r.recommended).collect();
    assert_eq!(recommended.len(), 1);
    assert_eq!(recommended[0].origin, "Slow fixture");
    // No ChooseSource, Play, or OpenIn: this single command must prepare and
    // launch the configured default mpv through the normal torrent pipeline.
    app.command(Command::WatchRelease(recommended[0].index));
    app.wait("default mpv started by WatchRelease", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    let progress = measured(&db, &target, 1.0);
    assert_eq!(
        progress.source_id.as_str(),
        format!("remote-torrent:v1:{}:file:0", prepared.info_hash)
    );
    assert_eq!(fast.downloads.load(Ordering::SeqCst), 0);
    assert_eq!(slow.downloads.load(Ordering::SeqCst), 1);
    app.stopped();
    app.finish();
}
