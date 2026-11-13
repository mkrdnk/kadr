mod support;

use std::{
    future::Future,
    io,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use kadr_media::{ByteSource, ReadFuture, Server, ServerOptions};
use kadr_torrent::{LocalSession, TorrentByteSource, TorrentFile};
use support::{Fixture, MISSING, SELECTED};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(15);

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    timeout(DEADLINE, future).await.expect("operation deadline")
}

/// Observe the lifetime of the real adapter read, not a mock availability wait.
struct Observed {
    source: Arc<TorrentByteSource>,
    active: watch::Sender<usize>,
}

struct ReadGuard<'a>(&'a watch::Sender<usize>);

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

impl Observed {
    async fn active(&self, expected: usize) {
        bounded(self.active.subscribe().wait_for(|n| *n == expected))
            .await
            .unwrap();
    }
}

impl ByteSource for Observed {
    fn len(&self) -> u64 {
        self.source.len()
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            self.active.send_modify(|n| *n += 1);
            let _guard = ReadGuard(&self.active);
            self.source.read_at(offset, max_len).await
        })
    }
}

async fn connect(server: &Server, range: &str) -> TcpStream {
    bounded(async {
        let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
        let url = server.private_url();
        let path = url
            .strip_prefix(&format!("http://{}", server.local_addr()))
            .unwrap();
        socket
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost\r\n\
                     Range: {range}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket
    })
    .await
}

async fn range(server: &Server, start: usize, end: usize) -> Vec<u8> {
    let mut socket = connect(server, &format!("bytes={start}-{end}")).await;
    bounded(async {
        let mut wire = Vec::new();
        socket.read_to_end(&mut wire).await.unwrap();
        let split = wire.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
        let head = std::str::from_utf8(&wire[..split]).unwrap();
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        let head = head.to_ascii_lowercase();
        assert!(head.contains(&format!("content-length: {}\r\n", end - start + 1)));
        assert!(head.contains(&format!(
            "content-range: bytes {start}-{end}/{SELECTED}\r\n"
        )));
        assert!(head.contains("accept-ranges: bytes\r\n"));
        wire[split..].to_vec()
    })
    .await
}

