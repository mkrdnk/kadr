use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

// Reduced live videoinfo snapshot: descriptions omitted; identity/categories,
// rights, original dimensions/size, and every derivative retained verbatim.
const FIXTURE: &str = include_str!("fixtures/videoinfo.json");

fn fixture(index: usize) -> Value {
    let mut data: Value = serde_json::from_str(FIXTURE).unwrap();
    data["query"]["pages"] = serde_json::json!([data["query"]["pages"][index].clone()]);
    data
}

fn bytes(data: &Value) -> Vec<u8> {
    serde_json::to_vec(data).unwrap()
}

fn query(work: &Work) -> MediaQuery {
    MediaQuery {
        media_id: "trakt:movie:1234".into(),
        kind: MediaKind::Movie,
        title: work.title.into(),
        original_title: None,
        year: Some(work.year),
        external: identity(work).external,
        episode: None,
    }
}

#[test]
fn matching_uses_domain_rules_and_canonical_identity() {
    for work in WORKS {
        let mut q = query(work);
        assert!(supports(&q));
        q.external = ExternalMetadata::default();
        q.title = work.title.to_uppercase().replace(' ', "...");
        assert!(supports(&q));
        q.year = None;
        assert!(!supports(&q));
        q.year = Some(work.year + 1);
        assert!(!supports(&q));
        q = query(work);
        q.external.imdb_id = Some("tt9999999".into());
        assert!(!supports(&q));
        q = query(work);
        q.external.tmdb_id = Some(9999999);
        assert!(!supports(&q));
        q = query(work);
        q.kind = MediaKind::Series;
        assert!(!supports(&q));
        q = query(work);
        q.episode = kadr_core::torrent::EpisodeNumber::new(1, 1);
        assert!(!supports(&q));
        q = query(work);
        q.title = "Untrusted display name".into();
        assert!(supports(&q)); // IDs, not the display name, bind this query.
    }
    let mut q = query(&WORKS[0]);
    q.external = ExternalMetadata::default();
    q.title.push_str(" sequel");
    assert!(!supports(&q));
}

#[test]
fn real_shaped_rows_have_distinct_encoding_ids_and_verified_credits() {
    for (index, work) in WORKS.iter().enumerate() {
        let sources = parse(&bytes(&fixture(index)), work, 42).unwrap();
        assert_eq!(sources.len(), if index == 0 { 3 } else { 1 });
        let ids: HashSet<_> = sources.iter().map(|source| &source.id).collect();
        assert_eq!(ids.len(), sources.len());
        for source in sources {
            assert!(accepts(&source));
            assert_eq!(credit(&source), Some(work.credit));
            assert_eq!(source.license.as_deref(), Some(work.credit));
            assert!(work.credit.contains(&page(work)));
            assert!(work.credit.contains(work.license_url));
            assert_eq!(source.title, work.title);
            assert_eq!(source.year, Some(work.year));
            assert_eq!(source.external, identity(work).external);
            assert_eq!(source.fetched_at, 42);
            assert_eq!(source.size, None);
            assert_eq!(source.seeds, None);
            assert_eq!(source.peers, None);
            assert_eq!(source.origins, vec![ID.to_owned()]);
            assert!(
                matches!(&source.locator, Locator::Http(url) if url.ends_with(".webm") && !url.contains('?'))
            );
        }
    }
}

#[test]
fn current_revision_is_usable_without_fetching_prior_file_history() {
    let mut data = fixture(1);
    data.as_object_mut().unwrap().remove("batchcomplete");
    data["continue"] = serde_json::json!({"vistart":"2009-05-04T13:11:23Z", "continue":"||"});
    let rows = parse(&bytes(&data), &WORKS[1], 42).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(accepts(&rows[0]));
}

