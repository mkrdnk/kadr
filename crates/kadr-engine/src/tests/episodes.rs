//! Episode acceptance at the engine command boundary, not native GUI or live
//! Trakt acceptance. Source search uses the real local bound provider/resolver;
//! opt-in playback uses generated media, a loopback seed and actual mpv IPC.

use super::*;

fn consent(app: &mut App) {
    let mut settings = app.ready();
    settings.network.torrents_enabled = true;
    app.command(Command::UpdateSettings(settings));
    app.wait("torrent consent persisted", |event| {
        matches!(event, Event::PreferencesSaved(_))
    });
}

/// Require the next selection acknowledgement, rather than skipping an unsafe
/// playable acknowledgement while waiting for a later unplayable one.
fn selection(app: &mut App, playable: bool) -> String {
    let Event::SourceSelected {
        playable: actual,
        suggested_filename,
        ..
    } = app.wait("selection acknowledgement", |event| {
        matches!(event, Event::SourceSelected { .. })
    })
    else {
        unreachable!()
    };
    assert_eq!(actual, playable);
    suggested_filename
}

fn open_torrent(app: &mut App, path: &Path, videos: usize) -> Vec<FileChoice> {
    app.command(Command::SelectSource(
        ManualSource::torrent_file(path.to_owned()).unwrap(),
    ));
    selection(app, false); // Metadata preparation is not a playable selection.
    let Event::TorrentFiles(files) = app.wait(
        "prepared video choices",
        |event| matches!(event, Event::TorrentFiles(files) if !files.is_empty()),
    ) else {
        unreachable!()
    };
    assert_eq!(files.len(), videos);
    files
}

fn cannot_launch_or_save(app: &mut App, destination: &Path) {
    app.command(Command::Play { resume: false });
    app.wait("no launchable selected file", |event| match event {
        Event::PlaybackActive(active) => {
            assert!(
                !active,
                "an unresolved/contradictory episode must not launch"
            );
            true
        }
        _ => false,
    });
    app.wait("actionable missing file error", |event| {
        matches!(event, Event::Status(Level::Error, message) if message.contains("Выберите видеофайл"))
    });
    app.command(Command::SaveTo(destination.to_owned()));
    app.wait("save rejected without active playback", |event| {
        matches!(event, Event::Status(Level::Warning, message) if message.contains("Сначала запустите"))
    });
    assert!(!destination.exists());
}

#[test]
fn explicit_episode_never_auto_selects_inconclusive_wrong_or_duplicate_files() {
    for names in [
        vec!["Own.unknown.mp4"],
        vec!["Own.S01E01E02.mp4"],
        vec!["Own.S01E01-S01E03.mp4"],
        vec!["Own.S01E02.mp4"],
        vec!["Own.S02E01.mp4"],
        vec!["Own.S01E01.mp4", "Other.S01E01.mp4"],
    ] {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let torrent = root.path().join("owned.torrent");
        fs::write(
            &torrent,
            metainfo(
                &names
                    .iter()
                    .map(|name| (*name, vec![1; 32 * 1024]))
                    .collect::<Vec<_>>(),
                true,
            ),
        )
        .unwrap();
        let mut app = App::start(&paths);
        consent(&mut app);
        app.command(Command::AssignEpisode(1, 1));
        open_torrent(&mut app, &torrent, names.len());
        selection(&mut app, false);
        cannot_launch_or_save(&mut app, &root.path().join("must-not-save.mp4"));
        app.finish();
        let db = Storage::open(&paths.database_path()).unwrap();
        assert!(db.history(10).unwrap().is_empty(), "{names:?}");
        assert!(db.saved_files().unwrap().is_empty(), "{names:?}");
        assert!(db.library_targets(10).unwrap().iter().all(|item| {
            item.progress.is_none()
                && item
                    .source
                    .as_ref()
                    .and_then(StoredSource::file_id)
                    .is_none()
        }));
    }
}

