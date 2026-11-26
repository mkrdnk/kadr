//! GPUI bridge. Only clipboard access and native dialogs happen on this thread.
//! Media storage, cache, network and process work belongs to `Engine`.

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use futures_util::StreamExt;
use gpui::{App, Context, Entity, PathPromptOptions};
use kadr_engine::{AsyncState, Command, Engine, Event, Level, Settings, source::ManualSource};
use kadr_ui::{KadrView, Section, SectionState, SourcePreview, UiEvent, UiStatus};

use crate::posters::PosterPolicy;

#[path = "desktop_models.rs"]
mod models;

#[derive(Default)]
struct SourceRequests(u64);

impl SourceRequests {
    fn begin(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }

    fn is_current(&self, request: u64) -> bool {
        self.0 == request
    }
}

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
    opening_library: bool,
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
        self.playable = false;
        self.source_input_invalid = false;
        self.suggested_filename.clear();
        view.set_source_preview(None, cx);
        view.set_torrent_files(Vec::new(), cx);
        view.set_playback_available(false, cx);
    }

    fn launch(&mut self, command: Command, view: &mut KadrView, cx: &mut Context<KadrView>) {
        if self.active || !self.playback_available() {
            return;
        }
        self.source_requests.begin();
        // Stop must work during metadata resolution and process launch, not just
        // after PlaybackActive(true) eventually arrives from the worker.
        self.active = true;
        view.set_playback_active(true, cx);
        if self.dispatch(command, view, cx) {
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
        opening_library: false,
        source_input_invalid: false,
    }));
    let state = desktop.clone();
    cx.subscribe(view, move |view, event, cx| match event {
        UiEvent::PasteSource => {
            if state.borrow().active || !state.borrow().ready {
                return;
            }
            state.borrow_mut().source_requests.begin();
            let result = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .ok_or_else(|| "Буфер обмена не содержит текстовую ссылку.".to_owned())
                .and_then(|text| ManualSource::parse_link(text.trim()).map_err(|e| e.to_string()));
            select_source(&state, &view, result, cx);
        }
        UiEvent::OpenTorrent => open_torrent(&state, &view, cx),
        UiEvent::Save => save_to(&state, &view, cx),
        UiEvent::ChooseCacheDirectory => choose_cache_directory(&state, &view, cx),
        _ => view.update(cx, |view, cx| {
            let mut state = state.borrow_mut();
            match event {
                UiEvent::Search(query) => {
                    state.last_search = query.clone();
                    state.dispatch(Command::Search(query.clone()), view, cx);
                }
                UiEvent::OpenMedia(id) | UiEvent::OpenLibrary(id) => {
                    if state.active {
                        return;
                    }
                    state.invalidate_source(view, cx);
                    view.set_sources(Vec::new(), cx);
                    let command = if matches!(event, UiEvent::OpenMedia(_)) {
                        Command::OpenMedia(id.clone())
                    } else {
                        Command::OpenLibrary(id.clone())
                    };
                    if state.dispatch(command, view, cx) {
                        state.opening_library = matches!(event, UiEvent::OpenLibrary(_));
                        view.show_section(
                            if state.opening_library {
                                Section::Sources
                            } else {
                                Section::Search
                            },
                            cx,
                        );
                    }
                }
                UiEvent::FindSources => {
                    if !state.active {
                        state.invalidate_source(view, cx);
                        if state.dispatch(Command::FindSources, view, cx) {
                            view.show_section(Section::Sources, cx);
                        }
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
                        state.dispatch(Command::SelectEpisode(*season, *number), view, cx);
                    } else {
                        state.dispatch(Command::AssignEpisode(*season, *number), view, cx);
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
                        state.source_input_invalid = false;
                    }
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
                UiEvent::Play | UiEvent::PlayFromStart => state.launch(
                    Command::Play {
                        resume: matches!(event, UiEvent::Play),
                    },
                    view,
                    cx,
                ),
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
                UiEvent::Cancel(_) => {
                    state.source_requests.begin();
                    if state.dispatch(Command::Cancel, view, cx) {
                        view.set_status(UiStatus::info("Отмена операции…"), cx);
                    }
                }
                UiEvent::SetDefaultPlayer(player) => {
                    if let Some(mut settings) = state.editable_settings() {
                        settings.default_player = models::preference(*player);
                        state.update_settings(settings, view, cx);
                    }
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
                UiEvent::ClearCache => {
                    state.dispatch(Command::ClearCache, view, cx);
                }
                UiEvent::ClearHistory => {
                    state.dispatch(Command::ClearHistory, view, cx);
                }
                UiEvent::UnlinkMedia => {
                    if !state.active {
                        state.source_requests.begin();
                        state.dispatch(Command::UnlinkMedia, view, cx);
                    }
                }
                UiEvent::Retry(section) => match section {
                    Section::Search => {
                        let query = state.last_search.clone();
                        state.dispatch(Command::Search(query), view, cx);
                    }
                    Section::Sources if !state.active => {
                        if state.source_input_invalid {
                            view.set_status(
                                UiStatus::warning(
                                    "Вставьте корректную ссылку или выберите .torrent.",
                                ),
                                cx,
                            );
                            return;
                        }
                        state.source_requests.begin();
                        state.dispatch(Command::FindSources, view, cx);
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
                UiEvent::PasteSource
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
        state.source_requests.begin();
        state.cache_requests.begin();
        let _ = view.update(cx, |view, cx| {
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
            if state.requested_settings.as_ref() == Some(&settings) {
                state.requested_settings = None;
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
        Event::Status(level, message) => view.set_status(
            match level {
                Level::Info => UiStatus::info(message),
                Level::Warning => UiStatus::warning(message),
                Level::Error => UiStatus::error(message),
            },
            cx,
        ),
        Event::PlaybackActive(active) => {
            state.active = active;
            view.set_playback_active(active, cx);
            view.set_playback_available(state.playback_available(), cx);
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
                view.set_section_state(Section::Search, SectionState::Error(message), cx);
            }
        },
        Event::MediaSelected { media, favorite } => {
            let media_id = media.as_ref().map(|media| media.id.clone());
            if state.media_id != media_id {
                state.source_requests.begin();
                state.media_id = media_id;
            }
            if state.opening_library {
                view.show_section(
                    if media.is_some() {
                        Section::Search
                    } else {
                        Section::Sources
                    },
                    cx,
                );
                state.opening_library = false;
            }
            view.set_media_detail(media.map(models::media_card), cx);
            view.set_media_favorite(favorite, cx);
        }
        // Cancellation and queued metadata events must not erase a local
        // validation error or make a rejected replacement's old source usable.
        Event::SourcesState(_) | Event::SourceSelected { .. } | Event::TorrentFiles(_)
            if state.source_input_invalid => {}
        Event::SourcesState(result) => match result {
            AsyncState::Loading => {
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
                view.set_sources(
                    sources
                        .into_iter()
                        .enumerate()
                        .map(|(index, source)| models::source_row(index, source))
                        .collect(),
                    cx,
                );
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
            state.opening_library = false;
            view.show_section(
                if source_available || state.media_id.is_none() {
                    Section::Sources
                } else {
                    Section::Search
                },
                cx,
            );
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
                if state.dispatch(Command::SelectSource(source), view, cx) {
                    view.show_section(Section::Sources, cx);
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
                state.dispatch(Command::Cancel, view, cx);
                state.source_input_invalid = true;
                view.set_status(UiStatus::error(message.clone()), cx);
                view.set_section_state(Section::Sources, SectionState::Error(message), cx);
            }
        }
    });
}

fn open_torrent(state: &Rc<RefCell<Desktop>>, view: &Entity<KadrView>, cx: &mut App) {
    if state.borrow().active || !state.borrow().ready {
        return;
    }
    let revision = state.borrow_mut().source_requests.begin();
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
            match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        select_source(
                            &state,
                            &view,
                            ManualSource::torrent_file(path).map_err(|e| e.to_string()),
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
    let revision = state_ref.source_requests.0;
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
