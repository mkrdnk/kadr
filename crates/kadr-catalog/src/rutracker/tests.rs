use super::*;

fn movie(title: &str, original: Option<&str>, year: Option<u16>) -> Media {
    Media {
        id: "test:1".into(),
        title: title.into(),
        original_title: original.map(str::to_owned),
        external: Default::default(),
        year,
        rating_tenths: None,
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
    matches_release(title, media, episode, &queries(media).unwrap())
}

fn episode(season: u16, number: u16) -> Episode {
    Episode {
        season,
        number,
        title: String::new(),
        description: String::new(),
    }
}

#[test]
fn distinct_localized_original_queries_are_literal_and_bounded() {
    let media = movie("  Дюна!  ", Some("Dune"), Some(2021));
    assert_eq!(queries(&media).unwrap(), ["Дюна", "Dune"]);
    assert_eq!(
        queries(&movie("Dune", Some(" DUNE! "), None)).unwrap(),
        ["Dune"]
    );
    assert_eq!(
        queries(&movie("\"Dune\" | +other -word", None, None)).unwrap(),
        ["Dune other word"]
    );
    assert_eq!(queries(&movie("", None, None)), Err(Error::InvalidQuery));
    assert_eq!(
        queries(&movie("X\nY", None, None)),
        Err(Error::InvalidQuery)
    );
    assert_eq!(
        queries(&movie(&"a".repeat(201), None, None)),
        Err(Error::InvalidQuery)
    );
    assert_eq!(queries(&movie("", Some("Valid"), None)).unwrap(), ["Valid"]);
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
        "Sherlock [2010-2017, UK]",
        "Sherlock Holmes S02E03 [2010-2017, UK]",
        "Sherlock S02E03 [2020, UK]",
    ] {
        assert!(!matches(title, &media, Some(&selected)), "{title}");
    }
    assert!(matches("Sherlock S01 [2010, UK]", &media, None));
    assert!(!matches("Sherlock [2010, UK]", &media, None));
    assert_eq!(
        quality("Sherlock S02E03 [2010] WEB-DL 1080p"),
        Some("1080p".into())
    );
    assert_eq!(quality("Sherlock [2010] HD"), None);
    assert_eq!(quality("Sherlock [2010] x1080p"), None);
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
