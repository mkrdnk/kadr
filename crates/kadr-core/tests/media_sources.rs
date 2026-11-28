use kadr_core::{
    catalog::{Episode, ExternalMetadata, Media, MediaKind},
    source::ManualSource,
    sources::{
        Locator, MediaQuery, MediaSource, SourceError, SourcePreferences, match_score,
        match_source, matches_query, movie_release_has_pack_suffix, rank_and_dedup, select_sources,
    },
    torrent::EpisodeNumber,
};

const V1: &str = "0123456789abcdef0123456789abcdef01234567";
const V2: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn media() -> Media {
    Media {
        id: "trakt:movie:42".into(),
        title: "The Movie".into(),
        original_title: Some("Original Movie".into()),
        year: Some(2021),
        external: ExternalMetadata::default(),
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
        attribution: String::new(),
        license: None,
        source_page: String::new(),
    }
}

fn query() -> MediaQuery {
    MediaQuery::from_media(&media(), None).unwrap()
}

fn source(topic: u64) -> MediaSource {
    let mut source = MediaSource::new(
        "provider",
        Locator::RuTracker(topic),
        "The Movie",
        MediaKind::Movie,
    )
    .unwrap();
    source.year = Some(2021);
    source
}

fn magnet(hash: &str) -> Locator {
    Locator::Magnet(format!("magnet:?xt=urn:btih:{hash}"))
}

#[test]
fn queries_require_canonical_trakt_identity_kind_and_concrete_episode() {
    let mut media = media();
    assert!(MediaQuery::from_media(&media, None).is_some());
    let episode = Episode {
        season: 0,
        number: 1,
        title: String::new(),
        description: String::new(),
        runtime_minutes: None,
        screenshot_url: None,
    };
    assert!(MediaQuery::from_media(&media, Some(&episode)).is_none());
    for id in [
        "old:movie:1",
        "trakt:movie:0",
        "trakt:movie:01",
        "trakt:movie:+1",
        "trakt:movie: 1",
        "trakt:movie:1:2",
        "trakt:movie:18446744073709551616",
        "trakt:movie:",
        "trakt:show:42",
    ] {
        media.id = id.into();
        assert!(MediaQuery::from_media(&media, None).is_none(), "{id}");
    }
    media.id = "trakt:show:42".into();
    media.kind = MediaKind::Series;
    assert!(MediaQuery::from_media(&media, None).is_none());
    assert_eq!(
        MediaQuery::from_media(&media, Some(&episode))
            .unwrap()
            .episode,
        EpisodeNumber::new(0, 1)
    );
    let invalid = Episode {
        number: 0,
        ..episode
    };
    assert!(MediaQuery::from_media(&media, Some(&invalid)).is_none());
    media.id = "trakt:movie:42".into();
    assert!(MediaQuery::from_media(&media, Some(&invalid)).is_none());
}

#[test]
fn locators_share_strict_manual_validation_and_keep_remote_torrent_role() {
    let url = "https://EXAMPLE.org:443/file?signature=secret#fragment";
    assert_eq!(
        Locator::Http(url.into()).validate().unwrap(),
        Locator::Http("https://example.org/file?signature=secret".into())
    );
    assert_eq!(
        Locator::TorrentUrl(url.into()).validate().unwrap(),
        Locator::TorrentUrl("https://example.org/file?signature=secret".into())
    );
    assert_eq!(
        Locator::RuTracker(0).validate(),
        Err(SourceError::InvalidLocator)
    );
    for locator in [
        Locator::Http("https://user:secret@example.org/a".into()),
        Locator::Http("https:example.org/a".into()),
        Locator::Http(" https://example.org/a".into()),
        Locator::Http("https://example.org/a%XX".into()),
        Locator::Http("https://example.org/a\\b".into()),
        Locator::Http(format!("magnet:?xt=urn:btih:{V1}")),
        Locator::Magnet("https://example.org/file".into()),
        Locator::Magnet("magnet:?dn=no-hash".into()),
        Locator::TorrentUrl("file:///tmp/file.torrent".into()),
        Locator::TorrentUrl("/tmp/file.torrent".into()),
    ] {
        assert_eq!(locator.validate(), Err(SourceError::InvalidLocator));
    }
    let direct =
        MediaSource::new("p", Locator::Http(url.into()), "Movie", MediaKind::Movie).unwrap();
    let torrent = MediaSource::new(
        "p",
        Locator::TorrentUrl(url.into()),
        "Movie",
        MediaKind::Movie,
    )
    .unwrap();
    assert_ne!(direct.id, torrent.id);
    assert_eq!(
        direct.id,
        ManualSource::parse_link(url).unwrap().id().as_str()
    );
}

