//! UI-independent command boundary. One worker owns SQLite, sessions and cache.
mod playback;
#[cfg(test)]
mod tests;
mod worker;

use std::{path::PathBuf, thread};

use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use kadr_cache::CacheUsage;
use kadr_core::{
    catalog::{Media, SourceCandidate},
    source::{ManualSource, SourceKind},
};
use kadr_storage::{AppPaths, LibraryItem, PlayerPreference, Settings};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub enum AsyncState<T> {
    Loading,
    Ready(T),
    Error(String),
}

pub struct FileChoice {
    pub index: usize,
    pub name: String,
    pub len: u64,
    pub season: Option<u16>,
    pub episode: Option<u16>,
    pub ambiguous: bool,
}

pub struct TransferStatus {
    pub stage: String,
    pub download_bps: Option<u64>,
    pub upload_bps: Option<u64>,
    pub peers: Option<usize>,
    pub done: Option<u64>,
    pub total: Option<u64>,
    pub buffered: Option<u64>,
    pub saving: String,
}

pub enum Event {
    Ready {
        settings: Settings,
        cache: Option<CacheUsage>,
    },
    PreferencesSaved(Settings),
    PreferencesRejected {
        settings: Settings,
        message: String,
    },
    Status(Level, String),
    PlaybackActive(bool),
    SearchState(AsyncState<Vec<Media>>),
    MediaSelected {
        media: Option<Media>,
        favorite: bool,
    },
    SourcesState(AsyncState<Vec<SourceCandidate>>),
    SourceSelected {
        kind: Option<SourceKind>,
        title: String,
        playable: bool,
        suggested_filename: String,
    },
    TorrentFiles(Vec<FileChoice>),
    Library(Vec<LibraryItem>),
    LibraryOpened {
        source_available: bool,
    },
    Transfer(Option<TransferStatus>),
    CacheUsage(CacheUsage),
}

#[derive(Clone, Copy, Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

pub enum Command {
    Search(String),
    OpenMedia(String),
    FindSources,
    SelectEpisode(u16, u16),
    SelectSource(ManualSource),
    ChooseSource(usize),
    ChooseTorrentFile(usize),
    AssignEpisode(u16, u16),
    Play {
        resume: bool,
    },
    Stop,
    OpenIn(PlayerPreference),
    SaveTo(PathBuf),
    SetFavorite {
        media_id: String,
        favorite: Option<bool>,
    },
    OpenLibrary(String),
    UpdateSettings(Settings),
    ClearCache,
    ClearHistory,
    UnlinkMedia,
    Cancel,
}

struct Request {
    command: Command,
    cancel: CancellationToken,
}

pub struct Backend {
    commands: mpsc::Sender<Request>,
    shutdown: CancellationToken,
    catalog_stop: CancellationToken,
    source_stop: CancellationToken,
    playback_stop: CancellationToken,
    save_stop: CancellationToken,
    thread: Option<thread::JoinHandle<()>>,
}

impl Backend {
    pub fn start() -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        Self::start_with(None, false)
    }

    fn start_with(
        paths: Option<AppPaths>,
        headless: bool,
    ) -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        Self::start_with_network(paths, headless, None)
    }

    fn start_with_network(
        paths: Option<AppPaths>,
        headless: bool,
        network: Option<kadr_torrent::engine::EngineOptions>,
    ) -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        let (commands, receiver) = mpsc::channel(32);
        let (events, output) = unbounded();
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let thread = thread::Builder::new()
            .name("kadr-media".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(worker::run(
                        receiver,
                        events,
                        worker_shutdown,
                        paths,
                        headless,
                        network,
                    )),
                    Err(_) => {
                        let _ = events.unbounded_send(Event::Status(
                            Level::Error,
                            "Не удалось запустить медиаядро.".into(),
                        ));
                    }
                }
            })?;
        Ok((
            Self {
                commands,
                catalog_stop: shutdown.child_token(),
                source_stop: shutdown.child_token(),
                playback_stop: shutdown.child_token(),
                save_stop: shutdown.child_token(),
                shutdown,
                thread: Some(thread),
            },
            output,
        ))
    }

    pub fn dispatch(&mut self, command: Command) -> bool {
        // Cancellation bypasses the queue: a pending metadata/IPC operation
        // cannot prevent Stop, a newer search, or application shutdown.
        let cancel = match &command {
            Command::Search(_)
            | Command::OpenMedia(_)
            | Command::FindSources
            | Command::SelectEpisode(_, _) => {
                if matches!(&command, Command::OpenMedia(_)) {
                    self.source_stop.cancel();
                }
                self.catalog_stop.cancel();
                self.catalog_stop = self.shutdown.child_token();
                self.catalog_stop.clone()
            }
            Command::SelectSource(_) | Command::ChooseSource(_) | Command::OpenLibrary(_) => {
                self.source_stop.cancel();
                self.source_stop = self.shutdown.child_token();
                self.source_stop.clone()
            }
            Command::Play { .. } => {
                self.playback_stop.cancel();
                self.playback_stop = self.shutdown.child_token();
                self.playback_stop.clone()
            }
            Command::OpenIn(_) => {
                if self.playback_stop.is_cancelled() {
                    self.playback_stop = self.shutdown.child_token();
                }
                self.playback_stop.clone()
            }
            Command::SaveTo(_) => {
                self.save_stop.cancel();
                self.save_stop = self.shutdown.child_token();
                self.save_stop.clone()
            }
            Command::Stop => {
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.playback_stop.cancel();
                self.save_stop.cancel();
                self.shutdown.child_token()
            }
            Command::Cancel => {
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.save_stop.cancel();
                self.shutdown.child_token()
            }
            Command::UpdateSettings(settings) => {
                if !settings.catalog_enabled {
                    self.catalog_stop.cancel();
                }
                if !settings.network.torrents_enabled {
                    self.source_stop.cancel();
                }
                self.shutdown.child_token()
            }
            _ => self.shutdown.child_token(),
        };
        self.commands.try_send(Request { command, cancel }).is_ok()
    }

    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// GPUI gives quit observers 100ms. Join after its platform loop has exited.
    pub fn join(&mut self) -> bool {
        self.cancel();
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancel();
    }
}
