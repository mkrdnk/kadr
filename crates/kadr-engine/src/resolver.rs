//! On-demand discovery. Providers own their transports; the engine owns
//! cancellation, concurrency and delivery. Nothing here opens a media stream.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures_util::{StreamExt, stream::FuturesUnordered};
use kadr_core::sources::{MediaQuery, MediaSource};
use tokio_util::sync::CancellationToken;

pub(crate) enum ProviderError {
    Timeout,
    Login(&'static str),
    /// Only redacted, adapter-generated diagnostics belong here.
    Failed(String),
}

impl ProviderError {
    pub fn message(&self) -> &str {
        match self {
            Self::Timeout => "Истекло время ожидания.",
            Self::Login(message) => message,
            Self::Failed(message) => message,
        }
    }
}

/// `had_results` survives filtering: an index with hits but no eligible
/// sources did not return an authoritative empty result.
#[derive(Clone, Default)]
pub(crate) struct ProviderBatch {
    pub rows: Vec<MediaSource>,
    pub had_results: bool,
    /// Ephemeral handoff only. Topic magnets can contain private tracker parameters.
    /// Never serialize these alongside the cacheable source descriptors.
    pub tracker_magnets: std::collections::BTreeMap<u64, String>,
    pub notice: Option<String>,
    pub from_cache: bool,
}

impl From<Vec<MediaSource>> for ProviderBatch {
    fn from(rows: Vec<MediaSource>) -> Self {
        Self {
            had_results: !rows.is_empty(),
            rows,
            ..Default::default()
        }
    }
}

type Offers = Result<ProviderBatch, ProviderError>;
pub(crate) type Search<'a> = Pin<Box<dyn Future<Output = Offers> + Send + 'a>>;

pub(crate) trait SourceProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn supports(&self, query: &MediaQuery) -> bool;
    fn validate(&self) -> Result<(), ProviderError>;
    /// Recheck provider-specific locator authority, including on cache reads.
    fn accepts(&self, _: &MediaSource) -> bool {
        true
    }
    fn timeout(&self) -> Duration;
    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a>;
}

#[derive(Default)]
pub(crate) struct ProviderRegistry(Vec<Arc<dyn SourceProvider>>);

impl ProviderRegistry {
    pub fn register(&mut self, provider: impl SourceProvider + 'static) {
        self.0.push(Arc::new(provider));
    }

    pub fn available(self, query: &MediaQuery) -> Vec<Arc<dyn SourceProvider>> {
        self.0.into_iter().filter(|p| p.supports(query)).collect()
    }
}

pub(crate) struct ProviderResult {
    pub id: String,
    pub name: String,
    pub result: Offers,
}

pub(crate) struct SourceResolver;

impl SourceResolver {
    pub async fn search(
        query: MediaQuery,
        providers: Vec<Arc<dyn SourceProvider>>,
        cancel: CancellationToken,
        mut deliver: impl FnMut(ProviderResult),
    ) {
        const CONCURRENT: usize = 4;
        let mut providers = providers.into_iter();
        let mut jobs = FuturesUnordered::new();
        loop {
            while jobs.len() < CONCURRENT {
                let Some(provider) = providers.next() else {
                    break;
                };
                jobs.push(run_provider(provider, &query, &cancel));
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                result = jobs.next() => match result {
                    Some(result) => deliver(result),
                    None => break,
                }
            }
        }
    }
}

async fn run_provider(
    provider: Arc<dyn SourceProvider>,
    query: &MediaQuery,
    cancel: &CancellationToken,
) -> ProviderResult {
    let result = match provider.validate() {
        Ok(()) => tokio::time::timeout(provider.timeout(), provider.search(query, cancel))
            .await
            .unwrap_or(Err(ProviderError::Timeout))
            .map(|mut batch| {
                batch.rows.retain(|offer| provider.accepts(offer));
                batch
            }),
        Err(error) => Err(error),
    };
    ProviderResult {
        id: provider.id().into(),
        name: provider.name().into(),
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::catalog::MediaKind;
    use tokio::sync::mpsc;

    struct Mock {
        id: &'static str,
        timeout: Duration,
        returns: Option<bool>,
        started: mpsc::UnboundedSender<&'static str>,
    }

    impl SourceProvider for Mock {
        fn id(&self) -> &str {
            self.id
        }
        fn name(&self) -> &str {
            self.id
        }
        fn supports(&self, _: &MediaQuery) -> bool {
            true
        }
        fn validate(&self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn timeout(&self) -> Duration {
            self.timeout
        }
        fn search<'a>(&'a self, _: &'a MediaQuery, _: &'a CancellationToken) -> Search<'a> {
            Box::pin(async {
                self.started.send(self.id).unwrap();
                match self.returns {
                    Some(true) => Ok(Vec::new().into()),
                    Some(false) => Err(ProviderError::Failed("offline".into())),
                    None => std::future::pending().await,
                }
            })
        }
    }

    fn movie() -> MediaQuery {
        MediaQuery {
            media_id: "trakt:movie:12".into(),
            kind: MediaKind::Movie,
            title: "Fixture".into(),
            original_title: None,
            year: Some(2020),
            external: Default::default(),
            episode: None,
        }
    }

    #[tokio::test]
    async fn ready_rows_and_errors_arrive_without_waiting_for_a_timed_out_peer() {
        let (started, _rx) = mpsc::unbounded_channel();
        let mut registry = ProviderRegistry::default();
        for (id, returns) in [
            ("slow", None),
            ("fast", Some(true)),
            ("offline", Some(false)),
        ] {
            registry.register(Mock {
                id,
                returns,
                timeout: Duration::from_millis(20),
                started: started.clone(),
            });
        }
        let query = movie();
        let providers = registry.available(&query);
        let mut received = Vec::new();
        SourceResolver::search(query, providers, CancellationToken::new(), |answer| {
            received.push((answer.id, answer.result));
        })
        .await;
        assert_eq!(received.len(), 3);
        assert!(
            received[..2]
                .iter()
                .any(|(id, result)| id == "fast" && result.is_ok())
        );
        assert!(
            received[..2]
                .iter()
                .any(|(id, result)| id == "offline"
                    && matches!(result, Err(ProviderError::Failed(_))))
        );
        assert!(matches!(
            received.last(),
            Some((_, Err(ProviderError::Timeout)))
        ));
    }

    #[tokio::test]
    async fn cancellation_drops_running_requests_and_does_not_start_queued_providers() {
        let (started, mut rx) = mpsc::unbounded_channel();
        let mut registry = ProviderRegistry::default();
        for id in ["a", "b", "c", "d", "queued"] {
            registry.register(Mock {
                id,
                returns: None,
                timeout: Duration::from_secs(60),
                started: started.clone(),
            });
        }
        let query = movie();
        let cancel = CancellationToken::new();
        let providers = registry.available(&query);
        let task = tokio::spawn(SourceResolver::search(
            query,
            providers,
            cancel.clone(),
            |_| panic!("stale callback"),
        ));
        for _ in 0..4 {
            assert_ne!(rx.recv().await, Some("queued"));
        }
        assert!(rx.try_recv().is_err());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(rx.try_recv().is_err());
    }
}
