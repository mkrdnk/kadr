use std::{ffi::OsString, io::Write, path::PathBuf, sync::Arc};

use cap_std::fs::Dir;
use kadr_media::ByteSource;
use tokio_util::sync::CancellationToken;

use crate::{Error, Result, fs, io_error};

const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveProgress {
    pub written_bytes: u64,
    pub total_bytes: u64,
}

struct Writer {
    dir: Dir,
    file: std::fs::File,
    temporary: String,
    final_name: OsString,
}

impl Writer {
    fn open(destination: PathBuf, total: u64) -> Result<Self> {
        let name = destination
            .file_name()
            .ok_or(Error::InvalidInput)?
            .to_owned();
        let parent = destination.parent().ok_or(Error::InvalidInput)?;
        let dir = fs::absolute_dir(parent, false)?;
        let metadata = rustix::fs::fstat(&dir).map_err(|error| io_error(error.into()))?;
        if metadata.st_uid != rustix::process::geteuid().as_raw() || metadata.st_mode & 0o022 != 0 {
            return Err(Error::UnsafePath);
        }
        if fs::exists(&dir, &name)? {
            return Err(Error::AlreadyExists);
        }
        fs::ensure_space(fs::free_space(&dir)?, total)?;
        let temporary = format!(".kadr-save-{}.part", fs::token());
        let file = fs::new_file(&dir, &temporary)?;
        Ok(Self {
            dir,
            file,
            temporary,
            final_name: name,
        })
    }

    fn write(&mut self, bytes: &[u8], remaining: u64) -> Result<()> {
        fs::ensure_space(fs::free_space(&self.dir)?, remaining)?;
        self.file.write_all(bytes).map_err(io_error)
    }

    fn finish(self, cancel: &CancellationToken, abandoned: &CancellationToken) -> Result<()> {
        self.file.sync_all().map_err(io_error)?;
        if cancel.is_cancelled() || abandoned.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // hard_link is an atomic, no-clobber publication on the SAME filesystem.
        // Unlike rename, it cannot replace a concurrently created destination.
        self.dir
            .hard_link(&self.temporary, &self.dir, &self.final_name)
            .map_err(io_error)?;
        fs::sync_dir(&self.dir)?;
        self.dir.remove_file(&self.temporary).map_err(io_error)?;
        fs::sync_dir(&self.dir)
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // The handle pins the parent: never re-resolve a mutable ambient path.
        let _ = self.dir.remove_file(&self.temporary);
    }
}

struct Abandon(CancellationToken);

impl Drop for Abandon {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Stream verified/available prefixes to a permanent, no-clobber destination.
///
/// The source must obey ByteSource's stable length/contents contract. Torrent
/// implementations must wait for verified pieces. Keep the cache lease alive
/// throughout this future. The destination MUST be outside the cache root;
/// this independent function cannot infer which roots the application owns.
///
/// Reads are bounded to 64 KiB. All blocking filesystem work is offloaded.
/// Dropping this future/cancelling removes its temporary file; cancellation
/// cannot undo a final name already atomically published. Callbacks run on the
/// async caller, must be quick, and must not log private source/destination data.
pub async fn save_source(
    source: Arc<dyn ByteSource>,
    destination: PathBuf,
    cancel: &CancellationToken,
    mut progress: impl FnMut(SaveProgress) + Send,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let total = source.len();
    let abandon = Abandon(CancellationToken::new());
    let mut writer = tokio::task::spawn_blocking(move || Writer::open(destination, total))
        .await
        .map_err(|_| Error::Io)??;
    let mut written = 0;
    progress(SaveProgress {
        written_bytes: written,
        total_bytes: total,
    });
    while written < total {
        let max = (total - written).min(CHUNK as u64) as usize;
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = source.read_at(written, max) => result.map_err(|_| Error::Source)?,
        };
        if bytes.is_empty() || bytes.len() > max {
            return Err(Error::Source);
        }
        let count = bytes.len() as u64;
        writer = tokio::task::spawn_blocking(move || {
            writer.write(&bytes, total - written)?;
            Ok::<_, Error>(writer)
        })
        .await
        .map_err(|_| Error::Io)??;
        written += count;
        progress(SaveProgress {
            written_bytes: written,
            total_bytes: total,
        });
    }
    let cancel = cancel.clone();
    let abandoned = abandon.0.clone();
    tokio::task::spawn_blocking(move || writer.finish(&cancel, &abandoned))
        .await
        .map_err(|_| Error::Io)?
}
