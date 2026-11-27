use super::*;
use kadr_core::{
    catalog::ExternalMetadata,
    sources::{SourcePreferences, rank_and_dedup, select_sources},
    torrent::EpisodeNumber,
};
use serde_json::json;

// Synthetic identity, hash and URLs; the v2 envelope/field types follow the
// published movie_details documentation. No real torrent is fetched or opened.
const FIXTURE: &[u8] = include_bytes!("fixtures/movie_details.json");

fn query() -> MediaQuery {
    MediaQuery {
        media_id: "private-local-id".into(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        original_title: None,
        year: Some(1999),
        external: ExternalMetadata {
            imdb_id: Some("tt0133093".into()),
            ..Default::default()
        },
        episode: None,
    }
}

fn fixture() -> Value {
    serde_json::from_slice(FIXTURE).unwrap()
}

fn parse_value(data: Value) -> Result<Vec<MediaSource>, Error> {
    parse(&serde_json::to_vec(&data).unwrap(), &query(), 42)
}

#[test]
fn documented_movie_details_maps_only_verified_identity_and_metadata() {
    let sources = parse(FIXTURE, &query(), 42).unwrap();
    assert_eq!(sources.len(), 2);
    let source = &sources[0];
    assert_eq!(source.provider, YTS_ID);
    assert_eq!(source.origins, [YTS_ID]);
    assert_eq!(source.title, "The Matrix");
    assert_eq!(source.release_title.as_deref(), Some("The Matrix (1999)"));
    assert_eq!(source.kind, MediaKind::Movie);
    assert_eq!(source.year, Some(1999));
    assert_eq!(source.external.imdb_id.as_deref(), Some("tt0133093"));
    assert_eq!(source.external.tmdb_id, None);
    assert_eq!(source.provider_item_id, None);
    assert_eq!(source.license, None);
    assert!(source.languages.is_empty()); // Movie language is not audio evidence.
    assert_eq!(source.video_codec, None);
    assert_eq!(source.audio_codec, None);
    assert_eq!(source.quality.as_deref(), Some("1080p"));
    assert_eq!(source.resolution, Some(1080));
    assert_eq!(source.seeds, Some(0));
    assert_eq!(source.peers, Some(13));
    assert_eq!(source.size, Some(3156800962));
    assert_eq!(source.fetched_at, 42);
    assert_eq!(source.match_score, 100);
    assert!(!source.possible_match);
    assert_eq!(
        source.locator,
        Locator::Magnet("magnet:?xt=urn:btih:abcdef0123456789abcdef0123456789abcdef01".into())
    );
    assert_eq!(
        source.info_hash.as_deref(),
        Some("abcdef0123456789abcdef0123456789abcdef01")
    );
    assert_eq!(source.clone().normalize().unwrap(), *source);
    assert_eq!(sources[1].seeds, None);
    assert_eq!(sources[1].peers, Some(0));
    assert_eq!(sources[1].size, None);
    assert!(sources.iter().all(accepts));
    assert!(!format!("{sources:?}").contains("magnet:"));
}

#[test]
fn exact_response_imdb_is_required_even_with_matching_title_and_year() {
    for imdb in [
        Value::Null,
        json!("tt"),
        json!("tt9999999"),
        json!("0133093"),
        json!("TT0133093"),
        json!(" tt0133093"),
        json!("tt0133093?private=fixture"),
        json!(133093),
    ] {
        let mut data = fixture();
        data["data"]["movie"]["imdb_code"] = imdb;
        assert!(parse_value(data).unwrap().is_empty());
    }
    let mut data = fixture();
    data["data"]["movie"]
        .as_object_mut()
        .unwrap()
        .remove("imdb_code");
    assert!(parse_value(data).unwrap().is_empty());
}

#[test]
fn imdb_match_cannot_override_title_year_or_movie_scope_conflicts() {
    for (field, value) in [
        ("title", json!("The Matrix Reloaded")),
        ("title", json!("The Matrix S01E01")),
        ("title", json!("The Matrix 1x02")),
        ("title", json!("The Matrix [1999-2003]")),
        ("title", json!("The Matrix (1999) Collection")),
        ("title", json!("The Matrix (TV Series)")),
        ("title", json!("The Matrix (FLAC soundtrack)")),
        ("title", json!("The Matrix (Audio only)")),
        ("title", json!("https://private.invalid/?fixture=secret")),
        ("title", json!("The Matrix\n")),
        ("title", json!("")),
        ("title", Value::Null),
        ("year", json!(2003)),
        ("year", json!("1999")),
        ("year", json!(0)),
        ("year", json!(2200)),
        ("year", json!(-1)),
        ("year", json!(1999.5)),
        ("year", Value::Null),
        ("kind", json!("series")),
        ("type", json!("series")),
        ("season", json!(1)),
        ("episode", json!(1)),
        ("pack", json!(true)),
    ] {
        let mut data = fixture();
        data["data"]["movie"][field] = value.clone();
        assert!(parse_value(data).unwrap().is_empty(), "{field}: {value}");
    }
}

#[test]
fn supported_title_aliases_and_unknown_query_year_keep_actual_api_identity() {
    for mode in 0..3 {
        let mut query = query();
        query.title = "Localized title".into();
        match mode {
            0 => query.original_title = Some("The Matrix".into()),
            1 => query.external.alternative_titles = vec!["The Matrix".into()],
            _ => {
                query.original_title = Some("The Matrix".into());
                query.year = None;
            }
        }
        let sources = parse(FIXTURE, &query, 42).unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources.iter().all(|source| source.title == "The Matrix"));
        assert!(sources.iter().all(accepts));
        assert!(sources.iter().all(|source| source.year == Some(1999)));
    }
}

