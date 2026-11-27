//! Explicit live acceptance. Never run these against Wikimedia/Trakt in the ordinary suite.
use super::*;

const TITLE: &str = "Big Buck Bunny";
const NETWORK_WAIT: Duration = Duration::from_secs(45);

fn resolve_and_play(app: &mut App, db: &Storage, id: &str) {
    app.command(Command::FindSources);
    let Event::SourceDiscovery {
        rows,
        failures,
        succeeded,
        ..
    } = app.wait_for(
        NETWORK_WAIT,
        "Commons discovery",
        |e| matches!(e, Event::SourceDiscovery { media_id, pending: 0, .. } if media_id == id),
    )
    else {
        unreachable!()
    };
    assert!(failures.is_empty(), "Commons search failed: {failures:?}");
    assert_eq!(succeeded, 1, "no indexer should be needed");
    let row = rows
        .iter()
        .filter(|row| row.origin == "Wikimedia Commons")
        .min_by_key(|row| {
            row.quality
                .as_deref()
                .and_then(|s| s.strip_suffix('p')?.parse::<u32>().ok())
                .unwrap_or(u32::MAX)
        })
        .expect("the live API must offer a reviewed rendition");
    assert!(row.license.contains("creativecommons.org") && row.license.contains("Blender"));
    app.command(Command::ChooseSource(row.index));
    app.selected();
    app.command(Command::Play { resume: false });
    app.wait_for(NETWORK_WAIT, "real mpv opened the Wikimedia film", |e| {
        matches!(e, Event::PlaybackActive(true))
    });
    let target = PlaybackTarget {
        media_id: MediaId::new(id).unwrap(),
        episode: None,
    };
    app.until(NETWORK_WAIT, "measured playback of the full film", || {
        db.load_progress(&target).unwrap().is_some_and(|p| {
            p.origin == ProgressOrigin::Mpv
                && p.position_seconds >= 1.0
                && p.duration_seconds
                    .is_some_and(|s| (590.0..650.0).contains(&s))
        })
    });
    app.stopped();
    app.finish();
}

#[test]
#[ignore = "requires live Wikimedia and mpv; begins at a saved movie, not a live Trakt catalog"]
fn live_commons_from_saved_card_plays_in_mpv() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut db = Storage::open_default(&paths).unwrap();
    // Intentionally a fixture ID, not a claim to have obtained metadata from Trakt.
    let id = MediaId::new("trakt:movie:1").unwrap();
    db.upsert_media(&kadr_storage::MediaRecord {
        id: id.clone(),
        title: TITLE.into(),
        original_title: None,
        year: Some(2008),
        kind: kadr_storage::MediaKind::Movie,
        description: String::new(),
        poster_url: None,
        attribution: "Local acceptance snapshot".into(),
    })
    .unwrap();
    db.set_favorite(&id, true).unwrap();
    let mut app = App::start(&paths);
    assert!(!app.ready().network.torrents_enabled);
    app.command(Command::OpenLibrary(id.as_str().into()));
    app.wait(
        "saved movie card",
        |e| matches!(e, Event::MediaSelected { media: Some(m), .. } if m.title == TITLE),
    );
    resolve_and_play(&mut app, &db, id.as_str());
}

#[test]
#[ignore = "clean-install live Trakt + Wikimedia + mpv; requires a configured Trakt client ID"]
fn live_clean_install_trakt_commons_playback() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut app = App::start(&paths);
    assert!(!app.ready().network.torrents_enabled);
    app.command(Command::Search(TITLE.into()));
    let Event::SearchState(result) = app.wait_for(NETWORK_WAIT, "live Trakt search", |e| {
        matches!(
            e,
            Event::SearchState(AsyncState::Ready(_) | AsyncState::Error(_))
        )
    }) else {
        unreachable!()
    };
    let rows = match result {
        AsyncState::Ready(rows) => rows,
        AsyncState::Error(message) => panic!("Trakt prerequisite: {message}"),
        AsyncState::Loading => unreachable!(),
    };
    let card = rows
        .iter()
        .find(|m| m.external.imdb_id.as_deref() == Some("tt1254207"))
        .expect("Trakt must return the reviewed film");
    app.command(Command::OpenMedia(card.id.clone()));
    app.wait_for(
        NETWORK_WAIT,
        "live Trakt details",
        |e| matches!(e, Event::MediaSelected { media: Some(m), .. } if m.id == card.id),
    );
    let db = Storage::open(&paths.database_path()).unwrap();
    resolve_and_play(&mut app, &db, &card.id);
}
