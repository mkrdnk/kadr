//! Native cinema pages. All titles, artwork and progress come from the controller.

use gpui::{
    Context, Div, FontWeight, KeyDownEvent, ObjectFit, Stateful, div, img, linear_color_stop,
    linear_gradient, prelude::*, px, rgb, rgba,
};

use super::{
    ACCENT, BACKGROUND, BORDER, CATALOG_ONBOARDING, Command, KadrView, LibraryRow, MUTED,
    MediaCard, MediaKind, PANEL, Section, SectionState, TEXT, UiEvent, icon,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CatalogOrder {
    #[default]
    Provider,
    Title,
    Newest,
    Rating,
}

impl CatalogOrder {
    pub(super) fn cycle(&mut self) {
        *self = match self {
            Self::Provider => Self::Title,
            Self::Title => Self::Newest,
            Self::Newest => Self::Rating,
            Self::Rating => Self::Provider,
        };
    }

    fn label(self) -> &'static str {
        match self {
            Self::Provider => "Порядок каталога",
            Self::Title => "По названию",
            Self::Newest => "Сначала новые",
            Self::Rating => "По рейтингу Trakt",
        }
    }
}

impl KadrView {
    pub(super) fn home(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let layout = Layout::new(width);
        page(layout)
            .child(page_heading("ВАШ КИНОЗАЛ", "Что посмотрим?"))
            .child(self.cinema_hero(layout.inner, cx))
            .child(self.cinema_row("Фильмы", Section::Movies, &self.model.movies, layout, cx))
            .child(self.cinema_row("Сериалы", Section::Shows, &self.model.shows, layout, cx))
            .when(
                self.model.movies.is_empty() && self.model.shows.is_empty(),
                |page| page.child(self.cinema_browse_state(cx)),
            )
    }

