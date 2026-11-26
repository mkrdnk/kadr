//! Display-only state. Source URLs and credentials belong to the controller.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
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
    Play,
    PlayFromStart,
    Stop,
    SetDefaultPlayer(PlayerChoice),
    Retry(Section),
    Cancel(Section),
    Search(String),
    Browse,
    OpenMedia(String),
    FindSources,
    SelectEpisode {
        season: u16,
        number: u16,
    },
    ChooseSource(usize),
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
    ChooseCacheDirectory,
    ClearCache,
    ClearHistory,
    UnlinkMedia,
    /// The controller remembers credentials only in the system vault, never config.
    SignInRuTracker(RuTrackerCredentials),
    ForgetRuTracker,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Movie,
    Series,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpisodeRow {
    pub season: u16,
    pub number: u16,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub kind: MediaKind,
    pub episodes: Vec<EpisodeRow>,
    pub attribution: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub index: usize,
    pub title: String,
    pub kind: SourceKind,
    pub quality: Option<String>,
    pub language: Option<String>,
    pub origin: String,
    pub license: Option<String>,
    /// Controller-provided size in bytes; absence is not a zero-size download.
    pub size: Option<u64>,
    pub seeds: Option<u32>,
    /// Tracker metadata is unverified, including quality and rights claims.
    pub rutracker: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryRow {
    pub id: String,
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
            && self.position.is_some_and(|position| position > 0)
            && self.position != self.duration
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreferencesDraft {
    pub cache_limit_bytes: u64,
    pub download_limit: RateLimit,
    pub upload_limit: RateLimit,
    pub peer_limit: u32,
    pub torrents_enabled: bool,
    pub catalog_enabled: bool,
    pub cache_directory: String,
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
    pub status: UiStatus,
    pub preferences_status: String,
    pub results: Vec<MediaCard>,
    pub movies: Vec<MediaCard>,
    pub shows: Vec<MediaCard>,
    pub browse_state: SectionState,
    pub media: Option<MediaCard>,
    pub sources: Vec<SourceRow>,
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
            status: UiStatus::info("Откройте источник или найдите фильм в каталоге."),
            preferences_status: "Изменения применяются после сохранения.".into(),
            results: Vec::new(),
            movies: Vec::new(),
            shows: Vec::new(),
            browse_state: SectionState::Empty("Каталог ещё не загружен.".into()),
            media: None,
            sources: Vec::new(),
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
            && !self.playback_active
            && !matches!(
                self.states[Section::Sources.index()],
                SectionState::Loading(_)
            )
    }

    /// Check again when handling a click/key event, not only when rendering.
    pub fn accepts(&self, event: &UiEvent) -> bool {
        match event {
            UiEvent::Play | UiEvent::PlayFromStart => self.can_play(),
            UiEvent::Stop => self.playback_active,
            UiEvent::PasteSource | UiEvent::OpenTorrent => {
                !self.playback_active
                    && !matches!(
                        self.states[Section::Sources.index()],
                        SectionState::Loading(_)
                    )
            }
            UiEvent::Retry(section) => {
                matches!(self.states[section.index()], SectionState::Error(_))
            }
            UiEvent::Cancel(section) => {
                matches!(self.states[section.index()], SectionState::Loading(_))
            }
            UiEvent::ChooseSource(index) => {
                self.sources.iter().any(|row| row.index == *index)
                    && self.accepts(&UiEvent::PasteSource)
            }
            UiEvent::ChooseTorrentFile(index) => {
                self.files.iter().any(|row| row.index == *index) && !self.playback_active
            }
            UiEvent::AssignEpisode { .. } => self.selected_file.is_some(),
            UiEvent::FindSources | UiEvent::ToggleFavorite => self.media.is_some(),
            UiEvent::SelectEpisode { season, number } => self.media.as_ref().is_some_and(|media| {
                media
                    .episodes
                    .iter()
                    .any(|episode| episode.season == *season && episode.number == *number)
            }),
            UiEvent::OpenMedia(id) => {
                !self.playback_active
                    && self
                        .results
                        .iter()
                        .chain(&self.movies)
                        .chain(&self.shows)
                        .chain(&self.media)
                        .any(|media| media.id == *id)
            }
            UiEvent::Browse => !matches!(self.browse_state, SectionState::Loading(_)),
            UiEvent::OpenLibrary(id) => {
                !self.playback_active && self.library.iter().any(|row| row.id == *id)
            }
            UiEvent::SetFavorite { id, .. } => self.library.iter().any(|row| row.id == *id),
            UiEvent::UnlinkMedia => self.media.is_some(),
            UiEvent::Save | UiEvent::OpenIn(_) => self.source.is_some() && self.playback_active,
            UiEvent::Search(query) => !query.trim().is_empty(),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn retry_and_cancel_only_apply_to_corresponding_states() {
        let mut model = ShellModel::default();
        let section = Section::Sources;
        assert!(!model.accepts(&UiEvent::Retry(section)));
        assert!(!model.accepts(&UiEvent::Cancel(section)));
        model.states[section.index()] = SectionState::Loading("Проверка источника".into());
        assert!(model.accepts(&UiEvent::Cancel(section)));
        assert!(!model.accepts(&UiEvent::Retry(section)));
        assert!(!model.accepts(&UiEvent::PasteSource));
        model.states[section.index()] = SectionState::Error("Источник недоступен".into());
        assert!(model.accepts(&UiEvent::Retry(section)));
        assert!(!model.accepts(&UiEvent::Cancel(section)));
        assert!(model.accepts(&UiEvent::PasteSource));
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
            title: "Public source".into(),
            kind: SourceKind::Http,
            quality: None,
            language: None,
            origin: "Provider".into(),
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
