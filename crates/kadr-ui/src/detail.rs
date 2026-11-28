//! Movie and series details. Playback capabilities and watch history are controller-owned.

use gpui::{Context, Div, FontWeight, Stateful, div, prelude::*, px, relative, rgb};

use super::{
    ACCENT, BACKGROUND, BORDER, Command, DIM, ELEVATED, EpisodeRow, HOVER, KadrView, MUTED,
    MediaCard, MediaKind, PANEL, PlayerChoice, Section, SourceRow, TEXT, UiEvent, body, icon,
};

/// A provider-labelled, already formatted rating. Missing ratings are omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailRating {
    pub provider: String,
    pub value: String,
}

/// Actual player measurements in seconds, or an explicitly stored completion.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackProgress {
    pub position: Option<u64>,
    pub duration: Option<u64>,
    pub completed: bool,
}

impl PlaybackProgress {
    fn is_resumable(&self) -> bool {
        !self.completed
            && self.position.is_some_and(|position| {
                position > 0 && self.duration.is_none_or(|duration| position < duration)
            })
    }

    fn measured(&self) -> Option<(u64, u64)> {
        match (self.position, self.duration) {
            (Some(position), Some(duration))
                if !self.completed && position > 0 && position < duration =>
            {
                Some((position, duration))
            }
            _ => None,
        }
    }

