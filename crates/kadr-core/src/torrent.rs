//! Lexical path safety and conservative episode recognition.
//! These checks do not replace symlink-safe filesystem operations in the IO layer.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{fmt, path::Path, sync::LazyLock};

/// Portable relative metadata path, split using `/`, never host-OS path normalization.
/// The original name is exposed only through explicit accessors, not `Debug`.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct TorrentRelativePath(String);

impl fmt::Debug for TorrentRelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TorrentRelativePath([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTorrentPath;

impl fmt::Display for InvalidTorrentPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Torrent file names must be safe relative paths without traversal or reserved components.")
    }
}

impl std::error::Error for InvalidTorrentPath {}

impl TorrentRelativePath {
    pub fn new(path: impl Into<String>) -> Result<Self, InvalidTorrentPath> {
        let path = path.into();
        if path.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || part
                    .chars()
                    .any(|c| c.is_control() || r#"\:<>|"*?"#.contains(c))
                || reserved_component(part)
        }) {
            return Err(InvalidTorrentPath);
        }
        Ok(Self(path))
    }

    /// Sensitive metadata; do not include in routine logs.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Join only beneath a trusted directory, without following untrusted symlinks.
    /// Percent escapes are literal filename characters, never URL-decoded.
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().expect("validated nonempty path")
    }
}

fn reserved_component(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// Season zero is allowed for specials; episode zero is not recognized automatically.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "StoredEpisodeNumber")]
pub struct EpisodeNumber {
    season: u16,
    episode: u16,
}

#[derive(Deserialize)]
struct StoredEpisodeNumber {
    season: u16,
    episode: u16,
}

impl TryFrom<StoredEpisodeNumber> for EpisodeNumber {
    type Error = &'static str;

    fn try_from(value: StoredEpisodeNumber) -> Result<Self, Self::Error> {
        Self::new(value.season, value.episode).ok_or("Episode number must be nonzero.")
    }
}

impl EpisodeNumber {
    pub fn new(season: u16, episode: u16) -> Option<Self> {
        (episode != 0).then_some(Self { season, episode })
    }
    pub fn season(self) -> u16 {
        self.season
    }
    pub fn episode(self) -> u16 {
        self.episode
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EpisodeRecognition {
    Recognized(EpisodeNumber),
    /// Multiple markers, multi-episode files/ranges, or out-of-range numbers.
    /// Only successfully parsed numbers are listed; an empty list is possible.
    Ambiguous(Vec<EpisodeNumber>),
    Unrecognized,
}

static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:s[0-9]+e[0-9]+|[0-9]+x[0-9]+)(?:(?:[ ._-]*e|[ ._]*-[ ._]*e?)[0-9]+)*")
        .expect("constant episode regex")
});
static FIRST_EPISODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:s([0-9]+)e([0-9]+)|([0-9]+)x([0-9]+))").expect("constant episode regex")
});
static EXTRA_EPISODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[e ._-]+([0-9]+)").expect("constant continuation regex"));

/// Inspect a basename, not parent directories. No guessing from title or file order.
pub fn recognize_episode(file_name: &str) -> EpisodeRecognition {
    let mut episodes = Vec::new();
    let mut marker_count = 0;
    let mut ambiguous = false;
    for marker in MARKER.find_iter(file_name) {
        let before = file_name[..marker.start()].chars().next_back();
        let after = file_name[marker.end()..].chars().next();
        if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
            continue;
        }
        let captures = FIRST_EPISODE
            .captures(marker.as_str())
            .expect("marker prefix");
        // A four-digit x-prefix is normally a resolution (1280x720), not a season.
        // Keep the x-form deliberately narrow instead of inventing an episode.
        if captures
            .get(3)
            .is_some_and(|season| season.as_str().len() > 2)
        {
            continue;
        }
        marker_count += 1;
        let season = captures
            .get(1)
            .or_else(|| captures.get(3))
            .unwrap()
            .as_str()
            .parse::<u16>();
        let episode = captures
            .get(2)
            .or_else(|| captures.get(4))
            .unwrap()
            .as_str()
            .parse::<u16>();
        match (season.as_ref(), episode) {
            (Ok(&s), Ok(e)) if e != 0 => episodes.push(EpisodeNumber {
                season: s,
                episode: e,
            }),
            _ => ambiguous = true,
        }
        let suffix = &marker.as_str()[captures.get(0).unwrap().end()..];
        if !suffix.is_empty() {
            ambiguous = true;
            for extra in EXTRA_EPISODE.captures_iter(suffix) {
                if let (Ok(&s), Ok(e)) = (season.as_ref(), extra[1].parse::<u16>())
                    && e != 0
                {
                    episodes.push(EpisodeNumber {
                        season: s,
                        episode: e,
                    });
                }
            }
        }
    }
    if marker_count == 0 {
        EpisodeRecognition::Unrecognized
    } else if ambiguous || marker_count > 1 {
        episodes.sort_unstable();
        episodes.dedup();
        EpisodeRecognition::Ambiguous(episodes)
    } else {
        EpisodeRecognition::Recognized(episodes[0])
    }
}