#[test]
fn missing_metadata_is_not_fabricated_and_known_zero_is_preserved() {
    let mut offer = MediaSource::new(
        " p ",
        Locator::RuTracker(1),
        " Movie  1080p RU ",
        MediaKind::Movie,
    )
    .unwrap();
    assert_eq!(offer.title, "Movie 1080p RU");
    assert_eq!(offer.quality, None);
    assert_eq!(offer.resolution, None);
    assert_eq!(offer.languages, Vec::<String>::new());
    assert_eq!(offer.size, None);
    assert_eq!(offer.seeds, None);
    assert_eq!(offer.peers, None);
    assert_eq!(offer.video_codec, None);
    assert_eq!(offer.audio_codec, None);
    offer.size = Some(0);
    offer.seeds = Some(0);
    offer.peers = Some(0);
    offer.languages = vec!["RU".into(), "en-US".into(), "ru".into()];
    offer.origins = vec![" Page ".into(), "Page".into()];
    offer.fetched_at = 123;
    let offer = offer.normalize().unwrap();
    assert_eq!(offer.languages, ["en-us", "ru"]);
    assert_eq!(offer.origins, ["Page", "p"]);
    assert_eq!(
        (offer.size, offer.seeds, offer.peers),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(offer.fetched_at, 123);
    assert_eq!(offer.clone().normalize().unwrap(), offer);
}

#[test]
fn invalid_metadata_and_hash_conflicts_are_rejected_without_input_in_errors() {
    let mut offer = source(1);
    offer.locator = magnet(V1);
    offer.info_hash = Some("ffffffffffffffffffffffffffffffffffffffff".into());
    assert_eq!(offer.normalize(), Err(SourceError::ConflictingInfoHashes));
    let mut offer = source(1);
    offer.info_hash = Some("private-broken-hash".into());
    let error = offer.normalize().unwrap_err();
    assert_eq!(error, SourceError::InvalidInfoHash);
    assert!(!error.to_string().contains("private"));
    let mut offer = source(1);
    offer.resolution = Some(0);
    assert_eq!(offer.normalize(), Err(SourceError::InvalidMetadata));
    let mut offer = source(1);
    offer.provider = "private\nprovider".into();
    assert_eq!(offer.normalize(), Err(SourceError::InvalidMetadata));
    let mut offer = source(1);
    offer.languages = vec!["ru/RU".into()];
    assert_eq!(offer.normalize(), Err(SourceError::InvalidMetadata));
    let mut offer = source(1);
    offer.episode = EpisodeNumber::new(1, 2);
    assert_eq!(offer.normalize(), Err(SourceError::InvalidMetadata));
    let mut offer = source(1);
    offer.match_score = 101;
    assert_eq!(offer.normalize(), Err(SourceError::InvalidMetadata));
}

#[test]
fn extra_hash_version_does_not_rewrite_the_providers_locator() {
    let mut offer = source(1);
    offer.locator = magnet(V1);
    offer.info_hash = Some(V2.to_uppercase());
    let normalized = offer.normalize().unwrap();
    assert_eq!(normalized.locator, magnet(V1));
    assert_eq!(normalized.info_hash.as_deref(), Some(V2));
    assert_eq!(
        normalized.id,
        ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{V1}"))
            .unwrap()
            .id()
            .as_str()
    );
    assert_eq!(normalized.clone().normalize().unwrap(), normalized);
    let mut v2 = source(2);
    v2.locator = Locator::Magnet(format!("magnet:?xt=urn:btmh:1220{V2}"));
    assert_eq!(
        rank_and_dedup(vec![normalized, v2], &SourcePreferences::default()).len(),
        1
    );
}

#[test]
fn movies_need_full_title_and_known_exact_year_without_matching_ids() {
    let query = query();
    let mut offer = source(1);
    assert_eq!(match_score(&query, &offer), Some(90));
    for title in [
        "Movie",
        "The Movie Returns",
        "The Movie 2021 1080p",
        "Another Movie",
    ] {
        offer.title = title.into();
        assert!(!matches_query(&query, &offer));
    }
    offer.title = "THE.MOVIE".into();
    assert!(matches_query(&query, &offer));
    offer.year = None;
    assert!(!matches_query(&query, &offer));
    offer.year = Some(2020);
    assert!(!matches_query(&query, &offer));
    offer.year = Some(2021);
    let mut unknown_year = query.clone();
    unknown_year.year = None;
    assert!(!matches_query(&unknown_year, &offer));
    offer.release_title = Some("The Movie".into());
    offer.title = "Different".into();
    assert!(!matches_query(&query, &offer));
}

#[test]
fn original_and_alternative_titles_are_exact_and_have_distinct_accuracy() {
    let mut query = query();
    let mut offer = source(1);
    offer.title = "Original Movie".into();
    assert_eq!(match_score(&query, &offer), Some(90));
    query
        .external
        .alternative_titles
        .push("Другое название".into());
    offer.title = "Другое название".into();
    assert_eq!(match_score(&query, &offer), Some(80));
    offer.title = "Unrelated".into();
    offer.external.alternative_titles.push("The Movie".into());
    assert_eq!(match_score(&query, &offer), Some(80));
    offer.external.alternative_titles = vec!["The".into()];
    assert_eq!(match_score(&query, &offer), None);
}

#[test]
fn external_ids_match_without_text_guessing_but_comparable_conflicts_veto() {
    let mut query = query();
    query.external.imdb_id = Some("tt1234567".into());
    query.external.tmdb_id = Some(123);
    let mut offer = source(1);
    offer.title = "Localized".into();
    offer.year = None;
    offer.external.imdb_id = Some("tt1234567".into());
    assert_eq!(match_score(&query, &offer), Some(100));
    offer.external.tmdb_id = Some(456);
    assert_eq!(match_score(&query, &offer), None);
    offer.external.imdb_id = None;
    offer.external.tmdb_id = Some(123);
    assert_eq!(match_score(&query, &offer), Some(100));
    offer.kind = MediaKind::Series;
    assert_eq!(match_score(&query, &offer), None);
    offer = source(1);
    offer.external.imdb_id = Some("tt7654321".into());
    assert_eq!(match_score(&query, &offer), None);
    offer.external.imdb_id = Some("malformed".into());
    assert_eq!(match_score(&query, &offer), Some(90));
}

#[test]
fn movie_year_conflicts_veto_even_matching_external_ids() {
    let mut query = query();
    query.external.imdb_id = Some("tt1234567".into());
    query.external.tmdb_id = Some(123);
    let mut offer = source(1);
    offer.year = Some(1982);
    for external in [
        ExternalMetadata {
            imdb_id: Some("tt1234567".into()),
            ..Default::default()
        },
        ExternalMetadata {
            tmdb_id: Some(123),
            ..Default::default()
        },
        query.external.clone(),
    ] {
        offer.external = external;
        assert_eq!(match_score(&query, &offer), None);
        assert!(
            select_sources(&query, vec![offer.clone()], &SourcePreferences::default()).is_empty()
        );
    }
    offer.year = query.year;
    assert_eq!(match_score(&query, &offer), Some(100));
}

#[test]
fn movie_pack_markers_are_only_metadata_suffix_evidence_not_literal_titles() {
    for release in [
        "The Thing [1982, USA] Collection",
        "The Thing 1982 Complete Films",
        "The Thing [Collection] [1982]",
        "The Thing [1982] Trilogy",
        "The Thing [1982] Сборник",
        "The Thing [1982] Все фильмы",
    ] {
        assert!(movie_release_has_pack_suffix(release), "{release}");
    }
    for release in [
        "The Collection [2012] 1080p",
        "Collection [2020] 1080p",
        "Complete Unknown 2016 1080p",
        "The Thing [1982] Complete BluRay",
        "The Thing [1982] Collector's Edition",
    ] {
        assert!(!movie_release_has_pack_suffix(release), "{release}");
    }
}

#[test]
fn exact_episode_is_required_even_with_a_show_id_and_specials_work() {
    let mut media = media();
    media.kind = MediaKind::Series;
    media.id = "trakt:show:42".into();
    media.external.tmdb_id = Some(42);
    let episode = Episode {
        season: 0,
        number: 2,
        title: String::new(),
        description: String::new(),
        runtime_minutes: None,
        screenshot_url: None,
    };
    let query = MediaQuery::from_media(&media, Some(&episode)).unwrap();
    let mut offer = source(1);
    offer.kind = MediaKind::Series;
    offer.external.tmdb_id = Some(42);
    for scope in [None, EpisodeNumber::new(1, 2), EpisodeNumber::new(0, 1)] {
        offer.episode = scope;
        assert!(!matches_query(&query, &offer));
    }
    offer.episode = EpisodeNumber::new(0, 2);
    assert_eq!(match_score(&query, &offer), Some(100));
}

#[test]
fn normalization_recomputes_ids_and_dedups_hash_encodings_and_hybrid_aliases() {
    let base32 = "AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH";
    let mut first = source(1);
    first.provider = "a".into();
    first.locator = magnet(V1);
    first.id = "untrusted".into();
    let mut second = source(2);
    second.provider = "b".into();
    second.locator = magnet(base32);
    let mut hybrid = source(3);
    hybrid.provider = "c".into();
    hybrid.locator = Locator::Magnet(format!("magnet:?xt=urn:btih:{V1}&xt=urn:btmh:1220{V2}"));
    let mut v2 = source(4);
    v2.provider = "d".into();
    v2.locator = Locator::Magnet(format!("magnet:?xt=urn:btmh:1220{V2}"));
    let prefs = SourcePreferences::default();
    let result = rank_and_dedup(
        vec![first.clone(), second.clone(), hybrid.clone(), v2.clone()],
        &prefs,
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].info_hash.as_deref(), Some(V1));
    assert_eq!(
        result[0].id,
        ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{V1}"))
            .unwrap()
            .id()
            .as_str()
    );
    assert!(
        ["a", "b", "c", "d"]
            .iter()
            .all(|origin| result[0].origins.iter().any(|value| value == origin))
    );
    let reverse = rank_and_dedup(vec![v2.clone(), hybrid, second, first], &prefs);
    assert_eq!(result, reverse);
    assert_eq!(rank_and_dedup(result.clone(), &prefs), result);
}

