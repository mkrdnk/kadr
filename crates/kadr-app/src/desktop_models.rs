//! Pure display conversions. Never copy source URLs or descriptors into UI DTOs.

use kadr_engine::{
    CacheUsage, FileChoice, LibraryItem, PlayerPreference, RateLimit, Settings, SourceCandidate,
    SourcePreferences, TorznabConfig, TransferStatus,
    catalog::{Media, MediaKind},
    source::SourceKind,
};
use kadr_ui::{
    EpisodeRow, LibraryRow, MediaCard, PlayerChoice, PreferencesDraft, RateLimit as UiRateLimit,
    SourceKind as UiSourceKind, SourcePreferencesDraft, SourceRow, TorrentFileRow, TorznabDraft,
    TransferView,
};

pub(super) fn preference(player: PlayerChoice) -> PlayerPreference {
    match player {
        PlayerChoice::Mpv => PlayerPreference::Mpv,
        PlayerChoice::Vlc => PlayerPreference::Vlc,
        PlayerChoice::Browser => PlayerPreference::Browser,
    }
}

pub(super) fn choice(player: PlayerPreference) -> PlayerChoice {
    match player {
        PlayerPreference::Mpv => PlayerChoice::Mpv,
        PlayerPreference::Vlc => PlayerChoice::Vlc,
        PlayerPreference::Browser => PlayerChoice::Browser,
    }
}

fn display_rate(limit: RateLimit) -> UiRateLimit {
    match limit {
        RateLimit::Paused => UiRateLimit::Paused,
        RateLimit::Unlimited => UiRateLimit::Unlimited,
        RateLimit::BytesPerSecond(bytes) => UiRateLimit::Bps(bytes),
    }
}

fn engine_rate(limit: UiRateLimit) -> RateLimit {
    match limit {
        UiRateLimit::Paused => RateLimit::Paused,
        UiRateLimit::Unlimited => RateLimit::Unlimited,
        UiRateLimit::Bps(bytes) => RateLimit::BytesPerSecond(bytes),
    }
}

