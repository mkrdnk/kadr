//! Consent-gated production torrent preparation and selected-file playback.
//!
//! Preparation never creates payload files. The caller reserves `total_bytes`
//! in its cache before `start`, and exclusively owns the supplied cache root.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use kadr_core::torrent::TorrentRelativePath;
use kadr_media::ByteSource;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerMode,
    ListenerOptions, ManagedTorrent, Session, SessionOptions, TorrentStatsState,
    limits::LimitsConfig,
};
pub use tokio_util::sync::CancellationToken;

use crate::{TorrentByteSource, TorrentFile, reject_symlink};

/// `Paused` is not a zero-valued rate: zero in upstream means unlimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimit {
    Paused,
    Unlimited,
    Bps(u32),
}

impl RateLimit {
    fn bps(self) -> Result<Option<NonZeroU32>> {
        match self {
            Self::Bps(1..16_384) => {
                bail!("torrent rate limit must be at least 16384 bytes per second");
            }
            Self::Bps(bps) => NonZeroU32::new(bps)
                .map(Some)
                .ok_or_else(|| anyhow!("rate limit must be positive")),
            _ => Ok(None),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineOptions {
    pub download_limit: RateLimit,
    pub upload_limit: RateLimit,
    pub peer_limit: usize,
    /// Explicit permission for all BitTorrent networking, including metadata.
    pub network_enabled: bool,
    /// DHT and trackers. False also binds sockets to the loopback interface and
    /// accepts only explicit loopback peers; intended for deterministic tests.
    pub discovery_enabled: bool,
    pub initial_peers: Vec<SocketAddr>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            download_limit: RateLimit::Unlimited,
            upload_limit: RateLimit::Paused,
            peer_limit: 40,
            network_enabled: false,
            discovery_enabled: true,
            initial_peers: Vec::new(),
        }
    }
}

impl EngineOptions {
    fn validate(&self) -> Result<()> {
        if !self.network_enabled {
            bail!("BitTorrent networking requires consent");
        }
        if self.peer_limit == 0 {
            bail!("peer limit must be positive");
        }
        self.download_limit.bps()?;
        self.upload_limit.bps()?;
        if self.initial_peers.iter().any(|peer| {
            peer.port() == 0
                || (!self.discovery_enabled && (!peer.is_ipv4() || !peer.ip().is_loopback()))
        }) {
            bail!("invalid initial peer");
        }
        Ok(())
    }

    fn limits(&self) -> Result<LimitsConfig> {
        Ok(LimitsConfig {
            download_bps: self.download_limit.bps()?,
            upload_bps: self.upload_limit.bps()?,
        })
    }
}

/// Validated metadata. Names are sensitive and deliberately redacted by Debug.
/// No live session or file handles survive preparation.
#[derive(Clone)]
pub struct PreparedTorrent {
    pub info_hash: String,
    pub files: Vec<TorrentFile>,
    pub total_bytes: u64,
    metainfo: Bytes,
    resolved_peers: Vec<SocketAddr>,
}

impl std::fmt::Debug for PreparedTorrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedTorrent")
            .field("info_hash", &self.info_hash)
            .field("files", &self.files)
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}

impl PreparedTorrent {
    /// Pure offline parse: no sockets, directories or payload files.
    pub fn from_bytes(bytes: Bytes) -> Result<Self> {
        let meta = librqbit::torrent_from_bytes(&bytes)
            .map_err(|_| anyhow!("cannot parse torrent metadata"))?;
        let info_hash = meta.info_hash.as_string();
        let info = meta
            .info
            .data
            .validate()
            .map_err(|_| anyhow!("invalid torrent metadata"))?;
        let mut files = Vec::new();
        let mut paths = HashSet::new();
        let mut total_bytes = 0u64;
        for (id, file) in info.iter_file_details().enumerate() {
            // Validate components separately too: a literal '/' in a bencoded
            // component must not change the on-disk tree after joining.
            let components = file.filename.to_vec();
            for component in &components {
                if component.contains('/') {
                    bail!("invalid torrent file path");
                }
                TorrentRelativePath::new(component.clone())
                    .map_err(|_| anyhow!("invalid torrent file path"))?;
            }
            let path = TorrentRelativePath::new(components.join("/"))
                .map_err(|_| anyhow!("invalid torrent file path"))?;
            if file.attrs().symlink || !paths.insert(path.as_str().to_lowercase()) {
                bail!("conflicting or unsupported torrent file paths");
            }
            total_bytes = total_bytes
                .checked_add(file.len)
                .ok_or_else(|| anyhow!("torrent size overflow"))?;
            files.push(TorrentFile {
                id,
                relative_filename: path.as_path().to_owned(),
                len: file.len,
            });
        }
        // Reject file/directory collisions, including case-folded names for
        // portability to common macOS filesystems.
        for path in &paths {
            for (index, _) in path.match_indices('/') {
                if paths.contains(&path[..index]) {
                    bail!("conflicting torrent file paths");
                }
            }
        }
        Ok(Self {
            info_hash,
            files,
            total_bytes,
            metainfo: bytes,
            resolved_peers: Vec::new(),
        })
    }

