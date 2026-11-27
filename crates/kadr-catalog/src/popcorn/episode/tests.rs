use super::*;
use kadr_core::catalog::ExternalMetadata;
use serde_json::json;

// Synthetic fixtures follow ShowNormalizer/EpisodeNormalizer/TorrentNormalizer:
// episode rows use seeds/peers, unlike the movie route's seed/peer. This agent's
// live requests were HTTP 403; these are not captured public magnet fixtures.
fn query() -> MediaQuery {
    MediaQuery {
        media_id: "local-only".into(),
        kind: MediaKind::Series,
        title: "Breaking Bad".into(),
        original_title: Some("Во все тяжкие".into()),
        year: Some(2008),
        external: ExternalMetadata {
            imdb_id: Some("tt0903747".into()),
            ..Default::default()
        },
        episode: EpisodeNumber::new(1, 1),
    }
}

fn row() -> Value {
    json!({
        "title": "Во все тяжкие / Breaking Bad S01E01 [2008, WEB-DL]",
        "url": "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
        "provider": "Rutracker",
        "quality": "1080p",
        "seeds": "0",
        "peers": 13
    })
}

fn parse_rows(rows: Value, query: &MediaQuery) -> Result<Vec<MediaSource>, Error> {
    let torrents: serde_json::Map<String, Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, row)| (index.to_string(), row.clone()))
        .collect();
    let episode = query.episode.unwrap_or(EpisodeNumber::new(1, 1).unwrap());
    parse_document(
        json!({
            "imdb_id": query.external.imdb_id,
            "_id": query.external.imdb_id,
            "title": query.title,
            "episodes": [{
                "season": episode.season(),
                "episode": episode.episode(),
                "torrents": torrents
            }]
        }),
        query,
    )
}

fn parse_document(document: Value, query: &MediaQuery) -> Result<Vec<MediaSource>, Error> {
    parse(&serde_json::to_vec(&document).unwrap(), query, 42)
}

#[test]
fn exact_single_maps_series_identity_and_episode_counts() {
    let sources = parse_rows(json!([row()]), &query()).unwrap();
    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(source.kind, MediaKind::Series);
    assert_eq!(source.episode, query().episode);
    assert_eq!(source.title, "Breaking Bad");
    assert_eq!(source.release_title.as_deref(), row()["title"].as_str());
    assert_eq!(source.external.imdb_id, query().external.imdb_id);
    assert_eq!(source.year, Some(2008));
    assert_eq!(source.seeds, Some(0));
    assert_eq!(source.peers, Some(13));
    assert_eq!(source.quality.as_deref(), Some("1080p"));
    assert_eq!(source.resolution, Some(1080));
    assert_eq!(source.size, None);
    assert_eq!(source.origins, ["popcorn", "popcorn:Rutracker"]);
    assert!(source.languages.is_empty());
    assert!(source.license.is_none());
    assert_eq!(source.fetched_at, 42);
    assert_eq!(source.match_score, 100);
    assert!(!source.possible_match);
    assert!(accepts(source));
}

#[test]
fn missing_or_invalid_counts_stay_unknown_and_zero_is_known() {
    let mut first = row();
    first["seeds"] = json!(-1);
    first["peers"] = json!("0");
    first["size"] = json!("123");
    let mut second = row();
    second["url"] = json!(format!("magnet:?xt=urn:btih:{}", "f".repeat(40)));
    second.as_object_mut().unwrap().remove("seeds");
    second["peer"] = json!(999);
    second["peers"] = json!(4294967296_u64);
    second["quality"] = json!("unknown");
    let sources = parse_rows(json!([first, second]), &query()).unwrap();
    assert_eq!(sources.len(), 2);
    let first = sources.iter().find(|row| row.size == Some(123)).unwrap();
    let second = sources.iter().find(|row| row.size.is_none()).unwrap();
    assert_eq!(first.seeds, None);
    assert_eq!(first.peers, Some(0));
    assert_eq!(second.seeds, None);
    assert_eq!(second.peers, None);
    assert_eq!(second.quality, None);
}

#[test]
fn later_season_year_is_not_the_series_premiere_year() {
    let mut query = query();
    query.episode = EpisodeNumber::new(5, 1);
    let mut row = row();
    row["title"] = json!("Breaking Bad S05E01 [2012, HDTV]");
    row["year"] = json!(2012);
    let sources = parse_rows(json!([row]), &query).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].year, Some(2008));
    assert!(accepts(&sources[0]));
}

