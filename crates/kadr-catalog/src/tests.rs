use super::*;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FAKE_CLIENT_ID: &str = "synthetic-test-client_id";

fn client() -> Client {
    let mut client = Client::new(Options::default()).unwrap();
    // Never mutate process environment or send a real client ID to fixtures.
    client.client_id = client_id(FAKE_CLIENT_ID);
    client
}

fn movie(id: u64) -> Value {
    json!({
        "ids":{"trakt":id,"slug":"dune-2021","tmdb":438631,"imdb":"tt1160419"},
        "title":"Dune", "year":2021, "overview":"История Арракиса.",
        "rating":8.26, "votes":100,
        "images":{
            "poster":["media.trakt.tv/movies/1/medium/poster.webp"],
            "fanart":["walter-r2.trakt.tv/images/movies/000/012/601/fanarts/medium/a.jpg.webp"]
        }
    })
}

fn show(id: u64) -> Value {
    json!({
        "ids":{"trakt":id,"slug":"sherlock","tvdb":176941},
        "title":"Sherlock", "year":2010, "overview":"История Шерлока.",
        "rating":9.1, "votes":10
    })
}

fn search_row(data: Value, kind: MediaKind) -> Value {
    match kind {
        MediaKind::Movie => json!({"type":"movie","score":123,"movie":data}),
        MediaKind::Series => json!({"type":"show","score":123,"show":data}),
    }
}

fn trending_row(data: Value, kind: MediaKind) -> Value {
    match kind {
        MediaKind::Movie => json!({"watchers":1,"movie":data}),
        MediaKind::Series => json!({"watchers":1,"show":data}),
    }
}

#[test]
fn movie_show_fixtures_use_only_documented_trakt_metadata() {
    let media = trakt::media(&movie(1), MediaKind::Movie).unwrap();
    assert_eq!(media.id, "trakt:movie:1");
    assert_eq!(media.title, "Dune");
    assert_eq!(media.original_title, None);
    assert_eq!(media.year, Some(2021));
    assert_eq!(media.rating_tenths, Some(83));
    assert_eq!(media.description, "История Арракиса.");
    assert_eq!(
        media.poster_url.as_deref(),
        Some("https://media.trakt.tv/movies/1/medium/poster.webp")
    );
    assert_eq!(
        media.backdrop_url.as_deref(),
        Some("https://walter-r2.trakt.tv/images/movies/000/012/601/fanarts/medium/a.jpg.webp")
    );
    assert_eq!(media.source_page, "https://trakt.tv/movies/1");
    assert!(media.attribution.contains("Trakt"));
    assert_eq!(media.license, None);
    assert!(media.episodes.is_empty());
    let series = trakt::media(&show(2), MediaKind::Series).unwrap();
    assert_eq!(series.id, "trakt:show:2");
    assert_eq!(series.kind, MediaKind::Series);
    assert_eq!(series.title, "Sherlock");
    assert_eq!(series.year, Some(2010));
    assert_eq!(series.original_title, None);
    assert_eq!(series.source_page, "https://trakt.tv/shows/2");

    // An explicitly supplied original title is metadata, not a fabricated fallback.
    let mut data = movie(1);
    data["original_title"] = json!("  Original\n title ");
    assert_eq!(
        trakt::media(&data, MediaKind::Movie)
            .unwrap()
            .original_title
            .as_deref(),
        Some("Original title")
    );
    data["original_title"] = json!("я".repeat(100_000));
    assert_eq!(
        trakt::media(&data, MediaKind::Movie)
            .unwrap()
            .original_title
            .unwrap()
            .chars()
            .count(),
        500
    );
    data["original_title"] = Value::Null;
    assert_eq!(
        trakt::media(&data, MediaKind::Movie)
            .unwrap()
            .original_title,
        None
    );
    data["ids"].as_object_mut().unwrap().remove("trakt");
    assert!(trakt::media(&data, MediaKind::Movie).is_none());
}

#[test]
fn only_actual_ten_point_votes_with_positive_counts_are_ratings() {
    for (score, count, expected) in [
        (json!(8.26), json!(1), Some(83)),
        (json!(10), json!(1), Some(100)),
        (json!(0), json!(1), Some(0)),
        (json!(9), json!(0), None),
        (json!(9), Value::Null, None),
        (json!(9), json!(-1), None),
        (json!(9), json!("100"), None),
        (json!(9), json!(1.5), None),
        (json!(-1), json!(10), None),
        (json!(10.1), json!(10), None),
        (json!("8.5"), json!(10), None),
        (Value::Null, json!(10), None),
    ] {
        let mut data = movie(1);
        data["rating"] = score;
        data["votes"] = count;
        assert_eq!(
            trakt::media(&data, MediaKind::Movie).unwrap().rating_tenths,
            expected
        );
    }
    let mut row = search_row(movie(1), MediaKind::Movie);
    row["movie"].as_object_mut().unwrap().remove("rating");
    row["movie"]["popularity"] = json!(999999);
    let rows = trakt::list(&json!([row]), MediaKind::Movie).unwrap();
    assert_eq!(rows[0].rating_tenths, None); // Search score is not a user rating.
}