    /// BEP 9 metadata exchange only. `list_only` stops before storage creation.
    /// Callers should bound this future with their metadata deadline.
    pub async fn from_magnet(
        magnet: &str,
        options: EngineOptions,
        cancel: CancellationToken,
    ) -> Result<Self> {
        options.validate()?;
        if !magnet.starts_with("magnet:?") {
            bail!("invalid magnet link");
        }
        // No caller-controlled URL is passed to the upstream HTTP fetch branch.
        let token = cancel.child_token();
        let _cancel_on_drop = token.clone().drop_guard();
        let session = create_session(PathBuf::new(), &options, token.clone()).await?;
        let lease = SessionLease(session.clone());
        let listed = tokio::select! {
            biased;
            () = token.cancelled() => bail!("torrent metadata resolution cancelled"),
            result = session.add_torrent(
                AddTorrent::from_url(magnet),
                Some(AddTorrentOptions {
                    list_only: true,
                    sub_folder: Some("payload".into()),
                    disable_trackers: !options.discovery_enabled,
                    initial_peers: Some(options.initial_peers.clone()),
                    peer_limit: Some(options.peer_limit),
                    ..Default::default()
                }),
            ) => result.map_err(|_| anyhow!("cannot resolve torrent metadata"))?,
        };
        let AddTorrentResponse::ListOnly(listed) = listed else {
            bail!("unexpected torrent metadata response");
        };
        let mut prepared = Self::from_bytes(listed.torrent_bytes)?;
        prepared.resolved_peers = listed.seen_peers;
        lease.0.stop().await;
        Ok(prepared)
    }

    /// Rehashes existing blocks in the same cache root rather than trusting a
    /// resume bitmap. Only the selected file (and boundary pieces) is wanted.
    /// The caller must reserve the WHOLE torrent size and serialize ownership
    /// of this root, including all concurrent opens of the same info hash.
    pub async fn start(
        &self,
        selected_file: usize,
        cache_output: PathBuf,
        options: EngineOptions,
        cancel: CancellationToken,
    ) -> Result<PlaybackTorrent> {
        options.validate()?;
        // Reparse our immutable original bytes: public display metadata may
        // have been edited by the caller and is never a filesystem authority.
        let metadata = Self::from_bytes(self.metainfo.clone())?;
        let selected = metadata
            .files
            .get(selected_file)
            .ok_or_else(|| anyhow!("invalid selected file"))?;
        let token = cancel.child_token();
        let cancel_guard = token.clone().drop_guard();
        if token.is_cancelled() {
            bail!("torrent start cancelled");
        }
        check_output(&cache_output, &metadata.files).await?;
        let session = create_session(cache_output, &options, token.clone()).await?;
        let lease = SessionLease(session.clone());
        let mut initial_peers = options.initial_peers.clone();
        initial_peers.extend(self.resolved_peers.iter().copied().filter(|peer| {
            options.discovery_enabled || (peer.is_ipv4() && peer.ip().is_loopback())
        }));
        let added = tokio::select! {
            biased;
            () = token.cancelled() => bail!("torrent start cancelled"),
            result = session.add_torrent(
                AddTorrent::from_bytes(self.metainfo.clone()),
                Some(AddTorrentOptions {
                    sub_folder: Some("payload".into()),
                    only_files: Some(vec![selected_file]),
                    overwrite: true,
                    paused: options.download_limit == RateLimit::Paused,
                    initial_peers: Some(initial_peers),
                    disable_trackers: !options.discovery_enabled,
                    peer_limit: Some(options.peer_limit),
                    ..Default::default()
                }),
            ) => result.map_err(|_| anyhow!("cannot start torrent files"))?,
        };
        let handle = added
            .into_handle()
            .ok_or_else(|| anyhow!("torrent handle unavailable"))?;
        tokio::select! {
            biased;
            () = token.cancelled() => bail!("torrent start cancelled"),
            result = handle.wait_until_initialized() => {
                result.map_err(|_| anyhow!("cannot initialize torrent files"))?;
            }
        }
        let source: Arc<dyn ByteSource> = Arc::new(TorrentByteSource {
            session: session.clone(),
            handle: handle.clone(),
            file_id: selected_file,
            length: selected.len,
        });
        cancel_guard.disarm();
        Ok(PlaybackTorrent {
            source,
            lease,
            handle,
            selected_file,
            selected_len: selected.len,
            total_bytes: metadata.total_bytes,
            upload_disabled: options.upload_limit == RateLimit::Paused,
            stopped: AtomicBool::new(false),
            control: tokio::sync::Mutex::new(Control {
                download: options.download_limit,
                upload: options.upload_limit,
                user_paused: false,
            }),
        })
    }
}