pub(super) fn preferences_draft(settings: &Settings) -> PreferencesDraft {
    PreferencesDraft {
        cache_limit_bytes: settings.cache_limit_bytes,
        download_limit: display_rate(settings.network.download),
        upload_limit: display_rate(settings.network.upload),
        peer_limit: u32::from(settings.network.max_connections),
        torrents_enabled: settings.network.torrents_enabled,
        catalog_enabled: settings.catalog_enabled,
        catalog_proxy: settings.catalog_proxy.clone(),
        cache_directory: settings
            .cache_directory
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

pub(super) fn settings_from_draft(
    mut settings: Settings,
    draft: &PreferencesDraft,
) -> Result<Settings, &'static str> {
    // Do not truncate a UI integer to the engine type. All semantic limits
    // remain the backend's responsibility, including zero versus unlimited.
    let peers = u16::try_from(draft.peer_limit)
        .map_err(|_| "Число соединений не помещается в допустимый диапазон.")?;
    settings.cache_limit_bytes = draft.cache_limit_bytes;
    settings.network.download = engine_rate(draft.download_limit);
    settings.network.upload = engine_rate(draft.upload_limit);
    settings.network.max_connections = peers;
    settings.network.torrents_enabled = draft.torrents_enabled;
    settings.catalog_enabled = draft.catalog_enabled;
    settings.catalog_proxy = draft
        .catalog_proxy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    // Preserve a non-UTF8 native picker path across an unchanged UI round-trip.
    let current_path = settings
        .cache_directory
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    if draft.cache_directory != current_path {
        settings.cache_directory = if draft.cache_directory.trim().is_empty() {
            None
        } else {
            Some(draft.cache_directory.clone().into())
        };
    }
    Ok(settings)
}

pub(super) fn source_kind(kind: Option<SourceKind>) -> UiSourceKind {
    match kind {
        Some(SourceKind::Http) => UiSourceKind::Http,
        Some(SourceKind::Magnet) => UiSourceKind::Magnet,
        Some(SourceKind::TorrentFile) => UiSourceKind::TorrentFile,
        None => UiSourceKind::LocalFile,
    }
}

pub(super) fn media_card(media: Media) -> MediaCard {
    let mut attribution = media.attribution;
    if let Some(license) = media.license {
        attribution.push_str(" · ");
        attribution.push_str(&license);
    }
    if !media.source_page.is_empty() {
        attribution.push_str(" · ");
        attribution.push_str(&media.source_page);
    }
    MediaCard {
        id: media.id,
        title: media.title,
        original_title: media.original_title,
        year: media.year,
        description: Some(media.description),
        poster_url: media.poster_url,
        backdrop_url: media.backdrop_url,
        rating_tenths: media.rating_tenths,
        kind: match media.kind {
            MediaKind::Movie => kadr_ui::MediaKind::Movie,
            MediaKind::Series => kadr_ui::MediaKind::Series,
        },
        episodes: media
            .episodes
            .into_iter()
            .map(|episode| EpisodeRow {
                season: episode.season,
                number: episode.number,
                title: episode.title,
            })
            .collect(),
        attribution,
    }
}

pub(super) fn source_preferences_draft(settings: &SourcePreferences) -> SourcePreferencesDraft {
    SourcePreferencesDraft {
        rutracker_enabled: settings.rutracker_enabled,
        commons_enabled: settings.commons_enabled,
        popcorn_enabled: settings.popcorn_enabled,
        torznab: settings
            .torznab
            .iter()
            .map(|endpoint| TorznabDraft {
                name: endpoint.name.clone(),
                base_url: endpoint.base_url.clone(),
                api_key_env: endpoint.api_key_env.clone(),
                enabled: endpoint.enabled,
            })
            .collect(),
        preferred_resolution: settings.preferred_resolution.clone(),
        preferred_language: settings.preferred_language.clone(),
    }
}

pub(super) fn source_preferences(
    draft: &SourcePreferencesDraft,
) -> Result<SourcePreferences, &'static str> {
    fn optional(value: &Option<String>) -> Option<String> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }
    let torznab = draft
        .torznab
        .iter()
        .map(|endpoint| TorznabConfig {
            name: endpoint.name.trim().to_owned(),
            base_url: endpoint.base_url.trim().to_owned(),
            api_key_env: optional(&endpoint.api_key_env),
            enabled: endpoint.enabled,
        })
        .collect();
    let preferences = SourcePreferences {
        rutracker_enabled: draft.rutracker_enabled,
        commons_enabled: draft.commons_enabled,
        popcorn_enabled: draft.popcorn_enabled,
        torznab,
        preferred_resolution: optional(&draft.preferred_resolution),
        preferred_language: optional(&draft.preferred_language),
    };
    preferences.validate().map_err(
        |_| "Проверьте названия, адреса и имена переменных KADR_TORZNAB_… в настройках источников.",
    )?;
    Ok(preferences)
}

pub(super) fn source_row(source: SourceCandidate) -> SourceRow {
    SourceRow {
        index: source.index,
        recommended: source.recommended,
        possible_match: source.possible_match,
        title: source.title,
        kind: source_kind(Some(source.kind)),
        quality: source.quality,
        video_codec: source.video_codec,
        audio_codec: source.audio_codec,
        peers: source.peers,
        language: source.language,
        origin: source.origin,
        license: (!source.license.is_empty()).then_some(source.license),
        size: source.size,
        seeds: source.seeds,
        rutracker: source.rutracker,
    }
}

pub(super) fn torrent_file(file: FileChoice) -> TorrentFileRow {
    TorrentFileRow {
        index: file.index,
        name: file.name,
        size: file.len,
        season: file.season,
        episode: file.episode,
        ambiguous: file.ambiguous,
    }
}

pub(super) fn transfer_view(transfer: TransferStatus) -> TransferView {
    TransferView {
        stage: transfer.stage,
        download_bps: transfer.download_bps,
        upload_bps: transfer.upload_bps,
        peers: transfer.peers,
        done: transfer.done,
        total: transfer.total,
        buffered: transfer.buffered,
        saving: transfer.saving,
    }
}

fn seconds(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value < u64::MAX as f64).then_some(value as u64)
}

