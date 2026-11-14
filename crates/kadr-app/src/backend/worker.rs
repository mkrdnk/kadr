use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt;
use kadr_cache::{CacheManager, SaveProgress};
use kadr_catalog::{Client, Options};
use kadr_core::{
    catalog::{Episode, Media, MediaKind, SourceCandidate},
    source::{ManualSource, SourceKind},
    torrent::{EpisodeRecognition, FileIndex, TorrentRelativePath, VideoCandidate},
};
use kadr_media::{
    ByteSource, LocalFile,
    players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry},
};
use kadr_storage::{
    AppPaths, EpisodeKey, MediaId, MediaRecord, PlaybackProgress, PlaybackTarget, PlayerPreference,
    ProgressOrigin, SavedFile, Settings, SourceId, Storage, StoredSource,
};
use kadr_torrent::engine::{EngineState, PreparedTorrent};
use sha2::{Digest, Sha256};
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use super::playback::{Stream, engine_options};
use super::{AsyncState, Command, Event, FileChoice, Level, Request, TransferStatus};

#[derive(Clone)]
enum Origin {
    Manual {
        source: ManualSource,
        prepared: Option<PreparedTorrent>,
    },
    Saved {
        path: PathBuf,
        source_id: SourceId,
    },
}

#[derive(Clone)]
struct Selection {
    origin: Origin,
    file_id: Option<usize>,
    title: String,
    filename: String,
    target: Option<PlaybackTarget>,
}

impl Selection {
    fn descriptor(&self) -> Option<StoredSource> {
        let Origin::Manual { source, .. } = &self.origin else {
            return None;
        };
        match source.kind() {
            SourceKind::Http => Some(StoredSource::Link(source.http_url()?.as_str().to_owned())),
            SourceKind::Magnet => Some(StoredSource::TorrentLink {
                uri: source.magnet_uri()?.to_owned(),
                file_id: self.file_id,
            }),
            SourceKind::TorrentFile => Some(StoredSource::TorrentFile {
                path: source.torrent_path()?.to_owned(),
                file_id: self.file_id,
            }),
        }
    }

    fn source_id(&self) -> io::Result<SourceId> {
        match &self.origin {
            Origin::Manual { source, .. } => {
                SourceId::new(source.id().as_str().to_owned()).map_err(storage_error)
            }
            Origin::Saved { source_id, .. } => Ok(source_id.clone()),
        }
    }

    fn is_playable(&self, network_enabled: bool) -> bool {
        match &self.origin {
            Origin::Saved { .. } => true,
            Origin::Manual { source, .. } if source.http_url().is_some() => true,
            Origin::Manual { prepared, .. } => {
                network_enabled
                    && self.file_id.is_some_and(|id| {
                        prepared
                            .as_ref()
                            .is_some_and(|metadata| metadata.files.iter().any(|file| file.id == id))
                    })
            }
        }
    }
}

struct Active {
    stream: Stream,
    player: Option<PlayerSession>,
    browser: bool,
    stop: CancellationToken,
    target: PlaybackTarget,
    source_id: SourceId,
    descriptor: Option<StoredSource>,
    origin: ProgressOrigin,
    progress_saved: bool,
    progress_failed: bool,
    last_progress: Option<PlaybackProgress>,
}

enum CatalogAnswer {
    Search(Result<Vec<Media>, String>),
    Details(Result<Media, String>),
    Sources(Result<Vec<SourceCandidate>, String>),
}

struct CatalogResult {
    cancel: CancellationToken,
    answer: CatalogAnswer,
}

type SaveResult = (PlaybackTarget, PathBuf, Result<(), kadr_cache::Error>);

enum SaveInput {
    Positional(Arc<dyn ByteSource>),
    FullResponse(kadr_media::HttpDownload),
}

impl SaveInput {
    fn len(&self) -> u64 {
        match self {
            Self::Positional(source) => source.len(),
            Self::FullResponse(download) => download.len(),
        }
    }
}

struct Worker {
    storage: Storage,
    paths: AppPaths,
    settings: Settings,
    cache: Arc<CacheManager>,
    catalog: Arc<Client>,
    events: UnboundedSender<Event>,
    selection: Option<Selection>,
    media: Option<Media>,
    episode: Option<Episode>,
    episode_explicit: bool,
    sources: Vec<SourceCandidate>,
    active: Option<Active>,
    save: Option<JoinHandle<SaveResult>>,
    save_stop: Option<CancellationToken>,
    save_generation: u64,
    saving: String,
    save_progress: mpsc::UnboundedSender<(u64, SaveProgress)>,
    catalog_results: mpsc::UnboundedSender<CatalogResult>,
    catalog_jobs: JoinSet<()>,
    headless: bool,
    network_override: Option<kadr_torrent::engine::EngineOptions>,
}