/// Engine metadata index, not a vector offset or an unchecked path.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileIndex(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoCandidate {
    index: FileIndex,
    path: TorrentRelativePath,
    size: u64,
    episode: EpisodeRecognition,
}

impl VideoCandidate {
    /// Conservative extension allowlist; selection does not promise codec support.
    /// Unknown episode names and zero-sized video entries remain manually selectable.
    pub fn from_file(index: FileIndex, path: TorrentRelativePath, size: u64) -> Option<Self> {
        let name = path.file_name();
        let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "mkv"
                | "mp4"
                | "m4v"
                | "avi"
                | "mov"
                | "webm"
                | "ts"
                | "m2ts"
                | "mts"
                | "mpg"
                | "mpeg"
                | "ogv"
        ) {
            return None;
        }
        let episode = recognize_episode(name);
        Some(Self {
            index,
            path,
            size,
            episode,
        })
    }
    pub fn index(&self) -> FileIndex {
        self.index
    }
    pub fn path(&self) -> &TorrentRelativePath {
        &self.path
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn episode(&self) -> &EpisodeRecognition {
        &self.episode
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EpisodeMatch {
    NotFound,
    Unique(FileIndex),
    /// Includes duplicate encodes and multi-episode files; never auto-select one.
    Ambiguous(Vec<FileIndex>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionError {
    DuplicateFileIndex,
    UnknownFileIndex,
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DuplicateFileIndex => "Torrent metadata contains duplicate file indices.",
            Self::UnknownFileIndex => "The selected file is not in the video candidate list.",
        })
    }
}
impl std::error::Error for SelectionError {}

/// Recognition is advisory. Matching never mutates a user's explicit selection.
#[derive(Clone, Debug)]
pub struct VideoSelection {
    candidates: Vec<VideoCandidate>,
    selected: Option<FileIndex>,
}

impl VideoSelection {
    pub fn new(candidates: Vec<VideoCandidate>) -> Result<Self, SelectionError> {
        let mut indices = std::collections::HashSet::new();
        if candidates.iter().any(|c| !indices.insert(c.index)) {
            return Err(SelectionError::DuplicateFileIndex);
        }
        Ok(Self {
            candidates,
            selected: None,
        })
    }

    pub fn candidates(&self) -> &[VideoCandidate] {
        &self.candidates
    }
    pub fn selected(&self) -> Option<FileIndex> {
        self.selected
    }

    pub fn select(&mut self, index: FileIndex) -> Result<(), SelectionError> {
        if !self.candidates.iter().any(|c| c.index == index) {
            return Err(SelectionError::UnknownFileIndex);
        }
        self.selected = Some(index);
        Ok(())
    }

    pub fn match_episode(&self, target: EpisodeNumber) -> EpisodeMatch {
        let mut matches = Vec::new();
        let mut ambiguous = false;
        for candidate in &self.candidates {
            match &candidate.episode {
                EpisodeRecognition::Recognized(number) if *number == target => {
                    matches.push(candidate.index)
                }
                EpisodeRecognition::Ambiguous(numbers) if numbers.contains(&target) => {
                    ambiguous = true;
                    matches.push(candidate.index);
                }
                _ => {}
            }
        }
        match matches.as_slice() {
            [] => EpisodeMatch::NotFound,
            [index] if !ambiguous => EpisodeMatch::Unique(*index),
            _ => EpisodeMatch::Ambiguous(matches),
        }
    }
}
