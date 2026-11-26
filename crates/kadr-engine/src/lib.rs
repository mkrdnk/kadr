//! UI-independent media engine. One worker owns SQLite, sessions and cache.
//!
//! Start an [`Engine`], consume its [`Event`] stream, and send [`Command`] values
//! after [`Event::Ready`]. Enqueuing a command is not an acknowledgement: settings,
//! playback and library changes are confirmed by events. The client owns native
//! dialogs, presentation state and any GUI-framework integration.
//!
//! [`Engine::cancel`] is non-blocking. Call [`Engine::join`] outside the UI event
//! loop to wait for player teardown, pending saves and storage cleanup.
mod dto;
mod playback;
#[cfg(test)]
mod tests;
mod worker;

pub use dto::{
    CacheUsage, EngineDirectories, LibraryEpisode, LibraryItem, LibraryProgress, NetworkLimits,
    PlayerPreference, RateLimit, Settings, SourceCandidate,
};
pub use kadr_core::source;

/// Provider-neutral metadata, without actionable source descriptors.
///
/// Source events use the engine's own [`crate::SourceCandidate`], not the internal
/// catalog type that carries playback URLs.
///
/// ```compile_fail
/// use kadr_engine::catalog::SourceCandidate;
/// ```
pub mod catalog {
    pub use kadr_core::catalog::{Episode, Media, MediaKind};
}

use std::{path::PathBuf, thread};

use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use kadr_core::{
    catalog::Media,
    source::{ManualSource, SourceKind},
};
use kadr_storage::AppPaths;
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

/// Single-client command handle with an independent worker and event stream.
pub struct Engine {
    commands: mpsc::Sender<Request>,
    shutdown: CancellationToken,
    catalog_stop: CancellationToken,
    source_stop: CancellationToken,
    playback_stop: CancellationToken,
    save_stop: CancellationToken,
    thread: Option<thread::JoinHandle<()>>,
}

impl Engine {
    /// Starts using the platform's application directories.
    ///
    /// Startup I/O failures arrive as events followed by stream closure, without
    /// a `Ready` event. The returned error only covers failure to spawn the worker.
    pub fn start() -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        Self::start_with(None, false)
    }

    /// Starts the engine using validated, user-selected application roots.
    pub fn start_with_directories(
        directories: EngineDirectories,
    ) -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        let paths = AppPaths::from_roots(
            directories.data_directory,
            directories.config_directory,
            directories.cache_directory,
        )
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Недопустимые пользовательские каталоги.",
            )
        })?;
        Self::start_with(Some(paths), false)
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

    /// Tries to enqueue a command without blocking the caller.
    ///
    /// Returns false if the queue is full or the worker has stopped. Cancellation
    /// of superseded work happens immediately, even if the queue rejects a command.
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

    /// Requests shutdown without blocking; use `join` to wait for completion.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Cancels outstanding work and waits for owned resources to stop.
    pub fn join(&mut self) -> bool {
        self.cancel();
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.cancel();
    }
}
