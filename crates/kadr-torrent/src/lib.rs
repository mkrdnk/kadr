//! Local-only P0 experiment, not the production torrent engine or cache.
//!
//! Only private `.torrent` bytes and explicit IPv4 loopback peers are accepted.
//! Each positional read owns a stream: no shared seek cursor, detached reader,
//! or availability wait survives cancellation of that read.

use std::{
    fmt,
    io::{self, SeekFrom},
    net::{Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, anyhow, bail};
use bytes::Bytes;
use kadr_media::{ByteSource, ReadFuture};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ListenerMode, ListenerOptions,
    ManagedTorrent, Session, SessionOptions,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Metadata for display/selection, never an HTTP path or filesystem capability.
#[derive(Clone, PartialEq, Eq)]
pub struct TorrentFile {
    pub id: usize,
    pub relative_filename: PathBuf,
    pub len: u64,
}

impl fmt::Debug for TorrentFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TorrentFile")
            .field("id", &self.id)
            .field("relative_filename", &"[redacted]")
            .field("len", &self.len)
            .finish()
    }
}

pub struct SelectedTorrent {
    pub files: Vec<TorrentFile>,
    pub source: Arc<TorrentByteSource>,
}

/// Numeric engine counters only, not kadr cache accounting. Upstream fatal
/// error strings may contain private filenames and never cross this boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TorrentCounters {
    pub progress_bytes: u64,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
    pub finished: bool,
    pub has_error: bool,
}

impl From<librqbit::TorrentStats> for TorrentCounters {
    fn from(stats: librqbit::TorrentStats) -> Self {
        Self {
            progress_bytes: stats.progress_bytes,
            uploaded_bytes: stats.uploaded_bytes,
            total_bytes: stats.total_bytes,
            finished: stats.finished,
            has_error: stats.error.is_some(),
        }
    }
}

/// An isolated session with discovery, trackers, persistence and UPnP disabled.
///
/// The supplied output directory must be dedicated to this experiment. This
/// wrapper does not enforce a storage budget or provide race-free confinement.
/// Call `stop` after shutting down HTTP; Drop requests cancellation only.
pub struct LocalSession {
    session: Arc<Session>,
    output_dir: PathBuf,
}

async fn reject_symlink(path: &Path) -> anyhow::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("symlink in torrent output path is not allowed");
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(anyhow!("cannot inspect torrent output path")),
    }
}

// Upstream registers initialization work with the session, not the add future.
// An interrupted/failed started add invalidates this whole experimental session
// rather than leaving an unowned download running after a caller's timeout.
struct CancelFailedAdd(Option<Arc<Session>>);

impl Drop for CancelFailedAdd {
    fn drop(&mut self) {
        if let Some(session) = &self.0 {
            session.cancellation_token().cancel();
        }
    }
}

