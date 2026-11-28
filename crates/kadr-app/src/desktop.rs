//! GPUI bridge. Only clipboard access and native dialogs happen on this thread.
//! Media storage, cache, network and process work belongs to `Engine`.

use std::{cell::RefCell, collections::VecDeque, path::PathBuf, rc::Rc};

use futures_util::StreamExt;
use gpui::{App, Context, Entity, PathPromptOptions};
use kadr_engine::{AsyncState, Command, Engine, Event, Level, Settings, source::ManualSource};
use kadr_ui::{
    KadrView, Section, SectionState, SourceDiscovery, SourcePreferencesDraft, SourcePreview,
    UiEvent, UiStatus,
};

use crate::posters::PosterPolicy;

#[path = "desktop_models.rs"]
mod models;
#[path = "desktop_sources.rs"]
mod sources;
use sources::{
    CardOpenRequest, DiscoveryIntent, MovieChoice, SourcePresentation, SourceRequests,
    WatchDecision,
};

pub struct Desktop {
    backend: Engine,
    ready: bool,
    closed: bool,
    active: bool,
    playable: bool,
    /// Last backend-confirmed settings. The view only displays acknowledged values.
    settings: Option<Settings>,
    /// Preserve consecutive edits while persistence acknowledgments are in flight.
    requested_settings: Option<Settings>,
    source_requests: SourceRequests,
    cache_requests: SourceRequests,
    suggested_filename: String,
    last_search: String,
    media_id: Option<String>,
    requested_media: Option<CardOpenRequest>,
    library_intent: Option<u64>,
    episode: Option<(u16, u16)>,
    sources: SourcePresentation,
    movie_choice: MovieChoice,
    watch_preferences: kadr_engine::PlaybackPreferences,
    watch_resume: bool,
    auto_discover: bool,
    catalog_requests: [Option<kadr_ui::CatalogRequest>; 2],
    genre_requested: [bool; 2],
    requested_source_preferences: VecDeque<SourcePreferencesDraft>,
    source_input_invalid: bool,
}

impl Desktop {
    fn playback_available(&self) -> bool {
        self.ready && !self.closed && self.playable
    }

    fn dispatch(
        &mut self,
        command: Command,
        view: &mut KadrView,
        cx: &mut Context<KadrView>,
    ) -> bool {
        if self.ready && !self.closed && self.backend.dispatch(command) {
            true
        } else {
            view.set_status(
                UiStatus::error("Медиаядро недоступно или занято. Попробуйте снова."),
                cx,
            );
            false
        }
    }

    fn update_settings(
        &mut self,
        settings: Settings,
        view: &mut KadrView,
        cx: &mut Context<KadrView>,
    ) {
        if self.dispatch(Command::UpdateSettings(settings.clone()), view, cx) {
            self.requested_settings = Some(settings);
            view.set_preferences_status("Сохранение настроек…", cx);
        }
    }

    fn editable_settings(&self) -> Option<Settings> {
        self.requested_settings
            .as_ref()
            .or(self.settings.as_ref())
            .cloned()
    }

    fn invalidate_source(&mut self, view: &mut KadrView, cx: &mut Context<KadrView>) {
        self.source_requests.begin();
        self.sources.clear();
        self.movie_choice = MovieChoice::None;
        view.set_movie_watch_pending(false, cx);
        self.library_intent = None;
        self.playable = false;
        self.source_input_invalid = false;
        self.suggested_filename.clear();
        view.set_rutracker_login(None, false, cx);
        view.set_source_discovery(None, cx);
        view.set_sources(Vec::new(), cx);
        view.set_source_preview(None, cx);
        view.set_torrent_files(Vec::new(), cx);
        view.set_playback_available(false, cx);
        view.set_section_state(
            Section::Sources,
            SectionState::Empty(
                "Вставьте свою ссылку, откройте .torrent или найдите источники.".into(),
            ),
            cx,
        );
    }

    fn discover(
        &mut self,
        force: bool,
        intent: DiscoveryIntent,
        view: &mut KadrView,
        cx: &mut Context<KadrView>,
    ) {
        let Some(media_id) = self.media_id.clone() else {
            view.set_movie_watch_pending(false, cx);
            return;
        };
        self.invalidate_source(view, cx);
        let command = if force {
            Command::RetrySources
        } else {
            Command::FindSources
        };
        self.begin_discovery(command, force, intent, media_id, view, cx);
    }

    fn begin_discovery(
        &mut self,
        command: Command,
        force: bool,
        intent: DiscoveryIntent,
        media_id: String,
        view: &mut KadrView,
        cx: &mut Context<KadrView>,
    ) {
        if self.dispatch(command, view, cx) {
            self.sources.discover(media_id, self.episode, force);
            self.sources.intent = intent;
            if intent != DiscoveryIntent::Browse {
                view.set_movie_watch_pending(true, cx);
            }
            view.begin_source_discovery(cx);
            view.set_section_state(
                Section::Sources,
                SectionState::Loading("Поиск источников…".into()),
                cx,
            );
        }
    }

    fn launch(&mut self, command: Command, view: &mut KadrView, cx: &mut Context<KadrView>) {
        if self.active || !self.playback_available() {
            return;
        }
        self.start_player(command, view, cx);
    }

    fn background_discovery(&mut self, view: &mut KadrView, cx: &mut Context<KadrView>) {
        if self.auto_discover
            && !self.sources.discovering()
            && self
                .settings
                .as_ref()
                .is_some_and(|settings| settings.catalog_enabled)
            && let Some(id) = self.media_id.clone()
        {
            self.begin_discovery(
                Command::DiscoverOnOpen,
                false,
                DiscoveryIntent::Browse,
                id,
                view,
                cx,
            );
        }
    }