pub(super) async fn run(
    mut commands: mpsc::Receiver<Request>,
    events: UnboundedSender<Event>,
    shutdown: CancellationToken,
    paths: Option<AppPaths>,
    headless: bool,
    network_override: Option<kadr_torrent::engine::EngineOptions>,
) {
    let paths = match paths.map_or_else(AppPaths::discover, Ok) {
        Ok(paths) => paths,
        Err(_) => {
            send_error(&events, "Не удалось определить пользовательские каталоги.");
            return;
        }
    };
    let storage = match Storage::open_default(&paths) {
        Ok(storage) => storage,
        Err(_) => {
            send_error(
                &events,
                "Не удалось открыть БД. Данные не сброшены; проверьте доступ и версию схемы.",
            );
            return;
        }
    };
    let settings = match storage.load_settings() {
        Ok(settings) => settings,
        Err(_) => {
            send_error(
                &events,
                "Не удалось прочитать настройки. Повреждённые данные не заменены defaults.",
            );
            return;
        }
    };
    let cache = match open_cache(&paths, &settings) {
        Ok(cache) => Arc::new(cache),
        Err(error) => {
            send_error(&events, &format!("Кэш недоступен: {error}"));
            return;
        }
    };
    let catalog = match Client::new(Options {
        enabled: settings.catalog_enabled,
    }) {
        Ok(client) => Arc::new(client),
        Err(_) => {
            send_error(&events, "Не удалось настроить каталоги.");
            return;
        }
    };
    crate::posters::set_enabled(settings.catalog_enabled);
    if std::env::args_os().any(|argument| argument == "--smoke-ui") {
        eprintln!("kadr-ui-smoke: backend-ready");
    }
    let (save_progress, mut progress) = mpsc::unbounded_channel();
    let (catalog_results, mut answers) = mpsc::unbounded_channel();
    let _ = events.unbounded_send(Event::Ready {
        settings: settings.clone(),
        cache: cache.usage().ok(),
    });
    // Keep database ownership on this worker for the entire application lifetime.
    let mut worker = Worker {
        storage,
        paths,
        settings,
        cache,
        catalog,
        events,
        selection: None,
        media: None,
        episode: None,
        episode_explicit: false,
        sources: Vec::new(),
        active: None,
        save: None,
        save_stop: None,
        save_generation: 0,
        saving: String::new(),
        save_progress,
        catalog_results,
        catalog_jobs: JoinSet::new(),
        headless,
        network_override,
    };
    worker.publish_library();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let stop = worker
            .active
            .as_ref()
            .map(|active| active.stop.clone())
            .unwrap_or_else(|| shutdown.clone());
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            () = stop.cancelled(), if worker.active.is_some() => {
                worker.close_active("Воспроизведение остановлено.").await;
            }
            request = commands.recv() => match request {
                Some(request) => worker.command(request).await,
                None => break,
            },
            Some(answer) = answers.recv() => worker.catalog_answer(answer),
            Some((generation, progress)) = progress.recv() => {
                if generation == worker.save_generation && worker.save.is_some() {
                    worker.saving = format!("Сохранение: {} / {} байт", progress.written_bytes, progress.total_bytes);
                    worker.publish_transfer();
                }
            },
            result = async {
                match worker.save.as_mut() {
                    Some(save) => save.await,
                    None => std::future::pending().await,
                }
            } => {
                worker.save = None;
                worker.save_finished(result);
                worker.publish_library();
                worker.publish_transfer();
                if worker.active.as_ref().is_some_and(|active| active.player.is_none() && !active.browser) {
                    worker.close_active("Загрузка для сохранения завершена.").await;
                }
            },
            Some(_) = worker.catalog_jobs.join_next(), if !worker.catalog_jobs.is_empty() => {},
            _ = tick.tick(), if worker.active.is_some() => worker.tick().await,
        }
    }
    worker.close_active("").await;
    worker.catalog_jobs.abort_all();
    while worker.catalog_jobs.join_next().await.is_some() {}
    crate::posters::set_enabled(false);
}

impl Worker {
    fn status(&self, level: Level, message: &str) {
        let _ = self
            .events
            .unbounded_send(Event::Status(level, message.to_owned()));
    }

    fn send(&self, event: Event) {
        let _ = self.events.unbounded_send(event);
    }

    fn idle(&self) -> bool {
        if self.active.is_some() {
            self.status(
                Level::Warning,
                "Сначала остановите текущую сессию. Данные кэша будут сохранены.",
            );
            false
        } else {
            true
        }
    }

    async fn command(&mut self, request: Request) {
        let cancel = request.cancel;
        if cancel.is_cancelled() {
            return;
        }
        match request.command {
            Command::Search(query) => {
                self.send(Event::SearchState(AsyncState::Loading));
                let client = self.catalog.clone();
                let output = self.catalog_results.clone();
                self.catalog_jobs.spawn(async move {
                    let result = client
                        .search(&query, &cancel)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = output.send(CatalogResult {
                        cancel,
                        answer: CatalogAnswer::Search(result),
                    });
                });
            }
            Command::OpenMedia(id) => {
                self.send(Event::SearchState(AsyncState::Loading));
                let client = self.catalog.clone();
                let output = self.catalog_results.clone();
                self.catalog_jobs.spawn(async move {
                    let result = client
                        .details(&id, &cancel)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = output.send(CatalogResult {
                        cancel,
                        answer: CatalogAnswer::Details(result),
                    });
                });
            }
            Command::FindSources => {
                let retry = self
                    .selection
                    .as_ref()
                    .and_then(|selection| match &selection.origin {
                        Origin::Manual {
                            source,
                            prepared: None,
                        } if source.http_url().is_none() => {
                            Some((source.clone(), selection.file_id, selection.target.clone()))
                        }
                        _ => None,
                    });
                if let Some((source, file_id, target)) = retry {
                    self.select_source(source, file_id, target, cancel).await;
                } else {
                    self.find_sources(cancel);
                }
            }
            Command::SelectEpisode(season, number) => {
                if !self.idle() {
                    return;
                }
                self.episode = self.media.as_ref().and_then(|media| {
                    media
                        .episodes
                        .iter()
                        .find(|episode| episode.season == season && episode.number == number)
                        .cloned()
                });
                if self.episode.is_none() {
                    self.status(Level::Error, "Эпизод отсутствует в карточке сериала.");
                    return;
                }
                self.episode_explicit = true;
                self.find_sources(cancel);
            }
            Command::SelectSource(source) => {
                if self.idle() {
                    self.select_source(source, None, None, cancel).await;
                }
            }
            Command::ChooseSource(index) => {
                if self.idle() {
                    let Some(candidate) = self.sources.get(index) else {
                        self.status(Level::Error, "Источник больше недоступен; повторите поиск.");
                        return;
                    };
                    match ManualSource::parse_link(&candidate.url) {
                        Ok(source) => self.select_source(source, None, None, cancel).await,
                        Err(_) => {
                            self.status(Level::Error, "Провайдер вернул неподдерживаемый источник.")
                        }
                    }
                }
            }
            Command::ChooseTorrentFile(index) => {
                if self.idle() {
                    self.choose_file(index);
                }
            }
            Command::AssignEpisode(season, number) => {
                if self.idle() {
                    if number == 0 {
                        self.status(Level::Error, "Номер эпизода должен быть больше нуля.");
                        return;
                    }
                    self.episode = Some(Episode {
                        season,
                        number,
                        title: format!("S{season:02}E{number:02}"),
                        description: String::new(),
                    });
                    self.episode_explicit = true;
                    if let Some(selection) = self.selection.as_mut() {
                        selection.target = None;
                    }
                    self.status(
                        Level::Info,
                        "Привязка эпизода изменена. Прогресс будет храниться отдельно.",
                    );
                }
            }
            Command::Play { resume } => {
                self.play(resume, self.settings.default_player, cancel)
                    .await
            }
            Command::OpenIn(player) => {
                if self.active.is_some() {
                    self.switch_player(player).await;
                } else {
                    self.play(true, player, cancel).await;
                }
            }
            Command::Stop => self.close_active("Сессия остановлена.").await,
            Command::SaveTo(path) => self.save_to(path, cancel).await,
            Command::SetFavorite { media_id, favorite } => self.favorite(media_id, favorite),
            Command::OpenLibrary(id) => {
                if self.idle() {
                    self.open_library(&id, cancel).await;
                }
            }
            Command::UpdateSettings(settings) => self.update_settings(settings),
            Command::ClearCache => {
                match self.cache.clear_inactive() {
                    Ok(usage) => {
                        self.send(Event::CacheUsage(usage));
                        self.status(Level::Info, "Неактивный кэш очищен. Активная сессия и сохранённые файлы не удалены.");
                    }
                    Err(error) => self.status(Level::Error, &error.to_string()),
                }
            }
            Command::ClearHistory => {
                if !self.idle() {
                    return;
                }
                if self.storage.clear_playback_data().is_ok() {
                    self.publish_library();
                    self.status(
                        Level::Info,
                        "История и прогресс очищены. Избранное и сохранённые файлы оставлены.",
                    );
                } else {
                    self.status(Level::Error, "Не удалось очистить историю.");
                }
            }
            Command::UnlinkMedia => {
                if self.idle() {
                    self.media = None;
                    self.episode = None;
                    self.episode_explicit = false;
                    if let Some(selection) = &mut self.selection {
                        selection.target = None;
                    }
                    self.send(Event::MediaSelected {
                        media: None,
                        favorite: false,
                    });
                    self.status(
                        Level::Info,
                        "Источник отвязан от каталога; используется локальная запись.",
                    );
                }
            }
            Command::Cancel => {
                self.sources.clear();
                if self.active.is_none() {
                    self.selection = None;
                    self.send(Event::TorrentFiles(Vec::new()));
                    self.send(Event::SourceSelected {
                        kind: None,
                        title: String::new(),
                        playable: false,
                        suggested_filename: String::new(),
                    });
                }
                self.send(Event::SearchState(AsyncState::Ready(Vec::new())));
                self.send(Event::SourcesState(AsyncState::Ready(Vec::new())));
                self.status(Level::Info, "Операция отменена.");
            }
        }
    }

