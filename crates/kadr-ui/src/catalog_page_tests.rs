use super::*;
use gpui::point;

const MOVIES: MediaKind = MediaKind::Movie;
const SHOWS: MediaKind = MediaKind::Series;

fn card(id: &str, kind: MediaKind) -> MediaCard {
    MediaCard {
        id: id.into(),
        title: id.into(),
        original_title: None,
        year: None,
        description: None,
        poster_url: None,
        backdrop_url: None,
        rating_tenths: None,
        runtime_minutes: None,
        certification: None,
        genres: Vec::new(),
        external_ratings: Vec::new(),
        kind,
        episodes: Vec::new(),
        attribution: "Trakt".into(),
    }
}

fn loaded(state: &mut CatalogState) -> CatalogRequest {
    let request = state.ensure(MOVIES).unwrap();
    assert!(state.apply_page(
        &request,
        CatalogPage {
            items: vec![card("z-first", MOVIES), card("a-second", MOVIES)],
            next_page: Some(2),
        },
    ));
    request
}

#[test]
fn entry_and_load_more_are_single_flight_and_server_ordered() {
    let mut state = CatalogState::default();
    loaded(&mut state);
    assert!(state.ensure(MOVIES).is_none());
    let next = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    assert_eq!(next.page, 2);
    assert!(state.dispatch(MOVIES, CatalogAction::LoadMore).is_none());
    assert!(state.apply_page(
        &next,
        CatalogPage {
            items: vec![card("m-third", MOVIES)],
            next_page: None,
        },
    ));
    assert_eq!(
        state
            .listing(MOVIES)
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["z-first", "a-second", "m-third"],
    );
    assert!(state.contains("m-third"));
    assert!(!state.contains("unknown"));
    assert!(state.dispatch(MOVIES, CatalogAction::LoadMore).is_none());
}

#[test]
fn overlapping_pages_keep_first_occurrence_in_server_encounter_order() {
    let mut state = CatalogState::default();
    let first = state.ensure(MOVIES).unwrap();
    state.apply_page(
        &first,
        CatalogPage {
            items: vec![
                card("b-first", MOVIES),
                card("a-second", MOVIES),
                card("b-first", MOVIES),
            ],
            next_page: Some(2),
        },
    );
    let next = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    let mut duplicate = card("a-second", MOVIES);
    duplicate.title = "Changed after first appearance".into();
    state.apply_page(
        &next,
        CatalogPage {
            items: vec![duplicate, card("c-third", MOVIES), card("c-third", MOVIES)],
            next_page: None,
        },
    );
    assert_eq!(
        state
            .listing(MOVIES)
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["b-first", "a-second", "c-third"],
    );
    assert_eq!(state.listing(MOVIES).items[1].title, "a-second");
}

#[test]
fn filters_request_page_one_and_reject_stale_responses() {
    let mut state = CatalogState::default();
    let old = state.ensure(MOVIES).unwrap();
    let popular = state
        .dispatch(MOVIES, CatalogAction::Genre(Some("drama".into())))
        .unwrap();
    let trending = state
        .dispatch(MOVIES, CatalogAction::Sort(CatalogSort::Trending))
        .unwrap();
    let year = state
        .dispatch(MOVIES, CatalogAction::Year(Some(1999)))
        .unwrap();
    assert_eq!(year.page, 1);
    assert_eq!(year.query.genre.as_deref(), Some("drama"));
    assert_eq!(year.query.sort, CatalogSort::Trending);
    assert_eq!(year.query.year, Some(1999));
    for stale in [old, popular, trending] {
        assert!(!state.apply_page(
            &stale,
            CatalogPage {
                items: vec![card("stale", MOVIES)],
                next_page: None,
            },
        ));
        assert!(!state.apply_failure(&stale, "late error"));
    }
    assert!(state.listing(MOVIES).items.is_empty());
    assert_eq!(state.listing(MOVIES).pending.as_ref(), Some(&year));
    assert!(
        state
            .dispatch(MOVIES, CatalogAction::Year(Some(1999)))
            .is_none()
    );
}

