//! Screen composition and form validation. No application services or IO.
use super::*;
use crate::input::TextInput;
use gpui::{Entity, img};

pub(super) struct Inputs {
    search: Entity<TextInput>,
    cache_limit: Entity<TextInput>,
    pub cache_directory: Entity<TextInput>,
    download: Entity<TextInput>,
    upload: Entity<TextInput>,
    peers: Entity<TextInput>,
    pub season: Entity<TextInput>,
    pub episode: Entity<TextInput>,
}

fn rate_text(rate: RateLimit) -> String {
    match rate {
        RateLimit::Paused => "paused".into(),
        RateLimit::Unlimited => "unlimited".into(),
        RateLimit::Bps(value) => value.to_string(),
    }
}

fn parse_rate(value: &str) -> Result<RateLimit, &'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "paused" => Ok(RateLimit::Paused),
        "unlimited" => Ok(RateLimit::Unlimited),
        value => value
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(RateLimit::Bps)
            .ok_or("Скорость: введите положительное число байт/с, unlimited или paused."),
    }
}

impl Inputs {
    pub fn new(preferences: &PreferencesDraft, cx: &mut Context<KadrView>) -> Self {
        Self {
            search: cx.new(|cx| TextInput::new("", "Название фильма или сериала", cx)),
            cache_limit: cx
                .new(|cx| TextInput::new(preferences.cache_limit_bytes.to_string(), "Байты", cx)),
            cache_directory: cx
                .new(|cx| TextInput::new(preferences.cache_directory.clone(), "Каталог кэша", cx)),
            download: cx.new(|cx| {
                TextInput::new(
                    rate_text(preferences.download_limit),
                    "unlimited / paused / байт/с",
                    cx,
                )
            }),
            upload: cx.new(|cx| {
                TextInput::new(
                    rate_text(preferences.upload_limit),
                    "unlimited / paused / байт/с",
                    cx,
                )
            }),
            peers: cx
                .new(|cx| TextInput::new(preferences.peer_limit.to_string(), "Максимум пиров", cx)),
            season: cx.new(|cx| TextInput::new("", "Сезон", cx)),
            episode: cx.new(|cx| TextInput::new("", "Эпизод", cx)),
        }
    }

    pub fn set_preferences(&self, value: &PreferencesDraft, cx: &mut Context<KadrView>) {
        for (input, value) in [
            (&self.cache_limit, value.cache_limit_bytes.to_string()),
            (&self.cache_directory, value.cache_directory.clone()),
            (&self.download, rate_text(value.download_limit)),
            (&self.upload, rate_text(value.upload_limit)),
            (&self.peers, value.peer_limit.to_string()),
        ] {
            input.update(cx, |input, cx| input.set_value(value, cx));
        }
    }

    fn preferences(
        &self,
        current: &PreferencesDraft,
        cx: &gpui::App,
    ) -> Result<PreferencesDraft, &'static str> {
        let cache_limit_bytes = self
            .cache_limit
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .map_err(|_| "Объём кэша: введите целое неотрицательное число байт.")?;
        let peer_limit = self
            .peers
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or("Число пиров должно быть положительным целым.")?;
        let cache_directory = self.cache_directory.read(cx).value().trim().to_string();
        if cache_directory.is_empty() {
            return Err("Укажите каталог кэша или выберите его кнопкой.");
        }
        Ok(PreferencesDraft {
            cache_limit_bytes,
            peer_limit,
            cache_directory,
            download_limit: parse_rate(self.download.read(cx).value())?,
            upload_limit: parse_rate(self.upload.read(cx).value())?,
            torrents_enabled: current.torrents_enabled,
            catalog_enabled: current.catalog_enabled,
        })
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum LibraryFilter {
    #[default]
    All,
    Favorites,
    Resume,
    History,
}

impl KadrView {
    fn catalog_images_enabled(&self) -> bool {
        // A draft can stop image requests immediately, but cannot opt back in
        // until the controller acknowledges the persisted preference.
        self.applied_catalog_enabled && self.model.preferences.catalog_enabled
    }

