//! Torznab configuration and authority remain behind the worker boundary.
//! Remote metainfo is deliberately not resumable after restart: its authenticated
//! tracker configuration is not stored in SQLite. Discovery caches only
//! sanitized descriptors returned by the adapter, never authenticated enclosures.

use super::*;
use kadr_catalog::torznab::{Client, Error};
use kadr_storage::TorznabConfig;

pub(super) struct Provider {
    id: String,
    name: String,
    client: Result<Client, &'static str>,
}

impl Provider {
    fn instance_id(config: &TorznabConfig) -> Option<String> {
        config
            .endpoint_identity()
            .ok()
            .map(|endpoint| format!("torznab:{:x}", Sha256::digest(endpoint.as_bytes())))
    }

    pub(super) fn new(config: &TorznabConfig, position: usize, proxy: Option<&str>) -> Self {
        let id = Self::instance_id(config).unwrap_or_else(|| format!("torznab-invalid-{position}"));
        let valid = config.validate();
        let name = if valid.is_ok() {
            config.name.clone()
        } else {
            format!("Torznab #{}", position + 1)
        };
        let client = valid
            .map_err(|_| "Некорректная конфигурация Torznab.")
            .and_then(|_| {
                let key = config
                    .api_key_env
                    .as_ref()
                    .map(|name| {
                        std::env::var(name)
                            .ok()
                            .filter(|value| !value.trim().is_empty())
                            .ok_or("API-ключ Torznab не задан в окружении.")
                    })
                    .transpose()?;
                Client::new(&config.base_url, key, proxy).map_err(
                    |_| "Не удалось создать клиент Torznab; проверьте конфигурацию и API-ключ.",
                )
            });
        Self { id, name, client }
    }

    fn client(&self) -> Result<&Client, ProviderError> {
        self.client
            .as_ref()
            .map_err(|message| ProviderError::Failed((*message).into()))
    }
}

impl SourceProvider for Provider {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn supports(&self, _: &MediaQuery) -> bool {
        true
    }
    fn validate(&self) -> Result<(), ProviderError> {
        self.client().map(|_| ())
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(45)
    }
    fn accepts(&self, source: &MediaSource) -> bool {
        source.provider == self.id
            && source.clone().normalize().is_ok()
            && self.client().is_ok_and(|client| client.accepts(source))
    }
    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            self.client()?
                .search(query, &self.id, now(), cancel)
                .await
                .map_err(provider_error)
        })
    }
}

fn provider_error(error: Error) -> ProviderError {
    let message = match error {
        Error::Timeout => return ProviderError::Timeout,
        Error::RateLimited => "Слишком много запросов. Повторите позже.",
        Error::Api(100..=102) | Error::Http(401 | 403) => {
            "Поставщик отклонил запрос. Проверьте API-ключ и права доступа."
        }
        Error::UnsupportedSearch => "Поставщик не поддерживает поиск для этого запроса.",
        Error::TooLarge | Error::InvalidResponse => {
            "Поставщик вернул слишком большой или некорректный ответ."
        }
        Error::Network => "Не удалось подключиться к поставщику. Проверьте адрес и настройки сети.",
        _ => return ProviderError::Failed(error.to_string()),
    };
    ProviderError::Failed(message.into())
}

