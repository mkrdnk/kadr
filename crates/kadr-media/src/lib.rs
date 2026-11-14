//! P0 loopback HTTP Range and external-player spikes, not a torrent engine.
//!
//! The URL is a private bearer capability: do not log it or the source's path.
//! Sources must keep a stable length and byte contents for the entire session.

mod range;
mod remote;
mod server;
mod source;

/// External-player P0 adapters for Linux and macOS.
#[cfg(unix)]
pub mod players;

pub use remote::RemoteSource;
pub use server::{Server, ServerOptions};
pub use source::{ByteSource, LocalFile, ReadFuture};