pub(super) fn library_row(item: LibraryItem) -> LibraryRow {
    let progress = item.progress.as_ref();
    let details = match progress {
        Some(progress) if progress.completed => "Просмотр завершён".into(),
        _ if item.episode.is_some() => {
            let episode = item.episode.as_ref().expect("checked episode");
            format!("Сезон {}, серия {}", episode.season, episode.number)
        }
        Some(_) => "Есть сохранённая позиция".into(),
        None if item.opened_at.is_some() => "Ранее открыто".into(),
        None if item.favorite => "В избранном".into(),
        None => "Источник подготовлен".into(),
    };
    LibraryRow {
        id: item.id,
        title: item.title,
        poster_url: item.poster_url,
        details,
        favorite: item.favorite,
        opened_at: item.opened_at,
        completed: progress.is_some_and(|value| value.completed),
        position: progress.and_then(|progress| seconds(progress.position_seconds)),
        duration: progress.and_then(|progress| progress.duration_seconds.and_then(seconds)),
    }
}

pub(super) fn cache_usage(usage: &CacheUsage) -> String {
    format!(
        "Временный кэш: {} байт; зарезервировано: {} байт; лимит: {} байт. Активных записей: {}.",
        usage.actual_bytes, usage.reserved_bytes, usage.limit_bytes, usage.active_entries,
    )
}

