//! Validated manual inputs. Accessors exposing URLs/paths are intentionally explicit.

use sha2::{Digest, Sha256};
use std::{
    fmt,
    path::{Component, Path, PathBuf},
};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    Http,
    Magnet,
    TorrentFile,
}

/// Versioned, deterministic storage key, not an authorization token.
/// It does not contain the URL/path, but is not a defense against dictionary attacks.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceId(String);

impl SourceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn new(domain: &str, value: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(domain.as_bytes());
        hash.update([0]);
        hash.update(value);
        Self(format!("source:v1:{domain}:{:x}", hash.finalize()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSourceId;

impl fmt::Display for InvalidSourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The stored source identifier is invalid or uses an unsupported version.")
    }
}
impl std::error::Error for InvalidSourceId {}

impl std::str::FromStr for SourceId {
    type Err = InvalidSourceId;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let rest = input.strip_prefix("source:v1:").ok_or(InvalidSourceId)?;
        let (domain, digest) = rest.split_once(':').ok_or(InvalidSourceId)?;
        if !matches!(domain, "http" | "btih" | "btmh" | "torrent-path")
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidSourceId);
        }
        Ok(Self(input.to_owned()))
    }
}

#[derive(Clone)]
enum Input {
    Http(Url),
    Magnet(String),
    TorrentFile(PathBuf),
}

/// Construct only through validation. `Debug` never exposes user input.
#[derive(Clone)]
pub struct ManualSource {
    input: Input,
    id: SourceId,
}

impl fmt::Debug for ManualSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualSource")
            .field("kind", &self.kind())
            .field("input", &"[redacted]")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceError {
    Empty,
    InvalidUrl,
    UnsupportedScheme,
    CredentialsNotAllowed,
    MissingInfoHash,
    InvalidInfoHash,
    ConflictingInfoHashes,
    InvalidTorrentPath,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "Enter an HTTP(S) or magnet link.",
            Self::InvalidUrl => "The link is malformed; check its syntax.",
            Self::UnsupportedScheme => "Only HTTP(S) and magnet links are supported.",
            Self::CredentialsNotAllowed => "Links containing user credentials are not supported.",
            Self::MissingInfoHash => "The magnet link needs a supported BitTorrent info hash.",
            Self::InvalidInfoHash => "The magnet link contains an invalid BitTorrent info hash.",
            Self::ConflictingInfoHashes => "The magnet link contains conflicting info hashes.",
            Self::InvalidTorrentPath => "Choose an absolute local path to a .torrent file.",
        })
    }
}

impl std::error::Error for SourceError {}

impl ManualSource {
    pub fn parse_link(input: &str) -> Result<Self, SourceError> {
        if input.is_empty() {
            return Err(SourceError::Empty);
        }
        // WHATWG parsing intentionally repairs some malformed inputs. Do not silently
        // turn pasted whitespace, backslashes, or invalid escapes into a different URL.
        if input.chars().any(|c| c.is_whitespace() || c.is_control())
            || input.contains('\\')
            || !valid_escapes(input)
        {
            return Err(SourceError::InvalidUrl);
        }
        let url = Url::parse(input).map_err(|_| SourceError::InvalidUrl)?;
        match url.scheme() {
            "http" | "https" => Self::http(url, input),
            "magnet" => Self::magnet(url, input),
            _ => Err(SourceError::UnsupportedScheme),
        }
    }

    fn http(mut url: Url, input: &str) -> Result<Self, SourceError> {
        let (_, after_scheme) = input.split_once(':').ok_or(SourceError::InvalidUrl)?;
        if !after_scheme.starts_with("//")
            || after_scheme.starts_with("///")
            || url.host_str().is_none()
        {
            return Err(SourceError::InvalidUrl);
        }
        let authority = after_scheme[2..]
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("");
        if authority.contains('@') || !url.username().is_empty() || url.password().is_some() {
            return Err(SourceError::CredentialsNotAllowed);
        }
        if authority.ends_with(':') {
            return Err(SourceError::InvalidUrl);
        }
        // Fragments do not identify the HTTP resource and are never sent to the server.
        url.set_fragment(None);
        let id = SourceId::new("http", url.as_str().as_bytes());
        Ok(Self {
            input: Input::Http(url),
            id,
        })
    }