    fn watch(
        &mut self,
        resume: bool,
        preferences: kadr_engine::PlaybackPreferences,
        view: &mut KadrView,
        cx: &mut Context<KadrView>,
    ) {
        if self.active {
            view.set_movie_watch_pending(false, cx);
            return;
        }
        self.watch_preferences = preferences.clone();
        self.watch_resume = resume;
        if self.movie_choice == MovieChoice::Explicit {
            view.set_movie_watch_pending(false, cx);
            if self.playback_available() {
                self.launch(
                    Command::PlayWithPreferences {
                        resume,
                        preferences,
                    },
                    view,
                    cx,
                );
            } else {
                view.show_source_overlay(cx);
            }
            return;
        }
        let intent = DiscoveryIntent::WatchMovie {
            navigation: view.navigation_revision(),
        };
        if self.sources.discovering() {
            self.sources.intent = intent;
            view.set_movie_watch_pending(true, cx);
            self.request_recommendation(view, cx);
        } else {
            self.discover(false, intent, view, cx);
        }
    }

    fn request_recommendation(&mut self, view: &mut KadrView, cx: &mut Context<KadrView>) {
        if self.sources.complete
            && self.sources.intent != DiscoveryIntent::Browse
            && !self.sources.requested_recommendation
        {
            self.sources.requested_recommendation = self.dispatch(
                Command::RecommendPlayback(self.watch_preferences.clone()),
                view,
                cx,
            );
            if !self.sources.requested_recommendation {
                self.sources.intent = DiscoveryIntent::Browse;
                view.show_source_overlay(cx);
            }
        }
    }

    fn start_player(&mut self, command: Command, view: &mut KadrView, cx: &mut Context<KadrView>) {
        self.source_requests.begin();
        // Stop must work during metadata resolution and process launch, not just
        // after PlaybackActive(true) eventually arrives from the worker.
        self.active = true;
        view.set_playback_active(true, cx);
        if self.dispatch(command, view, cx) {
            view.set_preparing(true, cx);
            view.set_status(
                UiStatus::info("Запуск плеера… Можно отменить кнопкой «Стоп»."),
                cx,
            );
        } else {
            self.active = false;
            view.set_playback_active(false, cx);
        }
    }

    pub fn join(&mut self) -> bool {
        self.backend.join()
    }
}

