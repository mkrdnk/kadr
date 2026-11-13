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