    fn find_sources(&mut self, cancel: CancellationToken) {
        let Some(media) = self.media.clone() else {
            self.status(
                Level::Warning,
                "Сначала откройте карточку фильма или сериала.",
            );
            return;
        };
        self.send(Event::SourcesState(AsyncState::Loading));
        let episode = self.episode.clone();
        let client = self.catalog.clone();
        let output = self.catalog_results.clone();
        self.catalog_jobs.spawn(async move {
            let result = client
                .find_sources(&media, episode.as_ref(), &cancel)
                .await
                .map_err(|error| error.to_string());
            let _ = output.send(CatalogResult {
                cancel,
                answer: CatalogAnswer::Sources(result),
            });
        });
    }

    fn catalog_answer(&mut self, result: CatalogResult) {
        if result.cancel.is_cancelled() {
            return;
        }
        match result.answer {
            CatalogAnswer::Search(result) => self.send(Event::SearchState(match result {
                Ok(media) => AsyncState::Ready(media),
                Err(message) => AsyncState::Error(message),
            })),
            CatalogAnswer::Details(result) => match result {
                Ok(media) => {
                    if self.active.is_some() {
                        self.status(
                            Level::Warning,
                            "Остановите просмотр перед выбором другого произведения.",
                        );
                        self.send(Event::SearchState(AsyncState::Ready(Vec::new())));
                        return;
                    }
                    let record = media_record(&media);
                    if record
                        .as_ref()
                        .is_some_and(|record| self.storage.upsert_media(record).is_err())
                    {
                        self.status(
                            Level::Warning,
                            "Карточка получена, но метаданные не удалось сохранить.",
                        );
                    }
                    let favorite = record
                        .and_then(|record| self.storage.library_entry(&record.id).ok().flatten())
                        .is_some_and(|entry| entry.favorite);
                    self.media = Some(media.clone());
                    self.episode = None;
                    self.episode_explicit = false;
                    self.sources.clear();
                    self.selection = None;
                    self.send(Event::TorrentFiles(Vec::new()));
                    self.send(Event::SourceSelected {
                        kind: None,
                        title: String::new(),
                        playable: false,
                        suggested_filename: String::new(),
                    });
                    self.send(Event::SearchState(AsyncState::Ready(Vec::new())));
                    self.send(Event::MediaSelected {
                        media: Some(media),
                        favorite,
                    });
                    self.send(Event::SourcesState(AsyncState::Ready(Vec::new())));
                }
                Err(message) => self.send(Event::SearchState(AsyncState::Error(message))),
            },
            CatalogAnswer::Sources(result) => match result {
                Ok(sources) => {
                    self.sources = sources.clone();
                    self.send(Event::SourcesState(AsyncState::Ready(sources)));
                }
                Err(message) => self.send(Event::SourcesState(AsyncState::Error(message))),
            },
        }
    }

    async fn select_source(
        &mut self,
        source: ManualSource,
        file_id: Option<usize>,
        target: Option<PlaybackTarget>,
        cancel: CancellationToken,
    ) {
        self.selection = Some(Selection {
            origin: Origin::Manual {
                source: source.clone(),
                prepared: None,
            },
            file_id,
            target: target.clone(),
            title: "Метаданные не получены".into(),
            filename: "video.mp4".into(),
        });
        if !self.episode_explicit {
            self.episode = None;
        }
        self.send(Event::TorrentFiles(Vec::new()));
        let kind = source.kind();
        self.send(Event::SourceSelected {
            kind: Some(kind),
            title: "Проверка источника…".into(),
            playable: false,
            suggested_filename: "video.mp4".into(),
        });
        self.send(Event::SourcesState(AsyncState::Loading));
        let prepared = if kind == SourceKind::Http {
            None
        } else {
            self.status(
                Level::Info,
                "Получение метаданных торрента… Для magnet требуется разрешение сети в Настройках.",
            );
            let result = match kind {
                SourceKind::TorrentFile => tokio::select! {
                    () = cancel.cancelled() => return,
                    result = read_metainfo(source.torrent_path().expect("validated torrent")) => result
                        .and_then(|bytes| PreparedTorrent::from_bytes(bytes.into()).map_err(|_| io::Error::other("Некорректные метаданные .torrent."))),
                },
                SourceKind::Magnet => match self.torrent_options() {
                    Ok(options) => PreparedTorrent::from_magnet(source.magnet_uri().expect("validated magnet"), options, cancel.clone()).await
                        .map_err(|_| io::Error::other("Не удалось получить magnet-метаданные. Разрешите BitTorrent, проверьте пиры или откройте .torrent.")),
                    Err(error) => Err(error),
                },
                SourceKind::Http => unreachable!(),
            };
            if cancel.is_cancelled() {
                return;
            }
            match result {
                Ok(prepared) => Some(prepared),
                Err(error) => {
                    self.send(Event::SourcesState(AsyncState::Error(error.to_string())));
                    self.status(Level::Error, &error.to_string());
                    return;
                }
            }
        };
        if cancel.is_cancelled() {
            return;
        }
        let filename = match &prepared {
            Some(prepared) => prepared
                .files
                .iter()
                .find(|file| Some(file.id) == file_id)
                .and_then(|file| file.relative_filename.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "video.mp4".into()),
            None => source
                .http_url()
                .and_then(|url| url.path_segments()?.next_back())
                .filter(|name| name.contains('.'))
                .map(str::to_owned)
                .unwrap_or_else(|| "video.mp4".into()),
        };
        let title = if kind == SourceKind::Http {
            "Ручной HTTP(S)-источник".into()
        } else {
            "BitTorrent".into()
        };
        self.selection = Some(Selection {
            origin: Origin::Manual { source, prepared },
            file_id,
            title,
            filename,
            target,
        });
        let choices = self.file_choices();
        let selected = self
            .selection
            .as_ref()
            .and_then(|selection| selection.file_id);
        let suggested = if selected.is_some() {
            selected
        } else {
            let matches: Vec<_> = choices
                .iter()
                .filter(|choice| {
                    self.episode.as_ref().is_some_and(|episode| {
                        choice.season == Some(episode.season)
                            && choice.episode == Some(episode.number)
                            && !choice.ambiguous
                    })
                })
                .map(|choice| choice.index)
                .collect();
            if matches.len() == 1 {
                Some(matches[0])
            } else if choices.len() == 1 {
                Some(choices[0].index)
            } else {
                None
            }
        };
        self.send(Event::TorrentFiles(choices));
        self.send(Event::SourcesState(AsyncState::Ready(Vec::new())));
        if let Some(index) = suggested {
            self.choose_file(index);
        } else {
            self.publish_selection();
        }
        self.status(Level::Info, "Источник проверен. Ссылка или путь сохраняются только в приватной локальной БД для продолжения просмотра.");
    }