#[test]
fn invalid_media_and_non_movie_show_wrappers_are_excluded() {
    // Trakt has no required TMDB-style adult flag.
    assert!(movie(1).get("adult").is_none());
    assert!(trakt::media(&movie(1), MediaKind::Movie).is_some());
    assert!(trakt::media(&show(2), MediaKind::Series).is_some());
    for id in [json!(0), json!(-1), json!("1"), json!(1.5), Value::Null] {
        let mut data = movie(1);
        data["ids"]["trakt"] = id;
        assert!(trakt::media(&data, MediaKind::Movie).is_none());
    }
    for title in [Value::Null, json!(123), json!(""), json!(" \t ")] {
        let mut data = movie(1);
        data["title"] = title;
        assert!(trakt::media(&data, MediaKind::Movie).is_none());
    }
    for (kind, data, other) in [
        (MediaKind::Movie, movie(1), "show"),
        (MediaKind::Series, show(2), "movie"),
    ] {
        let mut wrong_type = search_row(data.clone(), kind);
        wrong_type["type"] = json!(other);
        let mut wrong_type_value = search_row(data.clone(), kind);
        wrong_type_value["type"] = Value::Null;
        for row in [
            wrong_type,
            wrong_type_value,
            json!({"type":"person","person":{"ids":{"trakt":1},"name":"Person"}}),
            json!({"type":"episode","episode":{"ids":{"trakt":1},"title":"Episode"}}),
            json!({"type":"list","list":{"ids":{"trakt":1},"name":"List"}}),
            json!({"type":other}),
            data.clone(),
        ] {
            assert!(
                trakt::list(&json!([row]), kind).unwrap().is_empty(),
                "{kind:?}"
            );
        }
        for row in [search_row(data.clone(), kind), trending_row(data, kind)] {
            assert_eq!(trakt::list(&json!([row]), kind).unwrap().len(), 1);
        }
    }
}

#[test]
fn images_accept_only_the_two_public_trakt_host_families() {
    for kind in ["movies", "shows"] {
        for size in ["thumb", "medium", "original"] {
            for extension in ["jpg", "jpeg", "png", "webp"] {
                for (stem, legacy_stem) in [("poster", "posters"), ("fanart", "fanarts")] {
                    for path in [
                        format!("media.trakt.tv/{kind}/1/{size}/{stem}.{extension}"),
                        format!(
                            "media.trakt.tv/images/{kind}/000/012/601/{legacy_stem}/{size}/a_B-2.{extension}"
                        ),
                        format!(
                            "walter-r2.trakt.tv/images/{kind}/000/012/601/{legacy_stem}/{size}/a_B-2.{extension}"
                        ),
                    ] {
                        let mut data = movie(1);
                        data["images"] = json!({"poster":[path],"fanart":[path]});
                        let media = trakt::media(&data, MediaKind::Movie).unwrap();
                        let expected = format!("https://{path}");
                        assert_eq!(media.poster_url.as_deref(), Some(expected.as_str()));
                        assert_eq!(media.backdrop_url.as_deref(), Some(expected.as_str()));
                        assert!(is_public_catalog_image(&Url::parse(&expected).unwrap()));
                    }
                }
            }
        }
    }
    let legacy = "walter-r2.trakt.tv/images/shows/000/001/388/fanarts/medium/a-B_2.jpg.webp";
    let mut data = movie(1);
    data["images"] = json!({"fanart":[legacy]});
    assert_eq!(
        trakt::media(&data, MediaKind::Movie).unwrap().backdrop_url,
        Some(format!("https://{legacy}"))
    );
    assert!(is_public_catalog_image(
        &Url::parse(&format!("https://{legacy}")).unwrap()
    ));
    for path in [
        "https://media.trakt.tv/a.jpg",
        "http://media.trakt.tv/a.jpg",
        "//media.trakt.tv/a.jpg",
        "media.trakt.tv.evil.example/a.jpg",
        "evil.example/media.trakt.tv/a.jpg",
        "image.tmdb.org/a.jpg",
        "media.trakt.tv@evil.example/a.jpg",
        "user@media.trakt.tv/a.jpg",
        "media.trakt.tv:443/a.jpg",
        "media.trakt.tv:444/a.jpg",
        "media.trakt.tv/../a.jpg",
        "media.trakt.tv/a/../b.jpg",
        "media.trakt.tv/./a.jpg",
        "media.trakt.tv/%2e%2e/a.jpg",
        "media.trakt.tv/a%20b.jpg",
        "media.trakt.tv/a.jpg?token=secret",
        "media.trakt.tv/a.jpg#fragment",
        "media.trakt.tv/a\\b.jpg",
        "media.trakt.tv/a\n.jpg",
        "media.trakt.tv/а.jpg",
        "media.trakt.tv/a.svg",
        "media.trakt.tv/",
        "media.trakt.tv",
        "walter-r2.trakt.tv/movies/1/medium/poster.webp",
        "walter-r2.trakt.tv/images/movies/1/posters/medium/a.jpg.webp",
        "media.trakt.tv/people/1/medium/poster.webp",
        "media.trakt.tv/movies/0/medium/poster.webp",
        "media.trakt.tv/movies/1/unknown/poster.webp",
        "media.trakt.tv/movies/1/medium/arbitrary.webp",
        "media.trakt.tv/movies/1/medium/poster.webp?token=secret",
        "media.trakt.tv/movies/1/medium/poster.webp#fragment",
        "media.trakt.tv/movies/1/../1/medium/poster.webp",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a..jpg",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/.jpg",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a%20b.jpg",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a\\b.jpg",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/а.jpg",
        "walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a\n.jpg",
        "",
    ] {
        let mut data = movie(1);
        data["images"] = json!({"poster":[path],"fanart":[path]});
        let media = trakt::media(&data, MediaKind::Movie).unwrap();
        assert!(media.poster_url.is_none(), "{path:?}");
        assert!(media.backdrop_url.is_none(), "{path:?}");
    }
}