pub fn attach(
    view: &Entity<KadrView>,
    poster_policy: PosterPolicy,
    smoke_ui: bool,
    cx: &mut App,
) -> std::io::Result<Rc<RefCell<Desktop>>> {
    let (backend, mut events) = Engine::start()?;
    let desktop = Rc::new(RefCell::new(Desktop {
        backend,
        ready: false,
        closed: false,
        active: false,
        playable: false,
        settings: None,
        requested_settings: None,
        source_requests: SourceRequests::default(),
        cache_requests: SourceRequests::default(),
        suggested_filename: String::new(),
        last_search: String::new(),
        media_id: None,
        requested_media: None,
        library_intent: None,
        episode: None,
        sources: SourcePresentation::default(),
        movie_choice: MovieChoice::None,
        watch_preferences: kadr_engine::PlaybackPreferences::default(),
        watch_resume: true,
        auto_discover: false,
        catalog_requests: std::array::from_fn(|_| None),
        genre_requested: [false; 2],
        requested_source_preferences: VecDeque::new(),
        source_input_invalid: false,
    }));
    let state = desktop.clone();
    cx.subscribe(view, move |view, event, cx| match event {
        UiEvent::PasteSource | UiEvent::ImportSource { torrent: false, .. } => {
            if state.borrow().active || !state.borrow().ready {
                return;
            }
            state.borrow_mut().source_requests.begin();
            let result = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .ok_or_else(|| "Буфер обмена не содержит текстовую ссылку.".to_owned())
                .and_then(|text| ManualSource::parse_link(text.trim()).map_err(|e| e.to_string()));
            let attach = !matches!(event, UiEvent::ImportSource { attach: false, .. });
            select_source(&state, &view, result, attach, cx);
        }
        UiEvent::OpenTorrent | UiEvent::ImportSource { torrent: true, .. } => {
            let attach = !matches!(event, UiEvent::ImportSource { attach: false, .. });
            open_torrent(&state, &view, attach, cx);
        }
        UiEvent::Save => save_to(&state, &view, cx),
        UiEvent::ChooseCacheDirectory => choose_cache_directory(&state, &view, cx),
        _ => view.update(cx, |view, cx| {
            let mut state = state.borrow_mut();
            match event {
                UiEvent::Search(query) => {
                    state.last_search = query.clone();
                    state.dispatch(Command::Search(query.clone()), view, cx);
                }
                UiEvent::Browse => {
                    if state.dispatch(Command::Browse, view, cx) {
                        view.set_browse_state(
                            SectionState::Loading("Загружаем каталог…".into()),
                            cx,
                        );
                    }
                }
                UiEvent::CatalogPage(request) => {
                    let slot = usize::from(request.query.kind == kadr_ui::MediaKind::Series);
                    if state.dispatch(models::catalog_request(request), view, cx) {
                        state.catalog_requests[slot] = Some(request.clone());
                    } else {
                        view.set_catalog_page(
                            request,
                            Err("Не удалось начать загрузку.".into()),
                            cx,
                        );
                    }
                }
                UiEvent::CatalogGenres(kind) => {
                    let slot = usize::from(*kind == kadr_ui::MediaKind::Series);
                    if !state.genre_requested[slot] {
                        if state.dispatch(
                            Command::CatalogGenres(models::catalog_kind(*kind)),
                            view,
                            cx,
                        ) {
                            state.genre_requested[slot] = true;
                            view.set_catalog_genres_loading(*kind, cx);
                        } else {
                            view.set_catalog_genres(
                                *kind,
                                Err("Не удалось загрузить жанры.".into()),
                                cx,
                            );
                        }
                    }
                }
                UiEvent::OpenMedia(id) | UiEvent::OpenLibrary(id) => {
                    if state.active {
                        return;
                    }
                    state.invalidate_source(view, cx);
                    let command = if matches!(event, UiEvent::OpenMedia(_)) {
                        Command::OpenMedia(id.clone())
                    } else {
                        Command::OpenLibrary(id.clone())
                    };
                    if state.dispatch(command, view, cx) {
                        let opening_library = matches!(event, UiEvent::OpenLibrary(_));
                        // Library row IDs are opaque and need not equal a card ID.
                        state.requested_media = if opening_library {
                            None
                        } else {
                            Some(CardOpenRequest {
                                media_id: id.clone(),
                                navigation: view.navigation_revision(),
                            })
                        };
                        state.episode = None;
                        if opening_library {
                            state.watch_resume = true;
                            state.sources.prepare();
                            state.library_intent = Some(view.navigation_revision());
                        }
                    }
                }
                UiEvent::FindSources => {
                    if !state.active {
                        state.watch_resume = true;
                        view.show_source_overlay(cx);
                        if !state.sources.discovering()
                            && state.movie_choice != MovieChoice::Explicit
                        {
                            state.discover(false, DiscoveryIntent::Browse, view, cx);
                        }
                    }
                }
                UiEvent::DiscoverSources => {
                    if !state.active
                        && (!state.sources.discovering() || state.sources.complete)
                        && state.movie_choice != MovieChoice::Explicit
                    {
                        let refresh = state.sources.complete;
                        state.discover(refresh, DiscoveryIntent::Browse, view, cx);
                    }
                }
                UiEvent::WatchMovie => {
                    state.watch(
                        true,
                        models::playback_preferences(view.playback_options()),
                        view,
                        cx,
                    );
                }
                UiEvent::WatchDetail {
                    resume,
                    preferences,
                } => {
                    state.watch(
                        *resume,
                        models::playback_preferences(preferences.clone()),
                        view,
                        cx,
                    );
                }
                UiEvent::WatchSource(index) => {
                    if !state.active {
                        let resume = state.watch_resume;
                        state.movie_choice = MovieChoice::Recommended {
                            navigation: view.navigation_revision(),
                        };
                        state.sources.prepare();
                        view.set_source_discovery(None, cx);
                        state.start_player(
                            Command::WatchSource {
                                index: *index,
                                preferences: models::playback_preferences(view.playback_options()),
                                resume,
                            },
                            view,
                            cx,
                        );
                    }
                }
                UiEvent::SelectEpisode { season, number }
                | UiEvent::AssignEpisode { season, number } => {
                    if state.active {
                        return;
                    }
                    state.source_requests.begin();
                    if matches!(event, UiEvent::SelectEpisode { .. }) {
                        state.invalidate_source(view, cx);
                        if state.dispatch(Command::SelectEpisode(*season, *number), view, cx) {
                            state.episode = Some((*season, *number));
                            state.background_discovery(view, cx);
                        }
                    } else if state.dispatch(Command::AssignEpisode(*season, *number), view, cx) {
                        state.episode = Some((*season, *number));
                    }
                }
                UiEvent::ChooseSource(index) | UiEvent::ChooseTorrentFile(index) => {
                    if state.active {
                        return;
                    }
                    state.source_requests.begin();
                    state.playable = false;
                    view.set_playback_available(false, cx);
                    let command = if matches!(event, UiEvent::ChooseSource(_)) {
                        Command::ChooseSource(*index)
                    } else {
                        Command::ChooseTorrentFile(*index)
                    };
                    if state.dispatch(command, view, cx) {
                        state.movie_choice = MovieChoice::Explicit;
                        state.source_input_invalid = false;
                        state.sources.prepare();
                        view.set_source_discovery(None, cx);
                        if matches!(event, UiEvent::ChooseSource(_)) {
                            view.set_section_state(
                                Section::Sources,
                                SectionState::Loading("Проверка источника…".into()),
                                cx,
                            );
                        }
                    }
                }
                UiEvent::SignInRuTracker(credentials) => {
                    if !state.active
                        && !state.dispatch(
                            Command::SignInRuTracker {
                                username: credentials.username.clone(),
                                password: credentials.password.clone(),
                            },
                            view,
                            cx,
                        )
                    {
                        view.set_rutracker_login(
                            Some("Не удалось начать вход. Повторите попытку.".into()),
                            false,
                            cx,
                        );
                    }
                }
                UiEvent::ForgetRuTracker => {
                    state.dispatch(Command::ForgetRuTracker, view, cx);
                }
                UiEvent::ToggleFavorite => {
                    let media_id = state.media_id.clone().unwrap_or_default();
                    state.dispatch(
                        Command::SetFavorite {
                            media_id,
                            favorite: None,
                        },
                        view,
                        cx,
                    );
                }
                UiEvent::SetFavorite { id, favorite } => {
                    state.dispatch(
                        Command::SetFavorite {
                            media_id: id.clone(),
                            favorite: Some(*favorite),
                        },
                        view,
                        cx,
                    );
                }
                UiEvent::Play | UiEvent::PlayFromStart => {
                    let resume = matches!(event, UiEvent::Play) && state.watch_resume;
                    state.launch(
                        Command::PlayWithPreferences {
                            resume,
                            preferences: models::playback_preferences(view.playback_options()),
                        },
                        view,
                        cx,
                    );
                }
                UiEvent::OpenIn(player) => {
                    let command = Command::OpenIn(models::preference(*player));
                    if state.active {
                        state.source_requests.begin();
                        if state.dispatch(command, view, cx) {
                            view.set_status(UiStatus::info("Смена внешнего плеера…"), cx);
                        }
                    } else {
                        state.launch(command, view, cx);
                    }
                }
                UiEvent::Stop => {
                    state.source_requests.begin();
                    if state.dispatch(Command::Stop, view, cx) {
                        view.set_status(UiStatus::info("Остановка сессии…"), cx);
                    }
                }
                UiEvent::Cancel(section) => {
                    state.source_requests.begin();
                    state.requested_media = None;
                    state.library_intent = None;
                    if !state.active && *section == Section::Sources {
                        // Engine cancellation drops idle selections as well as searches.
                        // Do not leave the previous preview launchable after ignoring
                        // its untagged cancellation replies.
                        state.invalidate_source(view, cx);
                    }
                    let command = match section {
                        Section::Sources if view.is_preparing() => Command::Stop,
                        Section::Sources => Command::CancelSources,
                        _ => Command::Cancel,
                    };
                    if state.dispatch(command, view, cx) {
                        view.set_status(UiStatus::info("Отмена операции…"), cx);
                    }
                }
                UiEvent::CancelMoviePrefetch => {
                    // Speculative work must not clear a ready source or report
                    // errors when a card is simply dismissed.
                    state.backend.dispatch(Command::CancelMoviePrefetch);
                }
                UiEvent::SetDefaultPlayer(player) => {
                    if let Some(mut settings) = state.editable_settings() {
                        settings.default_player = models::preference(*player);
                        state.update_settings(settings, view, cx);
                    }
                }
                UiEvent::SetSidebarCollapsed(collapsed) => {
                    state.dispatch(Command::SetSidebarCollapsed(*collapsed), view, cx);
                }
                UiEvent::SavePreferences(draft) => {
                    if let Some(settings) = state.editable_settings() {
                        match models::settings_from_draft(settings, draft) {
                            Ok(settings) => state.update_settings(settings, view, cx),
                            Err(message) => {
                                view.set_preferences_status(message, cx);
                                view.set_status(UiStatus::error(message), cx);
                            }
                        }
                    }
                }
                UiEvent::SaveSourcePreferences(draft) => match models::source_preferences(draft) {
                    Ok(preferences) => {
                        let expected = models::source_preferences_draft(&preferences);
                        if state.dispatch(Command::SetSourcePreferences(preferences), view, cx) {
                            state.requested_source_preferences.push_back(expected);
                            view.set_source_preferences_status("Сохранение провайдеров…", cx);
                        }
                    }
                    Err(message) => {
                        view.set_source_preferences_status(message, cx);
                        view.set_status(UiStatus::error(message), cx);
                    }
                },
                UiEvent::ClearCache => {
                    state.dispatch(Command::ClearCache, view, cx);
                }
                UiEvent::ClearHistory => {
                    state.dispatch(Command::ClearHistory, view, cx);
                }
                UiEvent::UnlinkMedia => {
                    if !state.active && state.dispatch(Command::UnlinkMedia, view, cx) {
                        state.source_requests.begin();
                        let preparing = state.sources.preparing;
                        state.sources.clear();
                        state.sources.preparing = preparing;
                        view.set_source_discovery(None, cx);
                        view.set_sources(Vec::new(), cx);
                        state.media_id = None;
                        state.requested_media = None;
                        state.episode = None;
                        view.set_media_detail(None, cx);
                    }
                }
                UiEvent::Retry(section) => match section {
                    Section::Search => {
                        let query = state.last_search.clone();
                        state.dispatch(Command::Search(query), view, cx);
                    }
                    Section::Sources if !state.active => {
                        if state.source_input_invalid && state.media_id.is_none() {
                            view.set_status(
                                UiStatus::warning(
                                    "Вставьте корректную ссылку или выберите .torrent.",
                                ),
                                cx,
                            );
                            return;
                        }
                        if state.media_id.is_some() {
                            state.discover(true, DiscoveryIntent::Browse, view, cx);
                        } else {
                            state.source_requests.begin();
                            if state.dispatch(Command::RetrySources, view, cx) {
                                state.sources.prepare();
                                view.set_source_discovery(None, cx);
                            }
                        }
                    }
                    Section::Transfers => state.launch(Command::Play { resume: true }, view, cx),
                    Section::Settings => {
                        if let Some(settings) = state.editable_settings() {
                            state.update_settings(settings, view, cx);
                        }
                    }
                    _ => view.set_status(
                        UiStatus::info("Повторите действие в соответствующем разделе."),
                        cx,
                    ),
                },
                UiEvent::ImportSource { .. }
                | UiEvent::PasteSource
                | UiEvent::OpenTorrent
                | UiEvent::Save
                | UiEvent::ChooseCacheDirectory => unreachable!(),
            }
        }),
    })
    .detach();

    let state = desktop.clone();
    let quit_poster_policy = poster_policy.clone();
    cx.on_app_quit(move |_| {
        quit_poster_policy.set_enabled(false);
        state.borrow().backend.cancel();
        async {}
    })
    .detach();

    let state = desktop.clone();
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        while let Some(event) = events.next().await {
            // Apply confirmed catalogue policy before rendering new poster URLs.
            poster_policy.apply_event(&event);
            if smoke_ui && matches!(&event, Event::Ready { .. }) {
                eprintln!("kadr-ui-smoke: backend-ready");
            }
            let Ok(()) = view.update(cx, |view, cx| {
                apply_event(&mut state.borrow_mut(), view, event, cx);
            }) else {
                break;
            };
        }
        poster_policy.set_enabled(false);
        // EOF is terminal: never leave usable launch controls after a worker
        // panic, failed initialization or orderly shutdown.
        let mut state = state.borrow_mut();
        state.closed = true;
        state.ready = false;
        state.active = false;
        state.playable = false;
        state.sources.clear();
        state.source_requests.begin();
        state.cache_requests.begin();
        let _ = view.update(cx, |view, cx| {
            view.set_source_discovery(None, cx);
            view.set_rutracker_login(None, false, cx);
            view.set_movie_watch_pending(false, cx);
            view.set_preparing(false, cx);
            view.set_playback_available(false, cx);
            view.set_playback_active(false, cx);
            view.set_transfer(None, cx);
            view.set_status(
                UiStatus::error("Медиаядро завершило работу. Перезапустите приложение."),
                cx,
            );
        });
    })
    .detach();
    Ok(desktop)
}