    pub(super) fn catalog_page(&self, section: Section, width: f32, cx: &mut Context<Self>) -> Div {
        let layout = Layout::new(width);
        let items = if section == Section::Shows {
            &self.model.shows
        } else {
            &self.model.movies
        };
        let items = ordered(items, self.catalog_order);
        page(layout)
            .child(page_heading("ОТКРЫВАЙТЕ НОВОЕ", section.title()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .py_2()
                    .mb_5()
                    .border_y_1()
                    .border_color(rgb(BORDER))
                    .child(self.button(
                        "cinema-order",
                        self.catalog_order.label(),
                        Command::CycleCatalogOrder,
                        false,
                        !items.is_empty(),
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(format!("В каталоге: {}", items.len())),
                    ),
            )
            .child(
                div()
                    .grid()
                    .grid_cols(layout.columns as u16)
                    .gap_x(px(14.))
                    .gap_y(px(24.))
                    .children(items.iter().enumerate().map(|(index, media)| {
                        self.cinema_card(media, index, layout.card_width(), cx)
                    })),
            )
            .when(
                items.is_empty() || !matches!(self.model.browse_state, SectionState::Empty(_)),
                |page| page.child(self.cinema_browse_state(cx)),
            )
    }

    pub(super) fn detail_page(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let layout = Layout::new(width);
        let Some(media) = &self.model.media else {
            return page(layout)
                .child(self.button(
                    "cinema-back",
                    "Назад",
                    Command::BackToResults,
                    false,
                    true,
                    cx,
                ))
                .child(
                    div()
                        .py_8()
                        .text_color(rgb(MUTED))
                        .child("Карточка ещё не загружена."),
                );
        };
        let narrow = layout.inner < 580.;
        let poster_width = if narrow { 120. } else { 180. };
        // Opening another title or selecting an episode invalidates playback in
        // the shell. Do not treat the presence of metadata as a playable source.
        let playable = self.model.can_play();
        let watch_event = self.model.card_watch_event();
        let mut copy = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(kicker(kind_label(media.kind)))
            .child(
                div()
                    .text_size(px(if narrow { 30. } else { 44. }))
                    .line_height(px(if narrow { 34. } else { 47. }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(media.title.clone()),
            )
            .when_some(
                media
                    .original_title
                    .as_ref()
                    .filter(|title| !title.trim().is_empty()),
                |copy, title| {
                    copy.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child(title.clone()),
                    )
                },
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .when_some(media.year, |meta, year| meta.child(year.to_string()))
                    .when_some(media.rating_tenths, |meta, rating| {
                        meta.child(
                            div()
                                .text_color(rgb(ACCENT))
                                .child(format!("★ {}", rating_label(rating))),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .mt_2()
                    .child(self.button(
                        "cinema-watch",
                        if media.kind == MediaKind::Series && !playable {
                            "Найти и смотреть"
                        } else {
                            "Смотреть"
                        },
                        Command::Emit(watch_event.clone()),
                        true,
                        self.model.accepts(&watch_event),
                        cx,
                    ))
                    .child(self.button(
                        "cinema-sources",
                        "Источники",
                        Command::Emit(UiEvent::FindSources),
                        false,
                        self.model.accepts(&UiEvent::FindSources),
                        cx,
                    ))
                    .child(self.button(
                        "cinema-favorite",
                        if self.model.media_favorite {
                            "В избранном"
                        } else {
                            "В избранное"
                        },
                        Command::Emit(UiEvent::ToggleFavorite),
                        self.model.media_favorite,
                        true,
                        cx,
                    )),
            );
        copy = copy.child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(if self.model.movie_watch_pending {
                    "Подбираем источник для просмотра. Чтобы отменить, вернитесь назад или перейдите в другой раздел.".into()
                } else if playable {
                    format!("Готово к открытию в {}", self.model.default_player.label())
                } else if media.kind == MediaKind::Movie {
                    format!(
                        "«Смотреть» подберёт источник и откроет {}. Другой вариант можно выбрать в «Источниках».",
                        self.model.default_player.label()
                    )
                } else {
                    format!(
                        "Воспроизведение в {} после выбора источника",
                        self.model.default_player.label()
                    )
                }),
        );

        let mut content = page(layout)
            .relative()
            .child(self.cinema_backdrop(
                media.backdrop_url.as_deref(),
                media.poster_url.as_deref(),
                480.,
            ))
            .child(
                div().relative().flex().items_start().child(
                    self.button(
                        "cinema-back",
                        format!("‹ {}", self.detail_from.title()),
                        Command::BackToResults,
                        false,
                        true,
                        cx,
                    )
                    .h(px(33.)),
                ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .when(narrow, |hero| hero.flex_col())
                    .when(!narrow, |hero| hero.items_end().min_h(px(360.)))
                    .gap(px(if narrow { 20. } else { 30. }))
                    .pt(px(38.))
                    .child(self.cinema_poster(media, poster_width))
                    .child(copy),
            )
            .when(self.model.movie_watch_pending, |content| {
                content.child(self.source_discovery_status(cx))
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .mt_8()
                    .pt_6()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(kicker(if media.kind == MediaKind::Series {
                        "О СЕРИАЛЕ"
                    } else {
                        "О ФИЛЬМЕ"
                    }))
                    .child(
                        div()
                            .max_w(px(760.))
                            .text_size(px(13.))
                            .line_height(px(22.))
                            .text_color(rgb(MUTED))
                            .child(
                                media
                                    .description
                                    .as_ref()
                                    .filter(|text| !text.trim().is_empty())
                                    .cloned()
                                    .unwrap_or_else(|| "Описание пока отсутствует.".into()),
                            ),
                    )
                    .when(!media.attribution.trim().is_empty(), |body| {
                        body.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(media.attribution.clone()),
                        )
                    }),
            );
        if media.kind == MediaKind::Series {
            content = content.child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .mt_8()
                    .child(kicker("СЕЗОНЫ И ЭПИЗОДЫ"))
                    .when(media.episodes.is_empty(), |episodes| {
                        episodes.child(
                            div().text_size(px(12.)).text_color(rgb(MUTED))
                                .child("Список эпизодов отсутствует. Сезон и эпизод можно указать при выборе файла."),
                        )
                    })
                    .children(media.episodes.iter().enumerate().map(|(index, episode)| {
                        self.button(
                            ("cinema-episode", index),
                            format!("S{:02}E{:02} · {}", episode.season, episode.number, episode.title),
                            Command::Emit(UiEvent::SelectEpisode { season: episode.season, number: episode.number }),
                            self.model.selected_episode == Some((episode.season, episode.number)),
                            !self.model.playback_active,
                            cx,
                        )
                    })),
            );
        }
        content
    }

    fn cinema_images_enabled(&self) -> bool {
        self.preferences_loaded
            && self.applied_catalog_enabled
            && self.model.preferences.catalog_enabled
    }

    fn cinema_backdrop(&self, backdrop: Option<&str>, poster: Option<&str>, height: f32) -> Div {
        div()
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(height))
            .overflow_hidden()
            .bg(rgb(PANEL))
            .when_some(
                backdrop_image_url(backdrop, poster, self.cinema_images_enabled()),
                |backdrop, url| {
                    backdrop.child(
                        img(url)
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .opacity(0.5),
                    )
                },
            )
            .child(div().absolute().inset_0().bg(linear_gradient(
                90.,
                linear_color_stop(rgba(0x101012f5), 0.),
                linear_color_stop(rgba(0x10101240), 1.),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x14141600), 0.),
                linear_color_stop(rgb(BACKGROUND), 1.),
            )))
    }

    fn cinema_hero(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let resume = recent_resume(&self.model.library);
        let spotlight = self.model.movies.first();
        let browse_primary =
            resume.is_none() && spotlight.is_none() && self.cinema_images_enabled();
        let browsing = matches!(self.model.browse_state, SectionState::Loading(_));
        let (title, backdrop, poster, eyebrow, meta, action, command) = if let Some(row) = resume {
            (
                row.title.clone(),
                None,
                row.poster_url.as_deref(),
                "ПРОДОЛЖИТЬ ПРОСМОТР",
                row.details.clone(),
                "Продолжить",
                Command::Emit(UiEvent::OpenLibrary(row.id.clone())),
            )
        } else if let Some(media) = spotlight {
            (
                media.title.clone(),
                media.backdrop_url.as_deref(),
                media.poster_url.as_deref(),
                "В ЦЕНТРЕ ВНИМАНИЯ",
                metadata(media),
                "Подробнее",
                Command::Emit(UiEvent::OpenMedia(media.id.clone())),
            )
        } else {
            (
                "Большое кино.\nВаш выбор.".into(),
                None,
                None,
                "ДОБРО ПОЖАЛОВАТЬ В KADR",
                format!("Фильмы и сериалы — в вашем плеере. {CATALOG_ONBOARDING}"),
                if browse_primary {
                    if browsing {
                        "Загружаем каталог…"
                    } else {
                        "Загрузить каталог"
                    }
                } else {
                    "Найти фильм"
                },
                if browse_primary {
                    Command::Emit(UiEvent::Browse)
                } else {
                    Command::OpenSearch
                },
            )
        };
        let progress_width = (width - 48.).clamp(1., 280.);
        div()
            .relative()
            .min_h(px(260.))
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .overflow_hidden()
            .child(self.cinema_backdrop(backdrop, poster, 340.))
            .when(backdrop.is_none() && poster.is_none(), |hero| {
                hero.child(
                    div()
                        .absolute()
                        .right(px(28.))
                        .top(px(44.))
                        .opacity(0.08)
                        .child(icon("film", 160., TEXT)),
                )
            })
            .child(
                div()
                    .relative()
                    .p(px(if width < 500. { 24. } else { 32. }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(kicker(eyebrow))
                    .child(
                        div()
                            .max_w(px(650.))
                            .text_size(px(if width < 500. { 28. } else { 36. }))
                            .line_height(px(40.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(!meta.is_empty(), |hero| {
                        hero.child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(meta))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .mt_1()
                            .child(self.button(
                                "cinema-hero-action",
                                action,
                                command,
                                true,
                                !browse_primary || !browsing,
                                cx,
                            ))
                            .when(browse_primary, |actions| {
                                actions.child(self.button(
                                    "cinema-hero-search",
                                    "Поиск",
                                    Command::OpenSearch,
                                    false,
                                    true,
                                    cx,
                                ))
                            })
                            .when(resume.is_some(), |actions| {
                                actions.child(self.button(
                                    "cinema-hero-history",
                                    "История",
                                    Command::Navigate(Section::History),
                                    false,
                                    true,
                                    cx,
                                ))
                            }),
                    )
                    .when_some(
                        resume.and_then(measured_progress),
                        |hero, (position, duration)| {
                            hero.child(
                                div()
                                    .w(px(progress_width))
                                    .mt_2()
                                    .child(
                                        div()
                                            .flex()
                                            .justify_between()
                                            .text_size(px(10.))
                                            .text_color(rgb(MUTED))
                                            .child(format!("Просмотрено {}", time_label(position)))
                                            .child(format!(
                                                "Осталось {}",
                                                time_label(duration - position)
                                            )),
                                    )
                                    .child(
                                        div()
                                            .mt_2()
                                            .h(px(3.))
                                            .rounded_full()
                                            .bg(rgba(0xffffff29))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .rounded_full()
                                                    .bg(rgb(ACCENT))
                                                    .w(px(progress_width * position as f32
                                                        / duration as f32)),
                                            ),
                                    ),
                            )
                        },
                    ),
            )
    }

    fn cinema_row(
        &self,
        title: &'static str,
        section: Section,
        items: &[MediaCard],
        layout: Layout,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .mt(px(27.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .mb_3()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(self.button(
                        if section == Section::Shows {
                            "cinema-all-shows"
                        } else {
                            "cinema-all-movies"
                        },
                        "Все ›",
                        Command::Navigate(section),
                        false,
                        true,
                        cx,
                    )),
            )
            .when(!items.is_empty(), |row| {
                row.child(
                    div()
                        .grid()
                        .grid_cols(layout.columns as u16)
                        .gap(px(14.))
                        .children(items.iter().take(layout.columns).enumerate().map(
                            |(index, media)| {
                                // Each row has a distinct ID scope.
                                div().id((section.id(), index)).child(self.cinema_card(
                                    media,
                                    index,
                                    layout.card_width(),
                                    cx,
                                ))
                            },
                        )),
                )
            })
            .when(items.is_empty(), |row| {
                row.child(
                    div()
                        .py_4()
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                        .child("Здесь появятся названия из каталога."),
                )
            })
    }

    fn cinema_card(
        &self,
        media: &MediaCard,
        index: usize,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let command = Command::Emit(UiEvent::OpenMedia(media.id.clone()));
        let keyboard_command = command.clone();
        div()
            .id(("cinema-card", index))
            .group("cinema-card")
            .min_w_0()
            .rounded_md()
            .tab_index(0)
            .cursor_pointer()
            .focus(|style| style.bg(rgb(BORDER)))
            .child(
                self.cinema_poster(media, width).child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .bg(rgba(0x0c0c0eb5))
                        .opacity(0.)
                        .group_hover("cinema-card", |style| style.opacity(1.))
                        .child(
                            div()
                                .size(px(35.))
                                .rounded_full()
                                .bg(rgb(ACCENT))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon("play", 18., BACKGROUND)),
                        )
                        .child(div().text_size(px(10.)).child("Подробнее")),
                ),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(px(12.))
                    .truncate()
                    .child(media.title.clone()),
            )
            .child(
                div()
                    .mt_1()
                    .text_size(px(10.))
                    .text_color(rgb(MUTED))
                    .child(metadata(media)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.run_command(&command, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.is_held {
                    cx.stop_propagation();
                    this.run_command(&keyboard_command, cx);
                }
            }))
    }

    fn cinema_poster(&self, media: &MediaCard, width: f32) -> Div {
        div()
            .relative()
            .w(px(width))
            .h(px(width * 1.5))
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgba(0xffffff12))
            .bg(rgb(PANEL))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(
                        if media.kind == MediaKind::Series {
                            "tv"
                        } else {
                            "film"
                        },
                        28.,
                        MUTED,
                    )),
            )
            .when_some(
                image_url(media.poster_url.as_deref(), self.cinema_images_enabled()),
                |poster, url| {
                    poster.child(img(url).relative().size_full().object_fit(ObjectFit::Cover))
                },
            )
            .when_some(media.rating_tenths, |poster, rating| {
                poster.child(
                    div()
                        .absolute()
                        .right(px(6.))
                        .top(px(6.))
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgba(0xffffff18))
                        .bg(rgba(0x111114d9))
                        .text_size(px(9.))
                        .child(format!("★ {}", rating_label(rating))),
                )
            })
    }

    fn cinema_browse_state(&self, cx: &mut Context<Self>) -> Div {
        let loading = matches!(self.model.browse_state, SectionState::Loading(_));
        div()
            .mt_6()
            .p_5()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(14.))
                    .child(self.model.browse_state.heading()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
                    .child(self.model.browse_state.message().to_owned()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.button(
                        "cinema-load",
                        if loading {
                            "Загрузка…"
                        } else {
                            "Загрузить каталог"
                        },
                        Command::Emit(UiEvent::Browse),
                        true,
                        !loading
                            && self.applied_catalog_enabled
                            && self.model.preferences.catalog_enabled,
                        cx,
                    ))
                    .child(self.button(
                        "cinema-search",
                        "Поиск",
                        Command::OpenSearch,
                        false,
                        true,
                        cx,
                    )),
            )
    }
}

