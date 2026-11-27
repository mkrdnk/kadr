//! Owned engine-facing data. Persistence and source descriptors stay internal.

use std::{fmt, path::PathBuf};

use kadr_core::source::SourceKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerPreference {
    Mpv,
    Vlc,
    Browser,
}

impl PlayerPreference {
    pub(crate) fn from_storage(value: kadr_storage::PlayerPreference) -> Self {
        match value {
            kadr_storage::PlayerPreference::Mpv => Self::Mpv,
            kadr_storage::PlayerPreference::Vlc => Self::Vlc,
            kadr_storage::PlayerPreference::Browser => Self::Browser,
        }
    }

    pub(crate) fn into_storage(self) -> kadr_storage::PlayerPreference {
        match self {
            Self::Mpv => kadr_storage::PlayerPreference::Mpv,
            Self::Vlc => kadr_storage::PlayerPreference::Vlc,
            Self::Browser => kadr_storage::PlayerPreference::Browser,
        }
    }
}

/// Explicit semantics: zero is not silently interpreted as unlimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimit {
    Paused,
    BytesPerSecond(u64),
    Unlimited,
}

impl RateLimit {
    pub(crate) fn from_storage(value: kadr_storage::RateLimit) -> Self {
        match value {
            kadr_storage::RateLimit::Paused => Self::Paused,
            kadr_storage::RateLimit::BytesPerSecond(bytes) => Self::BytesPerSecond(bytes),
            kadr_storage::RateLimit::Unlimited => Self::Unlimited,
        }
    }

