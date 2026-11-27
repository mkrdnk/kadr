//! Controller-driven GPUI presentation layer for kadr.
//!
//! Subscribe to [`UiEvent`] before showing the view. The controller owns all
//! source clipboard/dialog access, validation, persistence, network and player work.
//! Setters and rendering perform no IO. Leaving Sources or an explicit movie
//! watch emits cancellation so the controller can discard stale responses.

mod cinema;
mod dialogs;
mod icons;
mod input;
mod model;
mod screens;
mod source_preferences;

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

/// Reusable root view. Playback URIs remain controller-owned; configured
/// provider URLs are transient preferences input.
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
    source_quality: SourceQuality,
    source_order_seeds: bool,
    rutracker_login: Option<RuTrackerLogin>,
    rutracker_focus: Option<FocusHandle>,
    focus_login: bool,
    source_preferences: source_preferences::SourcePreferencesEditor,
    source_preferences_status: String,
    navigation_revision: u64,
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
    SaveSourcePreferences,
    ToggleCommons,
    ToggleRuTracker,
    TogglePopcorn,
    AddTorznab,
    RemoveTorznab(usize),
    ToggleTorznab(usize),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceGroup {
    Suitable,
    Possible,
}

impl SourceGroup {
    const ALL: [Self; 2] = [Self::Suitable, Self::Possible];

    fn of(row: &SourceRow) -> Self {
        if row.possible_match {
            Self::Possible
        } else {
            Self::Suitable
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Suitable => "Подходящие источники",
            Self::Possible => "Возможные совпадения",
        }
    }

    fn explanation(self) -> Option<&'static str> {
        (self == Self::Possible).then_some(
            "Название похоже, год или эпизод не подтверждён. Проверьте раздачу и выберите источник вручную.",
        )
    }
}