    pub(super) fn show_state(&self, section: Section, has_data: bool) -> bool {
        !has_data || !matches!(self.model.states[section.index()], SectionState::Empty(_))
    }

    pub(super) fn submit_search(&mut self, cx: &mut Context<Self>) {
        if let Some(inputs) = &self.inputs {
            let query = inputs.search.read(cx).value().trim().to_string();
            if query.is_empty() {
                self.set_status(UiStatus::info("Введите название для поиска."), cx);
            } else {
                cx.emit(UiEvent::Search(query));
            }
        }
    }

    pub(super) fn submit_preferences(&mut self, cx: &mut Context<Self>) {
        if let Some(inputs) = &self.inputs {
            match inputs.preferences(&self.model.preferences, cx) {
                Ok(draft) => cx.emit(UiEvent::SavePreferences(draft)),
                Err(message) => self.set_status(UiStatus::error(message), cx),
            }
        }
    }

    pub(super) fn submit_episode(&mut self, cx: &mut Context<Self>) {
        if self.model.selected_file.is_none() {
            return;
        }
        if let Some(inputs) = &self.inputs {
            let season = inputs.season.read(cx).value().trim().parse::<u16>();
            let number = inputs.episode.read(cx).value().trim().parse::<u16>();
            match (season, number) {
                (Ok(season), Ok(number)) if number > 0 => {
                    cx.emit(UiEvent::AssignEpisode { season, number })
                }
                _ => self.set_status(
                    UiStatus::error("Введите сезон 0–65535 и эпизод 1–65535."),
                    cx,
                ),
            }
        }
    }

