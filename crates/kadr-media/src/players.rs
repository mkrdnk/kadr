//! External-player spike. No UI or persistent history; the caller owns snapshots.
//!
//! All commands use argv, never a shell. Source URLs are private capabilities:
//! do not log requests. Child stdout/stderr are discarded for the same reason.
//! Use `stop().await` before shutting down the Tokio runtime. Drop kills the
//! direct child (Tokio reaps it while the runtime lives), but cannot join it.
//! In Flatpak the direct child is flatpak-spawn: --watch-bus requests host
//! SIGINT on bridge exit, not guaranteed SIGKILL of a hostile host executable.

use std::{
    ffi::OsString,
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

const MAX_REPLY: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerKind {
    Mpv,
    Vlc,
}

/// A local path or a playable HTTP(S) URL, passed as exactly one argv element.
/// This type deliberately has no Debug implementation to avoid leaking URLs.
pub struct PlayRequest {
    pub media: OsString,
    pub resume: Option<Duration>,
}

pub struct PlayerOptions {
    /// Explicit binary path, or a name resolved by the OS using PATH.
    pub executable: PathBuf,
    /// Null audio/video output for automated smoke tests, not ordinary viewing.
    pub headless: bool,
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
    /// Grace period before kill, and maximum wait for reaping after kill.
    pub stop_timeout: Duration,
}

impl PlayerOptions {
    pub fn for_player(kind: PlayerKind) -> Self {
        Self {
            executable: match kind {
                PlayerKind::Mpv => "mpv",
                PlayerKind::Vlc => "vlc",
            }
            .into(),
            headless: false,
            connect_timeout: Duration::from_secs(5),
            command_timeout: Duration::from_secs(1),
            stop_timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Telemetry {
    /// A finite, nonnegative value reported by the player, not a local clock.
    Seconds(f64),
    /// Supported property currently unavailable (e.g. loading or live stream).
    Unavailable,
    /// VLC RC telemetry is not trusted by this prototype. No fake progress.
    Unsupported,
}

#[derive(Debug)]
pub struct PlayerSnapshot {
    pub position: Telemetry,
    pub duration: Telemetry,
    /// Process exit, not proof of media completion (the user may close a window).
    pub exit: Option<ExitStatus>,
}

/// A single managed direct child and its private IPC endpoint.
///
/// No background polling or detached per-session tasks. A timed-out, cancelled,
/// malformed, or disconnected command invalidates IPC to prevent reply mixing.
/// `stop` remains usable after any IPC failure. Dropping launch/snapshot/stop
/// futures is safe; dropping the session requests immediate child termination.
pub struct PlayerSession {
    kind: PlayerKind,
    child: Child,
    socket: Option<UnixStream>,
    _directory: TempDir,
    command_timeout: Duration,
    stop_timeout: Duration,
    request_id: u64,
}

impl PlayerSession {
    pub async fn launch(
        kind: PlayerKind,
        request: PlayRequest,
        options: PlayerOptions,
        cancel: &CancellationToken,
    ) -> io::Result<Self> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        if options.connect_timeout.is_zero()
            || options.command_timeout.is_zero()
            || options.stop_timeout.is_zero()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "player timeouts must be positive",
            ));
        }
        // Flatpak's /tmp is private; the host player must see the same endpoint.
        // Native builds retain the short base needed by macOS sockaddr_un.
        let flatpak = cfg!(target_os = "linux") && std::env::var_os("FLATPAK_ID").is_some();
        let base = if flatpak {
            shared_player_directory()?
        } else {
            PathBuf::from("/tmp")
        };
        let directory = tempfile::Builder::new()
            .prefix("kadr-player-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(base)
            .map_err(|e| redacted(e, "cannot create private player directory"))?;
        let socket_path = directory.path().join("ipc");
        // Do not silently truncate a Unix socket path or fall back to /tmp.
        if socket_path.as_os_str().len() >= 104 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "player IPC path is too long",
            ));
        }
        let mut command = if flatpak {
            let mut host = Command::new("flatpak-spawn");
            host.args(["--host", "--watch-bus"])
                .arg(&options.executable);
            host
        } else {
            Command::new(&options.executable)
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let start = request
            .resume
            .map(|time| format!("{:.6}", time.as_secs_f64()));
        match kind {
            PlayerKind::Mpv => {
                command.args([
                    "--no-config",
                    "--load-scripts=no",
                    "--ytdl=no",
                    "--input-terminal=no",
                    "--idle=no",
                    "--keep-open=no",
                    "--really-quiet",
                ]);
                command.arg(format!("--input-ipc-server={}", socket_path.display()));
                if let Some(start) = start {
                    command.arg(format!("--start={start}"));
                }
                if options.headless {
                    command.args(["--vo=null", "--ao=null"]);
                }
            }
            PlayerKind::Vlc => {
                command.args([
                    "--ignore-config",
                    "--no-one-instance",
                    "--no-playlist-enqueue",
                    "--no-media-library",
                    "--no-metadata-network-access",
                    "--extraintf=oldrc",
                    "--rc-fake-tty",
                    "--quiet",
                    "--play-and-exit",
                ]);
                command.arg(format!("--rc-unix={}", socket_path.display()));
                if let Some(start) = start {
                    command.arg(format!("--start-time={start}"));
                }
                if options.headless {
                    command.args(["--intf=dummy", "--vout=dummy", "--aout=dummy"]);
                }
            }
        }
        command.arg("--").arg(request.media);
        let child = command.spawn().map_err(|e| {
            redacted(
                e,
                "cannot launch player executable (check installation and permissions)",
            )
        })?;
        // Session is the RAII guard even while launch is pending/cancelled.
        let mut session = Self {
            kind,
            child,
            socket: None,
            _directory: directory,
            command_timeout: options.command_timeout,
            stop_timeout: options.stop_timeout,
            request_id: 0,
        };
        let connect = async {
            loop {
                if session.child.try_wait()?.is_some() {
                    return Err(io::Error::other("player exited before IPC became ready"));
                }
                match UnixStream::connect(&socket_path).await {
                    Ok(socket) => return Ok(socket),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                        ) => {}
                    Err(e) => return Err(redacted(e, "cannot connect to player IPC")),
                }
                sleep(Duration::from_millis(20)).await;
            }
        };
        let connected = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(cancelled()),
            result = timeout(options.connect_timeout, connect) => result
                .unwrap_or_else(|_| Err(timed_out("player IPC startup timed out"))),
        };
        match connected {
            Ok(socket) => {
                session.socket = Some(socket);
                Ok(session)
            }
            Err(error) => {
                // Explicit failure joins the child; dropping this future still
                // leaves kill_on_drop enabled.
                let _ = session.kill_and_wait().await;
                Err(error)
            }
        }
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .map_err(|e| redacted(e, "cannot inspect player exit"))
    }

    pub async fn snapshot(&mut self, cancel: &CancellationToken) -> io::Result<PlayerSnapshot> {
        let exit = self.try_wait()?;
        if self.kind == PlayerKind::Vlc {
            return Ok(PlayerSnapshot {
                position: Telemetry::Unsupported,
                duration: Telemetry::Unsupported,
                exit,
            });
        }
        if exit.is_some() {
            return Ok(PlayerSnapshot {
                position: Telemetry::Unavailable,
                duration: Telemetry::Unavailable,
                exit,
            });
        }
        let properties = async {
            let position = self.mpv_property("time-pos", cancel).await?;
            let duration = self.mpv_property("duration", cancel).await?;
            Ok::<_, io::Error>((position, duration))
        }
        .await;
        let (position, duration) = match properties {
            Ok(values) => values,
            Err(error) => {
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionReset
                ) {
                    // IPC can close just before the process exits at EOF.
                    // Resolve that race without assuming that IPC EOF means
                    // process exit, or waiting indefinitely on a live child.
                    let exited = tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(cancelled()),
                        result = timeout(self.command_timeout, self.child.wait()) => result,
                    };
                    if let Ok(Ok(status)) = exited {
                        return Ok(PlayerSnapshot {
                            position: Telemetry::Unavailable,
                            duration: Telemetry::Unavailable,
                            exit: Some(status),
                        });
                    }
                }
                return Err(error);
            }
        };
        Ok(PlayerSnapshot {
            position,
            duration,
            exit: self.try_wait()?,
        })
    }

    async fn mpv_property(
        &mut self,
        property: &str,
        cancel: &CancellationToken,
    ) -> io::Result<Telemetry> {
        // Take before any await. Cancellation by dropping the future closes
        // the stream rather than leaving a partial reply for the next caller.
        let mut socket = self.socket.take().ok_or_else(ipc_unavailable)?;
        self.request_id = self.request_id.wrapping_add(1);
        let id = self.request_id;
        let request = json!({"command": ["get_property", property], "request_id": id});
        let exchange = async {
            socket.write_all(request.to_string().as_bytes()).await?;
            socket.write_all(b"\n").await?;
            loop {
                let reply = read_json_line(&mut socket).await?;
                // mpv events do not carry request IDs. Delayed replies may not
                // masquerade as the requested property.
                if reply.get("request_id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                match reply.get("error").and_then(Value::as_str) {
                    Some("success") => {
                        return match reply.get("data") {
                            Some(Value::Null) => Ok(Telemetry::Unavailable),
                            Some(data) => data
                                .as_f64()
                                .filter(|value| value.is_finite() && *value >= 0.0)
                                .map(Telemetry::Seconds)
                                .ok_or_else(|| invalid_reply("invalid mpv numeric telemetry")),
                            None => Err(invalid_reply("mpv reply has no data")),
                        };
                    }
                    Some("property unavailable") => return Ok(Telemetry::Unavailable),
                    _ => return Err(invalid_reply("mpv rejected telemetry request")),
                }
            }
        };
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(cancelled()),
            result = timeout(self.command_timeout, exchange) => result
                .unwrap_or_else(|_| Err(timed_out("player IPC command timed out"))),
        };
        match result {
            Ok(value) => {
                self.socket = Some(socket);
                Ok(value)
            }
            Err(error) => Err(redacted(error, "player IPC telemetry failed")),
        }
    }

    /// Send graceful quit, then force-kill after the grace period. Always use
    /// process exit, never IPC EOF, as evidence that the player has terminated.
    /// Idempotent. If cancelled by dropping the future, it can be called again.
    pub async fn stop(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            self.socket = None;
            return Ok(status);
        }
        if let Some(mut socket) = self.socket.take() {
            let quit: &[u8] = match self.kind {
                PlayerKind::Mpv => b"{\"command\":[\"quit\"]}\n",
                PlayerKind::Vlc => b"quit\n",
            };
            let _ = timeout(self.command_timeout, socket.write_all(quit)).await;
        }
        match timeout(self.stop_timeout, self.child.wait()).await {
            Ok(result) => result.map_err(|e| redacted(e, "cannot wait for player exit")),
            Err(_) => self.kill_and_wait().await,
        }
    }

    async fn kill_and_wait(&mut self) -> io::Result<ExitStatus> {
        self.child
            .start_kill()
            .map_err(|e| redacted(e, "cannot terminate player"))?;
        timeout(self.stop_timeout, self.child.wait())
            .await
            .map_err(|_| timed_out("player did not exit after kill"))?
            .map_err(|e| redacted(e, "cannot reap player"))
    }
}