/// Safe UI state, never upstream error strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineState {
    Initializing,
    Downloading,
    Ready,
    Paused,
    Stopped,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineStats {
    pub state: EngineState,
    pub progress_bytes: u64,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_bps: u64,
    pub upload_bps: u64,
    pub peers: usize,
    pub selected_file_bytes: u64,
    pub selected_file_ready: bool,
}

struct Control {
    download: RateLimit,
    upload: RateLimit,
    user_paused: bool,
}

pub struct PlaybackTorrent {
    pub source: Arc<dyn ByteSource>,
    lease: SessionLease,
    handle: Arc<ManagedTorrent>,
    selected_file: usize,
    selected_len: u64,
    total_bytes: u64,
    upload_disabled: bool,
    stopped: AtomicBool,
    control: tokio::sync::Mutex<Control>,
}

impl PlaybackTorrent {
    pub fn stats(&self) -> EngineStats {
        let stats = self.handle.stats();
        let selected_bytes = stats
            .file_progress
            .get(self.selected_file)
            .copied()
            .unwrap_or_default();
        let ready = selected_bytes == self.selected_len
            && !matches!(
                stats.state,
                TorrentStatsState::Initializing { .. } | TorrentStatsState::Error
            );
        let stopped = self.lease.0.cancellation_token().is_cancelled();
        let state = if stopped {
            EngineState::Stopped
        } else {
            match stats.state {
                TorrentStatsState::Initializing { .. } => EngineState::Initializing,
                TorrentStatsState::Paused => EngineState::Paused,
                TorrentStatsState::Error => EngineState::Error,
                TorrentStatsState::Live if ready => EngineState::Ready,
                TorrentStatsState::Live => EngineState::Downloading,
            }
        };
        let (download_bps, upload_bps, peers) = stats.live.as_ref().map_or((0, 0, 0), |live| {
            (
                live.download_speed.as_bytes(),
                live.upload_speed.as_bytes(),
                live.snapshot.peer_stats.live as usize,
            )
        });
        EngineStats {
            state,
            progress_bytes: stats.progress_bytes,
            total_bytes: self.total_bytes,
            uploaded_bytes: stats.uploaded_bytes,
            download_bps,
            upload_bps,
            peers,
            selected_file_bytes: selected_bytes,
            selected_file_ready: ready,
        }
    }

    pub fn selected_file_ready(&self) -> bool {
        self.stats().selected_file_ready
    }