#[test]
fn rights_are_required_exact_and_unambiguous() {
    for (index, work) in WORKS.iter().enumerate() {
        for key in [
            "Artist",
            "Attribution",
            "License",
            "LicenseShortName",
            "LicenseUrl",
            "UsageTerms",
            "AttributionRequired",
            "Copyrighted",
            "Restrictions",
        ] {
            let mut data = fixture(index);
            data["query"]["pages"][0]["videoinfo"][0]["extmetadata"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert_eq!(
                parse(&bytes(&data), work, 1),
                Err(Error::InvalidResponse),
                "{key}"
            );
            for invalid in [
                serde_json::json!("unknown"),
                serde_json::json!([work.license_url, "https://example.org"]),
                Value::Null,
            ] {
                let mut data = fixture(index);
                data["query"]["pages"][0]["videoinfo"][0]["extmetadata"][key]["value"] = invalid;
                assert_eq!(
                    parse(&bytes(&data), work, 1),
                    Err(Error::InvalidResponse),
                    "{key}"
                );
            }
            let mut data = fixture(index);
            data["query"]["pages"][0]["videoinfo"][0]["extmetadata"][key]["source"] =
                serde_json::json!("untrusted-provider");
            assert_eq!(
                parse(&bytes(&data), work, 1),
                Err(Error::InvalidResponse),
                "{key}"
            );
        }
        for (key, value) in [
            ("LicenseAlternative", "CC BY 4.0"),
            ("Permission", "non-commercial only"),
            ("Categories", "Files with disputed copyright"),
        ] {
            let mut data = fixture(index);
            data["query"]["pages"][0]["videoinfo"][0]["extmetadata"][key] =
                serde_json::json!({"value": value, "source": "commons-desc-page"});
            assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
        }
        let mut data = fixture(index);
        data["query"]["pages"][0]["videoinfo"][0]["extmetadata"]["Categories"] =
            serde_json::json!({"value": ["Files with disputed copyright"]});
        assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    }
    for raw in [
        br#"{"License":1,"License":2}"#.as_slice(),
        br#"{"License":1,"\u004cicense":2}"#.as_slice(),
        br#"{"nested":{"a":1,"a":2}}"#.as_slice(),
    ] {
        assert_eq!(unique_keys(raw), Err(Error::InvalidResponse));
    }
    unique_keys(br#"{"nested":{"a":1},"a":2,"string":"[\"x\"]"}"#).unwrap();
}

#[test]
fn unsafe_identity_and_progressive_paths_fail_closed() {
    let work = &WORKS[0];
    let base = fixture(0);
    for pointer in [
        "/query/pages/0/pageid",
        "/query/pages/0/ns",
        "/query/pages/0/title",
        "/query/pages/0/imagerepository",
        "/query/pages/0/videoinfo/0/url",
        "/query/pages/0/videoinfo/0/descriptionurl",
        "/query/pages/0/videoinfo/0/mime",
    ] {
        let mut data = base.clone();
        *data.pointer_mut(pointer).unwrap() = serde_json::json!("wrong");
        assert_eq!(
            parse(&bytes(&data), work, 1),
            Err(Error::InvalidResponse),
            "{pointer}"
        );
    }
    let good_url = derivative_url(work, "240p.vp9.webm");
    for url in [
        good_url.replace("https:", "http:"),
        good_url.replace("upload.wikimedia.org", "upload.wikimedia.org.evil.test"),
        good_url.replace("https://", "https://user:password@"),
        good_url.replace("https://", "https://127.0.0.1/"),
        good_url.replace("/transcoded/", "/archive/"),
        good_url.replace("c/c0/", "d/d5/"),
        good_url.replace(".240p.", ".720p."),
        format!("{good_url}?token=private"),
        format!("{good_url}#fragment"),
        good_url.replace("upload.wikimedia.org", "upload.wikimedia.org:443"),
        good_url.replace("/transcoded/", "/transcoded/../transcoded/"),
    ] {
        let mut data = base.clone();
        data["query"]["pages"][0]["videoinfo"][0]["derivatives"][1]["src"] = serde_json::json!(url);
        assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    }
    for (key, value) in [
        ("height", serde_json::json!(1080)),
        ("width", serde_json::json!(0)),
        ("type", serde_json::json!("audio/webm")),
    ] {
        let mut data = base.clone();
        data["query"]["pages"][0]["videoinfo"][0]["derivatives"][1][key] = value;
        assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    }
}

#[test]
fn cache_cannot_authorize_another_work_path_or_rights_label() {
    let source = parse(&bytes(&fixture(0)), &WORKS[0], 42).unwrap().remove(0);
    let mut variants = Vec::new();
    let mut s = source.clone();
    s.provider = "other".into();
    variants.push(s);
    let mut s = source.clone();
    s.title = WORKS[1].title.into();
    variants.push(s);
    let mut s = source.clone();
    s.year = Some(2009);
    variants.push(s);
    let mut s = source.clone();
    s.external.tmdb_id = Some(999);
    variants.push(s);
    let mut s = source.clone();
    s.provider_item_id = Some("40108642:audio.webm".into());
    variants.push(s);
    let mut s = source.clone();
    s.locator = Locator::Http(format!(
        "{}?key=secret",
        derivative_url(&WORKS[0], "240p.vp9.webm")
    ));
    variants.push(s);
    for invalid in variants {
        assert!(!accepts(&invalid));
        assert_eq!(credit(&invalid), None);
    }
    for mutate in [
        |s: &mut MediaSource| s.license = None,
        |s: &mut MediaSource| s.license = Some("CC0".into()),
        |s: &mut MediaSource| s.size = Some(2964839055),
        |s: &mut MediaSource| s.id = "invented".into(),
        |s: &mut MediaSource| s.resolution = Some(1080),
        |s: &mut MediaSource| s.video_codec = Some("AV1".into()),
        |s: &mut MediaSource| s.origins.push("https://evil.test".into()),
    ] {
        let mut invalid = source.clone();
        mutate(&mut invalid);
        assert!(!accepts(&invalid));
    }
}

#[test]
fn response_and_collection_bounds() {
    let work = &WORKS[0];
    assert_eq!(
        parse(&vec![b' '; BODY_LIMIT + 1], work, 1),
        Err(Error::TooLarge)
    );
    assert_eq!(parse(b"not json", work, 1), Err(Error::InvalidResponse));
    let mut data = fixture(0);
    data["error"] = serde_json::json!({"code":"ratelimited"});
    assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    let mut data: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    data = fixture(0);
    let derivative = data["query"]["pages"][0]["videoinfo"][0]["derivatives"][1].clone();
    data["query"]["pages"][0]["videoinfo"][0]["derivatives"] =
        serde_json::json!(vec![derivative.clone(); DERIVATIVE_LIMIT + 1]);
    assert_eq!(parse(&bytes(&data), work, 1), Err(Error::TooLarge));
    data["query"]["pages"][0]["videoinfo"][0]["derivatives"] =
        serde_json::json!([derivative.clone(), derivative]);
    assert_eq!(parse(&bytes(&data), work, 1), Err(Error::InvalidResponse));
    let nested = (0..34).fold(Value::Null, |v, _| serde_json::json!([v]));
    assert_eq!(
        bounded(&nested, &mut NODE_LIMIT.clone(), 0),
        Err(Error::TooLarge)
    );
    assert_eq!(
        bounded(&serde_json::json!([1, 2, 3]), &mut 2, 0),
        Err(Error::TooLarge)
    );
}

async fn server(
    status: u16,
    headers: &str,
    body: Vec<u8>,
    delay: Duration,
) -> (Client, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let headers = headers.to_owned();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        tokio::time::sleep(delay).await;
        let reply = format!(
            "HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        );
        let _ = stream.write_all(reply.as_bytes()).await;
        let _ = stream.write_all(&body).await;
        String::from_utf8(request).unwrap()
    });
    let mut client = Client::new(None).unwrap();
    client.http = http_builder().no_proxy().build().unwrap();
    client.fixture_origin = Some(Url::parse(&origin).unwrap());
    (client, task)
}

#[tokio::test]
async fn real_get_uses_only_fixed_pageid_and_no_credentials() {
    let (client, task) = server(200, "", bytes(&fixture(0)), Duration::ZERO).await;
    let mut q = query(&WORKS[0]);
    q.title = "private title must not leave device".into();
    q.media_id = "trakt:movie:private".into();
    let sources = client
        .search(&q, 42, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(sources.len(), 3);
    assert!(
        sources
            .iter()
            .all(|source| accepts(source) && source.match_score == 100)
    );
    assert_eq!(sources[0].title, "Big Buck Bunny");
    let request = task.await.unwrap();
    let url = Url::parse(&format!(
        "https://commons.wikimedia.org{}",
        request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
    ))
    .unwrap();
    assert_eq!(url.path(), "/w/api.php");
    let params: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(params.len(), 7);
    for (key, value) in [
        ("action", "query"),
        ("prop", "videoinfo"),
        ("format", "json"),
        ("formatversion", "2"),
        ("pageids", "40108642"),
        ("vilimit", "1"),
        ("viprop", "url|size|mime|extmetadata|derivatives"),
    ] {
        assert_eq!(params.get(key).map(|s| s.as_ref()), Some(value));
    }
    assert!(!request.contains("private"));
    let lower = request.to_ascii_lowercase();
    assert!(lower.starts_with("get "));
    assert!(lower.contains("https://github.com/mkrdnk/kadr"));
    assert!(!lower.contains("authorization"));
    assert!(!lower.contains("cookie:"));
}

#[tokio::test]
async fn errors_redirects_and_cancellation_are_bounded_and_redacted() {
    for (status, expected) in [
        (429, Error::RateLimited),
        (503, Error::Http(503)),
        (302, Error::Http(302)),
    ] {
        let (client, task) = server(
            status,
            "Location: http://127.0.0.1:1/private\r\n",
            b"private secret".to_vec(),
            Duration::ZERO,
        )
        .await;
        let error = client
            .search(&query(&WORKS[0]), 1, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("private"));
        task.await.unwrap(); // One request only: no retries or redirect following.
    }
    let (client, task) = server(200, "", bytes(&fixture(0)), Duration::from_secs(5)).await;
    let cancel = CancellationToken::new();
    let q = query(&WORKS[0]);
    let request = client.search(&q, 1, &cancel);
    let stop = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(request, stop);
    assert_eq!(result, Err(Error::Cancelled));
    task.abort();
}

#[tokio::test]
async fn unsupported_cancelled_and_construction_do_no_io() {
    let (client, task) = server(200, "", Vec::new(), Duration::ZERO).await;
    let mut q = query(&WORKS[0]);
    q.title = "Unrecognized private movie".into();
    q.external = ExternalMetadata::default();
    assert!(
        client
            .search(&q, 1, &CancellationToken::new())
            .await
            .unwrap()
            .is_empty()
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        client.search(&query(&WORKS[0]), 1, &cancel).await,
        Err(Error::Cancelled)
    );
    assert_eq!(
        client
            .search(&query(&WORKS[0]), -1, &CancellationToken::new())
            .await,
        Err(Error::InvalidQuery)
    );
    assert!(!task.is_finished());
    task.abort();
    for proxy in ["http://127.0.0.1:9", "socks5h://127.0.0.1:9"] {
        Client::new(Some(proxy)).unwrap();
    }
    for proxy in ["http://user:secret@localhost:9", "file:///private", "bad"] {
        assert_eq!(Client::new(Some(proxy)).unwrap_err(), Error::InvalidProxy);
    }
}

#[tokio::test]
async fn http_body_limit_and_invalid_json_are_enforced() {
    for (body, expected) in [
        (vec![b' '; BODY_LIMIT + 1], Error::TooLarge),
        (
            b"not JSON; private server detail".to_vec(),
            Error::InvalidResponse,
        ),
    ] {
        let (client, task) = server(200, "", body, Duration::ZERO).await;
        assert_eq!(
            client
                .search(&query(&WORKS[0]), 1, &CancellationToken::new())
                .await,
            Err(expected)
        );
        task.await.unwrap();
    }
    // No Content-Length: exercise the accumulated chunk limit as well.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let _ = stream.write_all(&vec![b' '; BODY_LIMIT + 1]).await;
    });
    let mut client = Client::new(None).unwrap();
    client.http = http_builder().no_proxy().build().unwrap();
    client.fixture_origin = Some(origin);
    assert_eq!(
        client
            .search(&query(&WORKS[0]), 1, &CancellationToken::new())
            .await,
        Err(Error::TooLarge)
    );
    task.await.unwrap();
}

