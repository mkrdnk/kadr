use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// Deliberately excludes underlying OS/SQLite messages, SQL, paths and IDs.
/// `Debug`, `Display` and `source()` are safe for ordinary application logs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    InvalidInput(&'static str),
    CorruptData(&'static str),
    DirectoriesUnavailable,
    Io,
    Database,
    FutureSchema { found: i64, supported: i64 },
    InvalidSchemaVersion,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(field) => write!(f, "invalid {field}"),
            Self::CorruptData(field) => write!(f, "invalid stored {field}"),
            Self::DirectoriesUnavailable => f.write_str("standard user directories unavailable"),
            Self::Io => f.write_str("storage filesystem operation failed"),
            Self::Database => f.write_str("storage database operation failed"),
            Self::FutureSchema { found, supported } => write!(
                f,
                "database schema {found} is newer than supported schema {supported}"
            ),
            Self::InvalidSchemaVersion => f.write_str("invalid database schema version"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}
