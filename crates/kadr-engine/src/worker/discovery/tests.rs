use super::*;
use kadr_core::catalog::ExternalMetadata;

fn query() -> MediaQuery {
    MediaQuery {
        media_id: "trakt:movie:42".into(),
        kind: MediaKind::Movie,
        title: "The Thing".into(),
        original_title: None,
        year: Some(1982),
        external: ExternalMetadata::default(),
        episode: None,
    }
}

fn source(id: u64, seeds: Option<u32>) -> MediaSource {
    let mut source = MediaSource::new(
        "torznab:fixture",
        Locator::TorrentUrl(format!("https://example.org/download?id={id}")),
        "The Thing",
        MediaKind::Movie,
    )
    .unwrap();
    source.year = Some(1982);
    source.seeds = seeds;
    source
}

fn discovery(query: MediaQuery, sources: Vec<MediaSource>, pending: usize) -> Discovery {
    let mut discovery = Discovery {
        next_handle: 0,
        current: Some(SearchState {
            query,
            key: "fixture".into(),
            pending,
            succeeded: 1,
            failures: BTreeMap::new(),
            batches: sources,
            handles: BTreeMap::new(),
            ranked: Vec::new(),
            names: BTreeMap::new(),
        }),
    };
    discovery.rank(&crate::SourcePreferences::default());
    discovery
}

#[test]
fn recommendation_waits_for_all_providers_and_uses_the_final_seed_order() {
    let mut initial = source(1, Some(3));
    initial.match_score = 100;
    initial.resolution = Some(2160);
    let mut discovery = discovery(query(), vec![initial], 1);
    let original_handle = discovery.rows()[0].index;
    assert_eq!(discovery.recommended_handle(), None);
    assert!(discovery.rows().iter().all(|row| !row.recommended));
    let mut winner = source(2, Some(50));
    winner.match_score = 80;
    winner.resolution = Some(720);
    let state = discovery.current.as_mut().unwrap();
    state
        .batches
        .extend([source(3, None), source(4, Some(0)), winner.clone(), winner]);
    state.pending = 0;
    discovery.rank(&crate::SourcePreferences::default());
    let rows = discovery.rows();
    assert_eq!(
        rows.iter().map(|row| row.seeds).collect::<Vec<_>>(),
        [Some(50), Some(3), Some(0), None]
    );
    assert_eq!(rows.iter().filter(|row| row.recommended).count(), 1);
    assert!(rows[0].recommended);
    assert_eq!(discovery.recommended_handle(), Some(rows[0].index));
    assert_eq!(rows[1].index, original_handle);
    let Event::SourceDiscovery {
        rows: snapshot,
        pending,
        ..
    } = discovery.snapshot(&CancellationToken::new()).unwrap()
    else {
        panic!("discovery snapshot");
    };
    assert_eq!(pending, 0);
    assert_eq!(snapshot, rows);
}

#[test]
fn zero_unknown_http_manual_and_episodic_offers_are_not_film_recommendations() {
    let mut http = source(1, Some(100));
    http.locator = Locator::Http("https://example.org/video.mp4".into());
    let mut manual = source(2, Some(100));
    manual.provider = "manual-torrent".into();
    let sources = vec![http, manual, source(3, Some(0)), source(4, None)];
    let films = discovery(query(), sources, 0);
    assert_eq!(films.rows().len(), 4);
    assert_eq!(films.recommended_handle(), None);
    assert!(films.rows().iter().all(|row| !row.recommended));
    let mut episode_query = query();
    episode_query.kind = MediaKind::Series;
    episode_query.episode = kadr_core::torrent::EpisodeNumber::new(1, 1);
    let mut episode = source(5, Some(10));
    episode.kind = MediaKind::Series;
    episode.episode = episode_query.episode;
    let discovery = discovery(episode_query, vec![episode], 0);
    assert_eq!(discovery.rows().len(), 1);
    assert_eq!(discovery.recommended_handle(), None);
    assert!(!discovery.rows()[0].recommended);
}

#[test]
fn cached_rutracker_movie_scores_cannot_bypass_title_year_or_scope_evidence() {
    let query = query();
    for title in [
        "The Thing [2011, USA]",
        "The Thing [1982-2011]",
        "The Thing [1982] S01E01",
        "The Thing [1982] Season 1",
        "The Thing [1982] Collection",
        "The Thing [1982] 1x01",
        "The Thing Returns [1982]",
        "The Thing 1982 1080p",
    ] {
        let mut offer =
            MediaSource::new("rutracker", Locator::RuTracker(42), title, MediaKind::Movie).unwrap();
        offer.match_score = 100;
        offer.year = query.year;
        offer.external = query.external.clone();
        assert!(
            matched(&query, "rutracker", vec![offer]).is_empty(),
            "{title}"
        );
    }
    let mut offer = MediaSource::new(
        "rutracker",
        Locator::RuTracker(42),
        "The Thing [1982, USA] 1080p",
        MediaKind::Movie,
    )
    .unwrap();
    offer.match_score = 100;
    offer.seeds = Some(100);
    let rows = matched(&query, "rutracker", vec![offer.clone()]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].match_score, 80);
    let mut unknown_year = query;
    unknown_year.year = None;
    assert!(matched(&unknown_year, "rutracker", vec![offer]).is_empty());
}

#[test]
fn cached_generic_movie_with_matching_id_and_conflicting_year_is_excluded() {
    let mut query = query();
    query.external.imdb_id = Some("tt0084787".into());
    let mut offer = source(1, Some(1000));
    offer.external = query.external.clone();
    offer.year = Some(2011);
    assert!(matched(&query, "torznab:fixture", vec![offer]).is_empty());
}

#[test]
fn manual_duplicate_does_not_hide_the_most_popular_discovered_release() {
    let mut active = source(1, Some(27));
    active.info_hash = Some("a".repeat(40));
    let mut manual = active.clone();
    manual.provider = "manual-torrent".into();
    manual.locator = Locator::Magnet(format!("magnet:?xt=urn:btih:{}", "a".repeat(40)));
    manual.match_score = 100;
    manual.seeds = None;
    let discovery = discovery(query(), vec![manual, source(2, Some(2)), active], 0);
    let rows = discovery.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].seeds, Some(27));
    assert!(rows[0].recommended);
    assert!(!rows[1].recommended);
    assert_eq!(discovery.recommended_handle(), Some(rows[0].index));
    assert!(
        discovery.current.as_ref().unwrap().ranked[0]
            .1
            .origins
            .contains(&"manual-torrent".into())
    );
}

#[test]
fn restored_torznab_movies_recheck_release_scope_and_raw_year_even_with_ids() {
    let mut query = query();
    query.external.imdb_id = Some("tt0084787".into());
    let provider = torznab::Provider::new(
        &crate::TorznabConfig {
            name: "Owned fixture".into(),
            base_url: "https://example.org/api".into(),
            api_key_env: None,
            enabled: true,
        },
        0,
        None,
    );
    let mut offer = source(1, Some(1000));
    offer.provider = provider.id().to_owned();
    offer.external = query.external.clone();
    offer.match_score = 100;
    offer.release_title = Some("The Thing 1982 720p".into());
    assert!(provider.accepts(&offer));
    for release in [
        "The Thing 2011 1080p",
        "The Thing 1982 Collection",
        "The Thing 1982 Complete Films",
        "The Thing 1982 S01E01",
    ] {
        offer.release_title = Some(release.into());
        assert!(!provider.accepts(&offer), "{release}");
    }
    assert!(query_key(&query).starts_with("resolve-v2:"));
}
