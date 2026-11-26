use kadr_core::{
    session::*,
    source::{ManualSource, SourceKind},
    torrent::{EpisodeNumber, FileIndex},
};
use std::time::Duration;

fn key(file: Option<FileIndex>, episode: Option<EpisodeNumber>) -> PlaybackKey {
    PlaybackKey {
        source: ManualSource::parse_link("https://example.test/private?token=secret")
            .unwrap()
            .id(),
        file,
        episode,
    }
}

fn progress(position: u64, duration: Option<u64>) -> PlaybackProgress {
    PlaybackProgress {
        position: Duration::from_secs(position),
        duration: duration.map(Duration::from_secs),
    }
}

#[test]
fn torrent_lifecycle_player_switch_and_rebuffer_do_not_restart_source() {
    let key = key(Some(FileIndex(7)), EpisodeNumber::new(1, 2));
    let mut session = PlaybackSession::new(key.clone(), SourceKind::Magnet);
    assert_eq!(session.stage(), SourceStage::ReceivingMetadata);
    assert_eq!(
        session.apply(SessionEvent::StreamReady),
        Err(SessionError::InvalidTransition)
    );
    assert_eq!(session.stage(), SourceStage::ReceivingMetadata);
    session.apply(SessionEvent::MetadataReceived).unwrap();
    session.apply(SessionEvent::PeersConnected).unwrap();
    session.apply(SessionEvent::StreamReady).unwrap();
    session
        .apply(SessionEvent::PlayerOpened(ActivePlayer {
            kind: PlayerKind::Mpv,
            telemetry: Telemetry::Available,
        }))
        .unwrap();
    session.record_progress(progress(42, Some(120))).unwrap();
    session.apply(SessionEvent::BufferNeeded).unwrap();
    session.apply(SessionEvent::PlayerExited).unwrap();
    assert_eq!(session.stage(), SourceStage::Buffering);
    session.apply(SessionEvent::StreamReady).unwrap();
    session
        .apply(SessionEvent::PlayerOpened(ActivePlayer {
            kind: PlayerKind::Vlc,
            telemetry: Telemetry::Unavailable,
        }))
        .unwrap();
    assert_eq!(
        session.record_progress(progress(0, None)),
        Err(SessionError::TelemetryUnavailable)
    );
    assert_eq!(session.last_progress(), Some(progress(42, Some(120))));
    assert_eq!(session.key(), &key);
}

#[test]
fn browser_never_claims_telemetry_and_invalid_events_are_atomic() {
    let mut session = PlaybackSession::new(key(None, None), SourceKind::Http);
    assert_eq!(session.stage(), SourceStage::Buffering);
    assert_eq!(
        session.apply(SessionEvent::MetadataReceived),
        Err(SessionError::InvalidTransition)
    );
    session.apply(SessionEvent::StreamReady).unwrap();
    assert_eq!(
        session.apply(SessionEvent::PlayerOpened(ActivePlayer {
            kind: PlayerKind::Browser,
            telemetry: Telemetry::Available
        })),
        Err(SessionError::TelemetryUnavailable)
    );
    assert_eq!(session.player(), None);
    session
        .apply(SessionEvent::PlayerOpened(ActivePlayer {
            kind: PlayerKind::Browser,
            telemetry: Telemetry::Unavailable,
        }))
        .unwrap();
    assert_eq!(
        session.record_progress(progress(1, Some(60))),
        Err(SessionError::TelemetryUnavailable)
    );
    assert_eq!(session.last_progress(), None);
}

#[test]
fn progress_validation_and_terminal_states() {
    let mut session = PlaybackSession::new(key(None, None), SourceKind::Http);
    session.apply(SessionEvent::StreamReady).unwrap();
    session
        .apply(SessionEvent::PlayerOpened(ActivePlayer {
            kind: PlayerKind::Mpv,
            telemetry: Telemetry::Available,
        }))
        .unwrap();
    session.record_progress(progress(50, None)).unwrap();
    assert_eq!(
        session.record_progress(progress(51, Some(50))),
        Err(SessionError::InvalidPosition)
    );
    assert_eq!(session.last_progress(), Some(progress(50, None)));
    session.record_progress(progress(10, Some(50))).unwrap(); // seek backwards is valid
    session.apply(SessionEvent::SourceFailed).unwrap();
    assert_eq!(session.player(), None);
    assert_eq!(
        session.apply(SessionEvent::StreamReady),
        Err(SessionError::InvalidTransition)
    );
    assert_eq!(
        session.record_progress(progress(12, Some(50))),
        Err(SessionError::TelemetryUnavailable)
    );
    session.apply(SessionEvent::Stop).unwrap();
    session.apply(SessionEvent::Stop).unwrap();
    assert_eq!(session.stage(), SourceStage::Stopped);
    assert_eq!(
        session.apply(SessionEvent::SourceFailed),
        Err(SessionError::InvalidTransition)
    );
}

#[test]
fn history_distinguishes_files_and_explicit_episode_assignments() {
    assert_ne!(key(Some(FileIndex(1)), None), key(Some(FileIndex(2)), None));
    assert_ne!(
        key(Some(FileIndex(1)), EpisodeNumber::new(1, 2)),
        key(Some(FileIndex(1)), EpisodeNumber::new(1, 3))
    );
}
