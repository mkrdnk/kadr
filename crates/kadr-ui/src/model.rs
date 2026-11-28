//! Display-only state. Playback URLs and credentials belong to the controller.
//! Metadata, public image URLs and user-configured cache paths are display data.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Home,
    Movies,
    Shows,
    Search,
    Library,
    History,
    Sources,
    Transfers,
    Settings,
}

impl Section {
    pub const ALL: [Self; 9] = [
        Self::Home,
        Self::Movies,
        Self::Shows,
        Self::Search,
        Self::Library,
        Self::History,
        Self::Sources,
        Self::Transfers,
        Self::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Главная",
            Self::Movies => "Фильмы",
            Self::Shows => "Сериалы",
            Self::Search => "Поиск",
            Self::Library => "Моя библиотека",
            Self::History => "История",
            Self::Sources => "Источники",
            Self::Transfers => "Загрузки",
            Self::Settings => "Настройки",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Home => "nav-home",
            Self::Movies => "nav-movies",
            Self::Shows => "nav-shows",
            Self::Search => "nav-search",
            Self::Library => "nav-library",
            Self::History => "nav-history",
            Self::Sources => "nav-sources",
            Self::Transfers => "nav-transfers",
            Self::Settings => "nav-settings",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayerChoice {
    #[default]
    Mpv,
    Vlc,
    Browser,
}

impl PlayerChoice {
    pub const ALL: [Self; 3] = [Self::Mpv, Self::Vlc, Self::Browser];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mpv => "mpv",
            Self::Vlc => "VLC",
            Self::Browser => "Браузер",
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Mpv => "player-mpv",
            Self::Vlc => "player-vlc",
            Self::Browser => "player-browser",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceKind {
    #[default]
    Http,
    Magnet,
    TorrentFile,
    LocalFile,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP(S)",
            Self::Magnet => "Magnet",
            Self::TorrentFile => ".torrent",
            Self::LocalFile => "Локальное видео",
        }
    }
}

/// Only a controller-sanitized display name, never a URL, token or full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePreview {
    pub kind: SourceKind,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

/// Human-readable, privacy-safe feedback supplied by the controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiStatus {
    pub level: StatusLevel,
    pub message: String,
}

impl UiStatus {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Info,
            message: message.into(),
        }
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Warning,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: StatusLevel::Error,
            message: message.into(),
        }
    }
}

/// States are explicit; an unavailable integration is not an empty result.
/// Messages must be safe for display, not raw errors from a URL/file API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionState {
    Unavailable(String),
    Loading(String),
    Empty(String),
    Error(String),
}