    pub(super) fn search(&self, cx: &mut Context<Self>) -> Div {
        let inputs = self
            .inputs
            .as_ref()
            .expect("inputs initialized before rendering");
        let mut content = column().child(
            panel()
                .child(eyebrow("ПОИСК ПО НАЗВАНИЮ"))
                .child(
                    div()
                        .id("search-form")
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(inputs.search.clone()))
                        .child(self.button(
                            "search-submit",
                            "Найти",
                            Command::Search,
                            true,
                            true,
                            cx,
                        ))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" && !event.is_held {
                                cx.stop_propagation();
                                this.submit_search(cx);
                            }
                        })),
                )
                .child(body("Только результаты вашего запроса — без рекомендаций.")),
        );
        if self.detail_open
            && let Some(media) = &self.model.media
        {
            content = content.child(self.media_detail(media, cx));
        } else if !self.model.results.is_empty() {
            content = content.child(div().flex().flex_wrap().gap_4().children(
                self.model.results.iter().enumerate().map(|(index, media)| {
                    panel()
                        .w(px(240.))
                        .min_w_0()
                        .child(poster(media, 200., self.catalog_images_enabled()))
                        .child(div().text_lg().child(media.title.clone()))
                        .child(body(media_summary(media)))
                        .when_some(media.original_title.clone(), |card, title| {
                            card.child(body(title))
                        })
                        .child(body(media.attribution.clone()))
                        .child(self.button(
                            ("open-media", index),
                            "Подробнее",
                            Command::Emit(UiEvent::OpenMedia(media.id.clone())),
                            false,
                            true,
                            cx,
                        ))
                }),
            ));
        }
        if self.show_state(
            Section::Search,
            (self.detail_open && self.model.media.is_some()) || !self.model.results.is_empty(),
        ) {
            content = content.child(self.state_card(Section::Search, cx));
        }
        content
    }

    fn media_detail(&self, media: &MediaCard, cx: &mut Context<Self>) -> Div {
        panel()
            .child(self.button("back-results", "← Результаты", Command::BackToResults, false, true, cx))
            .child(div().flex().flex_wrap().gap_4()
                .child(poster(media, 180., self.catalog_images_enabled()))
                .child(column().flex_1().min_w(px(180.))
                    .child(div().text_2xl().child(media.title.clone()))
                    .when_some(media.original_title.clone(), |card, title| card.child(body(title)))
                    .child(body(media_summary(media)))
                    .child(body(media.description.clone().unwrap_or_else(|| "Описание отсутствует.".into())))
                    .child(body(media.attribution.clone()))))
            .when(media.kind == MediaKind::Series, |content| {
                content.child(column().child(eyebrow("СЕЗОНЫ И ЭПИЗОДЫ"))
                    .when(media.episodes.is_empty(), |content| content
                        .child(body("Список эпизодов не загружен. Обновите карточку из каталога либо укажите сезон и эпизод вручную при выборе файла."))
                        .when(media.id.starts_with("tvmaze:") || media.id.starts_with("archive:"), |content| content.child(
                            self.button("refresh-series", "Обновить данные и эпизоды",
                                Command::Emit(UiEvent::OpenMedia(media.id.clone())), false,
                                self.model.preferences.catalog_enabled && !self.model.playback_active, cx))))
                    .children(media.episodes.iter().enumerate().map(|(index, episode)| {
                        self.button(("catalog-episode", index),
                            format!("S{:02}E{:02} · {}", episode.season, episode.number, episode.title),
                            Command::Emit(UiEvent::SelectEpisode { season: episode.season, number: episode.number }),
                            self.model.selected_episode == Some((episode.season, episode.number)), true, cx)
                    })))
            })
            .child(div().flex().flex_wrap().gap_2()
                .child(self.button("find-sources", "Найти источники", Command::Emit(UiEvent::FindSources), true, true, cx))
                .child(self.button("favorite-media",
                    if self.model.media_favorite { "Убрать из избранного" } else { "В избранное" },
                    Command::Emit(UiEvent::ToggleFavorite), self.model.media_favorite, true, cx))
                .child(self.open_source_button(cx)))
    }

    pub(super) fn source_rows(&self, cx: &mut Context<Self>) -> Div {
        let mut content = column();
        if let Some(media) = &self.model.media {
            content = content.child(
                panel()
                    .child(eyebrow("ПРИВЯЗКА К КАРТОЧКЕ"))
                    .child(body(media.title.clone()))
                    .child(body(
                        self.model
                            .selected_episode
                            .map(|(s, e)| format!("S{s:02}E{e:02}"))
                            .unwrap_or_else(|| "Эпизод не выбран".into()),
                    ))
                    .child(self.button(
                        "unlink-media",
                        "Открыть без привязки к карточке",
                        Command::Emit(UiEvent::UnlinkMedia),
                        false,
                        true,
                        cx,
                    )),
            );
        }
        if !self.model.sources.is_empty() {
            content = content.child(eyebrow("НАЙДЕННЫЕ ИСТОЧНИКИ")).children(
                self.model
                    .sources
                    .iter()
                    .enumerate()
                    .map(|(index, source)| {
                        panel()
                            .child(div().text_lg().child(source.title.clone()))
                            .child(body(format!(
                                "{} · {} · {}",
                                source.kind.label(),
                                source.quality.as_deref().unwrap_or("Качество неизвестно"),
                                source.language.as_deref().unwrap_or("Язык неизвестен")
                            )))
                            .child(body(format!(
                                "Источник: {} · Лицензия: {}",
                                source.origin,
                                source.license.as_deref().unwrap_or("Не указана")
                            )))
                            .child(self.button(
                                ("choose-source", index),
                                "Выбрать источник",
                                Command::Emit(UiEvent::ChooseSource(source.index)),
                                false,
                                self.model.accepts(&UiEvent::ChooseSource(source.index)),
                                cx,
                            ))
                    }),
            );
        }
        content
    }

    pub(super) fn torrent_files(&self, cx: &mut Context<Self>) -> Div {
        if self.model.files.is_empty() {
            return column();
        }
        let inputs = self.inputs.as_ref().expect("inputs initialized");
        panel()
            .child(eyebrow("ВИДЕОФАЙЛЫ ТОРРЕНТА"))
            .child(body(
                "Выберите файл. Неоднозначную нумерацию можно исправить ниже.",
            ))
            .children(self.model.files.iter().enumerate().map(|(index, file)| {
                column()
                    .child(body(format!("{} · {}", file.name, bytes(file.size))))
                    .child(body(match (file.season, file.episode) {
                        (Some(s), Some(e)) => format!(
                            "Распознано S{s:02}E{e:02}{}",
                            if file.ambiguous {
                                " · нужна проверка"
                            } else {
                                ""
                            }
                        ),
                        _ => "Сезон и эпизод не определены — укажите вручную".into(),
                    }))
                    .child(self.button(
                        ("choose-file", index),
                        if self.model.selected_file == Some(file.index) {
                            "Файл выбран"
                        } else {
                            "Выбрать файл"
                        },
                        Command::Emit(UiEvent::ChooseTorrentFile(file.index)),
                        self.model.selected_file == Some(file.index),
                        self.model.accepts(&UiEvent::ChooseTorrentFile(file.index)),
                        cx,
                    ))
            }))
            .when(self.model.selected_file.is_some(), |content| {
                content.child(
                    column()
                        .child(eyebrow("ИСПРАВИТЬ СЕЗОН И ЭПИЗОД"))
                        .child(field(
                            "Сезон (0 — специальные выпуски)",
                            inputs.season.clone(),
                        ))
                        .child(field("Номер эпизода", inputs.episode.clone()))
                        .child(self.button(
                            "assign-episode",
                            "Применить нумерацию",
                            Command::AssignEpisode,
                            false,
                            true,
                            cx,
                        )),
                )
            })
    }

    pub(super) fn library(&self, cx: &mut Context<Self>) -> Div {
        let mut content = column().child(
            div().flex().flex_wrap().gap_2().children(
                [
                    (LibraryFilter::All, "Все"),
                    (LibraryFilter::Favorites, "Избранное"),
                    (LibraryFilter::Resume, "Продолжить"),
                    (LibraryFilter::History, "История"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (filter, label))| {
                    self.button(
                        ("library-filter", index),
                        label,
                        Command::FilterLibrary(filter),
                        self.library_filter == filter,
                        true,
                        cx,
                    )
                }),
            ),
        );
        let rows: Vec<_> = self
            .model
            .library
            .iter()
            .filter(|row| match self.library_filter {
                LibraryFilter::All => true,
                LibraryFilter::Favorites => row.favorite,
                LibraryFilter::Resume => row.can_resume(),
                LibraryFilter::History => row.has_history(),
            })
            .collect();
        if rows.is_empty() && !self.model.library.is_empty() {
            content = content.child(panel().child(body("В этом разделе нет записей.")));
        }
        content = content.children(rows.into_iter().enumerate().map(|(index, row)| {
            panel()
                .child(div().text_lg().child(row.title.clone()))
                .child(body(row.details.clone()))
                .child(body(match (row.position, row.duration) {
                    (Some(p), Some(d)) => format!("Позиция {} / {}", time(p), time(d)),
                    (Some(p), None) => format!("Позиция {} · длительность неизвестна", time(p)),
                    _ => "Позиция просмотра неизвестна".into(),
                }))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(self.button(
                            ("open-library", index),
                            if row.position.is_some_and(|p| p > 0) {
                                "Продолжить / открыть"
                            } else {
                                "Открыть"
                            },
                            Command::Emit(UiEvent::OpenLibrary(row.id.clone())),
                            true,
                            true,
                            cx,
                        ))
                        .child(self.button(
                            ("favorite-library", index),
                            if row.favorite {
                                "Убрать из избранного"
                            } else {
                                "В избранное"
                            },
                            Command::Emit(UiEvent::SetFavorite {
                                id: row.id.clone(),
                                favorite: !row.favorite,
                            }),
                            row.favorite,
                            true,
                            cx,
                        )),
                )
        }));
        if self.show_state(Section::Library, !self.model.library.is_empty()) {
            content = content.child(self.state_card(Section::Library, cx));
        }
        content.child(body("Точная позиция доступна при телеметрии mpv. Открытие в браузере не подтверждает просмотр."))
    }

    pub(super) fn transfers(&self, cx: &mut Context<Self>) -> Div {
        let mut content = column();
        if let Some(transfer) = &self.model.transfer {
            let progress = match (transfer.done, transfer.total) {
                (Some(done), Some(total)) if total > 0 => format!(
                    "{} / {} · {:.1}%",
                    bytes(done),
                    bytes(total),
                    (done as f64 / total as f64 * 100.).clamp(0., 100.)
                ),
                (Some(done), _) => format!("{} · общий размер неизвестен", bytes(done)),
                _ => "Объём загрузки неизвестен".into(),
            };
            content = content.child(
                panel()
                    .child(eyebrow("АКТИВНАЯ СЕССИЯ"))
                    .child(div().text_xl().child(transfer.stage.clone()))
                    .child(body(progress))
                    .child(body(format!(
                        "Проверенные данные выбранного файла: {}",
                        transfer
                            .buffered
                            .map(bytes)
                            .unwrap_or_else(|| "неизвестно".into())
                    )))
                    .child(body(format!(
                        "↓ {} · ↑ {} · пиры: {}",
                        transfer
                            .download_bps
                            .map(|n| format!("{}/с", bytes(n)))
                            .unwrap_or_else(|| "неизвестно".into()),
                        transfer
                            .upload_bps
                            .map(|n| format!("{}/с", bytes(n)))
                            .unwrap_or_else(|| "неизвестно".into()),
                        transfer
                            .peers
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "неизвестно".into())
                    )))
                    .child(body(format!("Сохранение: {}", transfer.saving))),
            );
        }
        if self.show_state(Section::Transfers, self.model.transfer.is_some()) {
            content = content.child(self.state_card(Section::Transfers, cx));
        }
        content.child(
            panel()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(self.button(
                            "save-transfer",
                            "Сохранить…",
                            Command::Emit(UiEvent::Save),
                            false,
                            self.model.accepts(&UiEvent::Save),
                            cx,
                        ))
                        .child(self.button(
                            "stop-transfer",
                            "Остановить",
                            Command::Emit(UiEvent::Stop),
                            false,
                            self.model.accepts(&UiEvent::Stop),
                            cx,
                        )),
                )
                .child(eyebrow("ОТКРЫТЬ В"))
                .child(
                    div().flex().flex_wrap().gap_2().children(
                        PlayerChoice::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, player)| {
                                self.button(
                                    ("open-in", index),
                                    player.label(),
                                    Command::Emit(UiEvent::OpenIn(player)),
                                    false,
                                    self.model.accepts(&UiEvent::OpenIn(player)),
                                    cx,
                                )
                            }),
                    ),
                )
                .when(!self.model.playback_active, |panel| {
                    panel.child(self.open_source_button(cx))
                }),
        )
    }

    pub(super) fn preference_fields(&self, cx: &mut Context<Self>) -> Div {
        let inputs = self.inputs.as_ref().expect("inputs initialized");
        column().child(panel().child(eyebrow("КЭШ И СЕТЬ"))
            .child(field("Максимальный объём кэша, байты", inputs.cache_limit.clone()))
            .child(field("Каталог кэша", inputs.cache_directory.clone()))
            .child(body("Смена каталога доступна после остановки сессии и очищает прежний неактивный кэш. Сохранённые файлы не затрагиваются."))
            .child(self.button("choose-cache-dir", "Выбрать каталог…", Command::Emit(UiEvent::ChooseCacheDirectory), false, true, cx))
            .child(field("Лимит загрузки, байт/с", inputs.download.clone()))
            .child(field("Лимит отдачи, байт/с", inputs.upload.clone()))
            .child(body("unlimited — без лимита; paused — пауза; числовой лимит BitTorrent — от 16 384 байт/с."))
            .child(field("Максимум пиров", inputs.peers.clone()))
            .child(self.button("enable-torrents",
                if self.model.preferences.torrents_enabled { "✓ BitTorrent включён" } else { "BitTorrent выключен" },
                Command::ToggleTorrents, self.model.preferences.torrents_enabled, true, cx))
            .child(body("BitTorrent может отдавать загруженные части другим участникам сети."))
            .child(self.button("enable-catalog",
                if self.model.preferences.catalog_enabled { "✓ Каталог включён" } else { "Каталог выключен" },
                Command::ToggleCatalog, self.model.preferences.catalog_enabled, true, cx))
            .child(self.button("save-preferences", "Сохранить настройки", Command::SavePreferences, true, true, cx)))
            .child(panel().child(eyebrow("ОЧИСТКА ЛОКАЛЬНЫХ ДАННЫХ"))
                .child(body("Очистка кэша не удаляет сохранённые вами файлы. Очистка истории удаляет позиции просмотра."))
                .child(self.button("clear-cache", "Очистить кэш…", Command::Emit(UiEvent::ClearCache), false, true, cx))
                .child(self.button("clear-history", "Очистить историю…", Command::Emit(UiEvent::ClearHistory), false, true, cx)))
    }
}

