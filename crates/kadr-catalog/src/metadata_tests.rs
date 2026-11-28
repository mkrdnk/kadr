use super::*;
use kadr_core::catalog::RatingSource;

#[test]
fn extended_metadata_is_optional_and_never_inferred() {
    let empty = trakt::media(&movie(1), MediaKind::Movie).unwrap();
    assert_eq!(empty.runtime_minutes, None);
    assert!(empty.genres.is_empty());
    assert_eq!(empty.certification, None);
    assert!(empty.external_ratings.is_empty());

    let mut data = movie(1);
    data["runtime"] = json!(155);
    data["genres"] = json!(["science-fiction", "drama", "drama", 7, "", "bad&slug"]);
    data["certification"] = json!("PG-13");
    // External ratings must come from the separate official endpoint.
    data["imdb"] = json!({"rating":9.9,"votes":100});
    let media = trakt::media(&data, MediaKind::Movie).unwrap();
    assert_eq!(media.runtime_minutes, Some(155));
    assert_eq!(media.genres, ["science-fiction", "drama"]);
    assert_eq!(media.certification.as_deref(), Some("PG-13"));
    assert!(media.external_ratings.is_empty());
    assert_eq!(media.rating_tenths, Some(83));

    for runtime in [
        Value::Null,
        json!(0),
        json!(-1),
        json!(1.5),
        json!("90"),
        json!(65_536),
    ] {
        data["runtime"] = runtime;
        assert_eq!(
            trakt::media(&data, MediaKind::Movie)
                .unwrap()
                .runtime_minutes,
            None
        );
    }
    for certification in [
        Value::Null,
        json!(""),
        json!(7),
        json!("a".repeat(100)),
        json!("PG\n13"),
    ] {
        data["certification"] = certification;
        assert_eq!(
            trakt::media(&data, MediaKind::Movie).unwrap().certification,
            None
        );
    }
}

fn ratings() -> Value {
    // Official extended=all shape. No fixture invents inline media ratings or
    // borrows the top-level Trakt score as a missing external provider score.
    json!({
        "rating":8.26, "votes":100, "distribution":{"10":50},
        "trakt":{"rating":8.26,"votes":100,"distribution":{}},
        "imdb":{"rating":8.34,"votes":1200,"link":"https://www.imdb.com/title/tt1160419"},
        "tmdb":{"rating":8.1,"votes":800,"link":null},
        "metascore":{"rating":74,"link":null},
        "rotten_tomatoes":{
            "rating":83,"user_rating":90,"state":"certified_fresh",
            "user_state":"upright","link":null
        },
        "letterboxd":{"rating":4.2,"votes":500,"link":null},
        "mal":{"rating":8.9,"votes":900,"link":null}
    })
}

#[test]
fn external_ratings_keep_original_sources_scales_and_optional_votes() {
    let parsed = trakt::external_ratings(&ratings(), MediaKind::Movie);
    assert_eq!(
        parsed
            .iter()
            .map(|r| (r.source, r.rating_tenths, r.max_tenths, r.votes))
            .collect::<Vec<_>>(),
        [
            (RatingSource::Imdb, 83, 100, Some(1200)),
            (RatingSource::Tmdb, 81, 100, Some(800)),
            (RatingSource::Metascore, 740, 1000, None),
            (RatingSource::RottenTomatoes, 830, 1000, None),
            (RatingSource::RottenTomatoesAudience, 900, 1000, None),
            (RatingSource::Letterboxd, 42, 50, Some(500)),
            (RatingSource::MyAnimeList, 89, 100, Some(900)),
        ]
    );
    let parsed = trakt::external_ratings(&ratings(), MediaKind::Series);
    assert_eq!(parsed.len(), 6);
    assert!(parsed.iter().all(|r| r.source != RatingSource::Letterboxd));
    assert_eq!(RatingSource::Imdb.label(), "IMDb");
    assert_eq!(RatingSource::Tmdb.label(), "TMDB");
}