#[test]
fn explicit_episode_allows_manual_unknown_and_pack_confirmation_and_assignment() {
    for (name, ambiguous) in [
        ("Own.unknown.mp4", false),
        ("Own.S01E01E02.mp4", true),
        ("Own.S01E01-S01E03.mp4", true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let torrent = root.path().join("owned.torrent");
        fs::write(&torrent, metainfo(&[(name, vec![1; 32 * 1024])], true)).unwrap();
        let mut app = App::start(&paths(root.path()));
        consent(&mut app);
        app.command(Command::AssignEpisode(1, 1));
        let files = open_torrent(&mut app, &torrent, 1);
        assert_eq!(files[0].ambiguous, ambiguous);
        assert_eq!((files[0].season, files[0].episode), (None, None));
        selection(&mut app, false);
        app.command(Command::ChooseTorrentFile(files[0].index));
        assert_eq!(selection(&mut app, true), name);
        app.command(Command::AssignEpisode(1, 2));
        assert_eq!(selection(&mut app, true), name);
        app.finish();
    }
}

fn contradictory_choice(app: &mut App, index: usize, chosen: &str, target: &str) {
    app.command(Command::ChooseTorrentFile(index));
    selection(app, false);
    app.wait(
        "explicit episode contradiction with recovery instructions",
        |event| {
            matches!(event, Event::Status(Level::Error, message)
            if message.contains(chosen) && message.contains(target)
                && message.contains("другой файл") && message.contains("измените эпизод")
                && message.contains("отвяжите"))
        },
    );
}

#[test]
fn contradictory_explicit_choice_clears_previous_file_and_supports_recovery() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let torrent = root.path().join("owned.torrent");
    fs::write(
        &torrent,
        metainfo(
            &[
                ("README.txt", b"owned fixture".to_vec()),
                ("Own.S01E02.mp4", vec![2; 32 * 1024]),
                ("Own.S01E01.mp4", vec![1; 32 * 1024]),
            ],
            true,
        ),
    )
    .unwrap();
    let mut app = App::start(&paths);
    consent(&mut app);
    app.command(Command::AssignEpisode(1, 1));
    let files = open_torrent(&mut app, &torrent, 2);
    assert_eq!((files[0].index, files[1].index), (1, 2));
    assert_eq!(selection(&mut app, true), "Own.S01E01.mp4");
    contradictory_choice(&mut app, 1, "S01E02", "S01E01");
    cannot_launch_or_save(&mut app, &root.path().join("wrong.mp4"));
    app.command(Command::ChooseTorrentFile(2));
    assert_eq!(selection(&mut app, true), "Own.S01E01.mp4");

    // Changing a target cannot leave a previously selected contradictory file
    // launchable. Unknown/pack assignment remains covered separately.
    app.command(Command::AssignEpisode(1, 2));
    selection(&mut app, false);
    app.wait("existing file revalidated on assignment", |event| {
        matches!(event, Event::Status(Level::Error, message)
            if message.contains("S01E01") && message.contains("S01E02"))
    });
    app.command(Command::ChooseTorrentFile(1));
    assert_eq!(selection(&mut app, true), "Own.S01E02.mp4");
    app.command(Command::ChooseTorrentFile(999));
    selection(&mut app, false);
    app.wait("invalid file handle remains recoverable", |event| {
        matches!(event, Event::Status(Level::Error, message) if message.contains("из списка"))
    });
    app.command(Command::ChooseTorrentFile(1));
    selection(&mut app, true);
    app.command(Command::UnlinkMedia);
    app.wait("manual unlink recovery", |event| {
        matches!(event, Event::MediaSelected { media: None, .. })
    });
    app.command(Command::ChooseTorrentFile(2));
    assert_eq!(selection(&mut app, true), "Own.S01E01.mp4");
    app.finish();
    let db = Storage::open(&paths.database_path()).unwrap();
    assert!(db.history(10).unwrap().is_empty());
    assert!(db.saved_files().unwrap().is_empty());
}

