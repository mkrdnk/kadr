use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;

use crate::{Error, Result};

/// App-specific directories. Debug intentionally hides all private paths.
#[derive(Clone, PartialEq, Eq)]
pub struct AppPaths {
    data: PathBuf,
    config: PathBuf,
    cache: PathBuf,
}

impl AppPaths {
    /// Uses XDG on Linux and Application Support / Caches on macOS.
    /// Discovery alone does not create directories or perform disk I/O.
    pub fn discover() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "kadr").ok_or(Error::DirectoriesUnavailable)?;
        Self::from_roots(dirs.data_dir(), dirs.config_dir(), dirs.cache_dir())
    }

    /// Explicit app-specific roots, e.g. supplied by a future settings UI.
    /// These are trusted user choices, never torrent-provided paths.
    pub fn from_roots(
        data: impl Into<PathBuf>,
        config: impl Into<PathBuf>,
        cache: impl Into<PathBuf>,
    ) -> Result<Self> {
        let paths = Self {
            data: data.into(),
            config: config.into(),
            cache: cache.into(),
        };
        for path in [&paths.data, &paths.config, &paths.cache] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
                || path.as_os_str().as_encoded_bytes().contains(&0)
            {
                return Err(Error::InvalidInput("application directory"));
            }
        }
        Ok(paths)
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    pub fn database_path(&self) -> PathBuf {
        self.data.join("kadr.sqlite3")
    }

    /// Creates app-owned directories with mode 0700 on Unix. Existing directory
    /// permissions are not changed. Call only on a background worker.
    pub fn create_dirs(&self) -> Result<()> {
        for path in [&self.data, &self.config, &self.cache] {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|_| Error::Io)?;
        }
        Ok(())
    }
}

impl fmt::Debug for AppPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppPaths([redacted])")
    }
}

pub(crate) fn validate_private_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(Error::InvalidInput("private path"));
    }
    Ok(())
}

// A tagged native blob avoids lossy Unicode conversion and rejects accidental
// reinterpretation if a database is copied between incompatible platforms.
pub(crate) fn encode_path(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = b"unix\0".to_vec();
        bytes.extend_from_slice(path.as_os_str().as_bytes());
        bytes
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = b"windows\0".to_vec();
        bytes.extend(path.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        bytes
    }
}

pub(crate) fn decode_path(bytes: Vec<u8>) -> Result<PathBuf> {
    #[cfg(unix)]
    let path = {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let bytes = bytes
            .strip_prefix(b"unix\0")
            .ok_or(Error::CorruptData("path encoding"))?;
        PathBuf::from(OsString::from_vec(bytes.to_vec()))
    };
    #[cfg(windows)]
    let path = {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let bytes = bytes
            .strip_prefix(b"windows\0")
            .filter(|bytes| bytes.len() % 2 == 0)
            .ok_or(Error::CorruptData("path encoding"))?;
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        PathBuf::from(OsString::from_wide(&wide))
    };
    validate_private_path(&path).map_err(|_| Error::CorruptData("private path"))?;
    Ok(path)
}