impl SectionState {
    pub(crate) fn heading(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "Пока недоступно",
            Self::Loading(_) => "Выполняется…",
            Self::Empty(_) => "Здесь пока пусто",
            Self::Error(_) => "Не удалось выполнить действие",
        }
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Unavailable(message)
            | Self::Loading(message)
            | Self::Empty(message)
            | Self::Error(message) => message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEvent {
    PasteSource,
    OpenTorrent,
    ImportSource {
        torrent: bool,
        attach: bool,
    },
    Play,
    PlayFromStart,
    Stop,
    SetDefaultPlayer(PlayerChoice),
    SetSidebarCollapsed(bool),
    Retry(Section),
    Cancel(Section),
    /// Stop quiet card warming without discarding an explicit source choice.
    CancelMoviePrefetch,
    Search(String),
    Browse,
    CatalogPage(crate::CatalogRequest),
    CatalogGenres(MediaKind),
    OpenMedia(String),
    /// Watch the current movie. The controller plays a ready explicit/saved
    /// choice normally, or resolves a recommendation for automatic playback.
    /// Unlike `FindSources`, this is a playback intent, not just discovery.
    WatchMovie,
    WatchDetail {
        resume: bool,
        preferences: PlaybackOptions,
    },
    FindSources,
    DiscoverSources,
    SelectEpisode {
        season: u16,
        number: u16,
    },
    ChooseSource(usize),
    WatchSource(usize),
    ToggleFavorite,
    SetFavorite {
        id: String,
        favorite: bool,
    },
    OpenLibrary(String),
    ChooseTorrentFile(usize),
    AssignEpisode {
        season: u16,
        number: u16,
    },
    Save,
    OpenIn(PlayerChoice),
    SavePreferences(PreferencesDraft),
    SaveSourcePreferences(SourcePreferencesDraft),
    ChooseCacheDirectory,
    ClearCache,
    ClearHistory,
    UnlinkMedia,
    /// The controller remembers credentials only in the system vault, never config.
    SignInRuTracker(RuTrackerCredentials),
    ForgetRuTracker,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackOptions {
    pub preferred_resolution: Option<String>,
    pub preferred_language: Option<String>,
    pub subtitles_off: bool,
}

/// Transient sign-in input; neither field is available in debug formatting.
#[derive(Clone, PartialEq, Eq)]
pub struct RuTrackerCredentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for RuTrackerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuTrackerCredentials([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaKind {
    #[default]
    Movie,
    Series,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EpisodeRow {
    pub season: u16,
    pub number: u16,
    pub title: String,
    pub runtime_minutes: Option<u32>,
    pub screenshot_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaCard {
    pub id: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub description: Option<String>,
    /// Public metadata URL only, never a playback or authenticated URL.
    pub poster_url: Option<String>,
    /// Public wide metadata image supplied by the controller; falls back to the poster.
    pub backdrop_url: Option<String>,
    /// Provider rating on a ten-point scale, in tenths (87 represents 8.7).
    pub rating_tenths: Option<u8>,
    pub external_ratings: Vec<crate::DetailRating>,
    pub runtime_minutes: Option<u32>,
    pub genres: Vec<String>,
    pub certification: Option<String>,
    pub kind: MediaKind,
    pub episodes: Vec<EpisodeRow>,
    pub attribution: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceRow {
    pub index: usize,
    /// Resolver recommendation only; never implies selection or starts playback.
    pub recommended: bool,
    /// Plausible release for explicit selection only.
    pub possible_match: bool,
    pub title: String,
    pub kind: SourceKind,
    pub quality: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub language: Option<String>,
    /// Search metadata, not verified against the video file.
    pub resolution: Option<u16>,
    pub languages: Vec<String>,
    pub origin: String,
    pub license: Option<String>,
    /// Controller-provided size in bytes; absence is not a zero-size download.
    pub size: Option<u64>,
    pub seeds: Option<u32>,
    pub peers: Option<u32>,
    /// Tracker metadata is unverified, including quality and rights claims.
    pub rutracker: bool,
}

/// Controller-sanitized provider feedback, never raw URLs or credentials.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceDiscovery {
    pub pending: usize,
    pub succeeded: usize,
    pub failures: Vec<String>,
    /// Non-error provider statuses, independent of failures and row count.
    pub notices: Vec<String>,
}

impl SourceDiscovery {
    pub(crate) fn heading(&self, has_rows: bool) -> &'static str {
        match (
            self.pending > 0,
            has_rows,
            self.succeeded > 0,
            self.failures.is_empty(),
        ) {
            (true, false, _, _) => "Поиск источников…",
            (true, true, _, _) => "Источники найдены · поиск продолжается",
            (false, _, false, false) => "Все провайдеры завершились с ошибкой",
            (false, true, _, false) => "Поиск завершён частично",
            (false, true, _, true) => "Поиск завершён",
            (false, false, _, _) => "Источники не найдены",
        }
    }
}

/// Configuration input only. API key values must never enter this draft.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TorznabDraft {
    pub name: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub enabled: bool,
}

impl std::fmt::Debug for TorznabDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorznabDraft")
            .field("name", &"<redacted>")
            .field("base_url", &"<redacted>")
            .field(
                "api_key_env",
                &self.api_key_env.as_ref().map(|_| "<redacted>"),
            )
            .field("enabled", &self.enabled)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePreferencesDraft {
    pub commons_enabled: bool,
    pub rutracker_enabled: bool,
    pub popcorn_enabled: bool,
    pub auto_discover: bool,
    pub torznab: Vec<TorznabDraft>,
    pub preferred_resolution: Option<String>,
    pub preferred_language: Option<String>,
}

impl Default for SourcePreferencesDraft {
    fn default() -> Self {
        Self {
            commons_enabled: true,
            rutracker_enabled: true,
            popcorn_enabled: false,
            auto_discover: false,
            torznab: Vec::new(),
            preferred_resolution: None,
            preferred_language: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TorrentFileRow {
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub season: Option<u16>,
    pub episode: Option<u16>,
    pub ambiguous: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryRow {
    pub id: String,
    pub media_id: String,
    pub episode: Option<(u16, u16)>,
    pub title: String,
    pub poster_url: Option<String>,
    pub details: String,
    pub favorite: bool,
    /// Actual successful source open, independently of player telemetry.
    pub opened_at: Option<i64>,
    pub completed: bool,
    /// Measured playback position and duration in seconds, not byte progress.
    pub position: Option<u64>,
    pub duration: Option<u64>,
}

impl LibraryRow {
    pub fn has_history(&self) -> bool {
        self.opened_at.is_some()
    }

    pub fn can_resume(&self) -> bool {
        !self.completed
            && self.position.is_some_and(|position| {
                position > 0 && self.duration.is_none_or(|duration| position < duration)
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferView {
    pub stage: String,
    pub download_bps: Option<u64>,
    pub upload_bps: Option<u64>,
    pub peers: Option<usize>,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub buffered: Option<u64>,
    pub saving: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimit {
    Paused,
    Unlimited,
    Bps(u64),
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreferencesDraft {
    pub cache_limit_bytes: u64,
    pub download_limit: RateLimit,
    pub upload_limit: RateLimit,
    pub peer_limit: u32,
    pub torrents_enabled: bool,
    pub catalog_enabled: bool,
    pub catalog_proxy: Option<String>,
    pub cache_directory: String,
}

impl std::fmt::Debug for PreferencesDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreferencesDraft")
            .field("cache_limit_bytes", &self.cache_limit_bytes)
            .field("download_limit", &self.download_limit)
            .field("upload_limit", &self.upload_limit)
            .field("peer_limit", &self.peer_limit)
            .field("torrents_enabled", &self.torrents_enabled)
            .field("catalog_enabled", &self.catalog_enabled)
            .field(
                "catalog_proxy",
                &self.catalog_proxy.as_ref().map(|_| "<redacted>"),
            )
            .field("cache_directory", &self.cache_directory)
            .finish()
    }
}

impl Default for PreferencesDraft {
    fn default() -> Self {
        Self {
            cache_limit_bytes: 10 * 1024 * 1024 * 1024,
            download_limit: RateLimit::Unlimited,
            upload_limit: RateLimit::Unlimited,
            peer_limit: 50,
            torrents_enabled: true,
            catalog_enabled: true,
            catalog_proxy: None,
            cache_directory: String::new(),
        }
    }
}

pub(crate) struct ShellModel {
    pub section: Section,
    pub states: [SectionState; Section::ALL.len()],
    pub default_player: PlayerChoice,
    pub source: Option<SourcePreview>,
    pub playback_available: bool,
    pub playback_active: bool,
    pub preparing: bool,
    pub status: UiStatus,
    pub preferences_status: String,
    pub results: Vec<MediaCard>,
    pub movies: Vec<MediaCard>,
    pub shows: Vec<MediaCard>,
    pub browse_state: SectionState,
    pub media: Option<MediaCard>,
    pub sources: Vec<SourceRow>,
    pub source_discovery: Option<SourceDiscovery>,
    pub source_discovery_starting: bool,
    pub movie_watch_pending: bool,
    pub files: Vec<TorrentFileRow>,
    pub library: Vec<LibraryRow>,
    pub transfer: Option<TransferView>,
    pub preferences: PreferencesDraft,
    pub selected_episode: Option<(u16, u16)>,
    pub selected_file: Option<usize>,
    pub media_favorite: bool,
}

impl Default for ShellModel {
    fn default() -> Self {
        Self {
            section: Section::Home,
            states: Section::ALL.map(|section| {
                SectionState::Empty(
                    match section {
                        Section::Search => "Введите название фильма или сериала.",
                        Section::Library => "Нет избранного и истории просмотра.",
                        Section::History => "История пока пуста.",
                        Section::Sources => {
                            "Вставьте HTTP(S)- или magnet-ссылку либо откройте файл .torrent."
                        }
                        Section::Transfers => "Нет активной сессии.",
                        Section::Settings => "Настройки ещё не загружены.",
                        _ => "Каталог ещё не загружен.",
                    }
                    .into(),
                )
            }),
            default_player: PlayerChoice::Mpv,
            source: None,
            playback_available: false,
            playback_active: false,
            preparing: false,
            status: UiStatus::info("Откройте источник или найдите фильм в каталоге."),
            preferences_status: "Изменения применяются после сохранения.".into(),
            results: Vec::new(),
            movies: Vec::new(),
            shows: Vec::new(),
            browse_state: SectionState::Empty("Каталог ещё не загружен.".into()),
            media: None,
            sources: Vec::new(),
            source_discovery: None,
            source_discovery_starting: false,
            movie_watch_pending: false,
            files: Vec::new(),
            library: Vec::new(),
            transfer: None,
            preferences: PreferencesDraft::default(),
            selected_episode: None,
            selected_file: None,
            media_favorite: false,
        }
    }
}

impl ShellModel {
    pub fn can_play(&self) -> bool {
        self.playback_available
            && self.source.is_some()
            && !self.movie_watch_pending
            && !self.playback_active
            && !self.preparing
            && (self.discovering()
                || !matches!(
                    self.states[Section::Sources.index()],
                    SectionState::Loading(_)
                ))
    }

    pub(crate) fn discovering(&self) -> bool {
        self.source_discovery_starting || self.source_discovery.is_some()
    }

    pub(super) fn can_open_media(&self, id: &str, in_catalog: impl FnOnce() -> bool) -> bool {
        !self.playback_active
            && (self
                .results
                .iter()
                .chain(&self.movies)
                .chain(&self.shows)
                .chain(&self.media)
                .any(|media| media.id == id)
                || in_catalog())
    }

    /// Check again when handling a click/key event, not only when rendering.
    pub fn accepts(&self, event: &UiEvent) -> bool {
        match event {
            UiEvent::Play | UiEvent::PlayFromStart => self.can_play(),
            UiEvent::Stop => self.playback_active,
            UiEvent::PasteSource | UiEvent::OpenTorrent | UiEvent::ImportSource { .. } => {
                !self.playback_active
                    && !self.preparing
                    && !self.movie_watch_pending
                    && (self.discovering()
                        || !matches!(
                            self.states[Section::Sources.index()],
                            SectionState::Loading(_)
                        ))
            }
            UiEvent::Retry(Section::Sources) if self.media.is_some() => {
                self.accepts(&UiEvent::PasteSource)
            }
            UiEvent::Retry(section) => {
                matches!(self.states[section.index()], SectionState::Error(_))
            }
            UiEvent::Cancel(section) => {
                *section == Section::Sources
                    || matches!(self.states[section.index()], SectionState::Loading(_))
            }
            UiEvent::ChooseSource(index) | UiEvent::WatchSource(index) => {
                self.sources.iter().any(|row| row.index == *index)
                    && self.accepts(&UiEvent::PasteSource)
            }
            UiEvent::ChooseTorrentFile(index) => {
                self.files.iter().any(|row| row.index == *index)
                    && !self.playback_active
                    && !self.movie_watch_pending
            }
            UiEvent::AssignEpisode { .. } => {
                self.selected_file.is_some() && !self.movie_watch_pending
            }
            UiEvent::WatchMovie => {
                self.media
                    .as_ref()
                    .is_some_and(|media| media.kind == MediaKind::Movie)
                    && self.accepts(&UiEvent::PasteSource)
            }
            UiEvent::WatchDetail { .. } => self.accepts(&UiEvent::FindSources),
            UiEvent::FindSources | UiEvent::DiscoverSources => {
                self.media.as_ref().is_some_and(|media| {
                    media.kind == MediaKind::Movie || self.selected_episode.is_some()
                }) && self.accepts(&UiEvent::PasteSource)
            }
            UiEvent::ToggleFavorite => self.media.is_some(),
            UiEvent::SelectEpisode { season, number } => {
                !self.playback_active
                    && self.media.as_ref().is_some_and(|media| {
                        media
                            .episodes
                            .iter()
                            .any(|episode| episode.season == *season && episode.number == *number)
                    })
            }
            UiEvent::OpenMedia(id) => self.can_open_media(id, || false),
            UiEvent::Browse => !matches!(self.browse_state, SectionState::Loading(_)),
            UiEvent::OpenLibrary(id) => {
                !self.playback_active && self.library.iter().any(|row| row.id == *id)
            }
            UiEvent::SetFavorite { id, .. } => self.library.iter().any(|row| row.id == *id),
            UiEvent::UnlinkMedia => self.media.is_some() && !self.movie_watch_pending,
            UiEvent::SetDefaultPlayer(_) | UiEvent::ForgetRuTracker => !self.movie_watch_pending,
            UiEvent::Save | UiEvent::OpenIn(_) => self.source.is_some() && self.playback_active,
            UiEvent::Search(query) => !query.trim().is_empty(),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(kind: MediaKind) -> MediaCard {
        MediaCard {
            id: "media".into(),
            title: "Title".into(),
            original_title: None,
            year: None,
            description: None,
            poster_url: None,
            backdrop_url: None,
            rating_tenths: None,
            external_ratings: vec![],
            runtime_minutes: None,
            genres: vec![],
            certification: None,
            kind,
            episodes: vec![EpisodeRow {
                season: 1,
                number: 2,
                title: "Episode".into(),
                runtime_minutes: None,
                screenshot_url: None,
            }],
            attribution: String::new(),
        }
    }

    #[test]
    fn detail_watch_intent_allows_resolution_but_rejects_duplicate_playback() {
        let mut model = ShellModel::default();
        let watch = UiEvent::WatchDetail {
            resume: true,
            preferences: PlaybackOptions::default(),
        };
        assert!(!model.accepts(&UiEvent::WatchMovie));
        assert!(!model.accepts(&watch));
        model.media = Some(media(MediaKind::Movie));
        assert!(model.accepts(&watch));
        assert!(model.accepts(&UiEvent::WatchMovie));
        model.source = Some(SourcePreview {
            kind: SourceKind::Http,
            title: "Manual HTTP source".into(),
        });
        assert!(model.accepts(&watch));
        model.playback_available = true;
        assert!(model.accepts(&watch));
        assert!(model.accepts(&UiEvent::Play));
        assert!(model.accepts(&UiEvent::WatchMovie));
        for (active, preparing) in [(true, false), (false, true)] {
            model.playback_active = active;
            model.preparing = preparing;
            assert!(!model.accepts(&watch));
            assert!(!model.accepts(&UiEvent::Play));
            assert!(!model.accepts(&UiEvent::WatchMovie));
            assert!(!model.accepts(&UiEvent::FindSources));
        }
    }

    #[test]
    fn series_watch_requires_an_episode_even_with_a_prepared_source() {
        let mut model = ShellModel {
            media: Some(media(MediaKind::Series)),
            ..ShellModel::default()
        };
        let watch = UiEvent::WatchDetail {
            resume: false,
            preferences: PlaybackOptions::default(),
        };
        assert!(!model.accepts(&watch));
        assert!(!model.accepts(&UiEvent::WatchMovie));
        assert!(!model.accepts(&UiEvent::FindSources));
        assert!(model.accepts(&UiEvent::SelectEpisode {
            season: 1,
            number: 2
        }));
        model.source = Some(SourcePreview {
            kind: SourceKind::LocalFile,
            title: "Episode".into(),
        });
        model.playback_available = true;
        assert!(!model.accepts(&watch));
        model.selected_episode = Some((1, 2));
        assert!(model.accepts(&watch));
        assert!(model.accepts(&UiEvent::FindSources));
        assert!(model.can_play());
    }

    #[test]
    fn explicit_watch_cannot_be_superseded_even_by_nonblocking_discovery() {
        let mut model = ShellModel {
            media: Some(media(MediaKind::Movie)),
            movie_watch_pending: true,
            source_discovery_starting: true,
            selected_file: Some(4),
            files: vec![TorrentFileRow {
                index: 4,
                name: "Movie".into(),
                size: 42,
                season: None,
                episode: None,
                ambiguous: false,
            }],
            ..ShellModel::default()
        };
        model.states[Section::Sources.index()] = SectionState::Loading("Discovery".into());
        for event in [
            UiEvent::WatchMovie,
            UiEvent::FindSources,
            UiEvent::Retry(Section::Sources),
            UiEvent::PasteSource,
            UiEvent::OpenTorrent,
            UiEvent::ChooseTorrentFile(4),
            UiEvent::AssignEpisode {
                season: 1,
                number: 2,
            },
            UiEvent::UnlinkMedia,
            UiEvent::SetDefaultPlayer(PlayerChoice::Vlc),
        ] {
            assert!(!model.accepts(&event), "{event:?}");
        }
        assert!(model.accepts(&UiEvent::Cancel(Section::Sources)));
        assert!(model.accepts(&UiEvent::OpenMedia("media".into())));
        assert!(model.accepts(&UiEvent::SelectEpisode {
            season: 1,
            number: 2,
        }));
        assert!(model.accepts(&UiEvent::ToggleFavorite));
        model.movie_watch_pending = false; // Fallback hands control to Sources.
        assert!(model.accepts(&UiEvent::FindSources));
        assert!(model.accepts(&UiEvent::PasteSource));
    }

    #[test]
    fn rutracker_login_event_debug_redacts_both_fields() {
        let event = UiEvent::SignInRuTracker(RuTrackerCredentials {
            username: "example-account".into(),
            password: "example-credential".into(),
        });
        assert_eq!(
            format!("{event:?}"),
            "SignInRuTracker(RuTrackerCredentials([redacted]))"
        );
    }

    #[test]
    fn browser_history_does_not_require_or_invent_resume_position() {
        let mut row = LibraryRow {
            id: "local:item".into(),
            media_id: "local:item".into(),
            episode: None,
            title: "Own video".into(),
            poster_url: None,
            details: String::new(),
            favorite: false,
            opened_at: Some(100),
            completed: false,
            position: None,
            duration: None,
        };
        assert!(row.has_history());
        assert!(!row.can_resume());
        row.opened_at = None;
        assert!(!row.has_history());
        row.position = Some(12);
        row.duration = Some(30);
        assert!(row.can_resume());
        row.completed = true;
        assert!(!row.can_resume());
        assert!(
            !row.has_history(),
            "a measurement is not an invented open timestamp"
        );
    }

    #[test]
    fn starts_without_fabricated_results_or_playback() {
        let model = ShellModel::default();
        assert_eq!(model.section, Section::Home);
        assert!(model.movies.is_empty());
        assert!(model.shows.is_empty());
        assert_eq!(model.default_player, PlayerChoice::Mpv);
        assert_eq!(model.status.level, StatusLevel::Info);
        assert!(!model.preferences_status.is_empty());
        assert!(model.source.is_none());
        assert!(!model.can_play());
        assert!(!model.accepts(&UiEvent::Play));
        assert!(!model.accepts(&UiEvent::PlayFromStart));
        assert!(!model.accepts(&UiEvent::Stop));
        assert!(matches!(
            model.states[Section::Search.index()],
            SectionState::Empty(_)
        ));
        assert!(matches!(
            model.states[Section::Library.index()],
            SectionState::Empty(_)
        ));
    }

    #[test]
    fn playback_requires_source_and_capability_and_no_active_launch() {
        let mut model = ShellModel {
            source: Some(SourcePreview {
                kind: SourceKind::Http,
                title: "HTTP-источник".into(),
            }),
            ..ShellModel::default()
        };
        assert!(!model.can_play());
        assert!(!model.accepts(&UiEvent::PlayFromStart));
        model.playback_available = true;
        assert!(model.accepts(&UiEvent::Play));
        assert!(model.accepts(&UiEvent::PlayFromStart));
        model.states[Section::Sources.index()] = SectionState::Loading("Проверка".into());
        assert!(!model.accepts(&UiEvent::Play));
        assert!(!model.accepts(&UiEvent::PlayFromStart));
        model.states[Section::Sources.index()] = SectionState::Empty("Готово".into());
        model.playback_active = true;
        assert!(!model.accepts(&UiEvent::Play));
        assert!(!model.accepts(&UiEvent::PlayFromStart));
        assert!(model.accepts(&UiEvent::Stop));
        assert!(!model.accepts(&UiEvent::PasteSource));
        assert!(!model.accepts(&UiEvent::OpenTorrent));
        model.playback_active = false;
        model.source = None;
        assert!(!model.can_play());
    }

    #[test]
    fn retry_and_cancel_only_apply_to_corresponding_states_except_source_cancellation() {
        let mut model = ShellModel::default();
        let section = Section::Sources;
        assert!(!model.accepts(&UiEvent::Retry(section)));
        assert!(model.accepts(&UiEvent::Cancel(section)));
        model.states[section.index()] = SectionState::Loading("Проверка источника".into());
        assert!(model.accepts(&UiEvent::Cancel(section)));
        assert!(!model.accepts(&UiEvent::Retry(section)));
        assert!(!model.accepts(&UiEvent::PasteSource));
        model.states[section.index()] = SectionState::Error("Источник недоступен".into());
        assert!(model.accepts(&UiEvent::Retry(section)));
        assert!(model.accepts(&UiEvent::Cancel(section)));
        assert!(model.accepts(&UiEvent::PasteSource));
        assert!(!model.accepts(&UiEvent::Cancel(Section::Search)));
    }

    #[test]
    fn discovery_is_nonblocking_but_preparation_and_playback_still_block_sources() {
        let mut model = ShellModel {
            playback_available: true,
            source: Some(SourcePreview {
                kind: SourceKind::Http,
                title: "Video".into(),
            }),
            sources: vec![SourceRow {
                index: 71,
                recommended: false,
                possible_match: true,
                title: "Source".into(),
                kind: SourceKind::Magnet,
                quality: None,
                language: None,
                resolution: None,
                languages: vec![],
                origin: "Indexer".into(),
                video_codec: None,
                audio_codec: None,
                peers: None,
                license: None,
                size: None,
                seeds: None,
                rutracker: true,
            }],
            ..ShellModel::default()
        };
        model.states[Section::Sources.index()] = SectionState::Loading("Discovery".into());
        model.source_discovery_starting = true;
        for snapshot in [
            None, // Provider count not known yet.
            Some(SourceDiscovery {
                pending: 2,
                succeeded: 0,
                failures: vec![],
                notices: vec![],
            }),
            Some(SourceDiscovery {
                pending: 1,
                succeeded: 1,
                failures: vec![],
                notices: vec!["RuTracker: найдено возможное совпадение".into()],
            }),
            Some(SourceDiscovery {
                pending: 0,
                succeeded: 1,
                failures: vec!["Indexer unavailable".into()],
                notices: vec!["RuTracker: найдено возможное совпадение".into()],
            }),
        ] {
            model.source_discovery_starting = snapshot.is_none();
            model.source_discovery = snapshot;
            assert!(model.accepts(&UiEvent::PasteSource));
            assert!(model.accepts(&UiEvent::OpenTorrent));
            assert!(model.accepts(&UiEvent::ChooseSource(71)));
            assert!(!model.accepts(&UiEvent::ChooseSource(0)));
            assert!(model.can_play());
        }
        model.source_discovery_starting = false;
        model.source_discovery = None; // Blocking preparation.
        assert!(!model.accepts(&UiEvent::PasteSource));
        assert!(!model.accepts(&UiEvent::OpenTorrent));
        assert!(!model.accepts(&UiEvent::ChooseSource(71)));
        assert!(!model.can_play());
        model.source_discovery = Some(SourceDiscovery::default());
        model.playback_active = true;
        assert!(!model.accepts(&UiEvent::PasteSource));
        assert!(!model.accepts(&UiEvent::OpenTorrent));
        assert!(!model.accepts(&UiEvent::ChooseSource(71)));
        assert!(!model.can_play());
        model.playback_active = false;
        model.preparing = true;
        assert!(!model.accepts(&UiEvent::PasteSource));
        assert!(!model.accepts(&UiEvent::OpenTorrent));
        assert!(!model.accepts(&UiEvent::ChooseSource(71)));
        assert!(!model.can_play());
        model.preparing = false;
        model.movie_watch_pending = true;
        assert!(!model.accepts(&UiEvent::ChooseSource(71)));
        assert!(!model.can_play());
        assert!(!model.accepts(&UiEvent::PlayFromStart));
        model.movie_watch_pending = false;
        assert!(model.accepts(&UiEvent::ChooseSource(71)));
        assert!(model.can_play());
    }

    #[test]
    fn discovery_feedback_distinguishes_loading_partial_complete_empty_and_failed() {
        let mut discovery = SourceDiscovery {
            pending: 2,
            ..SourceDiscovery::default()
        };
        assert_eq!(discovery.heading(false), "Поиск источников…");
        assert_eq!(
            discovery.heading(true),
            "Источники найдены · поиск продолжается"
        );
        discovery.pending = 0;
        discovery.succeeded = 2;
        assert_eq!(discovery.heading(true), "Поиск завершён");
        assert_eq!(discovery.heading(false), "Источники не найдены");
        discovery.failures.push("Provider unavailable".into());
        discovery.succeeded = 1;
        assert_eq!(discovery.heading(true), "Поиск завершён частично");
        discovery.succeeded = 0;
        assert_eq!(
            discovery.heading(false),
            "Все провайдеры завершились с ошибкой"
        );
    }

    #[test]
    fn provider_notices_survive_empty_possible_and_mixed_failure_results() {
        let notices = vec![
            "RuTracker: темы найдены, подходящих раздач нет".to_owned(),
            "Commons: источники не найдены".to_owned(),
        ];
        let mut discovery = SourceDiscovery {
            pending: 1,
            succeeded: 2,
            notices: notices.clone(),
            failures: vec!["Indexer: недоступен".into()],
        };
        assert_eq!(discovery.heading(false), "Поиск источников…");
        discovery.pending = 0;
        assert_eq!(discovery.heading(false), "Источники не найдены");
        assert_eq!(discovery.heading(true), "Поиск завершён частично");
        assert_eq!(discovery.notices, notices);
        assert_eq!(discovery.failures, ["Indexer: недоступен"]);
        discovery.failures.clear();
        assert_eq!(discovery.heading(true), "Поиск завершён");
        assert_eq!(discovery.heading(false), "Источники не найдены");
        assert_eq!(discovery.notices, notices, "notices are not failures");
    }

    #[test]
    fn commons_is_enabled_by_default_and_independent_of_torrent_providers() {
        let defaults = SourcePreferencesDraft::default();
        assert!(defaults.commons_enabled);
        assert!(defaults.rutracker_enabled);
        assert!(!defaults.popcorn_enabled);
        assert!(defaults.torznab.is_empty());
        let disabled = SourcePreferencesDraft {
            commons_enabled: false,
            ..defaults.clone()
        };
        assert!(!disabled.commons_enabled);
        assert_eq!(disabled.rutracker_enabled, defaults.rutracker_enabled);
        assert_eq!(disabled.popcorn_enabled, defaults.popcorn_enabled);
        assert_eq!(disabled.torznab, defaults.torznab);
    }

    #[test]
    fn source_preferences_event_debug_never_exposes_url_or_env_reference() {
        let event = UiEvent::SaveSourcePreferences(SourcePreferencesDraft {
            torznab: vec![TorznabDraft {
                name: "private-name".into(),
                base_url: "https://private-indexer.example/api?apikey=private-value".into(),
                api_key_env: Some("KADR_TORZNAB_PRIVATE".into()),
                enabled: true,
            }],
            ..SourcePreferencesDraft::default()
        });
        let debug = format!("{event:?}");
        for private in [
            "private-name",
            "private-indexer",
            "private-value",
            "KADR_TORZNAB_PRIVATE",
        ] {
            assert!(!debug.contains(private));
        }
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn sections_have_unique_stable_indices() {
        for (index, section) in Section::ALL.into_iter().enumerate() {
            assert_eq!(section.index(), index);
            assert!(!section.id().is_empty());
            assert!(!section.title().is_empty());
            assert!(
                Section::ALL[..index]
                    .iter()
                    .all(|previous| previous.id() != section.id())
            );
        }
    }

    #[test]
    fn player_controls_have_unique_ids_and_labels() {
        for (index, player) in PlayerChoice::ALL.into_iter().enumerate() {
            assert!(!player.label().is_empty());
            assert!(
                PlayerChoice::ALL[..index]
                    .iter()
                    .all(|previous| previous.id() != player.id())
            );
        }
    }

    #[test]
    fn state_messages_are_not_replaced_with_fake_results() {
        let states = [
            SectionState::Unavailable("Нет интеграции".into()),
            SectionState::Loading("Получение данных".into()),
            SectionState::Empty("Нет записей".into()),
            SectionState::Error("Не удалось получить данные".into()),
        ];
        for state in states {
            assert!(!state.heading().is_empty());
            assert!(!state.message().is_empty());
        }
    }

    #[test]
    fn status_severity_is_preserved() {
        assert_eq!(UiStatus::info("Готово").level, StatusLevel::Info);
        assert_eq!(UiStatus::warning("Ограничение").level, StatusLevel::Warning);
        assert_eq!(UiStatus::error("Ошибка").level, StatusLevel::Error);
    }

    #[test]
    fn stale_source_file_and_library_controls_do_not_emit() {
        let mut model = ShellModel::default();
        assert!(!model.accepts(&UiEvent::ChooseSource(7)));
        model.sources.push(SourceRow {
            index: 7,
            recommended: false,
            possible_match: false,
            title: "Public source".into(),
            kind: SourceKind::Http,
            quality: None,
            language: None,
            resolution: None,
            languages: vec![],
            origin: "Provider".into(),
            video_codec: None,
            audio_codec: None,
            peers: None,
            license: None,
            size: None,
            seeds: None,
            rutracker: false,
        });
        assert!(model.accepts(&UiEvent::ChooseSource(7)));
        assert!(!model.accepts(&UiEvent::ChooseSource(0)));
        model.playback_active = true;
        assert!(!model.accepts(&UiEvent::ChooseSource(7)));
        assert!(!model.accepts(&UiEvent::OpenLibrary("missing".into())));
        assert!(!model.accepts(&UiEvent::ChooseTorrentFile(0)));
        assert!(!model.accepts(&UiEvent::AssignEpisode {
            season: 1,
            number: 1
        }));
        assert!(!model.accepts(&UiEvent::ToggleFavorite));
        assert!(!model.accepts(&UiEvent::SelectEpisode {
            season: 1,
            number: 1
        }));
    }

    #[test]
    fn active_source_is_required_for_session_actions() {
        let mut model = ShellModel::default();
        assert!(!model.accepts(&UiEvent::Save));
        model.source = Some(SourcePreview {
            kind: SourceKind::LocalFile,
            title: "Video".into(),
        });
        assert!(!model.accepts(&UiEvent::Save));
        model.playback_active = true;
        assert!(model.accepts(&UiEvent::Save));
        assert!(model.accepts(&UiEvent::OpenIn(PlayerChoice::Vlc)));
        model.source = None;
        assert!(!model.accepts(&UiEvent::OpenIn(PlayerChoice::Vlc)));
    }
}