#[test]
fn guid_dedup_keeps_http_and_remote_torrent_roles_separate() {
    let mut http = source(1);
    http.provider_item_id = Some("same-guid".into());
    http.locator = Locator::Http("https://example.org/download".into());
    http.match_score = 100;
    let mut torrent = http.clone();
    torrent.locator = Locator::TorrentUrl("https://example.org/download".into());
    torrent.info_hash = Some(V1.into());
    torrent.match_score = 80;
    let result = rank_and_dedup(vec![http, torrent], &SourcePreferences::default());
    assert_eq!(result.len(), 2);
    assert!(
        result
            .iter()
            .all(|source| source.clone().normalize().is_ok())
    );
    assert_eq!(
        rank_and_dedup(result.clone(), &SourcePreferences::default()),
        result
    );
}

#[test]
fn same_named_series_with_conflicting_years_do_not_match_textually() {
    let mut query = query();
    query.media_id = "trakt:show:42".into();
    query.kind = MediaKind::Series;
    query.year = Some(2001);
    query.episode = EpisodeNumber::new(1, 1);
    let mut offer = source(1);
    offer.kind = MediaKind::Series;
    offer.episode = query.episode;
    offer.year = Some(2005);
    assert_eq!(match_score(&query, &offer), None);
    offer.year = None;
    assert_eq!(match_score(&query, &offer), Some(90));
}

