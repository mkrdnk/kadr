//! Popcorn and YTS are independent indexes, not metainfo servers.
//! Only a user-initiated source search may send the card's IMDb ID.
use super::*;
use kadr_catalog::popcorn as api;

pub(super) struct Provider {
    pub index: api::Index,
    pub proxy: Option<String>,
}

impl SourceProvider for Provider {
    fn id(&self) -> &str {
        self.index.id()
    }
    fn name(&self) -> &str {
        self.index.name()
    }
    fn supports(&self, query: &MediaQuery) -> bool {
        self.index.supports(query)
    }
    fn validate(&self) -> Result<(), ProviderError> {
        if self
            .proxy
            .as_deref()
            .is_none_or(kadr_core::proxy::is_valid_endpoint)
        {
            Ok(())
        } else {
            Err(ProviderError::Failed(
                "Проверьте адрес прокси каталога.".into(),
            ))
        }
    }
    fn timeout(&self) -> Duration {
        Duration::from_secs(21)
    }
    fn accepts(&self, offer: &MediaSource) -> bool {
        self.index.accepts(offer)
    }
    fn search<'a>(&'a self, query: &'a MediaQuery, cancel: &'a CancellationToken) -> Search<'a> {
        Box::pin(async move {
            api::Client::new(self.proxy.as_deref())
                .map_err(provider_error)?
                .search_index(self.index, query, now(), cancel)
                .await
                .map(Into::into)
                .map_err(provider_error)
        })
    }
}

fn provider_error(error: api::Error) -> ProviderError {
    let message = match error {
        api::Error::Timeout => return ProviderError::Timeout,
        api::Error::Http(401 | 403) => "Сервер запретил доступ к индексу.",
        api::Error::Http(300..=399) => "Индекс перенаправил запрос на неподдерживаемый адрес.",
        api::Error::RateLimited => "Сервер ограничил частоту запросов. Повторите позже.",
        api::Error::Network => "Нет соединения с индексом. Проверьте сеть и прокси каталога.",
        api::Error::Http(status) => {
            return ProviderError::Failed(format!("Индекс вернул HTTP {status}."));
        }
        api::Error::InvalidResponse | api::Error::TooLarge => "Не удалось прочитать список раздач.",
        _ => return ProviderError::Failed(error.to_string()),
    };
    ProviderError::Failed(message.into())
}

impl Worker {
    pub(super) async fn select_popcorn(&mut self, offer: &MediaSource, cancel: CancellationToken) {
        self.selection = None;
        self.send(Event::TorrentFiles(Vec::new()));
        self.send(Event::SourceSelected {
            kind: None,
            title: String::new(),
            playable: false,
            suggested_filename: String::new(),
        });
        if !self.settings.catalog_enabled
            || !self.settings.network.torrents_enabled
            || !self.source_preferences.popcorn_enabled
        {
            self.source_error(
                Level::Error,
                "Доступ к индексу выключен. Проверьте настройки и обновите источники.",
            );
            return;
        }
        let index = if offer.provider == api::YTS_ID {
            api::Index::Yts
        } else {
            api::Index::Popcorn
        };
        let uri = match &offer.locator {
            Locator::Magnet(uri) if index.accepts(offer) => uri,
            _ => {
                self.source_error(Level::Error, "Некорректная ссылка на выбранную раздачу.");
                return;
            }
        };
        let options = match self.torrent_options() {
            Ok(options) => options,
            Err(error) => {
                self.source_error(Level::Error, &error.to_string());
                return;
            }
        };
        self.send(Event::SourcesState(AsyncState::Loading));
        let result = tokio::time::timeout(
            Duration::from_secs(45),
            PreparedTorrent::from_magnet(uri, options, cancel.clone()),
        )
        .await;
        if cancel.is_cancelled() {
            return;
        }
        let message = match result {
            Ok(Ok(prepared)) if offer.info_hash.as_deref() == Some(prepared.info_hash.as_str()) => {
                let file_id = match offer.episode_file_hint.as_deref() {
                    Some(hint) => match unique_video_hint(&prepared.files, hint, offer.episode) {
                        Some(id) => Some(id),
                        None => {
                            self.source_error(Level::Error,"Не удалось однозначно сопоставить файл эпизода с содержимым раздачи. Выберите другую раздачу.");
                            return;
                        }
                    },
                    None => None,
                };
                self.finish_selection(Selection {
                    origin: Origin::RemoteTorrent {
                        prepared,
                        kind: SourceKind::Magnet,
                    },
                    file_id,
                    title: format!("{} · BitTorrent", index.name()),
                    filename: "video.mp4".into(),
                    target: None,
                });
                return;
            }
            Ok(Ok(_)) => "Метаданные не соответствуют выбранной раздаче.",
            Ok(Err(_)) => "Не удалось получить список файлов. Выберите другую раздачу.",
            Err(_) => {
                "За 45 секунд не удалось получить список файлов от пиров. Выберите другую раздачу."
            }
        };
        self.source_error(Level::Error, message);
    }
}

fn unique_video_hint(
    files: &[kadr_torrent::TorrentFile],
    hint: &str,
    target: Option<kadr_core::torrent::EpisodeNumber>,
) -> Option<usize> {
    let target = target?;
    let hint = TorrentRelativePath::new(hint).ok()?;
    let mut matches = files.iter().filter_map(|file| {
        if !file.relative_filename.ends_with(hint.as_path()) {
            return None;
        }
        let path = TorrentRelativePath::new(
            file.relative_filename
                .to_str()?
                .replace(std::path::MAIN_SEPARATOR, "/"),
        )
        .ok()?;
        let video = kadr_core::torrent::VideoCandidate::from_file(
            kadr_core::torrent::FileIndex(file.id.try_into().ok()?),
            path,
            file.len,
        )?;
        match video.episode() {
            EpisodeRecognition::Ambiguous(_) => None,
            EpisodeRecognition::Recognized(number) if *number != target => None,
            _ => Some(file.id),
        }
    });
    let id = matches.next()?;
    matches.next().is_none().then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_file_mapping_never_uses_pack_order_or_an_ambiguous_basename() {
        let ep = kadr_core::torrent::EpisodeNumber::new(1, 2);
        let files = [
            "A/01.mkv",
            "A/02.mkv",
            "B/02.mkv",
            "A/S01E03.mkv",
            "A/02.txt",
        ]
        .into_iter()
        .enumerate()
        .map(|(id, path)| kadr_torrent::TorrentFile {
            id,
            relative_filename: PathBuf::from(path),
            len: 4096,
        })
        .collect::<Vec<_>>();
        assert_eq!(unique_video_hint(&files, "A/02.mkv", ep), Some(1));
        assert_eq!(unique_video_hint(&files, "02.mkv", ep), None);
        assert_eq!(unique_video_hint(&files, "S01E03.mkv", ep), None);
        assert_eq!(unique_video_hint(&files, "../02.mkv", ep), None);
        assert_eq!(unique_video_hint(&files, "A/02.txt", ep), None);
        assert_eq!(unique_video_hint(&files, "missing.mkv", ep), None);
    }
}
