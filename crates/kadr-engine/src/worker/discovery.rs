use std::collections::BTreeMap;

use super::*;
use crate::resolver::{
    ProviderError, ProviderRegistry, ProviderResult, Search, SourceProvider, SourceResolver,
};
use kadr_core::sources::{SourcePreferences as RankPreferences, match_source, rank_and_dedup};

mod commons;
mod manual;
mod torznab;

#[cfg(test)]
mod tests;

fn is_local(id: &str) -> bool {
    matches!(id, "manual-http" | "manual-torrent")
}

/// The current target owns the provider batches and the selectable rows.
/// Never recycle a row handle for a new target or a different provider.
#[derive(Default)]
pub(super) struct Discovery {
    next_handle: usize,
    current: Option<SearchState>,
}

struct SearchState {
    query: MediaQuery,
    key: String,
    pending: usize,
    succeeded: usize,
    failures: BTreeMap<String, String>,
    batches: Vec<MediaSource>,
    handles: BTreeMap<(String, String), usize>,
    ranked: Vec<(usize, MediaSource)>,
    names: BTreeMap<String, String>,
}

impl Discovery {
    pub(super) fn active(&self) -> bool {
        self.current.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.current = None;
    }

    fn rank(&mut self, prefs: &super::super::SourcePreferences) {
        let Some(state) = &mut self.current else {
            return;
        };
        let rank = RankPreferences {
            resolutions: prefs
                .preferred_resolution
                .as_deref()
                .and_then(|s| s.trim_end_matches('p').parse().ok())
                .into_iter()
                .collect(),
            languages: prefs.preferred_language.iter().cloned().collect(),
        };
        state.ranked = rank_and_dedup(state.batches.clone(), &rank)
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
                    .filter_map(|id| state.names.get(id))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut row = crate::SourceCandidate::from_source(*handle, source, origin);
                row.recommended = recommended == Some(*handle);
                row
            })
            .collect()
    }

    pub(super) fn recommended_handle(&self) -> Option<usize> {
        let state = self.current.as_ref()?;
        if state.pending != 0 || state.query.kind != MediaKind::Movie {
            return None;
        }
        state
            .ranked
            .iter()
            .find(|(_, source)| {
                source.is_discovered_movie_torrent() && source.seeds.is_some_and(|seeds| seeds > 0)
            })
            .map(|(handle, _)| *handle)
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
        })
    }
}

struct RuTrackerProvider {
    account: tracker::Account,
    vault: tracker::Vault,
    media: Media,
    episode: Option<Episode>,
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
    fn search<'a>(&'a self, _: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            let offers = self
                .account
                .search(&self.vault, &self.media, self.episode.as_ref(), cancel)
                .await
                .map_err(|error| match error {
                    tracker::PrepareError::Login(message) => ProviderError::Login(message),
                    tracker::PrepareError::Failed(message) => ProviderError::Failed(message),
                })?;
            Ok(offers
                .into_iter()
                .filter_map(|o| {
                    let mut source = MediaSource::new(
                        self.id(),
                        Locator::RuTracker(o.topic_id),
                        o.title,
                        self.media.kind,
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
                    // This adapter already matched title, production year and scope.
                    // A pack that contains the episode is not an exact episode hit.
                    source.match_score = if self.episode.is_some() { 65 } else { 80 };
                    Some(source)
                })
                .collect())
        })
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
            offer.origins = vec![provider.to_owned()];
            match (&offer.locator, provider) {
                (Locator::RuTracker(_), "rutracker") if offer.license.is_none() => {
                    if query.kind == MediaKind::Movie
                        && !kadr_catalog::rutracker::matches_movie_release(&offer.title, query)
                    {
                        return None;
                    }
                    // Cached scores are not eligibility evidence.
                    offer.match_score = if query.episode.is_some() { 65 } else { 80 };
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
            });
        }
        self.episode.clone()
    }

    fn registry(&self, media: &Media, query: &MediaQuery) -> ProviderRegistry {
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
                media: media.clone(),
                episode: self.catalog_episode(),
            });
        }
        if self.settings.catalog_enabled && self.settings.network.torrents_enabled {
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
        let providers = self.registry(media, &query).available(&query);
        self.discovery.clear();
        if providers.is_empty() {
            self.source_error(Level::Warning, "Нет включённых поставщиков для этого фильма или эпизода. Проверьте настройки источников или откройте свой файл или ссылку.");
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
            batches: Vec::new(),
            handles: BTreeMap::new(),
            ranked: Vec::new(),
            names: BTreeMap::new(),
        };
        let mut missing = Vec::new();
        for provider in providers {
            state
                .names
                .insert(provider.id().into(), provider.name().into());
            if !force && !is_local(provider.id()) && provider.validate().is_ok() {
                match self
                    .storage
                    .source_cache_get(&state.key, provider.id(), now())
                {
                    Ok(Some(data)) => {
                        if let Ok(offers) = serde_json::from_slice::<Vec<MediaSource>>(&data)
                            && offers.iter().all(|offer| provider.accepts(offer))
                        {
                            state.batches.extend(matched(&query, provider.id(), offers));
                            state.succeeded += 1;
                            continue;
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
        let Some(state) = &mut self.discovery.current else {
            return;
        };
        state.pending -= 1;
        let mut cache_failed = false;
        let mut login = None;
        match answer.result {
            Ok(offers) => {
                let rows = matched(&state.query, &answer.id, offers);
                if !is_local(&answer.id)
                    && let Ok(data) = serde_json::to_vec(&rows)
                {
                    cache_failed = self
                        .storage
                        .source_cache_put(&state.key, &answer.id, now(), rows.is_empty(), &data)
                        .is_err();
                }
                state.succeeded += 1;
                state.batches.extend(rows);
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
            });
            self.episode_explicit = true;
        }
        self.tracker_search.cancel();
        self.reset_tracker_prompt();
        match source.locator.clone() {
            Locator::RuTracker(id) => {
                self.select_tracker(tracker::SelectionRequest::new(id), cancel)
                    .await
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