#[test]
fn dedup_keeps_the_complete_best_representative_not_a_hybrid_credential_substitute() {
    let mut accurate = source(1);
    accurate.provider = "accurate".into();
    accurate.match_score = 100;
    accurate.seeds = Some(10);
    accurate.info_hash = Some(V1.into());
    accurate = accurate.normalize().unwrap();
    let mut hybrid = source(2);
    hybrid.provider = "hybrid".into();
    hybrid.locator = Locator::Magnet(format!("magnet:?xt=urn:btih:{V1}&xt=urn:btmh:1220{V2}"));
    hybrid.match_score = 80;
    hybrid.seeds = Some(0);
    hybrid.peers = Some(0);
    let result = rank_and_dedup(
        vec![hybrid, accurate.clone()],
        &SourcePreferences::default(),
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].locator, accurate.locator);
    assert_eq!(result[0].provider, accurate.provider);
    assert_eq!(result[0].match_score, 100);
    assert_eq!(result[0].seeds, Some(10));
    assert_eq!(result[0].info_hash, accurate.info_hash);
    assert!(result[0].origins.iter().any(|origin| origin == "hybrid"));
}

#[test]
fn conflicting_secondary_hashes_never_expose_duplicate_stable_ids() {
    let mut first = source(1);
    first.locator = Locator::Magnet(format!("magnet:?xt=urn:btih:{V1}&xt=urn:btmh:1220{V2}"));
    first.provider = "best".into();
    first.match_score = 100;
    let mut second = source(2);
    second.locator = Locator::Magnet(format!(
        "magnet:?xt=urn:btih:{V1}&xt=urn:btmh:1220{}",
        "b".repeat(64)
    ));
    second.provider = "other".into();
    second.match_score = 80;
    let result = rank_and_dedup(
        vec![second.clone(), first.clone()],
        &SourcePreferences::default(),
    );
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].locator, first.locator);
    assert_eq!(result[0].provider, "best");
    assert_eq!(
        rank_and_dedup(vec![first, second], &SourcePreferences::default()),
        result
    );
}