#[test]
fn explicit_russian_labels_and_specials_are_numbered_not_guessed() {
    for (season, title) in [
        (1, "Breaking Bad [Сезон: 1, Серия: 1, 2008, HDTV]"),
        (0, "Breaking Bad S00E01 [2009, HDTV]"),
        (1, "Breaking Bad 1x01 [2008, HDTV]"),
    ] {
        let mut query = query();
        query.episode = EpisodeNumber::new(season, 1);
        let mut row = row();
        row["title"] = json!(title);
        assert_eq!(
            parse_rows(json!([row]), &query).unwrap().len(),
            1,
            "{title}"
        );
    }
}

#[test]
fn anime_uses_the_same_series_identity_without_absolute_episode_conversion() {
    let mut query = query();
    query.title = "Attack on Titan".into();
    query.original_title = Some("Shingeki no Kyojin".into());
    query.external.imdb_id = Some("tt2560140".into());
    query.year = Some(2013);
    let mut row = row();
    row["title"] = json!("Shingeki no Kyojin S01E01 [2013, HDTV]");
    let sources = parse_rows(json!([row]), &query).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].title, "Shingeki no Kyojin");
    assert_eq!(sources[0].external.imdb_id.as_deref(), Some("tt2560140"));
    assert!(accepts(&sources[0]));
    for title in [
        "[Group] Shingeki no Kyojin - 01 [1080p]",
        "Shingeki no Kyojin S01 [2013]",
        "Shingeki no Kyojin S01E02 [2013]",
    ] {
        let mut row = super::tests::row();
        row["title"] = json!(title);
        assert!(parse_rows(json!([row]), &query).unwrap().is_empty());
    }
}

#[test]
fn contradictory_identity_or_scope_never_inherits_endpoint_identity() {
    for (field, value) in [
        ("imdb", json!("tt2560140")),
        ("imdb_id", json!("tt2560140")),
        ("imdb_id", Value::Null),
        ("kind", json!("movie")),
        ("season", json!(2)),
        ("episode", json!(2)),
        ("episode", json!(-1)),
        ("season", Value::Null),
        ("pack", json!(true)),
        ("pack", json!("false")),
    ] {
        let mut row = row();
        row[field] = value;
        assert!(
            parse_rows(json!([row]), &query()).unwrap().is_empty(),
            "{field}"
        );
    }
    for title in [
        "Other Series S01E01 [2008]",
        "Breaking Bad Returns S01E01 [2008]",
        "Breaking Bad S02E01 [2008]",
        "Breaking Bad S01E02 [2008]",
        "Breaking Bad S01E01 [Сезон: 2]",
        "Breaking Bad S01E01 [Серия: 2]",
        "Breaking Bad S01E01-S01E02 [2008]",
        "Breaking Bad S01E01E02 [2008]",
        "Breaking Bad S01E01 [soundtrack]",
    ] {
        let mut row = row();
        row["title"] = json!(title);
        assert!(
            parse_rows(json!([row]), &query()).unwrap().is_empty(),
            "{title}"
        );
    }
}

#[test]
fn packs_need_mapped_file_even_when_they_mention_the_target_episode() {
    for title in [
        "Breaking Bad [Сезон: 1, Серии: 1-7, 2008]",
        "Breaking Bad S01 [2008]",
        "Breaking Bad S01E01-07 [2008]",
        "Breaking Bad S01E01 [Серии: 1-7]",
        "Breaking Bad S01E01 [Серии: 1, 2]",
        "Breaking Bad S01E01 [Season 1-2]",
        "Breaking Bad S01E01 Complete",
        "Breaking Bad S01E01 [Batch]",
        "Breaking Bad S01E01 [Pack]",
        "Breaking Bad [2008]",
    ] {
        for file in [Value::Null, json!("Breaking.Bad.S01E02.mkv")] {
            let mut row = row();
            row["title"] = json!(title);
            row["file"] = file;
            assert!(
                parse_rows(json!([row]), &query()).unwrap().is_empty(),
                "{title}"
            );
        }
    }
}