fn apply_event(state: &mut Desktop, view: &mut KadrView, event: Event, cx: &mut Context<KadrView>) {
    match event {
        Event::Ready { settings, cache } => {
            if state.closed {
                return;
            }
            state.ready = true;
            view.set_sidebar_collapsed(settings.sidebar_collapsed, cx);
            view.set_catalog_years((1888..=models::current_year()).rev().collect(), cx);
            publish_settings(view, &settings, cx);
            state.settings = Some(settings);
            view.set_preferences_status("Настройки загружены из локального хранилища.", cx);
            view.set_section_state(
                Section::Settings,
                cache.map_or_else(
                    || SectionState::Unavailable("Объём кэша пока неизвестен.".into()),
                    |usage| SectionState::Empty(models::cache_usage(&usage)),
                ),
                cx,
            );
            view.set_status(
                UiStatus::info("Готово. Найдите видео или откройте свой источник."),
                cx,
            );
            view.set_playback_available(state.playback_available(), cx);
        }
        Event::PreferencesSaved(settings) => {
            if state.requested_settings.as_ref().is_some_and(|requested| {
                let mut requested = requested.clone();
                requested.sidebar_collapsed = settings.sidebar_collapsed;
                requested == settings
            }) {
                state.requested_settings = None;
            }
            if !settings.catalog_enabled {
                state.catalog_requests = std::array::from_fn(|_| None);
                state.genre_requested = [false; 2];
                if state.sources.discovering() {
                    state.invalidate_source(view, cx);
                }
            }
            publish_settings(view, &settings, cx);
            state.settings = Some(settings);
            view.set_preferences_status(
                if state.requested_settings.is_some() {
                    "Предыдущие настройки сохранены. Ожидается подтверждение последних изменений…"
                } else {
                    "Настройки сохранены. Плеер применяется при следующем запуске."
                },
                cx,
            );
        }
        Event::PreferencesRejected { settings, message } => {
            state.requested_settings = None;
            publish_settings(view, &settings, cx);
            state.settings = Some(settings);
            view.set_preferences_status(
                "Изменение не применено; сохранённые настройки оставлены.",
                cx,
            );
            view.set_status(UiStatus::error(message), cx);
        }
        Event::SourcePreferences(preferences) => {
            state.auto_discover = preferences.auto_discover;
            if state.sources.discovering() {
                state.invalidate_source(view, cx);
            }
            let confirmed = models::source_preferences_draft(&preferences);
            let expected = state.requested_source_preferences.pop_front();
            let rejected = expected
                .as_ref()
                .is_some_and(|expected| expected != &confirmed);
            // Ordered confirmations for earlier saves must not replace newer drafts.
            if state.requested_source_preferences.is_empty() {
                view.set_source_preferences(confirmed, cx);
            }
            view.set_source_preferences_status(
                if !state.requested_source_preferences.is_empty() {
                    "Ожидается подтверждение последних изменений провайдеров…"
                } else if rejected {
                    "Изменение не применено; показаны сохранённые настройки провайдеров."
                } else if expected.is_some() {
                    "Настройки провайдеров сохранены."
                } else {
                    "Настройки провайдеров загружены."
                },
                cx,
            );
        }
        Event::Status(level, message) => {
            view.set_status(
                match level {
                    Level::Info => UiStatus::info(message),
                    Level::Warning => UiStatus::warning(message),
                    Level::Error => UiStatus::error(message),
                },
                cx,
            );
        }
        Event::SidebarCollapsed(collapsed) => {
            if let Some(settings) = &mut state.settings {
                settings.sidebar_collapsed = collapsed;
            }
            view.set_sidebar_collapsed(collapsed, cx);
        }
        Event::PlaybackActive(active) => {
            state.active = active;
            view.set_preparing(false, cx);
            view.set_playback_active(active, cx);
            view.set_playback_available(state.playback_available(), cx);
            if active {
                view.close_source_after_launch(cx);
            }
        }
        Event::MovieWatchNeedsInput(stamp) => {
            if stamp.is_current()
                && state.movie_choice
                    == (MovieChoice::Recommended {
                        navigation: view.navigation_revision(),
                    })
            {
                view.show_source_overlay(cx);
            }
        }
        Event::SearchState(result) => match result {
            AsyncState::Loading => {
                view.set_search_results(Vec::new(), cx);
                view.set_section_state(Section::Search, SectionState::Loading("Поиск…".into()), cx);
            }
            AsyncState::Ready(media) => {
                let empty = media.is_empty();
                view.set_search_results(media.into_iter().map(models::media_card).collect(), cx);
                view.set_section_state(
                    Section::Search,
                    SectionState::Empty(
                        if empty {
                            "Ничего не найдено."
                        } else {
                            "Результаты поиска"
                        }
                        .into(),
                    ),
                    cx,
                );
            }
            AsyncState::Error(message) => {
                view.set_search_results(Vec::new(), cx);
                view.set_status(UiStatus::error(message.clone()), cx);
                view.set_section_state(Section::Search, SectionState::Error(message), cx);
            }
        },
        Event::BrowseState(result) => match result {
            AsyncState::Loading => {
                view.set_browse_state(SectionState::Loading("Загружаем каталог…".into()), cx);
            }
            AsyncState::Ready(catalog) => {
                view.set_browse_catalog(
                    catalog.movies.into_iter().map(models::media_card).collect(),
                    catalog.shows.into_iter().map(models::media_card).collect(),
                    cx,
                );
                view.set_browse_state(SectionState::Empty("Больше результатов нет.".into()), cx);
            }
            AsyncState::Error(message) => view.set_browse_state(SectionState::Error(message), cx),
        },
        Event::CatalogPage { request, result } => {
            if let Some(slot) = state
                .catalog_requests
                .iter_mut()
                .find(|slot| slot.as_ref().is_some_and(|pending| pending.id == request))
                && let Some(pending) = slot.take()
            {
                view.set_catalog_page(
                    &pending,
                    result.map(|page| kadr_ui::CatalogPage {
                        items: page.items.into_iter().map(models::media_card).collect(),
                        next_page: page.next_page,
                    }),
                    cx,
                );
            }
        }
        Event::CatalogGenres { kind, result } => {
            if state
                .settings
                .as_ref()
                .is_some_and(|settings| settings.catalog_enabled)
            {
                let kind = models::ui_kind(kind);
                state.genre_requested[usize::from(kind == kadr_ui::MediaKind::Series)] =
                    result.is_ok();
                view.set_catalog_genres(
                    kind,
                    result.map(|genres| {
                        genres
                            .into_iter()
                            .map(|genre| kadr_ui::CatalogGenre {
                                slug: genre.slug,
                                name: genre.name,
                            })
                            .collect()
                    }),
                    cx,
                );
            }
        }
        Event::MediaSelected {
            media,
            favorite,
            episode,
        } => {
            let media_id = media.as_ref().map(|media| media.id.clone());
            // Details/favorite replies never get to replace a newer requested card.
            if let Some(requested) = &state.requested_media {
                if media_id.as_deref() != Some(requested.media_id.as_str()) {
                    return;
                }
                if requested.navigation != view.navigation_revision() {
                    state.requested_media = None;
                    return;
                }
            }
            let is_movie = media
                .as_ref()
                .is_some_and(|media| media.kind == kadr_engine::catalog::MediaKind::Movie);
            let opened_card = state.requested_media.take().filter(|request| {
                request.should_prefetch(media_id.as_deref(), true, view.navigation_revision())
            });
            if state.media_id != media_id {
                state.source_requests.begin();
                state.media_id = media_id;
            }
            state.episode = episode;
            view.set_media_detail(media.map(|media| models::media_card(*media)), cx);
            view.set_catalog_episode(episode, cx);
            view.set_media_favorite(favorite, cx);
            if let Some(request) = opened_card
                && view.detail_visible()
                && state.ready
                && !state.closed
            {
                if is_movie {
                    if state.auto_discover {
                        state.background_discovery(view, cx);
                    } else {
                        state
                            .backend
                            .dispatch(Command::PrefetchMovieSources(request.media_id));
                    }
                } else if let Some((season, number)) = view.preferred_episode()
                    && state.dispatch(Command::SelectEpisode(season, number), view, cx)
                {
                    state.episode = Some((season, number));
                    view.set_catalog_episode(state.episode, cx);
                    state.background_discovery(view, cx);
                }
            }
        }
        Event::SourceDiscovery {
            media_id,
            episode,
            rows,
            pending,
            succeeded,
            failures,
            notices,
            stamp,
        } => {
            if !stamp.is_current()
                || state.source_input_invalid
                || !state.sources.accepts_discovery(&media_id, episode)
            {
                return;
            }
            state.sources.received_discovery = true;
            state.sources.preparing = false;
            view.set_sources(rows.into_iter().map(models::source_row).collect(), cx);
            view.set_source_discovery(
                Some(SourceDiscovery {
                    pending,
                    succeeded,
                    failures,
                    notices,
                }),
                cx,
            );
            state.sources.complete = pending == 0;
            if pending == 0 {
                view.set_section_state(
                    Section::Sources,
                    SectionState::Empty("Поиск завершён.".into()),
                    cx,
                );
            }
            state.request_recommendation(view, cx);
        }
        Event::PlaybackRecommendation { stamp, index } => {
            if !stamp.is_current() || !state.sources.requested_recommendation {
                return;
            }
            state.sources.requested_recommendation = false;
            let action = state
                .sources
                .intent
                .finish(view.navigation_revision(), 0, index);
            view.set_movie_watch_pending(false, cx);
            match action {
                WatchDecision::None => {}
                WatchDecision::ShowSources => {
                    view.show_source_overlay(cx);
                }
                WatchDecision::Play(index) => {
                    state.movie_choice = MovieChoice::Recommended {
                        navigation: view.navigation_revision(),
                    };
                    state.sources.prepare();
                    view.set_source_discovery(None, cx);
                    state.start_player(
                        Command::WatchRecommended {
                            index,
                            preferences: state.watch_preferences.clone(),
                            resume: state.watch_resume,
                        },
                        view,
                        cx,
                    );
                }
            }
        }
        // Cancellation and queued metadata events must not erase a local
        // validation error or make a rejected replacement's old source usable.
        Event::SourcesState(_)
        | Event::SourceSelected { .. }
        | Event::TorrentFiles(_)
        | Event::RuTrackerLogin { .. }
            if state.source_input_invalid => {}
        Event::SourceSelected { .. } | Event::TorrentFiles(_) if !state.sources.preparing => {}
        Event::RuTrackerLogin { .. }
            if !state.sources.preparing && !state.sources.discovering() => {}
        Event::RuTrackerLogin { message, pending } => {
            view.set_rutracker_login(message, pending, cx);
        }
        Event::SourcesState(result) if !state.sources.preparing => {
            // The engine can reject discovery before emitting its first snapshot.
            // Other untagged states belong to older preparation requests.
            if state.sources.discovering()
                && !state.sources.received_discovery
                && let AsyncState::Error(message) = result
            {
                if state
                    .sources
                    .intent
                    .finish(view.navigation_revision(), 0, None)
                    == WatchDecision::ShowSources
                {
                    view.set_movie_watch_pending(false, cx);
                    view.show_source_overlay(cx);
                }
                state.sources.clear();
                view.set_source_discovery(None, cx);
                view.set_section_state(Section::Sources, SectionState::Error(message), cx);
            }
        }
        Event::SourcesState(AsyncState::Ready(_)) if state.sources.discovering() => {}
        Event::SourcesState(result) => match result {
            AsyncState::Loading => {
                state.sources.prepare();
                view.set_source_discovery(None, cx);
                state.playable = false;
                view.set_playback_available(false, cx);
                view.set_sources(Vec::new(), cx);
                view.set_section_state(
                    Section::Sources,
                    SectionState::Loading("Получение источников и метаданных…".into()),
                    cx,
                );
            }
            AsyncState::Ready(sources) => {
                let empty = sources.is_empty();
                view.set_sources(sources.into_iter().map(models::source_row).collect(), cx);
                view.set_section_state(
                    Section::Sources,
                    SectionState::Empty(
                        if empty {
                            "Источники не найдены. Можно открыть свою ссылку или .torrent."
                        } else {
                            "Выберите источник."
                        }
                        .into(),
                    ),
                    cx,
                );
            }
            AsyncState::Error(message) => {
                state.sources.prepare();
                view.set_source_discovery(None, cx);
                state.playable = false;
                view.set_playback_available(false, cx);
                view.set_sources(Vec::new(), cx);
                view.set_section_state(Section::Sources, SectionState::Error(message), cx);
            }
        },
        Event::SourceSelected {
            kind,
            title,
            playable,
            suggested_filename,
        } => {
            if state.sources.discovering() {
                if title.is_empty() && !playable {
                    return;
                }
                state.sources.prepare();
                view.set_source_discovery(None, cx);
            }
            state.playable = playable;
            state.suggested_filename = models::suggested_filename(&suggested_filename);
            view.set_source_preview(
                if title.is_empty() && !playable {
                    None
                } else {
                    Some(SourcePreview {
                        kind: models::source_kind(kind),
                        title,
                    })
                },
                cx,
            );
            view.set_playback_available(state.playback_available(), cx);
            view.set_section_state(
                Section::Sources,
                SectionState::Empty(
                    if playable {
                        "Источник готов."
                    } else {
                        "Выберите видеофайл."
                    }
                    .into(),
                ),
                cx,
            );
        }
        Event::TorrentFiles(files) => {
            if state.sources.discovering() && files.is_empty() {
                return;
            }
            view.set_torrent_files(files.into_iter().map(models::torrent_file).collect(), cx);
        }
        Event::Library(items) => {
            let empty = items.is_empty();
            view.set_library(items.into_iter().map(models::library_row).collect(), cx);
            view.set_section_state(
                Section::Library,
                SectionState::Empty(
                    if empty {
                        "Избранное и история пока пусты."
                    } else {
                        "Локальная библиотека"
                    }
                    .into(),
                ),
                cx,
            );
        }
        Event::LibraryOpened { source_available } => {
            // Preserve the legacy detail-only path for a card without a source.
            // A delayed reply cannot reopen Sources after navigation/dismissal.
            if state.library_intent.take() == Some(view.navigation_revision()) {
                if source_available {
                    state.movie_choice = MovieChoice::Explicit;
                }
                if state.media_id.is_none() {
                    view.show_source_overlay(cx);
                }
            }
        }
        Event::Transfer(transfer) => {
            let active = transfer.is_some();
            view.set_transfer(transfer.map(models::transfer_view), cx);
            view.set_section_state(
                Section::Transfers,
                SectionState::Empty(
                    if active {
                        "Текущая сессия"
                    } else {
                        "Нет активных передач."
                    }
                    .into(),
                ),
                cx,
            );
        }
        Event::CacheUsage(usage) => {
            view.set_section_state(
                Section::Settings,
                SectionState::Empty(models::cache_usage(&usage)),
                cx,
            );
        }
    }
}