#[test]
fn hash_is_the_only_locator_input_and_must_be_documented_hex_btih() {
    for hash in [
        Value::Null,
        json!(""),
        json!("a".repeat(39)),
        json!("a".repeat(41)),
        json!("z".repeat(40)),
        json!("A".repeat(32)), // Base32 is not the documented YTS hash field.
        json!("a".repeat(64)),
        json!("0123456789012345678901234567890123456789&tr=http://localhost"),
        json!("magnet:?xt=urn:btih:0123456789012345678901234567890123456789"),
        json!("https://private.invalid/file.torrent"),
        json!(42),
    ] {
        let mut data = fixture();
        data["data"]["movie"]["torrents"][0]["hash"] = hash;
        data["data"]["movie"]["torrents"][1] = Value::Null;
        assert!(parse_value(data).unwrap().is_empty());
    }
    let mut data = fixture();
    for row in data["data"]["movie"]["torrents"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("url");
    }
    assert_eq!(parse_value(data).unwrap().len(), 2);
}

#[test]
fn optional_unknown_metadata_does_not_become_zero_or_a_url() {
    for value in [
        Value::Null,
        json!(-1),
        json!(1.25),
        json!("unknown"),
        json!(u64::MAX),
        json!({}),
    ] {
        let mut data = fixture();
        data["data"]["movie"]["torrents"][0]["seeds"] = value.clone();
        data["data"]["movie"]["torrents"][0]["peers"] = value;
        let sources = parse_value(data).unwrap();
        assert_eq!(sources[0].seeds, None);
        assert_eq!(sources[0].peers, None);
    }
    let mut data = fixture();
    data["data"]["movie"]["torrents"][0]["quality"] = json!("unknown");
    data["data"]["movie"]["torrents"][0]["size_bytes"] = json!("2.94 GB");
    let sources = parse_value(data).unwrap();
    assert_eq!(sources[0].quality, None);
    assert_eq!(sources[0].resolution, None);
    assert_eq!(sources[0].size, None);
}

