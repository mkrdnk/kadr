use std::collections::BTreeMap;

use super::*;
use crate::resolver::{
    ProviderBatch, ProviderError, ProviderRegistry, ProviderResult, Search, SourceProvider,
    SourceResolver,
};
use kadr_core::sources::{SourcePreferences as RankPreferences, match_source, rank_and_dedup};

mod commons;
mod manual;
mod movie_prefetch;
mod popcorn;
mod recommendation;
mod torznab;

#[cfg(test)]
mod tests;

fn allows_disk_cache(id: &str) -> bool {
    // Popcorn supplies complete magnet URIs, potentially with private parameters.
    !matches!(
        id,
        "manual-http"
            | "manual-torrent"
            | kadr_catalog::popcorn::ID
            | kadr_catalog::popcorn::YTS_ID
    )
}

/// The current target owns the provider batches and the selectable rows.
/// Never recycle a row handle for a new target or a different provider.
#[derive(Default)]
pub(super) struct Discovery {
    next_handle: usize,
    current: Option<SearchState>,
    movie_prefetch: Option<movie_prefetch::MoviePrefetch>,
    rank_preferences: RankPreferences,
    playback_request: Option<crate::PlaybackPreferences>,
}

struct SearchState {
    query: MediaQuery,
    key: String,
    pending: usize,
    succeeded: usize,
    failures: BTreeMap<String, String>,
    notices: Vec<String>,
    batches: Vec<MediaSource>,
    handles: BTreeMap<(String, String), usize>,
    ranked: Vec<(usize, MediaSource)>,
    names: BTreeMap<String, String>,
    tracker_magnets: BTreeMap<u64, String>,
}

impl SearchState {
    fn cache_key(&self, provider: &str) -> std::borrow::Cow<'_, str> {
        // Older RuTracker batches conflated empty, unreadable and filtered
        // results. Do not invalidate unrelated providers' successful batches.
        if provider == "rutracker" {
            tracker_cache_key(&self.key).into()
        } else {
            self.key.as_str().into()
        }
    }

    fn record_tracker_empty(&mut self, had_topics: bool) {
        self.notices.push(
            if had_topics {
                "RuTracker нашёл темы, но они не подходят для выбранного фильма или эпизода."
            } else {
                "RuTracker не нашёл тем по запрошенным названиям."
            }
            .into(),
        );
    }

    fn accept_live_batch(
        &mut self,
        provider: &str,
        batch: ProviderBatch,
        storage: &mut Storage,
    ) -> kadr_storage::Result<()> {
        let rows = matched(&self.query, provider, batch.rows);
        let tracker_empty = provider == "rutracker" && rows.is_empty();
        if tracker_empty {
            self.record_tracker_empty(batch.had_results);
        }
        let key = self.cache_key(provider);
        let cached = if !allows_disk_cache(provider) || batch.from_cache {
            Ok(())
        } else if tracker_empty && batch.had_results {
            // A refresh supersedes an old cached result, but filtered topics
            // must not become a cached 'no results'.
            storage.source_cache_remove(&key, provider)
        } else {
            serde_json::to_vec(&rows)
                .map_err(|_| kadr_storage::Error::InvalidInput("source cache payload"))
                .and_then(|data| {
                    storage.source_cache_put(&key, provider, now(), rows.is_empty(), &data)
                })
        };
        self.succeeded += 1;
        if provider == "rutracker" {
            self.tracker_magnets = batch.tracker_magnets;
        }
        self.notices.extend(batch.notice);
        self.batches.extend(rows);
        cached
    }
}

impl Discovery {
    pub(super) fn cancel_movie_prefetch(&mut self) {
        self.movie_prefetch = None;
        if let Some(state) = &mut self.current {
            state.tracker_magnets.clear();
        }
    }

    pub(super) fn active(&self) -> bool {
        self.current.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
        self.playback_request = None;
        self.cancel_movie_prefetch();
    }