#[test]
fn provider_guid_is_scoped_stable_and_distinct_http_queries_are_not_conflated() {
    let mut first = source(1);
    first.locator = Locator::TorrentUrl("https://example.org/download?signature=one".into());
    first.provider_item_id = Some("guid".into());
    let first = first.normalize().unwrap();
    let mut second = first.clone();
    second.locator = Locator::TorrentUrl("https://example.org/download?signature=two".into());
    assert_eq!(first.id, second.clone().normalize().unwrap().id);
    assert_eq!(
        rank_and_dedup(
            vec![first.clone(), second.clone()],
            &SourcePreferences::default()
        )
        .len(),
        1
    );
    second.provider = "other-provider".into();
    assert_ne!(first.id, second.clone().normalize().unwrap().id);
    assert_eq!(
        rank_and_dedup(
            vec![first.clone(), second.clone()],
            &SourcePreferences::default()
        )
        .len(),
        2
    );
    let mut first = first;
    first.provider_item_id = None;
    second.provider_item_id = None;
    assert_eq!(
        rank_and_dedup(vec![first, second], &SourcePreferences::default()).len(),
        2
    );
}

#[test]
fn identical_manual_http_offer_keeps_the_discovered_resources_credit() {
    let file = Locator::Http("https://example.org/open.webm".into());
    let mut automatic = MediaSource::new(
        "licensed-provider",
        file.clone(),
        "Open film",
        MediaKind::Movie,
    )
    .unwrap();
    automatic.license = Some("Creator · CC BY 3.0".into());
    let mut manual = MediaSource::new("manual-http", file, "Open film", MediaKind::Movie).unwrap();
    manual.match_score = 100;
    let rows = rank_and_dedup(vec![automatic, manual], &SourcePreferences::default());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider, "manual-http");
    assert_eq!(rows[0].license.as_deref(), Some("Creator · CC BY 3.0"));
}

