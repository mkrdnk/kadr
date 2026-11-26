//! Controller-driven GPUI presentation layer for kadr.
//!
//! Subscribe to [`UiEvent`] before showing the view. The controller owns all
//! source clipboard/dialog access, validation, persistence, network and player work.
//! Setters only change display state and notify GPUI; rendering performs no IO.

mod input;
mod model;
mod screens;

pub use model::*;

use gpui::{
    Context, Div, ElementId, EventEmitter, FocusHandle, KeyDownEvent, Stateful, Window, div,
    prelude::*, px, rgb,
};
use model::ShellModel;

const BACKGROUND: u32 = 0x11161c;
const SIDEBAR: u32 = 0x171e26;
const PANEL: u32 = 0x1c2530;
const BORDER: u32 = 0x334150;
const TEXT: u32 = 0xe8edf3;
const MUTED: u32 = 0xa9b7c6;
const ACCENT: u32 = 0x9adbc5;
const WARNING: u32 = 0xf0cb87;
const ERROR: u32 = 0xf2a6a6;

/// Reusable root view. It owns presentation state, never a source URI.
///
/// `new()` starts on Sources with playback disabled. The application must
/// acknowledge requested setting changes using the setters after persistence.
#[derive(Default)]
pub struct KadrView {
    model: ShellModel,
    focus: Option<FocusHandle>,
    inputs: Option<screens::Inputs>,
    library_filter: screens::LibraryFilter,
    preferences_loaded: bool,
    applied_catalog_enabled: bool,
    detail_open: bool,
}

impl EventEmitter<UiEvent> for KadrView {}

#[derive(Clone)]
enum Command {
    Navigate(Section),
    Emit(UiEvent),
    Search,
    SavePreferences,
    AssignEpisode,
    ToggleTorrents,
    ToggleCatalog,
    FilterLibrary(screens::LibraryFilter),
    BackToResults,
}