/// A backend suggestion must never turn into a path or expose a signed URL in
/// the native save dialog. The actual destination is always picked by the user.
pub(super) fn suggested_filename(input: &str) -> String {
    if input.is_empty() || input.contains("://") || input.starts_with("magnet:") {
        return "video.mp4".into();
    }
    let name = input.rsplit(['/', '\\']).next().unwrap_or("");
    let name = name.split(['?', '#']).next().unwrap_or("");
    if name.is_empty() || matches!(name, "." | "..") || name.chars().any(|ch| ch.is_control()) {
        "video.mp4".into()
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commons_preference_round_trips_both_states_without_changing_other_providers() {
        assert!(SourcePreferencesDraft::default().commons_enabled);
        for commons_enabled in [true, false] {
            for rutracker_enabled in [true, false] {
                let preferences = SourcePreferences {
                    commons_enabled,
                    rutracker_enabled,
                    popcorn_enabled: false,
                    torznab: vec![TorznabConfig {
                        name: "Local".into(),
                        base_url: "http://localhost:9696/api".into(),
                        api_key_env: Some("KADR_TORZNAB_LOCAL".into()),
                        enabled: false,
                    }],
                    preferred_resolution: Some("1080p".into()),
                    preferred_language: Some("ru".into()),
                };
                let draft = source_preferences_draft(&preferences);
                assert_eq!(draft.commons_enabled, commons_enabled);
                assert_eq!(draft.rutracker_enabled, rutracker_enabled);
                assert_eq!(source_preferences(&draft).unwrap(), preferences);
            }
        }
    }

    #[test]
    fn popcorn_preference_round_trips_both_states_independently() {
        assert!(!SourcePreferencesDraft::default().popcorn_enabled);
        for popcorn_enabled in [false, true] {
            for other_providers_enabled in [false, true] {
                let preferences = SourcePreferences {
                    popcorn_enabled,
                    commons_enabled: other_providers_enabled,
                    rutracker_enabled: other_providers_enabled,
                    torznab: vec![TorznabConfig {
                        name: "Local".into(),
                        base_url: "http://localhost:9696/api".into(),
                        api_key_env: None,
                        enabled: other_providers_enabled,
                    }],
                    preferred_resolution: Some("1080p".into()),
                    preferred_language: Some("ru".into()),
                };
                let mut draft = source_preferences_draft(&preferences);
                assert_eq!(draft.popcorn_enabled, popcorn_enabled);
                assert_eq!(source_preferences(&draft).unwrap(), preferences);
                draft.popcorn_enabled = !popcorn_enabled;
                assert_eq!(
                    source_preferences(&draft).unwrap(),
                    SourcePreferences {
                        popcorn_enabled: !popcorn_enabled,
                        ..preferences
                    }
                );
            }
        }
    }

    #[test]
    fn source_preferences_preserve_multiple_endpoints_and_independent_rutracker_toggle() {
        let draft = SourcePreferencesDraft {
            rutracker_enabled: false,
            commons_enabled: true,
            popcorn_enabled: true,
            torznab: vec![
                TorznabDraft {
                    name: " Public ".into(),
                    base_url: " https://public.example/torznab ".into(),
                    api_key_env: None,
                    enabled: true,
                },
                TorznabDraft {
                    name: "Private".into(),
                    base_url: "http://localhost:9696/api/v1/indexer".into(),
                    api_key_env: Some(" KADR_TORZNAB_PRIVATE ".into()),
                    enabled: false,
                },
            ],
            preferred_resolution: Some(" 1080p ".into()),
            preferred_language: Some(" ru ".into()),
        };
        let preferences = source_preferences(&draft).unwrap();
        assert!(!preferences.rutracker_enabled);
        assert!(preferences.commons_enabled);
        assert!(preferences.popcorn_enabled);
        assert_eq!(preferences.torznab.len(), 2);
        assert!(preferences.torznab[0].enabled);
        assert_eq!(preferences.torznab[0].name, "Public");
        assert_eq!(
            preferences.torznab[0].base_url,
            "https://public.example/torznab"
        );
        assert!(preferences.torznab[0].api_key_env.is_none());
        assert!(!preferences.torznab[1].enabled);
        assert_eq!(
            preferences.torznab[1].api_key_env.as_deref(),
            Some("KADR_TORZNAB_PRIVATE")
        );
        let confirmed = source_preferences_draft(&preferences);
        assert_eq!(confirmed.preferred_resolution.as_deref(), Some("1080p"));
        assert_eq!(confirmed.preferred_language.as_deref(), Some("ru"));
        assert_eq!(confirmed.torznab[1].name, "Private");
    }

    #[test]
    fn source_preferences_accept_only_optional_prefixed_environment_names() {
        let mut draft = SourcePreferencesDraft {
            rutracker_enabled: true,
            commons_enabled: false,
            popcorn_enabled: false,
            torznab: vec![TorznabDraft {
                name: "Endpoint".into(),
                base_url: "http://localhost:9696".into(),
                api_key_env: None,
                enabled: true,
            }],
            preferred_resolution: Some(" ".into()),
            preferred_language: None,
        };
        for name in ["", " ", "KADR_TORZNAB_LOCAL", "KADR_TORZNAB_123"] {
            draft.torznab[0].api_key_env = Some(name.into());
            let preferences = source_preferences(&draft).unwrap();
            assert!(preferences.preferred_resolution.is_none());
        }
        for name in [
            "API_KEY",
            "key-value",
            "KADR_TORZNAB_",
            "KADR_TORZNAB_KEY=value",
            "KADR_TORZNAB_A B",
        ] {
            draft.torznab[0].api_key_env = Some(name.into());
            assert!(source_preferences(&draft).is_err());
        }
    }

    #[test]
    fn preferences_round_trip_without_interpreting_paused_as_unlimited() {
        let settings = Settings::default();
        let draft = preferences_draft(&settings);
        assert_eq!(
            settings_from_draft(settings.clone(), &draft).unwrap(),
            settings
        );
        let mut draft = draft;
        draft.download_limit = UiRateLimit::Paused;
        draft.upload_limit = UiRateLimit::Unlimited;
        let changed = settings_from_draft(settings, &draft).unwrap();
        assert_eq!(changed.network.download, RateLimit::Paused);
        assert_eq!(changed.network.upload, RateLimit::Unlimited);
    }

    #[test]
    fn oversized_connection_count_is_rejected_instead_of_wrapping() {
        let settings = Settings::default();
        let mut draft = preferences_draft(&settings);
        draft.peer_limit = u32::from(u16::MAX) + 1;
        assert!(settings_from_draft(settings.clone(), &draft).is_err());
        draft.peer_limit = u32::MAX;
        assert!(settings_from_draft(settings.clone(), &draft).is_err());
        draft.peer_limit = u32::from(u16::MAX);
        assert_eq!(
            settings_from_draft(settings, &draft)
                .unwrap()
                .network
                .max_connections,
            u16::MAX,
        );
    }

    #[test]
    fn non_player_settings_changes_preserve_default_player() {
        let settings = Settings {
            default_player: PlayerPreference::Vlc,
            ..Settings::default()
        };
        let mut draft = preferences_draft(&settings);
        draft.catalog_enabled = !draft.catalog_enabled;
        let changed = settings_from_draft(settings, &draft).unwrap();
        assert_eq!(changed.default_player, PlayerPreference::Vlc);
    }

    #[test]
    fn proxy_draft_round_trips_trims_and_clears_without_parsing() {
        let settings = Settings {
            catalog_proxy: Some("socks5h://127.0.0.1:1080".into()),
            ..Settings::default()
        };
        let mut draft = preferences_draft(&settings);
        assert_eq!(draft.catalog_proxy, settings.catalog_proxy);
        assert_eq!(
            settings_from_draft(settings.clone(), &draft).unwrap(),
            settings
        );
        draft.catalog_proxy = Some("  https://proxy.example:8443 \n".into());
        assert_eq!(
            settings_from_draft(settings.clone(), &draft)
                .unwrap()
                .catalog_proxy
                .as_deref(),
            Some("https://proxy.example:8443")
        );
        for value in [None, Some(" \t ".into())] {
            draft.catalog_proxy = value;
            assert_eq!(
                settings_from_draft(settings.clone(), &draft)
                    .unwrap()
                    .catalog_proxy,
                None
            );
        }
        draft.catalog_proxy = Some("not a proxy".into());
        assert_eq!(
            settings_from_draft(settings, &draft).unwrap().catalog_proxy,
            draft.catalog_proxy
        );
    }

    #[test]
    fn source_rows_preserve_engine_kind_index_and_provenance() {
        let source = SourceCandidate {
            index: 7,
            recommended: true,
            possible_match: false,
            kind: SourceKind::Magnet,
            title: "Разрешённое видео".into(),
            quality: Some("720p".into()),
            video_codec: Some("VP9".into()),
            audio_codec: Some("Opus".into()),
            peers: Some(4),
            language: None,
            license: "CC0".into(),
            origin: "Публичный каталог".into(),
            size: None,
            seeds: None,
            rutracker: false,
        };
        let row = source_row(source);
        assert_eq!(row.index, 7);
        assert!(row.recommended);
        assert!(!row.possible_match);
        assert_eq!(row.kind, UiSourceKind::Magnet);
        assert_eq!(row.quality.as_deref(), Some("720p"));
        assert_eq!(
            (
                row.video_codec.as_deref(),
                row.audio_codec.as_deref(),
                row.peers
            ),
            (Some("VP9"), Some("Opus"), Some(4))
        );
        assert_eq!(row.language, None);
        assert_eq!(row.license.as_deref(), Some("CC0"));
        assert_eq!(row.origin, "Публичный каталог");
    }

    #[test]
    fn popcorn_rows_preserve_provider_and_upstream_without_special_playback_handles() {
        for origin in ["Popcorn API", "Popcorn API · YTS"] {
            let row = source_row(SourceCandidate {
                index: 53,
                recommended: false,
                possible_match: false,
                kind: SourceKind::Magnet,
                title: "Movie release".into(),
                quality: Some("1080p".into()),
                video_codec: None,
                audio_codec: None,
                language: None,
                license: String::new(),
                origin: origin.into(),
                size: Some(1_024),
                seeds: Some(8),
                peers: None,
                rutracker: false,
            });
            assert_eq!(row.index, 53);
            assert_eq!(row.origin, origin);
            assert_eq!(row.kind, UiSourceKind::Magnet);
            assert_eq!(row.license, None);
            assert!(!row.rutracker);
            assert!(!row.recommended);
        }
    }

    #[test]
    fn source_rows_preserve_possible_and_exact_matches_without_changing_handles() {
        for possible_match in [false, true] {
            let row = source_row(SourceCandidate {
                index: 42,
                recommended: false,
                possible_match,
                kind: SourceKind::Magnet,
                title: "Tracker release".into(),
                quality: None,
                video_codec: None,
                audio_codec: None,
                peers: None,
                language: None,
                license: String::new(),
                origin: "RuTracker".into(),
                size: Some(1_024),
                seeds: Some(8),
                rutracker: true,
            });
            assert_eq!(row.possible_match, possible_match);
            assert_eq!(row.index, 42);
            assert!(!row.recommended);
            assert!(row.rutracker);
            assert_eq!(row.seeds, Some(8));
        }
    }

    #[test]
    fn media_conversion_retains_episodes_license_and_public_attribution() {
        let card = media_card(Media {
            id: "test:series".into(),
            title: "Сериал".into(),
            original_title: None,
            year: None,
            description: "Описание".into(),
            rating_tenths: None,
            backdrop_url: None,
            poster_url: None,
            kind: MediaKind::Series,
            episodes: vec![kadr_engine::catalog::Episode {
                season: 2,
                number: 3,
                title: "Эпизод".into(),
                description: String::new(),
            }],
            attribution: "Автор".into(),
            license: Some("CC BY".into()),
            source_page: "https://example.invalid/public-item".into(),
            external: Default::default(),
        });
        assert_eq!(card.id, "test:series");
        assert_eq!(card.kind, kadr_ui::MediaKind::Series);
        assert_eq!(card.episodes[0].season, 2);
        assert_eq!(card.episodes[0].number, 3);
        assert!(card.attribution.contains("Автор"));
        assert!(card.attribution.contains("CC BY"));
        assert!(
            card.attribution
                .contains("https://example.invalid/public-item")
        );
        assert_eq!(card.year, None);
        assert_eq!(card.poster_url, None);
    }

    #[test]
    fn torrent_row_preserves_backend_index_and_episode_ambiguity() {
        let row = torrent_file(FileChoice {
            index: 9,
            name: "series.S02E03.mp4".into(),
            len: 1234,
            season: Some(2),
            episode: Some(3),
            ambiguous: true,
        });
        assert_eq!(row.index, 9);
        assert_eq!(row.size, 1234);
        assert_eq!((row.season, row.episode), (Some(2), Some(3)));
        assert!(row.ambiguous);
    }

    #[test]
    fn library_rows_keep_opaque_ids_and_do_not_invent_progress() {
        let row = library_row(LibraryItem {
            id: "test:series@2:3".into(),
            title: "Series".into(),
            poster_url: None,
            favorite: true,
            opened_at: Some(123),
            episode: Some(kadr_engine::LibraryEpisode {
                season: 2,
                number: 3,
            }),
            progress: None,
        });
        assert_eq!(row.id, "test:series@2:3");
        assert_eq!(row.details, "Сезон 2, серия 3");
        assert!(row.favorite);
        assert_eq!(row.opened_at, Some(123));
        assert_eq!(row.position, None);
        assert_eq!(row.duration, None);
        assert!(!row.completed);
    }

    #[test]
    fn library_rows_display_only_measured_progress() {
        let row = library_row(LibraryItem {
            id: "opaque-item".into(),
            title: "Movie".into(),
            poster_url: None,
            favorite: false,
            opened_at: None,
            episode: None,
            progress: Some(kadr_engine::LibraryProgress {
                position_seconds: 61.9,
                duration_seconds: Some(f64::NAN),
                completed: true,
            }),
        });
        assert_eq!(row.id, "opaque-item");
        assert_eq!(row.details, "Просмотр завершён");
        assert_eq!(row.position, Some(61));
        assert_eq!(row.duration, None);
        assert!(row.completed);
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_lossy_display_does_not_corrupt_native_cache_path() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/kadr-cache-\xff".to_vec(),
        ));
        let settings = Settings {
            cache_directory: Some(path.clone()),
            ..Settings::default()
        };
        let draft = preferences_draft(&settings);
        let changed = settings_from_draft(settings, &draft).unwrap();
        assert_eq!(changed.cache_directory, Some(path));
    }

    #[test]
    fn save_dialog_never_receives_a_path_url_or_query() {
        for input in [
            "",
            "https://host/video.mp4?token=secret",
            "magnet:?xt=secret",
            "..",
            "bad\nname",
        ] {
            assert_eq!(suggested_filename(input), "video.mp4");
        }
        assert_eq!(suggested_filename("/private/film.mp4"), "film.mp4");
        assert_eq!(suggested_filename(r"C:\private\film.mp4"), "film.mp4");
        assert_eq!(suggested_filename("film.mp4?secret#part"), "film.mp4");
    }

    #[test]
    fn telemetry_unknown_and_invalid_values_stay_unknown() {
        assert_eq!(seconds(f64::NAN), None);
        assert_eq!(seconds(f64::INFINITY), None);
        assert_eq!(seconds(-1.0), None);
        assert_eq!(seconds(0.0), Some(0));
        assert_eq!(seconds(61.9), Some(61));
        let transfer = transfer_view(TransferStatus {
            stage: "Получение метаданных".into(),
            download_bps: None,
            upload_bps: None,
            peers: None,
            done: None,
            total: None,
            buffered: None,
            saving: String::new(),
        });
        assert_eq!(transfer.download_bps, None);
        assert_eq!(transfer.peers, None);
        assert_eq!(transfer.total, None);
    }

    #[test]
    fn saved_local_video_is_not_labeled_as_http_or_torrent() {
        assert_eq!(source_kind(None), UiSourceKind::LocalFile);
        for player in PlayerChoice::ALL {
            assert_eq!(choice(preference(player)), player);
        }
    }
}
