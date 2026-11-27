use super::*;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

const CAPS: &[u8] = include_bytes!("fixtures/caps.xml");
const SEARCH: &[u8] = include_bytes!("fixtures/search.xml");
const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// At most eight requests, bounded headers and per-connection deadline.
/// Drop aborts even fixtures waiting for an intentionally rejected request.
struct Server {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start(responses: Vec<Vec<u8>>) -> Self {
        assert!(responses.len() <= 8);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        assert!(header.len() < 16_384);
                        header.push(socket.read_u8().await.unwrap());
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(header).unwrap());
                    socket.write_all(&response).await.unwrap();
                })
                .await
                .unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }

    fn client(&self) -> Client {
        let mut client = Client::new(&self.endpoint, Some("fixture-secret".into()), None).unwrap();
        client.http = reqwest::Client::builder()
            .no_proxy()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        client
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn response(body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn query(kind: MediaKind) -> MediaQuery {
    MediaQuery {
        media_id: "trakt:movie:1".into(),
        kind,
        title: "Example".into(),
        original_title: None,
        year: Some(2020),
        external: Default::default(),
        episode: None,
    }
}

fn rss(items: &str) -> String {
    format!(
        "<rss version='2.0' xmlns:t='{TORZNAB_NS}'><channel><title>Fixture</title><link>https://example.org/</link><description>Fixture</description>{items}</channel></rss>"
    )
}

fn item(title: &str, locator: &str) -> String {
    format!("<item><title>{title}</title><t:attr name='category' value='2000'/>{locator}</item>")
}

#[test]
fn supported_parameters_select_specialized_search_or_free_text() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    let capabilities = caps(&xml(CAPS).unwrap()).unwrap();
    let mut movie = query(MediaKind::Movie);
    movie.external.imdb_id = Some("tt1234567".into());
    let url = client.search_url(&movie, &capabilities).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs.get("t").unwrap(), "movie");
    assert_eq!(pairs.get("imdbid").unwrap(), "1234567");
    assert!(!pairs.contains_key("q"));
    assert_eq!(pairs.get("limit").unwrap(), "25");

    let mut series = query(MediaKind::Series);
    series.episode = EpisodeNumber::new(1, 2);
    let url = client.search_url(&series, &capabilities).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs.get("t").unwrap(), "tvsearch");
    assert_eq!(pairs.get("q").unwrap(), "Example");
    assert_eq!(pairs.get("season").unwrap(), "1");
    assert_eq!(pairs.get("ep").unwrap(), "2");

    let doc = xml(b"<caps><searching><search available='yes' supportedParams='q'/><tv-search available='no' supportedParams='season,ep'/></searching></caps>").unwrap();
    let fallback = caps(&doc).unwrap();
    let url = client.search_url(&series, &fallback).unwrap();
    let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(pairs.get("t").unwrap(), "search");
    assert_eq!(pairs.get("q").unwrap(), "Example S01E02");
    assert!(!pairs.contains_key("season"));

    let doc = xml(b"<caps><searching><movie-search available='yes' supportedParams='genre'/></searching></caps>").unwrap();
    assert_eq!(
        client.search_url(&movie, &caps(&doc).unwrap()),
        Err(Error::UnsupportedSearch)
    );
}

#[test]
fn rss_validation_distinguishes_empty_malformed_and_api_error() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    assert!(
        client
            .sources(&xml(rss("").as_bytes()).unwrap(), "fixture", 50)
            .unwrap()
            .is_empty()
    );
    for body in [
        "<html><channel/></html>",
        "<rss><channel/></rss>",
        "<rss version='2.0'/>",
        "<rss version='2.0'><channel/><channel/></rss>",
    ] {
        assert!(matches!(
            client.sources(&xml(body.as_bytes()).unwrap(), "fixture", 50),
            Err(Error::InvalidResponse)
        ));
    }
    let many = rss(&"<item/>".repeat(RESULT_LIMIT + 1));
    assert!(matches!(
        client.sources(&xml(many.as_bytes()).unwrap(), "fixture", 50),
        Err(Error::TooLarge)
    ));
    assert!(matches!(
        xml(b"<error code='100' description='bad key'/>"),
        Err(Error::Api(100))
    ));
}