#[test]
fn different_content_claims_for_one_locator_do_not_bridge_unrelated_hashes() {
    let mut first = source(1);
    first.info_hash = Some(V1.into());
    let mut second = source(1);
    second.info_hash = Some("ffffffffffffffffffffffffffffffffffffffff".into());
    assert_eq!(
        rank_and_dedup(vec![first, second], &SourcePreferences::default()).len(),
        2
    );
}

#[test]
fn episodic_accuracy_and_availability_keep_the_existing_preferences() {
    let mut accurate = source(1);
    accurate.kind = MediaKind::Series;
    accurate.match_score = 100;
    let mut preferred = source(2);
    preferred.kind = MediaKind::Series;
    preferred.match_score = 90;
    preferred.resolution = Some(1080);
    preferred.languages = vec!["ru".into()];
    preferred.seeds = Some(100);
    let prefs = SourcePreferences {
        resolutions: vec![1080, 2160],
        languages: vec!["ru".into(), "en".into()],
    };
    assert_eq!(
        rank_and_dedup(vec![preferred.clone(), accurate], &prefs)[0].match_score,
        100
    );
    let mut unavailable = preferred.clone();
    unavailable.locator = Locator::RuTracker(3);
    unavailable.seeds = Some(0);
    unavailable.peers = Some(0);
    preferred.resolution = Some(720);
    preferred.languages = vec!["en".into()];
    assert_eq!(
        rank_and_dedup(vec![unavailable, preferred.clone()], &prefs)[0].seeds,
        Some(100)
    );
    let mut high = preferred.clone();
    high.locator = Locator::RuTracker(4);
    high.resolution = Some(2160);
    let mut exact = preferred.clone();
    exact.locator = Locator::RuTracker(5);
    exact.resolution = Some(1080);
    exact.languages = vec!["en".into()];
    let mut localized = exact.clone();
    localized.locator = Locator::RuTracker(6);
    localized.languages = vec!["ru".into()];
    let result = rank_and_dedup(vec![preferred, high, exact, localized], &prefs);
    assert_eq!(
        result
            .iter()
            .map(|source| source.locator.clone())
            .collect::<Vec<_>>(),
        [
            Locator::RuTracker(6),
            Locator::RuTracker(5),
            Locator::RuTracker(4),
            Locator::RuTracker(2),
        ]
    );
}

#[test]
fn movie_torrents_rank_and_dedup_by_seeds_before_accuracy_and_preferences() {
    let mut accurate = source(1);
    accurate.match_score = 100;
    accurate.resolution = Some(2160);
    accurate.languages = vec!["ru".into()];
    accurate.seeds = Some(2);
    let mut popular = source(2);
    popular.match_score = 80;
    popular.resolution = Some(720);
    popular.languages = vec!["en".into()];
    popular.seeds = Some(100);
    popular.info_hash = Some(V1.into());
    let mut duplicate = popular.clone();
    duplicate.provider = "duplicate".into();
    duplicate.match_score = 100;
    duplicate.resolution = Some(2160);
    duplicate.seeds = Some(1);
    let mut zero = source(3);
    zero.seeds = Some(0);
    zero.peers = Some(1000);
    let mut unknown = source(4);
    unknown.match_score = 100;
    unknown.peers = Some(10000);
    let prefs = SourcePreferences {
        resolutions: vec![2160],
        languages: vec!["ru".into()],
    };
    let sources = vec![unknown, duplicate, zero, accurate, popular.clone()];
    let expected = rank_and_dedup(sources.clone(), &prefs);
    assert_eq!(
        expected
            .iter()
            .map(|source| source.seeds)
            .collect::<Vec<_>>(),
        [Some(100), Some(2), Some(0), None]
    );
    assert_eq!(expected[0].provider, popular.provider);
    assert_eq!(expected[0].resolution, Some(720));
    assert!(expected[0].origins.contains(&"duplicate".into()));
    let mut reversed = sources;
    reversed.reverse();
    assert_eq!(rank_and_dedup(reversed, &prefs), expected);
    assert_eq!(rank_and_dedup(expected.clone(), &prefs), expected);
}

