//! Local offers for an attachment already bound to the current playback target.
//! Discovery does not open URLs, read metainfo, or prepare torrents.

use std::time::Duration;

use kadr_core::{
    source::{ManualSource, SourceKind},
    sources::{Locator, MediaQuery, MediaSource},
};
use tokio_util::sync::CancellationToken;

use crate::resolver::{ProviderError, Search, SourceProvider};

/// Construct only from the current target's validated user attachment. These
/// offers must bypass both cache reads and writes; a locator is not authorization.
pub(super) struct ManualProvider {
    query: MediaQuery,
    id: &'static str,
    offer: MediaSource,
}

impl ManualProvider {
    /// `info_hash` may come from already prepared metainfo. This adapter never
    /// invokes the torrent backend to discover it.
    pub(super) fn new(
        query: MediaQuery,
        source: ManualSource,
        info_hash: Option<String>,
    ) -> Result<Self, ProviderError> {
        let kind = source.kind();
        let (id, locator) = match kind {
            SourceKind::Http => (
                "manual-http",
                Locator::Http(
                    source
                        .http_url()
                        .expect("validated HTTP source")
                        .to_string(),
                ),
            ),
            SourceKind::Magnet => (
                "manual-torrent",
                Locator::Magnet(
                    source
                        .magnet_uri()
                        .expect("validated magnet source")
                        .to_owned(),
                ),
            ),
            SourceKind::TorrentFile => (
                "manual-torrent",
                Locator::TorrentFile(
                    source
                        .torrent_path()
                        .expect("validated torrent path")
                        .to_owned(),
                ),
            ),
        };
        // The catalogue title is safe for display; never use a path, URL, magnet
        // display name or backend filename as a release title.
        let mut offer = MediaSource::new(id, locator, query.title.clone(), query.kind)
            .map_err(|_| invalid_attachment())?;
        offer.year = query.year;
        offer.episode = query.episode;
        offer.external = query.external.clone();
        offer.info_hash = info_hash;
        // Explicit binding, not inferred title/episode matching.
        offer.match_score = 100;
        let offer = offer.normalize().map_err(|_| invalid_attachment())?;
        Ok(Self { query, id, offer })
    }
}

fn invalid_attachment() -> ProviderError {
    ProviderError::Failed("Не удалось добавить привязанный источник.".into())
}