#[test]
fn independent_yts_capability_survives_ranking_selection_and_deduplication() {
    let sources = parse(FIXTURE, &query(), 42).unwrap();
    let preferences = SourcePreferences::default();
    let ranked = rank_and_dedup(sources.clone(), &preferences);
    let selected = select_sources(&query(), sources.clone(), &preferences);
    assert_eq!(ranked.len(), 2);
    assert_eq!(selected.len(), 2);
    assert!(ranked.iter().chain(&selected).all(accepts));
    let mut duplicated = sources[0].clone();
    duplicated.origins.push("popcorn".into());
    duplicated = duplicated.normalize().unwrap();
    assert!(accepts(&duplicated));
    let deduped = rank_and_dedup(vec![sources[0].clone(), duplicated], &preferences);
    assert_eq!(deduped.len(), 1);
    assert!(accepts(&deduped[0]));
}

#[test]
fn accepts_rechecks_provider_movie_identity_safe_hash_only_magnet_and_normalization() {
    let original = parse(FIXTURE, &query(), 42).unwrap().remove(0);
    let mutations: &[fn(&mut MediaSource)] = &[
        |source| source.provider = "popcorn".into(),
        |source| source.kind = MediaKind::Series,
        |source| source.episode = EpisodeNumber::new(1, 1),
        |source| source.external.imdb_id = None,
        |source| source.year = None,
        |source| source.year = Some(2003),
        |source| source.title = "Other title".into(),
        |source| source.release_title = None,
        |source| source.license = Some("unverified".into()),
        |source| source.possible_match = true,
        |source| source.id = "forged".into(),
        |source| source.quality = Some("https://private.invalid".into()),
        |source| source.locator = Locator::Http("https://private.invalid".into()),
        |source| {
            let Locator::Magnet(uri) = &mut source.locator else {
                unreachable!()
            };
            uri.push_str("&tr=http://localhost/announce");
        },
        |source| {
            let Locator::Magnet(uri) = &mut source.locator else {
                unreachable!()
            };
            uri.make_ascii_uppercase();
        },
    ];
    for mutate in mutations {
        let mut source = original.clone();
        mutate(&mut source);
        assert!(!accepts(&source));
    }
}

#[test]
fn malformed_envelopes_and_limits_fail_with_redacted_errors() {
    for body in [
        b"<html>private</html>".as_slice(),
        b"{",
        b"null",
        b"[]",
        b"{}",
        br#"{"status":"error","status_message":"private"}"#,
        br#"{"status":"ok","data":{"movie":null}}"#,
        br#"{"status":"ok","data":{"movie":{"torrents":null}}}"#,
    ] {
        assert_eq!(parse(body, &query(), 42), Err(Error::InvalidResponse));
    }
    assert_eq!(
        parse(&vec![b' '; BODY_LIMIT + 1], &query(), 42),
        Err(Error::TooLarge)
    );
    let mut data = fixture();
    data["data"]["movie"]["torrents"] = json!(vec![Value::Null; RESULT_LIMIT + 1]);
    assert_eq!(parse_value(data), Err(Error::TooLarge));
    assert_eq!(parse(FIXTURE, &query(), -1), Err(Error::InvalidQuery));
    let mut data = fixture();
    data["data"]["movie"]["torrents"] = json!([]);
    assert!(parse_value(data).unwrap().is_empty());
}

#[test]
fn unsupported_queries_never_make_offers() {
    let mut queries = Vec::new();
    let mut q = query();
    q.kind = MediaKind::Series;
    queries.push(q);
    let mut q = query();
    q.episode = EpisodeNumber::new(1, 1);
    queries.push(q);
    let mut q = query();
    q.external.imdb_id = None;
    queries.push(q);
    let mut q = query();
    q.external.imdb_id = Some("tt".into());
    queries.push(q);
    let mut q = query();
    q.year = Some(0);
    queries.push(q);
    for query in queries {
        assert!(parse(FIXTURE, &query, 42).unwrap().is_empty());
    }
}