    fn file_choices(&self) -> Vec<FileChoice> {
        let Some(Selection {
            origin:
                Origin::Manual {
                    prepared: Some(prepared),
                    ..
                },
            ..
        }) = &self.selection
        else {
            return Vec::new();
        };
        prepared
            .files
            .iter()
            .filter_map(|file| {
                let path = TorrentRelativePath::new(
                    file.relative_filename
                        .to_str()?
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                )
                .ok()?;
                let video =
                    VideoCandidate::from_file(FileIndex(file.id.try_into().ok()?), path, file.len)?;
                let (season, episode, ambiguous) = match video.episode() {
                    EpisodeRecognition::Recognized(episode) => {
                        (Some(episode.season()), Some(episode.episode()), false)
                    }
                    EpisodeRecognition::Ambiguous(_) => (None, None, true),
                    EpisodeRecognition::Unrecognized => (None, None, false),
                };
                Some(FileChoice {
                    index: file.id,
                    name: video.path().as_str().to_owned(),
                    len: file.len,
                    season,
                    episode,
                    ambiguous,
                })
            })
            .collect()
    }

    fn choose_file(&mut self, index: usize) {
        let Some(choice) = self
            .file_choices()
            .into_iter()
            .find(|choice| choice.index == index)
        else {
            if let Some(selection) = &mut self.selection {
                selection.file_id = None;
            }
            self.publish_selection();
            self.status(Level::Error, "Выберите видеофайл из списка.");
            return;
        };
        if !self.episode_explicit {
            self.episode = None;
            if let (Some(season), Some(number)) = (choice.season, choice.episode) {
                self.episode = Some(Episode {
                    season,
                    number,
                    title: format!("S{season:02}E{number:02}"),
                    description: String::new(),
                });
            }
        }
        if let Some(selection) = &mut self.selection {
            if selection.file_id.is_some_and(|previous| previous != index) {
                selection.target = None;
            }
            selection.file_id = Some(index);
            selection.filename = Path::new(&choice.name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            selection.title = choice.name;
        }
        self.publish_selection();
    }

    fn publish_selection(&self) {
        let Some(selection) = &self.selection else {
            return;
        };
        let kind = match &selection.origin {
            Origin::Manual { source, .. } => Some(source.kind()),
            Origin::Saved { .. } => None,
        };
        let playable = selection.is_playable(self.settings.network.torrents_enabled);
        self.send(Event::SourceSelected {
            kind,
            title: selection.title.clone(),
            playable,
            suggested_filename: selection.filename.clone(),
        });
        if matches!(&selection.origin, Origin::Manual { source, prepared: None } if source.http_url().is_none())
        {
            self.send(Event::SourcesState(AsyncState::Error(
                "Метаданные ещё не получены. Проверьте разрешение BitTorrent в Настройках и нажмите «Повторить».".into(),
            )));
        }
    }

    fn target(&self, selection: &Selection) -> io::Result<PlaybackTarget> {
        if let Some(target) = &selection.target {
            return Ok(target.clone());
        }
        if self.media.is_none()
            && let Origin::Saved { path, .. } = &selection.origin
        {
            return Ok(PlaybackTarget {
                media_id: MediaId::new(format!(
                    "saved-item:v1:{:x}",
                    Sha256::digest(path.as_os_str().as_encoded_bytes())
                ))
                .map_err(storage_error)?,
                episode: self.episode.as_ref().map(|episode| EpisodeKey {
                    season: episode.season,
                    episode: episode.number,
                }),
            });
        }
        let id = if let Some(media) = &self.media
            && !media.id.starts_with("source:")
        {
            media.id.clone()
        } else {
            let source = selection.source_id()?;
            match selection.file_id {
                Some(index) => format!("{}:file:{index}", source.as_str()),
                None => source.as_str().to_owned(),
            }
        };
        Ok(PlaybackTarget {
            media_id: MediaId::new(id).map_err(storage_error)?,
            episode: self.episode.as_ref().map(|episode| EpisodeKey {
                season: episode.season,
                episode: episode.number,
            }),
        })
    }

    async fn play(&mut self, resume: bool, player: PlayerPreference, cancel: CancellationToken) {
        if self.active.is_some() {
            self.status(
                Level::Warning,
                "Сессия уже активна. Используйте «Открыть в…» для смены плеера.",
            );
            self.send(Event::PlaybackActive(true));
            return;
        }
        let result = self.open_active(cancel.clone()).await;
        match result {
            Ok(active) => self.active = Some(active),
            Err(error) => {
                self.send(Event::PlaybackActive(false));
                self.status(
                    if cancel.is_cancelled() {
                        Level::Info
                    } else {
                        Level::Error
                    },
                    &error.to_string(),
                );
                return;
            }
        }
        if let Err(error) = self.launch_player(player, resume).await {
            self.close_active("").await;
            self.status(Level::Error, &error.to_string());
        } else {
            self.send(Event::PlaybackActive(true));
            self.publish_transfer();
            self.publish_library();
        }
    }

    async fn open_active(&mut self, cancel: CancellationToken) -> io::Result<Active> {
        self.saving.clear();
        let selection = self
            .selection
            .clone()
            .ok_or_else(|| io::Error::other("Сначала выберите источник."))?;
        let target = self.target(&selection)?;
        let source_id = selection.source_id()?;
        let descriptor = selection.descriptor();
        let last_progress = self
            .storage
            .load_progress(&target)
            .map_err(storage_error)?
            .filter(|progress| progress.source_id == source_id);
        if let Some(descriptor) = &descriptor {
            self.storage
                .save_source(&source_id, descriptor)
                .map_err(storage_error)?;
            self.storage
                .link_source(&target, &source_id)
                .map_err(storage_error)?;
        }
        if self.media.is_none() {
            self.storage
                .upsert_media(&MediaRecord {
                    id: target.media_id.clone(),
                    title: selection.title.clone(),
                    original_title: None,
                    year: None,
                    kind: if target.episode.is_some() {
                        kadr_storage::MediaKind::Series
                    } else {
                        kadr_storage::MediaKind::Movie
                    },
                    description: "Локальный источник пользователя.".into(),
                    poster_url: None,
                    attribution: String::new(),
                })
                .map_err(storage_error)?;
        }
        let saved_association = match &selection.origin {
            Origin::Saved { path, .. }
                if self
                    .storage
                    .saved_file(&target)
                    .map_err(storage_error)?
                    .is_none_or(|saved| saved.path != *path) =>
            {
                Some(path.clone())
            }
            _ => None,
        };
        let mut stream = match &selection.origin {
            Origin::Saved { path, .. } => Stream::saved(path.clone(), cancel.child_token()).await?,
            Origin::Manual { source, prepared } => {
                if let Some(prepared) = prepared {
                    let file_id = selection
                        .file_id
                        .ok_or_else(|| io::Error::other("Выберите видеофайл торрента."))?;
                    Stream::torrent(
                        prepared,
                        file_id,
                        &self.cache,
                        self.torrent_options()?,
                        cancel.child_token(),
                    )
                    .await?
                } else {
                    Stream::http(source, cancel.child_token()).await?
                }
            }
        };
        if let Some(path) = saved_association
            && let Err(error) = self.storage.save_saved_file(&SavedFile {
                target: target.clone(),
                path,
                saved_at: now(),
            })
        {
            stream.stop().await;
            return Err(storage_error(error));
        }
        if cancel.is_cancelled() {
            stream.stop().await;
            return Err(io::Error::other("Запуск отменён."));
        }
        Ok(Active {
            stream,
            player: None,
            browser: false,
            stop: cancel,
            target,
            source_id,
            descriptor,
            origin: ProgressOrigin::Mpv,
            progress_saved: false,
            progress_failed: false,
            last_progress,
        })
    }

    async fn launch_player(
        &mut self,
        preference: PlayerPreference,
        restore: bool,
    ) -> io::Result<()> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| io::Error::other("Нет активного источника."))?;
        if preference == PlayerPreference::Browser {
            if active.stream.bytes.is_none() {
                return Err(io::Error::other(
                    "Браузеру нужен безопасный локальный поток. Этот HTTP-источник не предоставляет стабильные Range/validators; выберите mpv/VLC.",
                ));
            }
            open::that_detached(&active.stream.url)
                .map_err(|_| io::Error::other("Не удалось открыть системный браузер."))?;
            active.browser = true;
        } else {
            let resume = if restore {
                self.storage
                    .load_progress(&active.target)
                    .map_err(storage_error)?
                    .filter(|progress| !progress.completed && progress.position_seconds >= 1.0)
                    .and_then(|progress| {
                        Duration::try_from_secs_f64(progress.position_seconds).ok()
                    })
            } else {
                None
            };
            let kind = match preference {
                PlayerPreference::Mpv => PlayerKind::Mpv,
                _ => PlayerKind::Vlc,
            };
            let mut options = PlayerOptions::for_player(kind);
            options.headless = self.headless;
            #[cfg(target_os = "macos")]
            {
                let candidates = match kind {
                    PlayerKind::Mpv => [
                        "/opt/homebrew/bin/mpv",
                        "/usr/local/bin/mpv",
                        "/Applications/mpv.app/Contents/MacOS/mpv",
                    ],
                    PlayerKind::Vlc => [
                        "/Applications/VLC.app/Contents/MacOS/VLC",
                        "/opt/homebrew/bin/vlc",
                        "/usr/local/bin/vlc",
                    ],
                };
                if let Some(path) = candidates.iter().find(|path| Path::new(path).is_file()) {
                    options.executable = (*path).into();
                }
            }
            let player = PlayerSession::launch(
                kind,
                PlayRequest {
                    media: OsString::from(&active.stream.url),
                    resume,
                },
                options,
                &active.stop,
            )
            .await
            .map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    io::Error::other(
                        "Плеер не найден. Установите выбранный mpv/VLC или выберите другой.",
                    )
                } else {
                    io::Error::other(
                        "Не удалось запустить плеер или IPC. Проверьте установку и повторите.",
                    )
                }
            })?;
            active.player = Some(player);
            active.browser = false;
            active.origin = if kind == PlayerKind::Mpv {
                ProgressOrigin::Mpv
            } else {
                ProgressOrigin::Vlc
            };
        }
        let history_failed = self
            .storage
            .record_opened_target(&active.target, &active.source_id, now())
            .is_err();
        let warning = active.stream.proxy_warning;
        self.status(
            if history_failed || warning || preference != PlayerPreference::Mpv { Level::Warning } else { Level::Info },
            if history_failed { "Источник открыт, но историю не удалось сохранить." }
            else if preference == PlayerPreference::Browser { "Поток открыт в браузере. Контейнер/кодек могут не поддерживаться; автоматический прогресс браузера недоступен. Для завершения нажмите «Стоп»." }
            else if preference == PlayerPreference::Vlc { "VLC открыт. Эта версия RC не предоставляет надёжную телеметрию; новый прогресс не записывается." }
            else if warning { "mpv открыт напрямую: для этого источника локальный Range-прокси недоступен. Сохранение использует отдельный HTTP-запрос; браузер недоступен." }
            else { "mpv открыт. Реальная позиция сохраняется в приватной локальной БД." },
        );
        Ok(())
    }

    async fn switch_player(&mut self, preference: PlayerPreference) {
        if let Some(active) = &mut self.active {
            if let Some(mut player) = active.player.take() {
                let _ = player.stop().await;
            }
            active.browser = false;
        }
        if let Err(error) = self.launch_player(preference, true).await {
            self.status(Level::Error, &error.to_string());
            // Keep the source/cache available for another explicit player choice.
        }
        self.publish_transfer();
    }

    async fn tick(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let mut exited = None;
        if let Some(player) = &mut active.player {
            match player.snapshot(&active.stop).await {
                Ok(snapshot) => {
                    let progress = if let Telemetry::Seconds(position) = snapshot.position {
                        let duration = match snapshot.duration {
                            Telemetry::Seconds(duration)
                                if duration > 0.0 && duration >= position =>
                            {
                                Some(duration)
                            }
                            _ => None,
                        };
                        Some(PlaybackProgress {
                            target: active.target.clone(),
                            source_id: active.source_id.clone(),
                            position_seconds: position,
                            duration_seconds: duration,
                            updated_at: now(),
                            completed: snapshot.completed,
                            origin: active.origin,
                        })
                    } else if snapshot.completed {
                        active.last_progress.clone().map(|mut progress| {
                            // Preserve the last real observation. EOF is a
                            // separate IPC fact, not a fabricated final position.
                            progress.completed = true;
                            progress.updated_at = now();
                            progress
                        })
                    } else {
                        None
                    };
                    if let Some(progress) = progress {
                        let result = match &active.descriptor {
                            Some(descriptor) => {
                                self.storage.save_progress_for_source(&progress, descriptor)
                            }
                            None => self.storage.save_progress(&progress),
                        };
                        active.progress_failed = result.is_err();
                        active.progress_saved |= result.is_ok();
                        active.last_progress = Some(progress);
                    }
                    exited = snapshot.exit.map(|exit| exit.success());
                }
                Err(_) => exited = Some(false),
            }
        }
        if let Some(success) = exited {
            if self.save.is_some() {
                if let Some(active) = &mut self.active
                    && let Some(mut player) = active.player.take()
                {
                    let _ = player.stop().await;
                }
                self.status(
                    Level::Info,
                    "Плеер завершён; сохранение файла продолжается. «Стоп» отменит его.",
                );
            } else {
                self.close_active(if success {
                    "Плеер завершил работу."
                } else {
                    "Плеер или связь IPC завершились с ошибкой; выберите другой источник/плеер."
                })
                .await;
            }
        }
        self.publish_transfer();
    }

    async fn save_to(&mut self, destination: PathBuf, cancel: CancellationToken) {
        if self.save.is_some() {
            self.status(Level::Warning, "Сохранение уже выполняется.");
            return;
        }
        let Some(active) = self.active.as_ref() else {
            self.status(Level::Warning, "Сначала запустите выбранный источник.");
            return;
        };
        self.saving.clear();
        let target = active.target.clone();
        let positional = active.stream.bytes.clone();
        let fallback = active
            .descriptor
            .as_ref()
            .and_then(|descriptor| match descriptor {
                StoredSource::Link(link) => Some(link.clone()),
                _ => None,
            });
        let parent = match destination
            .parent()
            .and_then(|path| path.canonicalize().ok())
        {
            Some(parent) => parent,
            None => {
                self.status(Level::Error, "Папка сохранения недоступна.");
                return;
            }
        };
        let cache_root = cache_root(&self.paths, &self.settings);
        if cache_root
            .canonicalize()
            .is_ok_and(|root| parent.starts_with(root))
            || managed_cache_ancestor(&parent)
        {
            self.status(
                Level::Error,
                "Сохраните файл вне временного кэша, иначе очистка может удалить его.",
            );
            return;
        }
        let source = match positional {
            Some(source) => SaveInput::Positional(source),
            None => {
                let Some(url) = fallback else {
                    self.status(
                        Level::Error,
                        "Источник не предоставляет данные для сохранения.",
                    );
                    return;
                };
                self.saving = "Получение полного HTTP-файла…".into();
                self.publish_transfer();
                match kadr_media::HttpDownload::open(url, cancel.clone()).await {
                    Ok(download) => SaveInput::FullResponse(download),
                    Err(error) => {
                        self.saving = format!("Сохранение не начато: {error}");
                        self.status(
                            if cancel.is_cancelled() {
                                Level::Info
                            } else {
                                Level::Error
                            },
                            &self.saving,
                        );
                        self.publish_transfer();
                        return;
                    }
                }
            }
        };
        if let Err(error) = self.cache.check_save_space(&parent, source.len()) {
            self.saving = format!("Сохранение не начато: {error}");
            self.status(
                Level::Error,
                &format!("Недостаточно места с учётом резервов загрузки: {error}"),
            );
            self.publish_transfer();
            return;
        }
        let progress = self.save_progress.clone();
        let save_cancel = cancel.clone();
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        self.save_stop = Some(cancel);
        self.save = Some(tokio::spawn(async move {
            // Await the authoritative atomic-publication result. Cancelling
            // through save_cancel may prevent commit, but must not discard a
            // successful hardlink that finished just before Stop.
            let report = move |value| {
                let _ = progress.send((generation, value));
            };
            let result = match source {
                SaveInput::Positional(source) => {
                    kadr_cache::save_source(source, destination.clone(), &save_cancel, report).await
                }
                SaveInput::FullResponse(download) => {
                    let len = download.len();
                    let chunks =
                        futures_util::stream::try_unfold(download, |mut download| async move {
                            let bytes = download.next_chunk(64 * 1024).await?;
                            Ok::<_, io::Error>(if bytes.is_empty() {
                                None
                            } else {
                                Some((bytes, download))
                            })
                        })
                        .boxed();
                    let reader = tokio_util::io::StreamReader::new(chunks);
                    kadr_cache::save_reader(reader, len, destination.clone(), &save_cancel, report)
                        .await
                }
            };
            (target, destination, result)
        }));
        self.saving = "Ожидание проверенных данных и сохранение…".into();
        self.publish_transfer();
    }

    async fn close_active(&mut self, message: &str) {
        if let Some(stop) = self.save_stop.take() {
            stop.cancel();
        }
        if let Some(save) = self.save.take() {
            // A concurrent save may already have atomically committed. Do not
            // lose its permanent-copy record merely because Stop won the select.
            self.save_finished(save.await);
        }
        self.save_generation = self.save_generation.wrapping_add(1);
        let Some(mut active) = self.active.take() else {
            self.send(Event::PlaybackActive(false));
            return;
        };
        if let Some(mut player) = active.player.take() {
            let _ = player.stop().await;
        }
        active.stream.stop().await;
        let mut message = message.to_owned();
        if active.progress_failed {
            message.push_str(" Последний прогресс не удалось сохранить.");
        } else if active.progress_saved {
            message.push_str(" Последняя измеренная позиция сохранена.");
        }
        let progress_failed = active.progress_failed;
        drop(active);
        self.saving.clear();
        self.send(Event::PlaybackActive(false));
        self.send(Event::Transfer(None));
        self.publish_library();
        if let Ok(usage) = self.cache.usage() {
            self.send(Event::CacheUsage(usage));
        }
        if !message.is_empty() {
            self.status(
                if progress_failed {
                    Level::Warning
                } else {
                    Level::Info
                },
                &message,
            );
        }
    }

    fn save_finished(&mut self, result: Result<SaveResult, tokio::task::JoinError>) {
        self.save_stop = None;
        self.save_generation = self.save_generation.wrapping_add(1);
        match result {
            Ok((target, path, Ok(()))) => {
                let record = SavedFile {
                    target,
                    path,
                    saved_at: now(),
                };
                if self.storage.save_saved_file(&record).is_ok() {
                    self.saving = "Файл сохранён вне временного кэша.".into();
                    self.status(Level::Info, &self.saving);
                } else {
                    self.saving = "Файл сохранён, но путь не удалось записать в библиотеку.".into();
                    self.status(Level::Warning, &self.saving);
                }
            }
            Ok((_, _, Err(error))) => {
                self.saving = format!("Сохранение не завершено: {error}");
                self.status(Level::Warning, &self.saving);
            }
            Err(_) => self.status(Level::Error, "Фоновое сохранение завершилось с ошибкой."),
        }
    }

    fn publish_transfer(&self) {
        let Some(active) = &self.active else {
            return;
        };
        let (stage, down, up, peers, done, total, buffered) =
            if let Some(torrent) = &active.stream.torrent {
                let stats = torrent.stats();
                (
                    match stats.state {
                        EngineState::Initializing => "Проверка и подключение к пирам",
                        EngineState::Downloading if stats.peers == 0 => {
                            "Нет пиров — ожидание подключения"
                        }
                        EngineState::Downloading => "Буферизация / загрузка",
                        EngineState::Ready => "Файл загружен и проверен",
                        EngineState::Paused => "Сеть приостановлена",
                        EngineState::Stopped => "Торрент остановлен",
                        EngineState::Error => "Ошибка торрента",
                    },
                    Some(stats.download_bps),
                    Some(stats.upload_bps),
                    Some(stats.peers),
                    Some(stats.selected_file_bytes),
                    active.stream.total,
                    Some(stats.selected_file_bytes),
                )
            } else {
                (
                    if active.browser {
                        "Локальный поток открыт в браузере"
                    } else {
                        "Воспроизведение HTTP / локального файла"
                    },
                    None,
                    None,
                    None,
                    None,
                    active.stream.total,
                    None,
                )
            };
        self.send(Event::Transfer(Some(TransferStatus {
            stage: stage.into(),
            download_bps: down,
            upload_bps: up,
            peers,
            done,
            total,
            buffered,
            saving: self.saving.clone(),
        })));
    }

    fn publish_library(&self) {
        match self.storage.library_targets(1000) {
            Ok(items) => self.send(Event::Library(items)),
            Err(_) => self.status(
                Level::Error,
                "Не удалось прочитать библиотеку. Данные не сброшены.",
            ),
        }
    }

    fn favorite(&mut self, id: String, favorite: Option<bool>) {
        let id = if favorite.is_none() {
            self.media
                .as_ref()
                .map(|media| media.id.clone())
                .unwrap_or(id)
        } else {
            id.split('@').next().unwrap_or("").to_owned()
        };
        let Ok(id) = MediaId::new(id) else {
            self.status(Level::Warning, "Сначала откройте карточку произведения.");
            return;
        };
        let current = self
            .storage
            .library_entry(&id)
            .ok()
            .flatten()
            .is_some_and(|entry| entry.favorite);
        let favorite = favorite.unwrap_or(!current);
        if self.storage.set_favorite(&id, favorite).is_ok() {
            if self
                .media
                .as_ref()
                .is_some_and(|media| media.id == id.as_str())
            {
                self.send(Event::MediaSelected {
                    media: self.media.clone(),
                    favorite,
                });
            }
            self.publish_library();
        } else {
            self.status(Level::Error, "Не удалось изменить избранное.");
        }
    }

    async fn open_library(&mut self, id: &str, cancel: CancellationToken) {
        // Replacing a library item must never leave the previous input ready
        // for an unrelated card after an error or a later settings update.
        self.selection = None;
        self.send(Event::TorrentFiles(Vec::new()));
        self.send(Event::SourceSelected {
            kind: None,
            title: String::new(),
            playable: false,
            suggested_filename: String::new(),
        });
        let item = parse_library_target(id)
            .and_then(|target| self.storage.library_target(&target).ok().flatten());
        let Some(item) = item else {
            self.status(Level::Error, "Запись библиотеки не найдена.");
            return;
        };
        let target = item.target;
        self.media = item.media.map(|record| Media {
            id: record.id.as_str().to_owned(),
            title: record.title,
            original_title: record.original_title,
            year: record.year,
            kind: if record.kind == kadr_storage::MediaKind::Series {
                MediaKind::Series
            } else {
                MediaKind::Movie
            },
            description: record.description,
            poster_url: record.poster_url,
            attribution: record.attribution,
            episodes: Vec::new(),
            license: None,
            source_page: String::new(),
        });
        self.episode = target.episode.map(|key| Episode {
            season: key.season,
            number: key.episode,
            title: format!("S{:02}E{:02}", key.season, key.episode),
            description: String::new(),
        });
        self.episode_explicit = false;
        self.send(Event::MediaSelected {
            media: self.media.clone(),
            favorite: item.entry.favorite,
        });
        let saved = self.storage.saved_file(&target).ok().flatten();
        self.send(Event::LibraryOpened {
            source_available: saved.is_some() || item.source.is_some(),
        });
        if let Some(saved) = saved {
            let source_id = item
                .source
                .as_ref()
                .and_then(|source| source.source_id().ok())
                .or(item.entry.last_source_id)
                .unwrap_or_else(|| SourceId::new("saved").expect("constant ID"));
            let filename = saved
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            self.selection = Some(Selection {
                origin: Origin::Saved {
                    path: saved.path,
                    source_id,
                },
                file_id: None,
                title: filename.clone(),
                filename,
                target: Some(target),
            });
            self.send(Event::TorrentFiles(Vec::new()));
            self.publish_selection();
        } else if let Some(descriptor) = item.source {
            let file_id = match &descriptor {
                StoredSource::TorrentFile { file_id, .. }
                | StoredSource::TorrentLink { file_id, .. } => *file_id,
                _ => None,
            };
            match descriptor.manual_source() {
                Ok(source) => {
                    self.select_source(source, file_id, Some(target), cancel)
                        .await
                }
                Err(_) => self.status(
                    Level::Error,
                    "Сохранённый источник повреждён. Добавьте другой; история сохранена.",
                ),
            }
        } else {
            self.status(
                Level::Warning,
                "У записи пока нет источника. Откройте карточку и добавьте ссылку или .torrent.",
            );
        }
    }

    fn update_settings(&mut self, settings: Settings) {
        if settings.validate().is_err() {
            self.reject_settings("Проверьте лимиты, путь к кэшу и сетевые настройки.");
            return;
        }
        if self.active.is_some()
            && (settings.network != self.settings.network
                || settings.cache_limit_bytes != self.settings.cache_limit_bytes
                || settings.cache_directory != self.settings.cache_directory)
        {
            let message = "Остановите сессию перед изменением сети, пути или лимита кэша. Выбор плеера можно менять во время просмотра.";
            self.send(Event::PreferencesRejected {
                settings: self.settings.clone(),
                message: message.into(),
            });
            self.status(Level::Warning, message);
            return;
        }
        if settings.network.torrents_enabled && engine_options(&settings).is_err() {
            self.reject_settings(
                "Лимит BitTorrent: paused, unlimited или 16 384…4 294 967 295 байт/с.",
            );
            return;
        }
        let catalog = match Client::new(Options {
            enabled: settings.catalog_enabled,
        }) {
            Ok(client) => Arc::new(client),
            Err(_) => {
                self.reject_settings("Не удалось настроить каталог; прежние настройки оставлены.");
                return;
            }
        };
        let changed_root = settings.cache_directory != self.settings.cache_directory;
        let replacement = if changed_root {
            match open_cache(&self.paths, &settings) {
                Ok(cache) => Some(Arc::new(cache)),
                Err(error) => {
                    self.reject_settings(&format!("Новый каталог кэша недоступен: {error}"));
                    return;
                }
            }
        } else {
            if let Err(error) = self.cache.set_limit(settings.cache_limit_bytes) {
                self.reject_settings(&error.to_string());
                return;
            }
            None
        };
        if changed_root && let Err(error) = self.cache.clear_inactive() {
            self.reject_settings(&format!(
                "Не удалось освободить прежний временный кэш: {error}"
            ));
            return;
        }
        if self.storage.save_settings(&settings).is_err() {
            let _ = self.cache.set_limit(self.settings.cache_limit_bytes);
            self.reject_settings("Не удалось сохранить настройки; прежние значения оставлены.");
            return;
        }
        if let Some(cache) = replacement {
            self.cache = cache;
        }
        self.settings = settings;
        crate::posters::set_enabled(self.settings.catalog_enabled);
        self.catalog = catalog;
        self.send(Event::PreferencesSaved(self.settings.clone()));
        if !self.settings.catalog_enabled {
            self.sources.clear();
            self.send(Event::SearchState(AsyncState::Error(
                "Каталог отключён в Настройках. Ручные источники и локальная библиотека доступны."
                    .into(),
            )));
            self.send(Event::SourcesState(AsyncState::Ready(Vec::new())));
        }
        if let Ok(usage) = self.cache.usage() {
            self.send(Event::CacheUsage(usage));
        }
        self.publish_selection();
    }

    fn reject_settings(&self, message: &str) {
        self.send(Event::PreferencesRejected {
            settings: self.settings.clone(),
            message: message.into(),
        });
        self.status(Level::Error, message);
    }

    fn torrent_options(&self) -> io::Result<kadr_torrent::engine::EngineOptions> {
        let mut options = match &self.network_override {
            Some(options) => options.clone(),
            None => engine_options(&self.settings)?,
        };
        options.network_enabled = self.settings.network.torrents_enabled;
        Ok(options)
    }
}