impl KadrView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, status: UiStatus, cx: &mut Context<Self>) {
        self.model.status = status;
        cx.notify();
    }

    pub fn set_default_player(&mut self, player: PlayerChoice, cx: &mut Context<Self>) {
        self.model.default_player = player;
        cx.notify();
    }

    pub fn set_source_preview(&mut self, source: Option<SourcePreview>, cx: &mut Context<Self>) {
        self.model.source = source;
        cx.notify();
    }

    /// Set true only if a controller can actually handle Play.
    pub fn set_playback_available(&mut self, available: bool, cx: &mut Context<Self>) {
        self.model.playback_available = available;
        cx.notify();
    }

    /// Set true before starting an asynchronous launch, not just after it.
    /// Keeps duplicate Play and source replacement disabled until stopped.
    pub fn set_playback_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.model.playback_active = active;
        cx.notify();
    }

    pub fn set_section_state(
        &mut self,
        section: Section,
        state: SectionState,
        cx: &mut Context<Self>,
    ) {
        self.model.states[section.index()] = state;
        cx.notify();
    }

    pub fn set_preferences_status(&mut self, status: impl Into<String>, cx: &mut Context<Self>) {
        self.model.preferences_status = status.into();
        cx.notify();
    }

    pub fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.model.section = section;
        cx.notify();
    }

    pub fn set_search_results(&mut self, rows: Vec<MediaCard>, cx: &mut Context<Self>) {
        self.model.results = rows;
        self.detail_open = false;
        cx.notify();
    }

    pub fn set_media_detail(&mut self, media: Option<MediaCard>, cx: &mut Context<Self>) {
        self.detail_open = media.is_some();
        self.model.media = media;
        self.model.selected_episode = None;
        self.model.media_favorite = false;
        cx.notify();
    }

    pub fn set_media_favorite(&mut self, favorite: bool, cx: &mut Context<Self>) {
        self.model.media_favorite = favorite;
        cx.notify();
    }

    pub fn set_cache_directory(&mut self, directory: String, cx: &mut Context<Self>) {
        self.model.preferences.cache_directory = directory.clone();
        if let Some(inputs) = &self.inputs {
            inputs
                .cache_directory
                .update(cx, |input, cx| input.set_value(directory, cx));
        }
        cx.notify();
    }

    pub fn set_sources(&mut self, rows: Vec<SourceRow>, cx: &mut Context<Self>) {
        self.model.sources = rows;
        cx.notify();
    }

    pub fn set_torrent_files(&mut self, rows: Vec<TorrentFileRow>, cx: &mut Context<Self>) {
        self.model.files = rows;
        self.model.selected_file = None;
        cx.notify();
    }

    pub fn set_library(&mut self, rows: Vec<LibraryRow>, cx: &mut Context<Self>) {
        self.model.library = rows;
        cx.notify();
    }

    pub fn set_transfer(&mut self, transfer: Option<TransferView>, cx: &mut Context<Self>) {
        self.model.transfer = transfer;
        cx.notify();
    }

    pub fn set_preferences(&mut self, preferences: PreferencesDraft, cx: &mut Context<Self>) {
        if let Some(inputs) = &self.inputs {
            inputs.set_preferences(&preferences, cx);
        }
        self.applied_catalog_enabled = preferences.catalog_enabled;
        self.model.preferences = preferences;
        self.preferences_loaded = true;
        cx.notify();
    }

    fn run_command(&mut self, command: &Command, cx: &mut Context<Self>) {
        match command {
            Command::Navigate(section) => self.show_section(*section, cx),
            Command::Emit(event) if self.model.accepts(event) => {
                match event {
                    UiEvent::SelectEpisode { season, number } => {
                        self.model.selected_episode = Some((*season, *number));
                        cx.notify();
                    }
                    UiEvent::ChooseTorrentFile(index) => {
                        self.model.selected_file = Some(*index);
                        if let Some(file) =
                            self.model.files.iter().find(|file| file.index == *index)
                            && let Some(inputs) = &self.inputs
                        {
                            inputs.season.update(cx, |input, cx| {
                                input.set_value(
                                    file.season.map(|n| n.to_string()).unwrap_or_default(),
                                    cx,
                                )
                            });
                            inputs.episode.update(cx, |input, cx| {
                                input.set_value(
                                    file.episode.map(|n| n.to_string()).unwrap_or_default(),
                                    cx,
                                )
                            });
                        }
                        cx.notify();
                    }
                    _ => {}
                }
                cx.emit(event.clone());
            }
            Command::Emit(_) => {}
            Command::Search => self.submit_search(cx),
            Command::SavePreferences => self.submit_preferences(cx),
            Command::AssignEpisode => self.submit_episode(cx),
            Command::ToggleTorrents => {
                self.model.preferences.torrents_enabled = !self.model.preferences.torrents_enabled;
                cx.notify();
            }
            Command::ToggleCatalog => {
                self.model.preferences.catalog_enabled = !self.model.preferences.catalog_enabled;
                cx.notify();
            }
            Command::FilterLibrary(filter) => {
                self.library_filter = *filter;
                cx.notify();
            }
            Command::BackToResults => {
                self.detail_open = false;
                cx.notify();
            }
        }
    }

    fn button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<String>,
        command: Command,
        selected: bool,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let keyboard_command = command.clone();
        div()
            .id(id)
            .px_4()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { BORDER }))
            .bg(rgb(if selected { 0x29483f } else { PANEL }))
            .text_sm()
            .text_color(rgb(if selected { ACCENT } else { TEXT }))
            .child(label.into())
            .when(!enabled, |button| button.opacity(0.45))
            .when(enabled, |button| {
                button
                    .tab_index(0)
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x30404c)))
                    .focus(|style| style.border_color(rgb(ACCENT)).bg(rgb(0x30404c)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_command(&command, cx);
                    }))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                            && !event.is_held
                        {
                            cx.stop_propagation();
                            this.run_command(&keyboard_command, cx);
                        }
                    }))
            })
    }

    fn navigation(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(172.0))
            .p_4()
            .gap_2()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_6()
                    .child(div().text_3xl().text_color(rgb(ACCENT)).child("kadr"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("Смотреть по-своему"),
                    ),
            )
            .children(Section::ALL.into_iter().map(|section| {
                self.button(
                    section.id(),
                    section.title(),
                    Command::Navigate(section),
                    section == self.model.section,
                    true,
                    cx,
                )
            }))
            .child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .pt_4()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("Локальное приложение"),
            )
    }

    fn state_card(&self, section: Section, cx: &mut Context<Self>) -> Div {
        let state = &self.model.states[section.index()];
        let color = match state {
            SectionState::Error(_) => ERROR,
            SectionState::Loading(_) => ACCENT,
            _ => MUTED,
        };
        panel()
            .child(
                div()
                    .text_lg()
                    .text_color(rgb(color))
                    .child(state.heading()),
            )
            .child(body(state.message()))
            .when(matches!(state, SectionState::Error(_)), |card| {
                card.child(self.button(
                    "retry-section",
                    "Повторить",
                    Command::Emit(UiEvent::Retry(section)),
                    false,
                    true,
                    cx,
                ))
            })
            .when(matches!(state, SectionState::Loading(_)), |card| {
                card.child(self.button(
                    "cancel-section",
                    "Отменить",
                    Command::Emit(UiEvent::Cancel(section)),
                    false,
                    true,
                    cx,
                ))
            })
    }

    fn open_source_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        self.button(
            "go-to-sources",
            "Открыть свой источник",
            Command::Navigate(Section::Sources),
            true,
            true,
            cx,
        )
    }

    fn sources(&self, cx: &mut Context<Self>) -> Div {
        let can_paste = self.model.accepts(&UiEvent::PasteSource);
        let can_open = self.model.accepts(&UiEvent::OpenTorrent);
        let mut content = column().child(
            panel()
                .border_color(rgb(0x49695f))
                .child(eyebrow("ОТКРЫТЬ ИСТОЧНИК"))
                .child(div().text_xl().child("Есть ссылка на видео?"))
                .child(body(
                    "HTTP(S), magnet или файл .torrent — без поиска в каталоге.",
                ))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(self.button(
                            "paste-source",
                            "Вставить ссылку",
                            Command::Emit(UiEvent::PasteSource),
                            true,
                            can_paste,
                            cx,
                        ))
                        .child(self.button(
                            "open-torrent",
                            "Открыть .torrent",
                            Command::Emit(UiEvent::OpenTorrent),
                            false,
                            can_open,
                            cx,
                        )),
                )
                .child(body(
                    "Буфер обмена читается только по нажатию. Полная ссылка не отображается. \
                     Приложение проверит источник перед запуском.",
                )),
        );

        content = content
            .child(self.source_rows(cx))
            .child(self.torrent_files(cx));
        if let Some(source) = &self.model.source {
            content = content.child(
                panel()
                    .child(eyebrow("ВЫБРАННЫЙ ИСТОЧНИК"))
                    .child(div().text_lg().child(source.title.clone()))
                    .child(body(source.kind.label()))
                    .child(body(format!(
                        "Плеер по умолчанию: {}",
                        self.model.default_player.label()
                    )))
                    .child(
                        div().flex().flex_wrap().gap_2().children(
                            PlayerChoice::ALL
                                .into_iter()
                                .enumerate()
                                .map(|(index, player)| {
                                    self.button(
                                        ("source-player", index),
                                        player.label(),
                                        Command::Emit(UiEvent::SetDefaultPlayer(player)),
                                        self.model.default_player == player,
                                        !self.model.playback_active,
                                        cx,
                                    )
                                }),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(self.button(
                                "play-source",
                                if self.model.default_player == PlayerChoice::Mpv {
                                    "Продолжить / смотреть"
                                } else {
                                    "Воспроизвести"
                                },
                                Command::Emit(UiEvent::Play),
                                true,
                                self.model.can_play(),
                                cx,
                            ))
                            .when(self.model.default_player == PlayerChoice::Mpv, |buttons| {
                                buttons.child(self.button(
                                    "play-from-start",
                                    "С начала",
                                    Command::Emit(UiEvent::PlayFromStart),
                                    false,
                                    self.model.can_play(),
                                    cx,
                                ))
                            })
                            .child(self.button(
                                "stop-source",
                                "Остановить",
                                Command::Emit(UiEvent::Stop),
                                false,
                                self.model.playback_active,
                                cx,
                            )),
                    )
                    .when(self.model.default_player == PlayerChoice::Mpv, |card| {
                        card.child(body(
                            "Если сохранена измеренная позиция mpv, обычный запуск продолжит \
                             просмотр с неё. «С начала» запускает видео без возобновления.",
                        ))
                    })
                    .when(!self.model.playback_available, |card| {
                        card.child(body("Источник ещё не готов к запуску. Дождитесь проверки или выберите другой."))
                    })
                    .when(self.model.playback_active, |card| {
                        card.child(body(
                            "Сессия активна или запускается. Остановите её перед сменой источника.",
                        ))
                    }),
            );
        }
        // A preview is a real controller-provided success state; don't also show
        // the default "empty" card. Loading and errors remain visible.
        if self.show_state(
            Section::Sources,
            self.model.source.is_some()
                || !self.model.sources.is_empty()
                || !self.model.files.is_empty(),
        ) {
            content = content.child(self.state_card(Section::Sources, cx));
        }
        content
            .when(
                self.model.default_player == PlayerChoice::Browser,
                |content| content.child(browser_warning()),
            )
            .child(panel().child(eyebrow("О ПРАВАХ И СЕТИ")).child(body(
                "Открывайте только контент, на просмотр и распространение которого \
                         у вас есть права. BitTorrent может отдавать загруженные части \
                         другим участникам сети.",
            )))
    }

    fn settings(&self, cx: &mut Context<Self>) -> Div {
        column()
            .child(
                panel()
                    .child(eyebrow("ВОСПРОИЗВЕДЕНИЕ"))
                    .child(div().text_lg().child("Плеер по умолчанию"))
                    .child(div().flex().flex_wrap().gap_2().children(
                        PlayerChoice::ALL.into_iter().map(|player| {
                            self.button(
                                player.id(),
                                player.label(),
                                Command::Emit(UiEvent::SetDefaultPlayer(player)),
                                self.model.default_player == player,
                                true,
                                cx,
                            )
                        }),
                    ))
                    .child(body(
                        "Выбор не означает, что плеер установлен. Доступность проверяется \
                         приложением при запуске; kadr не устанавливает плееры.",
                    ))
                    .child(body(self.model.preferences_status.clone())),
            )
            .when(
                self.model.default_player == PlayerChoice::Browser,
                |content| content.child(browser_warning()),
            )
            .child(self.preference_fields(cx))
            .when(
                self.show_state(Section::Settings, self.preferences_loaded),
                |content| content.child(self.state_card(Section::Settings, cx)),
            )
            .child(
                panel()
                    .child(eyebrow("ЛОКАЛЬНО И БЕЗ АККАУНТА"))
                    .child(body(
                        "Аудиодорожки и субтитры выбираются во внешнем плеере. \
                         Встроенного видеоплеера, аккаунтов и облачной синхронизации нет.",
                    ))
                    .child(body(
                        "Сворачивание не останавливает воспроизведение. \
                         Для остановки используйте кнопку «Остановить» или выход из kadr.",
                    )),
            )
    }
}

