use super::*;
use crate::{CatalogQuery, CatalogSort, YearRange};

fn pagination(page: u32, limit: u16, count: u32, total: u32) -> Vec<(String, String)> {
    [
        ("X-Pagination-Page", page.to_string()),
        ("X-Pagination-Limit", limit.to_string()),
        ("X-Pagination-Page-Count", count.to_string()),
        ("X-Pagination-Item-Count", total.to_string()),
    ]
    .map(|(name, value)| (name.to_owned(), value))
    .to_vec()
}

#[tokio::test]
async fn catalog_uses_real_rankings_filters_and_both_response_shapes() {
    for (kind, data, kind_path) in [
        (MediaKind::Movie, movie(9), "movies"),
        (MediaKind::Series, show(9), "shows"),
    ] {
        for sort in [CatalogSort::Popular, CatalogSort::Trending] {
            let operation = match sort {
                CatalogSort::Popular => "popular",
                CatalogSort::Trending => "trending",
            };
            let row = match sort {
                CatalogSort::Popular => data.clone(),
                CatalogSort::Trending => trending_row(data.clone(), kind),
            };
            let fixture = fixture_headers(vec![(
                format!("/{kind_path}/{operation}"),
                (200, json!([row]), pagination(2, 20, 12, 230)),
            )])
            .await;
            let query = CatalogQuery {
                kind,
                sort,
                genres: vec!["science-fiction".into(), "drama".into()],
                years: Some(YearRange {
                    start: 2000,
                    end: 2025,
                }),
                page: 2,
                ..Default::default()
            };
            let page = fixture_client(&fixture)
                .catalog(&query, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].kind, kind);
            assert_eq!(page.items[0].year, data["year"].as_u64().map(|y| y as u16));
            assert_eq!(page.page, 2);
            assert_eq!(page.limit, 20);
            assert_eq!(page.page_count, Some(12));
            assert_eq!(page.total_items, Some(230));
            // A short raw response is not the last page.
            assert_eq!(page.next_page, Some(3));
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_trakt_headers(&requests[0]);
            let url = request_url(&requests[0]);
            assert_eq!(url.path(), format!("/{kind_path}/{operation}"));
            let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(pairs["extended"], "full");
            assert_eq!(pairs["page"], "2");
            assert_eq!(pairs["limit"], "20");
            assert_eq!(pairs["genres"], "science-fiction,drama");
            assert_eq!(pairs["years"], "2000-2025");
            assert_eq!(pairs.len(), 5);
        }
    }
}