    fn rank(&mut self, prefs: &super::super::SourcePreferences) {
        self.rank_preferences = rank_preferences(prefs);
        let Some(state) = &mut self.current else {
            return;
        };
        state.ranked = rank_and_dedup(state.batches.clone(), &self.rank_preferences)
            .into_iter()
            .map(|source| {
                let key = (source.provider.clone(), source.id.clone());
                let handle = *state.handles.entry(key).or_insert_with(|| {
                    self.next_handle = self
                        .next_handle
                        .checked_add(1)
                        .expect("source handle exhausted");
                    self.next_handle
                });
                (handle, source)
            })
            .collect();
    }

    pub(super) fn rows(&self) -> Vec<crate::SourceCandidate> {
        let Some(state) = &self.current else {
            return Vec::new();
        };
        let recommended = self.recommended_handle();
        state
            .ranked
            .iter()
            .map(|(handle, source)| {
                let origin = source
                    .origins
                    .iter()
                    .filter_map(|id| {
                        let name = state.names.get(id)?;
                        if id == kadr_catalog::popcorn::ID {
                            let labels = source
                                .origins
                                .iter()
                                .filter_map(|origin| kadr_catalog::popcorn::upstream_label(origin))
                                .collect::<Vec<_>>();
                            if !labels.is_empty() {
                                return Some(format!("{name} · {}", labels.join(", ")));
                            }
                        }
                        Some(name.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut row = crate::SourceCandidate::from_source(*handle, source, origin);
                row.recommended = recommended == Some(*handle);
                row
            })
            .collect()
    }

    pub(super) fn recommended_handle(&self) -> Option<usize> {
        self.recommendation(
            self.playback_request
                .as_ref()
                .unwrap_or(&crate::PlaybackPreferences::default()),
            |_| true,
        )
    }

    fn snapshot(&self, token: &CancellationToken) -> Option<Event> {
        let state = self.current.as_ref()?;
        Some(Event::SourceDiscovery {
            stamp: crate::DiscoveryStamp(token.clone()),
            media_id: state.query.media_id.clone(),
            episode: state.query.episode.map(|ep| (ep.season(), ep.episode())),
            rows: self.rows(),
            pending: state.pending,
            succeeded: state.succeeded,
            failures: state.failures.values().cloned().collect(),
            notices: state.notices.clone(),
        })
    }
}

struct RuTrackerProvider {
    account: tracker::Account,
    vault: tracker::Vault,
    warm: Option<movie_prefetch::WarmResult>,
    preferences: RankPreferences,
}

impl SourceProvider for RuTrackerProvider {
    fn id(&self) -> &str {
        "rutracker"
    }
    fn name(&self) -> &str {
        "RuTracker"
    }
    fn supports(&self, _: &MediaQuery) -> bool {
        true
    }
    fn validate(&self) -> Result<(), ProviderError> {
        Ok(())
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(45)
    }
    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            let search = if let Some(warm) = self.warm.clone() {
                match warm.await {
                    Ok(batch) => return Ok((*batch).clone()),
                    Err(error) => Err(error),
                }
            } else {
                self.account.search_quiet(query, cancel).await
            };
            let outcome = match search {
                // Only foreground searches may consult the credential vault.
                Err(rutracker::Error::AuthenticationRequired) => self
                    .account
                    .search_after_login_required(&self.vault, query, cancel)
                    .await
                    .map_err(tracker_error)?,
                other => other.map_err(|err| tracker_error(err.into()))?,
            };
            let mut batch = tracker_batch(outcome, query);
            if query.kind == MediaKind::Movie {
                movie_prefetch::resolve_attachments(
                    &mut batch,
                    &self.preferences,
                    &self.account,
                    cancel,
                )
                .await;
            }
            Ok(batch)
        })
    }
}

fn tracker_error(error: tracker::PrepareError) -> ProviderError {
    match error {
        tracker::PrepareError::Login(message) => ProviderError::Login(message),
        tracker::PrepareError::Failed(message) => ProviderError::Failed(message),
    }
}

fn tracker_cache_key(key: &str) -> String {
    format!("{key}:rt2")
}

