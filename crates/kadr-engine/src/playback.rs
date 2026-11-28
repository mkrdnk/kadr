use std::{io, path::PathBuf, sync::Arc, time::Duration};

use kadr_cache::{CacheLease, CacheManager};
use kadr_core::source::ManualSource;
use kadr_media::{ByteSource, LocalFile, RemoteSource, Server, ServerOptions};
use kadr_storage::{RateLimit, Settings};
use kadr_torrent::engine::{EngineOptions, PlaybackTorrent, PreparedTorrent};
use tokio_util::sync::CancellationToken;

/// Parse a declared pixel height, not a codec or an arbitrary player argument.
pub(crate) fn preferred_height(value: &str) -> Option<u16> {
    let value = value.strip_suffix(['p', 'i']).unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u16>().ok())
        .flatten()
        .filter(|height| (1..=16_384).contains(height))
}

pub(crate) fn preferred_audio_language(value: &str) -> Option<String> {
    if value.len() > 35 {
        return None;
    }
    let mut parts = value.split('-');
    let primary = parts.next()?;
    if !(2..=3).contains(&primary.len())
        || !primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        || !parts.all(|part| {
            !part.is_empty()
                && part.len() <= 8
                && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return None;
    }
    // Players accept ISO 639 language preferences, not a promise of a
    // particular regional variant, track index, codec, or dubbing.
    Some(primary.to_ascii_lowercase())
}

pub(crate) fn validate_preferences(preferences: &crate::PlaybackPreferences) -> io::Result<()> {
    if preferences
        .preferred_resolution
        .as_deref()
        .is_some_and(|value| preferred_height(value).is_none())
        || preferences
            .preferred_language
            .as_deref()
            .is_some_and(|value| preferred_audio_language(value).is_none())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Некорректные предпочтения качества или языка.",
        ));
    }
    Ok(())
}

/// Direct-player capabilities only. Browser playback deliberately has no
/// corresponding options. Legacy free-text stored labels are not CLI input.
pub(crate) fn player_preferences(
    preferences: &crate::PlaybackPreferences,
    stored: &crate::SourcePreferences,
) -> io::Result<kadr_media::players::PlayerPlaybackOptions> {
    validate_preferences(preferences)?;
    Ok(kadr_media::players::PlayerPlaybackOptions {
        subtitles_off: preferences.subtitles_off,
        audio_language: preferences
            .preferred_language
            .as_deref()
            .or(stored.preferred_language.as_deref())
            .and_then(preferred_audio_language),
    })
}

pub(super) fn engine_options(settings: &Settings) -> io::Result<EngineOptions> {
    fn rate(limit: RateLimit) -> io::Result<kadr_torrent::engine::RateLimit> {
        Ok(match limit {
            RateLimit::Paused => kadr_torrent::engine::RateLimit::Paused,
            RateLimit::Unlimited => kadr_torrent::engine::RateLimit::Unlimited,
            RateLimit::BytesPerSecond(value) if value >= 16_384 => {
                kadr_torrent::engine::RateLimit::Bps(value.try_into().map_err(|_| {
                    io::Error::other("Лимит торрента превышает поддерживаемый диапазон.")
                })?)
            }
            RateLimit::BytesPerSecond(_) => {
                return Err(io::Error::other(
                    "Минимальный числовой лимит BitTorrent — 16 384 байт/с. Для остановки выберите paused.",
                ));
            }
        })
    }
    Ok(EngineOptions {
        network_enabled: settings.network.torrents_enabled,
        discovery_enabled: true,
        initial_peers: Vec::new(),
        download_limit: rate(settings.network.download)?,
        upload_limit: rate(settings.network.upload)?,
        peer_limit: usize::from(settings.network.max_connections),
    })
}

pub(super) struct Stream {
    pub url: String,
    pub bytes: Option<Arc<dyn ByteSource>>,
    pub torrent: Option<PlaybackTorrent>,
    pub total: Option<u64>,
    pub proxy_warning: bool,
    http: Option<Server>,
    // The reservation outlives HTTP, torrent tasks and any save operation.
    _lease: Option<CacheLease>,
    cancel: CancellationToken,
}

