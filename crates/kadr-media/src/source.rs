use std::{future::Future, io, path::Path, pin::Pin, sync::Arc};

use bytes::Bytes;

/// A cancellable read of an available prefix at an absolute offset.
pub type ReadFuture<'a> = Pin<Box<dyn Future<Output = io::Result<Bytes>> + Send + 'a>>;

/// The deliberately small boundary between HTTP and byte availability.
///
/// Length and contents must remain stable during a session. A read must wait
/// asynchronously if its bytes are unavailable, return at most `max_len` bytes,
/// and never return an empty buffer before EOF. Dropping the future must release
/// availability waits; the server does this on disconnect, timeout, and stop.
/// Errors are not exposed as HTTP text (they could contain private source data).
pub trait ByteSource: Send + Sync + 'static {
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_>;
}

/// An already opened regular file. Positional reads never share a seek cursor.
///
/// Keep this file unchanged while serving it. The open handle pins the file,
/// so renaming/replacing the original path does not switch the authorized source.
pub struct LocalFile {
    file: Arc<std::fs::File>,
    length: u64,
}

impl LocalFile {
    pub async fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true);
        // A FIFO can block in open before its file type can be checked.
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK);
        let file = options.open(path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source must be a regular file",
            ));
        }
        Ok(Self {
            file: Arc::new(file.into_std().await),
            length: metadata.len(),
        })
    }
}

impl ByteSource for LocalFile {
    fn len(&self) -> u64 {
        self.length
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        let file = self.file.clone();
        Box::pin(async move {
            // File I/O can block in the OS. Keep it off the async executor.
            // The bounded read has no availability wait or detached async task.
            tokio::task::spawn_blocking(move || {
                let mut buffer = vec![0; max_len];
                loop {
                    #[cfg(unix)]
                    let result = {
                        use std::os::unix::fs::FileExt;
                        file.read_at(&mut buffer, offset)
                    };
                    #[cfg(windows)]
                    let result = {
                        use std::os::windows::fs::FileExt;
                        file.seek_read(&mut buffer, offset)
                    };
                    match result {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        result => {
                            buffer.truncate(result?);
                            return Ok(Bytes::from(buffer));
                        }
                    }
                }
            })
            .await
            .map_err(io::Error::other)?
        })
    }
}
