//! P0 loopback HTTP Range spike, not a torrent engine.
//!
//! The URL is a private bearer capability: do not log it or the source's path.
//! Sources must keep a stable length and byte contents for the entire session.

mod range;
mod server;
mod source;

pub use server::{Server, ServerOptions};
pub use source::{ByteSource, LocalFile, ReadFuture};
