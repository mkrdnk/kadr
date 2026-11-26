use std::{
    future::Future,
    io::Cursor,
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use kadr_cache::{Error, SaveProgress, save_reader};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio_util::sync::CancellationToken;

fn assert_no_temps(path: &Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kadr-save-")
        );
    }
}

struct Checked<R> {
    inner: R,
    calls: Arc<AtomicUsize>,
}

impl<R: AsyncRead + Unpin> AsyncRead for Checked<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        assert!(buffer.remaining() <= 64 * 1024);
        let this = self.get_mut();
        this.calls.fetch_add(1, Ordering::Relaxed);
        Pin::new(&mut this.inner).poll_read(cx, buffer)
    }
}

#[tokio::test]
async fn sequential_duplex_save_is_bounded_exact_and_reports_progress() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let destination = root.join("saved");
    let data: Vec<_> = (0..200_000).map(|n| (n % 251) as u8).collect();
    let (mut sender, receiver) = tokio::io::duplex(8191);
    let producer = tokio::spawn({
        let data = data.clone();
        async move {
            sender.write_all(&data).await.unwrap();
            sender.shutdown().await.unwrap();
        }
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let reader = Checked {
        inner: receiver,
        calls: calls.clone(),
    };
    let cancel = CancellationToken::new();
    let mut progress = Vec::new();
    save_reader(
        reader,
        data.len() as u64,
        destination.clone(),
        &cancel,
        |p| progress.push(p),
    )
    .await
    .unwrap();
    producer.await.unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), data);
    assert!(calls.load(Ordering::Relaxed) > 3);
    assert_eq!(
        progress[0],
        SaveProgress {
            written_bytes: 0,
            total_bytes: 200_000
        }
    );
    assert_eq!(progress.last().unwrap().written_bytes, 200_000);
    assert!(
        progress
            .windows(2)
            .all(|p| p[0].written_bytes < p[1].written_bytes)
    );
    // Cancellation after the atomic publication must not remove a saved file.
    cancel.cancel();
    assert!(destination.exists());
    assert_no_temps(&root);
}

#[tokio::test]
async fn empty_exact_truncated_and_excess_bodies_require_exact_eof() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for (index, (body, total, expected)) in [
        (b"".as_slice(), 0, Ok(())),
        (b"x".as_slice(), 0, Err(Error::Source)),
        (b"abc".as_slice(), 4, Err(Error::Source)),
        (b"abcde".as_slice(), 4, Err(Error::Source)),
        (b"abcd".as_slice(), 4, Ok(())),
    ]
    .into_iter()
    .enumerate()
    {
        let destination = root.join(format!("saved{index}"));
        let calls = Arc::new(AtomicUsize::new(0));
        let reader = Checked {
            inner: Cursor::new(body.to_vec()),
            calls: calls.clone(),
        };
        let result = save_reader(
            reader,
            total,
            destination.clone(),
            &CancellationToken::new(),
            |_| {},
        )
        .await;
        assert_eq!(result, expected);
        if expected.is_ok() {
            assert_eq!(std::fs::read(destination).unwrap(), body);
        } else {
            assert!(!destination.exists());
        }
        // Even an advertised zero-length body must be read once to confirm EOF.
        assert!(calls.load(Ordering::Relaxed) > 0);
        assert_no_temps(&root);
    }
}

struct ErrorReader;

impl AsyncRead for ErrorReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("PRIVATE transport URL")))
    }
}

#[tokio::test]
async fn transport_failure_during_body_or_eof_probe_is_private_and_cleans_up() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for total in [4, 8] {
        let destination = root.join(format!("saved{total}"));
        let reader = Cursor::new(b"data".to_vec()).chain(ErrorReader);
        let error = save_reader(
            reader,
            total,
            destination.clone(),
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap_err();
        assert_eq!(error, Error::Source);
        assert!(!format!("{error} {error:?}").contains("PRIVATE"));
        assert!(!destination.exists());
        assert_no_temps(&root);
    }
}

#[tokio::test]
async fn existing_and_racing_destinations_are_never_clobbered() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let destination = root.join("existing");
    std::fs::write(&destination, b"old").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let reader = Checked {
        inner: Cursor::new(b"new".to_vec()),
        calls: calls.clone(),
    };
    assert_eq!(
        save_reader(
            reader,
            3,
            destination.clone(),
            &CancellationToken::new(),
            |_| {}
        )
        .await,
        Err(Error::AlreadyExists),
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(destination).unwrap(), b"old");
    let destination = root.join("race");
    assert_eq!(
        save_reader(
            Cursor::new(b"new".to_vec()),
            3,
            destination.clone(),
            &CancellationToken::new(),
            |p| {
                assert!(!destination.exists());
                if p.written_bytes == p.total_bytes {
                    std::fs::write(&destination, b"competitor").unwrap();
                }
            }
        )
        .await,
        Err(Error::AlreadyExists),
    );
    assert_eq!(std::fs::read(destination).unwrap(), b"competitor");
    assert_no_temps(&root);
}