    fn label(&self) -> Option<String> {
        if self.completed {
            Some("Просмотрено".into())
        } else {
            self.measured().map(|(position, duration)| {
                format!("{} из {}", time_label(position), time_label(duration))
            })
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpisodeProgress {
    pub season: u16,
    pub number: u16,
    pub progress: PlaybackProgress,
}

/// Transient watch preferences are independent of the persisted Settings draft.
#[derive(Default)]
pub(super) struct DetailState {
    pub selected_season: Option<u16>,
    pub season_menu: bool,
    pub progress: Option<PlaybackProgress>,
    pub episode_progress: Vec<EpisodeProgress>,
    pub preferred_resolution: Option<String>,
    pub preferred_language: Option<String>,
    pub subtitles_off: bool,
}

impl KadrView {
    pub(super) fn detail_page(&self, width: f32, cx: &mut Context<Self>) -> Div {
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
        let narrow = inner < 620.;
        let page = div()
            .relative()
            .w(px(width))
            .max_w_full()
            .mx_auto()
            .min_w_0()
            .px(px(padding))
            .pt_6()
            .pb_8()
            .text_color(rgb(TEXT));
        let back = self.button(
            "detail-back",
            format!("‹ {}", self.detail_from.title()),
            Command::BackToResults,
            false,
            true,
            cx,
        );
        let Some(media) = &self.model.media else {
            return page.child(back).child(
                div()
                    .py_8()
                    .text_color(rgb(MUTED))
                    .child("Карточка ещё не загружена."),
            );
        };

        let ratings = rating_values(media);
        let metadata = metadata(media);
        let copy = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(kicker(match media.kind {
                MediaKind::Movie => "ФИЛЬМ",
                MediaKind::Series => "СЕРИАЛ",
            }))
            .child(
                div()
                    .text_size(px(if narrow { 30. } else { 42. }))
                    .line_height(px(if narrow { 35. } else { 47. }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(media.title.clone()),
            )
            .when_some(distinct_original_title(media), |copy, title| {
                copy.child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(MUTED))
                        .child(title.to_owned()),
                )
            })
            .when(!metadata.is_empty(), |copy| {
                copy.child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(19.))
                        .text_color(rgb(MUTED))
                        .child(metadata),
                )
            })
            .when(!ratings.is_empty(), |copy| {
                copy.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(ratings.into_iter().map(|rating| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(rgb(PANEL))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .child(
                                    div()
                                        .text_size(px(15.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(ACCENT))
                                        .child(rating.value),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(MUTED))
                                        .child(rating.provider),
                                )
                        })),
                )
            })
            .when_some(self.detail_current_progress(media), |copy, progress| {
                copy.when_some(progress.label(), |copy, label| {
                    copy.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .max_w(px(390.))
                            .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(label))
                            .when_some(progress.measured(), |progress, (position, duration)| {
                                progress.child(progress_bar(position, duration))
                            }),
                    )
                })
            })
            .when_some(
                media
                    .description
                    .as_deref()
                    .filter(|text| !text.trim().is_empty()),
                |copy, description| {
                    copy.child(
                        div()
                            .mt_1()
                            .max_w(px(750.))
                            .text_size(px(13.))
                            .line_height(px(21.))
                            .text_color(rgb(MUTED))
                            .child(description.trim().to_owned()),
                    )
                },
            )
            .when(!media.attribution.trim().is_empty(), |copy| {
                copy.child(
                    div()
                        .max_w(px(750.))
                        .text_size(px(10.))
                        .line_height(px(16.))
                        .text_color(rgb(MUTED))
                        .child(media.attribution.trim().to_owned()),
                )
            });

        page.child(self.cinema_backdrop(
            media.backdrop_url.as_deref(),
            media.poster_url.as_deref(),
            if narrow { 430. } else { 500. },
        ))
        .child(div().relative().flex().items_start().child(back))
        .child(
            div()
                .relative()
                .flex()
                .items_start()
                .when(narrow, |hero| hero.flex_col())
                .gap(px(if narrow { 20. } else { 30. }))
                .pt(px(if narrow { 24. } else { 40. }))
                .child(self.cinema_poster(media, if narrow { 120. } else { 190. }))
                .child(copy),
        )
        .child(self.detail_playback_options(narrow, cx))
        .child(self.detail_watch_actions(media, cx))
        .when(media.kind == MediaKind::Series, |page| {
            page.child(self.detail_episodes(media, narrow, cx))
        })
    }

    fn detail_current_progress(&self, media: &MediaCard) -> Option<&PlaybackProgress> {
        if media.kind == MediaKind::Series {
            let (season, number) = self.model.selected_episode?;
            self.detail
                .episode_progress
                .iter()
                .find(|row| row.season == season && row.number == number)
                .map(|row| &row.progress)
        } else {
            self.detail.progress.as_ref()
        }
    }

    fn detail_watch_actions(&self, media: &MediaCard, cx: &mut Context<Self>) -> Div {
        let selected = self.model.selected_episode;
        let needs_episode = media.kind == MediaKind::Series && selected.is_none();
        let watch_event = UiEvent::WatchDetail {
            resume: true,
            preferences: self.playback_options(),
        };
        let resumable = self
            .detail_current_progress(media)
            .is_some_and(PlaybackProgress::is_resumable);
        let label = if self.model.movie_watch_pending {
            "Подбираем источник…".into()
        } else if needs_episode {
            "Выберите эпизод".into()
        } else {
            let verb = if resumable {
                "Продолжить"
            } else {
                "Смотреть"
            };
            match selected.filter(|_| media.kind == MediaKind::Series) {
                Some((season, number)) => format!("{verb} · S{season:02}E{number:02}"),
                None => verb.into(),
            }
        };
        div().flex().flex_col().gap_2().mt_1().child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    self.button(
                        "detail-watch",
                        label,
                        Command::WatchDetail,
                        true,
                        !needs_episode && self.model.accepts(&watch_event),
                        cx,
                    )
                    .h(px(42.)),
                )
                .when(resumable, |actions| {
                    actions.child(self.button(
                        "detail-start-over",
                        "С начала",
                        Command::WatchDetailFromStart,
                        false,
                        !needs_episode && self.model.accepts(&watch_event),
                        cx,
                    ))
                })
                .child(self.button(
                    "detail-favorite",
                    if self.model.media_favorite {
                        "✓ В избранном"
                    } else {
                        "+ В избранное"
                    },
                    Command::Emit(UiEvent::ToggleFavorite),
                    false,
                    self.model.accepts(&UiEvent::ToggleFavorite),
                    cx,
                )),
        )
    }

    fn detail_playback_options(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let enabled =
            !self.model.playback_active && !self.model.preparing && !self.model.movie_watch_pending;
        let resolution = self.detail.preferred_resolution.as_deref();
        let language = self.detail.preferred_language.as_deref();
        let quality_options =
            preference_options(source_resolutions(&self.model.sources), resolution);
        let language_options = preference_options(source_languages(&self.model.sources), language);
        let subtitles_supported = self.model.default_player != PlayerChoice::Browser;
        div()
            .relative()
            .flex()
            .flex_col()
            .gap_4()
            .mt_6()
            .child(
                div()
                    .flex()
                    .when(narrow, |options| options.flex_col())
                    .gap_5()
                    .child(option_group("Качество источника").child(
                        div().flex().flex_wrap().gap_2().children(
                            quality_options.into_iter().enumerate().map(
                                |(index, (value, label))| {
                                    self.button(
                                        ("detail-quality", index),
                                        label,
                                        Command::SetDetailResolution(value.clone()),
                                        value.as_deref() == resolution,
                                        enabled,
                                        cx,
                                    )
                                    .h(px(30.))
                                    .px_3()
                                    .text_size(px(11.))
                                },
                            ),
                        ),
                    ))
                    .child(option_group("Предпочтительный язык аудио").child(
                        div().flex().flex_wrap().gap_2().children(
                            language_options.into_iter().enumerate().map(
                                |(index, (value, label))| {
                                    self.button(
                                        ("detail-language", index),
                                        label,
                                        Command::SetDetailLanguage(value.clone()),
                                        value.as_deref() == language,
                                        enabled,
                                        cx,
                                    )
                                    .h(px(30.))
                                    .px_3()
                                    .text_size(px(11.))
                                },
                            ),
                        ),
                    )),
            )
            .when(subtitles_supported, |options| {
                options.child(
                    option_group("Субтитры").child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                self.button(
                                    "detail-subtitles-player",
                                    "В плеере",
                                    Command::SetDetailSubtitlesOff(false),
                                    !self.detail.subtitles_off,
                                    enabled,
                                    cx,
                                )
                                .h(px(30.))
                                .px_3()
                                .text_size(px(11.)),
                            )
                            .child(
                                self.button(
                                    "detail-subtitles-off",
                                    "Выключены",
                                    Command::SetDetailSubtitlesOff(true),
                                    self.detail.subtitles_off,
                                    enabled,
                                    cx,
                                )
                                .h(px(30.))
                                .px_3()
                                .text_size(px(11.)),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(MUTED))
                                    .child("Выбор доступных дорожек — в плеере."),
                            ),
                    ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(body(if self.model.can_play() {
                        "Источник подготовлен"
                    } else {
                        "Источник: автоматически"
                    }))
                    .child(self.button(
                        "detail-manual-source",
                        "Выбрать…",
                        Command::Emit(UiEvent::FindSources),
                        false,
                        self.model.accepts(&UiEvent::FindSources),
                        cx,
                    ))
                    .when(
                        self.model.sources.is_empty()
                            && !self.source_work_active()
                            && !self.model.can_play(),
                        |row| {
                            row.child(self.button(
                                "detail-discover",
                                "Найти варианты",
                                Command::Emit(UiEvent::DiscoverSources),
                                false,
                                self.model.accepts(&UiEvent::DiscoverSources),
                                cx,
                            ))
                        },
                    ),
            )
            .when(!self.model.sources.is_empty(), |options| {
                options.child(body(
                    "Качество и язык заявлены источниками. Наличие дорожек не подтверждено.",
                ))
            })
            .when(
                !self.source_work_active()
                    && self.model.sources.is_empty()
                    && self
                        .model
                        .source_discovery
                        .as_ref()
                        .is_some_and(|discovery| discovery.pending == 0),
                |options| options.child(body("Подходящих источников пока нет.")),
            )
            .when(self.source_work_active(), |options| {
                options.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(body(if self.model.movie_watch_pending {
                            "Подбираем источник…"
                        } else {
                            "Ищем источники…"
                        }))
                        .child(self.button(
                            "cancel-inline-search",
                            "Отменить поиск",
                            Command::Emit(UiEvent::Cancel(Section::Sources)),
                            false,
                            true,
                            cx,
                        )),
                )
            })
    }

    fn detail_episodes(&self, media: &MediaCard, narrow: bool, cx: &mut Context<Self>) -> Div {
        let seasons = seasons(&media.episodes);
        let selected_season = selected_season(
            &seasons,
            self.detail.selected_season,
            self.model.selected_episode,
        );
        let episodes = season_episodes(&media.episodes, selected_season);
        div()
            .relative()
            .flex()
            .flex_col()
            .gap_3()
            .mt_6()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(kicker("ЭПИЗОДЫ"))
                    .when_some(selected_season, |header, season| {
                        header.child(self.button(
                            "detail-season-selector",
                            format!("{}  ▾", season_label(season)),
                            Command::ToggleDetailSeasons,
                            false,
                            !self.model.playback_active,
                            cx,
                        ).h(px(32.)).text_size(px(12.)))
                    })
                    .when(selected_season.is_some(), |heading| {
                        heading.child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(episode_count(episodes.len())),
                        )
                    }),
            )
            .when(self.detail.season_menu, |section| section.child(
                div().id("detail-season-options").max_h(px(168.)).overflow_y_scroll()
                .flex().flex_wrap().gap_2().children(
                    seasons.into_iter().map(|season| {
                        self.button(
                            ("detail-season", usize::from(season)),
                            season_label(season),
                            Command::SelectDetailSeason(season),
                            Some(season) == selected_season,
                            true,
                            cx,
                        )
                        .h(px(32.))
                        .text_size(px(12.))
                    }),
                ),
            ))
            .when(episodes.is_empty(), |section| {
                section.child(
                    div()
                        .py_3()
                        .text_size(px(12.))
                        .line_height(px(19.))
                        .text_color(rgb(MUTED))
                        .child("Список эпизодов отсутствует. Сезон и эпизод можно указать при выборе файла источника."),
                )
            })
            .child(div().flex().flex_col().children(episodes.into_iter().map(
                |episode| self.detail_episode_row(episode, narrow, cx),
            )))
    }

    fn detail_episode_row(
        &self,
        episode: &EpisodeRow,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.model.selected_episode == Some((episode.season, episode.number));
        let event = UiEvent::SelectEpisode {
            season: episode.season,
            number: episode.number,
        };
        let progress = self
            .detail
            .episode_progress
            .iter()
            .find(|row| row.season == episode.season && row.number == episode.number)
            .map(|row| &row.progress);
        self.action(
            (
                "detail-episode",
                (usize::from(episode.season) << 16) | usize::from(episode.number),
            ),
            Command::Emit(event.clone()),
            !self.model.playback_active && self.model.accepts(&event),
            cx,
        )
        .flex()
        .items_center()
        .gap_3()
        .min_h(px(48.))
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(rgb(BORDER))
        .bg(rgb(if selected { ELEVATED } else { BACKGROUND }))
        .hover(|style| style.bg(rgb(HOVER)))
        .child(
            div()
                .w(px(if narrow { 28. } else { 38. }))
                .flex_shrink_0()
                .text_size(px(14.))
                .text_color(rgb(if selected { ACCENT } else { MUTED }))
                .child(format!("{:02}", episode.number)),
        )
        .when(!narrow, |row| {
            row.child(
                div()
                    .w(px(58.))
                    .h(px(33.))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .rounded_sm()
                    .bg(rgb(PANEL))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("tv", 14., DIM))
                    .when_some(
                        episode.screenshot_url.as_ref().filter(|_| {
                            self.preferences_loaded
                                && self.applied_catalog_enabled
                                && self.model.preferences.catalog_enabled
                        }),
                        |preview, url| {
                            preview.relative().child(
                                gpui::img(url.clone())
                                    .absolute()
                                    .inset_0()
                                    .size_full()
                                    .object_fit(gpui::ObjectFit::Cover),
                            )
                        },
                    ),
            )
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(if selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(if episode.title.trim().is_empty() {
                            format!("Эпизод {}", episode.number)
                        } else {
                            episode.title.clone()
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .when_some(
                            episode.runtime_minutes.filter(|minutes| *minutes > 0),
                            |meta, minutes| meta.child(format!("{minutes} мин")),
                        )
                        .when_some(progress.and_then(PlaybackProgress::label), |meta, label| {
                            meta.child(label)
                        }),
                )
                .when_some(
                    progress.and_then(PlaybackProgress::measured),
                    |row, (position, duration)| {
                        row.child(
                            div()
                                .max_w(px(180.))
                                .mt_1()
                                .child(progress_bar(position, duration)),
                        )
                    },
                ),
        )
        .when(selected, |row| {
            row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(10.))
                    .text_color(rgb(ACCENT))
                    .when(!narrow, |status| status.child("Выбран"))
                    .child(icon("play", 14., ACCENT)),
            )
        })
        .when(progress.is_some_and(|progress| progress.completed), |row| {
            row.child(icon("check", 16., ACCENT))
        })
    }
}

