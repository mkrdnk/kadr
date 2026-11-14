//! Opt-in real binary tests. No display, media download, or external network.
#![cfg(unix)]

use std::{io, sync::Arc, time::Duration};

use kadr_media::{
    LocalFile, Server, ServerOptions,
    players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry},
};
use tempfile::TempDir;
use tokio::time::{Instant, sleep, timeout};
use tokio_util::sync::CancellationToken;

fn wav_fixture() -> TempDir {
    // Eight seconds of our own PCM silence, no third-party fixture/license.
    let directory = tempfile::tempdir().unwrap();
    let data_len = 8 * 8000 * 2_u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8000_u32.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(bytes.len() + data_len as usize, 0);
    std::fs::write(directory.path().join("own fixture.wav"), bytes).unwrap();
    directory
}

async fn serve_fixture(directory: &TempDir) -> Server {
    let file = LocalFile::open(directory.path().join("own fixture.wav"))
        .await
        .unwrap();
    Server::start(Arc::new(file), ServerOptions::default())
        .await
        .unwrap()
}

async fn launch(kind: PlayerKind, server: &Server, resume: f64) -> PlayerSession {
    eprintln!("Launching {kind:?} headless at {resume} seconds");
    let mut options = PlayerOptions::for_player(kind);
    options.headless = true;
    PlayerSession::launch(
        kind,
        PlayRequest {
            media: server.private_url().into(),
            resume: Some(Duration::from_secs_f64(resume)),
        },
        options,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
}

async fn wait_for_exit(session: &mut PlayerSession) {
    timeout(Duration::from_secs(10), async {
        loop {
            if let Some(status) = session.try_wait().unwrap() {
                assert!(status.success(), "player exit: {status}");
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires real mpv; run explicitly on Linux/macOS"]
async fn real_mpv_http_resume_telemetry_stop_and_eof() {
    let fixture = wav_fixture();
    let server = serve_fixture(&fixture).await;
    let mut session = launch(PlayerKind::Mpv, &server, 2.0).await;
    timeout(Duration::from_secs(4), async {
        loop {
            match session.snapshot(&CancellationToken::new()).await {
                Ok(snapshot) => {
                    if let (Telemetry::Seconds(position), Telemetry::Seconds(duration)) =
                        (snapshot.position, snapshot.duration)
                    {
                        assert!((1.9..5.0).contains(&position), "position {position}");
                        assert!((7.9..8.1).contains(&duration), "duration {duration}");
                        break;
                    }
                    assert!(snapshot.exit.is_none(), "player exited before telemetry");
                }
                Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                    panic!("IPC unexpectedly invalidated")
                }
                Err(error) => panic!("mpv telemetry: {error}"),
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(session.stop().await.unwrap().success());
    assert!(
        !session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .completed,
        "explicit quit is not natural completion"
    );
    let mut session = launch(PlayerKind::Mpv, &server, 7.0).await;
    wait_for_exit(&mut session).await;
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert!(snapshot.exit.unwrap().success());
    assert!(
        snapshot.completed,
        "queued mpv end-file/eof must be observed"
    );
    assert_eq!(snapshot.position, Telemetry::Unavailable);
    server.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires real VLC 3 oldrc; run explicitly on Linux/macOS"]
async fn real_vlc_http_resume_unsupported_telemetry_stop_and_eof() {
    let fixture = wav_fixture();
    let server = serve_fixture(&fixture).await;
    let mut session = launch(PlayerKind::Vlc, &server, 2.0).await;
    let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
    assert_eq!(snapshot.position, Telemetry::Unsupported);
    assert_eq!(snapshot.duration, Telemetry::Unsupported);
    assert!(!snapshot.completed);
    assert!(session.stop().await.unwrap().success());
    // Coarse end-to-end resume evidence, NOT a source of playback telemetry:
    // seven-second start in an eight-second file should finish well before
    // an unresumed playback could. Nothing here is persisted as progress.
    let started = Instant::now();
    let mut session = launch(PlayerKind::Vlc, &server, 7.0).await;
    wait_for_exit(&mut session).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        !session
            .snapshot(&CancellationToken::new())
            .await
            .unwrap()
            .completed,
        "VLC completion events are unsupported"
    );
    server.shutdown().await.unwrap();
}