impl SourceProvider for ManualProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn name(&self) -> &str {
        match self.offer.locator {
            Locator::Http(_) => "Свой HTTP-источник",
            _ => "Свой торрент",
        }
    }

    fn supports(&self, query: &MediaQuery) -> bool {
        query == &self.query
    }

    fn validate(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() || !self.supports(query) {
                return Ok(Vec::new().into());
            }
            let mut offer = self.offer.clone();
            offer.fetched_at = super::now();
            Ok(vec![offer].into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::{
        catalog::{ExternalMetadata, MediaKind},
        torrent::EpisodeNumber,
    };

    fn query() -> MediaQuery {
        MediaQuery {
            media_id: "trakt:movie:42".into(),
            kind: MediaKind::Movie,
            title: "Название фильма".into(),
            original_title: None,
            year: None,
            external: ExternalMetadata::default(),
            episode: None,
        }
    }

    fn provider(query: MediaQuery, source: ManualSource, hash: Option<String>) -> ManualProvider {
        ManualProvider::new(query, source, hash).unwrap_or_else(|_| panic!("valid attachment"))
    }

    async fn offers(provider: &ManualProvider, query: &MediaQuery) -> Vec<MediaSource> {
        provider
            .search(query, &CancellationToken::new())
            .await
            .unwrap_or_else(|_| panic!("local search should succeed"))
            .rows
    }

    #[tokio::test]
    async fn http_attachment_is_an_explicit_binding_without_network_access() {
        // Reserved, unreachable address: resolution must not fetch this resource.
        let source =
            ManualSource::parse_link("http://192.0.2.1:9/private.mp4?token=secret").unwrap();
        let query = query();
        let provider = provider(query.clone(), source, None);
        assert_eq!(provider.id(), "manual-http");
        assert_eq!(provider.name(), "Свой HTTP-источник");
        assert!(provider.validate().is_ok());
        let rows = offers(&provider, &query).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.title, query.title);
        assert_eq!(row.year, None);
        assert_eq!(row.release_title, None);
        assert_eq!(row.match_score, 100);
        assert!(row.fetched_at > 0);
        assert_eq!(row.resolution, None);
        assert_eq!(row.seeds, None);
        assert_eq!(row.info_hash, None);
        assert!(!row.title.contains("private"));
        assert!(!row.title.contains("secret"));
    }

    #[tokio::test]
    async fn torrent_file_attachment_uses_prepared_hash_without_opening_file_or_leaking_path() {
        let path = std::env::temp_dir()
            .join("private-user")
            .join("never-created.torrent");
        let query = query();
        let source = ManualSource::torrent_file(path.clone()).unwrap();
        let hash = "A".repeat(40);
        let provider = provider(query.clone(), source, Some(hash.clone()));
        assert_eq!(provider.id(), "manual-torrent");
        assert_eq!(provider.name(), "Свой торрент");
        let rows = offers(&provider, &query).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.locator, Locator::TorrentFile(path));
        assert_eq!(row.info_hash, Some(hash.to_ascii_lowercase()));
        assert_eq!(row.title, query.title);
        assert_eq!(row.release_title, None);
        assert_eq!(row.match_score, 100);
        assert!(row.fetched_at > 0);
        assert!(!format!("{row:?}").contains("private-user"));
        assert!(!row.title.contains("never-created"));
    }

    #[tokio::test]
    async fn magnet_uses_torrent_provider_and_preserves_episode_binding() {
        let mut query = query();
        query.kind = MediaKind::Series;
        query.media_id = "trakt:show:42".into();
        query.episode = EpisodeNumber::new(2, 3);
        let hash = "b".repeat(40);
        let source = ManualSource::parse_link(&format!(
            "magnet:?xt=urn:btih:{hash}&dn=private-release-name"
        ))
        .unwrap();
        let provider = provider(query.clone(), source, None);
        let rows = offers(&provider, &query).await;
        assert_eq!(provider.id(), "manual-torrent");
        assert_eq!(rows[0].info_hash, Some(hash));
        assert_eq!(rows[0].episode, query.episode);
        assert_eq!(rows[0].title, query.title);
        assert_eq!(rows[0].match_score, 100);
        let mut next_episode = query.clone();
        next_episode.episode = EpisodeNumber::new(2, 4);
        assert!(!provider.supports(&next_episode));
        assert!(offers(&provider, &next_episode).await.is_empty());
    }

    #[tokio::test]
    async fn divergent_query_and_cancelled_search_do_not_return_attachment() {
        let query = query();
        let source = ManualSource::parse_link("https://example.org/video.mp4").unwrap();
        let provider = provider(query.clone(), source, None);
        let mut divergent = query.clone();
        divergent.media_id = "trakt:movie:43".into();
        assert!(!provider.supports(&divergent));
        assert!(offers(&provider, &divergent).await.is_empty());
        divergent = query.clone();
        divergent.title = "Other movie".into();
        assert!(!provider.supports(&divergent));
        assert!(offers(&provider, &divergent).await.is_empty());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            provider
                .search(&query, &cancel)
                .await
                .unwrap_or_else(|_| panic!("cancel"))
                .rows
                .is_empty()
        );
    }

    #[test]
    fn invalid_or_conflicting_prepared_hash_is_a_redacted_error() {
        let hash = "b".repeat(40);
        let source = ManualSource::parse_link(&format!("magnet:?xt=urn:btih:{hash}")).unwrap();
        for hash in ["private-user-path".into(), "a".repeat(40)] {
            let result = ManualProvider::new(query(), source.clone(), Some(hash));
            match result {
                Err(error) => {
                    assert_eq!(error.message(), "Не удалось добавить привязанный источник.")
                }
                Ok(_) => panic!("invalid hash accepted"),
            }
        }
    }
}