#[test]
fn movie_torrent_ties_and_mixed_manual_http_rows_are_deterministic() {
    let mut first = source(1);
    first.seeds = Some(5);
    let mut second = source(2);
    second.seeds = Some(5);
    let mut manual = source(3);
    manual.provider = "manual-torrent".into();
    manual.match_score = 100;
    manual.info_hash = Some(V1.into());
    let mut duplicate = source(4);
    duplicate.seeds = Some(1000);
    duplicate.info_hash = Some(V1.into());
    let mut http = source(5);
    http.locator = Locator::Http("https://example.org/video.webm".into());
    http.match_score = 100;
    let prefs = SourcePreferences::default();
    let sources = vec![first, second, manual, duplicate, http];
    let expected = rank_and_dedup(sources.clone(), &prefs);
    assert_eq!(expected.len(), 4);
    assert_eq!(
        expected
            .iter()
            .filter(|s| s.is_discovered_movie_torrent())
            .count(),
        3
    );
    assert_eq!(expected[0].seeds, Some(1000));
    assert!(expected[0].origins.contains(&"manual-torrent".into()));
    assert!(
        expected
            .iter()
            .any(|s| matches!(s.locator, Locator::Http(_)))
    );
    let mut reversed = sources;
    reversed.reverse();
    assert_eq!(rank_and_dedup(reversed, &prefs), expected);
}

#[test]
fn explicit_language_preference_precedes_default_resolution_fallback() {
    let mut localized = source(1);
    localized.languages = vec!["ru".into()];
    localized.resolution = Some(720);
    let mut large = source(2);
    large.languages = vec!["en".into()];
    large.resolution = Some(2160);
    let prefs = SourcePreferences {
        resolutions: Vec::new(),
        languages: vec!["RU".into()],
    };
    let result = rank_and_dedup(vec![large, localized], &prefs);
    assert_eq!(result[0].locator, Locator::RuTracker(1));
}

#[test]
fn scoring_precedes_persistence_and_prefiltered_offers_have_a_separate_path() {
    let query = query();
    let mut offer = source(1);
    offer.match_score = 100;
    assert_eq!(match_source(&query, offer).unwrap().match_score, 90);
    let mut prefiltered = source(2);
    prefiltered.title = "The Movie / Original Movie (2021) 1080p".into();
    let prefs = SourcePreferences::default();
    assert!(select_sources(&query, vec![prefiltered.clone()], &prefs).is_empty());
    assert_eq!(rank_and_dedup(vec![prefiltered], &prefs).len(), 1);
    let mut invalid = source(3);
    invalid.locator = Locator::RuTracker(0);
    assert!(rank_and_dedup(vec![invalid], &prefs).is_empty());
}

#[test]
fn ordering_and_merged_origins_are_permutation_invariant_even_for_ties() {
    let mut first = source(1);
    first.origins = vec!["origin-z".into()];
    let mut duplicate = first.clone();
    duplicate.origins = vec!["origin-a".into()];
    duplicate.quality = Some("WEB".into());
    let second = source(2);
    let prefs = SourcePreferences::default();
    let expected = rank_and_dedup(
        vec![first.clone(), duplicate.clone(), second.clone()],
        &prefs,
    );
    for sources in [
        vec![second.clone(), duplicate.clone(), first.clone()],
        vec![duplicate.clone(), first.clone(), second.clone()],
        vec![first, second, duplicate],
    ] {
        assert_eq!(rank_and_dedup(sources, &prefs), expected);
    }
    assert_eq!(
        expected
            .iter()
            .find(|source| source.locator == Locator::RuTracker(1))
            .unwrap()
            .origins,
        ["origin-a", "origin-z", "provider"]
    );
}