#[tokio::test]
async fn explicit_proxy_has_no_direct_fallback() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = Client::new(Some(&format!("http://{}", proxy.local_addr().unwrap()))).unwrap();
    client.fixture_origin =
        Some(Url::parse(&format!("http://{}", origin.local_addr().unwrap())).unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
            assert!(request.len() < 8192);
        }
        stream
            .write_all(b"HTTP/1.1 502 Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        String::from_utf8(request).unwrap()
    });
    assert_eq!(
        client
            .search(&query(&WORKS[0]), 1, &CancellationToken::new())
            .await,
        Err(Error::Http(502))
    );
    let request = task.await.unwrap();
    assert!(request.starts_with("GET http://127.0.0.1:"));
    assert!(!request.to_ascii_lowercase().contains("authorization"));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), origin.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stalled_transport_timeout_is_redacted() {
    let (mut client, task) = server(200, "", Vec::new(), Duration::from_secs(5)).await;
    // Accelerated request timeout exercises the real reqwest error mapping;
    // production retains the fixed 20-second request + whole-operation limits.
    client.http = http_builder()
        .no_proxy()
        .timeout(Duration::from_millis(25))
        .build()
        .unwrap();
    assert_eq!(
        client
            .search(&query(&WORKS[0]), 1, &CancellationToken::new())
            .await,
        Err(Error::Timeout)
    );
    task.abort();
}

#[tokio::test]
#[ignore = "Explicit live Commons API request; no media download"]
async fn live_verified_files() {
    let client = Client::new(None).unwrap();
    for work in WORKS {
        let sources = client
            .search(&query(work), 1, &CancellationToken::new())
            .await
            .unwrap();
        assert!(!sources.is_empty());
        assert!(sources.iter().all(accepts));
    }
}