#[test]
fn file_must_independently_agree_and_parent_directory_is_not_episode_evidence() {
    let mut valid = row();
    valid["file"] = json!("Breaking.Bad/Breaking.Bad.S01E01.mkv");
    assert_eq!(parse_rows(json!([valid]), &query()).unwrap().len(), 1);
    for file in [
        json!("Breaking.Bad.S01E02.mkv"),
        json!("Breaking.Bad.S01E01E02.mkv"),
        json!("../Breaking.Bad.S01E01.mkv"),
        json!("/Breaking.Bad.S01E01.mkv"),
        json!("C:\\Breaking.Bad.S01E01.mkv"),
        json!(["Breaking.Bad.S01E01.mkv"]),
        json!("magnet:?private"),
        json!("Breaking.Bad.S01E01.srt"),
        json!("Breaking.Bad.S01E01.mp3"),
        json!("Breaking.Bad.Season.2.mkv"),
    ] {
        let mut row = row();
        row["file"] = file;
        assert!(parse_rows(json!([row]), &query()).unwrap().is_empty());
    }
}

#[test]
fn selected_offer_revalidation_rejects_tampered_identity_and_capability() {
    let original = parse_rows(json!([row()]), &query()).unwrap().remove(0);
    for mutation in 0..9 {
        let mut source = original.clone();
        match mutation {
            0 => source.episode = EpisodeNumber::new(1, 2),
            1 => source.kind = MediaKind::Movie,
            2 => source.external.imdb_id = None,
            3 => source.title = "Other Series".into(),
            4 => source.release_title = Some("Breaking Bad S01 [2008]".into()),
            5 => source.provider = "other".into(),
            6 => source.license = Some("not evidence".into()),
            7 => source.origins.push("popcorn:https://private".into()),
            _ => source.locator = Locator::Http("https://example.test/video".into()),
        }
        assert!(!accepts(&source), "mutation {mutation}");
    }
}

#[test]
fn alias_is_retained_so_selection_does_not_depend_on_discarded_query_aliases() {
    let mut query = query();
    query.title = "Localized title".into();
    query.original_title = None;
    query.external.alternative_titles = vec!["Breaking Bad".into()];
    let sources = parse_rows(json!([row()]), &query).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].title, "Breaking Bad");
    assert!(accepts(&sources[0]));
}

#[test]
fn bounds_shape_and_queries_fail_closed() {
    assert_eq!(
        parse(&vec![b' '; BODY_LIMIT + 1], &query(), 42),
        Err(Error::TooLarge)
    );
    assert_eq!(
        parse_rows(json!(vec![Value::Null; RESULT_LIMIT + 1]), &query()),
        Err(Error::TooLarge)
    );
    for value in [
        json!({}),
        json!({"episodes": []}),
        json!({"torrents": []}),
        json!(null),
    ] {
        assert_eq!(parse_document(value, &query()), Err(Error::InvalidResponse));
    }
    assert_eq!(
        parse(b"not json", &query(), 42),
        Err(Error::InvalidResponse)
    );
    assert!(
        parse_rows(json!([null, {}, false]), &query())
            .unwrap()
            .is_empty()
    );
    assert_eq!(parse(b"[]", &query(), -1), Err(Error::InvalidQuery));
    let mut invalid = query();
    invalid.episode = None;
    assert_eq!(parse_rows(json!([]), &invalid), Err(Error::InvalidQuery));
    invalid = query();
    invalid.kind = MediaKind::Movie;
    assert_eq!(parse_rows(json!([]), &invalid), Err(Error::InvalidQuery));
    invalid = query();
    invalid.external.imdb_id = Some("tt0903747/private".into());
    assert_eq!(parse_rows(json!([]), &invalid), Err(Error::InvalidQuery));
}

#[test]
fn long_running_series_does_not_apply_the_offer_limit_to_episode_numbers() {
    let mut episodes: Vec<_> = (2..=1200)
        .map(|i| json!({"season":1,"episode":i}))
        .collect();
    episodes.push(json!({"season":1, "episode":1, "torrents":{"1080p":row()}}));
    let doc = json!({"imdb_id":query().external.imdb_id,"episodes":episodes});
    assert_eq!(parse_document(doc, &query()).unwrap().len(), 1);
}

#[test]
fn mapped_pack_preserves_original_release_and_safe_filename_hint() {
    let mut row = row();
    row["title"] = json!("Breaking Bad [Сезон: 1, Серии: 1-7, 2008, HDTV]");
    row["file"] = json!("Breaking.Bad/Breaking.Bad.S01E01.mkv");
    let sources = parse_rows(json!([row]), &query()).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].episode_file_hint.as_deref(),
        Some("Breaking.Bad/Breaking.Bad.S01E01.mkv")
    );
    assert!(
        sources[0]
            .release_title
            .as_deref()
            .unwrap()
            .contains("Серии: 1-7")
    );
    assert!(accepts(&sources[0]));
    let mut missing_hint = sources[0].clone();
    missing_hint.episode_file_hint = None;
    assert!(!accepts(&missing_hint));
    for hint in ["Breaking.Bad.S01E02.mkv", "../Breaking.Bad.S01E01.mkv"] {
        let mut invalid = sources[0].clone();
        invalid.episode_file_hint = Some(hint.into());
        assert!(!accepts(&invalid));
    }
}

