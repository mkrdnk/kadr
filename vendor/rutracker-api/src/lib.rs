//! Locally hardened subset of published rutracker-api 0.2.1.
//! See PATCHES.md for provenance and intentional differences from upstream.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
pub mod client;
pub mod error;
pub mod models;
pub mod search;
pub mod topic;

mod auth;
mod download;
mod http;
mod parse_util;

pub use crate::client::{AuthenticatedClient, Client, ClientBuilder};
pub use crate::error::{Error, Result};
pub use crate::models::{InfoHash, TopicId, TopicState};
pub use crate::search::{
    MAX_QUERY_LEN, Order, PAGE_SIZE, SearchRequest, SearchResults, Sort, TorrentSummary,
};
pub use crate::topic::Topic;

#[cfg(test)]
mod tests;