impl Worker {
    pub(super) async fn select_torznab(
        &mut self,
        offer: MediaSource,
        url: &str,
        cancel: CancellationToken,
    ) {
        // Resolve the config again, never trust the config snapshot or locator
        // that originally produced a row (including a restored cache row).
        if !self.settings.catalog_enabled || !self.settings.network.torrents_enabled {
            self.source_error(
                Level::Error,
                "Для Torznab включите сетевой каталог и BitTorrent.",
            );
            return;
        }
        let configs = &self.source_preferences.torznab;
        let selected = configs.iter().enumerate().find(|(_, config)| {
            config.enabled
                && Provider::instance_id(config).as_deref() == Some(offer.provider.as_str())
        });
        let Some((position, config)) = selected else {
            self.source_error(
                Level::Error,
                "Поставщик Torznab больше недоступен; повторите поиск.",
            );
            return;
        };
        let mut provider = Provider::new(config, position, self.settings.catalog_proxy.as_deref());
        if !provider.accepts(&offer) {
            self.source_error(
                Level::Error,
                "Источник Torznab недействителен; повторите поиск.",
            );
            return;
        }
        let origin = kadr_core::sources::torznab_endpoint(&config.base_url).map(|url| url.origin());
        if configs.iter().enumerate().any(|(i, sibling)| {
            i != position
                && kadr_core::sources::torznab_endpoint(&sibling.base_url)
                    .is_some_and(|url| Some(url.origin()) == origin)
        }) && let Ok(client) = &mut provider.client
        {
            client.restrict_enclosure_key_to_api();
        }
        self.selection = None;
        self.send(Event::TorrentFiles(Vec::new()));
        self.send(Event::SourceSelected {
            kind: None,
            title: String::new(),
            playable: false,
            suggested_filename: String::new(),
        });
        self.send(Event::SourcesState(AsyncState::Loading));
        let result = async {
            let bytes = provider
                .client()?
                .metainfo(url, &cancel)
                .await
                .map_err(provider_error)?;
            let prepared = PreparedTorrent::from_bytes(bytes.into()).map_err(|_| {
                ProviderError::Failed("Torznab вернул некорректный файл .torrent.".into())
            })?;
            if offer
                .info_hash
                .as_ref()
                .is_some_and(|hash| hash != &prepared.info_hash)
            {
                return Err(ProviderError::Failed(
                    "Содержимое .torrent не соответствует выбранной раздаче.".into(),
                ));
            }
            Ok(prepared)
        }
        .await;
        if cancel.is_cancelled() {
            return;
        }
        match result {
            Ok(prepared) => {
                self.finish_selection(Selection {
                    origin: Origin::RemoteTorrent { prepared },
                    file_id: None,
                    title: "Torznab · BitTorrent".into(),
                    filename: "video.mp4".into(),
                    target: None,
                });
                self.status(Level::Info, "Метаданные Torznab получены. Для продолжения после перезапуска выберите источник заново; ключ и данные .torrent не сохраняются.");
            }
            Err(error) => self.source_error(Level::Error, error.message()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kadr_core::catalog::MediaKind;

    fn config(endpoint: &str) -> TorznabConfig {
        TorznabConfig {
            name: "Fixture".into(),
            base_url: endpoint.into(),
            api_key_env: None,
            enabled: true,
        }
    }

    #[test]
    fn identity_is_a_normalized_endpoint_not_a_name_or_key_reference() {
        let first = Provider::new(&config("https://EXAMPLE.org.:443/a/../%61pi///"), 0, None);
        let mut renamed = config("https://example.org/api");
        renamed.name = "Renamed".into();
        renamed.api_key_env = Some("INVALID_ENV_NAME".into());
        let second = Provider::new(&renamed, 7, None);
        assert_eq!(first.id(), second.id());
        assert!(first.validate().is_ok());
        assert!(second.validate().is_err());
        assert_ne!(
            first.id(),
            Provider::new(&config("https://example.org/other"), 0, None).id()
        );
        assert_ne!(
            first.id(),
            Provider::new(&config("https://other.example/api"), 0, None).id()
        );
        assert!(!first.id().contains("example"));
    }

    #[test]
    fn cache_and_selection_authorize_only_this_providers_safe_locators() {
        let provider = Provider::new(&config("https://example.org/api"), 0, None);
        let offer = |locator| {
            MediaSource::new(provider.id(), locator, "Fixture", MediaKind::Movie).unwrap()
        };
        assert!(provider.accepts(&offer(Locator::TorrentUrl(
            "https://example.org/download?id=1".into()
        ))));
        assert!(provider.accepts(&offer(Locator::Magnet(
            "magnet:?xt=urn%3Abtih%3A0123456789abcdef0123456789abcdef01234567".into()
        ))));
        for (index, locator) in [
            Locator::TorrentUrl("https://other.example/download?id=1".into()),
            Locator::TorrentUrl("http://example.org/download?id=1".into()),
            Locator::TorrentUrl("https://example.org:444/download?id=1".into()),
            Locator::TorrentUrl("https://example.org/download?APIKEY=secret".into()),
            Locator::TorrentUrl("https://example.org/download?access_token=secret".into()),
            Locator::TorrentUrl("https://example.org/download?sig=secret".into()),
            Locator::TorrentUrl("https://example.org/download?session=secret".into()),
            Locator::Http("https://example.org/video.mp4".into()),
            Locator::Magnet("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&tr=https://example.org/announce".into()),
            Locator::RuTracker(42),
        ].into_iter().enumerate() {
            assert!(!provider.accepts(&offer(locator)), "unsafe fixture {index}");
        }
        let mut foreign = offer(Locator::TorrentUrl(
            "https://example.org/download?id=1".into(),
        ));
        foreign.provider = "torznab:foreign".into();
        assert!(!provider.accepts(&foreign));
    }
}