#[test]
fn fixture_preserves_pack_scope_digests_guid_and_does_not_copy_query_metadata() {
    let client = Client::new(
        "https://example.org/api",
        Some("fixture-secret".into()),
        None,
    )
    .unwrap();
    let sources = client
        .sources(&xml(SEARCH).unwrap(), "fixture", 1234)
        .unwrap();
    assert_eq!(sources.len(), 1);
    let source = &sources[0];
    assert_eq!(source.title, "Example Show");
    assert_eq!(
        source.release_title.as_deref(),
        Some("Example Show S01E02-E04 1080p")
    );
    assert_eq!(source.kind, MediaKind::Series);
    assert_eq!(source.episode, None);
    assert_eq!(source.year, None);
    assert_eq!(source.resolution, Some(1080));
    assert_eq!(source.seeds, Some(12));
    assert_eq!(source.size, Some(123456789));
    assert_eq!(source.fetched_at, 1234);
    assert!(source.provider_item_id.is_some());
    assert!(
        !source
            .provider_item_id
            .as_ref()
            .unwrap()
            .contains("fixture-secret")
    );
    assert!(!format!("{source:?}").contains("fixture-secret"));
}

#[test]
fn unsafe_items_never_expose_keys_or_cross_origin_downloads() {
    let client = Client::new(
        "https://example.org/api",
        Some("fixture-secret".into()),
        None,
    )
    .unwrap();
    let magnet_attr = format!("<t:attr name='infohash' value='{HASH}'/>");
    let mut body = item("Example fixture-secret 2020", &magnet_attr);
    body.push_str(&item("Example 2020", "<enclosure type='application/x-bittorrent' url='https://other.org/get?apikey=fixture-secret'/>"));
    body.push_str(&item("Example 2020", "<enclosure type='application/x-bittorrent' url='https://example.org/get?id=1&amp;apikey=fixture-secret'/>"));
    let text = rss(&body);
    let sources = client
        .sources(&xml(text.as_bytes()).unwrap(), "fixture", 0)
        .unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].title, "Example");
    assert_eq!(sources[0].year, Some(2020));
    let Locator::TorrentUrl(url) = &sources[0].locator else {
        panic!("expected enclosure")
    };
    assert_eq!(url, "https://example.org/get?id=1");
}

#[test]
fn release_parser_is_conservative_and_retains_no_false_single_episode() {
    let exact = release_title("Example.Show.S01E02.1080p");
    assert_eq!(exact.title, "Example Show");
    assert_eq!(exact.episode, EpisodeNumber::new(1, 2));
    for raw in [
        "Example S01",
        "Example S01E02-E04",
        "Example S01E02 E03",
        "Example 1x02-04",
    ] {
        assert!(release_title(raw).scoped, "{raw}");
        assert_eq!(release_title(raw).episode, None, "{raw}");
    }
    assert_eq!(release_title("Example.2020.1080p").year, Some(2020));
    assert_eq!(release_title("Example 1080p").title, "Example 1080p");
}

#[test]
fn movie_year_claims_and_pack_suffixes_are_consistent_in_live_and_restored_offers() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    let hash = format!("<t:attr name='infohash' value='{HASH}'/>");
    for (title, attributes) in [
        ("Example 1982 1080p", "<t:attr name='year' value='2011'/>"),
        (
            "Example 1982 1080p",
            "<t:attr name='imdbyear' value='2011'/>",
        ),
        (
            "Example 1982 1080p",
            "<t:attr name='year' value='1982'/><t:attr name='imdbyear' value='2011'/>",
        ),
        ("Example 1982 Collection", ""),
        ("Example 1982 Complete Films", ""),
        ("Example [1982] Сборник", ""),
    ] {
        let body = rss(&item(title, &format!("{hash}{attributes}")));
        assert!(
            client
                .sources(&xml(body.as_bytes()).unwrap(), "fixture", 0)
                .unwrap()
                .is_empty(),
            "{title}: {attributes}"
        );
    }
    for title in ["The Collection 2012 1080p", "Collection [2020] 1080p"] {
        let body = rss(&item(title, &hash));
        let offers = client
            .sources(&xml(body.as_bytes()).unwrap(), "fixture", 0)
            .unwrap();
        assert_eq!(offers.len(), 1, "{title}");
        assert!(client.accepts(&offers[0]));
        for raw in [
            "Collection 2011 Collection",
            "Collection 2012 Complete Films",
            "Collection S01E01",
        ] {
            let mut cached = offers[0].clone();
            cached.release_title = Some(raw.into());
            assert!(!client.accepts(&cached), "{raw}");
        }
        let mut cached = offers[0].clone();
        cached.release_title = Some("Collection 1982 1080p".into());
        assert!(!client.accepts(&cached));
    }
}

