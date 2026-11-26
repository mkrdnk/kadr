//! Controller-driven GPUI presentation layer for kadr.
//!
//! Subscribe to [`UiEvent`] before showing the view. The controller owns all
//! source clipboard/dialog access, validation, persistence, network and player work.
//! Setters and rendering perform no IO. Leaving Sources emits cancellation for
//! an inline account request so the controller can discard stale responses.

mod cinema;
mod dialogs;
mod icons;
mod input;
mod model;
mod screens;

pub use icons::IconAssets;
pub use model::*;

use gpui::{
    Context, Div, ElementId, EventEmitter, FocusHandle, Focusable, KeyDownEvent, ScrollHandle,
    Stateful, Window, div, prelude::*, px, rgb,
};
use icons::icon;
use model::ShellModel;

const BACKGROUND: u32 = 0x141416;
const SIDEBAR: u32 = 0x19191c;
const PANEL: u32 = 0x202023;
const ELEVATED: u32 = 0x28282c;
const HOVER: u32 = 0x303034;
const BORDER: u32 = 0x343438;
const TEXT: u32 = 0xf4f4f4;
const MUTED: u32 = 0xa1a1aa;
const DIM: u32 = 0x71717a;
const ACCENT: u32 = 0xd5fa72;
const WARNING: u32 = 0xf0cb87;
const ERROR: u32 = 0xf2a6a6;

const CATALOG_ONBOARDING: &str = "В настроенной релизной сборке каталог Trakt работает без аккаунта, пользовательского токена \
     и собственного сервера. Вход через OAuth и личные секреты не нужны.";

/// Reusable root view. It owns presentation state, never a source URI.
///
/// `new()` starts on Home with playback disabled. The application must
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
    detail_from: Section,
    modal_from: Option<Section>,
    catalog_order: cinema::CatalogOrder,
    search_focus: bool,
    search_last: String,
    search_cursor: usize,
    search_scroll: ScrollHandle,
    preparing: bool,
    source_quality: SourceQuality,
    source_order_seeds: bool,
    rutracker_login: Option<RuTrackerLogin>,
    rutracker_focus: Option<FocusHandle>,
    focus_login: bool,
}

struct RuTrackerLogin {
    message: Option<String>,
    pending: bool,
}

impl EventEmitter<UiEvent> for KadrView {}

#[derive(Clone)]
enum Command {
    Navigate(Section),
    Emit(UiEvent),
    Search,
    OpenSearch,
    CloseOverlay,
    CycleCatalogOrder,
    SourceQuality(SourceQuality),
    ToggleSourceOrder,
    SavePreferences,
    AssignEpisode,
    ToggleTorrents,
    ToggleCatalog,
    FilterLibrary(screens::LibraryFilter),
    BackToResults,
    SignInRuTracker,
    DismissRuTracker,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum SourceQuality {
    #[default]
    All,
    Hd,
    FullHd,
    UltraHd,
}

impl SourceQuality {
    const ALL: [Self; 4] = [Self::All, Self::Hd, Self::FullHd, Self::UltraHd];

    fn label(self) -> &'static str {
        match self {
            Self::All => "Все",
            Self::Hd => "720p",
            Self::FullHd => "1080p",
            Self::UltraHd => "4K",
        }
    }

    fn matches(self, row: &SourceRow) -> bool {
        if self == Self::All {
            return true;
        }
        row.quality.as_deref().is_some_and(|quality| {
            let quality = quality.to_ascii_lowercase();
            match self {
                Self::All => true,
                Self::Hd => quality.contains("720p"),
                Self::FullHd => quality.contains("1080p"),
                Self::UltraHd => quality.contains("2160p") || quality.contains("4k"),
            }
        })
    }
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
        self.clear_rutracker_login(cx);
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

    pub fn set_preparing(&mut self, preparing: bool, cx: &mut Context<Self>) {
        self.preparing = preparing;
        cx.notify();
    }

    pub fn set_browse_catalog(
        &mut self,
        movies: Vec<MediaCard>,
        shows: Vec<MediaCard>,
        cx: &mut Context<Self>,
    ) {
        self.model.movies = movies;
        self.model.shows = shows;
        cx.notify();
    }