    pub(crate) fn into_storage(self) -> kadr_storage::RateLimit {
        match self {
            Self::Paused => kadr_storage::RateLimit::Paused,
            Self::BytesPerSecond(bytes) => kadr_storage::RateLimit::BytesPerSecond(bytes),
            Self::Unlimited => kadr_storage::RateLimit::Unlimited,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkLimits {
    /// Disabled until the user is informed about BitTorrent peer uploads.
    pub torrents_enabled: bool,
    pub download: RateLimit,
    pub upload: RateLimit,
    pub max_connections: u16,
}

impl NetworkLimits {
    pub(crate) fn from_storage(value: kadr_storage::NetworkLimits) -> Self {
        Self {
            torrents_enabled: value.torrents_enabled,
            download: RateLimit::from_storage(value.download),
            upload: RateLimit::from_storage(value.upload),
            max_connections: value.max_connections,
        }
    }

    pub(crate) fn into_storage(self) -> kadr_storage::NetworkLimits {
        kadr_storage::NetworkLimits {
            torrents_enabled: self.torrents_enabled,
            download: self.download.into_storage(),
            upload: self.upload.into_storage(),
            max_connections: self.max_connections,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Settings {
    pub default_player: PlayerPreference,
    pub network: NetworkLimits,
    pub cache_limit_bytes: u64,
    /// Catalog requests are allowed only in response to user action.
    pub catalog_enabled: bool,
    /// Explicit proxy endpoint for catalog requests and artwork, not playback.
    pub catalog_proxy: Option<String>,
    /// Persisting this does not move files. None uses the default cache directory.
    pub cache_directory: Option<PathBuf>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Settings([redacted])")
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_storage(kadr_storage::Settings::default())
    }
}

impl Settings {
    pub(crate) fn from_storage(value: kadr_storage::Settings) -> Self {
        Self {
            default_player: PlayerPreference::from_storage(value.default_player),
            network: NetworkLimits::from_storage(value.network),
            cache_limit_bytes: value.cache_limit_bytes,
            catalog_enabled: value.catalog_enabled,
            catalog_proxy: value.catalog_proxy,
            cache_directory: value.cache_directory,
        }
    }

    pub(crate) fn into_storage(self) -> kadr_storage::Settings {
        kadr_storage::Settings {
            default_player: self.default_player.into_storage(),
            network: self.network.into_storage(),
            cache_limit_bytes: self.cache_limit_bytes,
            catalog_enabled: self.catalog_enabled,
            catalog_proxy: self.catalog_proxy,
            cache_directory: self.cache_directory,
        }
    }
}

/// Logical bytes, not filesystem allocation. Reservations include absent bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheUsage {
    pub limit_bytes: u64,
    pub reserved_bytes: u64,
    pub actual_bytes: u64,
    pub active_entries: u64,
    pub active_leases: u64,
}

impl CacheUsage {
    pub(crate) fn from_cache(value: kadr_cache::CacheUsage) -> Self {
        Self {
            limit_bytes: value.limit_bytes,
            reserved_bytes: value.reserved_bytes,
            actual_bytes: value.actual_bytes,
            active_entries: value.active_entries,
            active_leases: value.active_leases,
        }
    }
}

/// Display metadata only; the worker retains the actionable source descriptor.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceCandidate {
    /// Selection handle, independent of display order and never a private URI.
    pub index: usize,
    pub kind: SourceKind,
    pub title: String,
    pub quality: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub language: Option<String>,
    pub license: String,
    pub origin: String,
    pub size: Option<u64>,
    pub seeds: Option<u32>,
    pub peers: Option<u32>,
    pub rutracker: bool,
    /// Completed discovery's best suitable film torrent with known seeds > 0.
    /// Never a provisional result, HTTP resource or explicit manual attachment.
    pub recommended: bool,
}

impl fmt::Debug for SourceCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SourceCandidate([redacted])")
    }
}

impl SourceCandidate {
    pub(crate) fn from_source(
        index: usize,
        value: &kadr_core::sources::MediaSource,
        origin: String,
    ) -> Self {
        use kadr_core::sources::Locator;
        Self {
            index,
            kind: match value.locator {
                Locator::Http(_) => SourceKind::Http,
                Locator::Magnet(_) => SourceKind::Magnet,
                Locator::TorrentUrl(_) | Locator::TorrentFile(_) | Locator::RuTracker(_) => {
                    SourceKind::TorrentFile
                }
            },
            title: value.release_title.as_ref().unwrap_or(&value.title).clone(),
            quality: value
                .quality
                .clone()
                .or_else(|| value.resolution.map(|h| format!("{h}p"))),
            language: (!value.languages.is_empty()).then(|| value.languages.join(", ")),
            video_codec: value.video_codec.clone(),
            audio_codec: value.audio_codec.clone(),
            license: value.license.clone().unwrap_or_default(),
            origin,
            size: value.size,
            seeds: value.seeds,
            peers: value.peers,
            rutracker: matches!(value.locator, Locator::RuTracker(_)),
            recommended: false,
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct LibraryItem {
    pub id: String,
    pub title: String,
    pub poster_url: Option<String>,
    pub favorite: bool,
    pub opened_at: Option<i64>,
    pub episode: Option<LibraryEpisode>,
    pub progress: Option<LibraryProgress>,
}

impl fmt::Debug for LibraryItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LibraryItem([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryEpisode {
    pub season: u16,
    pub number: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LibraryProgress {
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub completed: bool,
}

impl LibraryItem {
    pub(crate) fn from_storage(value: kadr_storage::LibraryItem) -> Self {
        let id = match value.target.episode {
            Some(episode) => format!(
                "{}@{}:{}",
                value.target.media_id.as_str(),
                episode.season,
                episode.episode
            ),
            None => value.target.media_id.as_str().to_owned(),
        };
        let (title, poster_url) = value.media.map_or_else(
            || ("Ручной источник".into(), None),
            |media| (media.title, public_catalog_poster(media.poster_url)),
        );
        Self {
            id,
            title,
            poster_url,
            favorite: value.entry.favorite,
            opened_at: value.opened_at,
            episode: value.target.episode.map(|episode| LibraryEpisode {
                season: episode.season,
                number: episode.episode,
            }),
            progress: value.progress.map(|progress| LibraryProgress {
                position_seconds: progress.position_seconds,
                duration_seconds: progress.duration_seconds,
                completed: progress.completed,
            }),
        }
    }
}

/// Stored snapshots are not trusted as public assets.
pub(crate) fn public_catalog_poster(value: Option<String>) -> Option<String> {
    value.filter(|value| {
        reqwest::Url::parse(value).is_ok_and(|url| kadr_catalog::is_public_catalog_image(&url))
    })
}

#[derive(Clone, PartialEq, Eq)]
pub struct EngineDirectories {
    pub data_directory: PathBuf,
    pub config_directory: PathBuf,
    pub cache_directory: PathBuf,
}

impl fmt::Debug for EngineDirectories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EngineDirectories([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_matches_storage() {
        let settings = Settings::default();
        assert_eq!(settings.default_player, PlayerPreference::Mpv);
        assert_eq!(
            settings.network,
            NetworkLimits {
                torrents_enabled: false,
                download: RateLimit::BytesPerSecond(10 * 1024 * 1024),
                upload: RateLimit::Paused,
                max_connections: 64,
            }
        );
        assert_eq!(settings.cache_limit_bytes, 10 * 1024 * 1024 * 1024);
        assert!(settings.catalog_enabled);
        assert_eq!(settings.catalog_proxy, None);
        assert_eq!(settings.cache_directory, None);
        assert_eq!(settings.into_storage(), kadr_storage::Settings::default());
    }

    #[test]
    fn settings_roundtrip_preserves_all_variants_and_fields() {
        let players = [
            PlayerPreference::Mpv,
            PlayerPreference::Vlc,
            PlayerPreference::Browser,
        ];
        let rates = [
            RateLimit::Paused,
            RateLimit::BytesPerSecond(42),
            RateLimit::Unlimited,
        ];
        for player in players {
            for download in rates {
                for upload in rates {
                    for enabled in [false, true] {
                        for directory in [None, Some(PathBuf::from("/private/cache"))] {
                            let settings = Settings {
                                default_player: player,
                                network: NetworkLimits {
                                    torrents_enabled: enabled,
                                    download,
                                    upload,
                                    max_connections: 4096,
                                },
                                cache_limit_bytes: 123_456_789,
                                catalog_enabled: !enabled,
                                catalog_proxy: enabled.then(|| "socks5h://127.0.0.1:1080".into()),
                                cache_directory: directory,
                            };
                            let stored = settings.clone().into_storage();
                            assert_eq!(
                                stored.default_player,
                                match player {
                                    PlayerPreference::Mpv => kadr_storage::PlayerPreference::Mpv,
                                    PlayerPreference::Vlc => kadr_storage::PlayerPreference::Vlc,
                                    PlayerPreference::Browser => {
                                        kadr_storage::PlayerPreference::Browser
                                    }
                                }
                            );
                            assert_eq!(Settings::from_storage(stored.clone()), settings);
                            assert_eq!(
                                Settings::from_storage(stored.clone()).into_storage(),
                                stored
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rate_limit_mapping_is_exact_without_validation_or_normalization() {
        for (engine, storage) in [
            (RateLimit::Paused, kadr_storage::RateLimit::Paused),
            (
                RateLimit::BytesPerSecond(0),
                kadr_storage::RateLimit::BytesPerSecond(0),
            ),
            (
                RateLimit::BytesPerSecond(u64::MAX),
                kadr_storage::RateLimit::BytesPerSecond(u64::MAX),
            ),
            (RateLimit::Unlimited, kadr_storage::RateLimit::Unlimited),
        ] {
            assert_eq!(engine.into_storage(), storage);
            assert_eq!(RateLimit::from_storage(storage), engine);
        }
    }

    #[test]
    fn settings_debug_redacts_private_values() {
        let settings = Settings {
            cache_directory: Some("/private/secret-cache".into()),
            catalog_proxy: Some("http://proxy.example:1234".into()),
            ..Settings::default()
        };
        assert_eq!(format!("{settings:?}"), "Settings([redacted])");
        assert_eq!(format!("{settings:#?}"), "Settings([redacted])");
    }

    #[test]
    fn cache_usage_preserves_every_counter() {
        let usage = CacheUsage::from_cache(kadr_cache::CacheUsage {
            limit_bytes: u64::MAX,
            reserved_bytes: 123,
            actual_bytes: 456,
            active_entries: 7,
            active_leases: 8,
        });
        assert_eq!(
            usage,
            CacheUsage {
                limit_bytes: u64::MAX,
                reserved_bytes: 123,
                actual_bytes: 456,
                active_entries: 7,
                active_leases: 8,
            }
        );
    }

    #[test]
    fn source_display_has_no_topic_credentials_or_unverified_license() {
        let mut source = kadr_core::sources::MediaSource::new(
            "rutracker",
            kadr_core::sources::Locator::RuTracker(42),
            "Source title",
            kadr_core::catalog::MediaKind::Movie,
        )
        .unwrap();
        source.quality = Some("1080p".into());
        source.size = Some(1024);
        source.seeds = Some(20);
        let candidate = SourceCandidate::from_source(9, &source, "RuTracker".into());
        assert_eq!(format!("{candidate:?}"), "SourceCandidate([redacted])");
        assert_eq!(format!("{candidate:#?}"), "SourceCandidate([redacted])");
        let SourceCandidate {
            index,
            kind,
            title,
            quality,
            video_codec,
            audio_codec,
            language,
            license,
            origin,
            size,
            seeds,
            peers,
            rutracker,
            recommended,
        } = candidate;
        assert_eq!(index, 9);
        assert_eq!(kind, SourceKind::TorrentFile);
        assert_eq!(title, "Source title");
        assert_eq!(quality.as_deref(), Some("1080p"));
        assert_eq!((video_codec, audio_codec, peers), (None, None, None));
        assert_eq!(language, None);
        assert!(license.is_empty());
        assert_eq!(origin, "RuTracker");
        assert_eq!((size, seeds, rutracker), (Some(1024), Some(20), true));
        assert!(!recommended);
    }

    fn stored_library_item() -> kadr_storage::LibraryItem {
        use kadr_storage::*;

        let target = PlaybackTarget {
            media_id: MediaId::new("catalog:series").unwrap(),
            episode: Some(EpisodeKey::new(0, 7).unwrap()),
        };
        LibraryItem {
            entry: LibraryEntry {
                media_id: target.media_id.clone(),
                favorite: true,
                last_opened_at: Some(999),
                last_source_id: Some(SourceId::new("private-source-id").unwrap()),
            },
            media: Some(MediaRecord {
                id: target.media_id.clone(),
                title: "/private/title.mkv".into(),
                original_title: None,
                year: None,
                kind: MediaKind::Series,
                description: "Private description".into(),
                poster_url: Some("https://private.example/poster?token=secret".into()),
                attribution: "Private attribution".into(),
            }),
            target: target.clone(),
            opened_at: Some(123),
            progress: Some(PlaybackProgress {
                target,
                source_id: SourceId::new("private-source-id").unwrap(),
                position_seconds: 12.5,
                duration_seconds: Some(100.25),
                updated_at: 234,
                completed: false,
                origin: ProgressOrigin::Mpv,
            }),
            source: Some(StoredSource::TorrentFile {
                path: "/private/descriptor.torrent".into(),
                file_id: Some(42),
            }),
        }
    }

    #[test]
    fn library_conversion_uses_exact_target_and_preserves_metadata() {
        let mut stored = stored_library_item();
        // Resume identity comes from target, not the entry or measured target.
        stored.entry.media_id = kadr_storage::MediaId::new("different:entry").unwrap();
        stored.progress.as_mut().unwrap().target.episode =
            Some(kadr_storage::EpisodeKey::new(9, 99).unwrap());
        let item = LibraryItem::from_storage(stored);
        // Exhaustive destructuring ensures no source descriptor is retained.
        let LibraryItem {
            id,
            title,
            poster_url,
            favorite,
            opened_at,
            episode,
            progress,
        } = item;
        assert_eq!(id, "catalog:series@0:7");
        assert_eq!(title, "/private/title.mkv");
        assert!(poster_url.is_none());
        assert!(favorite);
        assert_eq!(opened_at, Some(123));
        assert_eq!(
            episode,
            Some(LibraryEpisode {
                season: 0,
                number: 7,
            })
        );
        assert_eq!(
            progress,
            Some(LibraryProgress {
                position_seconds: 12.5,
                duration_seconds: Some(100.25),
                completed: false,
            })
        );
    }

    #[test]
    fn library_without_episode_or_snapshot_uses_manual_title_and_plain_id() {
        let mut stored = stored_library_item();
        stored.target.episode = None;
        stored.media = None;
        stored.entry.favorite = false;
        stored.opened_at = None;
        stored.progress = None;
        let item = LibraryItem::from_storage(stored);
        assert_eq!(item.id, "catalog:series");
        assert_eq!(item.title, "Ручной источник");
        assert!(!item.favorite);
        assert_eq!(item.opened_at, None);
        assert_eq!(item.episode, None);
        assert_eq!(item.progress, None);
    }

    #[test]
    fn library_preserves_unknown_duration_and_completion_without_inference() {
        for completed in [false, true] {
            let mut stored = stored_library_item();
            let progress = stored.progress.as_mut().unwrap();
            progress.position_seconds = 0.0;
            progress.duration_seconds = None;
            progress.completed = completed;
            let item = LibraryItem::from_storage(stored);
            assert_eq!(
                item.progress,
                Some(LibraryProgress {
                    position_seconds: 0.0,
                    duration_seconds: None,
                    completed,
                })
            );
        }
    }

    #[test]
    fn library_debug_never_exposes_titles_or_descriptors() {
        let item = LibraryItem::from_storage(stored_library_item());
        assert_eq!(format!("{item:?}"), "LibraryItem([redacted])");
        assert_eq!(format!("{item:#?}"), "LibraryItem([redacted])");
    }

    #[test]
    fn library_poster_is_a_public_provider_asset() {
        for (url, allowed) in [
            ("https://media.trakt.tv/movies/1/medium/poster.webp", true),
            ("https://media.trakt.tv/shows/2/medium/fanart.webp", true),
            (
                "https://media.trakt.tv/movies/1/medium/poster.webp?token=private",
                false,
            ),
            (
                "https://media.trakt.tv/movies/1/medium/secret/poster.webp",
                false,
            ),
            (
                "https://media.trakt.tv.example/movies/1/medium/poster.webp",
                false,
            ),
            ("https://image.tmdb.org/t/p/w500/movie.jpg", false),
            ("https://archive.org/services/img/movie", false),
            (
                "https://static.tvmaze.com/uploads/images/medium_portrait/1.jpg",
                false,
            ),
        ] {
            assert_eq!(public_catalog_poster(Some(url.into())).is_some(), allowed);
        }
    }

    #[test]
    fn engine_directories_debug_redacts_all_paths() {
        let directories = EngineDirectories {
            data_directory: "/private/data".into(),
            config_directory: "/private/config".into(),
            cache_directory: "/private/cache".into(),
        };
        assert_eq!(format!("{directories:?}"), "EngineDirectories([redacted])");
        assert_eq!(format!("{directories:#?}"), "EngineDirectories([redacted])");
    }
}