#[tokio::test]
async fn cancelled_before_start_or_at_final_progress_never_publishes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for before_start in [true, false] {
        let cancel = CancellationToken::new();
        if before_start {
            cancel.cancel();
        }
        let destination = root.join(format!("cancelled{before_start}"));
        assert_eq!(
            save_reader(
                Cursor::new(b"data".to_vec()),
                4,
                destination.clone(),
                &cancel,
                |p| {
                    if p.written_bytes == p.total_bytes {
                        cancel.cancel();
                    }
                }
            )
            .await,
            Err(Error::Cancelled),
        );
        assert!(!destination.exists());
        assert_no_temps(&root);
    }
}

#[tokio::test]
async fn cancellation_interrupts_eof_probe_including_empty_body() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for total in [0, 4] {
        let (mut sender, receiver) = tokio::io::duplex(16);
        if total != 0 {
            sender.write_all(b"data").await.unwrap();
        }
        // Keep the sender alive: the EOF probe must wait, not publish early.
        let ready = Arc::new(tokio::sync::Notify::new());
        let cancel = CancellationToken::new();
        let destination = root.join(format!("cancelled{total}"));
        let job = tokio::spawn({
            let ready = ready.clone();
            let cancel = cancel.clone();
            let destination = destination.clone();
            async move {
                save_reader(receiver, total, destination, &cancel, |p| {
                    if p.written_bytes == p.total_bytes {
                        ready.notify_one();
                    }
                })
                .await
            }
        });
        ready.notified().await;
        assert!(!destination.exists());
        cancel.cancel();
        assert_eq!(job.await.unwrap(), Err(Error::Cancelled));
        assert!(!destination.exists());
        assert_no_temps(&root);
        drop(sender);
    }
}

#[tokio::test]
async fn cancellation_and_task_abort_while_body_waits_remove_temporary() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for abort in [false, true] {
        let (mut sender, receiver) = tokio::io::duplex(16);
        sender.write_all(b"data").await.unwrap();
        let ready = Arc::new(tokio::sync::Notify::new());
        let cancel = CancellationToken::new();
        let destination = root.join(format!("interrupted{abort}"));
        let job = tokio::spawn({
            let ready = ready.clone();
            let cancel = cancel.clone();
            let destination = destination.clone();
            async move {
                save_reader(receiver, 8, destination, &cancel, |p| {
                    if p.written_bytes == 4 {
                        ready.notify_one();
                    }
                })
                .await
            }
        });
        ready.notified().await;
        if abort {
            job.abort();
            assert!(job.await.unwrap_err().is_cancelled());
        } else {
            cancel.cancel();
            assert_eq!(job.await.unwrap(), Err(Error::Cancelled));
        }
        assert!(!destination.exists());
        assert_no_temps(&root);
        drop(sender);
    }
}

struct GatedReader {
    started: tokio::sync::oneshot::Receiver<()>,
    ready: bool,
    supplied: bool,
    read: Arc<tokio::sync::Notify>,
}

impl AsyncRead for GatedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if !this.ready {
            match Pin::new(&mut this.started).poll(cx) {
                Poll::Ready(Ok(())) => this.ready = true,
                Poll::Ready(Err(_)) => {
                    return Poll::Ready(Err(std::io::Error::other("gate closed")));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        if !this.supplied {
            buffer.put_slice(b"data");
            this.supplied = true;
            this.read.notify_one();
        }
        Poll::Ready(Ok(()))
    }
}

#[test]
fn task_abort_with_blocking_write_queued_eventually_cleans_temporary() {
    // A single blocking worker makes the write interruption deterministic,
    // without disk-fill simulation, production test hooks, or timing sleeps.
    tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let destination = root.join("interrupted");
            let (started, waiting) = tokio::sync::oneshot::channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let read = Arc::new(tokio::sync::Notify::new());
            let reader = GatedReader {
                started: waiting,
                ready: false,
                supplied: false,
                read: read.clone(),
            };
            let job = tokio::spawn({
                let destination = destination.clone();
                async move {
                    let mut blocker = Some((started, blocked));
                    save_reader(reader, 4, destination, &CancellationToken::new(), |p| {
                        if p.written_bytes == 0 {
                            let (started, blocked) = blocker.take().unwrap();
                            tokio::task::spawn_blocking(move || {
                                let _ = started.send(());
                                blocked.recv().unwrap();
                            });
                        }
                    })
                    .await
                }
            });
            read.notified().await;
            // The reader returned its chunk only after the blocking worker was
            // occupied. Save is now suspended on its queued bounded write.
            job.abort();
            assert!(job.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            assert!(!destination.exists());
            assert_no_temps(&root);
        });
}