fn kicker(label: &'static str) -> Div {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(label)
}

fn option_group(label: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(label))
}

fn progress_bar(position: u64, duration: u64) -> Div {
    let ratio = (position as f64 / duration.max(1) as f64).clamp(0., 1.) as f32;
    div()
        .h(px(3.))
        .w_full()
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(BORDER))
        .child(div().h_full().w(relative(ratio)).bg(rgb(ACCENT)))
}

fn distinct_original_title(media: &MediaCard) -> Option<&str> {
    let original = media.original_title.as_deref()?.trim();
    let normalized = |title: &str| {
        title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    (!original.is_empty() && normalized(original) != normalized(&media.title)).then_some(original)
}

fn metadata(media: &MediaCard) -> String {
    let mut parts = Vec::new();
    if let Some(year) = media.year {
        parts.push(year.to_string());
    }
    if let Some(minutes) = media.runtime_minutes.filter(|minutes| *minutes > 0) {
        parts.push(format!("{minutes} мин"));
    }
    parts.extend(
        media
            .genres
            .iter()
            .map(|genre| genre.trim())
            .filter(|genre| !genre.is_empty())
            .map(str::to_owned),
    );
    if let Some(certification) = media
        .certification
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
    {
        parts.push(certification.to_owned());
    }
    parts.join(" · ")
}

fn rating_values(media: &MediaCard) -> Vec<DetailRating> {
    let mut ratings = Vec::new();
    if let Some(rating) = media.rating_tenths.filter(|rating| *rating <= 100) {
        ratings.push(DetailRating {
            provider: "Trakt".into(),
            value: format!("{}.{}/10", rating / 10, rating % 10),
        });
    }
    for rating in &media.external_ratings {
        let provider = rating.provider.trim();
        let value = rating.value.trim();
        if !provider.is_empty()
            && !value.is_empty()
            && !ratings
                .iter()
                .any(|existing| existing.provider.eq_ignore_ascii_case(provider))
        {
            ratings.push(DetailRating {
                provider: provider.into(),
                value: value.into(),
            });
        }
    }
    ratings
}

fn preference_options(
    choices: Vec<(String, String)>,
    selected: Option<&str>,
) -> Vec<(Option<String>, String)> {
    let mut options = vec![(None, "Автоматически".into())];
    options.extend(
        choices
            .into_iter()
            .map(|(value, label)| (Some(value), label)),
    );
    if let Some(selected) = selected
        && !options
            .iter()
            .any(|(value, _)| value.as_deref() == Some(selected))
    {
        options.push((Some(selected.into()), format!("{selected} · предпочтение")));
    }
    options
}

fn source_resolutions(sources: &[SourceRow]) -> Vec<(String, String)> {
    let mut resolutions: Vec<_> = sources
        .iter()
        .filter_map(|source| source.resolution)
        .filter(|resolution| *resolution > 0)
        .collect();
    resolutions.sort_unstable();
    resolutions.dedup();
    resolutions
        .into_iter()
        .map(|resolution| {
            let value = format!("{resolution}p");
            let label = if resolution == 2160 {
                "4K".into()
            } else {
                value.clone()
            };
            (value, label)
        })
        .collect()
}

fn source_languages(sources: &[SourceRow]) -> Vec<(String, String)> {
    let mut languages: Vec<_> = sources
        .iter()
        .flat_map(|source| &source.languages)
        .map(|language| language.trim().to_lowercase())
        .filter(|language| !language.is_empty())
        .collect();
    languages.sort();
    languages.dedup();
    languages
        .into_iter()
        .map(|language| (language.clone(), language))
        .collect()
}

fn seasons(episodes: &[EpisodeRow]) -> Vec<u16> {
    let mut seasons: Vec<_> = episodes.iter().map(|episode| episode.season).collect();
    seasons.sort_unstable_by_key(|season| (*season == 0, *season));
    seasons.dedup();
    seasons
}

fn selected_season(
    seasons: &[u16],
    selected_season: Option<u16>,
    selected_episode: Option<(u16, u16)>,
) -> Option<u16> {
    selected_season
        .filter(|season| seasons.contains(season))
        .or_else(|| {
            selected_episode
                .map(|(season, _)| season)
                .filter(|season| seasons.contains(season))
        })
        .or_else(|| seasons.first().copied())
}

fn season_episodes(episodes: &[EpisodeRow], season: Option<u16>) -> Vec<&EpisodeRow> {
    let mut rows: Vec<_> = episodes
        .iter()
        .filter(|episode| Some(episode.season) == season)
        .collect();
    rows.sort_by_key(|episode| episode.number);
    rows
}

fn season_label(season: u16) -> String {
    if season == 0 {
        "Спецвыпуски".into()
    } else {
        format!("Сезон {season}")
    }
}

fn episode_count(count: usize) -> String {
    let suffix = match (count % 100, count % 10) {
        (11..=14, _) => "эпизодов",
        (_, 1) => "эпизод",
        (_, 2..=4) => "эпизода",
        _ => "эпизодов",
    };
    format!("{count} {suffix}")
}

fn time_label(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} сек"),
        60..3600 => format!("{} мин", seconds / 60),
        _ => format!("{} ч {} мин", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> MediaCard {
        MediaCard {
            id: "test:movie".into(),
            title: "Test title".into(),
            original_title: None,
            year: None,
            description: None,
            poster_url: None,
            backdrop_url: None,
            rating_tenths: None,
            runtime_minutes: None,
            genres: Vec::new(),
            certification: None,
            external_ratings: Vec::new(),
            kind: MediaKind::Movie,
            episodes: Vec::new(),
            attribution: String::new(),
        }
    }

    fn episode(season: u16, number: u16) -> EpisodeRow {
        EpisodeRow {
            season,
            number,
            title: String::new(),
            runtime_minutes: None,
            screenshot_url: None,
        }
    }

    #[test]
    fn original_title_is_not_repeated_for_case_or_whitespace_differences() {
        let mut media = media();
        for title in ["Test title", " test  TITLE ", "\tTEST\ntitle"] {
            media.original_title = Some(title.into());
            assert_eq!(distinct_original_title(&media), None);
        }
        media.original_title = Some("  Original title  ".into());
        assert_eq!(distinct_original_title(&media), Some("Original title"));
        media.title = "Ёжик".into();
        media.original_title = Some(" ёЖИК ".into());
        assert_eq!(distinct_original_title(&media), None);
        media.original_title = Some(" ".into());
        assert_eq!(distinct_original_title(&media), None);
    }

    #[test]
    fn metadata_and_ratings_only_include_actual_nonempty_data() {
        let mut media = media();
        assert!(metadata(&media).is_empty());
        assert!(rating_values(&media).is_empty());
        media.year = Some(2024);
        media.runtime_minutes = Some(0);
        media.genres = vec![" Drama ".into(), " ".into()];
        media.certification = Some(" TV-14 ".into());
        assert_eq!(metadata(&media), "2024 · Drama · TV-14");
        media.rating_tenths = Some(85);
        media.external_ratings = vec![
            DetailRating {
                provider: "trakt".into(),
                value: "8.5".into(),
            },
            DetailRating {
                provider: "IMDb".into(),
                value: " 8.1/10 ".into(),
            },
            DetailRating {
                provider: "TMDB".into(),
                value: " ".into(),
            },
        ];
        let ratings = rating_values(&media);
        assert_eq!(ratings.len(), 2);
        assert_eq!(ratings[0].value, "8.5/10");
        assert_eq!(ratings[1].provider, "IMDb");
        assert_eq!(ratings[1].value, "8.1/10");
        media.rating_tenths = Some(101);
        media.external_ratings.clear();
        assert!(rating_values(&media).is_empty());
    }

    #[test]
    fn season_selection_is_valid_and_specials_are_not_the_implicit_default() {
        let rows = [episode(2, 1), episode(0, 1), episode(1, 2), episode(1, 1)];
        let seasons = seasons(&rows);
        assert_eq!(seasons, [1, 2, 0]);
        assert_eq!(selected_season(&seasons, None, None), Some(1));
        assert_eq!(selected_season(&seasons, None, Some((2, 1))), Some(2));
        assert_eq!(selected_season(&seasons, Some(0), Some((2, 1))), Some(0));
        assert_eq!(selected_season(&seasons, Some(8), Some((2, 1))), Some(2));
        assert_eq!(selected_season(&[], Some(1), Some((1, 1))), None);
        assert_eq!(season_label(0), "Спецвыпуски");
        assert_eq!(season_label(2), "Сезон 2");
        assert_eq!(selected_season(&[0], None, None), Some(0));
        assert_eq!(
            season_episodes(&rows, Some(1))
                .iter()
                .map(|episode| (episode.season, episode.number))
                .collect::<Vec<_>>(),
            [(1, 1), (1, 2)]
        );
        assert!(season_episodes(&rows, None).is_empty());
        assert!(season_episodes(&rows, Some(8)).is_empty());
    }

    #[test]
    fn progress_requires_real_incomplete_measurements_or_explicit_completion() {
        let mut progress = PlaybackProgress::default();
        assert_eq!(progress.label(), None);
        for (position, duration) in [(0, 100), (50, 0), (100, 100), (101, 100)] {
            progress.position = Some(position);
            progress.duration = Some(duration);
            assert_eq!(progress.measured(), None);
            assert_eq!(progress.label(), None);
        }
        progress.position = Some(75);
        progress.duration = Some(600);
        assert_eq!(progress.measured(), Some((75, 600)));
        assert_eq!(progress.label().as_deref(), Some("1 мин из 10 мин"));
        progress.completed = true;
        assert_eq!(progress.measured(), None);
        assert_eq!(progress.label().as_deref(), Some("Просмотрено"));
        progress.position = None;
        progress.duration = None;
        assert_eq!(progress.label().as_deref(), Some("Просмотрено"));
    }

    #[test]
    fn preference_options_preserve_actual_custom_values_without_claiming_tracks() {
        let options = preference_options(
            vec![("ru".into(), "ru".into()), ("en".into(), "en".into())],
            Some("ja"),
        );
        assert_eq!(options[0], (None, "Автоматически".into()));
        assert_eq!(
            options.last(),
            Some(&(Some("ja".into()), "ja · предпочтение".into()))
        );
        assert_eq!(
            preference_options(vec![("ru".into(), "ru".into())], Some("ru")).len(),
            2
        );
        assert!(source_languages(&[]).is_empty());
        assert!(source_resolutions(&[]).is_empty());
        assert_eq!(preference_options(Vec::new(), None).len(), 1);
        assert_eq!(episode_count(1), "1 эпизод");
        assert_eq!(episode_count(12), "12 эпизодов");
        assert_eq!(episode_count(23), "23 эпизода");
    }

    #[test]
    fn choices_use_only_reported_resolution_and_languages_not_release_title_guesses() {
        let mut source = SourceRow {
            index: 0,
            recommended: false,
            possible_match: false,
            title: "1080p RU EN release".into(),
            kind: super::super::SourceKind::Magnet,
            quality: Some("1080p".into()),
            language: Some("ru".into()),
            resolution: None,
            languages: Vec::new(),
            origin: "Provider".into(),
            video_codec: None,
            audio_codec: None,
            peers: None,
            license: None,
            size: None,
            seeds: None,
            rutracker: false,
        };
        assert!(source_resolutions(&[source.clone()]).is_empty());
        assert!(source_languages(&[source.clone()]).is_empty());
        source.resolution = Some(2160);
        source.languages = vec!["JA".into(), " ja ".into(), " ".into()];
        assert_eq!(
            source_resolutions(&[source.clone(), source.clone()]),
            [("2160p".into(), "4K".into())]
        );
        assert_eq!(
            source_languages(&[source.clone(), source]),
            [("ja".into(), "ja".into())]
        );
    }
}