#[test]
fn public_image_validator_rejects_noncanonical_destinations() {
    for url in [
        "http://media.trakt.tv/movies/1/medium/poster.webp",
        "https://media.trakt.tv.evil.example/movies/1/medium/poster.webp",
        "https://user@media.trakt.tv/movies/1/medium/poster.webp",
        "https://media.trakt.tv:444/movies/1/medium/poster.webp",
        "https://media.trakt.tv/movies/1/medium/poster.webp?token=secret",
        "https://media.trakt.tv/movies/1/medium/poster.webp#fragment",
        "https://walter-r2.trakt.tv/movies/1/medium/poster.webp",
        "https://walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a%20b.jpg",
        "https://walter-r2.trakt.tv/images/movies/000/012/601/posters/medium/a.svg",
    ] {
        assert!(!is_public_catalog_image(&Url::parse(url).unwrap()), "{url}");
    }
}

#[test]
fn image_arrays_choose_first_safe_entry_within_twenty_candidates() {
    let safe = "media.trakt.tv/movies/1/medium/poster.webp";
    let later = "walter-r2.trakt.tv/images/movies/000/012/601/fanarts/medium/other.png";
    let mut data = movie(1);
    data["images"] = json!({
        "poster":[null,123,"https://evil.example/a.jpg",safe,later],
        "fanart":[later,safe]
    });
    let media = trakt::media(&data, MediaKind::Movie).unwrap();
    assert_eq!(media.poster_url, Some(format!("https://{safe}")));
    assert_eq!(media.backdrop_url, Some(format!("https://{later}")));
    for (invalid_count, expected) in [(19, Some(format!("https://{safe}"))), (20, None)] {
        let mut entries = vec![Value::Null; invalid_count];
        entries.push(json!(safe));
        data["images"] = json!({"poster":entries,"fanart":entries});
        let media = trakt::media(&data, MediaKind::Movie).unwrap();
        assert_eq!(media.poster_url, expected);
        assert_eq!(media.backdrop_url, expected);
    }
    for images in [
        Value::Null,
        json!({}),
        json!({"poster":safe,"fanart":safe}),
        json!({"poster":[],"fanart":[]}),
    ] {
        data["images"] = images;
        let media = trakt::media(&data, MediaKind::Movie).unwrap();
        assert!(media.poster_url.is_none() && media.backdrop_url.is_none());
    }
}

