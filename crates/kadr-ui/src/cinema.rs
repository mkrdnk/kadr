//! Native cinema pages. All titles, artwork and progress come from the controller.

use gpui::{
    Context, Div, FontWeight, ObjectFit, Stateful, div, img, linear_color_stop, linear_gradient,
    prelude::*, px, rgb, rgba,
};

use super::{
    ACCENT, BACKGROUND, BORDER, CATALOG_ONBOARDING, Command, KadrView, LibraryRow, MUTED,
    MediaCard, MediaKind, PANEL, Section, SectionState, TEXT, UiEvent, cards::poster_title, icon,
};

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

    fn cinema_images_enabled(&self) -> bool {
        self.preferences_loaded
            && self.applied_catalog_enabled
            && self.model.preferences.catalog_enabled
    }

    pub(super) fn cinema_backdrop(
        &self,
        backdrop: Option<&str>,
        poster: Option<&str>,
        height: f32,
    ) -> Div {
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
        let action_enabled = match &command {
            Command::Emit(event) => self.accepts(event),
            _ => true,
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
                                action_enabled,
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
                                    true,
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

    pub(super) fn cinema_card(
        &self,
        media: &MediaCard,
        index: usize,
        width: f32,
        show_kind: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let event = UiEvent::OpenMedia(media.id.clone());
        let enabled = self.accepts(&event);
        self.poster_action(("cinema-card", index), Command::Emit(event), enabled, cx)
            .child(self.poster_image(media.poster_url.as_deref(), media.kind, width))
            .child(poster_title(media.title.clone()).mt_2())
            .when_some(card_metadata(media, show_kind), |card, metadata| {
                card.child(
                    div()
                        .mt_1()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child(metadata),
                )
            })
    }

    pub(super) fn cinema_poster(&self, media: &MediaCard, width: f32) -> Div {
        self.poster_image(media.poster_url.as_deref(), media.kind, width)
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
pub(super) struct Layout {
    width: f32,
    padding: f32,
    inner: f32,
    pub(super) columns: usize,
}

impl Layout {
    pub(super) fn new(width: f32) -> Self {
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

    pub(super) fn card_width(self) -> f32 {
        ((self.inner - 14. * (self.columns - 1) as f32) / self.columns as f32).max(1.)
    }
}

pub(super) fn page(layout: Layout) -> Div {
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

pub(super) fn page_heading(eyebrow: &'static str, title: &str) -> Div {
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

fn card_metadata(media: &MediaCard, show_kind: bool) -> Option<String> {
    if show_kind {
        Some(metadata(media))
    } else {
        media.year.map(|year| year.to_string())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, opened_at: i64, position: Option<u64>, duration: Option<u64>) -> LibraryRow {
        LibraryRow {
            id: id.into(),
            media_id: id.into(),
            episode: None,
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
            external_ratings: vec![],
            runtime_minutes: None,
            genres: vec![],
            certification: None,
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
    fn card_metadata_omits_redundant_kind_and_absent_year() {
        let mut item = media("Title", None, None);
        assert_eq!(card_metadata(&item, true).as_deref(), Some("Фильм"));
        assert_eq!(card_metadata(&item, false), None);
        item.year = Some(2024);
        item.kind = MediaKind::Series;
        assert_eq!(card_metadata(&item, true).as_deref(), Some("2024 · Сериал"));
        assert_eq!(card_metadata(&item, false).as_deref(), Some("2024"));
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
        assert_eq!(time_label(59), "59 сек");
        assert_eq!(time_label(3660), "1 ч 1 мин");
    }
}
