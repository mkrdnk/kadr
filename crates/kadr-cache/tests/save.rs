use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use kadr_cache::{CacheManager, Error, SaveProgress, save_source};
use kadr_media::{ByteSource, LocalFile, ReadFuture};
use tokio_util::sync::CancellationToken;

fn base(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().canonicalize().unwrap()
}

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

struct Memory {
    data: Bytes,
    calls: AtomicUsize,
    fail: bool,
}

impl ByteSource for Memory {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(max_len <= 64 * 1024);
        Box::pin(async move {
            if self.fail {
                return Err(std::io::Error::other("PRIVATE SOURCE URL"));
            }
            let start = offset as usize;
            // Short reads test offset advancement independently of chunk size.
            Ok(self
                .data
                .slice(start..(start + max_len.min(8191)).min(self.data.len())))
        })
    }
}

#[tokio::test]
async fn saves_bounded_bytes_progress_and_survives_cache_clear() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let manager = CacheManager::open(root.join("cache"), 300_000).unwrap();
    let lease = manager.reserve("torrent1", 200_000).unwrap();
    let data: Vec<_> = (0..200_000).map(|n| (n % 253) as u8).collect();
    std::fs::write(lease.path().join("selected.mp4"), &data).unwrap();
    let source = Arc::new(
        LocalFile::open(lease.path().join("selected.mp4"))
            .await
            .unwrap(),
    );
    let final_path = root.join("saved.mp4");
    let mut progress = Vec::new();
    save_source(
        source,
        final_path.clone(),
        &CancellationToken::new(),
        |value| progress.push(value),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&final_path).unwrap(), data);
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
    drop(lease);
    manager.clear_inactive().unwrap();
    assert_eq!(std::fs::read(&final_path).unwrap(), data);
    assert_no_temps(&root);
}

#[tokio::test]
async fn empty_and_short_reads_complete_without_whole_file_buffering() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    for length in [0, 1, 65_537, 200_000] {
        let source = Arc::new(Memory {
            data: Bytes::from(vec![19; length]),
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let output = root.join(format!("file{length}"));
        save_source(
            source.clone(),
            output.clone(),
            &CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(output).unwrap(), vec![19; length]);
        if length == 0 {
            assert_eq!(source.calls.load(Ordering::Relaxed), 0);
        }
    }
    assert_no_temps(&root);
}

#[tokio::test]
async fn no_clobber_including_concurrent_destination_creation() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let output = root.join("existing");
    std::fs::write(&output, b"old").unwrap();
    let source = Arc::new(Memory {
        data: Bytes::from_static(b"new"),
        calls: AtomicUsize::new(0),
        fail: false,
    });
    let result = save_source(
        source.clone(),
        output.clone(),
        &CancellationToken::new(),
        |_| {},
    )
    .await;
    assert_eq!(result, Err(Error::AlreadyExists));
    assert_eq!(source.calls.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read(&output).unwrap(), b"old");
    let race = root.join("race");
    let result = save_source(source, race.clone(), &CancellationToken::new(), |p| {
        assert!(!race.exists());
        if p.written_bytes == p.total_bytes {
            std::fs::write(&race, b"competitor").unwrap();
        }
    })
    .await;
    assert_eq!(result, Err(Error::AlreadyExists));
    assert_eq!(std::fs::read(race).unwrap(), b"competitor");
    assert_no_temps(&root);
}

struct Waiting {
    entered: tokio::sync::Notify,
}

impl ByteSource for Waiting {
    fn len(&self) -> u64 {
        10
    }

    fn read_at(&self, _: u64, _: usize) -> ReadFuture<'_> {
        Box::pin(async {
            self.entered.notify_one();
            std::future::pending().await
        })
    }
}

#[tokio::test]
async fn cancellation_while_waiting_verified_pieces_cleans_temporary() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let output = root.join("cancelled");
    let source = Arc::new(Waiting {
        entered: tokio::sync::Notify::new(),
    });
    let cancel = CancellationToken::new();
    let job = tokio::spawn({
        let source = source.clone();
        let cancel = cancel.clone();
        let output = output.clone();
        async move { save_source(source, output, &cancel, |_| {}).await }
    });
    source.entered.notified().await;
    assert!(!output.exists());
    cancel.cancel();
    assert_eq!(job.await.unwrap(), Err(Error::Cancelled));
    assert_no_temps(&root);
}

#[tokio::test]
async fn dropping_future_on_interruption_cleans_temporary() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let output = root.join("aborted");
    let source = Arc::new(Waiting {
        entered: tokio::sync::Notify::new(),
    });
    let job = tokio::spawn({
        let source = source.clone();
        let output = output.clone();
        async move { save_source(source, output, &CancellationToken::new(), |_| {}).await }
    });
    source.entered.notified().await;
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    assert!(!output.exists());
    assert_no_temps(&root);
}

#[tokio::test]
async fn failure_cancelled_before_start_and_symlinks_are_safe_and_redacted() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let output = root.join("PRIVATE PATH");
    let source = Arc::new(Memory {
        data: Bytes::from_static(b"bytes"),
        calls: AtomicUsize::new(0),
        fail: true,
    });
    let error = save_source(
        source.clone(),
        output.clone(),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(error, Error::Source);
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    assert!(!output.exists());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        save_source(source.clone(), output.clone(), &cancel, |_| {}).await,
        Err(Error::Cancelled)
    );
    std::os::unix::fs::symlink(root.join("outside"), &output).unwrap();
    assert_eq!(
        save_source(source, output, &CancellationToken::new(), |_| {}).await,
        Err(Error::AlreadyExists)
    );
    assert!(!root.join("outside").exists());
    assert_no_temps(&root);
}

struct InvalidSource {
    length: u64,
    oversized: bool,
}

impl ByteSource for InvalidSource {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_at(&self, _: u64, max: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            Ok(if self.oversized {
                Bytes::from(vec![0; max + 1])
            } else {
                Bytes::new()
            })
        })
    }
}

#[tokio::test]
async fn rejects_broken_source_contracts_and_free_space_failure_before_read() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let output = root.join("output");
    for oversized in [false, true] {
        let source = Arc::new(InvalidSource {
            length: 10,
            oversized,
        });
        assert_eq!(
            save_source(source, output.clone(), &CancellationToken::new(), |_| {}).await,
            Err(Error::Source),
        );
        assert!(!output.exists());
    }
    // A real filesystem preflight, with an intentionally impossible length.
    let source = Arc::new(InvalidSource {
        length: u64::MAX,
        oversized: false,
    });
    assert_eq!(
        save_source(source, output.clone(), &CancellationToken::new(), |_| {}).await,
        Err(Error::OutOfSpace),
    );
    assert!(!output.exists());
    assert_no_temps(&root);
}

#[tokio::test]
async fn destination_symlink_ancestors_are_never_followed() {
    let temp = tempfile::tempdir().unwrap();
    let root = base(&temp);
    let external = root.join("external");
    std::fs::create_dir(&external).unwrap();
    std::os::unix::fs::symlink(&external, root.join("link")).unwrap();
    let source = Arc::new(InvalidSource {
        length: 0,
        oversized: false,
    });
    assert_eq!(
        save_source(
            source,
            root.join("link/output"),
            &CancellationToken::new(),
            |_| {}
        )
        .await,
        Err(Error::UnsafePath),
    );
    assert!(std::fs::read_dir(external).unwrap().next().is_none());
    assert_no_temps(&root);
}