impl Render for KadrView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.inputs.is_none() {
            input::init(cx);
            self.inputs = Some(screens::Inputs::new(&self.model.preferences, cx));
        }
        // Establish the root keyboard focus once. Subsequent renders preserve
        // the user's focused control. This is GPUI state, not platform IO.
        if self.focus.is_none() {
            let focus = cx.focus_handle();
            window.focus(&focus);
            self.focus = Some(focus);
        }
        let section = self.model.section;
        let subtitle = match section {
            Section::Search => "Найдите фильм или сериал",
            Section::Library => "Избранное, история, продолжение",
            Section::Sources => "Ваш источник → внешний плеер",
            Section::Transfers => "Загрузка и активные сессии",
            Section::Settings => "Настройте приложение под себя",
        };
        let content = match section {
            Section::Search => self.search(cx),
            Section::Library => self.library(cx),
            Section::Sources => self.sources(cx),
            Section::Transfers => self.transfers(cx),
            Section::Settings => self.settings(cx),
        };
        let status_color = match self.model.status.level {
            StatusLevel::Info => MUTED,
            StatusLevel::Warning => WARNING,
            StatusLevel::Error => ERROR,
        };

        div()
            .id("kadr-shell")
            .track_focus(self.focus.as_ref().expect("focus initialized above"))
            .tab_group()
            .tab_stop(false)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                }
            }))
            .flex()
            .size_full()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_base()
            .child(self.navigation(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex_shrink_0()
                            .p_6()
                            .border_b_1()
                            .border_color(rgb(BORDER))
                            .child(eyebrow("KADR"))
                            .child(div().text_2xl().child(section.title()))
                            .child(body(subtitle)),
                    )
                    .child(
                        div()
                            .id(("section-scroll", section.index()))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p_6()
                            .child(content),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .px_6()
                            .py_3()
                            .border_t_1()
                            .border_color(rgb(BORDER))
                            .text_sm()
                            .text_color(rgb(status_color))
                            .child(self.model.status.message.clone()),
                    ),
            )
    }
}

fn column() -> Div {
    div().flex().flex_col().gap_4().w_full()
}

fn panel() -> Div {
    column()
        .p_5()
        .gap_3()
        .rounded_lg()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
}

fn eyebrow(text: &'static str) -> Div {
    div().text_xs().text_color(rgb(ACCENT)).child(text)
}

fn body(text: impl Into<String>) -> Div {
    div().text_sm().text_color(rgb(MUTED)).child(text.into())
}

fn browser_warning() -> Div {
    panel()
        .border_color(rgb(0x79633f))
        .child(
            div()
                .text_sm()
                .text_color(rgb(WARNING))
                .child("Браузер: ограниченная совместимость"),
        )
        .child(body(
            "Контейнер или кодек может не поддерживаться. Открытие потока не \
             гарантирует воспроизведение; автоматическая позиция просмотра недоступна.",
        ))
}