#[tokio::test]
async fn pagination_uses_raw_rows_and_headers_not_remaining_valid_cards() {
    let fixture = fixture_headers(vec![(
        "/movies/popular".into(),
        (
            200,
            json!([movie(7), movie(7), {}, movie(2)]),
            pagination(1, 4, 5, 20),
        ),
    )])
    .await;
    let result = fixture_client(&fixture)
        .catalog(
            &CatalogQuery {
                limit: 4,
                ..Default::default()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["trakt:movie:7", "trakt:movie:2"]
    );
    assert_eq!(result.next_page, Some(2));
}

#[tokio::test]
async fn pagination_is_bounded_and_never_invented_when_headers_are_absent() {
    for (page, headers, next_page) in [
        (1, vec![], None),
        (1, pagination(1, 20, 1, 1), None),
        (1000, pagination(1000, 20, 100_000, 2_000_000), None),
    ] {
        let fixture = fixture_headers(vec![(
            "/movies/popular".into(),
            (200, json!([movie(1)]), headers),
        )])
        .await;
        let result = fixture_client(&fixture)
            .catalog(
                &CatalogQuery {
                    page,
                    ..Default::default()
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.next_page, next_page);
    }
}

#[tokio::test]
async fn empty_raw_array_stops_but_fully_rejected_cards_do_not() {
    for (data, next_page) in [(json!([]), None), (json!([{}, {}]), Some(2))] {
        let fixture = fixture_headers(vec![(
            "/movies/popular".into(),
            (200, data, pagination(1, 2, 5, 10)),
        )])
        .await;
        let page = fixture_client(&fixture)
            .catalog(
                &CatalogQuery {
                    limit: 2,
                    ..Default::default()
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.next_page, next_page);
    }
}

#[tokio::test]
async fn server_clamped_limit_is_reported_and_empty_outdated_page_is_safe() {
    for (data, headers, query, expected_limit, next) in [
        (
            json!([movie(1)]),
            pagination(1, 1, 100, 100),
            CatalogQuery::default(),
            1,
            Some(2),
        ),
        (
            json!([]),
            pagination(1, 20, 0, 0),
            CatalogQuery::default(),
            20,
            None,
        ),
        (
            json!([]),
            pagination(2, 20, 1, 10),
            CatalogQuery {
                page: 2,
                ..Default::default()
            },
            20,
            None,
        ),
    ] {
        let fixture = fixture_headers(vec![("/movies/popular".into(), (200, data, headers))]).await;
        let page = fixture_client(&fixture)
            .catalog(&query, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(page.limit, expected_limit);
        assert_eq!(page.next_page, next);
    }
}

#[tokio::test]
async fn malformed_or_inconsistent_pagination_is_rejected() {
    for headers in [
        vec![("X-Pagination-Page".into(), "2".into())],
        vec![("X-Pagination-Limit".into(), "0".into())],
        vec![("X-Pagination-Limit".into(), "101".into())],
        vec![("X-Pagination-Page-Count".into(), "-1".into())],
        vec![("X-Pagination-Item-Count".into(), "4294967296".into())],
        vec![
            ("X-Pagination-Page".into(), "1".into()),
            ("X-Pagination-Page".into(), "1".into()),
        ],
    ] {
        let fixture = fixture_headers(vec![(
            "/movies/popular".into(),
            (200, json!([movie(1)]), headers),
        )])
        .await;
        assert_eq!(
            fixture_client(&fixture)
                .catalog(&CatalogQuery::default(), &CancellationToken::new())
                .await,
            Err(Error::InvalidResponse)
        );
    }
}

#[tokio::test]
async fn invalid_catalog_queries_and_privacy_gates_never_make_requests() {
    let fixture = fixture(vec![]).await;
    let mut client = fixture_client(&fixture);
    let cancel = CancellationToken::new();
    for query in [
        CatalogQuery {
            page: 0,
            ..Default::default()
        },
        CatalogQuery {
            page: 1001,
            ..Default::default()
        },
        CatalogQuery {
            limit: 0,
            ..Default::default()
        },
        CatalogQuery {
            limit: 101,
            ..Default::default()
        },
        CatalogQuery {
            genres: vec!["drama&years=2024".into()],
            ..Default::default()
        },
        CatalogQuery {
            genres: vec!["drama".into(); 11],
            ..Default::default()
        },
        CatalogQuery {
            years: Some(YearRange {
                start: 2025,
                end: 2020,
            }),
            ..Default::default()
        },
        CatalogQuery {
            years: Some(YearRange {
                start: 0,
                end: 2020,
            }),
            ..Default::default()
        },
    ] {
        assert_eq!(
            client.catalog(&query, &cancel).await,
            Err(Error::InvalidCatalogQuery)
        );
    }
    client.options.enabled = false;
    assert_eq!(
        client.catalog(&Default::default(), &cancel).await,
        Err(Error::Disabled)
    );
    assert_eq!(
        client.genres(MediaKind::Movie, &cancel).await,
        Err(Error::Disabled)
    );
    client.options.enabled = true;
    cancel.cancel();
    assert_eq!(
        client.catalog(&Default::default(), &cancel).await,
        Err(Error::Cancelled)
    );
    assert_eq!(
        client.genres(MediaKind::Series, &cancel).await,
        Err(Error::Cancelled)
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn genre_lists_are_provider_reported_bounded_and_deduplicated() {
    for (kind, path) in [
        (MediaKind::Movie, "/genres/movies"),
        (MediaKind::Series, "/genres/shows"),
    ] {
        let fixture = fixture(vec![(
            path.into(),
            (
                200,
                json!([
                    {"name":"Science Fiction", "slug":"science-fiction"},
                    {"name":"Duplicate", "slug":"science-fiction"},
                    {"name":"Drama", "slug":"drama"},
                    {"name":"Invalid", "slug":"https://evil.invalid/"},
                    {"name":7, "slug":"comedy"}
                ]),
            ),
        )])
        .await;
        let genres = fixture_client(&fixture)
            .genres(kind, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(genres.len(), 2);
        assert_eq!(genres[0].slug, "science-fiction");
        assert_eq!(genres[0].name, "Science Fiction");
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let url = request_url(&requests[0]);
        assert_eq!(url.path(), path);
        assert_eq!(url.query(), None);
    }
}