fn publish_settings(view: &mut KadrView, settings: &Settings, cx: &mut Context<KadrView>) {
    view.set_default_player(models::choice(settings.default_player), cx);
    view.set_preferences(models::preferences_draft(settings), cx);
}

fn select_source(
    state: &Rc<RefCell<Desktop>>,
    view: &Entity<KadrView>,
    result: Result<ManualSource, String>,
    attach: bool,
    cx: &mut App,
) {
    view.update(cx, |view, cx| {
        let mut state = state.borrow_mut();
        if state.active || !state.ready || state.closed {
            return;
        }
        state.invalidate_source(view, cx);
        match result {
            Ok(source) => {
                if state.dispatch(Command::ImportSource { source, attach }, view, cx) {
                    state.requested_media = None;
                    state.watch_resume = true;
                    state.movie_choice = MovieChoice::Explicit;
                    state.sources.prepare();
                    view.set_section_state(
                        Section::Sources,
                        SectionState::Loading("Проверка источника…".into()),
                        cx,
                    );
                }
            }
            Err(message) => {
                // Cancel in-flight metadata too; an old source must not become
                // launchable after a failed replacement.
                state.dispatch(Command::CancelSources, view, cx);
                state.source_input_invalid = true;
                view.set_status(UiStatus::error(message.clone()), cx);
                view.set_section_state(Section::Sources, SectionState::Error(message), cx);
            }
        }
    });
}

