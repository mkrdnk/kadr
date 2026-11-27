use super::*;
use kadr_core::{catalog::ExternalMetadata, torrent::EpisodeNumber};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

// Synthetic identities/URLs; field names, types, bilingual release structure and
// double HTML separators mirror the audited live movie response.
const FIXTURE: &str = include_str!("fixtures/movies.json");

fn query() -> MediaQuery {
    MediaQuery {
        media_id: "private-local-catalog-id".into(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        original_title: Some("The Matrix".into()),
        year: Some(1999),
        external: ExternalMetadata {
            imdb_id: Some("tt0133093".into()),
            ..Default::default()
        },
        episode: None,
    }
}

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn parse_value(data: Value) -> Result<Vec<MediaSource>, Error> {
    parse(&serde_json::to_vec(&data).unwrap(), &query(), 42)
}

#[test]
fn support_requires_canonical_imdb_single_movie() {
    assert!(supports(&query()));
    let mut q = query();
    q.year = None;
    assert!(supports(&q));
    for id in [
        "",
        "0133093",
        "tt123",
        "tt12345678901",
        " tt0133093",
        "tt0133093/other",
        "tt0133093?private",
    ] {
        let mut q = query();
        q.external.imdb_id = Some(id.into());
        assert!(!supports(&q));
    }
    let mut q = query();
    q.external.imdb_id = None;
    assert!(!supports(&q));
    q = query();
    q.kind = MediaKind::Series;
    assert!(!supports(&q));
    q = query();
    q.episode = EpisodeNumber::new(1, 1);
    assert!(!supports(&q));
    q = query();
    q.year = Some(0);
    assert!(!supports(&q));
}

#[test]
fn live_shaped_fixture_preserves_identity_safe_query_and_unknown_vs_zero() {
    let rows = parse_value(fixture()).unwrap();
    assert_eq!(rows.len(), 2);
    let first = &rows[0];
    assert_eq!(first.provider, ID);
    assert_eq!(first.external.imdb_id.as_deref(), Some("tt0133093"));
    assert_eq!(first.external.tmdb_id, None);
    assert_eq!(first.kind, MediaKind::Movie);
    assert_eq!(first.title, "The Matrix");
    assert_eq!(
        first.release_title,
        fixture()[0]["title"].as_str().map(str::to_owned)
    );
    assert_eq!(first.year, Some(1999));
    assert_eq!(first.quality.as_deref(), Some("1080p"));
    assert_eq!(first.resolution, Some(1080));
    assert_eq!(first.seeds, Some(0));
    assert_eq!(first.peers, Some(13));
    assert_eq!(first.size, Some(1234567890));
    assert_eq!(first.fetched_at, 42);
    assert_eq!(first.match_score, 100);
    assert!(!first.possible_match);
    assert_eq!(first.origins, ["popcorn", "popcorn:Rutracker"]);
    assert!(first.languages.is_empty()); // Locale is not audio-language evidence.
    assert!(first.license.is_none());
    assert_eq!(first.provider_item_id, None);
    let Locator::Magnet(uri) = &first.locator else {
        panic!()
    };
    assert!(uri.ends_with("&dn=The%20Matrix&tr=http%3A%2F%2Fbt.t-ru.org%2Fann%3Fmagnet"));
    assert!(!uri.contains("retracker.local"));
    assert!(!uri.contains("&amp;"));
    assert_eq!(rows[1].seeds, None);
    assert_eq!(rows[1].peers, Some(0));
    assert_eq!(rows[1].size, Some(0));
    assert_eq!(rows[1].quality, None);
    assert_eq!(rows[1].resolution, None);
    assert!(rows[1].possible_match);
    assert!(rows.iter().all(accepts));
}

#[test]
fn explicit_year_title_pack_and_scope_conflicts_override_endpoint_imdb() {
    for title in [
        "The Matrix [2003, BDRemux]",
        "The Matrix Reloaded [1999, BDRemux]",
        "The Matrix [1999-2003, BDRemux]",
        "The Matrix [1999] Collection",
        "The Matrix [1999] Trilogy",
        "The Matrix S01E01 [1999]",
        "The Matrix 1x02 [1999]",
        "The Matrix [1999, season 1]",
        "The Matrix [1999, FLAC soundtrack]",
        "The Matrix [1999, audiobook]",
        "The Matrix (1999) [Аудиодорожка, DTS]",
        "The Matrix (1999) [Audio only, AAC]",
        "The Matrix (1999) [TV Series]",
        "The Matrix [1999] https://private.invalid/?token=secret",
    ] {
        let mut data = fixture();
        data[0]["title"] = json!(title);
        assert!(parse_value(json!([data[0]])).unwrap().is_empty(), "{title}");
    }
    for (key, value) in [
        ("year", json!(2003)),
        ("imdb_id", json!("tt9999999")),
        ("kind", json!("series")),
        ("season", json!(1)),
        ("episode", json!(1)),
        ("pack", json!(true)),
    ] {
        let mut row = fixture()[0].clone();
        row[key] = value;
        assert!(parse_value(json!([row])).unwrap().is_empty(), "{key}");
    }
}

#[test]
fn unknown_metadata_is_not_inferred_or_coerced_to_zero() {
    for value in [
        Value::Null,
        json!("unknown"),
        json!(-1),
        json!(1.2),
        json!(u64::MAX),
        json!({}),
    ] {
        let mut row = fixture()[0].clone();
        row["seed"] = value;
        assert_eq!(parse_value(json!([row])).unwrap()[0].seeds, None);
    }
    let mut row = fixture()[0].clone();
    row["quality"] = json!("unknown");
    row["size"] = json!("2.94 GB");
    row["provider"] = json!("https://private.invalid/?secret=fixture");
    let source = &parse_value(json!([row])).unwrap()[0];
    assert_eq!(source.quality, None);
    assert_eq!(source.size, None);
    assert_eq!(source.origins, [ID]);
}

#[test]
fn original_and_alternative_titles_survive_accepts_revalidation() {
    for original in [true, false] {
        let mut q = query();
        q.title = "Localized catalog title".into();
        if !original {
            q.original_title = None;
            q.external.alternative_titles = vec!["The Matrix".into()];
        }
        let rows = parse(FIXTURE.as_bytes(), &q, 42).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(accepts));
        assert!(rows.iter().all(|source| source.title == "The Matrix"));
    }
}

