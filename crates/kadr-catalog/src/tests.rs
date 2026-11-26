use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn item(license: Value) -> Value {
    json!({
        "metadata": {
            "identifier":"test-film", "mediatype":"movies",
            "title":"Test <b>film</b>", "date":"1935-01-01",
            "description":["<p>A film &amp; its story.</p>"],
            "creator":"A Creator", "language":"English", "licenseurl":license
        },
        "files":[
            {"name":"movie file.mp4","format":"h.264"},
            {"name":"subdir/movie.webm","format":"WebM"},
            {"name":"private.mp4","private":"true"},
            {"name":"movie.mp3"}, {"name":"movie.torrent"},
            {"name":"../escape.mp4"}, {"name":"./escape.mp4"},
            {"name":"a//escape.mp4"}, {"name":"https://evil.example/movie.mp4"},
            {"name":"conflict.mp4","licenseurl":"https://example.com/rights"}
        ]
    })
}

#[test]
fn archive_parsing_and_safe_sources() {
    let data = item(json!("http://creativecommons.org/publicdomain/mark/1.0/"));
    let media = archive_media(&data["metadata"]).unwrap();
    assert_eq!(media.id, "archive:test-film");
    assert_eq!(media.title, "Test film");
    assert_eq!(media.description, "A film & its story.");
    assert_eq!(media.year, Some(1935));
    assert_eq!(media.kind, MediaKind::Movie);
    let sources = archive_sources("test-film", &data);
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources[0].url,
        "https://archive.org/download/test-film/movie%20file.mp4"
    );
    assert_eq!(
        sources[1].url,
        "https://archive.org/download/test-film/subdir/movie.webm"
    );
    assert!(sources[0].origin.contains("A Creator"));
    assert!(
        sources[0]
            .origin
            .contains("https://archive.org/details/test-film")
    );
    assert!(ManualSource::parse_link(&sources[0].url).is_ok());
    assert!(!format!("{:?}", sources[0]).contains("/download/"));
}

#[test]
fn license_gate_is_exact_and_fail_closed() {
    for accepted in [
        "http://creativecommons.org/publicdomain/mark/1.0/",
        "https://creativecommons.org/publicdomain/zero/1.0/",
        "https://creativecommons.org/licenses/by/4.0/",
        "https://creativecommons.org/licenses/by-sa/3.0/",
    ] {
        assert!(permitted_license(&json!(accepted)).is_some(), "{accepted}");
    }
    for rejected in [
        "",
        "public domain",
        "CC BY",
        "https://evil.example/licenses/by/4.0/",
        "https://creativecommons.org.evil.example/licenses/by/4.0/",
        "https://creativecommons.org@evil.example/licenses/by/4.0/",
        "https://user@creativecommons.org/licenses/by/4.0/",
        "https://creativecommons.org:444/licenses/by/4.0/",
        "https://creativecommons.org/licenses/by-nc/4.0/",
        "https://creativecommons.org/licenses/by-nd/4.0/",
        "https://creativecommons.org/licenses/by/4.0/?anything=1",
        "https://creativecommons.org/licenses/by/4.0/#anything",
        "https://creativecommons.org/licenses/by/4.0/evil",
    ] {
        assert!(
            archive_sources("test-film", &item(json!(rejected))).is_empty(),
            "{rejected}"
        );
    }
    assert!(archive_sources("test-film", &item(Value::Null)).is_empty());
    assert!(
        archive_sources(
            "test-film",
            &item(json!(["https://creativecommons.org/licenses/by/4.0/"]))
        )
        .is_empty()
    );
    for field in ["is_dark", "access-restricted-item"] {
        let mut data = item(json!("https://creativecommons.org/publicdomain/zero/1.0/"));
        if field == "is_dark" {
            data[field] = json!(true);
        } else {
            data["metadata"][field] = json!("true");
        }
        assert!(archive_sources("test-film", &data).is_empty());
    }
}

#[test]
fn identifiers_cannot_escape_endpoints_or_storage() {
    for accepted in ["archive:Test-film_1935.1", "tvmaze:335"] {
        assert!(provider_id(accepted).is_ok());
        assert!(
            accepted
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        );
    }
    for rejected in [
        "archive:../evil",
        "archive:a/b",
        "archive:a?x=y",
        "archive:%2f",
        "archive:.hidden",
        "archive:",
        "tvmaze:0",
        "tvmaze:001",
        "tvmaze:1?foo",
        "tvmaze:-1",
        "imdb:tt1",
    ] {
        assert!(
            matches!(provider_id(rejected), Err(Error::InvalidId)),
            "{rejected}"
        );
    }
}