#[test]
fn failed_append_preserves_cards_and_retries_exact_page_with_new_identity() {
    let mut state = CatalogState::default();
    loaded(&mut state);
    let failed = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    assert!(state.apply_failure(&failed, "offline"));
    assert_eq!(state.listing(MOVIES).items.len(), 2);
    assert!(state.dispatch(MOVIES, CatalogAction::LoadMore).is_none());
    assert!(state.ensure(MOVIES).is_none());
    let retry = state.dispatch(MOVIES, CatalogAction::Retry).unwrap();
    assert_eq!(retry.page, failed.page);
    assert_eq!(retry.query, failed.query);
    assert_ne!(retry.id, failed.id);
    assert!(state.dispatch(MOVIES, CatalogAction::Retry).is_none());
    assert!(!state.apply_failure(&failed, "stale error"));
}

#[test]
fn initial_failure_is_not_silently_retried_on_back() {
    let mut state = CatalogState::default();
    let initial = state.ensure(MOVIES).unwrap();
    assert!(state.ensure(MOVIES).is_none());
    state.apply_failure(&initial, "offline");
    assert!(state.ensure(MOVIES).is_none());
    assert_eq!(
        state.dispatch(MOVIES, CatalogAction::Retry).unwrap().page,
        1,
    );
}

#[test]
fn kinds_and_back_keep_independent_query_cards_and_scroll() {
    let mut state = CatalogState::default();
    loaded(&mut state);
    state
        .listing(MOVIES)
        .scroll()
        .set_offset(point(px(0.), px(-650.)));
    let shows = state.ensure(SHOWS).unwrap();
    state.apply_page(
        &shows,
        CatalogPage {
            items: vec![card("show", SHOWS)],
            next_page: None,
        },
    );
    assert!(state.ensure(MOVIES).is_none());
    assert_eq!(state.listing(MOVIES).scroll().offset().y, px(-650.));
    assert_eq!(state.listing(MOVIES).items.len(), 2);
    assert_eq!(state.listing(SHOWS).items[0].id, "show");
    state.dispatch(MOVIES, CatalogAction::Year(Some(2000)));
    assert_eq!(state.listing(MOVIES).scroll().offset().y, px(0.));
    assert_eq!(state.listing(SHOWS).items[0].id, "show");
    assert_eq!(state.listing(SHOWS).query.year, None);
}

#[test]
fn scroll_requires_downward_motion_and_stops_at_inflight_and_errors() {
    let mut state = CatalogState::default();
    loaded(&mut state);
    assert!(state.dispatch(MOVIES, CatalogAction::CheckScroll).is_none());
    let scroll = state.listing(MOVIES).scroll().clone();
    // Explicit offsets work without a window. Default max_offset is zero,
    // placing these movements within the prefetch threshold.
    scroll.set_offset(point(px(0.), px(-100.)));
    let next = state.dispatch(MOVIES, CatalogAction::CheckScroll).unwrap();
    scroll.set_offset(point(px(0.), px(-150.)));
    assert!(state.dispatch(MOVIES, CatalogAction::CheckScroll).is_none());
    state.apply_failure(&next, "offline");
    scroll.set_offset(point(px(0.), px(-200.)));
    assert!(state.dispatch(MOVIES, CatalogAction::CheckScroll).is_none());
    let retry = state.dispatch(MOVIES, CatalogAction::Retry).unwrap();
    state.apply_page(
        &retry,
        CatalogPage {
            items: Vec::new(),
            next_page: Some(3),
        },
    );
    scroll.set_offset(point(px(0.), px(-100.)));
    assert!(state.dispatch(MOVIES, CatalogAction::CheckScroll).is_none());
    scroll.set_offset(point(px(0.), px(-110.)));
    assert!(state.dispatch(MOVIES, CatalogAction::CheckScroll).is_some());
}