#[test]
fn v1_and_hybrid_magnets_work_but_pure_v2_is_not_playable() {
    let v1 = "magnet:?xt=urn:btih:0123456789012345678901234567890123456789";
    let v2 = format!("magnet:?xt=urn:btmh:1220{}", "a".repeat(64));
    assert!(magnet(&v2).is_none());
    let hybrid = format!("{v1}&{}", v2.strip_prefix("magnet:?").unwrap());
    assert_eq!(magnet(&hybrid), Some(hybrid));
}

#[test]
fn provenance_labels_are_canonical_and_not_urls_or_private_text() {
    assert_eq!(upstream_label("popcorn:Rutracker"), Some("Rutracker"));
    assert_eq!(
        upstream_label("popcorn:NNM Club-1_test"),
        Some("NNM Club-1_test")
    );
    for label in [
        "popcorn:",
        "popcorn: private",
        "popcorn:private ",
        "popcorn:a..b",
        "popcorn:тест",
        "popcorn:https://host",
        "popcorn:token=secret",
        "other:Rutor",
        "popcorn:a\n",
    ] {
        assert_eq!(upstream_label(label), None);
    }
}

#[test]
fn unsafe_trackers_are_removed_but_alternate_url_entrypoints_reject_row() {
    let base = "magnet:?xt=urn:btih:0123456789012345678901234567890123456789";
    for tracker in [
        "http://localhost/announce",
        "http://127.0.0.1/announce",
        "http://[::1]/announce",
        "http://192.168.1.1/announce",
        "http://arbitrary.example/announce",
        "http://bt.t-ru.org:8080/ann",
        "http://user:password@bt.t-ru.org/ann",
        "http://bt.t-ru.org/ann?token=secret",
        "http://bt.t-ru.org/other",
        "file:///announce",
    ] {
        let encoded: String = url::form_urlencoded::byte_serialize(tracker.as_bytes()).collect();
        assert_eq!(magnet(&format!("{base}&tr={encoded}")), Some(base.into()));
    }
    for tracker in [
        "http://bt.t-ru.org/ann",
        "http://bt2.t-ru.org/ann?magnet",
        "http://bt3.t-ru.org/ann",
        "http://bt4.t-ru.org/ann",
        "udp://opentor.net:6969",
    ] {
        let encoded: String = url::form_urlencoded::byte_serialize(tracker.as_bytes()).collect();
        let uri = format!("{base}&tr={encoded}&dn=The%20Matrix&xl=0");
        assert_eq!(magnet(&uri), Some(uri));
    }
    for key in ["ws", "as", "xs", "x.pe", "unknown"] {
        assert!(magnet(&format!("{base}&{key}=http%3A%2F%2F127.0.0.1")).is_none());
    }
    for invalid in [
        "https://private.invalid/file.torrent",
        "magnet:?dn=missing",
        "magnet:?xt=urn:btih:not-a-hash",
        "magnet:?xt=urn:btih:0123456789012345678901234567890123456789#fragment",
    ] {
        assert!(magnet(invalid).is_none());
    }
}