#[test]
fn fields_and_pages_are_bounded_without_fabrication_or_crawling() {
    let mut data = movie(1);
    data["title"] = json!("я".repeat(100_000));
    data["overview"] = json!("б".repeat(100_000));
    let media = trakt::media(&data, MediaKind::Movie).unwrap();
    assert_eq!(media.title.chars().count(), 500);
    assert_eq!(media.description.chars().count(), 16_000);
    let minimal = trakt::media(
        &json!({"ids":{"trakt":1},"title":"Only title"}),
        MediaKind::Movie,
    )
    .unwrap();
    assert_eq!(minimal.original_title, None);
    assert_eq!(minimal.year, None);
    assert_eq!(minimal.rating_tenths, None);
    assert_eq!(minimal.poster_url, None);
    assert_eq!(minimal.backdrop_url, None);
    assert!(minimal.description.is_empty());
    assert!(minimal.episodes.is_empty());
    let docs: Vec<_> = (1..=30)
        .map(|id| search_row(movie(id), MediaKind::Movie))
        .collect();
    let list = trakt::list(&json!(docs), MediaKind::Movie).unwrap();
    assert_eq!(list.len(), SEARCH_LIMIT);
    assert_eq!(list[19].id, "trakt:movie:20");
    let row = trending_row(movie(1), MediaKind::Movie);
    assert_eq!(
        trakt::list(&json!([row, row]), MediaKind::Movie)
            .unwrap()
            .len(),
        1
    );
    // Invalid/duplicate rows cannot trigger a second page or unbounded scanning.
    let mut docs = vec![Value::Null; SEARCH_LIMIT];
    docs.push(search_row(movie(1), MediaKind::Movie));
    assert!(
        trakt::list(&json!(docs), MediaKind::Movie)
            .unwrap()
            .is_empty()
    );
    for data in [json!({}), json!({"results":[]}), Value::Null, json!("html")] {
        assert_eq!(
            trakt::list(&data, MediaKind::Movie),
            Err(Error::InvalidResponse)
        );
    }
    assert!(
        trakt::list(&json!([]), MediaKind::Movie)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn descriptions_are_plain_text_and_year_is_not_inferred_from_other_fields() {
    let mut data = movie(1);
    data["title"] = json!("  Dune\n Part\tOne\u{0000} ");
    data["overview"] = json!("<p>First</p>\nSecond\u{0000}");
    let media = trakt::media(&data, MediaKind::Movie).unwrap();
    assert_eq!(media.title, "Dune Part One");
    assert_eq!(media.description, "First Second");
    for year in [Value::Null, json!("2021"), json!(-1), json!(1.5), json!(0)] {
        data["year"] = year;
        data["released"] = json!("2021-09-15");
        assert_eq!(trakt::media(&data, MediaKind::Movie).unwrap().year, None);
    }
}

#[test]
fn literal_titles_are_display_text_not_html() {
    for title in ["<3", "A < B", "<b>Title</b>", "Love & Other Things"] {
        let mut data = movie(1);
        data["title"] = json!(title);
        data["original_title"] = json!(title);
        let media = trakt::media(&data, MediaKind::Movie).unwrap();
        assert_eq!(media.title, title);
        assert_eq!(media.original_title.as_deref(), Some(title));
        let data = json!([{"number":0,"episodes":[{"season":0,"number":1,"title":title}]}]);
        assert_eq!(trakt::episodes(&data).unwrap()[0].title, title);
    }
}

#[test]
fn old_serialized_media_defaults_backdrop_and_rating() {
    let mut data =
        serde_json::to_value(trakt::media(&movie(1), MediaKind::Movie).unwrap()).unwrap();
    data.as_object_mut().unwrap().remove("backdrop_url");
    data.as_object_mut().unwrap().remove("rating_tenths");
    let media: Media = serde_json::from_value(data).unwrap();
    assert_eq!(media.backdrop_url, None);
    assert_eq!(media.rating_tenths, None);
}

#[test]
fn opaque_ids_are_canonical_positive_and_cannot_escape_endpoints() {
    for (id, kind, number) in [
        ("trakt:movie:1", MediaKind::Movie, 1),
        ("trakt:show:2", MediaKind::Series, 2),
        (
            "trakt:movie:18446744073709551615",
            MediaKind::Movie,
            u64::MAX,
        ),
    ] {
        assert_eq!(trakt::provider_id(id).unwrap(), (kind, number));
    }
    for id in [
        "archive:film",
        "tmdb:movie:1",
        "trakt:person:1",
        "trakt:tv:1",
        "trakt:movie:0",
        "trakt:show:01",
        "trakt:show:+1",
        "trakt:show:-1",
        "trakt:movie:",
        "trakt:movie:1:2",
        "trakt:show:1/seasons",
        "trakt:movie:1?x=y",
        "trakt:movie:1#x",
        "trakt:movie:%31",
        "trakt:movie:１",
        "trakt:movie:18446744073709551616",
        "TRAKT:movie:1",
        "trakt:movie: 1",
        "trakt:movie:1 ",
    ] {
        assert_eq!(trakt::provider_id(id), Err(Error::InvalidId), "{id}");
    }
}

#[test]
fn endpoints_are_fixed_https_bounded_and_client_id_free() {
    for (kind, operation, query, path) in [
        (
            MediaKind::Movie,
            "search",
            Some("Дюна & query=other"),
            "/search/movie",
        ),
        (
            MediaKind::Series,
            "search",
            Some("Дюна & query=other"),
            "/search/show",
        ),
        (MediaKind::Movie, "trending", None, "/movies/trending"),
        (MediaKind::Series, "trending", None, "/shows/trending"),
    ] {
        let url = trakt::list_url(kind, operation, query).unwrap();
        assert!(trakt::api_url(&url));
        assert_eq!(url.origin().ascii_serialization(), "https://api.trakt.tv");
        assert_eq!(url.path(), path);
        let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["limit"], "20");
        assert_eq!(pairs["extended"], "full");
        assert_eq!(pairs.get("query").map(String::as_str), query);
        assert_eq!(pairs.len(), if query.is_some() { 3 } else { 2 });
        assert!(!url.as_str().contains(FAKE_CLIENT_ID));
    }
    assert_eq!(
        trakt::list_url(MediaKind::Movie, "unsupported", None),
        Err(Error::InvalidResponse)
    );
    for segments in [
        vec!["movies", "1"],
        vec!["shows", "2"],
        vec!["shows", "2", "seasons"],
    ] {
        let url = trakt::endpoint(&segments).unwrap();
        assert!(trakt::api_url(&url));
        assert_eq!(url.path(), format!("/{}", segments.join("/")));
        assert!(!url.as_str().contains(FAKE_CLIENT_ID));
    }
    for url in [
        "http://api.trakt.tv/movies/1",
        "https://evil.example/movies/1",
        "https://api.trakt.tv.evil.example/movies/1",
        "https://user@api.trakt.tv/movies/1",
        "https://user:password@api.trakt.tv/movies/1",
        "https://api.trakt.tv:444/movies/1",
        "https://api.trakt.tv/movies/1#fragment",
    ] {
        assert!(!trakt::api_url(&Url::parse(url).unwrap()));
    }
}

#[test]
fn client_id_validation_precedence_and_diagnostics_are_safe() {
    for raw in [
        "",
        " ",
        " a",
        "a ",
        "a\tb",
        "a\nb",
        "a\r\nInjected: x",
        "Bearer token",
        "клиент",
        "a.b",
        "a/b",
        "a+b",
        "a=b",
        "a%b",
        "a~b",
    ] {
        assert!(client_id(raw).is_none());
    }
    assert!(client_id(&"a".repeat(257)).is_none());
    for raw in ["a", "A0-_", FAKE_CLIENT_ID, &"a".repeat(256)] {
        let header = client_id(raw).unwrap();
        assert_eq!(header.to_str().unwrap(), raw);
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains(raw));
    }
    for (runtime, compiled, expected) in [
        (None, None, None),
        (None, Some(FAKE_CLIENT_ID), Some(FAKE_CLIENT_ID)),
        (Some("runtime-id"), Some(FAKE_CLIENT_ID), Some("runtime-id")),
        (Some("runtime-id"), None, Some("runtime-id")),
        (Some(""), Some(FAKE_CLIENT_ID), None),
        (Some("invalid id"), Some(FAKE_CLIENT_ID), None),
        (None, Some("invalid id"), None),
        (None, Some(""), None),
    ] {
        let actual = configured_client_id(runtime, compiled);
        assert_eq!(actual.as_ref().and_then(|h| h.to_str().ok()), expected);
        assert!(actual.as_ref().is_none_or(HeaderValue::is_sensitive));
    }
    assert!(!format!("{:?}", client()).contains(FAKE_CLIENT_ID));
    let message = Error::MissingClientId.to_string();
    assert!(message.contains("[TRAKT_MISSING_CLIENT_ID]"));
    assert!(message.contains("KADR_TRAKT_CLIENT_ID"));
    assert!(message.to_lowercase().contains("client"));
    assert!(message.to_lowercase().contains("restart"));
    for error in [
        Error::MissingClientId,
        Error::Network,
        Error::Http(401),
        Error::Http(403),
        Error::InvalidResponse,
    ] {
        assert!(!format!("{error} {error:?}").contains(FAKE_CLIENT_ID));
        assert!(std::error::Error::source(&error).is_none());
    }
}