fn rank_preferences(prefs: &crate::SourcePreferences) -> RankPreferences {
    RankPreferences {
        resolutions: prefs
            .preferred_resolution
            .as_deref()
            .and_then(crate::playback::preferred_height)
            .into_iter()
            .collect(),
        languages: prefs.preferred_language.iter().cloned().collect(),
    }
}

fn tracker_batch(outcome: rutracker::SearchOutcome, query: &MediaQuery) -> ProviderBatch {
    let rows = outcome
        .offers
        .into_iter()
        .filter_map(|o| {
            let mut source = MediaSource::new(
                "rutracker",
                Locator::RuTracker(o.topic_id),
                o.title,
                query.kind,
            )
            .ok()?;
            source.resolution = o
                .quality
                .as_deref()
                .and_then(|q| q.trim_end_matches(['p', 'i']).parse().ok());
            source.quality = o.quality;
            source.size = o.size;
            source.seeds = o.seeds;
            source.fetched_at = now();
            Some(source)
        })
        .collect();
    ProviderBatch {
        rows: matched(query, "rutracker", rows),
        had_results: outcome.had_topics,
        ..Default::default()
    }
}

fn query_key(query: &MediaQuery) -> String {
    // Older Torznab cache rows discarded contradictory secondary year claims.
    // They cannot be revalidated from the stored representative alone.
    let base = format!("resolve-v2:{}", query.media_id);
    match query.episode {
        Some(ep) => format!("{base}:{}:{}", ep.season(), ep.episode()),
        None => base,
    }
}

/// Also apply on cache reads: a stale or malformed local record is not a
/// capability to send a URL to a player or impersonate a different provider.
fn matched(query: &MediaQuery, provider: &str, offers: Vec<MediaSource>) -> Vec<MediaSource> {
    offers
        .into_iter()
        .take(100)
        .filter(|o| o.provider == provider && o.kind == query.kind)
        .filter_map(|mut offer| {
            offer.origins.retain(|origin| {
                provider == kadr_catalog::popcorn::ID
                    && kadr_catalog::popcorn::upstream_label(origin).is_some()
            });
            // Normalization adds the authoritative provider ID.
            match (&offer.locator, provider) {
                (Locator::RuTracker(_), "rutracker") if offer.license.is_none() => {
                    // Neither a cached score nor a seed count proves that the
                    // release belongs to the current movie/episode.
                    let evidence = rutracker::classify_release(&offer.title, query)?;
                    offer.possible_match = evidence == rutracker::ReleaseMatch::Possible;
                    offer.match_score = if offer.possible_match { 50 } else { 80 };
                    offer.normalize().ok()
                }
                (Locator::Http(_), "manual-http")
                | (Locator::Magnet(_) | Locator::TorrentFile(_), "manual-torrent") => {
                    offer.normalize().ok()
                }
                (Locator::TorrentFile(_), _) | (_, "manual-http" | "manual-torrent") => None,
                (Locator::RuTracker(_), _) | (_, "rutracker") => None,
                _ => match_source(query, offer),
            }
        })
        .collect()
}

impl Worker {
    pub(super) fn catalog_episode(&self) -> Option<Episode> {
        // A library snapshot can identify an episode even when the restored
        // torrent filename contains no episode marker.
        if let Some(key) = self
            .selection
            .as_ref()
            .and_then(|s| s.target.as_ref())
            .and_then(|t| t.episode)
        {
            return Some(Episode {
                season: key.season,
                number: key.episode,
                title: String::new(),
                description: String::new(),
                runtime_minutes: None,
                screenshot_url: None,
            });
        }
        self.episode.clone()
    }