fn field(label: &'static str, input: Entity<TextInput>) -> Div {
    column().gap_2().child(body(label)).child(input)
}

fn media_summary(media: &MediaCard) -> String {
    format!(
        "{} · {}",
        match media.kind {
            MediaKind::Movie => "Фильм",
            MediaKind::Series => "Сериал",
        },
        media
            .year
            .map(|year| year.to_string())
            .unwrap_or_else(|| "Год неизвестен".into())
    )
}

fn poster(media: &MediaCard, width: f32, enabled: bool) -> Div {
    let url = media
        .poster_url
        .as_ref()
        .filter(|url| url.starts_with("https://") || url.starts_with("http://"));
    div()
        .w(px(width))
        .h(px(width * 1.5))
        .flex_shrink_0()
        .rounded_md()
        .overflow_hidden()
        .bg(rgb(BACKGROUND))
        .when_some(url.filter(|_| enabled), |card, url| {
            card.child(
                img(url.clone())
                    .size_full()
                    .object_fit(gpui::ObjectFit::Contain)
                    .with_loading(|| body("Загрузка постера…").into_any_element())
                    .with_fallback(|| body("Постер недоступен").into_any_element()),
            )
        })
        .when(!enabled, |card| card.child(body("Постеры отключены")))
        .when(enabled && url.is_none(), |card| {
            card.child(body("Постер отсутствует"))
        })
}