#[tokio::test]
async fn search_reads_caps_before_results_and_maps_api_failure() {
    let server = Server::start(vec![response(CAPS), response(SEARCH)]).await;
    let client = server.client();
    let mut series = query(MediaKind::Series);
    series.episode = EpisodeNumber::new(1, 2);
    let sources = tokio::time::timeout(
        Duration::from_secs(3),
        client.search(&series, "fixture", 1234, &CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sources.len(), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api?t=caps&apikey=fixture-secret "));
    assert!(requests[1].contains("t=tvsearch"));
    assert!(requests[1].contains("season=1&ep=2"));

    let server = Server::start(vec![response(
        b"<error code='100' description='fixture-secret'/>",
    )])
    .await;
    assert_eq!(
        server
            .client()
            .search(
                &query(MediaKind::Movie),
                "fixture",
                0,
                &CancellationToken::new()
            )
            .await,
        Err(Error::Api(100))
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn construction_and_errors_are_offline_and_redacted() {
    for endpoint in [
        "",
        "ftp://example.org/api",
        "http://example.org/api",
        "https://user:fixture-secret@example.org/api",
        "https://example.org/api?apikey=fixture-secret",
        "https://example.org/api#fixture-secret",
    ] {
        assert!(matches!(
            Client::new(endpoint, None, None),
            Err(Error::InvalidEndpoint)
        ));
    }
    let client = Client::new(
        "https://example.org/api",
        Some("fixture-secret".into()),
        None,
    )
    .unwrap();
    assert!(!format!("{client:?}").contains("fixture-secret"));
    for error in [
        Error::Api(100),
        Error::Http(401),
        Error::InvalidLocator,
        Error::InvalidResponse,
        Error::Network,
    ] {
        assert!(!format!("{error} {error:?}").contains("fixture-secret"));
    }
    assert!(matches!(
        Client::new("https://example.org/api", Some("\n".into()), None),
        Err(Error::InvalidApiKey)
    ));
    assert!(matches!(
        Client::new(
            "https://example.org/api",
            None,
            Some("http://u:p@example.org")
        ),
        Err(Error::InvalidProxy)
    ));
}

#[test]
fn metainfo_enclosures_preserve_the_magnet_identity_and_reject_conflicts() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    let locator = format!(
        "<enclosure type='application/x-bittorrent' url='https://example.org/get/1'/><t:attr name='magneturl' value='magnet:?xt=urn:btih:{HASH}'/>"
    );
    for (extra, valid) in [
        ("", true),
        (
            "<t:attr name='infohash' value='0123456789abcdef0123456789abcdef01234568'/>",
            false,
        ),
    ] {
        let text = rss(&item("Example 2020", &format!("{locator}{extra}")));
        let rows = client
            .sources(&xml(text.as_bytes()).unwrap(), "fixture", 1)
            .unwrap();
        assert_eq!(rows.len(), usize::from(valid));
        if valid {
            assert!(client.accepts(&rows[0]));
            assert!(matches!(rows[0].locator, Locator::TorrentUrl(_)));
            assert_eq!(rows[0].info_hash.as_deref(), Some(HASH));
            let mut changed = rows[0].clone();
            changed.locator = Locator::TorrentUrl("https://another.invalid/get/1".into());
            assert!(!client.accepts(&changed));
        }
    }
}

#[test]
fn hash_only_results_pass_the_same_authority_check_as_cached_magnets() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    let text = rss(&item(
        "Example 2020",
        &format!("<t:attr name='infohash' value='{HASH}'/>"),
    ));
    let rows = client
        .sources(&xml(text.as_bytes()).unwrap(), "fixture", 1)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].info_hash.as_deref(), Some(HASH));
    assert!(client.accepts(&rows[0]));
}

