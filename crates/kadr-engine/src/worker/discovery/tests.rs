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
        movie_prefetch: None,
        rank_preferences: RankPreferences::default(),
        playback_request: None,
        current: Some(SearchState {
            query,
            key: "fixture".into(),
            pending,
            succeeded: 1,
            failures: BTreeMap::new(),
            notices: Vec::new(),
            batches: sources,
            handles: BTreeMap::new(),
            ranked: Vec::new(),
            names: BTreeMap::new(),
            tracker_magnets: BTreeMap::new(),
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
fn zero_unknown_http_and_manual_offers_are_not_recommendations_but_exact_episodes_are() {
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
    assert_eq!(
        discovery.recommended_handle(),
        Some(discovery.rows()[0].index)
    );
    assert!(discovery.rows()[0].recommended);
}

#[test]
fn requested_quality_and_language_choose_a_suitable_available_offer_not_the_seed_winner() {
    let mut popular = source(1, Some(100));
    popular.resolution = Some(2160);
    popular.languages = vec!["en".into()];
    let mut suitable = source(2, Some(1));
    suitable.resolution = Some(1080);
    suitable.languages = vec!["ru".into()];
    let mut unavailable = source(3, Some(0));
    unavailable.resolution = Some(1080);
    unavailable.languages = vec!["ru".into()];
    let mut discovery = discovery(query(), vec![popular, suitable, unavailable], 0);
    let request = crate::PlaybackPreferences {
        preferred_resolution: Some("1080p".into()),
        preferred_language: Some("RU".into()),
        subtitles_off: true,
    };
    let handle = discovery.recommendation(&request, |_| true).unwrap();
    let rows = discovery.rows();
    assert_eq!(rows[0].seeds, Some(100), "display order remains seed-first");
    assert_ne!(handle, rows[0].index);
    assert_eq!(handle, rows[1].index);
    discovery.playback_request = Some(request);
    assert_eq!(discovery.recommended_handle(), Some(handle));
    assert!(discovery.rows()[1].recommended);
}

#[test]
fn recommendation_inherits_stored_preferences_but_explicit_request_overrides_them() {
    let mut high = source(1, Some(100));
    high.resolution = Some(2160);
    high.languages = vec!["en".into()];
    let mut low = source(2, Some(1));
    low.resolution = Some(720);
    low.languages = vec!["ru".into()];
    let mut discovery = discovery(query(), vec![high, low], 0);
    discovery.rank(&crate::SourcePreferences {
        preferred_resolution: Some("720p".into()),
        preferred_language: Some("ru".into()),
        ..Default::default()
    });
    let rows = discovery.rows();
    assert_eq!(discovery.recommended_handle(), Some(rows[1].index));
    let explicit = crate::PlaybackPreferences {
        preferred_resolution: Some("2160p".into()),
        preferred_language: Some("en".into()),
        ..Default::default()
    };
    assert_eq!(
        discovery.recommendation(&explicit, |_| true),
        Some(rows[0].index)
    );
    assert_eq!(discovery.recommendation(&explicit, |_| false), None);
}

#[test]
fn regional_language_request_matches_a_base_language_source_before_a_foreign_one() {
    let mut foreign = source(1, Some(100));
    foreign.languages = vec!["ru".into()];
    let mut matching = source(2, Some(1));
    matching.languages = vec!["en".into()];
    let discovery = discovery(query(), vec![foreign, matching], 0);
    let request = crate::PlaybackPreferences {
        preferred_language: Some("EN-us".into()),
        ..Default::default()
    };
    assert_eq!(
        discovery.recommendation(&request, |_| true),
        Some(discovery.rows()[1].index)
    );
}

#[test]
fn episode_recommendations_reject_mismatched_missing_and_possible_scope() {
    let mut query = query();
    query.kind = MediaKind::Series;
    query.episode = kadr_core::torrent::EpisodeNumber::new(2, 3);
    let mut exact = source(1, Some(1));
    exact.kind = MediaKind::Series;
    exact.episode = query.episode;
    let mut missing = exact.clone();
    missing.locator = Locator::TorrentUrl("https://example.org/missing".into());
    missing.episode = None;
    missing.seeds = Some(100);
    let mut other = exact.clone();
    other.locator = Locator::TorrentUrl("https://example.org/other".into());
    other.episode = kadr_core::torrent::EpisodeNumber::new(2, 4);
    let mut possible = exact.clone();
    possible.locator = Locator::TorrentUrl("https://example.org/possible".into());
    possible.possible_match = true;
    let discovery = discovery(query, vec![missing, other, possible, exact], 0);
    let winner = discovery.recommended_handle().unwrap();
    let source = &discovery
        .current
        .as_ref()
        .unwrap()
        .ranked
        .iter()
        .find(|(handle, _)| *handle == winner)
        .unwrap()
        .1;
    assert_eq!(source.seeds, Some(1));
    assert_eq!(
        source.locator,
        Locator::TorrentUrl("https://example.org/download?id=1".into())
    );
}

#[test]
fn exact_rutracker_episode_is_rechecked_without_fabricating_provider_episode_metadata() {
    let mut query = query();
    query.kind = MediaKind::Series;
    query.episode = kadr_core::torrent::EpisodeNumber::new(2, 3);
    let mut exact = MediaSource::new(
        "rutracker",
        Locator::RuTracker(42),
        "The Thing S02E03 [1982]",
        MediaKind::Series,
    )
    .unwrap();
    exact.seeds = Some(1);
    let mut pack = MediaSource::new(
        "rutracker",
        Locator::RuTracker(43),
        "The Thing S02 [1982]",
        MediaKind::Series,
    )
    .unwrap();
    pack.seeds = Some(100);
    let rows = matched(&query, "rutracker", vec![pack, exact]);
    let discovery = discovery(query, rows, 0);
    let handle = discovery.recommended_handle().unwrap();
    assert_eq!(
        discovery
            .current
            .as_ref()
            .unwrap()
            .ranked
            .iter()
            .find(|(candidate, _)| *candidate == handle)
            .unwrap()
            .1
            .locator,
        Locator::RuTracker(42)
    );
}

#[test]
fn only_reviewed_commons_http_identity_is_eligible_without_media_probes() {
    let mut query = query();
    query.title = "Big Buck Bunny".into();
    query.year = Some(2008);
    query.external = ExternalMetadata {
        imdb_id: Some("tt1254207".into()),
        tmdb_id: Some(10378),
        alternative_titles: Vec::new(),
    };
    let mut trusted = MediaSource::new(
        kadr_catalog::commons::ID,
        Locator::Http("https://upload.wikimedia.org/wikipedia/commons/transcoded/c/c0/Big_Buck_Bunny_4K.webm/Big_Buck_Bunny_4K.webm.1080p.vp9.webm".into()),
        query.title.clone(),
        MediaKind::Movie,
    )
    .unwrap();
    trusted.year = query.year;
    trusted.external = query.external.clone();
    trusted.provider_item_id = Some("40108642:1080p.vp9.webm".into());
    trusted.resolution = Some(1080);
    trusted.video_codec = Some("VP9".into());
    trusted.audio_codec = Some("Opus".into());
    trusted.license = Some("(c) copyright Blender Foundation | www.bigbuckbunny.org — Big Buck Bunny; https://commons.wikimedia.org/wiki/File:Big_Buck_Bunny_4K.webm — CC BY 3.0 https://creativecommons.org/licenses/by/3.0".into());
    let trusted = trusted.normalize().unwrap();
    assert!(kadr_catalog::commons::accepts(&trusted));
    let mut forged_url = trusted.clone();
    forged_url.locator = Locator::Http("https://example.org/video.webm".into());
    let mut forged_license = trusted.clone();
    forged_license.license = Some("CC".into());
    let mut untrusted_provider = trusted.clone();
    untrusted_provider.provider = "torznab:fixture".into();
    for forged in [forged_url, forged_license, untrusted_provider] {
        assert_eq!(
            discovery(query.clone(), vec![forged], 0).recommended_handle(),
            None
        );
    }
    let mut manual = trusted.clone();
    manual.provider = "manual-http".into();
    manual.origins = vec!["manual-http".into()];
    manual.match_score = 100;
    manual.license = None;
    let duplicates = discovery(query.clone(), vec![trusted.clone(), manual], 0);
    assert_eq!(duplicates.rows().len(), 1);
    let preferences = crate::PlaybackPreferences {
        preferred_resolution: Some("1080p".into()),
        ..Default::default()
    };
    assert_eq!(
        duplicates.recommendation(&preferences, kadr_catalog::commons::accepts),
        Some(duplicates.rows()[0].index),
        "dedup display metadata cannot revoke original Commons authority"
    );
    let discovery = discovery(query, vec![trusted], 0);
    let winner = discovery.recommended_handle().unwrap();
    let source = &discovery
        .current
        .as_ref()
        .unwrap()
        .ranked
        .iter()
        .find(|(handle, _)| *handle == winner)
        .unwrap()
        .1;
    assert!(kadr_catalog::commons::accepts(source));
    assert_eq!(
        source.seeds, None,
        "HTTP availability does not invent torrent seeds"
    );
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
        "The Thing 2011 1080p",
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
    let possible = matched(&unknown_year, "rutracker", vec![offer]);
    assert_eq!(possible.len(), 1);
    assert!(possible[0].possible_match);
}

#[test]
fn possible_rutracker_release_is_selectable_but_never_the_watch_recommendation() {
    let query = query();
    let mut possible = MediaSource::new(
        "rutracker",
        Locator::RuTracker(42),
        "The Thing 1982 1080p",
        MediaKind::Movie,
    )
    .unwrap();
    possible.match_score = 100;
    possible.seeds = Some(999);
    let rows = matched(&query, "rutracker", vec![possible]);
    let mut results = discovery(query, rows, 0);
    let possible = &results.rows()[0];
    assert!(possible.possible_match && !possible.recommended);
    assert_eq!(results.recommended_handle(), None);
    let possible_handle = possible.index;
    results
        .current
        .as_mut()
        .unwrap()
        .batches
        .push(source(1, Some(1)));
    results.rank(&crate::SourcePreferences::default());
    let rows = results.rows();
    assert!(rows[0].recommended && !rows[0].possible_match);
    assert_eq!(rows[1].index, possible_handle);
    assert!(!rows[1].recommended);
}

#[test]
fn filtered_rutracker_search_supersedes_a_cached_zero_without_storing_another() {
    let temp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open(&temp.path().join("library.db")).unwrap();
    let mut state = discovery(query(), vec![], 0).current.unwrap();
    let key = state.cache_key("rutracker").into_owned();
    let empty = |had_results| ProviderBatch {
        rows: vec![],
        had_results,
        ..Default::default()
    };
    state
        .accept_live_batch("rutracker", empty(false), &mut storage)
        .unwrap();
    let no_topics = state.notices.pop().unwrap();
    assert_eq!(
        storage
            .source_cache_get(&key, "rutracker", now())
            .unwrap()
            .unwrap(),
        b"[]"
    );
    state
        .accept_live_batch("rutracker", empty(true), &mut storage)
        .unwrap();
    assert_ne!(state.notices.pop().unwrap(), no_topics);
    assert!(
        storage
            .source_cache_get(&key, "rutracker", now())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn foreground_joins_card_lookup_and_persists_only_topic_descriptors() {
    use futures_util::FutureExt;
    let query = query();
    let mut row = MediaSource::new(
        "rutracker",
        Locator::RuTracker(42),
        "The Thing [1982]",
        MediaKind::Movie,
    )
    .unwrap();
    row.seeds = Some(5);
    let (started, wait) = tokio::sync::oneshot::channel();
    let warm = async move {
        wait.await.unwrap();
        Ok(Arc::new(ProviderBatch {
            tracker_magnets: BTreeMap::from([(42, "test-only:private-tracker-data".into())]),
            ..vec![row].into()
        }))
    }
    .boxed()
    .shared();
    let opened = tokio::spawn(warm.clone());
    let provider = RuTrackerProvider {
        account: tracker::Account::new(None),
        vault: Default::default(),
        preferences: RankPreferences::default(),
        warm: Some(warm),
    };
    let cancel = CancellationToken::new();
    let search = provider.search(&query, &cancel);
    started.send(()).unwrap();
    let batch = search.await.unwrap_or_else(|_| panic!("joined prefetch"));
    assert!(opened.await.unwrap().is_ok());
    let mut state = discovery(query, vec![], 0).current.unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open(&temp.path().join("fixture.db")).unwrap();
    state
        .accept_live_batch("rutracker", batch, &mut storage)
        .unwrap();
    assert!(state.tracker_magnets.contains_key(&42));
    let stored = storage
        .source_cache_get(&state.cache_key("rutracker"), "rutracker", now())
        .unwrap()
        .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("private-tracker-data"));
    assert!(matches!(
        serde_json::from_slice::<Vec<MediaSource>>(&stored).unwrap()[0].locator,
        Locator::RuTracker(42)
    ));
}

#[test]
fn cached_rutracker_episode_pack_requires_manual_selection_and_correct_scope() {
    let mut query = query();
    query.kind = MediaKind::Series;
    query.episode = kadr_core::torrent::EpisodeNumber::new(2, 3);
    let offer = |id, title| {
        let mut row =
            MediaSource::new("rutracker", Locator::RuTracker(id), title, query.kind).unwrap();
        row.match_score = 100;
        row
    };
    let cached = serde_json::to_vec(&[
        offer(1, "The Thing S01 [1982]"),
        offer(2, "The Thing S02 [1982]"),
        offer(3, "The Thing S02E03 [1982]"),
    ])
    .unwrap();
    let rows = matched(
        &query,
        "rutracker",
        serde_json::from_slice(&cached).unwrap(),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].locator, Locator::RuTracker(2));
    assert!(rows[0].possible_match);
    assert_eq!(rows[1].locator, Locator::RuTracker(3));
    assert!(!rows[1].possible_match);
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
fn popcorn_rows_keep_upstream_and_keep_magnets_out_of_storage_and_ui() {
    use kadr_catalog::popcorn::ID;
    let mut movie = query();
    movie.external.imdb_id = Some("tt0084787".into());
    let mut row = MediaSource::new(
        ID,
        Locator::Magnet(format!(
            "magnet:?xt=urn:btih:{}&dn=never-display-or-persist",
            "a".repeat(40)
        )),
        movie.title.clone(),
        MediaKind::Movie,
    )
    .unwrap();
    row.release_title = Some("The Thing [1982] 720p".into());
    row.external.imdb_id = movie.external.imdb_id.clone();
    row.year = movie.year;
    row.origins
        .extend(["popcorn:Yts".into(), "rutracker".into()]);
    let mut discovery = discovery(movie, vec![], 0);
    let temp = tempfile::tempdir().unwrap();
    let mut storage = Storage::open(&temp.path().join("fixture.db")).unwrap();
    let state = discovery.current.as_mut().unwrap();
    state.names.insert(ID.into(), "Popcorn API".into());
    state.names.insert("rutracker".into(), "RuTracker".into());
    state
        .accept_live_batch(ID, vec![row].into(), &mut storage)
        .unwrap();
    assert!(!allows_disk_cache(ID));
    assert!(
        storage
            .source_cache_get(&state.cache_key(ID), ID, now())
            .unwrap()
            .is_none()
    );
    discovery.rank(&crate::SourcePreferences::default());
    let rows = discovery.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, SourceKind::Magnet);
    assert_eq!(rows[0].origin, "Popcorn API · Yts");
    assert!(!rows[0].rutracker);
    assert_eq!(rows[0].title, "The Thing [1982] 720p");
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