    fn registry(&self, query: &MediaQuery) -> ProviderRegistry {
        let mut registry = ProviderRegistry::default();
        if self.settings.catalog_enabled && self.source_preferences.commons_enabled {
            registry.register(commons::Provider {
                proxy: self.settings.catalog_proxy.clone(),
            });
        }
        let target = PlaybackTarget {
            media_id: MediaId::new(query.media_id.clone()).expect("validated Trakt ID"),
            episode: query.episode.map(|ep| EpisodeKey {
                season: ep.season(),
                episode: ep.episode(),
            }),
        };
        let current = self
            .selection
            .as_ref()
            .and_then(|selection| match &selection.origin {
                Origin::Manual { source, prepared }
                    if self.target(selection).ok().as_ref() == Some(&target) =>
                {
                    Some((
                        source.clone(),
                        prepared.as_ref().map(|p| p.info_hash.clone()),
                    ))
                }
                _ => None,
            });
        let local = current.or_else(|| match self.storage.source_for_target(&target) {
            Ok(Some(source)) => source.manual_source().ok().map(|s| (s, None)),
            Ok(None) => None,
            Err(_) => {
                self.status(Level::Warning, "Не удалось прочитать сохранённый источник.");
                None
            }
        });
        if let Some((input, hash)) = local
            && let Ok(provider) = manual::ManualProvider::new(query.clone(), input, hash)
        {
            registry.register(provider);
        }
        if self.tracker_allowed() {
            registry.register(RuTrackerProvider {
                account: self.tracker.clone(),
                vault: self.tracker_vault.clone(),
                preferences: rank_preferences(&self.source_preferences),
                warm: self
                    .discovery
                    .movie_prefetch
                    .as_ref()
                    .filter(|p| p.matches(query))
                    .map(|p| p.result()),
            });
        }
        if self.settings.catalog_enabled && self.settings.network.torrents_enabled {
            if self.source_preferences.popcorn_enabled {
                for index in [
                    kadr_catalog::popcorn::Index::Popcorn,
                    kadr_catalog::popcorn::Index::Yts,
                ] {
                    registry.register(popcorn::Provider {
                        index,
                        proxy: self.settings.catalog_proxy.clone(),
                    });
                }
            }
            for (position, config) in self.source_preferences.torznab.iter().enumerate() {
                if config.enabled {
                    registry.register(torznab::Provider::new(
                        config,
                        position,
                        self.settings.catalog_proxy.as_deref(),
                    ));
                }
            }
        }
        registry
    }