fn bytes(value: u64) -> String {
    for (scale, suffix) in [(1_u64 << 30, "ГиБ"), (1 << 20, "МиБ"), (1 << 10, "КиБ")] {
        if value >= scale {
            return format!("{:.1} {suffix}", value as f64 / scale as f64);
        }
    }
    format!("{value} Б")
}

fn time(seconds: u64) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn poster_requests_require_acknowledged_opt_in() {
        let mut view = KadrView::default();
        assert!(!view.catalog_images_enabled());
        view.model.preferences.catalog_enabled = true;
        assert!(!view.catalog_images_enabled());
        view.applied_catalog_enabled = true;
        assert!(view.catalog_images_enabled());
        view.model.preferences.catalog_enabled = false;
        assert!(!view.catalog_images_enabled());
    }

    #[test]
    fn explicit_rate_states_do_not_confuse_zero_with_unlimited() {
        assert_eq!(parse_rate("unlimited"), Ok(RateLimit::Unlimited));
        assert_eq!(parse_rate(" paused "), Ok(RateLimit::Paused));
        assert_eq!(parse_rate("1024"), Ok(RateLimit::Bps(1024)));
        for invalid in ["0", "-1", "", "1.5", "18446744073709551616"] {
            assert!(parse_rate(invalid).is_err());
        }
    }
    #[test]
    fn presentation_units_are_explicit() {
        assert_eq!(bytes(1024), "1.0 КиБ");
        assert_eq!(bytes(0), "0 Б");
        assert_eq!(time(3661), "1:01:01");
    }
}
