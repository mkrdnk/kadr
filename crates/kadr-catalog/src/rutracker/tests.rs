use super::*;

fn movie(title: &str, original: Option<&str>, year: Option<u16>) -> Media {
    Media {
        id: "test:1".into(),
        title: title.into(),
        original_title: original.map(str::to_owned),
        external: Default::default(),
        year,
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

fn matches(title: &str, media: &Media, episode: Option<&Episode>) -> bool {
    match classify_release(title, &query(media, episode)) {
        Some(ReleaseMatch::Exact) => true,
        Some(ReleaseMatch::Possible) => media.kind == MediaKind::Series,
        None => false,
    }
}

fn query(media: &Media, episode: Option<&Episode>) -> MediaQuery {
    MediaQuery {
        media_id: media.id.clone(),
        kind: media.kind,
        title: media.title.clone(),
        original_title: media.original_title.clone(),
        year: media.year,
        external: media.external.clone(),
        episode: episode.and_then(|e| EpisodeNumber::new(e.season, e.number)),
    }
}

fn movie_queries(media: &Media) -> Result<Vec<String>, Error> {
    queries(&query(media, None))
}

fn episode(season: u16, number: u16) -> Episode {
    Episode {
        season,
        number,
        title: String::new(),
        description: String::new(),
        runtime_minutes: None,
        screenshot_url: None,
    }
}

#[test]
fn distinct_localized_original_queries_are_literal_and_bounded() {
    let media = movie("  Дюна!  ", Some("Dune"), Some(2021));
    assert_eq!(movie_queries(&media).unwrap(), ["Дюна", "Dune"]);
    assert_eq!(
        movie_queries(&movie("Dune", Some(" DUNE! "), None)).unwrap(),
        ["Dune"]
    );
    assert_eq!(
        movie_queries(&movie("\"Dune\" | +other -word", None, None)).unwrap(),
        ["Dune other word"]
    );
    assert_eq!(
        movie_queries(&movie("", None, None)),
        Err(Error::InvalidQuery)
    );
    assert_eq!(
        movie_queries(&movie("X\nY", None, None)),
        Err(Error::InvalidQuery)
    );
    assert_eq!(
        movie_queries(&movie(&"a".repeat(201), None, None)),
        Err(Error::InvalidQuery)
    );
    assert_eq!(
        movie_queries(&movie("", Some("Valid"), None)).unwrap(),
        ["Valid"]
    );
}

#[test]
fn film_matching_requires_full_title_and_production_year_not_release_numbers() {
    let media = movie("Нечто", Some("The Thing"), Some(1982));
    for title in [
        "Нечто / The Thing (Джон Карпентер) [1982, США, ужасы] BDRip 1080p",
        "The Thing (1982) [BDRip 1080p] Remastered 2024",
    ] {
        assert!(matches(title, &media, None), "{title}");
    }
    for title in [
        "The Thing [2011, USA] 1080p re-release 1982",
        "The Thing [1080p] 1982",
        "The Thing from Another World [1982, USA]",
        "Another The Thing [1982, USA]",
        "The Thing [1982-2011, USA]",
        "The Thing [2024 remaster]",
        "The Thing (2024) [1982, USA]",
        "The Thing [1982, USA] Season 1",
        "The Thing [1982, USA] S01E01",
    ] {
        assert!(!matches(title, &media, None), "{title}");
    }
    let remake = movie("The Thing", None, Some(2024));
    assert!(!matches(
        "The Thing [1982, USA] Remastered 2024",
        &remake,
        None
    ));
    assert!(!matches(
        "The Thing [2024 remaster] [1982, USA]",
        &remake,
        None
    ));
    assert!(!matches("The Thing [2024, Remastered]", &remake, None));
    let numeric = movie("1917", None, Some(2019));
    assert!(matches("1917 [2019, UK] 1080p", &numeric, None));
    assert!(!matches("1917 [2020, UK]", &numeric, None));
}

#[test]
fn film_cache_eligibility_uses_the_live_rule_and_requires_known_year() {
    let mut media = movie("Нечто", Some("The Thing"), Some(1982));
    media.id = "trakt:movie:42".into();
    let query = MediaQuery::from_media(&media, None).unwrap();
    for title in [
        "Нечто / The Thing [1982, USA] 1080p",
        "The Thing (1982) [Remastered 2024]",
        "The Thing [2011, USA] 1080p",
        "The Thing [1982-2011]",
        "The Thing (2011) [1982, USA]",
        "The Thing [1982] S01E01",
        "The Thing [1982] 1x01",
        "The Thing [1982] Collection",
        "The Thing [1982] Complete Films",
        "The Thing [1982] Сборник",
        "The Thing Returns [1982]",
        "The Thing 1982 1080p",
    ] {
        assert_eq!(
            matches_movie_release(title, &query),
            matches(title, &media, None),
            "{title}"
        );
    }
    media.year = None;
    let unknown = MediaQuery::from_media(&media, None).unwrap();
    assert!(!matches("The Thing [1982]", &media, None));
    assert!(!matches_movie_release("The Thing [1982]", &unknown));
    let mut collection = movie("The Collection", None, Some(2012));
    collection.id = "trakt:movie:43".into();
    let query = MediaQuery::from_media(&collection, None).unwrap();
    assert!(matches_movie_release("The Collection [2012] 1080p", &query));
}

#[test]
fn series_keep_source_scope_and_reject_known_wrong_seasons_and_episodes() {
    let mut media = movie("Шерлок", Some("Sherlock"), Some(2010));
    media.kind = MediaKind::Series;
    let selected = episode(2, 3);
    for title in [
        "Шерлок / Sherlock / Сезон: 2 / Серии: 1-3 из 3 [2010-2017, UK] 1080p",
        "Sherlock S02E03 [2010-2017, UK]",
        "Sherlock S02E01-E03 [2010-2017, UK]",
        "Sherlock S02 [2010-2017, UK]",
        "Sherlock [2010-2017, UK] Seasons 1-4",
    ] {
        assert!(matches(title, &media, Some(&selected)), "{title}");
    }
    for title in [
        "Sherlock S01E03 [2010-2017, UK]",
        "Sherlock S02E01 [2010-2017, UK]",
        "Sherlock S02E01-E02 [2010-2017, UK]",
        "Sherlock S02 / Episodes 1-2 [2010-2017, UK]",
        "Sherlock S02E03 / Episodes 1-2 [2010-2017, UK]",
        "Sherlock S02 / Season 3 [2010-2017, UK]",
        "Sherlock / Сезон: 2 / Серии: 1-2 [2010-2017, UK]",
        "Sherlock / Сезон: 3 / Серии: 1-3 [2010-2017, UK]",
        "Sherlock Holmes S02E03 [2010-2017, UK]",
    ] {
        assert!(!matches(title, &media, Some(&selected)), "{title}");
    }
    assert_eq!(
        classify_release(
            "Sherlock S01 [2010, UK]",
            &query(&media, Some(&episode(1, 1)))
        ),
        Some(ReleaseMatch::Possible)
    );
    assert!(!matches("Sherlock [2010, UK]", &media, None));
    assert_eq!(
        quality("Sherlock S02E03 [2010] WEB-DL 1080p"),
        Some("1080p".into())
    );
    assert_eq!(quality("Sherlock [2010] HD"), None);
    assert_eq!(quality("Sherlock [2010] x1080p"), None);
}

#[test]
fn classifier_keeps_unknown_metadata_possible_without_weakening_movie_watch() {
    let media = movie("Нечто", Some("The Thing"), Some(1982));
    let query = query(&media, None);
    for title in [
        "The Thing 1982 1080p",
        "The Thing 1080p",
        "The Thing [1080p] 1982",
        "The Thing",
        "The Thing [2024 remaster]",
        "The Thing Remastered 2024",
        "The Thing 1080p [1982]",
        "The Thing 1982 [1982]",
    ] {
        assert_eq!(
            classify_release(title, &query),
            Some(ReleaseMatch::Possible),
            "{title}"
        );
        assert!(!matches_movie_release(title, &query), "{title}");
    }
    for title in [
        "The Thing 2011 1080p",
        "The Thing 2011 Remastered 2024",
        "The Thing [1080p] 2011",
        "The Thing [1982] 2011",
        "The Thing [1982] [2011]",
        "The Thing [2011, USA, Remastered 2024]",
        "The Thing Returns 1982 1080p",
        "The Thing / Collection",
        "The Thing [1982] Complete Films",
        "The Thing [1982] S01E01",
    ] {
        assert_eq!(classify_release(title, &query), None, "{title}");
    }
    let mut unknown = query.clone();
    unknown.year = None;
    assert_eq!(
        classify_release("The Thing [1982]", &unknown),
        Some(ReleaseMatch::Possible)
    );
    assert_eq!(classify_release("The Thing [1982] [2011]", &unknown), None);
    assert!(!matches_movie_release("The Thing [1982]", &unknown));
    assert_eq!(
        classify_release("The Thing [1982] Remastered 2024", &query),
        Some(ReleaseMatch::Exact)
    );
}

#[test]
fn aliases_are_distinct_bounded_and_used_without_external_id_evidence() {
    let mut media = movie("Нечто", Some("The Thing"), Some(1982));
    media.external.imdb_id = Some("tt0084787".into());
    media.external.alternative_titles = vec![
        "THE THING!".into(),
        "  ".into(),
        "X\nY".into(),
        "La Cosa".into(),
        "Das Ding".into(),
        "Ignored fifth title".into(),
    ];
    let query = query(&media, None);
    assert_eq!(
        queries(&query).unwrap(),
        ["Нечто", "The Thing", "La Cosa", "Das Ding"]
    );
    assert_eq!(
        classify_release("La Cosa [1982] 1080p", &query),
        Some(ReleaseMatch::Exact)
    );
    assert_eq!(
        classify_release("Das Ding 1982 1080p", &query),
        Some(ReleaseMatch::Possible)
    );
    assert_eq!(classify_release("La Cosa [2011]", &query), None);
    assert_eq!(
        classify_release("Another Movie [1982] tt0084787", &query),
        None
    );
}

#[test]
fn episode_classifier_preserves_conflict_veto_and_allows_unknown_or_season_years() {
    let mut media = movie("Шерлок", Some("Sherlock"), Some(2010));
    media.kind = MediaKind::Series;
    let query = query(&media, Some(&episode(2, 3)));
    for title in [
        "Sherlock S02E03 [2012]",
        "Sherlock / Сезон 2 [2012-2013]",
        "Sherlock [2012] Seasons 1-4",
        "Sherlock S02E03",
        "Sherlock [2010]",
        "Sherlock",
    ] {
        assert_eq!(
            classify_release(title, &query),
            Some(ReleaseMatch::Possible),
            "{title}"
        );
    }
    for title in [
        "Sherlock S01E03",
        "Sherlock S02E01",
        "Sherlock S02E03 / S01E03",
        "Sherlock S02 / S01E03E04",
        "Sherlock S02 / S02E01E02",
        "Sherlock Season 2 / Season 3",
        "Sherlock Season 2 / Episodes 1-3 / Episode 5",
        "Sherlock 1x03",
        "Sherlock S02 [2009]",
        "Sherlock S01 [2012]",
        "Sherlock Holmes S02E03",
    ] {
        assert_eq!(classify_release(title, &query), None, "{title}");
    }
    assert_eq!(
        classify_release("Sherlock 2x03 [2010-2017]", &query),
        Some(ReleaseMatch::Exact)
    );
    assert_eq!(
        classify_release("Sherlock S02 [2010-2017]", &query),
        Some(ReleaseMatch::Possible)
    );
    assert_eq!(
        classify_release("Sherlock S02E03 / 1x03 [2010]", &query),
        None
    );
    assert_eq!(
        classify_release("Sherlock S02E03 [2020]", &query),
        Some(ReleaseMatch::Possible)
    );
    assert_eq!(
        classify_release("Sherlock / Season 2 / Episode 3 [2010-2017]", &query),
        Some(ReleaseMatch::Exact)
    );
}

#[test]
fn legacy_bare_series_scope_does_not_enable_unscoped_engine_queries() {
    let mut media = movie("Sherlock", None, Some(2010));
    media.kind = MediaKind::Series;
    let query = query(&media, None);
    assert_eq!(
        classify_release("Sherlock Season 2 [2010-2017]", &query),
        None
    );
    assert_eq!(
        classify_topic("Sherlock Season 2 [2010-2017]", &query),
        Some(ReleaseMatch::Possible)
    );
    assert_eq!(classify_topic("Sherlock [2010]", &query), None);
}

fn page(rows: &[(u64, &str)], total_pages: u32) -> SearchPage {
    SearchPage {
        offers: rows
            .iter()
            .map(|&(topic_id, title)| Offer {
                topic_id,
                title: title.into(),
                quality: quality(title),
                size: None,
                seeds: None,
            })
            .collect(),
        had_topics: !rows.is_empty(),
        total_pages,
    }
}

#[tokio::test]
async fn fallback_stops_after_primary_confirmed_hits() {
    let query = query(&movie("Нечто", Some("The Thing"), Some(1982)), None);
    let mut calls = Vec::new();
    let outcome = search_with(&query, &CancellationToken::new(), |title, number| {
        calls.push((title, number));
        std::future::ready(Ok(page(&[(1, "The Thing [1982]")], 10)))
    })
    .await
    .unwrap();
    assert_eq!(calls, [("Нечто".into(), 1)]);
    assert!(outcome.had_topics);
    assert_eq!(outcome.offers.len(), 1);
}

#[tokio::test]
async fn fallback_tries_alternatives_before_advertised_extra_pages() {
    let query = query(&movie("Нечто", Some("The Thing"), Some(1982)), None);
    let mut calls = Vec::new();
    let outcome = search_with(&query, &CancellationToken::new(), |title, number| {
        calls.push((title.clone(), number));
        let rows = match (title.as_str(), number) {
            ("Нечто", 1) => vec![(1, "Other Movie [1982]"), (2, "The Thing 1982 1080p")],
            ("The Thing", 1) => vec![(2, "The Thing 1982 1080p")],
            ("Нечто", 2) => vec![(3, "The Thing [1982]")],
            _ => panic!("unexpected search"),
        };
        std::future::ready(Ok(page(&rows, 20)))
    })
    .await
    .unwrap();
    assert_eq!(
        calls,
        [
            ("Нечто".into(), 1),
            ("The Thing".into(), 1),
            ("Нечто".into(), 2)
        ]
    );
    assert_eq!(
        outcome
            .offers
            .iter()
            .map(|offer| offer.topic_id)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert!(outcome.had_topics);
}

#[tokio::test]
async fn fallback_is_bounded_and_does_not_invent_pagination() {
    let mut media = movie("One", Some("Two"), Some(1982));
    media.external.alternative_titles = vec!["Three".into(), "Four".into(), "Five".into()];
    let query = query(&media, None);
    for total_pages in [0, 1, 100] {
        let mut calls = Vec::new();
        let outcome = search_with(&query, &CancellationToken::new(), |title, number| {
            calls.push((title, number));
            std::future::ready(Ok(page(&[], total_pages)))
        })
        .await
        .unwrap();
        assert_eq!(
            calls.len(),
            if total_pages > 1 {
                SEARCH_CALL_LIMIT
            } else {
                QUERY_LIMIT
            }
        );
        assert!(calls.iter().all(|(_, number)| *number <= SEARCH_PAGE_LIMIT));
        assert_eq!(
            outcome,
            SearchOutcome {
                offers: Vec::new(),
                had_topics: false
            }
        );
    }
}

#[tokio::test]
async fn outcome_distinguishes_filtered_topics_from_true_empty() {
    let query = query(&movie("The Thing", None, Some(1982)), None);
    for had_topics in [false, true] {
        let outcome = search_with(&query, &CancellationToken::new(), |_, _| {
            let mut result = page(&[], 0);
            result.had_topics = had_topics;
            std::future::ready(Ok(result))
        })
        .await
        .unwrap();
        assert_eq!(
            outcome,
            SearchOutcome {
                offers: Vec::new(),
                had_topics
            }
        );
    }
    let outcome = search_with(&query, &CancellationToken::new(), |_, _| {
        std::future::ready(Ok(page(&[(1, "The Thing [2011]")], 1)))
    })
    .await
    .unwrap();
    assert!(outcome.had_topics);
    assert!(outcome.offers.is_empty());
}

#[tokio::test]
async fn optional_fallback_preserves_choices_but_never_hides_login_or_cancellation() {
    let query = query(&movie("Нечто", Some("The Thing"), Some(1982)), None);
    for error in [
        Error::Network,
        Error::Timeout,
        Error::Http(503),
        Error::Cancelled,
        Error::AuthenticationRequired,
        Error::Authorization,
        Error::RateLimited,
        Error::InvalidResponse,
        Error::TooLarge,
    ] {
        let mut calls = 0;
        let result = search_with(&query, &CancellationToken::new(), |_, _| {
            calls += 1;
            std::future::ready(if calls == 1 {
                Ok(page(&[(1, "The Thing 1982 1080p")], 2))
            } else {
                Err(error)
            })
        })
        .await;
        if matches!(error, Error::Network | Error::Timeout | Error::Http(503)) {
            assert_eq!(result.unwrap().offers.len(), 1);
        } else {
            assert_eq!(result, Err(error));
        }
        let result = search_with(&query, &CancellationToken::new(), |_, _| {
            std::future::ready(Err(error))
        })
        .await;
        assert_eq!(result, Err(error));
    }
}

#[test]
fn numeric_titles_and_resolution_dimensions_are_not_production_years() {
    let mut media = movie("1917", Some("Число 1917"), Some(2019));
    media.external.alternative_titles = vec!["The Year 1917".into()];
    let query = query(&media, None);
    for title in [
        "1917 / Число 1917 [2019] 1920 x 1080",
        "The Year 1917 [2019] 1920x1080",
        "1917 [2019] 1920*1080",
        "1917 [2019] 1920 * 1080",
        "1917 [2019] BDRip 2000 kbps",
        "1917 [2019] 2000 MB",
    ] {
        assert_eq!(
            classify_release(title, &query),
            Some(ReleaseMatch::Exact),
            "{title}"
        );
    }
}

#[test]
fn unbracketed_series_year_ranges_preserve_matching_first_season() {
    let mut media = movie("Sherlock", None, Some(2010));
    media.kind = MediaKind::Series;
    let query = query(&media, Some(&episode(1, 1)));
    for title in [
        "Sherlock 2010-2017 S01E01",
        "Sherlock / Season 1 [2010] 2010-2017",
    ] {
        assert_eq!(
            classify_release(title, &query),
            Some(ReleaseMatch::Possible),
            "{title}"
        );
    }
    assert_eq!(classify_release("Sherlock 2011-2017 S01E01", &query), None);
}

#[tokio::test]
async fn confirmed_hits_displace_possible_offers_without_displacing_other_confirmed_hits() {
    let query = query(&movie("Нечто", Some("The Thing"), Some(1982)), None);
    let mut calls = 0;
    let outcome = search_with(&query, &CancellationToken::new(), |_, _| {
        calls += 1;
        let result = if calls == 1 {
            page(
                &(1..=50)
                    .map(|id| (id, "The Thing 1982 1080p"))
                    .collect::<Vec<_>>(),
                1,
            )
        } else {
            page(&[(51, "The Thing [1982]"), (52, "The Thing [1982]")], 1)
        };
        std::future::ready(Ok(result))
    })
    .await
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(outcome.offers.len(), OFFER_LIMIT);
    assert!(outcome.offers.iter().any(|offer| offer.topic_id == 51));
    assert!(outcome.offers.iter().any(|offer| offer.topic_id == 52));
}

#[tokio::test]
async fn fallback_skips_unencodable_titles_but_propagates_invalid_queries_when_none_work() {
    let query = query(&movie("Unencodable", Some("The Thing"), Some(1982)), None);
    let mut calls = 0;
    let outcome = search_with(&query, &CancellationToken::new(), |_, _| {
        calls += 1;
        std::future::ready(if calls == 1 {
            Err(Error::InvalidQuery)
        } else {
            Ok(page(&[(1, "The Thing [1982]")], 1))
        })
    })
    .await
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(outcome.offers.len(), 1);
    assert_eq!(
        search_with(&query, &CancellationToken::new(), |_, _| {
            std::future::ready(Err(Error::InvalidQuery))
        })
        .await,
        Err(Error::InvalidQuery)
    );
}

#[tokio::test]
async fn cancellation_during_fallback_keeps_error_instead_of_usable_choices() {
    let query = query(&movie("Нечто", Some("The Thing"), Some(1982)), None);
    let cancel = CancellationToken::new();
    let mut calls = 0;
    let result = search_with(&query, &cancel, |_, _| {
        calls += 1;
        if calls == 2 {
            cancel.cancel();
        }
        std::future::ready(Ok(page(&[(1, "The Thing 1982 1080p")], 2)))
    })
    .await;
    assert_eq!(calls, 2);
    assert_eq!(result, Err(Error::Cancelled));
}

#[test]
fn magnet_is_validated_and_never_synthesized() {
    let valid = format!("magnet:?xt=urn:btih:{}&dn=Selected", "a".repeat(40));
    assert_eq!(validate_magnet(valid.clone()).unwrap(), valid);
    for value in [
        "https://rutracker.org/forum/viewtopic.php?t=1",
        "magnet:?xt=urn:btih:invalid",
        "magnet:?dn=NoHash",
        "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    ] {
        assert_eq!(validate_magnet(value.into()), Err(Error::InvalidResponse));
    }
}

#[test]
fn errors_and_offer_debug_are_redacted() {
    let secret = "private-account-cookie-topic-query";
    for error in [
        api::Error::Authorization(secret.into()),
        api::Error::InvalidArgument(secret.into()),
        api::Error::Parse {
            location: secret.into(),
            detail: secret.into(),
        },
        api::Error::ApiError(secret.into()),
    ] {
        let public = Error::from(error);
        assert!(!format!("{public:?} {public}").contains(secret));
        assert!(std::error::Error::source(&public).is_none());
    }
    assert_eq!(
        Error::from(api::Error::NotAuthenticated),
        Error::AuthenticationRequired
    );
    assert_eq!(
        Error::from(api::Error::Authorization(secret.into())),
        Error::Authorization
    );
    assert_eq!(Error::from(api::Error::TooLarge), Error::TooLarge);
    let offer = Offer {
        topic_id: 123456789,
        title: secret.into(),
        quality: None,
        size: None,
        seeds: None,
    };
    let debug = format!("{offer:?}");
    assert!(!debug.contains(secret));
    assert!(!debug.contains("123456789"));
}

#[tokio::test]
async fn cancellation_validation_and_anonymous_download_do_not_contact_service() {
    let mut client = Client::new().unwrap();
    let token = CancellationToken::new();
    assert_eq!(
        client.download(1, &token).await,
        Err(Error::AuthenticationRequired)
    );
    assert_eq!(client.download(0, &token).await, Err(Error::InvalidTopic));
    assert_eq!(client.magnet(0, &token).await, Err(Error::InvalidTopic));
    assert_eq!(
        client.login("", "", &token).await,
        Err(Error::Authorization)
    );
    assert_eq!(
        client.search(&movie("", None, None), None, &token).await,
        Err(Error::InvalidQuery)
    );
    let media = movie("Dune", None, Some(2021));
    assert_eq!(
        client.search(&media, Some(&episode(1, 1)), &token).await,
        Ok(Vec::new())
    );
    client.logout();
    client.logout();
    token.cancel();
    assert_eq!(
        client.search(&media, None, &token).await,
        Err(Error::Cancelled)
    );
    assert_eq!(client.magnet(1, &token).await, Err(Error::Cancelled));
    assert_eq!(client.download(1, &token).await, Err(Error::Cancelled));
    assert_eq!(
        client.login("unused", "unused", &token).await,
        Err(Error::Cancelled)
    );
    assert!(client.authenticated.is_none());
}

#[tokio::test]
async fn cancellation_drops_in_flight_operation_and_wins_over_ready_result() {
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let (result, ()) = tokio::join!(request::<()>(&token, std::future::pending()), async move {
        tokio::task::yield_now().await;
        cancelled.cancel();
    });
    assert_eq!(result, Err(Error::Cancelled));
    assert_eq!(
        request(&token, async { Ok(123) }).await,
        Err(Error::Cancelled)
    );
}