impl Stream {
    pub async fn http(source: &ManualSource, cancel: CancellationToken) -> io::Result<Self> {
        let url = source
            .http_url()
            .ok_or_else(|| io::Error::other("Ожидался HTTP-источник."))?
            .as_str()
            .to_owned();
        let remote = match RemoteSource::open(&url, cancel.clone()).await {
            Ok(remote) => {
                // A successful HEAD does not prove Range support. Probe before
                // exposing a proxy that would otherwise fail on its first GET.
                match remote.read_at(0, 1).await {
                    Ok(_) => Ok(remote),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        match remote {
            Ok(remote) => {
                let mime = remote
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                let mime = if mime == "application/octet-stream" {
                    mime_for_name(source.http_url().expect("validated HTTP").path()).to_owned()
                } else {
                    mime
                };
                Self::serve(Arc::new(remote), None, None, cancel, &mime).await
            }
            // Direct HTTP remains supported even when the origin has no stable
            // validators/Range. Browser and Save cannot invent a safe proxy.
            Err(_) => Ok(Self {
                url,
                bytes: None,
                torrent: None,
                total: None,
                proxy_warning: true,
                http: None,
                _lease: None,
                cancel,
            }),
        }
    }

    pub async fn saved(path: PathBuf, cancel: CancellationToken) -> io::Result<Self> {
        let mime = mime_for_name(&path.to_string_lossy());
        let source = LocalFile::open(path).await.map_err(|_| {
            io::Error::other("Сохранённый файл недоступен. Выберите другой источник.")
        })?;
        Self::serve(Arc::new(source), None, None, cancel, mime).await
    }

    pub async fn torrent(
        prepared: &PreparedTorrent,
        file_id: usize,
        cache: &CacheManager,
        options: EngineOptions,
        cancel: CancellationToken,
    ) -> io::Result<Self> {
        let lease = cache
            .reserve(&prepared.info_hash, prepared.total_bytes)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let torrent = prepared
            .start(file_id, lease.path().to_owned(), options, cancel.clone())
            .await
            .map_err(|_| {
                io::Error::other(
                    "Не удалось запустить торрент. Проверьте сеть, лимиты и доступное место.",
                )
            })?;
        let mime = prepared
            .files
            .iter()
            .find(|file| file.id == file_id)
            .map(|file| mime_for_name(&file.relative_filename.to_string_lossy()))
            .unwrap_or("application/octet-stream");
        let bytes = torrent.source.clone();
        match Self::serve(bytes, Some(torrent), Some(lease), cancel, mime).await {
            Ok(stream) => Ok(stream),
            Err(error) => Err(error),
        }
    }

    async fn serve(
        source: Arc<dyn ByteSource>,
        torrent: Option<PlaybackTorrent>,
        lease: Option<CacheLease>,
        cancel: CancellationToken,
        mime: &str,
    ) -> io::Result<Self> {
        let server = match Server::start_with_content_type(
            source.clone(),
            ServerOptions {
                read_timeout: Duration::from_secs(30),
            },
            mime,
        )
        .await
        {
            Ok(server) => server,
            Err(error) => {
                if let Some(torrent) = &torrent {
                    torrent.stop().await;
                }
                return Err(error);
            }
        };
        Ok(Self {
            url: server.private_url(),
            total: Some(source.len()),
            bytes: Some(source),
            torrent,
            http: Some(server),
            _lease: lease,
            cancel,
            proxy_warning: false,
        })
    }

    pub async fn stop(&mut self) {
        self.cancel.cancel();
        if let Some(http) = self.http.take() {
            let _ = http.shutdown().await;
        }
        if let Some(torrent) = self.torrent.take() {
            torrent.stop().await;
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(super) fn mime_for_name(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" | "ogg" => "video/ogg",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_player_preferences_use_only_validated_language_and_subtitle_capabilities() {
        let stored = crate::SourcePreferences {
            preferred_language: Some("RU".into()),
            ..Default::default()
        };
        let inherited =
            player_preferences(&crate::PlaybackPreferences::default(), &stored).unwrap();
        assert_eq!(inherited.audio_language.as_deref(), Some("ru"));
        assert!(!inherited.subtitles_off);
        let explicit = player_preferences(
            &crate::PlaybackPreferences {
                preferred_resolution: Some("1080p".into()),
                preferred_language: Some("EN-us".into()),
                subtitles_off: true,
            },
            &stored,
        )
        .unwrap();
        assert_eq!(explicit.audio_language.as_deref(), Some("en"));
        assert!(explicit.subtitles_off);
        // Storage historically accepts labels; those labels are not CLI options.
        let stored = crate::SourcePreferences {
            preferred_language: Some("Russian --script=unsafe".into()),
            ..Default::default()
        };
        assert!(
            player_preferences(&crate::PlaybackPreferences::default(), &stored)
                .unwrap()
                .audio_language
                .is_none()
        );
    }

    #[test]
    fn invalid_request_preferences_fail_before_any_playback() {
        for language in [
            "",
            "--script=x",
            "en,ru",
            "ru\n",
            "r",
            "English",
            "en--US",
            "en_US",
        ] {
            let preferences = crate::PlaybackPreferences {
                preferred_language: Some(language.into()),
                ..Default::default()
            };
            assert!(validate_preferences(&preferences).is_err(), "{language:?}");
        }
        for resolution in ["0p", "1080p --script=x", "65535p", "-1", "4K", ""] {
            let preferences = crate::PlaybackPreferences {
                preferred_resolution: Some(resolution.into()),
                ..Default::default()
            };
            assert!(
                validate_preferences(&preferences).is_err(),
                "{resolution:?}"
            );
        }
        for resolution in ["720", "1080p", "2160p", "1080i"] {
            assert!(preferred_height(resolution).is_some(), "{resolution}");
        }
    }
}
