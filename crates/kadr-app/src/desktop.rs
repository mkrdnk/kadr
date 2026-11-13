use std::{cell::RefCell, rc::Rc};

use futures_util::StreamExt;
use gpui::{App, Entity, PathPromptOptions};
use kadr_core::source::{ManualSource, SourceKind};
use kadr_storage::PlayerPreference;
use kadr_ui::{
    KadrView, PlayerChoice, Section, SectionState, SourceKind as UiSourceKind, SourcePreview,
    UiEvent, UiStatus,
};

use crate::backend::{Backend, Event, Level};

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
    backend: Backend,
    source: Option<ManualSource>,
    ready: bool,
    active: bool,
    source_requests: SourceRequests,
}

impl Desktop {
    pub fn join(&mut self) -> bool {
        self.backend.join()
    }
}

pub fn attach(view: &Entity<KadrView>, cx: &mut App) -> std::io::Result<Rc<RefCell<Desktop>>> {
    let (backend, mut events) = Backend::start()?;
    let desktop = Rc::new(RefCell::new(Desktop {
        backend,
        source: None,
        ready: false,
        active: false,
        source_requests: SourceRequests::default(),
    }));
    let state = desktop.clone();
    cx.subscribe(view, move |view, event, cx| match event {
        UiEvent::PasteSource => {
            if state.borrow().active {
                return;
            }
            {
                let mut state = state.borrow_mut();
                state.source_requests.begin();
            }
            let result = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .ok_or_else(|| "Буфер обмена не содержит текстовую ссылку.".to_owned())
                .and_then(|text| ManualSource::parse_link(text.trim()).map_err(|e| e.to_string()));
            select_source(&state, &view, result, cx);
        }
        UiEvent::OpenTorrent => {
            if state.borrow().active {
                return;
            }
            let revision = {
                let mut state = state.borrow_mut();
                state.source_requests.begin()
            };
            let paths = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Открыть .torrent".into()),
            });
            let state = state.clone();
            cx.spawn(async move |cx| {
                let result = paths.await;
                let _ = cx.update(|cx| {
                    if !state.borrow().source_requests.is_current(revision) {
                        return;
                    }
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
        UiEvent::Play | UiEvent::PlayFromStart => {
            let mut state = state.borrow_mut();
            if state.active || !state.ready {
                return;
            }
            state.source_requests.begin();
            if let Some(source) = state.source.clone() {
                if state.backend.play(source, matches!(event, UiEvent::Play)) {
                    state.active = true;
                    view.update(cx, |view, cx| {
                        view.set_playback_active(true, cx);
                        view.set_status(
                            UiStatus::info("Запуск плеера… Можно отменить кнопкой «Стоп»."),
                            cx,
                        );
                    });
                } else {
                    view.update(cx, |view, cx| {
                        view.set_status(
                            UiStatus::error("Медиаядро недоступно или занято. Попробуйте снова."),
                            cx,
                        );
                    });
                }
            }
        }
        UiEvent::Stop => {
            state.borrow().backend.stop();
            view.update(cx, |view, cx| {
                view.set_status(UiStatus::info("Остановка плеера…"), cx);
            });
        }
        UiEvent::SetDefaultPlayer(player) => {
            if !state.borrow().backend.set_player(preference(*player)) {
                view.update(cx, |view, cx| {
                    view.set_status(
                        UiStatus::error("Не удалось передать настройку медиаядру."),
                        cx,
                    );
                });
            }
        }
        UiEvent::Retry(_) | UiEvent::Cancel(_) => {
            view.update(cx, |view, cx| {
                view.set_status(
                    UiStatus::warning(
                        "Этот адаптер ещё не подключён. Используйте ручной HTTP-источник.",
                    ),
                    cx,
                );
            });
        }
    })
    .detach();

    let state = desktop.clone();
    cx.on_app_quit(move |_| {
        state.borrow().backend.cancel();
        async {}
    })
    .detach();

    let state = desktop.clone();
    let view = view.downgrade();
    cx.spawn(async move |cx| {
        while let Some(event) = events.next().await {
            let Ok(()) = view.update(cx, |view, cx| match event {
                Event::Ready(player) => {
                    state.borrow_mut().ready = true;
                    view.set_default_player(choice(player), cx);
                    view.set_preferences_status("Настройки сохраняются в SQLite. Сеть BitTorrent в UI ещё не запускается.", cx);
                    view.set_status(UiStatus::info("Готово. Вставьте HTTP(S)-ссылку в разделе «Источники»."), cx);
                    view.set_playback_available(state.borrow().source.as_ref().is_some_and(|s| s.http_url().is_some()), cx);
                }
                Event::PreferenceSaved(player) => {
                    view.set_default_player(choice(player), cx);
                    view.set_preferences_status("Выбор плеера сохранён. Активный плеер не переключается.", cx);
                }
                Event::PlaybackActive(active) => {
                    state.borrow_mut().active = active;
                    view.set_playback_active(active, cx);
                }
                Event::Status(level, message) => {
                    let status = match level {
                        Level::Info => UiStatus::info(message),
                        Level::Warning => UiStatus::warning(message),
                        Level::Error => UiStatus::error(message),
                    };
                    view.set_status(status, cx);
                }
                Event::LibraryCount(count) => {
                    let message = if count == 0 {
                        SectionState::Empty("История пуста. Запустите ручной HTTP-источник в mpv или VLC.".into())
                    } else {
                        SectionState::Unavailable(format!(
                            "В локальной истории: {count} записей (показаны последние 100). \
                             Список библиотеки ещё не подключён. Для продолжения вставьте тот же источник."
                        ))
                    };
                    view.set_section_state(Section::Library, message, cx);
                }
            }) else {
                break;
            };
        }
        // A failed worker must not leave launch controls apparently usable.
        {
            let mut state = state.borrow_mut();
            state.ready = false;
            state.active = false;
        }
        let _ = view.update(cx, |view, cx| {
            view.set_playback_available(false, cx);
            view.set_playback_active(false, cx);
        });
    })
    .detach();
    Ok(desktop)
}

fn select_source(
    state: &Rc<RefCell<Desktop>>,
    view: &Entity<KadrView>,
    result: Result<ManualSource, String>,
    cx: &mut App,
) {
    let mut state = state.borrow_mut();
    // A file picker can finish after the user has started another source.
    if state.active {
        return;
    }
    match result {
        Ok(source) => {
            let (kind, title) = match source.kind() {
                SourceKind::Http => (UiSourceKind::Http, "HTTP(S)-источник"),
                SourceKind::Magnet => (UiSourceKind::Magnet, "Magnet-источник"),
                SourceKind::TorrentFile => (UiSourceKind::TorrentFile, "Локальный .torrent"),
            };
            let http = source.http_url().is_some();
            state.source = Some(source);
            view.update(cx, |view, cx| {
                view.set_source_preview(Some(SourcePreview { kind, title: title.into() }), cx);
                view.set_playback_available(state.ready && http, cx);
                view.show_section(Section::Sources, cx);
                view.set_status(
                    if http {
                        UiStatus::info("Источник проверен. «Продолжить / смотреть» восстановит позицию, «С начала» — начнёт заново. URL не сохраняется.")
                    } else {
                        UiStatus::warning("Источник проверен. BitTorrent пока доступен только как отдельный spike; сеть не запущена.")
                    },
                    cx,
                );
            });
        }
        Err(message) => {
            // Do not accidentally play the previous source after invalid replacement.
            state.source = None;
            view.update(cx, |view, cx| {
                view.set_source_preview(None, cx);
                view.set_playback_available(false, cx);
                view.set_status(UiStatus::error(message), cx);
            });
        }
    }
}

fn preference(player: PlayerChoice) -> PlayerPreference {
    match player {
        PlayerChoice::Mpv => PlayerPreference::Mpv,
        PlayerChoice::Vlc => PlayerPreference::Vlc,
        PlayerChoice::Browser => PlayerPreference::Browser,
    }
}

fn choice(player: PlayerPreference) -> PlayerChoice {
    match player {
        PlayerPreference::Mpv => PlayerChoice::Mpv,
        PlayerPreference::Vlc => PlayerChoice::Vlc,
        PlayerPreference::Browser => PlayerChoice::Browser,
    }
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