/// The manifest exposes only this runtime subdirectory, not all host /tmp.
/// Never accept a symlink, another user's directory, or a writable shared parent.
fn shared_player_directory() -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("Flatpak requires absolute XDG_RUNTIME_DIR"))?;
    let path = runtime.join("kadr");
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(redacted(error, "cannot create shared player directory")),
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(&path)
        .map_err(|error| redacted(error, "unsafe shared player directory"))?;
    let metadata = directory.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "shared player directory must be owned by this user and not group/world writable",
        ));
    }
    // Flatpak's :create permission pre-creates this directory as 0755. Tighten
    // through the no-follow descriptor before placing any private IPC in it.
    directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

// Deliberately do not buffer beyond a reply's newline. This keeps future-drop
// semantics simple, and caps a single event/reply independently of peer input.
async fn read_json_line(socket: &mut UnixStream) -> io::Result<Value> {
    let mut bytes = Vec::new();
    loop {
        let byte = socket.read_u8().await?;
        if byte == b'\n' {
            return serde_json::from_slice(&bytes).map_err(|_| invalid_reply("malformed mpv JSON"));
        }
        if bytes.len() == MAX_REPLY {
            return Err(invalid_reply("mpv reply exceeds size limit"));
        }
        bytes.push(byte);
    }
}

fn redacted(error: io::Error, message: &'static str) -> io::Error {
    io::Error::new(error.kind(), message)
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "player operation cancelled")
}

fn timed_out(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

fn invalid_reply(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn ipc_unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        "player IPC is no longer usable",
    )
}