#[test]
fn accepts_rechecks_normalization_identity_release_and_safe_locator() {
    let source = parse_value(fixture()).unwrap().remove(0);
    assert!(accepts(&source));
    let mut deduped = source.clone();
    deduped.origins.push("rutracker".into());
    deduped = deduped.normalize().unwrap();
    assert!(accepts(&deduped));
    let mut mutations = Vec::new();
    let mut s = source.clone();
    s.provider = "other".into();
    mutations.push(s);
    let mut s = source.clone();
    s.external.imdb_id = None;
    mutations.push(s);
    let mut s = source.clone();
    s.year = Some(2003);
    mutations.push(s);
    let mut s = source.clone();
    s.release_title = Some("The Matrix [1999] Collection".into());
    mutations.push(s);
    let mut s = source.clone();
    s.release_title = None;
    mutations.push(s);
    let mut s = source.clone();
    s.locator = Locator::Http("https://public.invalid/video".into());
    mutations.push(s);
    let mut s = source.clone();
    s.license = Some("unverified".into());
    mutations.push(s);
    let mut s = source.clone();
    s.origins.push("popcorn:https://secret.invalid".into());
    mutations.push(s);
    let mut s = source.clone();
    s.id = "forged".into();
    mutations.push(s);
    assert!(mutations.iter().all(|s| !accepts(s)));
    let mut possible = parse_value(fixture()).unwrap().remove(1);
    possible.possible_match = false;
    assert!(!accepts(&possible));
}

#[test]
fn malformed_and_oversized_responses_fail_without_body_details() {
    for body in [
        b"<html>private response</html>".as_slice(),
        b"{}",
        b"null",
        b"{",
        b"[1,2]",
    ] {
        let result = parse(body, &query(), 42);
        if body == b"[1,2]" {
            assert_eq!(result.unwrap().len(), 0);
        } else {
            assert_eq!(result, Err(Error::InvalidResponse));
        }
    }
    assert_eq!(
        parse(&vec![b' '; BODY_LIMIT + 1], &query(), 42),
        Err(Error::TooLarge)
    );
    assert_eq!(
        parse_value(json!(vec![Value::Null; RESULT_LIMIT + 1])),
        Err(Error::TooLarge)
    );
    for error in [
        Error::InvalidProxy,
        Error::InvalidQuery,
        Error::Cancelled,
        Error::Network,
        Error::Timeout,
        Error::RateLimited,
        Error::Http(503),
        Error::TooLarge,
        Error::InvalidResponse,
    ] {
        assert!(!error.to_string().contains("private"));
    }
}