#[derive(Clone, Copy)]
struct Layout {
    width: f32,
    padding: f32,
    inner: f32,
    columns: usize,
}

impl Layout {
    fn new(width: f32) -> Self {
        let width = if width.is_finite() {
            width.clamp(1., 1400.)
        } else {
            1.
        };
        let padding = if width < 360. {
            16.
        } else if width < 960. {
            24.
        } else {
            32.
        };
        let inner = (width - padding * 2.).max(1.);
        let columns = (((inner + 14.) / 124.).floor() as usize).clamp(1, 6);
        Self {
            width,
            padding,
            inner,
            columns,
        }
    }

    fn card_width(self) -> f32 {
        ((self.inner - 14. * (self.columns - 1) as f32) / self.columns as f32).max(1.)
    }
}

fn page(layout: Layout) -> Div {
    div()
        .w(px(layout.width))
        .max_w_full()
        .mx_auto()
        .min_w_0()
        .px(px(layout.padding))
        .pt(px(29.))
        .pb(px(48.))
        .text_color(rgb(TEXT))
}

fn kicker(text: impl Into<String>) -> Div {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(text.into())
}

fn page_heading(eyebrow: &'static str, title: &str) -> Div {
    div().mb_5().child(kicker(eyebrow)).child(
        div()
            .mt_1()
            .text_size(px(27.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(title.to_owned()),
    )
}

fn kind_label(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie => "Фильм",
        MediaKind::Series => "Сериал",
    }
}

fn metadata(media: &MediaCard) -> String {
    match media.year {
        Some(year) => format!("{year} · {}", kind_label(media.kind)),
        None => kind_label(media.kind).into(),
    }
}

fn rating_label(rating: u8) -> String {
    format!("Trakt {}.{:01}", rating / 10, rating % 10)
}

fn image_url(url: Option<&str>, enabled: bool) -> Option<String> {
    url.filter(|url| enabled && (url.starts_with("https://") || url.starts_with("http://")))
        .map(str::to_owned)
}

fn backdrop_image_url(
    backdrop: Option<&str>,
    poster: Option<&str>,
    enabled: bool,
) -> Option<String> {
    image_url(backdrop, enabled).or_else(|| image_url(poster, enabled))
}

fn measured_progress(row: &LibraryRow) -> Option<(u64, u64)> {
    match (row.position, row.duration) {
        (Some(position), Some(duration))
            if !row.completed && position > 0 && position < duration =>
        {
            Some((position, duration))
        }
        _ => None,
    }
}

fn recent_resume(rows: &[LibraryRow]) -> Option<&LibraryRow> {
    rows.iter()
        .filter(|row| measured_progress(row).is_some())
        .max_by_key(|row| row.opened_at)
}

fn time_label(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds} сек")
    } else if seconds < 3600 {
        format!("{} мин", seconds / 60)
    } else {
        format!("{} ч {} мин", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn ordered(items: &[MediaCard], order: CatalogOrder) -> Vec<&MediaCard> {
    let mut items: Vec<_> = items.iter().collect();
    match order {
        CatalogOrder::Provider => {}
        CatalogOrder::Title => items.sort_by_cached_key(|media| media.title.to_lowercase()),
        CatalogOrder::Newest => items.sort_by_key(|media| std::cmp::Reverse(media.year)),
        CatalogOrder::Rating => items.sort_by_key(|media| std::cmp::Reverse(media.rating_tenths)),
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, opened_at: i64, position: Option<u64>, duration: Option<u64>) -> LibraryRow {
        LibraryRow {
            id: id.into(),
            title: id.into(),
            details: String::new(),
            favorite: false,
            poster_url: None,
            opened_at: Some(opened_at),
            completed: false,
            position,
            duration,
        }
    }

    fn media(title: &str, year: Option<u16>, rating_tenths: Option<u8>) -> MediaCard {
        MediaCard {
            id: title.into(),
            title: title.into(),
            original_title: None,
            year,
            rating_tenths,
            description: None,
            poster_url: None,
            backdrop_url: None,
            kind: MediaKind::Movie,
            episodes: vec![],
            attribution: String::new(),
        }
    }

    #[test]
    fn resume_requires_real_incomplete_measurements_and_uses_recency() {
        let mut completed = row("completed", 500, Some(12), Some(30));
        completed.completed = true;
        let rows = vec![
            row("old", 10, Some(12), Some(30)),
            row("recent", 20, Some(15), Some(30)),
            row("unmeasured", 100, None, None),
            row("unknown duration", 200, Some(15), None),
            row("past end", 300, Some(40), Some(30)),
            row("zero", 400, Some(0), Some(30)),
            completed,
        ];
        assert_eq!(recent_resume(&rows).unwrap().id, "recent");
        assert!(recent_resume(&[]).is_none());
        assert!(measured_progress(&row("zero duration", 1, Some(1), Some(0))).is_none());
    }

    #[test]
    fn catalog_sort_is_stable_and_missing_values_are_last() {
        let items = vec![
            media("Beta", None, None),
            media("alpha", Some(2020), Some(80)),
            media("Gamma", Some(2024), Some(80)),
        ];
        let titles = |order| {
            ordered(&items, order)
                .iter()
                .map(|media| media.title.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(CatalogOrder::Provider), ["Beta", "alpha", "Gamma"]);
        assert_eq!(titles(CatalogOrder::Title), ["alpha", "Beta", "Gamma"]);
        assert_eq!(titles(CatalogOrder::Newest), ["Gamma", "alpha", "Beta"]);
        assert_eq!(titles(CatalogOrder::Rating), ["alpha", "Gamma", "Beta"]);
        let mut order = CatalogOrder::default();
        for _ in 0..4 {
            order.cycle();
        }
        assert_eq!(order, CatalogOrder::default());
    }

    #[test]
    fn responsive_cards_fit_available_content_width() {
        for width in [1., 240., 360., 540., 900., 1120., 1800.] {
            let layout = Layout::new(width);
            assert!((1..=6).contains(&layout.columns));
            assert!(layout.card_width() > 0.);
            let used =
                layout.card_width() * layout.columns as f32 + 14. * (layout.columns - 1) as f32;
            assert!((used - layout.inner).abs() < 0.01);
        }
    }

    #[test]
    fn image_requests_require_opt_in_and_public_urls() {
        assert_eq!(
            image_url(Some("https://example.org/poster.jpg"), true),
            Some("https://example.org/poster.jpg".into())
        );
        assert!(image_url(Some("https://example.org/poster.jpg"), false).is_none());
        assert!(image_url(Some("file:///private/poster.jpg"), true).is_none());
        assert!(image_url(None, true).is_none());
    }

    #[test]
    fn backdrop_prefers_wide_image_and_preserves_poster_fallback_and_privacy_gate() {
        let wide = Some("https://example.org/backdrop.jpg");
        let poster = Some("https://example.org/poster.jpg");
        assert_eq!(backdrop_image_url(wide, poster, true).as_deref(), wide);
        assert_eq!(backdrop_image_url(None, poster, true).as_deref(), poster);
        assert_eq!(
            backdrop_image_url(Some("file:///private/image.jpg"), poster, true).as_deref(),
            poster
        );
        assert!(backdrop_image_url(wide, poster, false).is_none());
        assert!(backdrop_image_url(None, None, true).is_none());
    }

    #[test]
    fn cinema_images_require_loaded_acknowledged_preferences_and_no_draft_opt_out() {
        let mut view = KadrView::default();
        assert!(!view.cinema_images_enabled());
        view.applied_catalog_enabled = true;
        assert!(!view.cinema_images_enabled());
        view.preferences_loaded = true;
        assert!(view.cinema_images_enabled());
        view.model.preferences.catalog_enabled = false;
        assert!(!view.cinema_images_enabled());
        view.model.preferences.catalog_enabled = true;
        view.applied_catalog_enabled = false;
        assert!(!view.cinema_images_enabled());
    }

    #[test]
    fn display_values_do_not_invent_metadata_or_round_to_hours() {
        assert_eq!(metadata(&media("Title", None, None)), "Фильм");
        assert_eq!(rating_label(87), "Trakt 8.7");
        assert_eq!(rating_label(100), "Trakt 10.0");
        assert_eq!(CatalogOrder::Rating.label(), "По рейтингу Trakt");
        assert_eq!(time_label(59), "59 сек");
        assert_eq!(time_label(3660), "1 ч 1 мин");
    }
}