#[test]
fn caps_are_explicit_and_bounded() {
    let doc = xml(CAPS).unwrap();
    let parsed = caps(&doc).unwrap();
    assert_eq!(parsed.limit, 25);
    assert!(parsed.movie.supports("imdbid"));
    assert!(parsed.tv.supports("season"));
    assert!(!parsed.tv.supports("rid"));
    let doc = xml(b"<caps><searching><search available='yes'/></searching></caps>").unwrap();
    assert!(!caps(&doc).unwrap().search.supports("q"));
    for body in [
        "<rss/>",
        "<caps/>",
        "<caps><limits max='-1'/><searching/></caps>",
    ] {
        assert!(caps(&xml(body.as_bytes()).unwrap()).is_err());
    }
}

#[test]
fn xml_rejects_errors_malformed_dtd_body_nodes_and_depth() {
    for body in [
        "<error code='100' description='fixture-secret'/>",
        "<rss><channel><error code='201'/></channel></rss>",
    ] {
        assert!(matches!(xml(body.as_bytes()), Err(Error::Api(100 | 201))));
    }
    for body in [
        "<rss>",
        "<!DOCTYPE rss [<!ENTITY key 'fixture-secret'>]><rss>&key;</rss>",
        "<error code='fixture-secret'/>",
    ] {
        assert!(matches!(xml(body.as_bytes()), Err(Error::InvalidResponse)));
    }
    assert!(matches!(xml(&[0xff]), Err(Error::InvalidResponse)));
    assert!(matches!(
        xml(&vec![b' '; BODY_LIMIT + 1]),
        Err(Error::TooLarge)
    ));
    let deep = format!(
        "{}{}",
        "<n>".repeat(DEPTH_LIMIT + 1),
        "</n>".repeat(DEPTH_LIMIT + 1)
    );
    assert!(matches!(xml(deep.as_bytes()), Err(Error::TooLarge)));
    let many = format!("<rss>{}</rss>", "<n/>".repeat(NODE_LIMIT as usize));
    assert!(xml(many.as_bytes()).is_err());
}

#[test]
fn repeated_attributes_are_lists_and_scope_is_not_collapsed() {
    let doc = xml(SEARCH).unwrap();
    let item = doc.descendants().find(|n| named(*n, "item")).unwrap();
    assert_eq!(attrs(item, "category"), ["5000", "5040"]);
    assert_eq!(attrs(item, "episode"), ["2", "4"]);
    assert_eq!(attr(item, "episode"), None);
    assert_eq!(number::<u16>(attr(item, "season")), Some(1));
    assert_eq!(number::<u16>(Some("-1")), None);
    assert_eq!(number::<u16>(Some("65536")), None);
}

#[test]
fn locators_do_not_cache_keys_or_accept_cross_origin_credentials() {
    let client = Client::new(
        "https://example.org/api",
        Some("fixture-secret".into()),
        None,
    )
    .unwrap();
    let clean = client
        .clean_locator("https://example.org/get?id=2&APIKEY=fixture-secret#fragment")
        .unwrap();
    assert_eq!(clean.as_str(), "https://example.org/get?id=2");
    for url in [
        "http://example.org/get?id=2",
        "https://other.org/get?id=2",
        "https://example.org:444/get",
        "https://u:p@example.org/get",
        "https://example.org/get?passkey=unknown-secret",
        "https://example.org/get?id=2&authkey=other-secret",
        "https://example.org/get?id=2&api-key=other-secret",
        "https://example.org/get?id=2&torrent_pass=other-secret",
        "https://example.org/get?id=2&signature=other-secret",
        "https://example.org/fixture-secret/get",
        "https://example.org/get?id=fixture%252dsecret",
    ] {
        assert!(client.clean_locator(url).is_none(), "{url}");
    }
    for text in [
        "fixture-secret",
        "x&v=fixture%252dsecret",
        "fixture%2Dsecret",
    ] {
        assert!(!client.safe_text(text, 2000), "{text}");
    }
    assert!(client.safe_text("Example S01E02-E04 1080p", 2000));
}

