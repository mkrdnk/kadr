//! Opt-in real video playback through the tokenized HTTP Range server.
//! Synthesizes our own MPEG4/AAC MP4; no display or downloaded media.
#![cfg(unix)]

use std::{
    io,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use kadr_media::{
    ByteSource, LocalFile, ReadFuture, Server, ServerOptions,
    players::{PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::Command,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

struct VerifiedSource {
    file: LocalFile,
    expected: Vec<u8>,
    reads: Mutex<Vec<(u64, usize)>>,
}

impl ByteSource for VerifiedSource {
    fn len(&self) -> u64 {
        self.file.len()
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            let bytes = self.file.read_at(offset, max_len).await?;
            let start =
                usize::try_from(offset).map_err(|_| io::Error::other("fixture offset overflow"))?;
            if self.expected.get(start..start + bytes.len()) != Some(bytes.as_ref()) {
                return Err(io::Error::other("served video bytes differ from fixture"));
            }
            self.reads.lock().unwrap().push((offset, bytes.len()));
            Ok(bytes)
        })
    }
}

async fn generate_fixture() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "8",
            "-c:v",
            "mpeg4",
            "-q:v",
            "4",
            "-threads:v",
            "2",
            "-c:a",
            "aac",
            "-b:a",
            "64k",
            "-threads:a",
            "1",
            "-movflags",
            "+faststart",
        ])
        .arg(directory.path().join("own test video.mp4"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("this ignored test requires installed ffmpeg with lavfi/mpeg4/aac");
    let result = timeout(Duration::from_secs(20), child.wait()).await;
    if result.is_err() {
        child.start_kill().unwrap();
        timeout(Duration::from_secs(3), child.wait())
            .await
            .expect("ffmpeg must exit after kill")
            .unwrap();
        panic!("video fixture generation exceeded 20 seconds");
    }
    assert!(result.unwrap().unwrap().success(), "ffmpeg fixture failed");
    directory
}

async fn serve_fixture(directory: &TempDir) -> (Server, Arc<VerifiedSource>) {
    let path = directory.path().join("own test video.mp4");
    let source = Arc::new(VerifiedSource {
        file: LocalFile::open(&path).await.unwrap(),
        expected: tokio::fs::read(path).await.unwrap(),
        reads: Mutex::new(Vec::new()),
    });
    assert!(source.len() > 128 * 1024, "video must span multiple chunks");
    let server = Server::start(source.clone(), ServerOptions::default())
        .await
        .unwrap();
    (server, source)
}

async fn assert_http_seek(server: &Server, source: &VerifiedSource) {
    // Unaligned nonzero seek, independently verified over real HTTP. This is
    // not a claim that every player must issue this exact Range: players may
    // satisfy their media seek from already-prefetched bytes.
    let start = 64 * 1024 + 13;
    let length = 32771;
    let end = start + length - 1;
    let uri: hyper::Uri = server.private_url().parse().unwrap();
    let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nRange: bytes={start}-{end}\r\nConnection: close\r\n\r\n",
        uri.path(),
        server.local_addr()
    );
    let mut response = Vec::new();
    timeout(Duration::from_secs(3), async {
        socket.write_all(request.as_bytes()).await.unwrap();
        socket.read_to_end(&mut response).await.unwrap();
    })
    .await
    .unwrap();
    let boundary = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("HTTP headers");
    let headers = std::str::from_utf8(&response[..boundary]).unwrap();
    assert!(headers.starts_with("HTTP/1.1 206 "));
    assert!(headers.to_ascii_lowercase().contains(&format!(
        "content-range: bytes {start}-{end}/{}",
        source.len()
    )));
    assert_eq!(
        &response[boundary + 4..],
        &source.expected[start..=end],
        "HTTP seek must return exactly the selected MP4 bytes"
    );
    let mut reads = source.reads.lock().unwrap();
    assert!(
        reads
            .iter()
            .any(|&(offset, size)| offset == start as u64 && size > 0)
    );
    reads.clear();
}