#[tokio::test]
async fn construction_without_client_id_and_consent_cancellation_gates() {
    // Inspect only presence, never print real runtime or compiled client IDs.
    let mut client = Client::new(Options { enabled: false }).unwrap();
    let runtime = std::env::var("KADR_TRAKT_CLIENT_ID").ok();
    let expected = configured_client_id(runtime.as_deref(), option_env!("KADR_TRAKT_CLIENT_ID"));
    assert_eq!(client.client_id.is_some(), expected.is_some());
    client.client_id = None;
    let cancel = CancellationToken::new();
    assert_eq!(client.search("query", &cancel).await, Err(Error::Disabled));
    assert_eq!(client.browse(&cancel).await, Err(Error::Disabled));
    assert_eq!(
        client.details("trakt:movie:1", &cancel).await,
        Err(Error::Disabled)
    );
    cancel.cancel();
    assert_eq!(client.search("query", &cancel).await, Err(Error::Disabled));
    let cancel = CancellationToken::new();
    client.options.enabled = true;
    assert_eq!(
        client.search("query", &cancel).await,
        Err(Error::MissingClientId)
    );
    assert_eq!(client.browse(&cancel).await, Err(Error::MissingClientId));
    assert_eq!(
        client.details("trakt:movie:1", &cancel).await,
        Err(Error::MissingClientId)
    );
    cancel.cancel();
    assert_eq!(client.search("query", &cancel).await, Err(Error::Cancelled));
    assert_eq!(client.browse(&cancel).await, Err(Error::Cancelled));
    assert_eq!(
        client.details("trakt:movie:1", &cancel).await,
        Err(Error::Cancelled)
    );
}

// All transport fixtures use synthetic credentials only.
type Route = (u16, Value);
struct Fixture {
    origin: Url,
    requests: Arc<Mutex<Vec<String>>>,
    max_active: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture(routes: Vec<(String, Route)>) -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let routes: Arc<HashMap<_, _>> = Arc::new(routes.into_iter().collect());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let saved = requests.clone();
    let max = max_active.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let routes = routes.clone();
            let saved = saved.clone();
            let active = active.clone();
            let max = max.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    assert!(request.len() < 16_000);
                }
                let request = String::from_utf8(request).unwrap();
                let target = request.split_whitespace().nth(1).unwrap();
                let path = target.split('?').next().unwrap();
                let (status, body) = routes.get(path).cloned().unwrap_or((404, json!({})));
                saved.lock().unwrap().push(request);
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Fixture {
        origin,
        requests,
        max_active,
        task,
    }
}

fn fixture_client(fixture: &Fixture) -> Client {
    let mut client = client();
    client.fixture_origin = Some(fixture.origin.clone());
    client
}

fn request_url(request: &str) -> Url {
    let target = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    Url::parse(&format!("https://api.trakt.tv{target}")).unwrap()
}

fn assert_trakt_headers(request: &str) {
    let url = request_url(request);
    assert!(!url.as_str().contains(FAKE_CLIENT_ID));
    let headers: Vec<_> = request
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim()))
        .collect();
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| name == "trakt-api-key")
            .collect::<Vec<_>>(),
        vec![&("trakt-api-key".into(), FAKE_CLIENT_ID)]
    );
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| name == "trakt-api-version")
            .collect::<Vec<_>>(),
        vec![&("trakt-api-version".into(), "2")]
    );
    assert_eq!(
        headers
            .iter()
            .filter(|(name, _)| name == "content-type")
            .collect::<Vec<_>>(),
        vec![&("content-type".into(), "application/json")]
    );
    assert!(!headers.iter().any(|(name, _)| name == "authorization"));
    assert_eq!(request.matches(FAKE_CLIENT_ID).count(), 1);
}

#[tokio::test]
async fn search_browse_details_use_exact_routes_and_client_id_only_in_header() {
    let fixture = fixture(vec![
        (
            "/search/movie".into(),
            (200, json!([search_row(movie(1), MediaKind::Movie)])),
        ),
        (
            "/search/show".into(),
            (200, json!([search_row(show(2), MediaKind::Series)])),
        ),
        (
            "/movies/trending".into(),
            (200, json!([trending_row(movie(3), MediaKind::Movie)])),
        ),
        (
            "/shows/trending".into(),
            (200, json!([trending_row(show(4), MediaKind::Series)])),
        ),
        ("/movies/1".into(), (200, movie(1))),
    ])
    .await;
    let client = fixture_client(&fixture);
    let cancel = CancellationToken::new();
    let query = "Дюна & friends";
    let results = client.search(query, &cancel).await.unwrap();
    assert_eq!(
        results.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["trakt:movie:1", "trakt:show:2"]
    );
    let browse = client.browse(&cancel).await.unwrap();
    assert_eq!(browse.movies[0].id, "trakt:movie:3");
    assert_eq!(browse.shows[0].id, "trakt:show:4");
    assert_eq!(
        client.details("trakt:movie:1", &cancel).await.unwrap(),
        results[0]
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(fixture.max_active.load(Ordering::SeqCst), 2);
    for request in requests.iter() {
        assert_trakt_headers(request);
        let url = request_url(request);
        let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["extended"], "full");
        if url.path().starts_with("/search/") {
            assert_eq!(pairs["limit"], "20");
            assert_eq!(pairs["query"], query);
            assert_eq!(url.query_pairs().count(), 3);
        } else if url.path().ends_with("/trending") {
            assert_eq!(pairs["limit"], "20");
            assert_eq!(url.query_pairs().count(), 2);
        } else {
            assert_eq!(url.path(), "/movies/1");
            assert_eq!(url.query_pairs().count(), 1);
        }
    }
}