#[test]
fn magnet_retains_only_validated_identity() {
    let raw =
        format!("magnet:?xt=urn:btih:{HASH}&dn=fixture-secret&tr=https%3A%2F%2Ftracker%2Fpasskey");
    let clean = magnet(&raw).unwrap();
    assert!(!clean.contains("fixture-secret"));
    assert!(!clean.contains("tracker"));
    assert!(ManualSource::parse_link(&clean).is_ok());
    assert!(magnet("magnet:?xt=urn:btih:bad").is_none());
    assert!(
        magnet(&format!(
            "magnet:?xt=urn:btih:{HASH}&xt=urn:btih:{}",
            "a".repeat(40)
        ))
        .is_none()
    );
}

#[tokio::test]
async fn metainfo_reapplies_key_and_rejects_cross_origin_without_io() {
    let server = Server::start(vec![response(b"d4:infodee")]).await;
    let client = server.client();
    let url = server.endpoint.replace("/api", "/get?id=2");
    let bytes = tokio::time::timeout(
        Duration::from_secs(2),
        client.metainfo(&url, &CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(bytes, b"d4:infodee");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("GET /get?id=2&apikey=fixture-secret HTTP/1.1"));
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    assert_eq!(
        client
            .metainfo("https://other.invalid/get", &CancellationToken::new())
            .await,
        Err(Error::InvalidLocator)
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(client.metainfo(&url, &cancel).await, Err(Error::Cancelled));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn shared_origin_never_receives_sibling_credentials() {
    let server = Server::start(vec![response(b"de"), response(b"de")]).await;
    let mut client = server.client();
    client.restrict_enclosure_key_to_api();
    let cancel = CancellationToken::new();
    client
        .metainfo(
            &format!("{}/sibling?apikey=fixture-secret", server.endpoint),
            &cancel,
        )
        .await
        .unwrap();
    client
        .metainfo(&format!("{}?t=get&id=1", server.endpoint), &cancel)
        .await
        .unwrap();
    let requests = server.requests();
    assert!(!requests[0].contains("fixture-secret"));
    assert!(requests[1].contains("apikey=fixture-secret"));
}

#[tokio::test]
async fn transport_rejects_redirects_http_oversize_and_fake_metainfo() {
    let responses = [
        (b"HTTP/1.1 302 Found\r\nLocation: https://other.invalid/get\r\nContent-Length: 0\r\n\r\n".to_vec(), Error::Http(302)),
        (b"HTTP/1.1 429 Rate Limited\r\nContent-Length: 0\r\n\r\n".to_vec(), Error::RateLimited),
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", BODY_LIMIT + 1).into_bytes(), Error::TooLarge),
        (response(b"<html>login</html>"), Error::InvalidResponse),
        (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n400001\r\n".to_vec(), Error::Network),
    ];
    for (reply, expected) in responses {
        let server = Server::start(vec![reply]).await;
        let client = server.client();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                client.metainfo(&server.endpoint, &CancellationToken::new()),
            )
            .await
            .unwrap(),
            Err(expected)
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn unknown_length_body_is_capped_while_reading() {
    let mut reply = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    reply.extend_from_slice(format!("{:x}\r\n", BODY_LIMIT + 1).as_bytes());
    reply.extend_from_slice(&vec![b'x'; BODY_LIMIT + 1]);
    reply.extend_from_slice(b"\r\n0\r\n\r\n");
    let server = Server::start(vec![reply]).await;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(3),
            server
                .client()
                .metainfo(&server.endpoint, &CancellationToken::new()),
        )
        .await
        .unwrap(),
        Err(Error::TooLarge)
    );
}

#[tokio::test]
async fn stalled_transport_times_out_or_is_cancelled_in_flight() {
    for cancel_in_flight in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let accepted = Arc::new(tokio::sync::Notify::new());
        let accepted_server = accepted.clone();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            accepted_server.notify_one();
            tokio::time::sleep(Duration::from_secs(2)).await;
            drop(socket);
        });
        let server = Server {
            endpoint,
            requests: Default::default(),
            task,
        };
        let mut client = server.client();
        client.http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let cancel = CancellationToken::new();
        let canceller = if cancel_in_flight {
            let token = cancel.clone();
            Some(tokio::spawn(async move {
                accepted.notified().await;
                token.cancel();
            }))
        } else {
            None
        };
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                client.metainfo(&server.endpoint, &cancel),
            )
            .await
            .unwrap(),
            Err(if cancel_in_flight {
                Error::Cancelled
            } else {
                Error::Timeout
            })
        );
        if let Some(task) = canceller {
            task.await.unwrap();
        }
    }
}

