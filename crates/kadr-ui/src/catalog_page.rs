//! Server-ordered catalog pages. The desktop adapter owns IO and maps these DTOs.
//!
//! Keep this state on the root view, not on the detail page. Attach `scroll()` to
//! the actual section scroller and dispatch `CheckScroll` after a scroll event.
//! Setters never request data; dispatch returns the single request to emit.

#[cfg(test)]
#[path = "catalog_page_tests.rs"]
mod tests;

use std::collections::HashSet;

use gpui::{Context, Div, ScrollHandle, div, prelude::*, px, rgb};

use super::{
    BORDER, Command, KadrView, MUTED, MediaCard, MediaKind, Section, TEXT, UiEvent,
    cinema::{Layout, page, page_heading},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CatalogSort {
    #[default]
    Popular,
    Trending,
}

impl CatalogSort {
    fn label(self) -> &'static str {
        match self {
            Self::Popular => "Популярное",
            Self::Trending => "В тренде",
        }
    }
}

/// Single genre and exact year controls map to the backend's genre list/year range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogQuery {
    pub kind: MediaKind,
    pub sort: CatalogSort,
    pub genre: Option<String>,
    pub year: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRequest {
    /// Monotonic across both kinds and opt-out resets, for stale-response rejection.
    pub id: u64,
    pub query: CatalogQuery,
    pub page: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPage {
    pub items: Vec<MediaCard>,
    /// Only the server decides whether another page exists, never the item count.
    pub next_page: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogGenre {
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub enum CatalogAction {
    Sort(CatalogSort),
    Genre(Option<String>),
    Year(Option<u16>),
    ToggleGenres,
    ToggleYears,
    CloseFilters,
    LoadMore,
    Retry,
    CheckScroll,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilterMenu {
    Genres,
    Years,
}

#[derive(Default)]
enum GenresStatus {
    #[default]
    Unloaded,
    Loading,
    Ready,
    Error(String),
}

pub struct CatalogListing {
    query: CatalogQuery,
    items: Vec<MediaCard>,
    ids: HashSet<String>,
    next_page: Option<u32>,
    pending: Option<CatalogRequest>,
    error: Option<String>,
    loaded: bool,
    scroll: ScrollHandle,
    last_scroll_y: f32,
    menu: Option<FilterMenu>,
    genres: Vec<CatalogGenre>,
    genres_status: GenresStatus,
}

impl CatalogListing {
    fn new(kind: MediaKind) -> Self {
        Self {
            query: CatalogQuery {
                kind,
                sort: CatalogSort::Popular,
                genre: None,
                year: None,
            },
            items: Vec::new(),
            ids: HashSet::new(),
            next_page: Some(1),
            pending: None,
            error: None,
            loaded: false,
            scroll: ScrollHandle::new(),
            last_scroll_y: 0.,
            menu: None,
            genres: Vec::new(),
            genres_status: GenresStatus::Unloaded,
        }
    }

    pub fn scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    fn is_empty_result(&self) -> bool {
        self.loaded && self.items.is_empty() && self.next_page.is_none()
    }

    fn reset_results(&mut self) {
        self.items.clear();
        self.ids.clear();
        self.next_page = Some(1);
        self.pending = None;
        self.error = None;
        self.loaded = false;
        self.scroll = ScrollHandle::new();
        self.last_scroll_y = 0.;
        self.menu = None;
    }
}

/// Independent movie/show state survives detail navigation and tab switches.
pub struct CatalogState {
    movies: CatalogListing,
    shows: CatalogListing,
    years: Vec<u16>,
    next_id: u64,
}

impl Default for CatalogState {
    fn default() -> Self {
        Self {
            movies: CatalogListing::new(MediaKind::Movie),
            shows: CatalogListing::new(MediaKind::Series),
            years: Vec::new(),
            next_id: 0,
        }
    }
}

impl CatalogState {
    pub fn listing(&self, kind: MediaKind) -> &CatalogListing {
        match kind {
            MediaKind::Movie => &self.movies,
            MediaKind::Series => &self.shows,
        }
    }

    fn listing_mut(&mut self, kind: MediaKind) -> &mut CatalogListing {
        match kind {
            MediaKind::Movie => &mut self.movies,
            MediaKind::Series => &mut self.shows,
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.movies.ids.contains(id) || self.shows.ids.contains(id)
    }

    /// Opt-out invalidates pending work without reusing request identities.
    pub fn reset(&mut self) {
        self.movies = CatalogListing::new(MediaKind::Movie);
        self.shows = CatalogListing::new(MediaKind::Series);
    }

    /// Supported selectable years are supplied by the adapter, not inferred from
    /// whichever titles happened to be returned in the current page.
    pub fn set_years(&mut self, years: Vec<u16>) {
        self.years = years;
    }

    pub fn set_genres_loading(&mut self, kind: MediaKind) {
        self.listing_mut(kind).genres_status = GenresStatus::Loading;
    }

    pub fn set_genres(&mut self, kind: MediaKind, genres: Vec<CatalogGenre>) {
        let listing = self.listing_mut(kind);
        listing.genres = genres;
        listing.genres_status = GenresStatus::Ready;
    }

    pub fn set_genres_error(&mut self, kind: MediaKind, message: impl Into<String>) {
        self.listing_mut(kind).genres_status = GenresStatus::Error(message.into());
    }

    /// Entering a cached or failed listing never silently refetches it.
    pub fn ensure(&mut self, kind: MediaKind) -> Option<CatalogRequest> {
        let listing = self.listing(kind);
        if listing.loaded || listing.error.is_some() {
            return None;
        }
        self.request(kind)
    }

    fn request(&mut self, kind: MediaKind) -> Option<CatalogRequest> {
        let listing = self.listing(kind);
        if listing.pending.is_some() {
            return None;
        }
        let page = listing.next_page?;
        self.next_id += 1;
        let id = self.next_id;
        let listing = self.listing_mut(kind);
        let request = CatalogRequest {
            id,
            query: listing.query.clone(),
            page,
        };
        listing.error = None;
        listing.pending = Some(request.clone());
        Some(request)
    }

    pub fn dispatch(&mut self, kind: MediaKind, action: CatalogAction) -> Option<CatalogRequest> {
        let listing = self.listing_mut(kind);
        match action {
            CatalogAction::Sort(sort) => {
                if listing.query.sort == sort {
                    return None;
                }
                listing.query.sort = sort;
            }
            CatalogAction::Genre(genre) => {
                listing.menu = None;
                if listing.query.genre == genre {
                    return None;
                }
                listing.query.genre = genre;
            }
            CatalogAction::Year(year) => {
                listing.menu = None;
                if listing.query.year == year {
                    return None;
                }
                listing.query.year = year;
            }
            CatalogAction::ToggleGenres | CatalogAction::ToggleYears => {
                let menu = if matches!(action, CatalogAction::ToggleGenres) {
                    FilterMenu::Genres
                } else {
                    FilterMenu::Years
                };
                listing.menu = (listing.menu != Some(menu)).then_some(menu);
                return None;
            }
            CatalogAction::CloseFilters => {
                listing.menu = None;
                return None;
            }
            CatalogAction::Retry => {
                return if listing.error.is_some() {
                    self.request(kind)
                } else {
                    None
                };
            }
            CatalogAction::LoadMore => {
                return if listing.loaded && listing.error.is_none() {
                    self.request(kind)
                } else {
                    None
                };
            }
            CatalogAction::CheckScroll => {
                let y = f32::from(listing.scroll.offset().y);
                let down = y < listing.last_scroll_y;
                listing.last_scroll_y = y;
                let remaining = f32::from(listing.scroll.max_offset().height) + y;
                return if down && remaining <= 400. && listing.loaded && listing.error.is_none() {
                    self.request(kind)
                } else {
                    None
                };
            }
        }
        listing.reset_results();
        self.request(kind)
    }

    pub fn apply_page(&mut self, request: &CatalogRequest, page: CatalogPage) -> bool {
        if self.listing(request.query.kind).pending.as_ref() != Some(request) {
            return false;
        }
        if page.next_page.is_some_and(|next| next <= request.page) {
            return self.apply_failure(
                request,
                "Каталог вернул некорректную следующую страницу. Повторите загрузку.",
            );
        }
        let listing = self.listing_mut(request.query.kind);
        // Popular/trending can shift between requests. Keep the first occurrence
        // of each title while preserving the provider's encounter order.
        listing.items.extend(
            page.items
                .into_iter()
                .filter(|item| listing.ids.insert(item.id.clone())),
        );
        listing.next_page = page.next_page;
        listing.loaded = true;
        listing.pending = None;
        listing.error = None;
        true
    }

    /// An append error keeps existing cards and retries the same server page.
    pub fn apply_failure(&mut self, request: &CatalogRequest, message: impl Into<String>) -> bool {
        let listing = self.listing_mut(request.query.kind);
        if listing.pending.as_ref() != Some(request) {
            return false;
        }
        listing.pending = None;
        listing.error = Some(message.into());
        true
    }
}

impl KadrView {
    pub(super) fn render_catalog_page(
        &self,
        section: Section,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let kind = if section == Section::Shows {
            MediaKind::Series
        } else {
            MediaKind::Movie
        };
        let listing = self.catalogs.listing(kind);
        let layout = Layout::new(width);
        let enabled = self.applied_catalog_enabled && self.model.preferences.catalog_enabled;
        let genre = listing
            .query
            .genre
            .as_deref()
            .map(|slug| {
                listing
                    .genres
                    .iter()
                    .find(|genre| genre.slug == slug)
                    .map_or(slug, |genre| genre.name.as_str())
            })
            .unwrap_or("Все жанры");
        let mut toolbar = div().flex().flex_wrap().items_center().gap_2();
        for (index, sort) in [CatalogSort::Popular, CatalogSort::Trending]
            .into_iter()
            .enumerate()
        {
            toolbar = toolbar.child(self.button(
                ("catalog-sort", index),
                sort.label(),
                Command::Catalog(kind, CatalogAction::Sort(sort)),
                listing.query.sort == sort,
                enabled,
                cx,
            ));
        }
        toolbar = toolbar
            .child(self.button(
                "catalog-genres",
                format!("Жанр: {genre} ▾"),
                Command::Catalog(kind, CatalogAction::ToggleGenres),
                listing.menu == Some(FilterMenu::Genres),
                enabled,
                cx,
            ))
            .child(
                self.button(
                    "catalog-years",
                    listing
                        .query
                        .year
                        .map_or_else(|| "Год: любой ▾".into(), |year| format!("Год: {year} ▾")),
                    Command::Catalog(kind, CatalogAction::ToggleYears),
                    listing.menu == Some(FilterMenu::Years),
                    enabled,
                    cx,
                ),
            );

        page(layout)
            .child(page_heading("ОТКРЫВАЙТЕ НОВОЕ", section.title()))
            .child(toolbar)
            .when(enabled, |page| {
                page.child(self.catalog_filters(kind, cx)).child(
                    div()
                        .mt_3()
                        .mb_5()
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                        .child(match listing.query.sort {
                            CatalogSort::Popular => {
                                "Популярное — самые популярные названия в Trakt."
                            }
                            CatalogSort::Trending => "В тренде — то, что сейчас смотрят в Trakt.",
                        }),
                )
            })
            .when(enabled && !listing.items.is_empty(), |page| {
                page.child(
                    div()
                        .grid()
                        .grid_cols(layout.columns as u16)
                        .gap_x(px(14.))
                        .gap_y(px(24.))
                        .children(listing.items.iter().enumerate().map(|(index, media)| {
                            self.cinema_card(media, index, layout.card_width(), false, cx)
                        })),
                )
            })
            .child(self.catalog_feedback(kind, enabled, cx))
    }

    fn catalog_filters(&self, kind: MediaKind, cx: &mut Context<Self>) -> Div {
        let listing = self.catalogs.listing(kind);
        let Some(menu) = listing.menu else {
            return div();
        };
        let mut options = div()
            .id("catalog-filter-options")
            .max_h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(self.button(
                "catalog-filter-all",
                if menu == FilterMenu::Genres {
                    "Все жанры"
                } else {
                    "Любой год"
                },
                Command::Catalog(
                    kind,
                    if menu == FilterMenu::Genres {
                        CatalogAction::Genre(None)
                    } else {
                        CatalogAction::Year(None)
                    },
                ),
                if menu == FilterMenu::Genres {
                    listing.query.genre.is_none()
                } else {
                    listing.query.year.is_none()
                },
                true,
                cx,
            ));
        if menu == FilterMenu::Genres {
            for (index, genre) in listing.genres.iter().enumerate() {
                options = options.child(self.button(
                    ("catalog-genre", index),
                    &genre.name,
                    Command::Catalog(kind, CatalogAction::Genre(Some(genre.slug.clone()))),
                    listing.query.genre.as_deref() == Some(genre.slug.as_str()),
                    true,
                    cx,
                ));
            }
        } else {
            for (index, year) in self.catalogs.years.iter().enumerate() {
                options = options.child(self.button(
                    ("catalog-year", index),
                    year.to_string(),
                    Command::Catalog(kind, CatalogAction::Year(Some(*year))),
                    listing.query.year == Some(*year),
                    true,
                    cx,
                ));
            }
        }
        let mut panel = div()
            .mt_3()
            .p_3()
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_md()
            .flex()
            .flex_col()
            .gap_3()
            .child(options);
        if menu == FilterMenu::Genres {
            let message = match &listing.genres_status {
                GenresStatus::Loading => Some("Загружаем жанры…"),
                GenresStatus::Error(message) => Some(message.as_str()),
                GenresStatus::Unloaded => Some("Жанры ещё не загружены."),
                GenresStatus::Ready if listing.genres.is_empty() => {
                    Some("Каталог не вернул жанры.")
                }
                GenresStatus::Ready => None,
            };
            if let Some(message) = message {
                panel = panel.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                        .child(message.to_owned()),
                );
                if !matches!(listing.genres_status, GenresStatus::Loading) {
                    panel = panel.child(self.button(
                        "catalog-retry-genres",
                        "Загрузить жанры",
                        Command::Emit(UiEvent::CatalogGenres(kind)),
                        false,
                        true,
                        cx,
                    ));
                }
            }
        } else if self.catalogs.years.is_empty() {
            panel = panel.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child("Список годов пока недоступен."),
            );
        }
        panel.child(self.button(
            "catalog-close-filters",
            "Закрыть фильтр",
            Command::Catalog(kind, CatalogAction::CloseFilters),
            false,
            true,
            cx,
        ))
    }

    fn catalog_feedback(&self, kind: MediaKind, enabled: bool, cx: &mut Context<Self>) -> Div {
        let listing = self.catalogs.listing(kind);
        let panel = div()
            .mt_5()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(px(12.))
            .text_color(rgb(MUTED));
        if !enabled {
            return panel
                .child("Каталог отключён. Включите его в настройках, чтобы смотреть подборки.")
                .child(self.button(
                    "catalog-open-settings",
                    "Настройки каталога",
                    Command::Navigate(Section::Settings),
                    false,
                    true,
                    cx,
                ));
        }
        if listing.pending.is_some() {
            return panel.child(if listing.items.is_empty() {
                "Загружаем каталог…"
            } else {
                "Загружаем следующую страницу…"
            });
        }
        if let Some(error) = &listing.error {
            return panel
                .child(
                    div()
                        .text_color(rgb(TEXT))
                        .child(if listing.items.is_empty() {
                            "Не удалось загрузить каталог"
                        } else {
                            "Не удалось загрузить следующую страницу"
                        }),
                )
                .child(error.clone())
                .child(self.button(
                    "catalog-retry",
                    "Повторить",
                    Command::Catalog(kind, CatalogAction::Retry),
                    true,
                    true,
                    cx,
                ));
        }
        let panel = if listing.is_empty_result() {
            panel
                .child("По этим фильтрам ничего не найдено.")
                .child("Попробуйте другой жанр или год.")
        } else if listing.loaded && listing.items.is_empty() {
            panel.child("На этой странице нет доступных карточек. Можно загрузить следующую.")
        } else {
            panel
        };
        if listing.loaded && listing.next_page.is_some() {
            panel.child(self.button(
                "catalog-load-more",
                "Загрузить ещё",
                Command::Catalog(kind, CatalogAction::LoadMore),
                false,
                true,
                cx,
            ))
        } else if listing.loaded && !listing.items.is_empty() {
            panel.child("Вы дошли до конца каталога.")
        } else {
            panel
        }
    }
}