#[tokio::test]
async fn search_prioritizes_normalized_exact_titles_globally_without_extra_requests() {
    let mut broad_movie = movie(1);
    broad_movie["title"] = json!("Breaking Bad: The Movie");
    let mut exact_movie = movie(3);
    exact_movie["title"] = json!("BREAKING BAD");
    let mut broad_show = show(2);
    broad_show["title"] = json!("Breaking Bad Stories");
    let mut exact_show = show(4);
    exact_show["title"] = json!("Breaking   Bad");
    let fixture = fixture(vec![
        (
            "/search/movie".into(),
            (
                200,
                json!([
                    search_row(broad_movie, MediaKind::Movie),
                    search_row(exact_movie, MediaKind::Movie)
                ]),
            ),
        ),
        (
            "/search/show".into(),
            (
                200,
                json!([
                    search_row(broad_show, MediaKind::Series),
                    search_row(exact_show, MediaKind::Series)
                ]),
            ),
        ),
    ])
    .await;
    let results = fixture_client(&fixture)
        .search("  breaking   bad  ", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        results.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        [
            "trakt:movie:3",
            "trakt:show:4",
            "trakt:movie:1",
            "trakt:show:2"
        ]
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn search_exact_title_matching_preserves_literal_angle_brackets() {
    let mut broad = movie(1);
    broad["title"] = json!("I <3 Movies");
    let mut exact = show(2);
    exact["title"] = json!("<3");
    let fixture = fixture(vec![
        (
            "/search/movie".into(),
            (200, json!([search_row(broad, MediaKind::Movie)])),
        ),
        (
            "/search/show".into(),
            (200, json!([search_row(exact, MediaKind::Series)])),
        ),
    ])
    .await;
    let results = fixture_client(&fixture)
        .search("<3", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        results
            .iter()
            .map(|media| media.id.as_str())
            .collect::<Vec<_>>(),
        ["trakt:show:2", "trakt:movie:1"]
    );
    assert_eq!(results[0].title, "<3");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn endpoint_failure_only_degrades_to_nonempty_real_results() {
    for operation in ["search", "trending"] {
        for (movie_status, movie_data, show_status, show_data, expected) in [
            (503, None, 200, Some(show(2)), Ok((0, 1))),
            (200, Some(movie(1)), 429, None, Ok((1, 0))),
            (503, None, 200, None, Err(Error::Http(503))),
            (200, None, 429, None, Err(Error::RateLimited)),
            (503, None, 429, None, Err(Error::Http(503))),
            (200, None, 200, None, Ok((0, 0))),
        ] {
            let rows = |data: Option<Value>, kind| {
                Value::Array(
                    data.into_iter()
                        .map(|data| {
                            if operation == "search" {
                                search_row(data, kind)
                            } else {
                                trending_row(data, kind)
                            }
                        })
                        .collect(),
                )
            };
            let paths = if operation == "search" {
                ["/search/movie", "/search/show"]
            } else {
                ["/movies/trending", "/shows/trending"]
            };
            let fixture = fixture(vec![
                (
                    paths[0].into(),
                    (movie_status, rows(movie_data, MediaKind::Movie)),
                ),
                (
                    paths[1].into(),
                    (show_status, rows(show_data, MediaKind::Series)),
                ),
            ])
            .await;
            let client = fixture_client(&fixture);
            let cancel = CancellationToken::new();
            let result = if operation == "search" {
                client.search("query", &cancel).await.map(|rows| {
                    (
                        rows.iter().filter(|m| m.kind == MediaKind::Movie).count(),
                        rows.iter().filter(|m| m.kind == MediaKind::Series).count(),
                    )
                })
            } else {
                client
                    .browse(&cancel)
                    .await
                    .map(|b| (b.movies.len(), b.shows.len()))
            };
            assert_eq!(result, expected, "{operation}");
            assert_eq!(fixture.requests.lock().unwrap().len(), 2);
        }
    }
}

fn season(number: u16, count: u16) -> Value {
    let rows: Vec<_> = (1..=count)
        .rev()
        .map(|episode| {
            json!({
                "season":number, "number":episode,
                "title":format!("Эпизод {episode}"), "overview":"Описание"
            })
        })
        .collect();
    json!({"number":number,"episodes":rows})
}

#[test]
fn seasons_and_episodes_are_complete_sorted_and_include_specials() {
    let data = json!([season(9, 2), season(1, 2), season(0, 2)]);
    let episodes = trakt::episodes(&data).unwrap();
    assert_eq!(
        episodes
            .iter()
            .map(|e| (e.season, e.number))
            .collect::<Vec<_>>(),
        [(0, 1), (0, 2), (1, 1), (1, 2), (9, 1), (9, 2)]
    );
    assert!(episodes.iter().all(|e| e.title.starts_with("Эпизод")));
    assert!(episodes.iter().all(|e| e.description == "Описание"));
    assert!(trakt::episodes(&json!([])).unwrap().is_empty());
    assert!(trakt::episodes(&json!([season(0, 0)])).unwrap().is_empty());
}

#[test]
fn malformed_duplicate_or_mismatched_seasons_and_episodes_are_rejected() {
    let mut invalid = vec![
        Value::Null,
        json!({}),
        json!({"seasons":[]}),
        json!([null]),
        json!([{}]),
        json!([{"number":1}]),
        json!([{"number":1,"episodes":null}]),
        json!([{"number":1,"episodes":{}}]),
        json!([season(1, 1), season(1, 1)]),
        json!([{"number":1,"episodes":[
            {"season":1,"number":1},{"season":1,"number":1}
        ]}]),
        json!([{"number":1,"episodes":[{"season":2,"number":1}]}]),
        json!([{"number":1,"episodes":[{"number":1}]}]),
        json!([{"number":1,"episodes":[{"season":1}]}]),
    ];
    for number in [
        Value::Null,
        json!(-1),
        json!("1"),
        json!(1.5),
        json!(65_536),
    ] {
        invalid.push(json!([{"number":number,"episodes":[]}]));
        invalid.push(json!([{"number":1,"episodes":[{"season":number,"number":1}]}]));
        invalid.push(json!([{"number":1,"episodes":[{"season":1,"number":number}]}]));
    }
    invalid.push(json!([{"number":1,"episodes":[{"season":1,"number":0}]}]));
    for data in invalid {
        assert_eq!(
            trakt::episodes(&data),
            Err(Error::InvalidResponse),
            "{data}"
        );
    }
}

#[test]
fn episode_text_is_bounded_plain_and_never_fabricated() {
    let data = json!([{"number":0,"episodes":[
        {"season":0,"number":1,"title":"я".repeat(100_000),"overview":"б".repeat(100_000)},
        {"season":0,"number":2},
        {"season":0,"number":3,"title":"Title\n next","overview":"<p>Text</p>\u{0000}"}
    ]}]);
    let episodes = trakt::episodes(&data).unwrap();
    assert_eq!(episodes[0].title.chars().count(), 500);
    assert_eq!(episodes[0].description.chars().count(), 8_000);
    assert!(episodes[1].title.is_empty() && episodes[1].description.is_empty());
    assert_eq!(episodes[2].title, "Title next");
    assert_eq!(episodes[2].description, "Text");
}

#[test]
fn episode_collection_limits_accept_boundaries_and_reject_instead_of_truncating() {
    for (seasons, expected) in [
        ((0..100).map(|n| season(n, 1)).collect::<Vec<_>>(), Ok(100)),
        (vec![season(1, 1_000)], Ok(1_000)),
        ((0..20).map(|n| season(n, 1_000)).collect(), Ok(20_000)),
        (
            (0..101).map(|n| season(n, 1)).collect(),
            Err(Error::TooLarge),
        ),
        (vec![season(1, 1_001)], Err(Error::TooLarge)),
        (
            (0..21).map(|n| season(n, 1_000)).collect(),
            Err(Error::TooLarge),
        ),
    ] {
        assert_eq!(
            trakt::episodes(&json!(seasons)).map(|rows| rows.len()),
            expected
        );
    }
}

#[tokio::test]
async fn show_details_fetch_all_seasons_once_without_per_season_fanout() {
    let seasons: Vec<_> = (0..=9).rev().map(|n| season(n, 2)).collect();
    let fixture = fixture(vec![
        ("/shows/2".into(), (200, show(2))),
        ("/shows/2/seasons".into(), (200, json!(seasons))),
    ])
    .await;
    let media = fixture_client(&fixture)
        .details("trakt:show:2", &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(media.episodes.len(), 20);
    assert_eq!((media.episodes[0].season, media.episodes[0].number), (0, 1));
    assert_eq!(
        (media.episodes[19].season, media.episodes[19].number),
        (9, 2)
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(fixture.max_active.load(Ordering::SeqCst), 1);
    for request in requests.iter() {
        assert_trakt_headers(request);
        let url = request_url(request);
        let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.query_pairs().count(), 1);
        assert_eq!(
            pairs["extended"],
            if url.path() == "/shows/2" {
                "full"
            } else {
                "episodes,full"
            }
        );
        assert!(["/shows/2", "/shows/2/seasons"].contains(&url.path()));
    }
}

#[tokio::test]
async fn unavailable_invalid_or_wrong_details_never_invent_results() {
    for (status, body, expected) in [
        (503, json!({}), Error::Http(503)),
        (200, json!({}), Error::InvalidResponse),
        (
            200,
            json!([season(1, 1), season(1, 1)]),
            Error::InvalidResponse,
        ),
        (
            200,
            json!([{"number":1,"episodes":[{"season":2,"number":1}]}]),
            Error::InvalidResponse,
        ),
        (
            200,
            json!((0..101).map(|n| season(n, 0)).collect::<Vec<_>>()),
            Error::TooLarge,
        ),
    ] {
        let fixture = fixture(vec![
            ("/shows/2".into(), (200, show(2))),
            ("/shows/2/seasons".into(), (status, body)),
        ])
        .await;
        assert_eq!(
            fixture_client(&fixture)
                .details("trakt:show:2", &CancellationToken::new())
                .await,
            Err(expected)
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    }
    for (kind, path, id, body) in [
        (MediaKind::Movie, "/movies/1", "trakt:movie:1", movie(2)),
        (MediaKind::Series, "/shows/2", "trakt:show:2", show(3)),
    ] {
        let fixture = fixture(vec![(path.into(), (200, body))]).await;
        assert_eq!(
            fixture_client(&fixture)
                .details(id, &CancellationToken::new())
                .await,
            Err(Error::InvalidResponse),
            "{kind:?}"
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
    let mut no_title = movie(1);
    no_title["title"] = Value::Null;
    let missing_title = fixture(vec![("/movies/1".into(), (200, no_title))]).await;
    assert_eq!(
        fixture_client(&missing_title)
            .details("trakt:movie:1", &CancellationToken::new())
            .await,
        Err(Error::NotFound)
    );
    let fixture = fixture(vec![
        ("/shows/2".into(), (200, show(2))),
        ("/shows/2/seasons".into(), (200, json!([]))),
    ])
    .await;
    assert!(
        fixture_client(&fixture)
            .details("trakt:show:2", &CancellationToken::new())
            .await
            .unwrap()
            .episodes
            .is_empty()
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

async fn server(response: Vec<u8>) -> (Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).await;
        let _ = stream.write_all(&response).await;
    });
    (url, task)
}

#[tokio::test]
async fn transport_status_redirects_json_and_body_limits() {
    let client = client();
    let cancel = CancellationToken::new();
    for (response, expected) in [
        (
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".into(),
            Error::Http(401),
        ),
        (
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".into(),
            Error::Http(403),
        ),
        (
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n".into(),
            Error::RateLimited,
        ),
        (
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into(),
            Error::NotFound,
        ),
        (
            "HTTP/1.1 302 Found\r\nLocation: https://evil.example/\r\nContent-Length: 0\r\n\r\n"
                .into(),
            Error::Http(302),
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nhtml".into(),
            Error::InvalidResponse,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                BODY_LIMIT + 1
            ),
            Error::TooLarge,
        ),
    ] {
        let (url, task) = server(response.into_bytes()).await;
        assert_eq!(
            client.transport(client.http.get(url), &cancel).await,
            Err(expected)
        );
        task.await.unwrap();
    }
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        BODY_LIMIT + 1
    )
    .into_bytes();
    response.extend(vec![b' '; BODY_LIMIT + 1]);
    response.extend_from_slice(b"\r\n0\r\n\r\n");
    let (url, task) = server(response).await;
    assert_eq!(
        client.transport(client.http.get(url), &cancel).await,
        Err(Error::TooLarge)
    );
    task.await.unwrap();
    let (url, task) = server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
    assert_eq!(
        client
            .transport(client.http.get(url), &cancel)
            .await
            .unwrap(),
        json!({})
    );
    task.await.unwrap();
}

#[tokio::test]
async fn redirects_never_forward_client_id_to_another_destination() {
    let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: http://{}/leak\r\nContent-Length: 0\r\n\r\n",
        destination.local_addr().unwrap()
    );
    let (url, task) = server(response.into_bytes()).await;
    let mut client = client();
    client.fixture_origin = Some(url);
    assert_eq!(
        client
            .json(
                trakt::endpoint(&["movies", "1"]).unwrap(),
                &CancellationToken::new()
            )
            .await,
        Err(Error::Http(302))
    );
    task.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), destination.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stalled_transport_times_out_without_exposing_request_context() {
    let client = client();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = connection.read(&mut request).await;
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
            .await
            .unwrap();
        std::future::pending::<()>().await;
    });
    let result = client
        .transport(
            client.http.get(url).timeout(Duration::from_millis(50)),
            &CancellationToken::new(),
        )
        .await;
    task.abort();
    assert_eq!(result, Err(Error::Timeout));
}

#[tokio::test]
async fn invalid_queries_ids_and_non_trakt_destinations_never_make_requests() {
    let fixture = fixture(vec![]).await;
    let client = fixture_client(&fixture);
    let cancel = CancellationToken::new();
    for query in [
        "".to_string(),
        " \t ".into(),
        "a".repeat(201),
        "a\nb".into(),
    ] {
        assert_eq!(
            client.search(&query, &cancel).await,
            Err(Error::InvalidQuery)
        );
    }
    assert_eq!(
        client.details("archive:film", &cancel).await,
        Err(Error::InvalidId)
    );
    assert_eq!(
        client
            .json(Url::parse("http://127.0.0.1:1/movies/1").unwrap(), &cancel)
            .await,
        Err(Error::InvalidResponse)
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_interrupts_transport_and_the_single_seasons_request() {
    let client = client();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let cancel = CancellationToken::new();
    let pending = client.transport(client.http.get(url), &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("unexpected response: {result:?}"),
        connection = listener.accept() => { let _connection = connection.unwrap(); cancel.cancel(); }
    }
    assert_eq!(pending.await, Err(Error::Cancelled));

    let fixture = fixture(vec![
        ("/shows/2".into(), (200, show(2))),
        ("/shows/2/seasons".into(), (200, json!([season(1, 1)]))),
    ])
    .await;
    let client = fixture_client(&fixture);
    let cancel = CancellationToken::new();
    let pending = client.details("trakt:show:2", &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("unexpected response: {result:?}"),
        _ = async {
            loop {
                if fixture.requests.lock().unwrap().len() == 2 { break; }
                tokio::task::yield_now().await;
            }
        } => { cancel.cancel(); }
    }
    assert_eq!(pending.await, Err(Error::Cancelled));
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
}

/// Explicit opt-in only, using the caller's runtime client ID, never the compiled default.
#[tokio::test]
#[ignore = "live Trakt network; opt in with KADR_TRAKT_CLIENT_ID and --ignored live_catalog"]
async fn live_catalog() {
    let mut client = Client::new(Options::default()).unwrap();
    client.client_id = std::env::var("KADR_TRAKT_CLIENT_ID")
        .ok()
        .as_deref()
        .and_then(client_id);
    assert!(
        client.client_id.is_some(),
        "Set a valid runtime KADR_TRAKT_CLIENT_ID to opt in."
    );
    let cancel = CancellationToken::new();
    let results = client.search("Breaking Bad", &cancel).await.unwrap();
    let found = results
        .iter()
        .find(|m| m.kind == MediaKind::Series && m.title == "Breaking Bad")
        .unwrap();
    let browse = client.browse(&cancel).await.unwrap();
    assert!(!browse.movies.is_empty() && !browse.shows.is_empty());
    let movie = client.details(&browse.movies[0].id, &cancel).await.unwrap();
    assert_eq!(movie.id, browse.movies[0].id);
    let show = client.details(&found.id, &cancel).await.unwrap();
    assert_eq!(show.id, found.id);
    assert!(show.episodes.iter().any(|e| e.season == 1 && e.number == 1));
}