    pub(super) fn resolve_sources(&mut self, token: CancellationToken, force: bool) {
        let Some(media) = &self.media else {
            self.source_error(
                Level::Warning,
                "Сначала откройте карточку фильма или эпизода.",
            );
            return;
        };
        let Some(query) = MediaQuery::from_media(media, self.catalog_episode().as_ref()) else {
            self.source_error(Level::Warning, "Для поиска откройте фильм из Trakt или выберите эпизод сериала. Свой источник можно открыть без каталога.");
            return;
        };
        let key = query_key(&query);
        if !force
            && !self.tracker_search.is_cancelled()
            && self
                .discovery
                .current
                .as_ref()
                .is_some_and(|s| s.key == key && s.pending > 0)
        {
            self.publish_discovery();
            return;
        }
        self.tracker_search.cancel();
        if force {
            self.discovery.cancel_movie_prefetch();
        }
        let providers = self.registry(&query).available(&query);
        let popcorn_missing_id = self.source_preferences.popcorn_enabled
            && self.settings.catalog_enabled
            && self.settings.network.torrents_enabled
            && query.external.imdb_id.is_none();
        let imdb_notice = "В карточке нет IMDb ID для поиска по киноиндексам. Откройте карточку в каталоге Trakt или выберите другой источник.";
        let mut previous_magnets = self
            .discovery
            .current
            .take()
            .filter(|state| !force && state.query == query)
            .map(|state| state.tracker_magnets)
            .unwrap_or_default();
        if providers.is_empty() {
            self.source_error(Level::Warning, if popcorn_missing_id { imdb_notice } else {
                "Нет включённых поставщиков для этого фильма или эпизода. Проверьте настройки источников или откройте свой файл или ссылку."
            });
            return;
        }
        // Keep an explicitly attached manual input available on subsequent
        // refreshes even before the user has played (and persisted) it.
        if !matches!(
            self.selection.as_ref().map(|s| &s.origin),
            Some(Origin::Manual { .. })
        ) {
            self.selection = None;
        }
        self.send(Event::TorrentFiles(Vec::new()));
        self.send(Event::SourceSelected {
            kind: None,
            title: String::new(),
            playable: false,
            suggested_filename: String::new(),
        });
        let mut state = SearchState {
            query: query.clone(),
            key,
            pending: 0,
            succeeded: 0,
            failures: BTreeMap::new(),
            notices: popcorn_missing_id
                .then(|| imdb_notice.to_owned())
                .into_iter()
                .collect(),
            batches: Vec::new(),
            handles: BTreeMap::new(),
            ranked: Vec::new(),
            names: BTreeMap::new(),
            tracker_magnets: BTreeMap::new(),
        };
        let mut missing = Vec::new();
        for provider in providers {
            state
                .names
                .insert(provider.id().into(), provider.name().into());
            let uses_prefetch = provider.id() == "rutracker"
                && self
                    .discovery
                    .movie_prefetch
                    .as_ref()
                    .is_some_and(|p| p.matches(&query));
            if !force
                && !uses_prefetch
                && allows_disk_cache(provider.id())
                && provider.validate().is_ok()
            {
                match self.storage.source_cache_get(
                    &state.cache_key(provider.id()),
                    provider.id(),
                    now(),
                ) {
                    Ok(Some(data)) => {
                        if let Ok(offers) = serde_json::from_slice::<Vec<MediaSource>>(&data)
                            && offers.iter().all(|offer| provider.accepts(offer))
                        {
                            let was_empty = offers.is_empty();
                            let rows = matched(&query, provider.id(), offers);
                            // If a non-empty batch no longer matches updated
                            // card metadata, perform a fresh search.
                            if provider.id() != "rutracker" || was_empty || !rows.is_empty() {
                                if provider.id() == "rutracker" && was_empty {
                                    state.record_tracker_empty(false);
                                }
                                if provider.id() == "rutracker" {
                                    // The descriptor cache never contains private links. Reuse
                                    // only links that this card already resolved for these IDs.
                                    for row in &rows {
                                        if let Locator::RuTracker(id) = row.locator
                                            && let Some(magnet) = previous_magnets.remove(&id)
                                        {
                                            state.tracker_magnets.insert(id, magnet);
                                        }
                                    }
                                }
                                state.batches.extend(rows);
                                state.succeeded += 1;
                                continue;
                            }
                        }
                    }
                    Err(_) => self.status(
                        Level::Warning,
                        "Кэш источников недоступен; выполняется сетевой поиск.",
                    ),
                    Ok(None) => {}
                }
            }
            missing.push(provider);
        }
        self.tracker_search = token.child_token();
        state.pending = missing.len();
        self.discovery.current = Some(state);
        self.discovery.playback_request = None;
        self.discovery.rank(&self.source_preferences);
        self.publish_discovery();
        if !missing.is_empty() {
            let cancel = self.tracker_search.clone();
            let output = self.catalog_results.clone();
            self.catalog_jobs.spawn(async move {
                SourceResolver::search(query, missing, cancel.clone(), |result| {
                    let _ = output.send(CatalogResult {
                        cancel: cancel.clone(),
                        answer: CatalogAnswer::Sources(result),
                    });
                })
                .await;
            });
        }
    }

    fn publish_discovery(&self) {
        if let Some(event) = self.discovery.snapshot(&self.tracker_search) {
            self.send(event);
        }
    }