#[test]
fn malformed_null_or_out_of_scale_external_scores_remain_absent() {
    for data in [
        Value::Null,
        json!([]),
        json!({"rating":9.9,"votes":100}),
        json!({"trakt":{"rating":9.9,"votes":100}}),
        json!({"imdb":null,"tmdb":{"rating":null,"votes":null}}),
        json!({"imdb":{"rating":8.3,"votes":0}}),
        json!({"imdb":{"rating":"8.3","votes":1}}),
        json!({"imdb":{"rating":10.1},"tmdb":{"rating":-0.1}}),
        json!({"metascore":{"rating":101},"rotten_tomatoes":{"rating":-1,"user_rating":101}}),
        json!({"letterboxd":{"rating":5.1},"mal":{"rating":10.1}}),
    ] {
        assert!(
            trakt::external_ratings(&data, MediaKind::Movie).is_empty(),
            "{data}"
        );
    }
    for score in [0.0, 10.0] {
        let parsed = trakt::external_ratings(
            &json!({"imdb":{"rating":score,"votes":null}}),
            MediaKind::Movie,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].rating_tenths, (score * 10.0) as u16);
        assert_eq!(parsed[0].votes, None);
    }
}

#[tokio::test]
async fn details_use_official_all_ratings_endpoint_without_losing_card_on_failure() {
    for (kind, path, id, data) in [
        (MediaKind::Movie, "/movies/1", "trakt:movie:1", movie(1)),
        (MediaKind::Series, "/shows/2", "trakt:show:2", show(2)),
    ] {
        for (status, body, enriched) in [
            (200, ratings(), true),
            (200, json!({"rating":9.9,"votes":100}), false),
            (200, json!([]), false),
            (403, json!({}), false),
            (404, json!({}), false),
            (429, json!({}), false),
            (503, json!({}), false),
        ] {
            let mut routes = vec![
                (path.into(), (200, data.clone())),
                (format!("{path}/ratings"), (status, body)),
            ];
            if kind == MediaKind::Series {
                routes.push((format!("{path}/seasons"), (200, json!([]))));
            }
            let fixture = fixture(routes).await;
            let media = fixture_client(&fixture)
                .details(id, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(media.id, id);
            assert_eq!(media.external_ratings.is_empty(), !enriched);
            assert_eq!(
                media.rating_tenths,
                trakt::media(&data, kind).unwrap().rating_tenths
            );
            let requests = fixture.requests.lock().unwrap();
            let request = requests
                .iter()
                .find(|request| request_url(request).path().ends_with("/ratings"))
                .unwrap();
            assert_trakt_headers(request);
            assert_eq!(request_url(request).query(), Some("extended=all"));
            assert_eq!(requests.len(), if kind == MediaKind::Movie { 2 } else { 3 });
        }
    }
}

#[tokio::test]
async fn cancellation_during_optional_enrichment_is_not_partial_success() {
    let fixture = fixture(vec![
        ("/movies/1".into(), (200, movie(1))),
        ("/movies/1/ratings".into(), (200, ratings())),
    ])
    .await;
    let client = fixture_client(&fixture);
    let cancel = CancellationToken::new();
    let pending = client.details("trakt:movie:1", &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => panic!("unexpected response: {result:?}"),
        reached = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if fixture.requests.lock().unwrap().len() == 2 { break; }
                tokio::task::yield_now().await;
            }
        }) => {
            reached.expect("ratings request should reach the fixture");
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(request_url(&requests[1]).path(), "/movies/1/ratings");
            cancel.cancel();
        }
    }
    assert_eq!(pending.await, Err(Error::Cancelled));
}

