use kadr_storage::{
    EpisodeKey, MediaId, PlaybackProgress, PlaybackTarget, ProgressOrigin, SavedFile, Storage,
    StoredSource,
};

fn target(episode: Option<u16>) -> PlaybackTarget {
    PlaybackTarget {
        media_id: MediaId::new("catalog:series:1").unwrap(),
        episode: episode.map(|episode| EpisodeKey::new(1, episode).unwrap()),
    }
}

fn source(file_id: usize) -> StoredSource {
    StoredSource::TorrentLink {
        uri: "magnet:?xt=urn:btih:0123456789012345678901234567890123456789".into(),
        file_id: Some(file_id),
    }
}

fn observation(episode: u16, source: &StoredSource, updated_at: i64) -> PlaybackProgress {
    PlaybackProgress {
        target: target(Some(episode)),
        source_id: source.source_id().unwrap(),
        position_seconds: f64::from(episode) * 10.0,
        duration_seconds: Some(100.0),
        updated_at,
        completed: false,
        origin: ProgressOrigin::Mpv,
    }
}

#[test]
fn rutracker_history_keeps_topic_hash_and_each_selected_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let first = StoredSource::RuTracker {
        topic_id: 42,
        info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
        file_id: Some(7),
    };
    let mut second = first.clone();
    if let StoredSource::RuTracker { file_id, .. } = &mut second {
        *file_id = Some(9);
    }
    let id = first.source_id().unwrap();
    assert_eq!(second.source_id().unwrap(), id);
    assert!(first.manual_source().is_err());
    for (episode, source) in [(1, &first), (2, &second)] {
        let progress = observation(episode, source, i64::from(episode));
        store.save_source(&id, source).unwrap();
        store.link_source(&progress.target, &id).unwrap();
        store.save_progress_for_source(&progress, source).unwrap();
    }
    drop(store);

    let store = Storage::open(&path).unwrap();
    for (episode, source) in [(1, first), (2, second)] {
        let item = target(Some(episode));
        assert_eq!(
            store.source_for_target(&item).unwrap(),
            Some(source.clone())
        );
        assert_eq!(
            store.library_target(&item).unwrap().unwrap().source,
            Some(source)
        );
    }
}

#[test]
fn every_episode_remains_resumable_with_its_immutable_selection_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let first = source(7);
    let second = source(9);
    let id = first.source_id().unwrap();
    let earlier = observation(1, &first, 100);
    let later = observation(2, &second, 200);
    store.save_source(&id, &first).unwrap();
    store.link_source(&earlier.target, &id).unwrap();
    store.save_progress_for_source(&earlier, &first).unwrap();
    store.save_source(&id, &second).unwrap();
    store.link_source(&later.target, &id).unwrap();
    store.save_progress_for_source(&later, &second).unwrap();
    // Neither a same-torrent file reselection nor a newer browser launch may
    // reinterpret an episode's measured position.
    store.save_source(&id, &source(11)).unwrap();
    store.link_source(&later.target, &id).unwrap();
    let browser = StoredSource::Link("https://browser.example/new-video?token=secret".into());
    let browser_id = browser.source_id().unwrap();
    store.save_source(&browser_id, &browser).unwrap();
    store
        .record_opened(&target(None).media_id, &browser_id, 300)
        .unwrap();
    drop(store);

    let store = Storage::open(&path).unwrap();
    let rows = store.library_targets(100).unwrap();
    assert_eq!(
        rows.len(),
        3,
        "progress and links must not duplicate targets"
    );
    assert_eq!(rows[0].target, target(None));
    assert_eq!(rows[0].source, Some(browser));
    assert!(rows[0].progress.is_none());
    assert_eq!(rows[1].target, later.target);
    assert_eq!(rows[1].progress, Some(later.clone()));
    assert_eq!(rows[1].source, Some(second));
    assert_eq!(rows[2].target, earlier.target);
    assert_eq!(rows[2].progress, Some(earlier.clone()));
    assert_eq!(rows[2].source, Some(first));
    assert_eq!(
        store.source_for_target(&later.target).unwrap(),
        Some(source(11))
    );
    for row in &rows {
        assert_eq!(
            store.library_target(&row.target).unwrap().as_ref(),
            Some(row)
        );
    }
    assert_eq!(store.library_targets(2).unwrap(), rows[..2]);
    // Existing one-row-per-media API retains its latest-progress semantics.
    let summary = store.library(100).unwrap().remove(0);
    assert_eq!(summary.target, later.target);
    assert_eq!(summary.progress, Some(later));
    assert_eq!(summary.source, rows[1].source);
}

