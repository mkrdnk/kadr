//! A single, card-scoped HTTP job. Its result can be joined by an explicit
//! Sources/Watch request. It cannot start a torrent or publish UI events.
use super::*;
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use std::time::Instant;

pub(super) type WarmResult =
    Shared<BoxFuture<'static, Result<Arc<ProviderBatch>, rutracker::Error>>>;

pub(super) struct MoviePrefetch {
    query: MediaQuery,
    cancel: CancellationToken,
    started: Instant,
    result: WarmResult,
}

impl MoviePrefetch {
    pub(super) fn matches(&self, query: &MediaQuery) -> bool {
        !self.cancel.is_cancelled()
            && self.query == *query
            && self.started.elapsed() < Duration::from_secs(300)
    }

    pub(super) fn result(&self) -> WarmResult {
        self.result.clone()
    }
}

impl Drop for MoviePrefetch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Worker {
    pub(in crate::worker) fn prefetch_movie(&mut self, id: &str, token: CancellationToken) {
        let Some(media) = self.media.as_ref() else {
            return;
        };
        if media.id != id
            || media.kind != MediaKind::Movie
            || !self.tracker_allowed()
            || self.active.is_some()
            || self.selection.is_some()
            || self.discovery.active()
        {
            return;
        }
        let Some(query) = MediaQuery::from_media(media, None) else {
            return;
        };
        if self
            .discovery
            .movie_prefetch
            .as_ref()
            .is_some_and(|p| p.matches(&query))
        {
            return;
        }

        // SQLite knows only topic descriptors. Even a cache hit needs to read
        // the attachment before we can hand a private magnet to the player.
        let cached = self
            .storage
            .source_cache_get(&tracker_cache_key(&query_key(&query)), "rutracker", now())
            .ok()
            .flatten()
            .and_then(|data| serde_json::from_slice::<Vec<MediaSource>>(&data).ok())
            .and_then(|sources| {
                let empty = sources.is_empty();
                let rows = matched(&query, "rutracker", sources);
                (empty || !rows.is_empty()).then_some(ProviderBatch {
                    from_cache: true,
                    ..rows.into()
                })
            });
        let account = self.tracker.clone();
        let prefs = rank_preferences(&self.source_preferences);
        let cancel = token.child_token();
        let stop = cancel.clone();
        let target = query.clone();
        let result = async move {
            let run = async {
                let mut batch = match cached {
                    Some(rows) => rows,
                    None => tracker_batch(account.search_quiet(&target, &stop).await?, &target),
                };
                resolve_attachments(&mut batch, &prefs, &account, &stop).await;
                Ok(Arc::new(batch))
            };
            tokio::select! {
                biased;
                () = stop.cancelled() => Err(rutracker::Error::Cancelled),
                outcome = tokio::time::timeout(Duration::from_secs(45), run) =>
                    outcome.unwrap_or(Err(rutracker::Error::Timeout)),
            }
        }
        .boxed()
        .shared();
        let drive = result.clone();
        self.discovery.movie_prefetch = Some(MoviePrefetch {
            query,
            cancel,
            started: Instant::now(),
            result,
        });
        self.catalog_jobs.spawn(async move {
            let _ = drive.await;
        });
    }
}

/// Resolve at most the first three matches, in the same confidence/seed order
/// as movie discovery. No per-shelf requests or preloading of media payloads.
async fn load_attachments<F, Fut>(
    batch: &mut ProviderBatch,
    prefs: &RankPreferences,
    cancel: &CancellationToken,
    mut lookup: F,
) where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, rutracker::Error>>,
{
    let candidates = rank_and_dedup(batch.rows.clone(), prefs);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    for candidate in candidates.into_iter().take(3) {
        let Locator::RuTracker(topic) = candidate.locator else {
            continue;
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            result = tokio::time::timeout_at(deadline, lookup(topic)) => result.unwrap_or(Err(rutracker::Error::Timeout)),
        };
        match result {
            Ok(Some(magnet)) => {
                batch.tracker_magnets.insert(topic, magnet);
            }
            Ok(None) => {}
            Err(rutracker::Error::Cancelled) => return,
            Err(error) => {
                // Keep search rows usable, but don't hammer other topics when
                // the site denies access. Explicit selection can retry later.
                let detail = tracker_error(error.into());
                batch.notice = Some(format!(
                    "Не удалось заранее проверить ссылку RuTracker. {}",
                    detail.message()
                ));
                break;
            }
        }
    }
}

pub(super) async fn resolve_attachments(
    batch: &mut ProviderBatch,
    prefs: &RankPreferences,
    account: &tracker::Account,
    cancel: &CancellationToken,
) {
    load_attachments(batch, prefs, cancel, |id| account.lookup_magnet(id, cancel)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    fn batch() -> ProviderBatch {
        let rows = (1..=5)
            .map(|id| {
                let mut row = MediaSource::new(
                    "rutracker",
                    Locator::RuTracker(id),
                    "Film [2020]",
                    MediaKind::Movie,
                )
                .unwrap();
                row.possible_match = id == 5;
                row.seeds = Some(id as u32);
                row
            })
            .collect::<Vec<_>>();
        rows.into()
    }

    #[tokio::test]
    async fn prefetch_resolves_only_three_ranked_topics_and_preserves_rows() {
        let mut batch = batch();
        let mut requests = Vec::new();
        load_attachments(
            &mut batch,
            &RankPreferences::default(),
            &CancellationToken::new(),
            |id| {
                requests.push(id);
                ready(Ok((id != 3).then(|| format!("test-only:{id}"))))
            },
        )
        .await;
        assert_eq!(requests, [4, 3, 2]);
        assert_eq!(batch.tracker_magnets.len(), 2);
        assert_eq!(batch.tracker_magnets[&4], "test-only:4");
        assert_eq!(batch.rows.len(), 5);
        assert!(batch.notice.is_none());
    }

    #[tokio::test]
    async fn denied_attachment_does_not_prompt_or_erase_preceding_hits() {
        let mut batch = batch();
        let mut calls = 0;
        load_attachments(
            &mut batch,
            &RankPreferences::default(),
            &CancellationToken::new(),
            |_| {
                calls += 1;
                ready(if calls == 1 {
                    Ok(Some("test-only".into()))
                } else {
                    Err(rutracker::Error::Http(403))
                })
            },
        )
        .await;
        assert_eq!(calls, 2);
        assert_eq!(batch.rows.len(), 5);
        assert_eq!(batch.tracker_magnets.len(), 1);
        let notice = batch.notice.unwrap();
        assert!(notice.contains("доступ запрещён"));
        assert!(!notice.contains("Войдите"));
    }

    #[tokio::test]
    async fn cancellation_drops_a_pending_lookup_without_reading_other_topics() {
        let mut batch = batch();
        let stop = CancellationToken::new();
        let prefs = RankPreferences::default();
        let mut calls = 0;
        let (started, received) = tokio::sync::oneshot::channel();
        let mut started = Some(started);
        tokio::join!(
            load_attachments(&mut batch, &prefs, &stop, |_| {
                calls += 1;
                started.take().unwrap().send(()).unwrap();
                pending()
            }),
            async {
                received.await.unwrap();
                stop.cancel();
            }
        );
        assert_eq!(calls, 1);
        assert!(batch.tracker_magnets.is_empty());
    }
}
