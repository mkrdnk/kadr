//! Common newtype wrappers and enums.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Numeric topic identifier (e.g. `5956108` from `viewtopic.php?t=5956108`).
///
/// Construct via [`TopicId::new`], `From<u64>`, or [`FromStr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopicId(u64);

impl TopicId {
    /// Wrap a raw `u64` topic id.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Return the raw integer value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TopicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u64> for TopicId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl FromStr for TopicId {
    type Err = Error;

    /// Parse a decimal topic id.
    ///
    /// # Errors
    /// Returns [`Error::Parse`] when the string is not a valid `u64`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim()
            .parse::<u64>()
            .map(TopicId)
            .map_err(|e| Error::parse("models::TopicId", format!("invalid topic_id {s:?}: {e}")))
    }
}

/// SHA-1 info-hash of a torrent (20 bytes, hex-encoded over the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InfoHash([u8; 20]);

impl InfoHash {
    /// Wrap raw 20 bytes.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    /// Lowercase hex (40 characters).
    pub fn to_hex(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for InfoHash {
    type Err = Error;

    /// Parse a 40-character lowercase or uppercase hex string.
    ///
    /// # Errors
    /// Returns [`Error::Parse`] when the length differs from 40 chars or any
    /// byte is not valid hex.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.len() != 40 {
            return Err(Error::parse(
                "models::InfoHash",
                format!("info_hash must be 40 hex chars, got {} ({s:?})", s.len()),
            ));
        }
        let mut out = [0u8; 20];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex = std::str::from_utf8(chunk).map_err(|e| {
                Error::parse(
                    "models::InfoHash",
                    format!("info_hash contains non-ASCII at {i}: {e}"),
                )
            })?;
            out[i] = u8::from_str_radix(hex, 16).map_err(|e| {
                Error::parse(
                    "models::InfoHash",
                    format!("info_hash byte {i} ({hex:?}) invalid: {e}"),
                )
            })?;
        }
        Ok(InfoHash(out))
    }
}

impl Serialize for InfoHash {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for InfoHash {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Moderation / approval state of a topic on rutracker.
///
/// Marked `#[non_exhaustive]`: rutracker may introduce new states.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TopicState {
    /// Approved by moderators (rutracker label: "проверено").
    Approved,
    /// Awaiting moderator approval (rutracker label: "не проверено").
    NotApproved,
    /// Needs metadata fixes (rutracker label: "недооформлено").
    NeedEdit,
    /// Doubtful — flagged by moderators (rutracker label: "сомнительно").
    Dubiously,
    /// Merged into another topic (rutracker label: "поглощена").
    Consumed,
    /// Temporary upload (rutracker label: "временная").
    Temporary,
    /// Closed by moderators (rutracker label: "закрыто").
    Closed,
    /// Recognised label not covered above — original Russian text preserved.
    Other(String),
}

impl TopicState {
    /// Map a Russian status label from rutracker HTML to the corresponding enum.
    pub fn from_label(label: &str) -> Self {
        match label.trim() {
            "проверено" => Self::Approved,
            "не проверено" => Self::NotApproved,
            "недооформлено" => Self::NeedEdit,
            "сомнительно" => Self::Dubiously,
            "поглощена" => Self::Consumed,
            "временная" => Self::Temporary,
            "закрыто" => Self::Closed,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl fmt::Display for TopicState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Approved => f.write_str("проверено"),
            Self::NotApproved => f.write_str("не проверено"),
            Self::NeedEdit => f.write_str("недооформлено"),
            Self::Dubiously => f.write_str("сомнительно"),
            Self::Consumed => f.write_str("поглощена"),
            Self::Temporary => f.write_str("временная"),
            Self::Closed => f.write_str("закрыто"),
            Self::Other(s) => f.write_str(s),
        }
    }
}