#[test]
fn unbound_sole_unknown_or_pack_remains_automatically_selectable() {
    for name in ["Own.unknown.mp4", "Own.S01E01E02.mp4"] {
        let root = tempfile::tempdir().unwrap();
        let torrent = root.path().join("owned.torrent");
        fs::write(&torrent, metainfo(&[(name, vec![1; 32 * 1024])], true)).unwrap();
        let mut app = App::start(&paths(root.path()));
        consent(&mut app);
        open_torrent(&mut app, &torrent, 1);
        assert_eq!(selection(&mut app, true), name);
        app.finish();
    }
}

fn bind_episode(paths: &AppPaths, descriptor: &StoredSource) -> PlaybackTarget {
    let target = PlaybackTarget {
        media_id: MediaId::new("trakt:show:905").unwrap(),
        episode: Some(EpisodeKey::new(1, 1).unwrap()),
    };
    let mut db = Storage::open_default(paths).unwrap();
    db.upsert_media(&kadr_storage::MediaRecord {
        id: target.media_id.clone(),
        title: "Owned series".into(),
        original_title: None,
        year: Some(2024),
        kind: kadr_storage::MediaKind::Series,
        description: String::new(),
        poster_url: None,
        attribution: String::new(),
    })
    .unwrap();
    let id = descriptor.source_id().unwrap();
    db.save_source(&id, descriptor).unwrap();
    db.link_source(&target, &id).unwrap();
    target
}

#[test]
fn persisted_manual_pack_choice_and_stale_file_recovery_are_preserved() {
    for index in [0, 999] {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let torrent = root.path().join("owned.torrent");
        let name = "Own.S01E01E02.mp4";
        fs::write(&torrent, metainfo(&[(name, vec![1; 32 * 1024])], true)).unwrap();
        let descriptor = StoredSource::TorrentFile {
            path: torrent,
            file_id: Some(index),
        };
        let target = bind_episode(&paths, &descriptor);
        let mut app = App::start(&paths);
        consent(&mut app);
        app.command(Command::OpenLibrary(library_id(&target)));
        app.wait("library restored", |event| {
            matches!(
                event,
                Event::LibraryOpened {
                    source_available: true
                }
            )
        });
        selection(&mut app, false); // Preparation.
        app.wait(
            "restored pack choices",
            |event| matches!(event, Event::TorrentFiles(files) if files.len() == 1),
        );
        if index == 0 {
            assert_eq!(selection(&mut app, true), name);
        } else {
            selection(&mut app, false);
            app.wait("stale stored file rejected", |event| {
                matches!(event, Event::Status(Level::Error, message) if message.contains("из списка"))
            });
            app.command(Command::ChooseTorrentFile(0));
            assert_eq!(selection(&mut app, true), name);
        }
        app.finish();
        assert_eq!(
            Storage::open(&paths.database_path())
                .unwrap()
                .source_for_target(&target)
                .unwrap(),
            Some(descriptor),
            "restoring/recovering a selection alone must not rewrite history"
        );
    }
}

