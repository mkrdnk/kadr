//! Display-only state. Raw source URLs, credentials and filesystem paths belong
//! to the application controller, not to this model.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    Search,
    Library,
    #[default]
    Sources,
    Transfers,
    Settings,
}

impl Section {
    pub const ALL: [Self; 5] = [
        Self::Search,
        Self::Library,
        Self::Sources,
        Self::Transfers,
        Self::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Search => "Поиск",
            Self::Library => "Библиотека",
            Self::Sources => "Источники",
            Self::Transfers => "Передачи",
            Self::Settings => "Настройки",
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Search => 0,
            Self::Library => 1,
            Self::Sources => 2,
            Self::Transfers => 3,
            Self::Settings => 4,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Search => "nav-search",
            Self::Library => "nav-library",
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
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP(S)",
            Self::Magnet => "Magnet",
            Self::TorrentFile => ".torrent",
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
}

pub(crate) struct ShellModel {
    pub section: Section,
    pub states: [SectionState; 5],
    pub default_player: PlayerChoice,
    pub source: Option<SourcePreview>,
    pub playback_available: bool,
    pub playback_active: bool,
    pub status: UiStatus,
    pub preferences_status: String,
}

impl Default for ShellModel {
    fn default() -> Self {
        Self {
            section: Section::Sources,
            states: [
                SectionState::Unavailable(
                    "Каталог ещё не подключён. Поиск не отправляет запросы в сеть.".into(),
                ),
                SectionState::Unavailable(
                    "Библиотека ещё не подключена. Избранное и история не загружены.".into(),
                ),
                SectionState::Empty(
                    "Добавьте HTTP(S)- или magnet-ссылку из буфера обмена либо выберите файл .torrent."
                        .into(),
                ),
                SectionState::Unavailable(
                    "Медиадвижок ещё не подключён. Данных о загрузках, скорости и пирах нет.".into(),
                ),
                SectionState::Unavailable(
                    "Пути, объём кэша и сетевые лимиты появятся после подключения хранилища и медиадвижка."
                        .into(),
                ),
            ],
            default_player: PlayerChoice::Mpv,
            source: None,
            playback_available: false,
            playback_active: false,
            status: UiStatus::info(
                "Каркас P1 · Каталог и торрент-движок ещё не подключены.",
            ),
            preferences_status: "Сохранение настроек ещё не подключено.".into(),
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
            UiEvent::SetDefaultPlayer(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_without_fabricated_results_or_playback() {
        let model = ShellModel::default();
        assert_eq!(model.section, Section::Sources);
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
            SectionState::Unavailable(_)
        ));
        assert!(matches!(
            model.states[Section::Library.index()],
            SectionState::Unavailable(_)
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
}