impl KadrView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture when starting asynchronous work; only explicitly navigate on its
    /// completion if the user has not navigated elsewhere in the meantime.
    pub fn navigation_revision(&self) -> u64 {
        self.navigation_revision
    }

    /// Only a visible movie card can authorize speculative source warming.
    pub fn movie_card_open(&self) -> bool {
        self.detail_open
            && self.modal_from.is_none()
            && self
                .model
                .media
                .as_ref()
                .is_some_and(|media| media.kind == MediaKind::Movie)
    }

    fn record_navigation(&mut self) {
        self.navigation_revision = self.navigation_revision.wrapping_add(1);
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
        self.model.preparing = preparing;
        cx.notify();
    }

    /// Acknowledge the explicit movie-watch intent. The UI sets this before
    /// emitting `WatchMovie`, without opening Sources or blocking navigation.
    /// Clear it on failure/cancellation, before showing fallback Sources, or
    /// when handing off to normal playback (`set_playback_active`/`set_preparing`).
    /// Discovery snapshots alone do not complete an explicit watch.
    pub fn set_movie_watch_pending(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.model.movie_watch_pending = pending;
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
        if section == Section::Sources && !matches!(state, SectionState::Loading(_)) {
            self.model.source_discovery_starting = false;
        }
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
        if self.model.section == section {
            if self.detail_open {
                self.cancel_detail_sources(cx);
            }
            // Even before the details reply, navigating to the same section
            // invalidates the pending card's permission to prefetch.
            self.record_navigation();
        } else if self.detail_open && section == Section::Sources && !self.source_work_active() {
            self.cancel_detail_sources(cx);
        }
        self.modal_from = None;
        self.change_section(section, cx);
        self.detail_open = false;
    }

    fn change_section(&mut self, section: Section, cx: &mut Context<Self>) {
        // Also invalidate completed discovery: queued provider events may still arrive.
        if let Some(event) = self.source_exit_event(section) {
            self.clear_rutracker_login(cx);
            self.model.movie_watch_pending = false;
            // Invalidate the controller's selected offer and any in-flight login.
            cx.emit(event);
        }
        self.record_section_change(section);
        cx.notify();
    }

    fn record_section_change(&mut self, section: Section) {
        if self.model.section != section {
            self.record_navigation();
            self.model.section = section;
        }
    }

    fn source_exit_event(&self, next: Section) -> Option<UiEvent> {
        if next == self.model.section {
            None
        } else if self.model.movie_watch_pending
            || (self.model.section == Section::Sources && self.source_work_active())
        {
            Some(UiEvent::Cancel(Section::Sources))
        } else if self.detail_open && next != Section::Sources {
            Some(UiEvent::CancelMoviePrefetch)
        } else {
            None
        }
    }

    fn source_work_active(&self) -> bool {
        self.model.movie_watch_pending
            || self.model.discovering()
            || self.rutracker_login.is_some()
            || matches!(
                self.model.states[Section::Sources.index()],
                SectionState::Loading(_)
            )
    }

    fn cancel_detail_sources(&mut self, cx: &mut Context<Self>) {
        if self.source_work_active() {
            self.model.movie_watch_pending = false;
            self.clear_rutracker_login(cx);
            cx.emit(UiEvent::Cancel(Section::Sources));
        } else if self.detail_open {
            cx.emit(UiEvent::CancelMoviePrefetch);
        }
    }

    fn show_overlay(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.opens_overlay(section) {
            self.modal_from = Some(self.page());
            if self.model.section == section {
                self.record_navigation();
            }
            self.change_section(section, cx);
        }
    }

    fn opens_overlay(&self, section: Section) -> bool {
        self.model.section != section || (self.detail_open && self.modal_from.is_none())
    }

    fn page(&self) -> Section {
        self.modal_from.unwrap_or(self.model.section)
    }

    /// Hand control back to explicit source selection (including watch fallback).
    /// The controller must also clear its automatic watch intent before calling.
    pub fn show_source_overlay(&mut self, cx: &mut Context<Self>) {
        self.model.movie_watch_pending = false;
        self.show_overlay(Section::Sources, cx);
        cx.notify();
    }

    fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if let Some(section) = self.modal_from.take() {
            if self.returns_to_movie_with_results() {
                // Closing the table is not leaving the movie. Keep its finished
                // lookup for Watch, but never leave a pending watch/login running.
                self.record_section_change(section);
                cx.notify();
                return;
            }
            // A card opened from Sources has the same section as its overlay.
            // Closing that overlay still leaves discovery, but not a ready choice.
            if self.model.section == Section::Sources && section == Section::Sources {
                self.cancel_detail_sources(cx);
                self.record_navigation();
            }
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

    fn returns_to_movie_with_results(&self) -> bool {
        self.detail_open
            && self.model.section == Section::Sources
            && self
                .model
                .media
                .as_ref()
                .is_some_and(|card| card.kind == MediaKind::Movie)
            && self
                .model
                .source_discovery
                .as_ref()
                .is_some_and(|result| result.pending == 0)
            && !self.model.movie_watch_pending
            && self.rutracker_login.is_none()
            && !self.model.source_discovery_starting
            && !self.model.preparing
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

    pub fn set_catalog_episode(&mut self, episode: Option<(u16, u16)>, cx: &mut Context<Self>) {
        self.model.selected_episode = episode;
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

    /// Updates display state only; never opens Sources or changes navigation.
    pub fn set_source_discovery(
        &mut self,
        discovery: Option<SourceDiscovery>,
        cx: &mut Context<Self>,
    ) {
        self.model.source_discovery_starting = false;
        self.model.source_discovery = discovery;
        cx.notify();
    }

    /// Initial discovery before the controller knows how many providers will run.
    /// Unlike preparation, this keeps manual source actions and arrived rows usable.
    pub fn begin_source_discovery(&mut self, cx: &mut Context<Self>) {
        self.model.source_discovery = None;
        self.model.source_discovery_starting = true;
        cx.notify();
    }

    pub fn set_source_preferences(
        &mut self,
        draft: SourcePreferencesDraft,
        cx: &mut Context<Self>,
    ) {
        self.source_preferences = source_preferences::SourcePreferencesEditor::new(draft, cx);
        cx.notify();
    }

    pub fn set_source_preferences_status(
        &mut self,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.source_preferences_status = message.into();
        cx.notify();
    }

    /// Show the inline Sources account prompt only after the controller needs login.
    /// Messages must be safe for display, never raw authentication errors or secrets.
    /// `None, false` dismisses it. The controller owns vault access and cancellation.
    /// Leaving Sources emits `Cancel(Sources)`; ignore stale login responses then.
    /// Replacing media, source preview or files also clears the prompt.
    /// Streaming discovery rows do not interrupt an account prompt.
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
            Command::CloseOverlay => {
                self.record_navigation();
                self.dismiss_overlay(cx);
            }
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
                        | UiEvent::WatchMovie
                        | UiEvent::FindSources
                        | UiEvent::SelectEpisode { .. }
                        | UiEvent::UnlinkMedia
                        | UiEvent::ForgetRuTracker
                        | UiEvent::Cancel(Section::Sources)
                        | UiEvent::Retry(Section::Sources)
                ) && self.rutracker_login.is_some()
                {
                    self.clear_rutracker_login(cx);
                    cx.notify();
                }
                match event {
                    UiEvent::WatchMovie | UiEvent::FindSources => {
                        self.model.movie_watch_pending = matches!(event, UiEvent::WatchMovie);
                        self.source_quality = SourceQuality::All;
                        self.source_order_seeds = false;
                        cx.notify();
                    }
                    UiEvent::Cancel(Section::Sources) => {
                        self.model.movie_watch_pending = false;
                        cx.notify();
                    }
                    UiEvent::OpenMedia(_) | UiEvent::OpenLibrary(_) => {
                        // Opening another card can keep the same section.
                        if self.model.section != Section::Sources || self.detail_open {
                            self.cancel_detail_sources(cx);
                        }
                        self.record_navigation();
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
            Command::SaveSourcePreferences => {
                match self.source_preferences.draft(cx) {
                    Ok(draft) => cx.emit(UiEvent::SaveSourcePreferences(draft)),
                    Err(message) => self.source_preferences_status = message.into(),
                }
                cx.notify();
            }
            Command::ToggleCommons => {
                self.source_preferences.toggle_commons();
                cx.notify();
            }
            Command::ToggleRuTracker => {
                self.source_preferences.toggle_rutracker();
                cx.notify();
            }
            Command::TogglePopcorn => {
                self.source_preferences.toggle_popcorn();
                cx.notify();
            }
            Command::AddTorznab => {
                self.source_preferences.add(cx);
                cx.notify();
            }
            Command::RemoveTorznab(id) => {
                self.source_preferences.remove(*id);
                cx.notify();
            }
            Command::ToggleTorznab(id) => {
                self.source_preferences.toggle(*id);
                cx.notify();
            }
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
                self.cancel_detail_sources(cx);
                self.record_navigation();
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
            .when(self.model.media.is_some(), |content| {
                content.child(self.button(
                    "refresh-sources",
                    "Обновить источники",
                    Command::Emit(UiEvent::Retry(Section::Sources)),
                    false,
                    self.model.accepts(&UiEvent::Retry(Section::Sources)),
                    cx,
                ))
            })
            .when(self.model.discovering(), |content| {
                content.child(self.source_discovery_status(cx))
            })
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
        if self.show_source_section_state()
            && self.show_state(
                Section::Sources,
                self.model.source.is_some()
                    || !self.model.sources.is_empty()
                    || !self.model.files.is_empty(),
            )
        {
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

    fn visible_source_groups(&self) -> Vec<(SourceGroup, Vec<&SourceRow>)> {
        let rows = self.visible_sources();
        SourceGroup::ALL
            .into_iter()
            .filter_map(|group| {
                let sources: Vec<_> = rows
                    .iter()
                    .copied()
                    .filter(|row| SourceGroup::of(row) == group)
                    .collect();
                (!sources.is_empty()).then_some((group, sources))
            })
            .collect()
    }

    fn show_source_section_state(&self) -> bool {
        !self.model.discovering()
            || matches!(
                self.model.states[Section::Sources.index()],
                SectionState::Error(_) | SectionState::Unavailable(_)
            )
    }

    fn source_discovery_status(&self, cx: &mut Context<Self>) -> Div {
        let discovery = self.model.source_discovery.as_ref();
        panel()
            .child(eyebrow(if self.model.movie_watch_pending {
                "Подбираем источник для просмотра…"
            } else {
                discovery.map_or("Поиск источников…", |discovery| {
                    discovery.heading(!self.model.sources.is_empty())
                })
            }))
            .when(self.model.movie_watch_pending, |card| {
                card.child(body(
                    "Дождитесь подбора источника и запуска плеера. Уход с карточки отменит запрос.",
                ))
            })
            .when_some(discovery, |card, discovery| {
                card.child(body(format!(
                    "Ожидается: {} · Успешно: {} · Ошибок: {}",
                    discovery.pending,
                    discovery.succeeded,
                    discovery.failures.len()
                )))
                .when(
                    discovery.pending == 0
                        && self.model.media.as_ref().is_some_and(|m| m.kind == MediaKind::Movie)
                        && !self.model.sources.is_empty()
                        && !self.model.sources.iter().any(|row| row.recommended),
                    |card| card.child(body("Нет подходящей раздачи с известным числом сидов больше нуля. Выберите источник вручную.")),
                )
                .children(discovery.notices.iter().map(|message| body(message.clone())))
                .children(
                    discovery
                        .failures
                        .iter()
                        .map(|message| div().text_color(rgb(WARNING)).child(body(message.clone()))),
                )
            })
            .when(
                self.model.movie_watch_pending
                    || self.model.source_discovery_starting
                    || discovery.is_some_and(|discovery| discovery.pending > 0),
                |card| {
                    card.child(self.button(
                        "cancel-discovery",
                        "Отменить поиск",
                        Command::Emit(UiEvent::Cancel(Section::Sources)),
                        false,
                        true,
                        cx,
                    ))
                },
            )
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
            .child(self.source_preference_fields(cx))
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
            self.source_preferences.ensure_inputs(cx);
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
                if !this.model.preparing && key == "k" && event.keystroke.modifiers.secondary() {
                    this.run_command(&Command::OpenSearch, cx);
                } else if key == "escape" && this.model.preparing {
                    this.run_command(&Command::Emit(UiEvent::Stop), cx);
                } else if key == "escape" && this.modal_from.is_some() {
                    this.dismiss_overlay(cx);
                } else if this.model.section == Section::Search && !this.model.preparing {
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

const COMMONS_DISCLOSURE: &str = "Встроенный Wikimedia Commons работает без аккаунта и API-ключа: только ограниченный набор \
     проверенных фильмов Blender, не универсальный каталог видео. Для поиска нужно разрешение \
     внешнего каталога, но BitTorrent не нужен.";

const POPCORN_DISCLOSURE: &str = "Popcorn API / YTS выключен по умолчанию; прежнее согласие не переносится. \
     Только кнопки «Источники» и «Смотреть» отправляют IMDb ID: \
     Popcorn (fusme.link, jfper.link, uxert.link, yrkde.link) — фильмы и нумерованные эпизоды, включая аниме с IMDb; \
     YTS (yts.lt, yts.gg) — только фильмы. Нужную серию kadr выбирает локально из ответа по IMDb ID сериала. \
     Открытие карточки не вызывает запросов к этим сервисам. Фильмы могут запрашиваться в обеих группах независимо. \
     В каждой группе резервные зеркала используются при временных сбоях, но не при 401/403/429. \
     Только YTS: один переход на https://yts.pw с тем же путём и параметрами запроса; \
     другие перенаправления запрещены. Нужны разрешения каталога и BitTorrent.";

const MOVIE_PREFETCH_DISCLOSURE: &str = "При открытии карточки фильма kadr может заранее найти раздачи и magnet-ссылки RuTracker, \
     если RuTracker включён, а внешний каталог и BitTorrent разрешены. Для сериалов такого поиска нет. \
     Фоновый поиск не запрашивает пароль и не запускает скачивание видео или воспроизведение. \
     Wikimedia Commons, Torznab и Popcorn API запрашиваются по кнопке «Источники» или «Смотреть».";

const SOURCE_DISCLOSURE: &str = "Trakt предоставляет только метаданные и ссылки на изображения, не видео. \
     При сетевом поиске приложение обращается только к включённым поставщикам: \
     Wikimedia Commons, RuTracker, настроенным Torznab API и Popcorn API. \
     Отключение внешнего каталога отключает сетевой поиск источников. \
     Отключение BitTorrent отключает поиск RuTracker, Torznab и Popcorn API, не Commons. \
     Данные раздач RuTracker/Torznab/Popcorn API и права на их контент не проверены. \
     Вход не включает BitTorrent: разрешение на обмен частями файлов задаётся отдельно в настройках.";

fn source_disclosure() -> Div {
    column()
        .child(body(COMMONS_DISCLOSURE))
        .child(body(SOURCE_DISCLOSURE))
        .child(body(MOVIE_PREFETCH_DISCLOSURE))
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

    #[test]
    fn source_disclosure_distinguishes_commons_scope_and_consent_from_torrents() {
        for text in [
            "Wikimedia Commons",
            "без аккаунта и API-ключа",
            "ограниченный набор",
            "проверенных фильмов Blender",
            "не универсальный каталог видео",
            "внешнего каталога",
            "BitTorrent не нужен",
        ] {
            assert!(COMMONS_DISCLOSURE.contains(text), "{text}");
        }
        for text in [
            "Trakt предоставляет только метаданные",
            "только к включённым поставщикам",
            "Отключение внешнего каталога отключает сетевой поиск источников",
            "Wikimedia Commons, RuTracker, настроенным Torznab API и Popcorn API",
            "Отключение BitTorrent отключает поиск RuTracker, Torznab и Popcorn API, не Commons",
            "Данные раздач RuTracker/Torznab/Popcorn API и права на их контент не проверены",
            "Вход не включает BitTorrent",
        ] {
            assert!(SOURCE_DISCLOSURE.contains(text), "{text}");
        }
        for text in [
            "При открытии карточки фильма",
            "RuTracker включён",
            "внешний каталог и BitTorrent разрешены",
            "Для сериалов такого поиска нет",
            "не запрашивает пароль",
            "не запускает скачивание видео или воспроизведение",
            "Wikimedia Commons, Torznab и Popcorn API запрашиваются по кнопке",
        ] {
            assert!(MOVIE_PREFETCH_DISCLOSURE.contains(text), "{text}");
        }
    }

    #[test]
    fn popcorn_disclosure_explains_opt_in_scope_privacy_and_permissions() {
        for text in [
            "выключен по умолчанию",
            "прежнее согласие не переносится",
            "Только кнопки «Источники» и «Смотреть» отправляют IMDb ID",
            "Popcorn (fusme.link, jfper.link, uxert.link, yrkde.link)",
            "фильмы и нумерованные эпизоды, включая аниме с IMDb",
            "YTS (yts.lt, yts.gg) — только фильмы",
            "Нужную серию kadr выбирает локально",
            "Открытие карточки не вызывает запросов к этим сервисам",
            "Фильмы могут запрашиваться в обеих группах независимо",
            "В каждой группе резервные зеркала используются при временных сбоях, но не при 401/403/429",
            "Только YTS: один переход на https://yts.pw с тем же путём и параметрами запроса",
            "другие перенаправления запрещены",
            "каталога и BitTorrent",
        ] {
            assert!(POPCORN_DISCLOSURE.contains(text), "{text}");
        }
        assert!(!POPCORN_DISCLOSURE.contains("Для сериалов запросов нет"));
    }

    fn source(index: usize, quality: Option<&str>, seeds: Option<u32>) -> SourceRow {
        SourceRow {
            index,
            recommended: false,
            possible_match: false,
            title: format!("Source {index}"),
            kind: SourceKind::Magnet,
            quality: quality.map(str::to_owned),
            language: None,
            origin: "RuTracker".into(),
            video_codec: None,
            audio_codec: None,
            peers: None,
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
    fn popcorn_rows_keep_origin_and_use_existing_manual_selection() {
        let mut view = KadrView::new();
        let mut popcorn = source(53, Some("1080p"), Some(8));
        popcorn.rutracker = false;
        popcorn.origin = "Popcorn API · YTS".into();
        view.model.sources = vec![popcorn];
        let rows = view.visible_sources();
        assert_eq!(rows[0].origin, "Popcorn API · YTS");
        assert_eq!(rows[0].index, 53);
        assert!(view.model.accepts(&UiEvent::ChooseSource(53)));
        assert!(!view.model.accepts(&UiEvent::ChooseSource(0)));
        assert!(view.model.source.is_none());
        assert!(!view.model.can_play(), "discovery must not start playback");
    }

    #[test]
    fn possible_rutracker_matches_are_separate_without_reordering_other_providers() {
        let mut view = KadrView::new();
        let mut possible = source(71, Some("1080p"), Some(100));
        possible.possible_match = true;
        let mut commons = source(42, None, None);
        commons.rutracker = false;
        commons.kind = SourceKind::Http;
        commons.origin = "Wikimedia Commons".into();
        let mut torznab = source(98, Some("1080p"), Some(20));
        torznab.rutracker = false;
        torznab.origin = "Indexer".into();
        view.model.sources = vec![
            possible,
            commons,
            source(7, Some("1080p"), Some(10)),
            torznab,
        ];
        let groups = |view: &KadrView| {
            view.visible_source_groups()
                .into_iter()
                .map(|(group, rows)| {
                    (
                        group,
                        rows.into_iter().map(|row| row.index).collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            groups(&view),
            [
                (SourceGroup::Suitable, vec![42, 7, 98]),
                (SourceGroup::Possible, vec![71]),
            ]
        );
        view.source_order_seeds = true;
        assert_eq!(
            groups(&view),
            [
                (SourceGroup::Suitable, vec![98, 7, 42]),
                (SourceGroup::Possible, vec![71]),
            ]
        );
        view.source_quality = SourceQuality::FullHd;
        assert_eq!(
            groups(&view),
            [
                (SourceGroup::Suitable, vec![98, 7]),
                (SourceGroup::Possible, vec![71]),
            ]
        );
        assert!(view.model.accepts(&UiEvent::ChooseSource(71)));
        assert!(!view.model.accepts(&UiEvent::ChooseSource(0)));
        assert!(view.model.source.is_none());
        assert!(!view.model.can_play());
        assert_eq!(
            SourceGroup::Possible.explanation(),
            Some(
                "Название похоже, год или эпизод не подтверждён. Проверьте раздачу и выберите источник вручную."
            )
        );
        assert!(SourceGroup::Suitable.explanation().is_none());
        view.source_quality = SourceQuality::UltraHd;
        assert!(groups(&view).is_empty(), "do not render empty groups");
    }

    #[test]
    fn match_confidence_is_not_inferred_from_other_providers_titles_or_names() {
        let mut row = source(7, None, None);
        row.rutracker = false;
        row.origin = "RuTracker via indexer".into();
        row.title = "Возможное совпадение".into();
        row.possible_match = false;
        assert_eq!(SourceGroup::of(&row), SourceGroup::Suitable);
        row.possible_match = true;
        assert_eq!(SourceGroup::of(&row), SourceGroup::Possible);
        row.possible_match = false;
        assert_eq!(SourceGroup::of(&row), SourceGroup::Suitable);
    }

    #[test]
    fn recommendation_is_controller_owned_and_does_not_select_or_reorder_rows() {
        let mut view = KadrView::new();
        let mut recommended = source(42, Some("720p"), None);
        recommended.recommended = true;
        view.model.sources = vec![source(7, Some("1080p"), Some(10)), recommended];
        assert_eq!(
            view.visible_sources()
                .iter()
                .map(|row| row.index)
                .collect::<Vec<_>>(),
            [7, 42]
        );
        assert!(view.visible_sources()[1].recommended);
        assert!(view.model.source.is_none());
        assert!(!view.model.can_play());
        assert!(view.model.accepts(&UiEvent::ChooseSource(7)));
        assert!(view.model.accepts(&UiEvent::ChooseSource(42)));
        view.source_quality = SourceQuality::FullHd;
        assert_eq!(view.visible_sources().len(), 1);
        assert!(!view.visible_sources()[0].recommended);
    }

    #[test]
    fn explicit_watch_can_be_cancelled_without_a_source_overlay() {
        let mut view = KadrView::new();
        view.detail_open = true;
        view.model.movie_watch_pending = true;
        assert!(view.modal_from.is_none());
        assert!(view.source_work_active(), "card dismissal must cancel too");
        for current in Section::ALL {
            view.model.section = current;
            for next in Section::ALL {
                assert_eq!(
                    view.source_exit_event(next),
                    (current != next).then_some(UiEvent::Cancel(Section::Sources))
                );
            }
        }
        view.model.movie_watch_pending = false;
        view.model.source = Some(SourcePreview {
            kind: SourceKind::Http,
            title: "Chosen source".into(),
        });
        view.model.playback_available = true;
        assert!(!view.source_work_active());
        assert!(view.model.can_play());
        assert_eq!(
            view.source_exit_event(Section::Home),
            Some(UiEvent::CancelMoviePrefetch),
            "idle card exit cancels warming without discarding the source"
        );
    }

    #[test]
    fn idle_card_exit_cancels_only_prefetch_and_sources_overlay_keeps_it() {
        let mut view = KadrView::new();
        view.detail_open = true;
        for current in Section::ALL {
            view.model.section = current;
            for next in Section::ALL {
                assert_eq!(
                    view.source_exit_event(next),
                    (current != next && next != Section::Sources)
                        .then_some(UiEvent::CancelMoviePrefetch)
                );
            }
        }
        assert!(!view.model.movie_watch_pending);
        assert!(!view.model.discovering());
        assert!(!view.model.playback_active);
        assert!(!view.source_work_active());
        view.detail_open = false;
        view.model.section = Section::Home;
        assert_eq!(view.source_exit_event(Section::Settings), None);
    }

    #[test]
    fn prefetch_requires_visible_movie_card_not_overlay_or_series() {
        let mut view = KadrView::new();
        view.model.media = Some(MediaCard {
            id: "movie".into(),
            title: "Film".into(),
            original_title: None,
            year: None,
            description: None,
            poster_url: None,
            backdrop_url: None,
            rating_tenths: None,
            kind: MediaKind::Movie,
            episodes: vec![],
            attribution: String::new(),
        });
        assert!(!view.movie_card_open());
        view.detail_open = true;
        assert!(view.movie_card_open());
        view.modal_from = Some(Section::Home);
        assert!(!view.movie_card_open());
        view.model.section = Section::Sources;
        view.model.source_discovery = Some(SourceDiscovery::default());
        assert!(
            view.returns_to_movie_with_results(),
            "dismissing settled Sources returns to the same movie and keeps prepared links"
        );
        view.model.source_discovery.as_mut().unwrap().pending = 1;
        assert!(!view.returns_to_movie_with_results());
        view.model.source_discovery.as_mut().unwrap().pending = 0;
        view.model.movie_watch_pending = true;
        assert!(!view.returns_to_movie_with_results());
        view.model.movie_watch_pending = false;
        view.modal_from = None;
        view.model.media.as_mut().unwrap().kind = MediaKind::Series;
        assert!(!view.movie_card_open());
        view.model.media = None;
        assert!(!view.movie_card_open());
    }

    #[test]
    fn source_fallback_opens_over_a_card_even_when_the_card_came_from_sources() {
        let mut view = KadrView::new();
        view.model.section = Section::Sources;
        assert!(!view.opens_overlay(Section::Sources));
        view.detail_open = true;
        assert!(view.opens_overlay(Section::Sources));
        view.modal_from = Some(Section::Sources);
        assert!(!view.opens_overlay(Section::Sources));
        view.model.source_discovery = Some(SourceDiscovery::default());
        assert!(
            view.source_work_active(),
            "same-section dismiss must cancel discovery"
        );
        view.model.source_discovery = None;
        view.model.playback_available = true;
        view.model.source = Some(SourcePreview {
            kind: SourceKind::Http,
            title: "Explicit choice".into(),
        });
        assert!(!view.source_work_active(), "ready choice survives dismiss");
        assert!(view.model.can_play());
    }

    #[test]
    fn leaving_sources_cancels_completed_empty_failed_and_login_states() {
        let mut view = KadrView::new();
        view.model.section = Section::Sources;
        view.model.source_discovery = Some(SourceDiscovery {
            pending: 0,
            succeeded: 1,
            failures: Vec::new(),
            notices: Vec::new(),
        });
        for state in [
            SectionState::Empty("Complete".into()),
            SectionState::Loading("Searching".into()),
            SectionState::Error("All failed".into()),
            SectionState::Unavailable("Disabled".into()),
        ] {
            view.model.states[Section::Sources.index()] = state;
            for pending in [false, true] {
                view.rutracker_login = Some(RuTrackerLogin {
                    message: None,
                    pending,
                });
                for next in Section::ALL {
                    assert_eq!(
                        view.source_exit_event(next),
                        (next != Section::Sources).then_some(UiEvent::Cancel(Section::Sources))
                    );
                }
                view.rutracker_login = None;
                assert_eq!(
                    view.source_exit_event(Section::Home),
                    Some(UiEvent::Cancel(Section::Sources))
                );
            }
        }
        view.model.source_discovery = None;
        view.model.states[Section::Sources.index()] = SectionState::Empty("Источник готов.".into());
        assert_eq!(
            view.source_exit_event(Section::Home),
            None,
            "do not discard a prepared manual source"
        );
        view.model.section = Section::Home;
        assert_eq!(view.source_exit_event(Section::Settings), None);
    }

    #[test]
    fn refreshing_current_media_is_available_after_success_and_empty_results() {
        let mut view = KadrView::new();
        view.model.media = Some(MediaCard {
            id: "media".into(),
            title: "Film".into(),
            original_title: None,
            year: None,
            description: None,
            poster_url: None,
            backdrop_url: None,
            rating_tenths: None,
            kind: MediaKind::Movie,
            episodes: vec![],
            attribution: String::new(),
        });
        for state in [
            SectionState::Empty("No sources".into()),
            SectionState::Empty("Complete".into()),
            SectionState::Error("All failed".into()),
        ] {
            view.model.states[Section::Sources.index()] = state;
            assert!(view.model.accepts(&UiEvent::Retry(Section::Sources)));
        }
        view.model.sources = vec![source(42, None, None)];
        assert!(view.model.accepts(&UiEvent::Retry(Section::Sources)));
        view.model.states[Section::Sources.index()] = SectionState::Loading("Searching".into());
        view.model.source_discovery_starting = true;
        assert!(view.model.accepts(&UiEvent::Retry(Section::Sources)));
        view.model.playback_active = true;
        assert!(!view.model.accepts(&UiEvent::Retry(Section::Sources)));
    }

    #[test]
    fn discovery_does_not_hide_later_source_errors_or_unavailability() {
        let mut view = KadrView::new();
        view.model.source_discovery = Some(SourceDiscovery {
            pending: 0,
            succeeded: 1,
            failures: Vec::new(),
            notices: Vec::new(),
        });
        for state in [
            SectionState::Error("Selected source failed".into()),
            SectionState::Unavailable("Sources disabled".into()),
        ] {
            view.model.states[Section::Sources.index()] = state;
            assert!(view.show_source_section_state());
        }
        view.model.states[Section::Sources.index()] = SectionState::Empty("Complete".into());
        assert!(!view.show_source_section_state());
        view.model.states[Section::Sources.index()] = SectionState::Loading("Discovery".into());
        assert!(!view.show_source_section_state());
        view.model.source_discovery = None;
        assert!(view.show_source_section_state());
        view.model.source_discovery_starting = true;
        assert!(!view.show_source_section_state());
    }

    #[test]
    fn navigation_revision_changes_for_navigation_not_data_updates() {
        let mut view = KadrView::new();
        let initial = view.navigation_revision();
        view.record_section_change(Section::Home);
        assert_eq!(view.navigation_revision(), initial);
        view.record_section_change(Section::Sources);
        let sources = view.navigation_revision();
        assert_ne!(sources, initial);
        view.model.sources = vec![source(42, None, None)];
        view.model.source_discovery = Some(SourceDiscovery::default());
        assert_eq!(view.navigation_revision(), sources);
        view.record_navigation(); // A detail/overlay navigation within the same section.
        assert_ne!(view.navigation_revision(), sources);
    }

    #[test]
    fn sidebar_breakpoints_follow_the_reference_widths() {
        assert_eq!(sidebar_width(900.), 58.);
        assert_eq!(sidebar_width(901.), 190.);
        assert_eq!(sidebar_width(1120.), 190.);
        assert_eq!(sidebar_width(1121.), 220.);
    }
}