async fn launch(kind: PlayerKind, server: &Server, resume: u64) -> PlayerSession {
    let mut options = PlayerOptions::for_player(kind);
    options.headless = true;
    PlayerSession::launch(
        kind,
        PlayRequest {
            media: server.private_url().into(),
            resume: Some(Duration::from_secs(resume)),
        },
        options,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
}

async fn assert_playing(
    kind: PlayerKind,
    session: &mut PlayerSession,
    source: &VerifiedSource,
    resume: u64,
) {
    timeout(Duration::from_secs(4), async {
        loop {
            let snapshot = session.snapshot(&CancellationToken::new()).await.unwrap();
            assert!(
                snapshot.exit.is_none(),
                "player exited before playback evidence"
            );
            match kind {
                PlayerKind::Mpv => {
                    if let (Telemetry::Seconds(position), Telemetry::Seconds(duration)) =
                        (snapshot.position, snapshot.duration)
                    {
                        assert!(
                            position >= resume as f64 - 0.15 && position < 7.9,
                            "mpv resumed position: {position}"
                        );
                        assert!((7.9..8.2).contains(&duration), "mpv duration: {duration}");
                        eprintln!("mpv MP4: position={position:.3}s duration={duration:.3}s");
                        break;
                    }
                }
                PlayerKind::Vlc => {
                    assert_eq!(snapshot.position, Telemetry::Unsupported);
                    assert_eq!(snapshot.duration, Telemetry::Unsupported);
                    // RC readiness alone does not prove HTTP media was opened.
                    // Wait for actual verified MP4 reads before stopping.
                    let bytes_read: usize =
                        source.reads.lock().unwrap().iter().map(|read| read.1).sum();
                    if bytes_read > 128 * 1024 {
                        break;
                    }
                }
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

fn assert_player_reads(kind: PlayerKind, source: &VerifiedSource) {
    let reads = source.reads.lock().unwrap();
    let bytes: usize = reads.iter().map(|read| read.1).sum();
    assert!(bytes > 128 * 1024, "player must read real MP4 data");
    assert!(reads.iter().any(|&(offset, size)| offset > 0 && size > 0));
    eprintln!(
        "{kind:?} MP4: {} verified source reads, {bytes} bytes; HTTP nonzero Range checked separately",
        reads.len()
    );
}

#[tokio::test]
#[ignore = "requires real ffmpeg, mpv and VLC; no display or network download"]
async fn real_mpv_vlc_generated_mp4_http_resume_stop_and_eof() {
    let fixture = generate_fixture().await;
    for kind in [PlayerKind::Mpv, PlayerKind::Vlc] {
        // Explicit stop after starting a resumed video.
        let (server, source) = serve_fixture(&fixture).await;
        assert_http_seek(&server, &source).await;
        let mut session = launch(kind, &server, 2).await;
        assert_playing(kind, &mut session, &source, 2).await;
        sleep(Duration::from_millis(250)).await;
        assert!(session.try_wait().unwrap().is_none());
        assert!(session.stop().await.unwrap().success());
        assert_player_reads(kind, &source);
        server.shutdown().await.unwrap();

        // Natural EOF after a late resume. Elapsed time here is only a coarse
        // resume test, never telemetry or persisted progress.
        let (server, source) = serve_fixture(&fixture).await;
        let started = Instant::now();
        let mut session = launch(kind, &server, 6).await;
        assert_playing(kind, &mut session, &source, 6).await;
        timeout(Duration::from_secs(6), async {
            loop {
                if let Some(status) = session.try_wait().unwrap() {
                    assert!(status.success(), "{kind:?} natural EOF: {status}");
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "resumed eight-second MP4 must finish earlier than unresumed video"
        );
        assert!(session.stop().await.unwrap().success());
        assert_player_reads(kind, &source);
        server.shutdown().await.unwrap();
    }
}
