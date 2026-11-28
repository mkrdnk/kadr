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
mod resolver;
#[cfg(test)]
mod tests;
mod worker;

pub use dto::{
    CacheUsage, EngineDirectories, LibraryEpisode, LibraryItem, LibraryProgress, NetworkLimits,
    PlaybackPreferences, PlayerPreference, RateLimit, Settings, SourceCandidate,
};
pub use kadr_catalog::BrowseCatalog;
pub use kadr_core::source;
pub use kadr_storage::{SourcePreferences, TorznabConfig};

/// Provider-neutral metadata, without actionable source descriptors.
///
/// Source events use the engine's own [`crate::SourceCandidate`]. Topic IDs,
/// magnet URLs and tracker credentials remain behind the engine boundary.
///
/// ```compile_fail
/// use kadr_engine::catalog::SourceCandidate;
/// ```
pub mod catalog {
    pub use kadr_catalog::{
        CatalogPage, CatalogQuery, CatalogSort, Genre, YearRange, is_public_catalog_image,
    };
    pub use kadr_core::catalog::{Episode, Media, MediaKind};

    /// A separate, disposable cache for public artwork, outside the torrent cache.
    /// Returns a location only; the desktop image client owns access and eviction.
    pub fn artwork_cache_directory() -> std::io::Result<std::path::PathBuf> {
        super::AppPaths::discover()
            .map(|paths| paths.cache_dir().join("catalog-images-v1"))
            .map_err(|_| std::io::Error::other("Не удалось определить каталог кэша обложек."))
    }
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

/// A read-only validity check for snapshots already queued on the UI loop.
pub struct DiscoveryStamp(CancellationToken);

impl DiscoveryStamp {
    pub fn is_current(&self) -> bool {
        !self.0.is_cancelled()
    }
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
    SidebarCollapsed(bool),
    Status(Level, String),
    PlaybackActive(bool),
    /// A requested release needs a file choice, login, or another source.
    MovieWatchNeedsInput(DiscoveryStamp),
    BrowseState(AsyncState<BrowseCatalog>),
    CatalogPage {
        request: u64,
        result: Result<catalog::CatalogPage, String>,
    },
    CatalogGenres {
        kind: catalog::MediaKind,
        result: Result<Vec<catalog::Genre>, String>,
    },
    SearchState(AsyncState<Vec<Media>>),
    MediaSelected {
        media: Option<Box<Media>>,
        favorite: bool,
        episode: Option<(u16, u16)>,
    },
    SourcesState(AsyncState<Vec<SourceCandidate>>),
    /// Cumulative provider results. Only the originating target should render
    /// this snapshot; a zero pending count marks the end of the search.
    SourceDiscovery {
        stamp: DiscoveryStamp,
        media_id: String,
        episode: Option<(u16, u16)>,
        rows: Vec<SourceCandidate>,
        pending: usize,
        succeeded: usize,
        failures: Vec<String>,
        /// Informational outcomes from successful providers, not failures.
        notices: Vec<String>,
    },
    SourcePreferences(SourcePreferences),
    PlaybackRecommendation {
        stamp: DiscoveryStamp,
        index: Option<usize>,
    },
    RuTrackerLogin {
        message: Option<String>,
        pending: bool,
    },
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
    /// Fetch bounded provider shelves in response to explicit user action.
    Browse,
    CatalogPage {
        request: u64,
        query: catalog::CatalogQuery,
    },
    CatalogGenres(catalog::MediaKind),
    Search(String),
    /// Supply a local Trakt card to command-boundary tests without API credentials.
    #[cfg(test)]
    OpenFixtureMovie(Box<kadr_core::catalog::Media>),
    OpenMedia(String),
    /// Warm only RuTracker on an explicitly opened movie. Never starts playback.
    PrefetchMovieSources(String),
    CancelMoviePrefetch,
    FindSources,
    DiscoverOnOpen,
    RetrySources,
    SelectEpisode(u16, u16),
    SelectSource(ManualSource),
    ImportSource {
        source: ManualSource,
        attach: bool,
    },
    ChooseSource(usize),
    /// Explicit Watch action, bound to the winning handle of a completed search.
    WatchRelease(usize),
    RecommendPlayback(PlaybackPreferences),
    WatchRecommended {
        index: usize,
        preferences: PlaybackPreferences,
        resume: bool,
    },
    WatchSource {
        index: usize,
        preferences: PlaybackPreferences,
        resume: bool,
    },
    /// Submit the user's own credentials. The command intentionally has no Debug.
    SignInRuTracker {
        username: String,
        password: String,
    },
    ForgetRuTracker,
    ChooseTorrentFile(usize),
    AssignEpisode(u16, u16),
    Play {
        resume: bool,
    },
    PlayWithPreferences {
        resume: bool,
        preferences: PlaybackPreferences,
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
    SetSidebarCollapsed(bool),
    SetSourcePreferences(SourcePreferences),
    ClearCache,
    ClearHistory,
    UnlinkMedia,
    CancelSources,
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
    browse_stop: CancellationToken,
    catalog_pages: [CancellationToken; 2],
    genre_stop: CancellationToken,
    catalog_stop: CancellationToken,
    movie_prefetch_stop: CancellationToken,
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
        let catalog_stop = shutdown.child_token();
        Ok((
            Self {
                commands,
                browse_stop: shutdown.child_token(),
                catalog_pages: std::array::from_fn(|_| shutdown.child_token()),
                genre_stop: shutdown.child_token(),
                movie_prefetch_stop: catalog_stop.child_token(),
                catalog_stop,
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
            Command::PrefetchMovieSources(_) => {
                if self.catalog_stop.is_cancelled() {
                    self.catalog_stop = self.shutdown.child_token();
                }
                if self.movie_prefetch_stop.is_cancelled() {
                    self.movie_prefetch_stop = self.catalog_stop.child_token();
                }
                self.movie_prefetch_stop.clone()
            }
            Command::CancelMoviePrefetch => {
                self.movie_prefetch_stop.cancel();
                self.shutdown.child_token()
            }
            Command::Browse => {
                self.browse_stop.cancel();
                self.browse_stop = self.shutdown.child_token();
                self.browse_stop.clone()
            }
            Command::CatalogPage { query, .. } => {
                let stop =
                    &mut self.catalog_pages[usize::from(query.kind == catalog::MediaKind::Series)];
                stop.cancel();
                *stop = self.shutdown.child_token();
                stop.clone()
            }
            Command::CatalogGenres(_) => {
                if self.genre_stop.is_cancelled() {
                    self.genre_stop = self.shutdown.child_token();
                }
                self.genre_stop.child_token()
            }
            Command::FindSources | Command::DiscoverOnOpen => {
                // The worker coalesces repeated clicks for the current target.
                self.source_stop.cancel();
                if self.catalog_stop.is_cancelled() {
                    self.catalog_stop = self.shutdown.child_token();
                }
                self.catalog_stop.clone()
            }
            Command::SelectEpisode(_, _) | Command::AssignEpisode(_, _) => {
                // A synchronous target change invalidates previous IO. It
                // must itself survive the immediately following Find/Select:
                // that command relies on the target already queued before it.
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.shutdown.child_token()
            }
            Command::Search(_) | Command::OpenMedia(_) | Command::RetrySources => {
                if !matches!(&command, Command::Search(_)) {
                    self.source_stop.cancel();
                }
                self.catalog_stop.cancel();
                self.catalog_stop = self.shutdown.child_token();
                self.catalog_stop.clone()
            }
            Command::SelectSource(_)
            | Command::ImportSource { .. }
            | Command::ChooseSource(_)
            | Command::OpenLibrary(_)
            | Command::SignInRuTracker { .. }
            | Command::ForgetRuTracker
            | Command::UnlinkMedia => {
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.source_stop = self.shutdown.child_token();
                self.source_stop.clone()
            }
            Command::Play { .. }
            | Command::PlayWithPreferences { .. }
            | Command::WatchRelease(_)
            | Command::WatchRecommended { .. }
            | Command::WatchSource { .. } => {
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
            Command::CancelSources => {
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.shutdown.child_token()
            }
            Command::SetSourcePreferences(_) => {
                self.catalog_stop.cancel();
                self.source_stop.cancel();
                self.shutdown.child_token()
            }
            Command::UpdateSettings(settings) => {
                if !settings.catalog_enabled {
                    self.browse_stop.cancel();
                    for stop in &self.catalog_pages {
                        stop.cancel();
                    }
                    self.genre_stop.cancel();
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