pub(super) fn play_selected_episode() {
    use kadr_torrent::engine::{EngineOptions, RateLimit as EngineRate};

    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let correct = generate_video(&root.path().join("correct.mp4"));
    let wrong = generate_video_from(
        &root.path().join("wrong.mp4"),
        "color=c=blue:size=160x90:rate=10",
    );
    assert_ne!(correct, wrong, "the episodes must be distinguishable");
    let files = [
        ("Own.S01E01.mp4", correct.clone()),
        ("Own.S01E02.mp4", wrong),
        ("README.txt", b"generated, owned episode fixtures".to_vec()),
    ];
    let metadata = metainfo(&files, true);
    let seed_root = root.path().join("seed");
    fs::create_dir_all(seed_root.join("owned-series")).unwrap();
    for (name, bytes) in &files {
        fs::write(seed_root.join("owned-series").join(name), bytes).unwrap();
    }
    let seed = LocalSeed::start(&seed_root, metadata.clone());
    let torrent = root.path().join("owned.torrent");
    fs::write(&torrent, metadata).unwrap();
    let target = bind_episode(
        &paths,
        &StoredSource::TorrentFile {
            path: torrent.clone(),
            file_id: None,
        },
    );
    let other = PlaybackTarget {
        media_id: target.media_id.clone(),
        episode: Some(EpisodeKey::new(1, 2).unwrap()),
    };
    let options = EngineOptions {
        network_enabled: true,
        discovery_enabled: false,
        initial_peers: vec![seed.address()],
        download_limit: EngineRate::Bps(512 * 1024),
        upload_limit: EngineRate::Paused,
        peer_limit: 1,
    };
    let (backend, events) =
        Engine::start_with_network(Some(paths.clone()), true, Some(options)).unwrap();
    let mut app = App { backend, events };
    consent(&mut app);
    app.command(Command::OpenLibrary(library_id(&target)));
    app.wait("episode identity restored", |event| {
        matches!(event, Event::MediaSelected { media: Some(media), episode: Some((1, 1)), .. }
            if media.id == target.media_id.as_str())
    });
    app.wait(
        "bound torrent prepared",
        |event| matches!(event, Event::TorrentFiles(files) if files.len() == 2),
    );
    selection(&mut app, false);
    app.command(Command::FindSources);
    let Event::SourceDiscovery { rows, failures, .. } = app.wait(
        "actual episode query completed by local provider",
        |event| {
            matches!(event, Event::SourceDiscovery {
                media_id, episode: Some((1, 1)), pending: 0, succeeded: 1, ..
            } if media_id == target.media_id.as_str())
        },
    ) else {
        unreachable!()
    };
    assert!(failures.is_empty());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Owned series");
    assert_eq!(rows[0].kind, SourceKind::TorrentFile);
    app.command(Command::ChooseSource(rows[0].index));
    selection(&mut app, false); // Metadata preparation.
    let Event::TorrentFiles(choices) = app.wait(
        "source choice prepares both episodes",
        |event| matches!(event, Event::TorrentFiles(files) if files.len() == 2),
    ) else {
        unreachable!()
    };
    assert_eq!((choices[0].index, choices[0].episode), (0, Some(1)));
    assert_eq!((choices[1].index, choices[1].episode), (1, Some(2)));
    assert_eq!(selection(&mut app, true), "Own.S01E01.mp4");

    // Even an explicit click on S01E02 must not launch/save it as S01E01.
    contradictory_choice(&mut app, 1, "S01E02", "S01E01");
    cannot_launch_or_save(&mut app, &root.path().join("rejected.mp4"));
    app.command(Command::ChooseTorrentFile(0));
    assert_eq!(selection(&mut app, true), "Own.S01E01.mp4");
    app.command(Command::Play { resume: false });
    app.wait("actual mpv launched for S01E01", |event| {
        matches!(event, Event::PlaybackActive(true))
    });
    let db = Storage::open(&paths.database_path()).unwrap();
    measured(&db, &target, 1.0);
    let expected = StoredSource::TorrentFile {
        path: torrent,
        file_id: Some(0),
    };
    assert_eq!(
        db.source_for_target(&target).unwrap(),
        Some(expected.clone())
    );
    let saved = root.path().join("saved-S01E01.mp4");
    app.command(Command::SaveTo(saved.clone()));
    eventually(WAIT, "verified S01E01 saved under its own target", || {
        db.saved_file(&target).unwrap().is_some()
    });
    assert_eq!(fs::read(&saved).unwrap(), correct);
    assert_eq!(db.saved_file(&target).unwrap().unwrap().path, saved);
    assert!(db.load_progress(&other).unwrap().is_none());
    assert!(db.source_for_target(&other).unwrap().is_none());
    assert!(db.saved_file(&other).unwrap().is_none());
    let items = db.library_targets(10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].target, target);
    assert_eq!(items[0].source, Some(expected));
    app.stopped();
    app.finish();
    drop(seed);
}