fn open_torrent(state: &Rc<RefCell<Desktop>>, view: &Entity<KadrView>, attach: bool, cx: &mut App) {
    if state.borrow().active || !state.borrow().ready {
        return;
    }
    let revision = state.borrow_mut().source_requests.begin();
    let navigation = view.read(cx).navigation_revision();
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Открыть .torrent".into()),
    });
    let state = state.clone();
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        let result = paths.await;
        let _ = cx.update(|cx| {
            let state_ref = state.borrow();
            if !state_ref.source_requests.is_current(revision)
                || !state_ref.ready
                || state_ref.closed
                || state_ref.active
            {
                return;
            }
            drop(state_ref);
            let Some(view) = view.upgrade() else { return };
            if view.read(cx).navigation_revision() != navigation {
                return;
            }
            match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        select_source(
                            &state,
                            &view,
                            ManualSource::torrent_file(path).map_err(|e| e.to_string()),
                            attach,
                            cx,
                        );
                    }
                }
                Ok(Ok(None)) => {}
                _ => view.update(cx, |view, cx| {
                    view.set_status(
                        UiStatus::error("Не удалось открыть диалог выбора файла."),
                        cx,
                    );
                }),
            }
        });
    })
    .detach();
}

fn save_to(state: &Rc<RefCell<Desktop>>, view: &Entity<KadrView>, cx: &mut App) {
    let state_ref = state.borrow();
    if !state_ref.playback_available() {
        return;
    }
    // Saving may run alongside playback; source replacement invalidates this
    // result. No private URL or torrent path enters the native dialog.
    let revision = state_ref.source_requests.current();
    let filename = state_ref.suggested_filename.clone();
    drop(state_ref);
    let path = cx.prompt_for_new_path(&home_directory(), Some(&filename));
    let state = state.clone();
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        let result = path.await;
        let _ = view.update(cx, |view, cx| {
            let mut state = state.borrow_mut();
            if !state.ready || state.closed || !state.source_requests.is_current(revision) {
                return;
            }
            match result {
                Ok(Ok(Some(path))) => {
                    state.dispatch(Command::SaveTo(path), view, cx);
                }
                Ok(Ok(None)) => {}
                _ => view.set_status(UiStatus::error("Не удалось открыть диалог сохранения."), cx),
            }
        });
    })
    .detach();
}