    fn magnet(url: Url, input: &str) -> Result<Self, SourceError> {
        if !input
            .split_once(':')
            .is_some_and(|(_, rest)| rest.starts_with('?'))
            || !url.path().is_empty()
            || url.has_host()
            || url.fragment().is_some()
        {
            return Err(SourceError::InvalidUrl);
        }
        let mut v1 = None;
        let mut v2 = None;
        for (key, value) in url.query_pairs() {
            if key != "xt" {
                continue;
            }
            let lower = value.to_ascii_lowercase();
            if let Some(hash) = lower.strip_prefix("urn:btih:") {
                let bytes = decode_btih(hash).ok_or(SourceError::InvalidInfoHash)?;
                insert_hash(&mut v1, bytes)?;
            } else if let Some(hash) = lower.strip_prefix("urn:btmh:") {
                // BEP 52: SHA-256 multihash (0x12, 0x20, 32 bytes).
                let hash = hash
                    .strip_prefix("1220")
                    .ok_or(SourceError::InvalidInfoHash)?;
                let bytes = decode_hex(hash, 32).ok_or(SourceError::InvalidInfoHash)?;
                insert_hash(&mut v2, bytes)?;
            }
        }
        // Hybrid magnets use their v1 identity; adding trackers, a display name or
        // an equivalent base32 xt never creates a second history entry.
        let id = if let Some(hash) = v1 {
            SourceId::new("btih", &hash)
        } else if let Some(hash) = v2 {
            SourceId::new("btmh", &hash)
        } else {
            return Err(SourceError::MissingInfoHash);
        };
        Ok(Self {
            input: Input::Magnet(input.to_owned()),
            id,
        })
    }

    /// No file is read. Existence, permissions and bencoded contents are IO-layer checks.
    /// Require absolute paths to avoid identities depending on an implicit working directory.
    pub fn torrent_file(path: PathBuf) -> Result<Self, SourceError> {
        if !path.is_absolute()
            || path.components().any(|c| matches!(c, Component::ParentDir))
            || path.as_os_str().as_encoded_bytes().contains(&0)
            || path.as_os_str().as_encoded_bytes().ends_with(b"/")
            || !path
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("torrent"))
        {
            return Err(SourceError::InvalidTorrentPath);
        }
        let path: PathBuf = path.components().collect();
        let id = SourceId::new("torrent-path", path.as_os_str().as_encoded_bytes());
        Ok(Self {
            input: Input::TorrentFile(path),
            id,
        })
    }

    pub fn kind(&self) -> SourceKind {
        match self.input {
            Input::Http(_) => SourceKind::Http,
            Input::Magnet(_) => SourceKind::Magnet,
            Input::TorrentFile(_) => SourceKind::TorrentFile,
        }
    }

    pub fn id(&self) -> SourceId {
        self.id.clone()
    }

    /// Sensitive: do not log the returned URL (including its query).
    pub fn http_url(&self) -> Option<&Url> {
        match &self.input {
            Input::Http(url) => Some(url),
            _ => None,
        }
    }

    /// Sensitive: may contain private trackers and display names.
    pub fn magnet_uri(&self) -> Option<&str> {
        match &self.input {
            Input::Magnet(uri) => Some(uri),
            _ => None,
        }
    }

    /// Sensitive: do not log the returned path.
    pub fn torrent_path(&self) -> Option<&Path> {
        match &self.input {
            Input::TorrentFile(path) => Some(path),
            _ => None,
        }
    }
}

fn valid_escapes(input: &str) -> bool {
    let bytes = input.as_bytes();
    bytes.iter().enumerate().all(|(i, b)| {
        *b != b'%'
            || bytes
                .get(i + 1..i + 3)
                .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
    })
}

fn insert_hash(slot: &mut Option<Vec<u8>>, hash: Vec<u8>) -> Result<(), SourceError> {
    if slot.as_ref().is_some_and(|old| *old != hash) {
        return Err(SourceError::ConflictingInfoHashes);
    }
    *slot = Some(hash);
    Ok(())
}

fn decode_hex(input: &str, len: usize) -> Option<Vec<u8>> {
    if input.len() != len * 2 || !input.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..len)
        .map(|i| u8::from_str_radix(&input[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn decode_btih(input: &str) -> Option<Vec<u8>> {
    if input.len() == 40 {
        return decode_hex(input, 20);
    }
    if input.len() != 32 {
        return None;
    }
    let mut bytes = Vec::with_capacity(20);
    let mut buffer = 0u32;
    let mut bits = 0;
    for b in input.bytes() {
        let digit = match b {
            b'a'..=b'z' => b - b'a',
            b'2'..=b'7' => b - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(digit);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
        }
    }
    Some(bytes)
}