#[test]
fn tvmaze_series_and_episodes() {
    let data = json!({"id":335,"name":"Sherlock","summary":"<p>A <b>detective</b>.</p>",
        "premiered":"2010-07-25",
        "image":{"medium":"https://static.tvmaze.com/uploads/images/medium_portrait/1.jpg"}});
    let media = tvmaze_media(&data).unwrap();
    assert_eq!(media.id, "tvmaze:335");
    assert_eq!(media.kind, MediaKind::Series);
    assert_eq!(media.year, Some(2010));
    assert_eq!(media.description, "A detective.");
    assert_eq!(media.license.as_deref(), Some(TV_LICENSE));
    assert!(media.attribution.contains("TVmaze"));
    let episode = tvmaze_episode(&json!({"season":2,"number":1,"name":"A Scandal",
        "summary":"<p>A story.</p>"}))
    .unwrap();
    assert_eq!((episode.season, episode.number), (2, 1));
    assert!(tvmaze_episode(&json!({"season":1,"number":null,"name":"Unknown special"})).is_none());
    assert!(tvmaze_episode(&json!({"season":70000,"number":1})).is_none());
    let mut malicious = data;
    malicious["image"]["medium"] = json!("https://evil.example/tracker.jpg");
    assert!(tvmaze_media(&malicious).unwrap().poster_url.is_none());
}

#[test]
fn display_text_is_bounded_and_optional_fields_are_safe() {
    assert_eq!(plain(&json!("a".repeat(100_000)), 16_000).len(), 16_000);
    assert_eq!(plain(&Value::Null, 100), "");
    let media = tvmaze_media(&json!({"id":1,"name":"No fields"})).unwrap();
    assert_eq!(media.year, None);
    assert_eq!(media.poster_url, None);
    assert_eq!(media.description, "");
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
async fn transport_status_json_and_body_limits() {
    let client = Client::new(Options::default()).unwrap();
    let cancel = CancellationToken::new();
    for (response, expected) in [
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
        assert_eq!(client.json(url, &cancel).await.unwrap_err(), expected);
        task.await.unwrap();
    }
    // Also bound chunked bodies: do not rely on a truthful Content-Length header.
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        BODY_LIMIT + 1
    )
    .into_bytes();
    response.extend(vec![b' '; BODY_LIMIT + 1]);
    response.extend_from_slice(b"\r\n0\r\n\r\n");
    let (url, task) = server(response).await;
    assert_eq!(
        client.json(url, &cancel).await.unwrap_err(),
        Error::TooLarge
    );
    task.await.unwrap();
    let (url, task) = server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_vec()).await;
    assert_eq!(client.json(url, &cancel).await.unwrap(), json!({}));
    task.await.unwrap();
}

#[tokio::test]
async fn cancellation_and_disabled_switch_stop_requests() {
    let disabled = Client::new(Options { enabled: false }).unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        disabled.search("query", &cancel).await.unwrap_err(),
        Error::Disabled
    );
    let client = Client::new(Options::default()).unwrap();
    cancel.cancel();
    assert_eq!(
        client.search("private query", &cancel).await.unwrap_err(),
        Error::Cancelled
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let cancel = CancellationToken::new();
    let pending = client.json(url, &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("unexpected early response: {result:?}"),
        connection = listener.accept() => { let _connection = connection.unwrap(); cancel.cancel(); }
    }
    assert_eq!(pending.await.unwrap_err(), Error::Cancelled);
    assert_eq!(
        client
            .search("", &CancellationToken::new())
            .await
            .unwrap_err(),
        Error::InvalidQuery
    );
}

/// Independent of deterministic tests. Makes explicit live provider requests and
/// reads a small first chunk of a real permitted video, without saving the movie.
#[tokio::test]
#[ignore = "live network, opt in with cargo test -p kadr-catalog live_catalog -- --ignored --nocapture"]
async fn live_catalog() {
    let client = Client::new(Options::default()).unwrap();
    let cancel = CancellationToken::new();
    let films = client
        .search_archive("night of the living dead", &cancel)
        .await
        .unwrap();
    assert!(!films.is_empty(), "Archive search returned no movies");
    let results = client.search("Sherlock", &cancel).await.unwrap();
    let series = results
        .iter()
        .find(|m| m.id == "tvmaze:335")
        .expect("live TVmaze result");
    let details = client.details(&series.id, &cancel).await.unwrap();
    assert!(
        details
            .episodes
            .iter()
            .any(|e| e.season == 1 && e.number == 1)
    );
    assert!(
        client
            .find_sources(&details, details.episodes.first(), &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    // Stable test target, not the application catalog: normal searches are fully dynamic.
    let movie = client
        .details("archive:NightOfTheLivingDead1968-Restored", &cancel)
        .await
        .unwrap();
    let sources = client.find_sources(&movie, None, &cancel).await.unwrap();
    assert!(
        !sources.is_empty(),
        "live explicit-license item has no playable files"
    );
    let probe = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let host = attempt.url().host_str().unwrap_or("");
            if attempt.previous().len() < 5
                && attempt.url().scheme() == "https"
                && (host == "archive.org" || host.ends_with(".archive.org"))
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .unwrap();
    let mut response = probe
        .get(&sources[0].url)
        .header("Range", "bytes=0-31")
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "permitted source is not accessible: {}",
        response.status()
    );
    let chunk = response.chunk().await.unwrap().unwrap();
    assert!(!chunk.is_empty());
    println!(
        "Live Archive search: {} movies; TVmaze details: {} episodes; licensed source candidates: {}; video byte probe: HTTP {}",
        films.len(),
        details.episodes.len(),
        sources.len(),
        response.status()
    );
}
