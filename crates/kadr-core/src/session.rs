//! Pure session transitions. IO adapters report facts; this module starts no work.

use std::{fmt, time::Duration};

use crate::{
    source::{SourceId, SourceKind},
    torrent::{EpisodeNumber, FileIndex},
};

/// Local history does not require a catalogue ID. A selected torrent file and
/// manually assigned episode keep progress separate even within a single source.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PlaybackKey {
    pub source: SourceId,
    pub file: Option<FileIndex>,
    pub episode: Option<EpisodeNumber>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceStage {
    ReceivingMetadata,
    Connecting,
    Buffering,
    Ready,
    Failed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerKind {
    Mpv,
    Vlc,
    Browser,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Telemetry {
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivePlayer {
    pub kind: PlayerKind,
    pub telemetry: Telemetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackProgress {
    pub position: Duration,
    pub duration: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    MetadataReceived,
    PeersConnected,
    StreamReady,
    BufferNeeded,
    /// A successful adapter launch, not a request to spawn a process.
    PlayerOpened(ActivePlayer),
    PlayerExited,
    SourceFailed,
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    InvalidTransition,
    TelemetryUnavailable,
    InvalidPosition,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidTransition => "This action is not valid in the current session state.",
            Self::TelemetryUnavailable => "The current player does not provide playback progress.",
            Self::InvalidPosition => "Playback position cannot exceed the reported duration.",
        })
    }
}
impl std::error::Error for SessionError {}

/// Source readiness and external player lifetime are separate. Switching/exiting
/// a player does not restart source acquisition. Failure/stop are terminal.
#[derive(Clone, Debug)]
pub struct PlaybackSession {
    key: PlaybackKey,
    stage: SourceStage,
    player: Option<ActivePlayer>,
    last_progress: Option<PlaybackProgress>,
}

impl PlaybackSession {
    pub fn new(key: PlaybackKey, kind: SourceKind) -> Self {
        Self {
            key,
            stage: match kind {
                SourceKind::Http => SourceStage::Buffering,
                SourceKind::Magnet | SourceKind::TorrentFile => SourceStage::ReceivingMetadata,
            },
            player: None,
            last_progress: None,
        }
    }

    pub fn key(&self) -> &PlaybackKey {
        &self.key
    }
    pub fn stage(&self) -> SourceStage {
        self.stage
    }
    pub fn player(&self) -> Option<ActivePlayer> {
        self.player
    }

    /// Last observed position, not an assertion of live telemetry. Adapters/storage
    /// attach observation timestamps; this pure model does not read a clock.
    pub fn last_progress(&self) -> Option<PlaybackProgress> {
        self.last_progress
    }

    /// On error, state is unchanged. Stop is idempotent, including after failure.
    pub fn apply(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        use SessionEvent as E;
        use SourceStage as S;
        match (self.stage, event) {
            (_, E::Stop) => {
                self.stage = S::Stopped;
                self.player = None;
            }
            (S::Failed | S::Stopped, _) => return Err(SessionError::InvalidTransition),
            (S::ReceivingMetadata, E::MetadataReceived) => self.stage = S::Connecting,
            (S::Connecting, E::PeersConnected) => self.stage = S::Buffering,
            (S::Buffering, E::StreamReady) => self.stage = S::Ready,
            (S::Ready, E::BufferNeeded) => self.stage = S::Buffering,
            (S::Ready, E::PlayerOpened(player)) => {
                if player.kind == PlayerKind::Browser && player.telemetry == Telemetry::Available {
                    return Err(SessionError::TelemetryUnavailable);
                }
                self.player = Some(player);
            }
            (_, E::PlayerExited) if self.player.is_some() => self.player = None,
            (_, E::SourceFailed) => {
                self.stage = S::Failed;
                self.player = None;
            }
            _ => return Err(SessionError::InvalidTransition),
        }
        Ok(())
    }

    pub fn record_progress(&mut self, progress: PlaybackProgress) -> Result<(), SessionError> {
        if !self
            .player
            .is_some_and(|p| p.telemetry == Telemetry::Available)
        {
            return Err(SessionError::TelemetryUnavailable);
        }
        if progress
            .duration
            .is_some_and(|duration| progress.position > duration)
        {
            return Err(SessionError::InvalidPosition);
        }
        self.last_progress = Some(progress);
        Ok(())
    }
}