impl LocalSession {
    pub async fn new(output_dir: PathBuf) -> anyhow::Result<Self> {
        // librqbit creates a wildcard IPv6 UDP tracker socket even with trackers
        // disabled. Interface binding confines that hidden listener as well as
        // peer sockets. Never fall back to an unrestricted interface.
        let device = match std::env::consts::OS {
            "linux" => "lo",
            "macos" => "lo0",
            _ => bail!("local torrent spike requires Linux or macOS loopback interface binding"),
        };
        reject_symlink(&output_dir).await?;
        let session = Session::new_with_opts(
            output_dir.clone(),
            SessionOptions {
                dht: None,
                bind_device_name: Some(device.into()),
                disable_trackers: true,
                disable_local_service_discovery: true,
                ipv4_only: true,
                persistence: None,
                listen: Some(ListenerOptions {
                    mode: ListenerMode::TcpOnly,
                    listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                    enable_upnp_port_forwarding: false,
                    ipv4_only: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| anyhow!("cannot create local torrent session"))?;
        Ok(Self {
            session,
            output_dir,
        })
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.session.listen_addr().expect("TCP listener configured")
    }

    /// Validate before starting any peer activity. Requiring `private=1` is
    /// important: librqbit has no public global PEX-off option; private torrents
    /// suppress both sending PEX and accepting PEX peers.
    ///
    /// `overwrite` permits rehashing existing files (e.g. our local seed). Never
    /// enable it for a directory containing unrelated user files.
    /// Callers own the deadline for metadata/initialization. Once a real add
    /// starts, failure or cancellation invalidates the entire session; stop and
    /// discard it. Pure input rejections and duplicate adds leave it usable.
    pub async fn add_selected(
        &self,
        torrent_bytes: Bytes,
        file_id: usize,
        peers: Vec<SocketAddr>,
        overwrite: bool,
    ) -> anyhow::Result<SelectedTorrent> {
        if self.session.cancellation_token().is_cancelled() {
            bail!("torrent session stopped; create a new session");
        }
        if peers
            .iter()
            .any(|p| !p.is_ipv4() || !p.ip().is_loopback() || p.port() == 0)
        {
            bail!("only explicit IPv4 loopback peers with nonzero ports are allowed");
        }
        // list_only parses and validates metainfo without starting a torrent.
        let listed = self
            .session
            .add_torrent(
                AddTorrent::from_bytes(torrent_bytes.clone()),
                Some(AddTorrentOptions {
                    list_only: true,
                    disable_trackers: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|_| anyhow!("cannot parse torrent metadata"))?;
        let AddTorrentResponse::ListOnly(listed) = listed else {
            bail!("expected list-only metadata response");
        };
        if !listed.info.info().private {
            bail!("local-only spike requires private=1 to disable peer exchange");
        }
        if file_id >= listed.info.iter_file_lengths().count() {
            bail!("invalid selected file");
        }
        // Conservative preflight only: a local process can still race these
        // checks. Do not use an attacker-writable output directory.
        reject_symlink(&self.output_dir).await?;
        let subfolder = listed
            .output_folder
            .strip_prefix(&self.output_dir)
            .context("torrent output must remain under the configured directory")?;
        let relative_files: Vec<_> = listed
            .info
            .iter_file_details()
            .map(|file| subfolder.join(file.filename.to_pathbuf()))
            .collect();
        for relative in relative_files {
            let mut path = self.output_dir.clone();
            for component in relative.components() {
                let Component::Normal(component) = component else {
                    bail!("torrent output path contains a non-normal component");
                };
                path.push(component);
                reject_symlink(&path).await?;
            }
        }
        let mut cleanup = CancelFailedAdd(Some(self.session.clone()));
        let add = self.session.add_torrent(
            AddTorrent::from_bytes(torrent_bytes),
            Some(AddTorrentOptions {
                only_files: Some(vec![file_id]),
                initial_peers: Some(peers),
                disable_trackers: true,
                overwrite,
                ..Default::default()
            }),
        );
        let added = tokio::select! {
            biased;
            () = self.session.cancellation_token().cancelled() => {
                bail!("torrent session stopped during add");
            }
            result = add => result.map_err(|_| anyhow!("cannot start torrent files"))?,
        };
        let handle = match added {
            AddTorrentResponse::Added(_, handle) => handle,
            AddTorrentResponse::AlreadyManaged(_, _) => {
                cleanup.0.take();
                bail!("torrent is already managed; selection and peers were not changed");
            }
            AddTorrentResponse::ListOnly(_) => bail!("expected a torrent handle"),
        };
        tokio::select! {
            biased;
            () = self.session.cancellation_token().cancelled() => {
                bail!("torrent session stopped during initialization");
            }
            result = handle.wait_until_initialized() => {
                result.map_err(|_| anyhow!("cannot initialize torrent files"))?
            },
        }
        let files = handle
            .with_metadata(|m| {
                m.file_infos
                    .iter()
                    .enumerate()
                    .map(|(id, f)| TorrentFile {
                        id,
                        relative_filename: f.relative_filename.clone(),
                        len: f.len,
                    })
                    .collect::<Vec<_>>()
            })
            .map_err(|_| anyhow!("torrent metadata unavailable"))?;
        let length = files.get(file_id).context("invalid selected file")?.len;
        cleanup.0.take();
        Ok(SelectedTorrent {
            files,
            source: Arc::new(TorrentByteSource {
                session: self.session.clone(),
                handle,
                file_id,
                length,
            }),
        })
    }

    /// Upstream pauses torrents, cancels tasks, then sleeps one second. This is
    /// best effort, not a join-all guarantee; callers should bound the operation.
    pub async fn stop(self) {
        self.session.stop().await;
    }
}

impl Drop for LocalSession {
    fn drop(&mut self) {
        self.session.cancellation_token().cancel();
    }
}

/// A stable selected file, backed exclusively by librqbit's verified pieces.
pub struct TorrentByteSource {
    session: Arc<Session>,
    handle: Arc<ManagedTorrent>,
    file_id: usize,
    length: u64,
}

impl TorrentByteSource {
    pub fn file_id(&self) -> usize {
        self.file_id
    }

    /// Engine counters, not kadr cache accounting.
    pub fn stats(&self) -> TorrentCounters {
        self.handle.stats().into()
    }
}

impl ByteSource for TorrentByteSource {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            if offset >= self.length || max_len == 0 {
                return Ok(Bytes::new());
            }
            let read = async {
                // Independent stream per read: seek changes upstream's built-in
                // 32 MiB lookahead; dropping this future deregisters the stream.
                let mut stream = self
                    .handle
                    .clone()
                    .stream(self.file_id)
                    .await
                    .map_err(|_| io::Error::other("torrent stream unavailable"))?;
                stream
                    .seek(SeekFrom::Start(offset))
                    .await
                    .map_err(|error| io::Error::new(error.kind(), "torrent seek failed"))?;
                let size = (max_len.min(64 * 1024) as u64).min(self.length - offset) as usize;
                let mut bytes = vec![0; size];
                // A single read returns a verified prefix, possibly only to the
                // piece boundary. HTTP already loops over short source reads.
                let count = stream
                    .read(&mut bytes)
                    .await
                    .map_err(|error| io::Error::new(error.kind(), "torrent read failed"))?;
                if count == 0 {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                bytes.truncate(count);
                Ok(Bytes::from(bytes))
            };
            tokio::select! {
                biased;
                () = self.session.cancellation_token().cancelled() => {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "torrent session stopped"))
                }
                result = read => result,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_do_not_expose_upstream_fatal_error_diagnostics() {
        let counters = TorrentCounters::from(librqbit::TorrentStats {
            state: librqbit::TorrentStatsState::Error,
            file_progress: vec![100],
            error: Some("write error at /SENSITIVE/private-video.mp4".into()),
            progress_bytes: 100,
            uploaded_bytes: 20,
            total_bytes: 1_000,
            finished: false,
            live: None,
        });
        assert!(counters.has_error);
        assert_eq!(counters.progress_bytes, 100);
        assert!(!format!("{counters:?}").contains("SENSITIVE"));
        assert!(!format!("{counters:?}").contains("private-video"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interrupted_add_guard_invalidates_session_but_success_disarms_it() {
        let directory = tempfile::tempdir().unwrap();
        let session = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            LocalSession::new(directory.path().into()),
        )
        .await
        .unwrap()
        .unwrap();
        let mut successful = CancelFailedAdd(Some(session.session.clone()));
        successful.0.take();
        drop(successful);
        assert!(!session.session.cancellation_token().is_cancelled());
        let interrupted = CancelFailedAdd(Some(session.session.clone()));
        drop(interrupted);
        assert!(session.session.cancellation_token().is_cancelled());
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                session.add_selected(Bytes::new(), 0, vec![], false),
            )
            .await
            .unwrap()
            .err()
            .expect("invalidated session must reject subsequent adds")
            .to_string()
            .contains("session stopped")
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), session.stop())
            .await
            .unwrap();
    }
}