    pub fn set_browse_state(&mut self, state: SectionState, cx: &mut Context<Self>) {
        self.model.browse_state = state;
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
        if section == Section::Search {
            self.run_command(&Command::OpenSearch, cx);
            return;
        }
        self.modal_from = None;
        self.detail_open = false;
        self.change_section(section, cx);
    }

    fn change_section(&mut self, section: Section, cx: &mut Context<Self>) {
        // Leaving Sources cancels its pending request, even before a login prompt arrives.
        if self.model.section == Section::Sources
            && section != Section::Sources
            && (self.rutracker_login.is_some()
                || matches!(
                    self.model.states[Section::Sources.index()],
                    SectionState::Loading(_)
                ))
        {
            self.clear_rutracker_login(cx);
            // Invalidate the controller's selected offer and any in-flight login.
            cx.emit(UiEvent::Cancel(Section::Sources));
        }
        self.model.section = section;
        cx.notify();
    }

    fn show_overlay(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.model.section != section {
            self.modal_from = Some(self.page());
            self.change_section(section, cx);
        }
    }

    fn page(&self) -> Section {
        self.modal_from.unwrap_or(self.model.section)
    }

    pub fn show_source_overlay(&mut self, cx: &mut Context<Self>) {
        self.show_overlay(Section::Sources, cx);
    }

    fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if let Some(section) = self.modal_from.take() {
            if self.model.section == Section::Search
                && matches!(
                    self.model.states[Section::Search.index()],
                    SectionState::Loading(_)
                )
            {
                cx.emit(UiEvent::Cancel(Section::Search));
            }
            self.change_section(section, cx);
        }
    }

    pub fn set_search_results(&mut self, rows: Vec<MediaCard>, cx: &mut Context<Self>) {
        self.model.results = rows;
        self.search_cursor = 0;
        cx.notify();
    }

    pub fn set_media_detail(&mut self, media: Option<MediaCard>, cx: &mut Context<Self>) {
        self.clear_rutracker_login(cx);
        let changed = self.model.media.as_ref().map(|media| &media.id)
            != media.as_ref().map(|media| &media.id);
        if changed {
            self.detail_open = media.is_some() && self.page() == self.detail_from;
            self.model.selected_episode = None;
        }
        self.model.media = media;
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
        self.clear_rutracker_login(cx);
        self.model.sources = rows;
        cx.notify();
    }

    /// Show the inline Sources account prompt only after the controller needs login.
    /// Messages must be safe for display, never raw authentication errors or secrets.
    /// `None, false` dismisses it. The controller owns vault access and cancellation.
    /// Leaving Sources emits `Cancel(Sources)`; ignore stale login responses then.
    /// Replacing media, source rows, source preview or files also clears the prompt.
    pub fn set_rutracker_login(
        &mut self,
        message: Option<String>,
        pending: bool,
        cx: &mut Context<Self>,
    ) {
        if self.model.section != Section::Sources || (message.is_none() && !pending) {
            self.clear_rutracker_login(cx);
        } else {
            self.focus_login |= self
                .rutracker_login
                .as_ref()
                .is_none_or(|login| login.pending != pending);
            if pending {
                self.clear_rutracker_password(cx);
            }
            self.rutracker_login = Some(RuTrackerLogin { message, pending });
        }
        cx.notify();
    }

    fn clear_rutracker_password(&mut self, cx: &mut Context<Self>) {
        if let Some(inputs) = &self.inputs {
            inputs.rutracker_password.update(cx, |input, cx| {
                input.set_value(String::new(), cx);
            });
        }
    }

    fn clear_rutracker_login(&mut self, cx: &mut Context<Self>) {
        self.rutracker_login = None;
        self.focus_login = false;
        self.clear_rutracker_password(cx);
        if let Some(inputs) = &self.inputs {
            inputs.rutracker_username.update(cx, |input, cx| {
                input.set_value(String::new(), cx);
            });
        }
    }

    pub fn set_torrent_files(&mut self, rows: Vec<TorrentFileRow>, cx: &mut Context<Self>) {
        self.clear_rutracker_login(cx);
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
        if !preferences.catalog_enabled {
            self.model.movies.clear();
            self.model.shows.clear();
            self.model.results.clear();
            self.model.browse_state = SectionState::Unavailable("Сетевой каталог отключён.".into());
        } else if !self.applied_catalog_enabled {
            self.model.browse_state = SectionState::Empty("Каталог ещё не загружен.".into());
        }
        self.applied_catalog_enabled = preferences.catalog_enabled;
        self.model.preferences = preferences;
        self.preferences_loaded = true;
        cx.notify();
    }

    fn run_command(&mut self, command: &Command, cx: &mut Context<Self>) {
        match command {
            Command::Navigate(Section::Search) | Command::OpenSearch => {
                self.show_overlay(Section::Search, cx);
                self.search_focus = true;
                cx.notify();
            }
            Command::Navigate(section) => {
                self.show_section(*section, cx);
                if matches!(section, Section::Movies | Section::Shows)
                    && self.preferences_loaded
                    && self.applied_catalog_enabled
                    && self.model.movies.is_empty()
                    && self.model.shows.is_empty()
                    && matches!(self.model.browse_state, SectionState::Empty(_))
                {
                    cx.emit(UiEvent::Browse);
                }
            }
            Command::CloseOverlay => self.dismiss_overlay(cx),
            Command::CycleCatalogOrder => {
                self.catalog_order.cycle();
                cx.notify();
            }
            Command::SourceQuality(quality) => {
                self.source_quality = *quality;
                cx.notify();
            }
            Command::ToggleSourceOrder => {
                self.source_order_seeds = !self.source_order_seeds;
                cx.notify();
            }
            Command::Emit(event) if self.model.accepts(event) => {
                if matches!(
                    event,
                    UiEvent::ChooseSource(_)
                        | UiEvent::ChooseTorrentFile(_)
                        | UiEvent::FindSources
                        | UiEvent::SelectEpisode { .. }
                        | UiEvent::UnlinkMedia
                        | UiEvent::ForgetRuTracker
                        | UiEvent::Cancel(Section::Sources)
                ) && self.rutracker_login.is_some()
                {
                    self.clear_rutracker_login(cx);
                    cx.notify();
                }
                match event {
                    UiEvent::FindSources => {
                        self.source_quality = SourceQuality::All;
                        self.source_order_seeds = false;
                    }
                    UiEvent::OpenMedia(_) | UiEvent::OpenLibrary(_) => {
                        self.detail_from = self.page();
                        self.modal_from = None;
                        self.detail_open = false;
                        self.model.media = None;
                        self.change_section(self.detail_from, cx);
                    }
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
            Command::Search => self.run_search(false, cx),
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
            Command::SignInRuTracker => self.submit_rutracker_login(cx),
            Command::DismissRuTracker => {
                if self.rutracker_login.is_some() {
                    self.clear_rutracker_login(cx);
                    cx.emit(UiEvent::Cancel(Section::Sources));
                    cx.notify();
                }
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
        self.action(id, command, enabled, cx)
            .flex()
            .items_center()
            .justify_center()
            .h(px(38.))
            .px(px(16.))
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { BORDER }))
            .bg(rgb(if selected { ACCENT } else { ELEVATED }))
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(if selected { BACKGROUND } else { TEXT }))
            .child(label.into())
            .when(enabled, |button| {
                button.hover(|style| style.bg(rgb(if selected { 0xe0ff8d } else { HOVER })))
            })
    }

    fn action(
        &self,
        id: impl Into<ElementId>,
        command: Command,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let keyboard_command = command.clone();
        div()
            .id(id)
            .when(!enabled, |button| button.opacity(0.45))
            .when(enabled, |button| {
                button
                    .tab_index(0)
                    .cursor_pointer()
                    .focus(|style| style.border_color(rgb(ACCENT)))
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

    fn navigation(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let compact = width <= 900.;
        let rail = sidebar_width(width);
        let page = self.page();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(rail))
            .px(px(if compact { 8. } else { 13. }))
            .pt(px(20.))
            .pb(px(12.))
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(0x2a2a2e))
            .child(
                self.action("brand", Command::Navigate(Section::Home), true, cx)
                    .flex()
                    .items_center()
                    .gap(px(0.))
                    .when(compact, |brand| brand.justify_center())
                    .px(px(7.))
                    .mb(px(17.))
                    .text_size(px(if compact { 22. } else { 29. }))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(if compact { "k" } else { "kadr" })
                    .child(div().text_color(rgb(ACCENT)).child(".")),
            )
            .child(
                self.action("open-search", Command::OpenSearch, true, cx)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(34.))
                    .px(px(8.))
                    .mb(px(14.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(0x2d2d31))
                    .bg(rgb(0x242428))
                    .text_color(rgb(MUTED))
                    .text_size(px(13.))
                    .when(compact, |item| item.justify_center())
                    .child(icon("search", 17., MUTED))
                    .when(!compact, |item| {
                        item.child(div().flex_1().child("Поиск")).child(
                            div().text_size(px(10.)).text_color(rgb(DIM)).child(
                                if cfg!(target_os = "macos") {
                                    "⌘ K"
                                } else {
                                    "Ctrl K"
                                },
                            ),
                        )
                    })
                    .hover(|item| item.bg(rgb(HOVER))),
            )
            .children(
                [Section::Home, Section::Movies, Section::Shows]
                    .into_iter()
                    .map(|section| self.nav_item(section, page == section, compact, cx)),
            )
            .when(!compact, |rail| {
                rail.child(eyebrow("БИБЛИОТЕКА").ml(px(9.)).mt(px(24.)).mb(px(7.)))
            })
            .children(
                [Section::Library, Section::History, Section::Transfers]
                    .into_iter()
                    .map(|section| self.nav_item(section, page == section, compact, cx)),
            )
            .child(div().flex_1())
            .child(self.nav_item(Section::Sources, page == Section::Sources, compact, cx))
            .child(self.nav_item(Section::Settings, page == Section::Settings, compact, cx))
            .child(
                self.action(
                    "player-chip",
                    Command::Navigate(Section::Settings),
                    true,
                    cx,
                )
                .flex()
                .items_center()
                .gap(px(9.))
                .border_t_1()
                .border_color(rgb(0x29292d))
                .mt(px(12.))
                .pt(px(13.))
                .px(px(8.))
                .when(compact, |item| item.justify_center())
                .child(status_dot())
                .when(!compact, |item| {
                    item.child(
                        div()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .child(self.model.default_player.label()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(DIM))
                                    .child("Плеер по умолчанию"),
                            ),
                    )
                    .child(icon("chevron", 14., DIM))
                }),
            )
    }

    fn nav_item(
        &self,
        section: Section,
        selected: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let glyph = match section {
            Section::Home => "home",
            Section::Movies => "film",
            Section::Shows => "tv",
            Section::Search => "search",
            Section::Library => "bookmark",
            Section::History => "clock",
            Section::Transfers => "download",
            Section::Sources => "link",
            Section::Settings => "settings",
        };
        let fg = if selected { TEXT } else { MUTED };
        self.action(section.id(), Command::Navigate(section), true, cx)
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(5.))
            .h(px(34.))
            .my(px(1.))
            .px(px(9.))
            .text_size(px(13.))
            .text_color(rgb(fg))
            .when(compact, |item| item.justify_center())
            .when(selected, |item| {
                item.bg(rgb(HOVER)).border_l_2().border_color(rgb(ACCENT))
            })
            .focus(|item| item.bg(rgb(HOVER)).border_l_2().border_color(rgb(ACCENT)))
            .hover(|item| item.bg(rgb(if selected { HOVER } else { 0x242428 })))
            .child(icon(glyph, 17., fg))
            .when(!compact, |item| item.child(section.title()))
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
            .when(
                section == Section::Sources && !matches!(state, SectionState::Loading(_)),
                |card| {
                    card.when_some(self.source_search_notice(), |card, notice| {
                        card.child(body(notice))
                    })
                },
            )
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
        let mut content = column()
            .gap(px(12.))
            .when(self.rutracker_login.is_some(), |content| {
                content.child(self.rutracker_login_form(cx))
            })
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
            .child(
                div()
                    .pt(px(11.))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(eyebrow("СВОЙ ИСТОЧНИК"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .my(px(8.))
                            .child(self.button(
                                "paste-source",
                                "Вставить ссылку",
                                Command::Emit(UiEvent::PasteSource),
                                false,
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
                    .child(source_disclosure()),
            )
    }

    fn visible_sources(&self) -> Vec<&SourceRow> {
        let mut rows: Vec<_> = self
            .model
            .sources
            .iter()
            .filter(|row| self.source_quality.matches(row))
            .collect();
        if self.source_order_seeds {
            rows.sort_by_key(|row| std::cmp::Reverse(row.seeds));
        }
        rows
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
            .child(
                panel()
                    .child(eyebrow("АККАУНТ RUTRACKER"))
                    .child(body(
                        "Вход запрашивается в «Источниках», только если для выбранного предложения \
                         не удалось получить magnet. По умолчанию данные запоминаются в системном \
                         хранилище секретов. Если оно недоступно, вход действует только в текущей \
                         сессии; в конфигурацию пароль не записывается.",
                    ))
                    .child(self.button(
                        "forget-rutracker",
                        "Забыть аккаунт RuTracker",
                        Command::Emit(UiEvent::ForgetRuTracker),
                        false,
                        true,
                        cx,
                    )),
            )
            .when(
                self.show_state(Section::Settings, self.preferences_loaded),
                |content| content.child(self.state_card(Section::Settings, cx)),
            )
            .child(
                panel()
                    .child(eyebrow("ЛОКАЛЬНО И БЕЗ СЕРВЕРА KADR"))
                    .child(body(
                        "Аудиодорожки и субтитры выбираются во внешнем плеере. \
                         Встроенного видеоплеера, аккаунта kadr и облачной синхронизации нет. \
                         Аккаунт RuTracker необязателен.",
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
            self.rutracker_focus = Some(cx.focus_handle());
        }
        // Establish the root keyboard focus once. Subsequent renders preserve
        // the user's focused control. This is GPUI state, not platform IO.
        if self.focus.is_none() {
            let focus = cx.focus_handle();
            window.focus(&focus);
            self.focus = Some(focus);
        }
        if self.search_focus && self.model.section == Section::Search {
            if let Some(inputs) = &self.inputs {
                cx.focus_view(&inputs.search, window);
            }
            self.search_focus = false;
        } else if self.model.section != Section::Search
            && self.inputs.as_ref().is_some_and(|inputs| {
                inputs
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .contains_focused(window, cx)
            })
        {
            window.focus(self.focus.as_ref().expect("focus initialized above"));
        }
        if self.focus_login && self.model.section == Section::Sources {
            if self
                .rutracker_login
                .as_ref()
                .is_some_and(|login| login.pending)
            {
                window.focus(
                    self.rutracker_focus
                        .as_ref()
                        .expect("login focus initialized"),
                );
            } else if let Some(inputs) = &self.inputs {
                cx.focus_view(&inputs.rutracker_username, window);
            }
            self.focus_login = false;
        }
        // Restore shell navigation when the focused form (including its buttons)
        // disappears. GPUI cannot route keys to a removed focus node.
        if (self.model.section != Section::Sources || self.rutracker_login.is_none())
            && self
                .rutracker_focus
                .as_ref()
                .is_some_and(|focus| focus.contains_focused(window, cx))
        {
            window.focus(self.focus.as_ref().expect("focus initialized above"));
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width);
        let content_width = width - sidebar_width(width);
        let section = self.page();
        let detail = self.detail_open && self.model.media.is_some();
        let content = if detail {
            self.detail_page(content_width, cx)
        } else {
            match section {
                Section::Home | Section::Search => self.home(content_width, cx),
                Section::Movies | Section::Shows => self.catalog_page(section, content_width, cx),
                Section::Library | Section::History => utility_page(section, self.library(cx)),
                Section::Sources => utility_page(section, self.sources(cx)),
                Section::Transfers => utility_page(section, self.transfers(cx)),
                Section::Settings => utility_page(section, self.settings(cx)),
            }
        };
        let status_color = match self.model.status.level {
            StatusLevel::Info => MUTED,
            StatusLevel::Warning => WARNING,
            StatusLevel::Error => ERROR,
        };

        let overlay = self.overlay(width, f32::from(viewport.height), cx);
        div()
            .id("kadr-shell")
            .track_focus(self.focus.as_ref().expect("focus initialized above"))
            .tab_group()
            .tab_stop(false)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if !this.preparing && key == "k" && event.keystroke.modifiers.secondary() {
                    this.run_command(&Command::OpenSearch, cx);
                } else if key == "escape" && this.preparing {
                    this.run_command(&Command::Emit(UiEvent::Stop), cx);
                } else if key == "escape" && this.modal_from.is_some() {
                    this.dismiss_overlay(cx);
                } else if this.model.section == Section::Search && !this.preparing {
                    match key {
                        "up" | "down" => this.move_search_cursor(key == "down", cx),
                        "enter" if !event.is_held => this.run_search(true, cx),
                        "tab" if event.keystroke.modifiers.shift => window.focus_prev(),
                        "tab" => window.focus_next(),
                        _ => return,
                    }
                } else if key == "tab" && event.keystroke.modifiers.shift {
                    window.focus_prev();
                } else if key == "tab" {
                    window.focus_next();
                } else {
                    return;
                }
                cx.stop_propagation();
            }))
            .relative()
            .size_full()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .text_size(px(13.))
            .child(
                div()
                    .size_full()
                    .flex()
                    .child(self.navigation(width, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id((
                                        "section-scroll",
                                        section.index() * 2 + usize::from(detail),
                                    ))
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(content),
                            )
                            .when(
                                self.model.status.level != StatusLevel::Info && overlay.is_none(),
                                |page| {
                                    page.child(
                                        div()
                                            .px(px(22.))
                                            .py(px(7.))
                                            .border_t_1()
                                            .border_color(rgb(BORDER))
                                            .text_size(px(11.))
                                            .text_color(rgb(status_color))
                                            .child(self.model.status.message.clone()),
                                    )
                                },
                            ),
                    ),
            )
            .when_some(overlay, |root, layer| root.child(layer))
    }
}

fn sidebar_width(width: f32) -> f32 {
    if width <= 900. {
        58.
    } else if width <= 1120. {
        190.
    } else {
        220.
    }
}

fn utility_page(section: Section, content: Div) -> Div {
    column()
        .gap(px(20.))
        .max_w(px(1400.))
        .mx_auto()
        .px(px(30.))
        .pt(px(28.))
        .pb(px(48.))
        .child(
            div().child(eyebrow("KADR")).child(
                div()
                    .mt(px(3.))
                    .text_size(px(27.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(section.title()),
            ),
        )
        .child(content)
}

fn status_dot() -> Div {
    div()
        .size(px(6.))
        .rounded_full()
        .bg(rgb(ACCENT))
        .flex_shrink_0()
}

fn column() -> Div {
    div().flex().flex_col().gap_4().w_full()
}

fn panel() -> Div {
    column()
        .p(px(16.))
        .gap(px(10.))
        .rounded(px(7.))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
}

fn eyebrow(text: &'static str) -> Div {
    div()
        .text_size(px(10.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(text)
}

fn body(text: impl Into<String>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(text.into())
}

fn source_disclosure() -> Div {
    body(
        "Trakt предоставляет только метаданные и ссылки на изображения, не видео. \
         При включённом сетевом поиске локальное приложение отправляет запросы напрямую \
         в RuTracker. Предложения трекера не проверены; права на контент неизвестны. \
         Вход не включает BitTorrent: разрешение на обмен частями файлов задаётся отдельно в настройках.",
    )
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

#[cfg(test)]
mod shell_tests {
    use super::*;

    fn source(index: usize, quality: Option<&str>, seeds: Option<u32>) -> SourceRow {
        SourceRow {
            index,
            title: format!("Source {index}"),
            kind: SourceKind::Magnet,
            quality: quality.map(str::to_owned),
            language: None,
            origin: "RuTracker".into(),
            license: None,
            size: None,
            seeds,
            rutracker: true,
        }
    }

    #[test]
    fn source_tabs_and_seed_order_preserve_engine_indices() {
        let mut view = KadrView::new();
        view.model.sources = vec![
            source(3, Some("2160p"), Some(5)),
            source(5, None, None),
            source(8, Some("4K"), Some(12)),
            source(13, Some("1080p"), Some(20)),
        ];
        let indices = |view: &KadrView| {
            view.visible_sources()
                .into_iter()
                .map(|row| row.index)
                .collect::<Vec<_>>()
        };
        assert_eq!(indices(&view), [3, 5, 8, 13]);
        view.source_order_seeds = true;
        assert_eq!(indices(&view), [13, 8, 3, 5]);
        view.source_quality = SourceQuality::UltraHd;
        assert_eq!(indices(&view), [8, 3]);
        assert!(view.model.accepts(&UiEvent::ChooseSource(8)));
    }

    #[test]
    fn sidebar_breakpoints_follow_the_reference_widths() {
        assert_eq!(sidebar_width(900.), 58.);
        assert_eq!(sidebar_width(901.), 190.);
        assert_eq!(sidebar_width(1120.), 190.);
        assert_eq!(sidebar_width(1121.), 220.);
    }
}