#[test]
fn source_only_saved_only_and_favorite_only_targets_have_explicit_membership() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    let favorite = store.library_targets(10).unwrap();
    assert_eq!(favorite.len(), 1);
    assert_eq!(favorite[0].target, target(None));
    assert!(favorite[0].entry.favorite);
    assert!(favorite[0].source.is_none());
    assert!(store.library_target(&target(Some(1))).unwrap().is_none());
    store.set_favorite(&target(None).media_id, false).unwrap();
    assert!(store.library_targets(10).unwrap().is_empty());
    assert!(store.library_target(&target(None)).unwrap().is_none());

    let saved = SavedFile {
        target: target(Some(4)),
        path: dir.path().join("episode4.mp4"),
        saved_at: 100,
    };
    store.save_saved_file(&saved).unwrap();
    let linked = source(6);
    let id = linked.source_id().unwrap();
    store.save_source(&id, &linked).unwrap();
    store.link_source(&target(Some(5)), &id).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    let rows = store.library_targets(10).unwrap();
    assert_eq!(
        rows.len(),
        2,
        "a favorite must not fabricate an extra item target"
    );
    assert_eq!(rows[0].target, saved.target);
    assert_eq!(
        store.saved_file(&rows[0].target).unwrap(),
        Some(saved.clone())
    );
    assert!(rows[0].source.is_none());
    assert_eq!(rows[1].target, target(Some(5)));
    assert_eq!(rows[1].source, Some(linked));
    assert!(
        rows.iter()
            .all(|row| row.entry.favorite && row.progress.is_none())
    );
    assert!(store.library_target(&target(None)).unwrap().is_none());
    store.remove_saved_file(&saved.target).unwrap();
    assert!(store.library_target(&saved.target).unwrap().is_none());
    assert_eq!(store.library_targets(10).unwrap().len(), 1);
    // Source-only targets are intentional members of this API even without
    // a favorite/history/progress row.
    store.set_favorite(&target(None).media_id, false).unwrap();
    assert_eq!(store.library_targets(10).unwrap().len(), 1);
    store.clear_playback_data().unwrap();
    assert!(store.library_targets(10).unwrap().is_empty());
}

#[test]
fn clearing_history_keeps_each_favorite_episode_and_saved_copy_selectable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    for episode in [1, 2] {
        let descriptor = source(usize::from(episode) + 10);
        let id = descriptor.source_id().unwrap();
        store.save_source(&id, &descriptor).unwrap();
        store.link_source(&target(Some(episode)), &id).unwrap();
        store
            .save_progress_for_source(
                &observation(episode, &descriptor, i64::from(episode)),
                &descriptor,
            )
            .unwrap();
    }
    let saved = SavedFile {
        target: target(Some(3)),
        path: dir.path().join("episode3.mp4"),
        saved_at: 3,
    };
    store.save_saved_file(&saved).unwrap();
    store.clear_playback_data().unwrap();
    drop(store);
    let store = Storage::open(&path).unwrap();
    let rows = store.library_targets(100).unwrap();
    assert_eq!(rows.len(), 3);
    for episode in [1, 2] {
        let item = store
            .library_target(&target(Some(episode)))
            .unwrap()
            .unwrap();
        assert_eq!(item.target, target(Some(episode)));
        assert!(item.progress.is_none());
        assert_eq!(item.source, Some(source(usize::from(episode) + 10)));
        assert!(item.entry.favorite);
    }
    let item = store.library_target(&saved.target).unwrap().unwrap();
    assert_eq!(store.saved_file(&item.target).unwrap(), Some(saved));
    assert!(item.source.is_none());
    assert!(item.progress.is_none());
    assert!(store.library_target(&target(None)).unwrap().is_none());
}

#[test]
fn exact_lookup_never_falls_back_and_limits_are_bounded_and_stable() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    for episode in [2, 1] {
        let descriptor = source(usize::from(episode));
        let id = descriptor.source_id().unwrap();
        store.save_source(&id, &descriptor).unwrap();
        store.link_source(&target(Some(episode)), &id).unwrap();
        store
            .save_progress_for_source(&observation(episode, &descriptor, 10), &descriptor)
            .unwrap();
    }
    assert_eq!(store.library_targets(1).unwrap()[0].target, target(Some(1)));
    assert!(store.library_targets(0).is_err());
    assert!(store.library_targets(1001).is_err());
    assert!(store.library_target(&target(None)).unwrap().is_none());
    assert!(store.library_target(&target(Some(3))).unwrap().is_none());
    let mut other = target(Some(1));
    other.media_id = MediaId::new("another-series").unwrap();
    assert!(store.library_target(&other).unwrap().is_none());
    let mut invalid = target(Some(1));
    invalid.episode.as_mut().unwrap().episode = 0;
    assert!(store.library_target(&invalid).is_err());
}

