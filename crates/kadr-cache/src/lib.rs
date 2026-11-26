//! Worker-only disk cache and interruption-safe, bounded streaming saves.
//!
//! Pass `AppPaths::cache_dir().join("torrents-v1")` to [`CacheManager::open`].
//! Reserve the WHOLE torrent before creating any torrent files, and retain its
//! lease until every writer/reader/save has stopped. Never save inside this root.
//! All filesystem operations require Unix (the desktop targets Linux/macOS).

#[cfg(not(unix))]
compile_error!("kadr-cache currently supports Linux and macOS only");

mod cache;
mod fs;
mod save;

pub use cache::{CacheLease, CacheManager, CacheUsage};
pub use save::{SaveProgress, save_reader, save_source};

/// Sanitized failures: no underlying OS/source errors, keys, or private paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidInput,
    UnsafePath,
    Busy,
    CorruptIndex,
    QuotaExceeded,
    ReservationExceeded,
    OutOfSpace,
    AlreadyExists,
    Cancelled,
    Source,
    Io,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid cache or save input",
            Self::UnsafePath => "unsafe filesystem entry",
            Self::Busy => "cache is already owned by another manager",
            Self::CorruptIndex => "cache index is invalid or unsupported",
            Self::QuotaExceeded => "cache quota cannot accommodate active reservations",
            Self::ReservationExceeded => "cache data exceeds its reservation; stop the writer",
            Self::OutOfSpace => "insufficient free disk space",
            Self::AlreadyExists => "save destination already exists",
            Self::Cancelled => "save cancelled",
            Self::Source => "source read failed or violated its byte contract",
            Self::Io => "cache or save filesystem operation failed",
        })
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn io_error(error: std::io::Error) -> Error {
    match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::NOSPC.raw_os_error() => Error::OutOfSpace,
        Some(code) if code == rustix::io::Errno::DQUOT.raw_os_error() => Error::OutOfSpace,
        _ if error.kind() == std::io::ErrorKind::AlreadyExists => Error::AlreadyExists,
        _ => Error::Io,
    }
}
