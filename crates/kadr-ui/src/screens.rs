//! Screen composition and form validation. No application services or IO.
use super::*;
use crate::input::TextInput;
use gpui::{Entity, img};

pub(super) struct Inputs {
    pub(super) search: Entity<TextInput>,
    cache_limit: Entity<TextInput>,
    pub cache_directory: Entity<TextInput>,
    download: Entity<TextInput>,
    upload: Entity<TextInput>,
    peers: Entity<TextInput>,
    catalog_proxy: Entity<TextInput>,
    pub season: Entity<TextInput>,
    pub episode: Entity<TextInput>,
    pub rutracker_username: Entity<TextInput>,
    pub rutracker_password: Entity<TextInput>,
}

fn rate_text(rate: RateLimit) -> String {
    match rate {
        RateLimit::Paused => "paused".into(),
        RateLimit::Unlimited => "unlimited".into(),
        RateLimit::Bps(value) => value.to_string(),
    }
}

fn proxy_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
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
            catalog_proxy: cx.new(|cx| {
                TextInput::new(
                    preferences.catalog_proxy.clone().unwrap_or_default(),
                    "Например, socks5h://127.0.0.1:1080",
                    cx,
                )
            }),
            season: cx.new(|cx| TextInput::new("", "Сезон", cx)),
            episode: cx.new(|cx| TextInput::new("", "Эпизод", cx)),
            rutracker_username: cx.new(|cx| TextInput::new("", "Имя пользователя RuTracker", cx)),
            rutracker_password: cx.new(|cx| TextInput::password("Пароль RuTracker", cx)),
        }
    }

    pub fn set_preferences(&self, value: &PreferencesDraft, cx: &mut Context<KadrView>) {
        for (input, value) in [
            (&self.cache_limit, value.cache_limit_bytes.to_string()),
            (&self.cache_directory, value.cache_directory.clone()),
            (&self.download, rate_text(value.download_limit)),
            (&self.upload, rate_text(value.upload_limit)),
            (&self.peers, value.peer_limit.to_string()),
            (
                &self.catalog_proxy,
                value.catalog_proxy.clone().unwrap_or_default(),
            ),
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
            catalog_proxy: proxy_value(self.catalog_proxy.read(cx).value()),
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

impl LibraryFilter {
    fn matches(self, row: &LibraryRow) -> bool {
        match self {
            Self::All => true,
            Self::Favorites => row.favorite,
            Self::Resume => row.can_resume(),
            Self::History => row.has_history(),
        }
    }
}

impl KadrView {
    fn catalog_images_enabled(&self) -> bool {
        // A draft can stop image requests immediately, but cannot opt back in
        // until the controller acknowledges the persisted preference.
        self.applied_catalog_enabled && self.model.preferences.catalog_enabled
    }

    pub(super) fn source_search_notice(&self) -> Option<&'static str> {
        match (
            self.catalog_images_enabled(),
            self.model.preferences.torrents_enabled,
        ) {
            (false, _) => Some(
                "Сетевой поиск источников (Wikimedia Commons / RuTracker / Torznab) недоступен: разрешите внешний каталог \
                 в настройках и сохраните изменения. Trakt предоставляет только метаданные и ссылки на изображения, не видео. \
                 Для Commons BitTorrent не нужен; RuTracker и Torznab требуют отдельного разрешения BitTorrent. \
                 Свой HTTP(S)-источник можно открыть без поиска.",
            ),
            (true, false) => Some(
                "Поиск RuTracker / Torznab недоступен: разрешите BitTorrent в настройках и сохраните изменения. \
                 Поиск Wikimedia Commons доступен, если поставщик включён; он ограничен проверенными фильмами Blender. \
                 BitTorrent обменивается частями файлов с другими участниками сети. \
                 Свой HTTP(S)-источник можно открыть без BitTorrent.",
            ),
            (true, true) => None,
        }
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

    pub(super) fn can_submit_rutracker_login(&self) -> bool {
        self.model.section == Section::Sources
            && self
                .rutracker_login
                .as_ref()
                .is_some_and(|login| !login.pending)
    }

    pub(super) fn submit_rutracker_login(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit_rutracker_login() {
            return;
        }
        let Some(inputs) = &self.inputs else {
            return;
        };
        let username = inputs
            .rutracker_username
            .read(cx)
            .value()
            .trim()
            .to_string();
        if username.is_empty() || inputs.rutracker_password.read(cx).value().is_empty() {
            if let Some(login) = &mut self.rutracker_login {
                login.message = Some("Введите имя пользователя и пароль.".into());
            }
            cx.notify();
            return;
        }
        let password = inputs.rutracker_password.read(cx).value().to_string();
        self.clear_rutracker_password(cx);
        self.rutracker_login = Some(RuTrackerLogin {
            message: None,
            pending: true,
        });
        self.focus_login = true;
        cx.notify();
        // Never put a secret-bearing event in a retained button command.
        cx.emit(UiEvent::SignInRuTracker(RuTrackerCredentials {
            username,
            password,
        }));
    }

    pub(super) fn rutracker_login_form(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let login = self.rutracker_login.as_ref().expect("visible login prompt");
        let inputs = self.inputs.as_ref().expect("inputs initialized");
        panel()
            .id("rutracker-login")
            .gap_4()
            .track_focus(
                self.rutracker_focus
                    .as_ref()
                    .expect("login focus initialized"),
            )
            .tab_stop(false)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon("bookmark", 22., ACCENT))
                    .child(
                        column().gap_1().child(eyebrow("АККАУНТ ИСТОЧНИКА")).child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Вход в RuTracker"),
                        ),
                    ),
            )
            .child(body(
                "Вход используется только на RuTracker. Браузер и отдельный сервер kadr не нужны.",
            ))
            .when_some(login.message.clone(), |form, message| {
                form.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .text_sm()
                        .text_color(rgb(WARNING))
                        .child(message),
                )
            })
            .when(!login.pending, |form| {
                form.child(field("Имя пользователя", inputs.rutracker_username.clone()))
                    .child(field("Пароль", inputs.rutracker_password.clone()))
            })
            .when(login.pending, |form| {
                form.child(
                    div()
                        .py_3()
                        .text_sm()
                        .text_color(rgb(ACCENT))
                        .child("Выполняется вход…"),
                )
            })
            .child(body(
                "По умолчанию запоминаем вход только в системном хранилище секретов. \
                 Если оно недоступно — только текущая сессия, без записи пароля в конфигурацию. \
                 Удалить сохранённый вход можно в «Настройках».",
            ))
            .child(source_disclosure())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(self.button(
                        "rutracker-sign-in",
                        "Войти и продолжить",
                        Command::SignInRuTracker,
                        true,
                        !login.pending,
                        cx,
                    ))
                    .child(self.button(
                        "rutracker-dismiss",
                        "Отменить вход",
                        Command::DismissRuTracker,
                        false,
                        true,
                        cx,
                    )),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.is_held {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "enter" => {
                        cx.stop_propagation();
                        this.submit_rutracker_login(cx);
                    }
                    "escape" => {
                        cx.stop_propagation();
                        this.run_command(&Command::DismissRuTracker, cx);
                    }
                    _ => {}
                }
            }))
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

    pub(super) fn source_rows(&self, cx: &mut Context<Self>) -> Div {
        let mut content = column().gap_3();
        if let Some(media) = &self.model.media {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_1()
                    .child(
                        column()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("ПРИВЯЗАНО К КАРТОЧКЕ"),
                            )
                            .child(div().text_sm().child(media.title.clone()))
                            .when_some(self.model.selected_episode, |context, (s, e)| {
                                context.child(body(format!("S{s:02}E{e:02}")))
                            }),
                    )
                    .child(self.button(
                        "unlink-media",
                        "Без привязки",
                        Command::Emit(UiEvent::UnlinkMedia),
                        false,
                        self.model.accepts(&UiEvent::UnlinkMedia),
                        cx,
                    )),
            );
        }
        let rows = self.visible_sources();
        if rows.is_empty() && !self.model.sources.is_empty() {
            content = content.child(body("Нет раздач с указанным качеством."));
        } else if !rows.is_empty() {
            content = content
                .child(
                    column().gap_0().rounded_md().border_1().border_color(rgb(BORDER))
                        .bg(rgb(PANEL)).overflow_hidden()
                        .child(
                            source_table_row().py_2().text_size(px(9.)).text_color(rgb(MUTED))
                                .child(div().flex_1().min_w(px(150.)).child("КАЧЕСТВО / ТИП"))
                                .child(div().w(px(150.)).flex_shrink_0().child("ПРОВАЙДЕР / ЯЗЫК"))
                                .child(div().w(px(78.)).flex_shrink_0().child("РАЗМЕР"))
                                .child(div().w(px(58.)).flex_shrink_0().child("СИДЫ"))
                                .child(div().w(px(94.)).flex_shrink_0()),
                        )
                        .children(rows.into_iter().map(|source| {
                            source_table_row()
                                .py_3().border_t_1().border_color(rgb(BORDER))
                                .child(
                                    column().gap_1().flex_1().min_w(px(150.))
                                        .child(div().flex().items_center().flex_wrap().gap_2()
                                            .child(div().text_size(px(14.)).font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(source.quality.clone().unwrap_or_else(|| "Неизвестно".into())))
                                            .child(div().text_size(px(10.)).text_color(rgb(MUTED))
                                                .child(if source.rutracker { "BitTorrent" } else { source.kind.label() })))
                                        .when(source.recommended, |cell| cell.child(
                                            div().text_size(px(10.)).text_color(rgb(ACCENT)).child("Рекомендуемый"),
                                        ))
                                        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(source.title.clone()))
                                        .when_some(source.video_codec.as_ref(), |cell, codec| cell.child(body(format!("Видео: {codec}"))))
                                        .when_some(source.audio_codec.as_ref(), |cell, codec| cell.child(body(format!("Аудио: {codec}"))))
                                        .child(div().text_size(px(10.)).text_color(rgb(MUTED))
                                            .child(format!("Лицензия: {}", source.license.as_deref().unwrap_or("не указана"))))
                                        .when(source.rutracker, |cell| cell.child(
                                            div().text_size(px(10.)).text_color(rgb(WARNING)).child("Не проверено · права неизвестны"),
                                        )),
                                )
                                .child(
                                    column().gap_1().w(px(150.)).flex_shrink_0()
                                        .child(div().text_size(px(11.)).child(
                                            if source.rutracker && !source.origin.to_ascii_lowercase().contains("rutracker") {
                                                format!("RuTracker · {}", source.origin)
                                            } else {
                                                source.origin.clone()
                                            },
                                        ))
                                        .child(div().text_size(px(10.)).text_color(rgb(MUTED))
                                            .child(source.language.clone().unwrap_or_else(|| "Язык неизвестен".into()))),
                                )
                                .child(div().w(px(78.)).flex_shrink_0().text_size(px(11.))
                                    .child(source.size.map(bytes).unwrap_or_else(|| "Неизвестно".into())))
                                .child(div().w(px(58.)).flex_shrink_0().text_size(px(11.))
                                    .text_color(rgb(if source.seeds.is_some_and(|n| n > 0) { ACCENT } else { MUTED }))
                                    .child(source.seeds.map(|n| n.to_string()).unwrap_or_else(|| "—".into()))
                                    .when_some(source.peers, |cell, peers| cell.child(div().text_color(rgb(MUTED)).child(format!("Пиры: {peers}")))))
                                .child(
                                    self.button(
                                        ("choose-source", source.index),
                                        "Выбрать",
                                        Command::Emit(UiEvent::ChooseSource(source.index)),
                                        false,
                                        self.model.accepts(&UiEvent::ChooseSource(source.index)),
                                        cx,
                                    ).w(px(94.)).flex_shrink_0().px_2().py_2().text_size(px(11.)).text_center(),
                                )
                        })),
                )
                .child(div().text_size(px(10.)).text_color(rgb(MUTED))
                    .child("«Неизвестно» и «—» — нет данных. Выбор источника не запускает воспроизведение."));
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
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(icon(
                        "film",
                        18.,
                        if self.model.selected_file == Some(file.index) {
                            ACCENT
                        } else {
                            MUTED
                        },
                    ))
                    .child(
                        column()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(file.name.clone()))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(if file.ambiguous { WARNING } else { MUTED }))
                                    .child(match (file.season, file.episode) {
                                        (Some(s), Some(e)) => format!(
                                            "S{s:02}E{e:02}{}",
                                            if file.ambiguous {
                                                " · нужна проверка"
                                            } else {
                                                ""
                                            }
                                        ),
                                        _ => {
                                            "Сезон и эпизод не определены — укажите вручную".into()
                                        }
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(bytes(file.size)),
                    )
                    .child(self.button(
                        ("choose-file", index),
                        if self.model.selected_file == Some(file.index) {
                            "Выбран"
                        } else {
                            "Выбрать"
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
                        .pt_4()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .child(eyebrow("ИСПРАВИТЬ СЕЗОН И ЭПИЗОД"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_4()
                                .child(
                                    field("Сезон (0 — спецвыпуски)", inputs.season.clone())
                                        .flex_1()
                                        .min_w(px(180.)),
                                )
                                .child(
                                    field("Номер эпизода", inputs.episode.clone())
                                        .flex_1()
                                        .min_w(px(180.)),
                                ),
                        )
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
        let history = self.model.section == Section::History;
        let section = if history {
            Section::History
        } else {
            Section::Library
        };
        let filter = if history {
            LibraryFilter::History
        } else {
            self.library_filter
        };
        let mut content = column().gap_5().when(!history, |content| {
            content.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(BACKGROUND))
                    .children(
                        [
                            (LibraryFilter::All, "Все"),
                            (LibraryFilter::Favorites, "Избранное"),
                            (LibraryFilter::Resume, "Продолжить"),
                            (LibraryFilter::History, "История"),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (candidate, label))| {
                            self.button(
                                ("library-filter", index),
                                label,
                                Command::FilterLibrary(candidate),
                                filter == candidate,
                                true,
                                cx,
                            )
                            .text_size(px(11.))
                            .px_3()
                            .py_2()
                        }),
                    ),
            )
        });
        let rows: Vec<_> = self
            .model
            .library
            .iter()
            .filter(|row| filter.matches(row))
            .collect();
        let empty = rows.is_empty();
        let state = &self.model.states[section.index()];
        if empty && matches!(state, SectionState::Empty(_)) {
            let (heading, message) = match filter {
                LibraryFilter::All => (
                    "Ваша коллекция начинается здесь",
                    "Найдите фильм в каталоге и добавьте его в избранное. Открытые источники появятся в истории.",
                ),
                LibraryFilter::Favorites => (
                    "Пока нет избранного",
                    "Добавьте фильм или сериал в избранное — он появится здесь.",
                ),
                LibraryFilter::Resume => (
                    "Пока нечего продолжить",
                    "Здесь появятся незавершённые просмотры с известной позицией воспроизведения.",
                ),
                LibraryFilter::History => (
                    "История пока пуста",
                    "Здесь появятся успешно открытые источники. Само открытие не подтверждает просмотр.",
                ),
            };
            content = content.child(
                column()
                    .items_center()
                    .justify_center()
                    .py(px(80.))
                    .gap_3()
                    .child(icon(if history { "clock" } else { "bookmark" }, 28., MUTED))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_center()
                            .child(heading),
                    )
                    .child(
                        div()
                            .max_w(px(370.))
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .text_center()
                            .child(message),
                    )
                    .child(div().pt_3().child(self.button(
                        "library-browse",
                        "Открыть каталог",
                        Command::Navigate(Section::Search),
                        true,
                        true,
                        cx,
                    ))),
            );
        }
        content = content.child(column().gap_3().children(rows.into_iter().enumerate().map(
            |(index, row)| {
                panel()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .child(library_poster(row, self.catalog_images_enabled()))
                    .child(
                        column()
                            .gap_2()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(row.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(row.details.clone()),
                            )
                            .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(
                                if row.completed {
                                    "Просмотр завершён".into()
                                } else {
                                    match (row.position, row.duration) {
                                        (Some(p), Some(d)) => {
                                            format!("Позиция {} / {}", time(p), time(d))
                                        }
                                        (Some(p), None) => {
                                            format!("Позиция {} · длительность неизвестна", time(p))
                                        }
                                        _ => "Позиция просмотра неизвестна".into(),
                                    }
                                },
                            ))
                            .when_some(
                                measured_fraction(row.position, row.duration),
                                |card, fraction| card.child(progress_track(fraction)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .pt_1()
                                    .child(self.button(
                                        ("open-library", index),
                                        if row.can_resume() {
                                            "Продолжить / открыть"
                                        } else {
                                            "Открыть"
                                        },
                                        Command::Emit(UiEvent::OpenLibrary(row.id.clone())),
                                        true,
                                        self.model.accepts(&UiEvent::OpenLibrary(row.id.clone())),
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
                                        false,
                                        self.model.accepts(&UiEvent::SetFavorite {
                                            id: row.id.clone(),
                                            favorite: !row.favorite,
                                        }),
                                        cx,
                                    )),
                            ),
                    )
            },
        )));
        if !matches!(state, SectionState::Empty(_)) {
            content = content.child(self.state_card(section, cx));
        }
        content.when(!empty, |content| content.child(
            div().text_size(px(10.)).text_color(rgb(MUTED))
                .child("Точная позиция доступна при телеметрии mpv. Открытие в браузере не подтверждает просмотр."),
        ))
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
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(icon("download", 22., ACCENT))
                            .child(
                                column()
                                    .gap_1()
                                    .flex_1()
                                    .min_w_0()
                                    .child(eyebrow("АКТИВНАЯ СЕССИЯ"))
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(transfer.stage.clone()),
                                    ),
                            ),
                    )
                    .child(
                        column()
                            .gap_2()
                            .child(div().text_xs().text_color(rgb(MUTED)).child(progress))
                            .when_some(
                                measured_fraction(transfer.done, transfer.total),
                                |content, fraction| content.child(progress_track(fraction)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_5()
                            .child(source_metric(
                                "Загрузка",
                                &transfer
                                    .download_bps
                                    .map(|n| format!("{}/с", bytes(n)))
                                    .unwrap_or_else(|| "Неизвестно".into()),
                            ))
                            .child(source_metric(
                                "Отдача",
                                &transfer
                                    .upload_bps
                                    .map(|n| format!("{}/с", bytes(n)))
                                    .unwrap_or_else(|| "Неизвестно".into()),
                            ))
                            .child(source_metric(
                                "Пиры",
                                &transfer
                                    .peers
                                    .map(|n| n.to_string())
                                    .unwrap_or_else(|| "Неизвестно".into()),
                            )),
                    )
                    .child(
                        column()
                            .gap_2()
                            .pt_4()
                            .border_t_1()
                            .border_color(rgb(BORDER))
                            .child(body(format!(
                                "Проверенные данные выбранного файла: {}",
                                transfer
                                    .buffered
                                    .map(bytes)
                                    .unwrap_or_else(|| "неизвестно".into())
                            )))
                            .child(body(format!("Сохранение: {}", transfer.saving))),
                    ),
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
        column().gap_5()
            .child(panel().gap_4()
                .child(eyebrow("О ПРИЛОЖЕНИИ И ИСТОЧНИКАХ ДАННЫХ"))
                .child(div().text_size(px(28.)).font_weight(gpui::FontWeight::SEMIBOLD).child("KADR"))
                .child(div().text_sm().child("Метаданные фильмов и сериалов, рейтинги и ссылки на изображения — Trakt. Не источник видео."))
                .child(div().text_sm().text_color(rgb(TEXT))
                    .child("kadr uses Trakt but is not endorsed or certified by Trakt."))
                .child(body("https://trakt.tv/"))
                .child(body(CATALOG_ONBOARDING))
                .child(body("Для разработки: KADR_TRAKT_CLIENT_ID — публичный ID приложения Trakt; \
                    его можно задать вне приложения, в окружении процесса. Это не личный токен и не client secret."))
                .child(body(COMMONS_DISCLOSURE))
                .child(body("RuTracker и настроенные Torznab API используются для поиска источников. Для них внешний каталог и BitTorrent должны быть разрешены. \
                    Данные раздач и права на контент не проверены.")))
            .child(panel().gap_4()
                .child(eyebrow("ХРАНИЛИЩЕ"))
                .child(div().text_lg().font_weight(gpui::FontWeight::MEDIUM).child("Кэш"))
                .child(field("Максимальный объём кэша, байты", inputs.cache_limit.clone()))
                .child(field("Каталог кэша", inputs.cache_directory.clone()))
                .child(div().flex().flex_wrap().items_center().gap_3()
                    .child(self.button("choose-cache-dir", "Выбрать каталог…", Command::Emit(UiEvent::ChooseCacheDirectory), false, true, cx)))
                .child(body("Смена каталога доступна после остановки сессии и очищает прежний неактивный кэш. Сохранённые файлы не затрагиваются.")))
            .child(panel().gap_4()
                .child(eyebrow("СЕТЬ"))
                .child(div().text_lg().font_weight(gpui::FontWeight::MEDIUM).child("BitTorrent и каталог"))
                .child(div().flex().flex_wrap().gap_4()
                    .child(field("Лимит загрузки, байт/с", inputs.download.clone()).flex_1().min_w(px(180.)))
                    .child(field("Лимит отдачи, байт/с", inputs.upload.clone()).flex_1().min_w(px(180.))))
                .child(body("unlimited — без лимита; paused — пауза; числовой лимит BitTorrent — от 16 384 байт/с."))
                .child(field("Максимум пиров", inputs.peers.clone()))
                .child(field("Прокси", inputs.catalog_proxy.clone()))
                .child(body("http://, https://, socks5:// или socks5h:// — без имени пользователя и пароля. Пустое поле — прямое соединение."))
                .child(body("Для Trakt, постеров и API Wikimedia Commons, RuTracker и Torznab. Не применяется к BitTorrent, HTTP-видео, внешним плеерам и локальному серверу воспроизведения. При недоступности прокси автоматического перехода на прямое соединение нет."))
                .child(div().flex().flex_wrap().items_center().gap_4().pt_4().border_t_1().border_color(rgb(BORDER))
                    .child(column().gap_1().flex_1().min_w(px(180.))
                        .child(div().text_sm().child("Обмен BitTorrent"))
                        .child(body("Может отдавать загруженные части другим участникам сети. Отключение блокирует поиск RuTracker/Torznab, но не Wikimedia Commons.")))
                    .child(self.button("enable-torrents",
                        if self.model.preferences.torrents_enabled { "Включён" } else { "Выключен" },
                        Command::ToggleTorrents, self.model.preferences.torrents_enabled, true, cx)))
                .child(div().flex().flex_wrap().items_center().gap_4().pt_4().border_t_1().border_color(rgb(BORDER))
                    .child(column().gap_1().flex_1().min_w(px(180.))
                        .child(div().text_sm().child("Внешний каталог"))
                        .child(body("Разрешает получать метаданные и изображения Trakt и обращаться к включённым сетевым поставщикам, включая Wikimedia Commons. Привязанные ручные источники остаются доступны без сетевого поиска. BitTorrent включается отдельно.")))
                    .child(self.button("enable-catalog",
                        if self.model.preferences.catalog_enabled { "Включён" } else { "Выключен" },
                        Command::ToggleCatalog, self.model.preferences.catalog_enabled, true, cx)))
                .child(source_disclosure()))
            .child(div().flex().child(self.button("save-preferences", "Сохранить настройки", Command::SavePreferences, true, true, cx)))
            .child(panel().child(eyebrow("ОЧИСТКА ЛОКАЛЬНЫХ ДАННЫХ"))
                .child(body("Очистка кэша не удаляет сохранённые вами файлы. Очистка истории удаляет позиции просмотра."))
                .child(div().flex().flex_wrap().gap_2()
                    .child(self.button("clear-cache", "Очистить кэш…", Command::Emit(UiEvent::ClearCache), false, true, cx))
                    .child(self.button("clear-history", "Очистить историю…", Command::Emit(UiEvent::ClearHistory), false, true, cx))))
    }
}