#[test]
fn empty_and_short_pages_follow_server_next_page_not_item_count() {
    let mut state = CatalogState::default();
    let first = state.ensure(MOVIES).unwrap();
    state.apply_page(
        &first,
        CatalogPage {
            items: Vec::new(),
            next_page: Some(4),
        },
    );
    assert!(!state.listing(MOVIES).is_empty_result());
    let next = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    assert_eq!(next.page, 4);
    state.apply_page(
        &next,
        CatalogPage {
            items: vec![card("only-one", MOVIES)],
            next_page: None,
        },
    );
    assert!(state.dispatch(MOVIES, CatalogAction::LoadMore).is_none());
}

#[test]
fn empty_result_requires_exhausted_server_pages() {
    let mut state = CatalogState::default();
    assert!(!state.listing(MOVIES).is_empty_result());
    let first = state.ensure(MOVIES).unwrap();
    assert!(!state.listing(MOVIES).is_empty_result());
    state.apply_page(
        &first,
        CatalogPage {
            items: Vec::new(),
            next_page: Some(2),
        },
    );
    assert!(!state.listing(MOVIES).is_empty_result());
    let next = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    state.apply_page(
        &next,
        CatalogPage {
            items: Vec::new(),
            next_page: None,
        },
    );
    assert!(state.listing(MOVIES).is_empty_result());
}

#[test]
fn opt_out_invalidates_inflight_and_never_reuses_identity() {
    let mut state = CatalogState::default();
    let old = loaded(&mut state);
    let append = state.dispatch(MOVIES, CatalogAction::LoadMore).unwrap();
    state.reset();
    assert!(!state.contains("z-first"));
    let fresh = state.ensure(MOVIES).unwrap();
    assert!(fresh.id > append.id);
    assert!(fresh.id > old.id);
    assert!(!state.apply_failure(&append, "late error"));
    assert!(!state.apply_page(
        &append,
        CatalogPage {
            items: vec![card("should-not-return", MOVIES)],
            next_page: None,
        },
    ));
    assert_eq!(state.listing(MOVIES).pending.as_ref(), Some(&fresh));
}

#[test]
fn invalid_pagination_is_visible_and_retryable_not_an_infinite_loop() {
    let mut state = CatalogState::default();
    let first = state.ensure(MOVIES).unwrap();
    assert!(state.apply_page(
        &first,
        CatalogPage {
            items: vec![card("discarded", MOVIES)],
            next_page: Some(1),
        },
    ));
    assert!(state.listing(MOVIES).items.is_empty());
    assert!(state.listing(MOVIES).error.is_some());
    assert_eq!(
        state.dispatch(MOVIES, CatalogAction::Retry).unwrap().page,
        1,
    );
}

#[test]
fn lookups_do_not_change_query_or_invalidate_pending_page() {
    let mut state = CatalogState::default();
    let pending = state.ensure(MOVIES).unwrap();
    state.set_genres_loading(MOVIES);
    state.set_genres_error(MOVIES, "offline");
    assert!(
        matches!(&state.listing(MOVIES).genres_status, GenresStatus::Error(message) if message == "offline")
    );
    state.set_genres(
        MOVIES,
        vec![CatalogGenre {
            slug: "drama".into(),
            name: "Драма".into(),
        }],
    );
    state.set_years(vec![2026, 2025, 2024]);
    assert_eq!(state.listing(MOVIES).pending.as_ref(), Some(&pending));
    assert_eq!(state.listing(MOVIES).query, pending.query);
    assert!(matches!(
        state.listing(MOVIES).genres_status,
        GenresStatus::Ready
    ));
    assert_eq!(state.listing(MOVIES).genres.len(), 1);
    assert!(state.listing(SHOWS).genres.is_empty());
}
