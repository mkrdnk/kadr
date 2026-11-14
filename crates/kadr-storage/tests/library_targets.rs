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