    pub(super) fn provider_answer(&mut self, answer: ProviderResult) {
        if answer.id == "rutracker" {
            // The foreground receiver now owns the result, including any successful
            // login after an unauthenticated prefetch. Never replay the old failure.
            self.discovery.movie_prefetch = None;
        }
        let Some(state) = &mut self.discovery.current else {
            return;
        };
        state.pending -= 1;
        let mut cache_failed = false;
        let mut login = None;
        match answer.result {
            Ok(batch) => {
                cache_failed = state
                    .accept_live_batch(&answer.id, batch, &mut self.storage)
                    .is_err();
            }
            Err(error) => {
                state
                    .failures
                    .insert(answer.id, format!("{}: {}", answer.name, error.message()));
                if let ProviderError::Login(message) = error {
                    login = Some(message);
                }
            }
        }
        self.discovery.rank(&self.source_preferences);
        self.publish_discovery();
        if cache_failed {
            self.status(Level::Warning, "Не удалось сохранить результаты в кэше.");
        }
        if let Some(message) = login {
            self.request_tracker_discovery_login(message);
        }
    }

    pub(super) fn set_source_preferences(&mut self, preferences: crate::SourcePreferences) {
        self.tracker_search.cancel();
        self.discovery.clear();
        self.reset_tracker_prompt();
        self.publish_source_offers();
        if self.storage.save_source_preferences(&preferences).is_ok() {
            self.source_preferences = preferences;
            self.status(Level::Info, "Настройки источников сохранены.");
        } else {
            self.status(Level::Error, "Настройки источников не сохранены. Проверьте адреса, названия, язык, разрешение и имена переменных API-ключей.");
        }
        self.send(Event::SourcePreferences(self.source_preferences.clone()));
    }

    pub(super) fn can_choose_offer(&self, index: usize) -> bool {
        let Some(query) = self
            .media
            .as_ref()
            .and_then(|media| MediaQuery::from_media(media, self.catalog_episode().as_ref()))
        else {
            return false;
        };
        let Some(state) = &self.discovery.current else {
            return false;
        };
        state.query == query
            && state.ranked.iter().any(|(handle, source)| {
                *handle == index
                    && self
                        .registry(&query)
                        .available(&query)
                        .iter()
                        .any(|provider| {
                            provider.id() == source.provider
                                && provider.validate().is_ok()
                                && provider.accepts(source)
                        })
            })
    }

    pub(super) async fn choose_offer(&mut self, index: usize, cancel: CancellationToken) {
        let source = self
            .discovery
            .current
            .as_ref()
            .and_then(|s| s.ranked.iter().find(|(handle, _)| *handle == index))
            .map(|(_, s)| s.clone());
        let Some(source) = source else {
            self.source_error(Level::Error, "Источник больше недоступен; повторите поиск.");
            return;
        };
        if let Some(episode) = self
            .discovery
            .current
            .as_ref()
            .and_then(|s| s.query.episode)
        {
            self.episode = Some(Episode {
                season: episode.season(),
                number: episode.episode(),
                title: String::new(),
                description: String::new(),
                runtime_minutes: None,
                screenshot_url: None,
            });
            self.episode_explicit = true;
        }
        self.tracker_search.cancel();
        self.reset_tracker_prompt();
        match source.locator.clone() {
            Locator::RuTracker(id) => {
                let request = self
                    .discovery
                    .current
                    .as_ref()
                    .and_then(|s| s.tracker_magnets.get(&id))
                    .map_or_else(
                        || tracker::SelectionRequest::new(id),
                        |magnet| tracker::SelectionRequest::with_magnet(id, magnet.clone()),
                    );
                self.select_tracker(request, cancel).await
            }
            Locator::Magnet(_)
                if matches!(
                    source.provider.as_str(),
                    kadr_catalog::popcorn::ID | kadr_catalog::popcorn::YTS_ID
                ) =>
            {
                self.select_popcorn(&source, cancel).await
            }
            Locator::Http(uri) | Locator::Magnet(uri) => match ManualSource::parse_link(&uri) {
                Ok(source) => self.select_source(source, None, None, cancel).await,
                Err(_) => self.source_error(Level::Error, "Некорректный URI источника."),
            },
            Locator::TorrentFile(path) => match ManualSource::torrent_file(path) {
                Ok(source) => self.select_source(source, None, None, cancel).await,
                Err(_) => self.source_error(Level::Error, "Некорректный путь к файлу торрента."),
            },
            Locator::TorrentUrl(url) => self.select_torznab(source, &url, cancel).await,
        }
    }
}