async fn released(address: SocketAddr) {
    bounded(async {
        // Upstream stop is best effort. Check actual socket release, rather
        // than assuming its one-second sleep proves all tasks have joined.
        loop {
            if TcpStream::connect(address).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let listener = TcpListener::bind(address).await.unwrap();
        drop(listener);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_partial_peer_ranges_seek_concurrency_and_cancellation() {
    let fixture = Fixture::new(true);
    let seed_dir = tempfile::tempdir().unwrap();
    let download_dir = tempfile::tempdir().unwrap();
    fixture.write_partial_seed(seed_dir.path());
    let seed = bounded(LocalSession::new(seed_dir.path().into()))
        .await
        .unwrap();
    let seed_init_started = Instant::now();
    let seed_torrent = bounded(seed.add_selected(fixture.torrent.clone(), 1, vec![], true))
        .await
        .unwrap();
    let seed_init = seed_init_started.elapsed();
    let downloader = bounded(LocalSession::new(download_dir.path().into()))
        .await
        .unwrap();
    for address in [seed.listen_addr(), downloader.listen_addr()] {
        assert_eq!(address.ip().to_string(), "127.0.0.1");
        assert_ne!(address.port(), 0);
    }
    let download_init_started = Instant::now();
    let selected = bounded(downloader.add_selected(
        fixture.torrent.clone(),
        1,
        vec![seed.listen_addr()],
        false,
    ))
    .await
    .unwrap();
    let download_init = download_init_started.elapsed();
    assert_eq!(selected.files.len(), 3);
    for (id, (name, len)) in [
        ("prefix.bin", support::PREFIX),
        ("selected.bin", SELECTED),
        ("tail.bin", 3 * support::PIECE),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(selected.files[id].id, id);
        assert_eq!(selected.files[id].relative_filename.to_str().unwrap(), name);
        assert_eq!(selected.files[id].len, len as u64);
    }
    assert_eq!(selected.source.file_id(), 1);
    assert_eq!(selected.source.len(), SELECTED as u64);
    let first_read_started = Instant::now();
    let first = bounded(selected.source.read_at(0, 1024)).await.unwrap();
    let first_read = first_read_started.elapsed();
    assert_eq!(first, fixture.selected[..first.len()]);
    assert!(!first.is_empty());
    println!(
        "Timing sample: selected={} bytes, total={} bytes, piece={} bytes; \
         seed metadata+rehash={seed_init:?}; downloader metadata+init={download_init:?}; \
         first verified source prefix ({} bytes, post-init)={first_read:?}",
        SELECTED,
        support::PREFIX + SELECTED + 3 * support::PIECE,
        support::PIECE,
        first.len(),
    );
    // Upstream AlreadyManaged ignores new peers/only_files. The wrapper must
    // reject it, not silently return a source with an unapplied selection.
    assert!(
        bounded(downloader.add_selected(fixture.torrent.clone(), 2, vec![], false))
            .await
            .err()
            .expect("duplicate add must fail")
            .to_string()
            .contains("already managed")
    );
    let observed = Arc::new(Observed {
        source: selected.source.clone(),
        active: watch::channel(0).0,
    });
    let server = bounded(Server::start(
        observed.clone(),
        ServerOptions {
            read_timeout: DEADLINE,
        },
    ))
    .await
    .unwrap();
    // Seek to the end, back to start, then the middle. Missing seed pieces
    // guarantee the selected file cannot have completed, even on fast machines.
    for (start, end) in [
        (SELECTED - 1000, SELECTED - 1),
        (0, 40_000), // crosses pieces and the unaligned file boundary
        (SELECTED / 2, SELECTED / 2 + 30_000),
    ] {
        let started = Instant::now();
        assert_eq!(
            range(&server, start, end).await,
            fixture.selected[start..=end]
        );
        println!(
            "HTTP seek/range {start}-{end} ({} bytes, full response)={:?}",
            end - start + 1,
            started.elapsed(),
        );
        assert!(!selected.source.stats().finished);
    }
    let (first, second, third) = tokio::join!(
        range(&server, 12, 33_333),
        range(&server, SELECTED - 40_000, SELECTED - 1),
        range(&server, SELECTED / 2, SELECTED / 2 + 4444),
    );
    assert_eq!(first, fixture.selected[12..=33_333]);
    assert_eq!(second, fixture.selected[SELECTED - 40_000..]);
    assert_eq!(third, fixture.selected[SELECTED / 2..=SELECTED / 2 + 4444]);

    // Corrupted on disk in the seed is not "available": no unverified zeroes.
    assert!(
        timeout(
            Duration::from_millis(150),
            observed.read_at(MISSING as u64, 100)
        )
        .await
        .is_err()
    );
    observed.active(0).await;

    let socket = connect(&server, &format!("bytes={MISSING}-{}", MISSING + 99)).await;
    observed.active(1).await;
    drop(socket);
    observed.active(0).await; // HTTP disconnect drops the real FileStream wait

    // Server read timeout must fail/truncate HTTP, not return a successful body.
    let short = bounded(Server::start(
        observed.clone(),
        ServerOptions {
            read_timeout: Duration::from_millis(150),
        },
    ))
    .await
    .unwrap();
    let mut socket = connect(&short, &format!("bytes={MISSING}-{}", MISSING + 99)).await;
    let mut wire = Vec::new();
    bounded(socket.read_to_end(&mut wire)).await.unwrap();
    observed.active(0).await;
    let body = wire
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .map(|split| &wire[split + 4..])
        .unwrap_or(&[]);
    assert!(
        body.is_empty(),
        "unavailable piece must never produce bytes"
    );
    bounded(short.shutdown()).await.unwrap();

    let socket = connect(&server, &format!("bytes={MISSING}-{}", MISSING + 99)).await;
    observed.active(1).await;
    let http_addr = server.local_addr();
    let http_stop_started = Instant::now();
    bounded(server.shutdown()).await.unwrap();
    let http_stop = http_stop_started.elapsed();
    observed.active(0).await;
    drop(socket);
    released(http_addr).await;
    assert!(
        bounded(selected.source.read_at(SELECTED as u64, 100))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        bounded(selected.source.read_at(0, 0))
            .await
            .unwrap()
            .is_empty()
    );

    // A pending direct read is also interrupted by torrent-session stop.
    let wait = observed.read_at(MISSING as u64, 100);
    let download_addr = downloader.listen_addr();
    let seed_addr = seed.listen_addr();
    let download_stop_started = Instant::now();
    let (read, ()) = tokio::join!(bounded(wait), bounded(downloader.stop()));
    let download_stop = download_stop_started.elapsed();
    assert_eq!(read.unwrap_err().kind(), io::ErrorKind::Interrupted);
    observed.active(0).await;
    let seed_stop_started = Instant::now();
    bounded(seed.stop()).await;
    let seed_stop = seed_stop_started.elapsed();
    // Keep handles alive to verify cancellation, not just last-Arc destruction.
    assert!(!seed_torrent.source.stats().finished);
    released(download_addr).await;
    released(seed_addr).await;
    println!(
        "Stop timing sample: HTTP={http_stop:?}; downloader={download_stop:?}; \
         partial seed={seed_stop:?}; listeners independently released/rebound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reject_nonprivate_metadata_external_peers_and_invalid_file_ids() {
    let dir = tempfile::tempdir().unwrap();
    let session = bounded(LocalSession::new(dir.path().into())).await.unwrap();
    let fixture = Fixture::new(true);
    for peer in ["192.0.2.1:42", "[::1]:42", "127.0.0.1:0"] {
        assert!(
            bounded(session.add_selected(
                fixture.torrent.clone(),
                1,
                vec![peer.parse().unwrap()],
                false
            ))
            .await
            .is_err()
        );
    }
    assert!(
        bounded(session.add_selected(Fixture::new(false).torrent, 1, vec![], false))
            .await
            .err()
            .expect("must reject public torrent")
            .to_string()
            .contains("private=1")
    );
    assert!(
        bounded(session.add_selected(fixture.torrent, 42, vec![], false))
            .await
            .is_err()
    );
    let address = session.listen_addr();
    bounded(session.stop()).await;
    released(address).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_started_add_invalidates_session_instead_of_orphaning_download() {
    let fixture = Fixture::new(true);
    let dir = tempfile::tempdir().unwrap();
    fixture.write_partial_seed(dir.path());
    let session = bounded(LocalSession::new(dir.path().into())).await.unwrap();
    // Existing files without overwrite cause a real initialization/storage
    // failure, not a metainfo rejection before the add starts.
    assert!(
        bounded(session.add_selected(fixture.torrent.clone(), 1, vec![], false))
            .await
            .is_err()
    );
    assert!(
        bounded(session.add_selected(fixture.torrent, 1, vec![], true))
            .await
            .err()
            .expect("failed started add invalidates session")
            .to_string()
            .contains("session stopped")
    );
    let address = session.listen_addr();
    bounded(session.stop()).await;
    released(address).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_redact_metadata_names_and_upstream_filesystem_paths() {
    let fixture = Fixture::new(true);
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("SENSITIVE-output");
    // Reproduce an upstream filesystem error whose original chain contains
    // both the absolute output directory and torrent-provided filename.
    std::fs::create_dir_all(output.join("kadr-fixture/selected.bin")).unwrap();
    let session = bounded(LocalSession::new(output.clone())).await.unwrap();
    let invalid = bounded(session.add_selected(
        b"malformed-SENSITIVE-metainfo".as_slice().into(),
        0,
        vec![],
        false,
    ))
    .await
    .err()
    .expect("malformed metadata must fail");
    assert!(!format!("{invalid:?}").contains("SENSITIVE"));
    let error = bounded(session.add_selected(fixture.torrent, 1, vec![], true))
        .await
        .err()
        .expect("directory collision must fail");
    for diagnostic in [
        error.to_string(),
        format!("{error:?}"),
        format!("{error:#}"),
    ] {
        assert!(!diagnostic.contains("SENSITIVE"));
        assert!(!diagnostic.contains("selected.bin"));
        assert!(!diagnostic.contains(&output.to_string_lossy().to_string()));
    }
    let file = TorrentFile {
        id: 1,
        relative_filename: "SENSITIVE-video.mp4".into(),
        len: 100,
    };
    assert!(!format!("{file:?}").contains("SENSITIVE"));
    bounded(session.stop()).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reject_existing_output_directory_and_metadata_file_symlinks() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new(true);
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let linked_root = dir.path().join("linked-output");
    symlink(outside.path(), &linked_root).unwrap();
    assert!(bounded(LocalSession::new(linked_root)).await.is_err());
    let output = dir.path().join("output");
    let subfolder = output.join("kadr-fixture");
    std::fs::create_dir_all(&subfolder).unwrap();
    let sentinel = outside.path().join("sentinel");
    std::fs::write(&sentinel, b"must not be overwritten").unwrap();
    symlink(&sentinel, subfolder.join("selected.bin")).unwrap();
    let session = bounded(LocalSession::new(output)).await.unwrap();
    assert!(
        bounded(session.add_selected(fixture.torrent, 1, vec![], true))
            .await
            .err()
            .expect("metadata leaf symlink must be rejected before overwrite")
            .to_string()
            .contains("symlink")
    );
    assert_eq!(std::fs::read(sentinel).unwrap(), b"must not be overwritten");
    let address = session.listen_addr();
    bounded(session.stop()).await;
    released(address).await;
}