#[test]
fn contradictory_scope_attributes_cannot_claim_one_episode() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    for metadata in [
        "<t:attr name='season' value='1'/><t:attr name='episode' value='2'/><t:attr name='episode' value='4'/>",
        "<t:attr name='season' value='1'/><t:attr name='season' value='2'/><t:attr name='episode' value='2'/>",
        "<t:attr name='season' value='1-2'/><t:attr name='episode' value='2'/>",
    ] {
        let body = rss(&format!(
            "<item><title>Example S01E02</title><t:attr name='category' value='5000'/><t:attr name='infohash' value='{HASH}'/>{metadata}</item>"
        ));
        let sources = client
            .sources(&xml(body.as_bytes()).unwrap(), "fixture", 0)
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].episode, None);
    }
}

#[test]
fn video_dimensions_are_not_episode_markers() {
    let client = Client::new("https://example.org/api", None, None).unwrap();
    for dimensions in ["1920x1080", "1280x720", "3840x2160"] {
        let title = format!("Example.2020.{dimensions}");
        assert!(!release_title(&title).scoped);
        let body = rss(&item(
            &title,
            &format!("<t:attr name='infohash' value='{HASH}'/>"),
        ));
        let sources = client
            .sources(&xml(body.as_bytes()).unwrap(), "fixture", 0)
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].title, "Example");
    }
    assert_eq!(
        release_title("Example 1x02").episode,
        EpisodeNumber::new(1, 2)
    );
}

#[tokio::test]
async fn explicit_proxy_preserves_origin_and_credential_boundaries() {
    let body = rss(&item(
        "Example 2020",
        "<enclosure type='application/x-bittorrent' url='http://127.0.0.2/get?id=2&amp;apikey=fixture-secret'/>",
    ));
    let server = Server::start(vec![
        response(CAPS),
        response(body.as_bytes()),
        response(b"de"),
    ])
    .await;
    let proxy = server.endpoint.strip_suffix("/api").unwrap();
    let client = Client::new(
        "http://127.0.0.2/api",
        Some("fixture-secret".into()),
        Some(proxy),
    )
    .unwrap();
    let sources = tokio::time::timeout(
        Duration::from_secs(3),
        client.search(
            &query(MediaKind::Movie),
            "fixture",
            0,
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let Locator::TorrentUrl(url) = &sources[0].locator else {
        panic!("expected metainfo")
    };
    assert_eq!(url, "http://127.0.0.2/get?id=2");
    assert_eq!(
        client
            .metainfo("http://other.invalid/get", &CancellationToken::new())
            .await,
        Err(Error::InvalidLocator)
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            client.metainfo(url, &CancellationToken::new())
        )
        .await
        .unwrap()
        .unwrap(),
        b"de"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].starts_with("GET http://127.0.0.2/get?id=2&apikey=fixture-secret "));
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("other.invalid")
                && !request.to_ascii_lowercase().contains("authorization:"))
    );
}
