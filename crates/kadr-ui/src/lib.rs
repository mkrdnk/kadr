//! Controller-driven GPUI presentation layer for kadr.
//!
//! Subscribe to [`UiEvent`] before showing the view. The controller owns all
//! source clipboard/dialog access, validation, persistence, network and player work.
//! Setters and rendering perform no IO. Leaving Sources or an explicit movie
//! watch emits cancellation so the controller can discard stale responses.

mod cards;
mod catalog_page;
mod cinema;
mod detail;
mod dialogs;
mod icons;
mod input;
mod modal;
mod model;
mod navigation;
mod screens;
mod source_dialog;
mod source_preferences;

pub use catalog_page::{CatalogGenre, CatalogPage, CatalogRequest, CatalogSort};
pub use detail::{DetailRating, EpisodeProgress, PlaybackProgress};
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
    modal_focus: modal::ModalFocus,
    source_dialog: source_dialog::SourceDialogState,
    sidebar_collapsed: bool,
    catalogs: catalog_page::CatalogState,
    detail: detail::DetailState,
    remembered_episodes: std::collections::HashMap<String, (u16, u16)>,
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
    OpenSource,
    ToggleSidebar,
    CloseOverlay,
    Catalog(MediaKind, catalog_page::CatalogAction),
    SourceDialog(source_dialog::SourceDialogCommand),
    SelectDetailSeason(u16),
    ToggleDetailSeasons,
    SetDetailResolution(Option<String>),
    SetDetailLanguage(Option<String>),
    SetDetailSubtitlesOff(bool),
    WatchDetail,
    WatchDetailFromStart,
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
    ToggleAutoDiscover,
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

    pub fn detail_visible(&self) -> bool {
        self.detail_open && self.modal_from.is_none()
    }

    pub fn set_sidebar_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        self.sidebar_collapsed = collapsed;
        cx.notify();
    }

    pub fn set_catalog_years(&mut self, years: Vec<u16>, cx: &mut Context<Self>) {
        self.catalogs.set_years(years);
        cx.notify();
    }

    pub fn set_catalog_page(
        &mut self,
        request: &CatalogRequest,
        result: Result<CatalogPage, String>,
        cx: &mut Context<Self>,
    ) {
        let changed = match result {
            Ok(page) => self.catalogs.apply_page(request, page),
            Err(error) => self.catalogs.apply_failure(request, error),
        };
        if changed {
            cx.notify();
        }
    }

    pub fn set_catalog_genres_loading(&mut self, kind: MediaKind, cx: &mut Context<Self>) {
        self.catalogs.set_genres_loading(kind);
        cx.notify();
    }

    pub fn set_catalog_genres(
        &mut self,
        kind: MediaKind,
        result: Result<Vec<CatalogGenre>, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(genres) => self.catalogs.set_genres(kind, genres),
            Err(error) => self.catalogs.set_genres_error(kind, error),
        }
        cx.notify();
    }

    pub fn playback_options(&self) -> PlaybackOptions {
        PlaybackOptions {
            preferred_resolution: self.detail.preferred_resolution.clone(),
            preferred_language: self.detail.preferred_language.clone(),
            subtitles_off: self.detail.subtitles_off
                && self.model.default_player != PlayerChoice::Browser,
        }
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

    pub fn is_preparing(&self) -> bool {
        self.model.preparing
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
        if section == Section::Sources {
            self.run_command(&Command::OpenSource, cx);
            return;
        }
        if self.model.section == section {
            if self.detail_open {
                self.cancel_detail_sources(cx);
            }
            // Even before the details reply, navigating to the same section
            // invalidates the pending card's permission to prefetch.
            self.record_navigation();
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
            || (next != Section::Sources && self.source_work_active())
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
            || self.model.source_discovery_starting
            || self
                .model
                .source_discovery
                .as_ref()
                .is_some_and(|value| value.pending > 0)
            || self.rutracker_login.is_some()
            || (!self.model.discovering()
                && matches!(
                    self.model.states[Section::Sources.index()],
                    SectionState::Loading(_)
                ))
    }

    fn cancel_detail_sources(&mut self, cx: &mut Context<Self>) {
        if self.source_work_active() || self.model.discovering() {
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
        self.source_dialog.show_picker();
        self.show_overlay(Section::Sources, cx);
        cx.notify();
    }

    pub fn close_source_after_launch(&mut self, cx: &mut Context<Self>) {
        if self.model.section == Section::Sources
            && let Some(section) = self.modal_from.take()
        {
            self.record_section_change(section);
            self.clear_rutracker_login(cx);
            cx.notify();
        }
    }

    fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        if let Some(section) = self.modal_from.take() {
            if self.keep_sources_on_dismiss() {
                // Closing the table is not leaving the movie. Keep its finished
                // lookup for Watch, but never leave a pending watch/login running.
                self.record_section_change(section);
                cx.notify();
                return;
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

    fn keep_sources_on_dismiss(&self) -> bool {
        self.model.section == Section::Sources
            && !self.source_work_active()
            && !self.model.preparing
    }

    pub fn set_search_results(&mut self, rows: Vec<MediaCard>, cx: &mut Context<Self>) {
        self.model.results = rows;
        self.search_cursor = 0;
        cx.notify();
    }

    pub fn set_media_detail(&mut self, media: Option<MediaCard>, cx: &mut Context<Self>) {
        let changed = self.model.media.as_ref().map(|media| &media.id)
            != media.as_ref().map(|media| &media.id);
        if changed {
            self.clear_rutracker_login(cx);
            self.detail_open = media.is_some() && self.page() == self.detail_from;
            self.model.selected_episode = None;
            self.detail = detail::DetailState::default();
            self.source_dialog.reset();
        }
        self.model.media = media;
        self.model.media_favorite = false;
        self.refresh_detail_progress();
        cx.notify();
    }

    pub fn set_media_favorite(&mut self, favorite: bool, cx: &mut Context<Self>) {
        self.model.media_favorite = favorite;
        cx.notify();
    }

    pub fn set_catalog_episode(&mut self, episode: Option<(u16, u16)>, cx: &mut Context<Self>) {
        self.model.selected_episode = episode;
        if let Some(episode) = episode {
            self.detail.selected_season = Some(episode.0);
        }
        self.refresh_detail_progress();
        cx.notify();
    }

    /// Prefer an explicit previous selection, then measured history, then a
    /// regular episode. Specials never become the implicit default.
    pub fn preferred_episode(&self) -> Option<(u16, u16)> {
        let media = self.model.media.as_ref()?;
        let valid = |key: (u16, u16)| {
            media
                .episodes
                .iter()
                .any(|row| (row.season, row.number) == key)
        };
        let regular = || {
            media
                .episodes
                .iter()
                .filter(|row| row.season > 0)
                .map(|row| (row.season, row.number))
        };
        let next = |key| regular().filter(|candidate| *candidate > key).min();
        if let Some(key) = self
            .remembered_episodes
            .get(&media.id)
            .copied()
            .filter(|key| valid(*key))
        {
            if key.0 > 0
                && self.model.library.iter().any(|row| {
                    row.media_id == media.id && row.episode == Some(key) && row.completed
                })
            {
                return next(key).or(Some(key));
            }
            return Some(key);
        }
        let last = self
            .model
            .library
            .iter()
            .filter(|row| row.media_id == media.id && (row.can_resume() || row.completed))
            .filter(|row| row.episode.is_some_and(|key| key.0 > 0 && valid(key)))
            .max_by_key(|row| row.opened_at);
        if let Some(row) = last
            && let Some(key) = row.episode
        {
            if row.can_resume() {
                return Some(key);
            }
            return next(key).or(Some(key));
        }
        regular().min()
    }

    fn refresh_detail_progress(&mut self) {
        let Some(media) = &self.model.media else {
            return;
        };
        let progress = |row: &LibraryRow| PlaybackProgress {
            position: row.position,
            duration: row.duration,
            completed: row.completed,
        };
        self.detail.progress = self
            .model
            .library
            .iter()
            .find(|row| row.media_id == media.id && row.episode == self.model.selected_episode)
            .map(progress);
        self.detail.episode_progress = self
            .model
            .library
            .iter()
            .filter_map(|row| {
                let (season, number) = row.episode?;
                (row.media_id == media.id).then(|| EpisodeProgress {
                    season,
                    number,
                    progress: progress(row),
                })
            })
            .collect();
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
        self.source_dialog.reset();
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
        self.refresh_detail_progress();
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
            self.catalogs.reset();
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

    fn accepts(&self, event: &UiEvent) -> bool {
        match event {
            UiEvent::OpenMedia(id) => self.model.can_open_media(id, || self.catalogs.contains(id)),
            _ => self.model.accepts(event),
        }
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
                if self.applied_catalog_enabled
                    && self.model.preferences.catalog_enabled
                    && let Some(kind) = catalog_kind(*section)
                {
                    if let Some(request) = self.catalogs.ensure(kind) {
                        cx.emit(UiEvent::CatalogPage(request));
                    }
                    cx.emit(UiEvent::CatalogGenres(kind));
                }
            }
            Command::OpenSource => {
                self.run_source_dialog_command(&source_dialog::SourceDialogCommand::OpenImport, cx)
            }
            Command::ToggleSidebar => {
                cx.emit(UiEvent::SetSidebarCollapsed(!self.sidebar_collapsed))
            }
            Command::CloseOverlay if self.model.preparing => {
                self.run_command(&Command::Emit(UiEvent::Stop), cx);
            }
            Command::CloseOverlay => {
                self.record_navigation();
                self.dismiss_overlay(cx);
            }
            Command::Catalog(kind, action) => {
                if self.applied_catalog_enabled
                    && self.model.preferences.catalog_enabled
                    && let Some(request) = self.catalogs.dispatch(*kind, action.clone())
                {
                    cx.emit(UiEvent::CatalogPage(request));
                }
                cx.notify();
            }
            Command::SourceDialog(command) => self.run_source_dialog_command(command, cx),
            Command::WatchDetail | Command::WatchDetailFromStart => self.run_command(
                &Command::Emit(UiEvent::WatchDetail {
                    resume: matches!(command, Command::WatchDetail),
                    preferences: self.playback_options(),
                }),
                cx,
            ),
            Command::SetDetailResolution(value) => {
                self.detail.preferred_resolution = value.clone();
                cx.notify();
            }
            Command::SetDetailLanguage(value) => {
                self.detail.preferred_language = value.clone();
                cx.notify();
            }
            Command::SetDetailSubtitlesOff(off) => {
                self.detail.subtitles_off = *off;
                cx.notify();
            }
            Command::SelectDetailSeason(season) => {
                self.detail.season_menu = false;
                if let Some(number) = self.model.media.as_ref().and_then(|media| {
                    media
                        .episodes
                        .iter()
                        .filter(|episode| episode.season == *season)
                        .map(|episode| episode.number)
                        .min()
                }) {
                    self.detail.selected_season = Some(*season);
                    if self.model.selected_episode.map(|key| key.0) != Some(*season) {
                        self.run_command(
                            &Command::Emit(UiEvent::SelectEpisode {
                                season: *season,
                                number,
                            }),
                            cx,
                        );
                    }
                    cx.notify();
                }
            }
            Command::ToggleDetailSeasons => {
                self.detail.season_menu = !self.detail.season_menu;
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
            Command::Emit(event) if self.accepts(event) => {
                if matches!(
                    event,
                    UiEvent::ChooseSource(_)
                        | UiEvent::WatchSource(_)
                        | UiEvent::ChooseTorrentFile(_)
                        | UiEvent::WatchMovie
                        | UiEvent::WatchDetail { .. }
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
                    UiEvent::WatchMovie | UiEvent::WatchDetail { .. } | UiEvent::FindSources => {
                        self.model.movie_watch_pending = !matches!(event, UiEvent::FindSources);
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
                        self.model.movie_watch_pending = false;
                        if let Some(media) = &self.model.media {
                            self.remembered_episodes
                                .insert(media.id.clone(), (*season, *number));
                        }
                        self.set_catalog_episode(Some((*season, *number)), cx);
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
            Command::ToggleAutoDiscover => {
                self.source_preferences.toggle_auto_discover();
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
            Command::OpenSource,
            true,
            true,
            cx,
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
        self.sync_source_dialog();
        let modal_active = self.model.preparing || self.modal_from.is_some();
        self.modal_focus.update(modal_active, window, cx);
        if self.search_focus && self.model.section == Section::Search {
            if let Some(inputs) = &self.inputs {
                cx.focus_view(&inputs.search, window);
            }
            self.search_focus = false;
        } else if !modal_active
            && self.model.section != Section::Search
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
        if !modal_active
            && (self.model.section != Section::Sources || self.rutracker_login.is_none())
            && self
                .rutracker_focus
                .as_ref()
                .is_some_and(|focus| focus.contains_focused(window, cx))
        {
            window.focus(self.focus.as_ref().expect("focus initialized above"));
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width);
        let collapsed = self.sidebar_collapsed || width < 600.;
        let content_width = (width - navigation::sidebar_width(width, collapsed)).max(1.);
        let section = self.page();
        let detail = self.detail_open && self.model.media.is_some();
        let listing = (!detail).then(|| catalog_kind(section)).flatten();
        let catalog_scroll = listing.map(|kind| self.catalogs.listing(kind).scroll().clone());
        let content = if detail {
            self.detail_page(content_width, cx)
        } else {
            match section {
                Section::Home | Section::Search | Section::Sources => self.home(content_width, cx),
                Section::Movies | Section::Shows => {
                    self.render_catalog_page(section, content_width, cx)
                }
                Section::Library | Section::History => utility_page(section, self.library(cx)),
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
                if key == "tab" && (this.model.preparing || this.modal_from.is_some()) {
                    this.modal_focus
                        .cycle(event.keystroke.modifiers.shift, window, cx);
                } else if !this.model.preparing
                    && key == "o"
                    && event.keystroke.modifiers.secondary()
                    && !event.is_held
                {
                    this.run_command(&Command::OpenSource, cx);
                } else if !this.model.preparing
                    && key == "k"
                    && event.keystroke.modifiers.secondary()
                {
                    this.run_command(&Command::OpenSearch, cx);
                } else if key == "escape" && this.model.preparing {
                    this.run_command(&Command::Emit(UiEvent::Stop), cx);
                } else if key == "escape" && this.modal_from.is_some() {
                    this.run_command(&Command::CloseOverlay, cx);
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
                    .child(self.navigation(width, collapsed, cx))
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
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .when_some(catalog_scroll, |scroll, handle| {
                                        let kind =
                                            listing.expect("catalog scroll belongs to a listing");
                                        let check = Command::Catalog(
                                            kind,
                                            catalog_page::CatalogAction::CheckScroll,
                                        );
                                        let on_scroll = cx.listener(move |_, _, window, cx| {
                                            let check = check.clone();
                                            cx.defer_in(window, move |this, _, cx| {
                                                this.run_command(&check, cx);
                                            });
                                        });
                                        scroll.track_scroll(&handle).on_scroll_wheel(on_scroll)
                                    })
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

fn catalog_kind(section: Section) -> Option<MediaKind> {
    match section {
        Section::Movies => Some(MediaKind::Movie),
        Section::Shows => Some(MediaKind::Series),
        _ => None,
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
     При поиске источников передаётся IMDb ID: \
     Popcorn (fusme.link, jfper.link, uxert.link, yrkde.link) — фильмы и нумерованные эпизоды, включая аниме с IMDb; \
     YTS (yts.lt, yts.gg) — только фильмы. Нужную серию kadr выбирает локально из ответа по IMDb ID сериала. \
     При открытии карточки или выборе эпизода запросы идут только с отдельно включённым автоматическим поиском. \
     Фильмы могут запрашиваться в обеих группах независимо. \
     В каждой группе резервные зеркала используются при временных сбоях, но не при 401/403/429. \
     Только YTS: один переход на https://yts.pw с тем же путём и параметрами запроса; \
     другие перенаправления запрещены. Нужны разрешения каталога и BitTorrent.";

const MOVIE_PREFETCH_DISCLOSURE: &str = "При открытии карточки фильма kadr может заранее найти раздачи и magnet-ссылки RuTracker, \
     если RuTracker включён, а внешний каталог и BitTorrent разрешены. \
     Фоновый поиск не запрашивает пароль и не запускает скачивание видео или воспроизведение. \
     Для остальных поставщиков нужен явный запрос или отдельно разрешённый поиск при открытии.";

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
    fn cards_from_catalog_pages_can_open_without_home_shelves_or_search_results() {
        let mut view = KadrView::new();
        let movie = "trakt:movie:123";
        let show = "trakt:show:456";
        for (kind, id) in [(MediaKind::Movie, movie), (MediaKind::Series, show)] {
            let request = view.catalogs.ensure(kind).unwrap();
            assert!(view.catalogs.apply_page(
                &request,
                CatalogPage {
                    items: vec![MediaCard {
                        id: id.into(),
                        kind,
                        ..MediaCard::default()
                    }],
                    next_page: None,
                },
            ));
        }
        let open_movie = UiEvent::OpenMedia(movie.into());
        let open_show = UiEvent::OpenMedia(show.into());
        assert!(!view.model.accepts(&open_movie));
        assert!(view.accepts(&open_movie));
        assert!(view.accepts(&open_show));
        assert!(!view.accepts(&UiEvent::OpenMedia("trakt:movie:999".into())));

        view.model.playback_active = true;
        assert!(!view.accepts(&open_movie));
        assert!(!view.accepts(&open_show));

        view.model.playback_active = false;
        view.catalogs.reset();
        assert!(!view.accepts(&open_movie));
        assert!(!view.accepts(&open_show));
    }

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
            "не запрашивает пароль",
            "не запускает скачивание видео или воспроизведение",
            "Для остальных поставщиков нужен явный запрос или отдельно разрешённый поиск при открытии",
        ] {
            assert!(MOVIE_PREFETCH_DISCLOSURE.contains(text), "{text}");
        }
    }

    #[test]
    fn popcorn_disclosure_explains_opt_in_scope_privacy_and_permissions() {
        for text in [
            "выключен по умолчанию",
            "прежнее согласие не переносится",
            "При поиске источников передаётся IMDb ID",
            "Popcorn (fusme.link, jfper.link, uxert.link, yrkde.link)",
            "фильмы и нумерованные эпизоды, включая аниме с IMDb",
            "YTS (yts.lt, yts.gg) — только фильмы",
            "Нужную серию kadr выбирает локально",
            "При открытии карточки или выборе эпизода запросы идут только с отдельно включённым автоматическим поиском",
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
            languages: Vec::new(),
            resolution: None,
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
            runtime_minutes: None,
            certification: None,
            genres: Vec::new(),
            external_ratings: Vec::new(),
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
            view.keep_sources_on_dismiss(),
            "dismissing settled Sources returns to the same movie and keeps prepared links"
        );
        view.model.source_discovery.as_mut().unwrap().pending = 1;
        assert!(!view.keep_sources_on_dismiss());
        view.model.source_discovery.as_mut().unwrap().pending = 0;
        view.model.movie_watch_pending = true;
        assert!(!view.keep_sources_on_dismiss());
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
        view.model.source_discovery = Some(SourceDiscovery {
            pending: 1,
            ..SourceDiscovery::default()
        });
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
    fn leaving_sources_cancels_pending_search_or_login_but_keeps_completed_lookup() {
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
                assert_eq!(view.source_exit_event(Section::Home), None);
            }
        }
        view.model.source_discovery.as_mut().unwrap().pending = 1;
        assert_eq!(
            view.source_exit_event(Section::Home),
            Some(UiEvent::Cancel(Section::Sources))
        );
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
    fn closing_import_cancels_unfinished_metadata_but_keeps_a_ready_source() {
        let mut view = KadrView::new();
        view.model.section = Section::Sources;
        view.model.states[Section::Sources.index()] = SectionState::Loading("Checking".into());
        assert!(!view.keep_sources_on_dismiss());
        assert_eq!(
            view.source_exit_event(Section::Home),
            Some(UiEvent::Cancel(Section::Sources))
        );

        view.model.states[Section::Sources.index()] = SectionState::Empty("Ready".into());
        assert!(view.keep_sources_on_dismiss());
        assert_eq!(view.source_exit_event(Section::Home), None);
        view.model.preparing = true;
        assert!(!view.keep_sources_on_dismiss());
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
            runtime_minutes: None,
            certification: None,
            genres: Vec::new(),
            external_ratings: Vec::new(),
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
    fn episode_defaults_use_measured_history_and_preserve_explicit_specials() {
        let mut view = KadrView::new();
        view.model.media = Some(MediaCard {
            id: "series".into(),
            kind: MediaKind::Series,
            episodes: [(0, 1), (1, 1), (1, 2), (2, 1)]
                .into_iter()
                .map(|(season, number)| EpisodeRow {
                    season,
                    number,
                    ..EpisodeRow::default()
                })
                .collect(),
            ..MediaCard::default()
        });
        assert_eq!(view.preferred_episode(), Some((1, 1)));
        view.model.library = vec![LibraryRow {
            media_id: "series".into(),
            episode: Some((1, 2)),
            position: Some(42),
            ..LibraryRow::default()
        }];
        assert_eq!(view.preferred_episode(), Some((1, 2)));
        view.model.library[0].completed = true;
        assert_eq!(view.preferred_episode(), Some((2, 1)));
        view.remembered_episodes.insert("series".into(), (0, 1));
        assert_eq!(view.preferred_episode(), Some((0, 1)));
        view.remembered_episodes.insert("series".into(), (99, 1));
        assert_eq!(view.preferred_episode(), Some((2, 1)));
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
}