#[tokio::test]
async fn optional_ratings_transport_failures_do_not_discard_details() {
    for failure in ["invalid-json", "oversize", "network", "timeout"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut connection, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(connection.read_u8().await.unwrap());
                }
                if index == 0 {
                    let body = movie(1).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    connection.write_all(response.as_bytes()).await.unwrap();
                    continue;
                }
                let request = String::from_utf8(request).unwrap();
                assert_eq!(request_url(&request).path(), "/movies/1/ratings");
                match failure {
                    "invalid-json" => {
                        connection.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{"
                        ).await.unwrap();
                    }
                    "oversize" => {
                        connection.write_all(format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            BODY_LIMIT + 1
                        ).as_bytes()).await.unwrap();
                    }
                    "network" => {}
                    "timeout" => {
                        connection
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                            .await
                            .unwrap();
                        std::future::pending::<()>().await;
                    }
                    _ => unreachable!(),
                }
            }
        });
        let mut client = client();
        client.fixture_origin = Some(origin);
        client.http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.details("trakt:movie:1", &CancellationToken::new()),
        )
        .await;
        server.abort();
        let media = result.unwrap().unwrap();
        assert_eq!(media.id, "trakt:movie:1");
        assert_eq!(media.title, "Dune");
        assert!(media.external_ratings.is_empty(), "{failure}");
    }
}

#[test]
fn episode_runtime_and_screenshot_use_explicit_metadata_and_existing_allowlist_only() {
    for (screenshot, expected) in [
        (
            "walter-r2.trakt.tv/images/movies/000/012/601/fanarts/medium/a.jpg.webp",
            Some("https://walter-r2.trakt.tv/images/movies/000/012/601/fanarts/medium/a.jpg.webp"),
        ),
        (
            "evil.invalid/images/episodes/000/012/601/screenshots/medium/a.webp",
            None,
        ),
        // No official screenshot path example: do not speculate/widen policy.
        (
            "walter-r2.trakt.tv/images/episodes/000/012/601/screenshots/medium/a.webp",
            None,
        ),
        ("media.trakt.tv/episodes/1/medium/screenshot.webp", None),
        (
            "media.trakt.tv/movies/1/medium/poster.webp?token=secret",
            None,
        ),
        ("media.trakt.tv/movies/1/medium/../poster.webp", None),
    ] {
        let data = json!([{"number":1,"episodes":[{
            "season":1,"number":1,"title":"Pilot","runtime":45,
            "images":{"screenshot":[screenshot]}
        }]}]);
        let episode = &trakt::episodes(&data).unwrap()[0];
        assert_eq!(episode.runtime_minutes, Some(45));
        assert_eq!(episode.screenshot_url.as_deref(), expected);
    }
    let episodes = trakt::episodes(&json!([{"number":1,"episodes":[
        {"season":1,"number":1},
        {"season":1,"number":2,"runtime":0,"images":{"screenshot":null}},
        {"season":1,"number":3,"runtime":"45","images":{"screenshot":"bad"}}
    ]}]))
    .unwrap();
    assert!(
        episodes
            .iter()
            .all(|e| e.runtime_minutes.is_none() && e.screenshot_url.is_none())
    );
}

#[test]
fn older_media_and_episode_snapshots_default_new_metadata_and_roundtrip() {
    let mut old = serde_json::to_value(trakt::media(&movie(1), MediaKind::Movie).unwrap()).unwrap();
    for field in [
        "runtime_minutes",
        "genres",
        "certification",
        "external_ratings",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    let decoded: Media = serde_json::from_value(old).unwrap();
    assert_eq!(decoded.runtime_minutes, None);
    assert!(decoded.genres.is_empty());
    assert_eq!(decoded.certification, None);
    assert!(decoded.external_ratings.is_empty());
    let episode: Episode = serde_json::from_value(json!({
        "season":1,"number":1,"title":"Pilot","description":"Old snapshot"
    }))
    .unwrap();
    assert_eq!(episode.runtime_minutes, None);
    assert_eq!(episode.screenshot_url, None);
    let mut enriched = decoded;
    enriched.runtime_minutes = Some(155);
    enriched.genres = vec!["drama".into()];
    enriched.certification = Some("PG-13".into());
    enriched.external_ratings = trakt::external_ratings(&ratings(), MediaKind::Movie);
    enriched.episodes = vec![episode];
    let restored: Media = serde_json::from_slice(&serde_json::to_vec(&enriched).unwrap()).unwrap();
    assert_eq!(restored, enriched);
}