struct Server {
    origin: Url,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                    assert!(request.len() < 8192);
                }
                received
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                let _ = socket.write_all(&response).await;
            }
        });
        Self {
            origin,
            requests,
            task,
        }
    }

    fn client(&self) -> Client {
        Client {
            http: http_builder().no_proxy().build().unwrap(),
            fixture_origins: Some(vec![self.origin.clone()]),
            timeout: TIMEOUT,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn response(body: &[u8]) -> Vec<u8> {
    let mut reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nSet-Cookie: fixture=private\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    reply.extend_from_slice(body);
    reply
}

#[tokio::test]
async fn fixed_get_contains_only_validated_id_and_locales_never_cookies() {
    let server = Server::start(vec![response(FIXTURE.as_bytes()), response(b"[]")]).await;
    let client = server.client();
    let cancel = CancellationToken::new();
    assert_eq!(client.search(&query(), 42, &cancel).await.unwrap().len(), 2);
    assert!(
        client
            .search(&query(), 42, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert!(
            request.starts_with(
                "GET /movie/tt0133093/torrents?contentLocale=ru&locale=ru HTTP/1.1\r\n"
            )
        );
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("cookie:"));
        assert!(!lower.contains("authorization:"));
        assert!(!request.contains("The Matrix"));
        assert!(!request.contains("private-local-catalog-id"));
    }
}

#[tokio::test]
async fn unsupported_cancelled_and_invalid_requests_do_not_connect() {
    let server = Server::start(vec![response(b"[]")]).await;
    let client = server.client();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        client.search(&query(), 42, &cancel).await,
        Err(Error::Cancelled)
    );
    let cancel = CancellationToken::new();
    assert_eq!(
        client.search(&query(), -1, &cancel).await,
        Err(Error::InvalidQuery)
    );
    let mut q = query();
    q.external.imdb_id = None;
    assert!(client.search(&q, 42, &cancel).await.unwrap().is_empty());
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn redirects_http_errors_404_and_bounded_bodies() {
    let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    chunked.extend_from_slice(format!("{:x}\r\n", BODY_LIMIT + 1).as_bytes());
    chunked.extend_from_slice(&vec![b'x'; BODY_LIMIT + 1]);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    for (reply, expected) in [
        (b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/private\r\nContent-Length: 0\r\n\r\n".to_vec(), Err(Error::Http(302))),
        (b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(), Ok(Vec::new())),
        (b"HTTP/1.1 429 Rate Limited\r\nContent-Length: 0\r\n\r\n".to_vec(), Err(Error::RateLimited)),
        (b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(), Err(Error::Http(503))),
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BODY_LIMIT + 1).into_bytes(), Err(Error::TooLarge)),
        (chunked, Err(Error::TooLarge)),
        (response(b"not json"), Err(Error::InvalidResponse)),
    ] {
        let server = Server::start(vec![reply]).await;
        assert_eq!(tokio::time::timeout(Duration::from_secs(3),
            server.client().search(&query(), 42, &CancellationToken::new())).await.unwrap(), expected);
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_and_timeout_cover_stalled_body() {
    for cancel_in_flight in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let accepted = Arc::new(tokio::sync::Notify::new());
        let notify = accepted.clone();
        let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n[")
                .await
                .unwrap();
            notify.notify_one();
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(socket);
        });
        let client = Client {
            http: http_builder().no_proxy().build().unwrap(),
            fixture_origins: Some(vec![origin]),
            timeout: Duration::from_millis(150),
        };
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let canceller = tokio::spawn(async move {
            accepted.notified().await;
            if cancel_in_flight {
                token.cancel();
            }
        });
        let result =
            tokio::time::timeout(Duration::from_secs(1), client.search(&query(), 42, &cancel))
                .await
                .unwrap();
        assert_eq!(
            result,
            Err(if cancel_in_flight {
                Error::Cancelled
            } else {
                Error::Timeout
            })
        );
        canceller.await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn explicit_proxy_targets_fixed_host_without_fallback_or_secret_debug() {
    for proxy in [
        "http://user:secret@127.0.0.1:9",
        "http://127.0.0.1:9/path",
        "invalid",
    ] {
        assert!(matches!(Client::new(Some(proxy)), Err(Error::InvalidProxy)));
    }
    let server = Server::start(vec![
        b"HTTP/1.1 502 Fixture\r\nContent-Length: 0\r\n\r\n"
            .to_vec();
        4
    ])
    .await;
    let client = Client::new(Some(server.origin.as_str())).unwrap();
    assert_eq!(
        client.search(&query(), 42, &CancellationToken::new()).await,
        Err(Error::Network)
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    for (request, host) in requests.iter().zip(Index::Popcorn.hosts()) {
        assert!(request.starts_with(&format!("CONNECT {host}:443 HTTP/1.1\r\n")));
    }
    assert!(!format!("{client:?}").contains(server.origin.as_str()));
    assert!(!format!("{:?}", parse_value(fixture()).unwrap()).contains("magnet:"));
}

#[tokio::test]
async fn mirrors_retry_transient_errors_only_and_stop_at_first_valid_answer() {
    for (status, retry) in [
        (503, true),
        (401, false),
        (403, false),
        (429, false),
        (404, false),
        (200, false),
    ] {
        let first = Server::start(vec![if status == 200 {
            response(b"[]")
        } else {
            format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .into_bytes()
        }])
        .await;
        let second = Server::start(vec![response(FIXTURE.as_bytes())]).await;
        let mut client = first.client();
        client.fixture_origins = Some(vec![first.origin.clone(), second.origin.clone()]);
        let rows = client.search(&query(), 42, &CancellationToken::new()).await;
        assert_eq!(first.requests().len(), 1);
        assert_eq!(second.requests().len(), usize::from(retry));
        match status {
            503 => assert_eq!(rows.unwrap().len(), 2),
            404 | 200 => assert!(rows.unwrap().is_empty()),
            429 => assert_eq!(rows, Err(Error::RateLimited)),
            _ => assert_eq!(rows, Err(Error::Http(status))),
        }
    }
}

#[test]
fn authorized_routes_and_redirects_preserve_identity_and_media_scope() {
    let mut q = query();
    let url = Index::Yts.url("yts.lt", &q).unwrap();
    assert_eq!(
        url.as_str(),
        "https://yts.lt/api/v2/movie_details.json?imdb_id=tt0133093"
    );
    assert!(
        canonical_yts_redirect(
            &url,
            Some("https://yts.pw/api/v2/movie_details.json?imdb_id=tt0133093")
        )
        .is_some()
    );
    for location in [
        "http://yts.pw/api/v2/movie_details.json?imdb_id=tt0133093",
        "https://yts.pw/other",
        "https://other.example/api/v2/movie_details.json?imdb_id=tt0133093",
        "https://yts.pw/api/v2/movie_details.json?imdb_id=tt9999999",
    ] {
        assert!(canonical_yts_redirect(&url, Some(location)).is_none());
    }
    q.kind = MediaKind::Series;
    q.episode = EpisodeNumber::new(0, 1);
    assert!(!Index::Yts.supports(&q));
    assert!(Index::Popcorn.supports(&q));
    assert_eq!(
        Index::Popcorn.url("yrkde.link", &q).unwrap().path(),
        "/show/tt0133093"
    );
}