#[test]
fn browser_history_is_exact_per_episode_and_prelaunch_links_are_not_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    store.set_favorite(&target(None).media_id, true).unwrap();
    let first = source(7);
    let second = source(9);
    let id = first.source_id().unwrap();
    store.save_source(&id, &second).unwrap();
    store.link_source(&target(Some(2)), &id).unwrap();
    store
        .record_opened_target(&target(Some(2)), &id, 200)
        .unwrap();
    store.save_source(&id, &first).unwrap();
    store.link_source(&target(Some(1)), &id).unwrap();
    // Older than the media-wide event, but the first real open of episode 1.
    store
        .record_opened_target(&target(Some(1)), &id, 100)
        .unwrap();
    store.save_source(&id, &source(11)).unwrap();
    store.link_source(&target(Some(3)), &id).unwrap();
    // Launching episode 3 failed: no record_opened_target call.
    // A new prelaunch selection on episode 1 must not rewrite its real history.
    store.link_source(&target(Some(1)), &id).unwrap();
    drop(store);
    let mut store = Storage::open(&path).unwrap();
    let rows = store.library_targets(100).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].target, target(Some(2)));
    assert_eq!(rows[1].target, target(Some(1)));
    assert_eq!(rows[2].target, target(Some(3)));
    assert!(rows.iter().all(|item| item.progress.is_none()));
    assert_eq!(
        rows.iter().filter(|item| item.opened_at.is_some()).count(),
        2
    );
    assert_eq!(rows[0].opened_at, Some(200));
    assert_eq!(rows[0].source, Some(second.clone()));
    assert_eq!(rows[1].opened_at, Some(100));
    assert_eq!(rows[1].source, Some(first));
    assert_eq!(rows[2].opened_at, None);
    assert_eq!(
        rows[1].entry.last_opened_at,
        Some(200),
        "aggregate is not target history"
    );
    assert!(store.library_target(&target(None)).unwrap().is_none());
    let summary = store.library(10).unwrap().remove(0);
    assert_eq!(summary.target, target(Some(2)));
    assert_eq!(summary.opened_at, Some(200));
    store.clear_playback_data().unwrap();
    let rows = store.library_targets(100).unwrap();
    assert_eq!(rows.len(), 3, "favorite source links remain");
    assert!(
        rows.iter()
            .all(|item| item.opened_at.is_none() && item.progress.is_none())
    );
    assert!(store.history(10).unwrap().is_empty());
    assert_eq!(
        store.source_for_target(&target(Some(2))).unwrap(),
        Some(second)
    );
}

#[test]
fn target_open_staleness_validation_and_progress_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Storage::open(&dir.path().join("db")).unwrap();
    let original = source(7);
    let id = original.source_id().unwrap();
    store.save_source(&id, &original).unwrap();
    store.link_source(&target(Some(1)), &id).unwrap();
    store
        .record_opened_target(&target(Some(1)), &id, 100)
        .unwrap();
    store.save_source(&id, &source(9)).unwrap();
    store.link_source(&target(Some(1)), &id).unwrap();
    store
        .record_opened_target(&target(Some(1)), &id, 99)
        .unwrap();
    let item = store.library_target(&target(Some(1))).unwrap().unwrap();
    assert_eq!(item.opened_at, Some(100));
    assert_eq!(item.source, Some(original.clone()));
    assert!(
        store
            .record_opened_target(&target(Some(1)), &id, -1)
            .is_err()
    );
    let mut invalid = target(Some(1));
    invalid.episode.as_mut().unwrap().episode = 0;
    assert!(store.record_opened_target(&invalid, &id, 101).is_err());
    // Measurement is not an open event and cannot give a different episode
    // the first episode's history timestamp.
    store
        .save_progress_for_source(&observation(2, &original, 300), &original)
        .unwrap();
    let measured = store.library_target(&target(Some(2))).unwrap().unwrap();
    assert_eq!(measured.opened_at, None);
    let summary = store.library(10).unwrap().remove(0);
    assert_eq!(summary.target, measured.target);
    assert_eq!(summary.opened_at, None);
    // An actual open snapshot may differ from the old player's progress; the
    // resume descriptor must still match the measured file.
    store
        .record_opened_target(&target(Some(2)), &id, 301)
        .unwrap();
    let item = store.library_target(&target(Some(2))).unwrap().unwrap();
    assert_eq!(item.opened_at, Some(301));
    assert_eq!(item.source, Some(original));
}

#[test]
fn target_history_write_failure_rolls_back_the_media_history_update() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Storage::open(&path).unwrap();
    let descriptor = source(7);
    let id = descriptor.source_id().unwrap();
    store.save_source(&id, &descriptor).unwrap();
    store
        .record_opened_target(&target(Some(1)), &id, 100)
        .unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER reject_target_history BEFORE INSERT ON target_history
         BEGIN SELECT RAISE(ABORT, 'sensitive-input-must-not-leak'); END;",
    )
    .unwrap();
    let error = store
        .record_opened_target(&target(Some(2)), &id, 200)
        .unwrap_err();
    assert!(!format!("{error} {error:?}").contains("sensitive-input"));
    assert_eq!(
        store
            .library_entry(&target(None).media_id)
            .unwrap()
            .unwrap()
            .last_opened_at,
        Some(100)
    );
    assert!(store.library_target(&target(Some(2))).unwrap().is_none());
}