fn field(label: &'static str, input: Entity<TextInput>) -> Div {
    column()
        .gap_2()
        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(label))
        .child(input)
}

fn source_table_row() -> Div {
    div().flex().items_center().gap_3().px_4()
}

fn source_metric(label: &'static str, value: &str) -> Div {
    column().gap_1().min_w(px(100.)).child(body(label)).child(
        div()
            .text_size(px(16.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .child(value.to_owned()),
    )
}

fn library_poster(row: &LibraryRow, enabled: bool) -> Div {
    let url = row
        .poster_url
        .as_ref()
        .filter(|url| enabled && (url.starts_with("https://") || url.starts_with("http://")));
    div()
        .w(px(76.))
        .h(px(114.))
        .flex_shrink_0()
        .rounded_md()
        .overflow_hidden()
        .bg(rgb(BACKGROUND))
        .flex()
        .items_center()
        .justify_center()
        .when_some(url, |card, url| {
            card.child(
                img(url.clone())
                    .size_full()
                    .object_fit(gpui::ObjectFit::Cover)
                    .with_loading(|| icon("film", 22., MUTED).into_any_element())
                    .with_fallback(|| icon("film", 22., MUTED).into_any_element()),
            )
        })
        .when(url.is_none(), |card| card.child(icon("film", 22., MUTED)))
}

fn measured_fraction(done: Option<u64>, total: Option<u64>) -> Option<f32> {
    match (done, total) {
        (Some(done), Some(total)) if total > 0 => {
            Some((done as f64 / total as f64).clamp(0., 1.) as f32)
        }
        _ => None,
    }
}

fn progress_track(fraction: f32) -> Div {
    div()
        .w_full()
        .h(px(3.))
        .rounded_full()
        .overflow_hidden()
        .bg(rgb(BORDER))
        .child(div().h_full().w(gpui::relative(fraction)).bg(rgb(ACCENT)))
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
    fn proxy_field_normalizes_only_whitespace_and_leaves_validation_to_storage() {
        assert_eq!(PreferencesDraft::default().catalog_proxy, None);
        for value in ["", " \n\t "] {
            assert_eq!(proxy_value(value), None);
        }
        for value in [
            "http://proxy.example:8080",
            "https://proxy.example:8443",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
            "not a proxy",
        ] {
            assert_eq!(proxy_value(&format!("  {value}\t")), Some(value.into()));
        }
    }

    #[test]
    fn preferences_debug_redacts_the_proxy_address() {
        let draft = PreferencesDraft {
            catalog_proxy: Some("socks5h://private-endpoint.example:1080".into()),
            ..PreferencesDraft::default()
        };
        let debug = format!("{draft:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("private-endpoint"));
    }

    #[test]
    fn login_requires_visible_requested_prompt_and_cannot_submit_twice() {
        let mut view = KadrView::new();
        assert!(!view.can_submit_rutracker_login());
        view.rutracker_login = Some(RuTrackerLogin {
            message: Some("Войдите для выбранного источника.".into()),
            pending: false,
        });
        assert!(
            !view.can_submit_rutracker_login(),
            "Home does not expose the form"
        );
        view.model.section = Section::Sources;
        assert!(view.can_submit_rutracker_login());
        view.rutracker_login.as_mut().unwrap().pending = true;
        assert!(!view.can_submit_rutracker_login());
        view.rutracker_login.as_mut().unwrap().pending = false;
        view.model.section = Section::Settings;
        assert!(!view.can_submit_rutracker_login());
        view.model.section = Section::Sources;
        view.rutracker_login = None;
        assert!(!view.can_submit_rutracker_login());
    }

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
    fn source_search_notice_explains_each_disabled_permission_and_draft_enable() {
        let mut view = KadrView::default();
        let notice = view.source_search_notice().unwrap();
        assert!(notice.contains("Сетевой поиск источников"));
        assert!(notice.contains("Wikimedia Commons"));
        assert!(notice.contains("RuTracker"));
        assert!(notice.contains("внешний каталог"));
        assert!(notice.contains("Trakt"));
        assert!(notice.contains("только метаданные"));
        assert!(notice.contains("Для Commons BitTorrent не нужен"));
        view.applied_catalog_enabled = true;
        assert!(view.source_search_notice().is_none());
        view.model.preferences.torrents_enabled = false;
        let notice = view.source_search_notice().unwrap();
        assert!(notice.contains("разрешите BitTorrent"));
        assert!(notice.contains("Поиск Wikimedia Commons доступен, если поставщик включён"));
        assert!(!notice.contains("Сетевой поиск источников"));
        assert!(notice.contains("HTTP(S)"));
        view.model.preferences.catalog_enabled = false;
        let notice = view.source_search_notice().unwrap();
        assert!(notice.contains("Сетевой поиск источников"));
        assert!(notice.contains("разрешите внешний каталог"));
        assert!(!notice.contains("разрешите BitTorrent"));
        view.model.preferences.catalog_enabled = true;
        view.applied_catalog_enabled = false;
        assert!(
            view.source_search_notice()
                .unwrap()
                .contains("сохраните изменения")
        );
    }

    #[test]
    fn source_search_notice_does_not_treat_a_disabled_commons_provider_as_enabled() {
        let mut view = KadrView {
            applied_catalog_enabled: true,
            source_preferences: source_preferences::SourcePreferencesEditor::from(
                SourcePreferencesDraft {
                    commons_enabled: false,
                    ..SourcePreferencesDraft::default()
                },
            ),
            ..KadrView::default()
        };
        view.model.preferences.torrents_enabled = false;
        assert!(
            view.source_search_notice()
                .unwrap()
                .contains("доступен, если поставщик включён")
        );
        // Permission notices explain consent, not unsaved provider flags.
        view.model.preferences.torrents_enabled = true;
        assert!(view.source_search_notice().is_none());
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
    fn progress_requires_measured_values_and_nonzero_total() {
        assert_eq!(measured_fraction(None, Some(100)), None);
        assert_eq!(measured_fraction(Some(20), None), None);
        assert_eq!(measured_fraction(Some(20), Some(0)), None);
        assert_eq!(measured_fraction(Some(0), Some(100)), Some(0.));
        assert_eq!(measured_fraction(Some(25), Some(100)), Some(0.25));
        assert_eq!(measured_fraction(Some(120), Some(100)), Some(1.));
    }

    #[test]
    fn library_filters_do_not_invent_history_or_resume() {
        let mut row = LibraryRow {
            id: "local:item".into(),
            title: "Own video".into(),
            poster_url: None,
            details: String::new(),
            favorite: true,
            opened_at: None,
            completed: false,
            position: None,
            duration: None,
        };
        assert!(LibraryFilter::All.matches(&row));
        assert!(LibraryFilter::Favorites.matches(&row));
        assert!(!LibraryFilter::History.matches(&row));
        assert!(!LibraryFilter::Resume.matches(&row));

        row.opened_at = Some(100);
        assert!(LibraryFilter::History.matches(&row));
        assert!(!LibraryFilter::Resume.matches(&row));

        row.position = Some(25);
        row.duration = Some(100);
        assert!(LibraryFilter::Resume.matches(&row));
        row.completed = true;
        assert!(!LibraryFilter::Resume.matches(&row));
    }

    #[test]
    fn presentation_units_are_explicit() {
        assert_eq!(bytes(1024), "1.0 КиБ");
        assert_eq!(bytes(0), "0 Б");
        assert_eq!(time(3661), "1:01:01");
    }
}