fn choose_cache_directory(state: &Rc<RefCell<Desktop>>, view: &Entity<KadrView>, cx: &mut App) {
    if !state.borrow().ready {
        return;
    }
    let revision = state.borrow_mut().cache_requests.begin();
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Каталог временного видеокэша".into()),
    });
    let state = state.clone();
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        let result = paths.await;
        let _ = view.update(cx, |view, cx| {
            let mut state = state.borrow_mut();
            if !state.ready || state.closed || !state.cache_requests.is_current(revision) {
                return;
            }
            match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next()
                        && let Some(mut settings) = state.editable_settings()
                    {
                        settings.cache_directory = Some(path);
                        state.update_settings(settings, view, cx);
                    }
                }
                Ok(Ok(None)) => {}
                _ => view.set_status(
                    UiStatus::error("Не удалось открыть диалог выбора каталога."),
                    cx,
                ),
            }
        });
    })
    .detach();
}

fn home_directory() -> PathBuf {
    // Environment lookup is not filesystem IO. Never canonicalize or inspect
    // the destination here; the native dialog and worker handle it.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::SourceRequests;

    #[test]
    fn old_picker_results_and_errors_cannot_replace_newer_paste_or_launch() {
        let mut requests = SourceRequests::default();
        let old_picker = requests.begin();
        assert!(requests.is_current(old_picker));
        let newer_paste = requests.begin();
        assert!(!requests.is_current(old_picker));
        assert!(requests.is_current(newer_paste));
        let next_picker = requests.begin();
        requests.begin(); // Starting playback invalidates pending selection.
        assert!(!requests.is_current(next_picker));
        // Stopping playback must not roll the generation back.
        assert!(!requests.is_current(old_picker));
    }
}