    /// Full selected file readiness, suitable for copying to permanent storage.
    /// `source` remains the only supported verified-byte access capability.
    pub async fn wait_until_ready(&self, cancel: CancellationToken) -> Result<()> {
        loop {
            let stats = self.stats();
            if stats.selected_file_ready {
                return Ok(());
            }
            if matches!(stats.state, EngineState::Error | EngineState::Stopped) {
                bail!("torrent file unavailable");
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => bail!("torrent file wait cancelled"),
                () = self.lease.0.cancellation_token().cancelled() => bail!("torrent stopped"),
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    pub async fn pause(&self) -> Result<()> {
        let mut control = self.control.lock().await;
        self.ensure_running()?;
        if !self.handle.is_paused() {
            self.lease
                .0
                .pause(&self.handle)
                .await
                .map_err(|_| anyhow!("cannot pause torrent"))?;
        }
        control.user_paused = true;
        Ok(())
    }

    pub async fn resume(&self) -> Result<()> {
        let mut control = self.control.lock().await;
        self.ensure_running()?;
        if control.download == RateLimit::Paused
            || (control.upload == RateLimit::Paused && !self.upload_disabled)
        {
            bail!("change paused network limits before resuming");
        }
        if self.handle.is_paused() {
            self.lease
                .0
                .unpause(&self.handle)
                .await
                .map_err(|_| anyhow!("cannot resume torrent"))?;
        }
        control.user_paused = false;
        Ok(())
    }

    /// Live numeric rate changes. Pausing uploads on a session originally
    /// allowing uploads conservatively pauses both directions. A session
    /// created with uploads disabled must be restarted to enable uploads.
    pub async fn set_limits(&self, download: RateLimit, upload: RateLimit) -> Result<()> {
        let download_bps = download.bps()?;
        let upload_bps = upload.bps()?;
        let mut control = self.control.lock().await;
        self.ensure_running()?;
        if self.upload_disabled && upload != RateLimit::Paused {
            bail!("restart torrent to enable uploads");
        }
        let pause = control.user_paused
            || download == RateLimit::Paused
            || (upload == RateLimit::Paused && !self.upload_disabled);
        if pause && !self.handle.is_paused() {
            self.lease
                .0
                .pause(&self.handle)
                .await
                .map_err(|_| anyhow!("cannot pause torrent"))?;
        }
        self.lease.0.ratelimits.set_download_bps(download_bps);
        self.lease.0.ratelimits.set_upload_bps(upload_bps);
        if !pause && self.handle.is_paused() {
            self.lease
                .0
                .unpause(&self.handle)
                .await
                .map_err(|_| anyhow!("cannot resume torrent"))?;
        }
        control.download = download;
        control.upload = upload;
        Ok(())
    }

    fn ensure_running(&self) -> Result<()> {
        if self.lease.0.cancellation_token().is_cancelled() {
            bail!("torrent stopped");
        }
        Ok(())
    }

    /// Idempotent bounded upstream shutdown. Stops source reads immediately;
    /// upstream stop then pauses torrents and allows a one-second grace period.
    pub async fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            self.lease.0.cancellation_token().cancel();
            self.lease.0.stop().await;
        }
    }
}

/// Cancellation is synchronous; best-effort async reaping also runs when a
/// preparation/start future is dropped after upstream spawned initialization.
struct SessionLease(Arc<Session>);

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.0.cancellation_token().cancel();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let session = self.0.clone();
            runtime.spawn(async move { session.stop().await });
        }
    }
}

async fn create_session(
    output: PathBuf,
    options: &EngineOptions,
    token: CancellationToken,
) -> Result<Arc<Session>> {
    let loopback_device = if options.discovery_enabled {
        None
    } else {
        Some(match std::env::consts::OS {
            "linux" => "lo".to_owned(),
            "macos" => "lo0".to_owned(),
            _ => bail!("local tests require loopback interface binding"),
        })
    };
    let new_session = Session::new_with_opts(
        output,
        SessionOptions {
            dht: options.discovery_enabled.then_some(DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
            bind_device_name: loopback_device,
            disable_trackers: !options.discovery_enabled,
            disable_local_service_discovery: true,
            persistence: None,
            fastresume: false,
            cancellation_token: Some(token.clone()),
            peer_limit: Some(options.peer_limit),
            ratelimits: options.limits()?,
            disable_upload: options.upload_limit == RateLimit::Paused,
            ipv4_only: !options.discovery_enabled,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (
                    if options.discovery_enabled {
                        Ipv4Addr::UNSPECIFIED
                    } else {
                        Ipv4Addr::LOCALHOST
                    },
                    0,
                )
                    .into(),
                enable_upnp_port_forwarding: false,
                ipv4_only: !options.discovery_enabled,
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    tokio::select! {
        biased;
        () = token.cancelled() => bail!("torrent session cancelled"),
        result = new_session => result.map_err(|_| anyhow!("cannot create torrent session")),
    }
}

async fn check_output(root: &Path, files: &[TorrentFile]) -> Result<()> {
    // Validate every existing ancestor, not just the final cache directory.
    // The cache manager must keep this tree private to avoid TOCTOU races.
    let mut path = PathBuf::new();
    for component in root.components() {
        path.push(component);
        reject_symlink(&path).await?;
    }
    reject_symlink(&root.join("payload")).await?;
    for file in files {
        let mut path = root.join("payload");
        for component in file.relative_filename.components() {
            path.push(component);
            reject_symlink(&path).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