#[test]
fn anime_pack_uses_exact_mapping_not_guessed_absolute_episode_number() {
    let mut query = query();
    query.title = "Cowboy Bebop".into();
    query.original_title = Some("Ковбой Бибоп".into());
    query.external.imdb_id = Some("tt0213338".into());
    query.year = Some(1998);
    let mut row = row();
    row["title"] = json!("Ковбой Бибоп / Cowboy Bebop [S01] [1998, BDRip]");
    row["file"] = json!("Cowboy Bebop/01 - Asteroid Blues.mkv");
    let document = json!({
        "imdb_id": "tt0213338",
        "episodes": [
            {"season": 2, "episode": 1, "torrents": {"1080p": row}},
            {"season": 1, "episode": 1, "torrents": {"0": row, "1080p": row}},
            {"season": 1, "episode": 2, "torrents": {"1080p": null}}
        ]
    });
    let sources = parse_document(document.clone(), &query).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].episode, EpisodeNumber::new(1, 1));
    assert_eq!(
        sources[0].episode_file_hint.as_deref(),
        Some("Cowboy Bebop/01 - Asteroid Blues.mkv")
    );
    assert!(accepts(&sources[0]));
    let mut other_episode = document;
    other_episode["episodes"][1]["episode"] = json!(3);
    assert!(parse_document(other_episode, &query).unwrap().is_empty());
}

#[test]
fn mapped_file_does_not_override_contradictory_pack_scope() {
    for title in [
        "Breaking Bad [Сезон: 2, Серии: 1-7, 2009]",
        "Breaking Bad [Сезон: 1, Серии: 2-7, 2008]",
        "Breaking Bad S02 [2009]",
        "Breaking Bad S01E02 [2008]",
        "Other Series S01 [2008]",
    ] {
        let mut row = row();
        row["title"] = json!(title);
        row["file"] = json!("01.mkv");
        assert!(
            parse_rows(json!([row]), &query()).unwrap().is_empty(),
            "{title}"
        );
    }
}

#[test]
fn conflicting_files_for_one_torrent_and_episode_are_not_ranked() {
    let mut a = row();
    a["title"] = json!("Breaking Bad S01 [2008]");
    a["file"] = json!("first.mkv");
    let mut b = a.clone();
    b["file"] = json!("second.mkv");
    assert!(parse_rows(json!([a, b, a]), &query()).unwrap().is_empty());
}

#[test]
fn show_identity_and_unique_episode_mapping_are_required() {
    for document in [
        json!({"imdb_id": "tt2560140", "episodes": []}),
        json!({"_id": "tt0903747", "episodes": []}),
        json!({"imdb_id": "tt0903747", "_id": "tt2560140", "episodes": []}),
        json!({"imdb_id": "tt0903747", "episodes": null}),
        json!({"imdb_id": "tt0903747", "episodes": [
            {"season": 1, "episode": 1, "torrents": {}},
            {"season": "1", "episode": "1", "torrents": {}}
        ]}),
        json!({"imdb_id": "tt0903747", "episodes": [
            {"season": 1, "episode": 1, "torrents": [null]}
        ]}),
    ] {
        assert_eq!(
            parse_document(document, &query()),
            Err(Error::InvalidResponse)
        );
    }
    for episode in [
        Value::Null,
        json!({"season": 2, "episode": 1}),
        json!({"season": 1, "episode": 1}),
        json!({"season": 1, "episode": 1, "torrents": null}),
        json!({"season": 1, "episode": 1, "torrents": []}),
        json!({"season": 1, "episode": 1, "torrents": {"0": null}}),
    ] {
        assert!(
            parse_document(
                json!({"imdb_id": "tt0903747", "episodes": [episode]}),
                &query()
            )
            .unwrap()
            .is_empty()
        );
    }
    assert_eq!(
        parse_document(
            json!({"imdb_id": "tt0903747", "episodes": vec![Value::Null; EPISODE_LIMIT + 1]}),
            &query()
        ),
        Err(Error::TooLarge)
    );
}