fn cache_root(paths: &AppPaths, settings: &Settings) -> PathBuf {
    settings
        .cache_directory
        .as_deref()
        .unwrap_or_else(|| paths.cache_dir())
        .join("torrents-v1")
}

fn managed_cache_ancestor(parent: &Path) -> bool {
    // Former roots remain disposable after changing preferences.
    parent.ancestors().any(|ancestor| {
        ancestor.join("manager.lock").is_file() && ancestor.join("index.json").is_file()
    })
}

fn parse_library_target(row: &str) -> Option<PlaybackTarget> {
    let (id, episode) = match row.split_once('@') {
        Some((id, encoded)) => {
            let (season, number) = encoded.split_once(':')?;
            (
                id,
                Some(EpisodeKey::new(season.parse().ok()?, number.parse().ok()?).ok()?),
            )
        }
        None => (row, None),
    };
    Some(PlaybackTarget {
        media_id: MediaId::new(id).ok()?,
        episode,
    })
}

fn open_cache(paths: &AppPaths, settings: &Settings) -> Result<CacheManager, kadr_cache::Error> {
    CacheManager::open(cache_root(paths, settings), settings.cache_limit_bytes)
}

fn media_record(media: &Media) -> Option<MediaRecord> {
    Some(MediaRecord {
        id: MediaId::new(media.id.clone()).ok()?,
        title: media.title.clone(),
        original_title: media.original_title.clone(),
        year: media.year,
        kind: if media.kind == MediaKind::Series {
            kadr_storage::MediaKind::Series
        } else {
            kadr_storage::MediaKind::Movie
        },
        description: media.description.clone(),
        poster_url: media.poster_url.clone(),
        attribution: format!(
            "{}{}{}",
            media.attribution,
            media
                .license
                .as_ref()
                .map(|license| format!(" · {license}"))
                .unwrap_or_default(),
            if media.source_page.is_empty() {
                String::new()
            } else {
                format!(" · {}", media.source_page)
            }
        ),
    })
}

fn storage_error(_: kadr_storage::Error) -> io::Error {
    io::Error::other("Не удалось сохранить или прочитать локальные данные. Проверьте доступ к БД.")
}

fn send_error(events: &UnboundedSender<Event>, message: &str) {
    let _ = events.unbounded_send(Event::Status(Level::Error, message.into()));
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

async fn read_metainfo(path: &Path) -> io::Result<Vec<u8>> {
    let file = LocalFile::open(path).await.map_err(|_| {
        io::Error::other("Файл .torrent недоступен или не является обычным файлом.")
    })?;
    if file.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other(
            "Файл .torrent превышает допустимый размер 8 MiB.",
        ));
    }
    let mut bytes = Vec::with_capacity(file.len() as usize);
    while bytes.len() < file.len() as usize {
        let chunk = file
            .read_at(bytes.len() as u64, 64 * 1024)
            .await
            .map_err(|_| io::Error::other("Не удалось прочитать .torrent."))?;
        if chunk.is_empty() {
            return Err(io::Error::other("Файл .torrent изменился во время чтения."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
