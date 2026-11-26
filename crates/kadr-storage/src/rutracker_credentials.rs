//! One RuTracker account in the current OS user's credential store.
//!
//! Linux uses the default Secret Service collection; macOS uses a generic
//! Keychain password. Both fields are stored together as the secret, not as
//! public item metadata. There is no file, database, environment, or CLI fallback.
//!
//! All calls block and may display a native unlock/access prompt. Run them on a
//! background worker, never on the UI thread. Missing items are not errors;
//! unavailable services, denied access, and corrupt items are errors.
//! Linux requires an existing default collection: without one, loading returns
//! `None`, forgetting succeeds, and storing reports an unavailable store.
//! macOS items are explicitly non-synchronizing (not iCloud Keychain items).
//!
//! Secret Service's `plain` session transports the secret over the local user's
//! D-Bus connection; encryption at rest is the credential service's responsibility.
//! No encrypted D-Bus session is negotiated. Secrets are ordinary Rust allocations
//! and are not guaranteed to be erased from memory after use.

#![forbid(unsafe_code)]

pub use crate::Error;
use crate::Result;

#[cfg(target_os = "linux")]
#[path = "rutracker_credentials/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "rutracker_credentials/macos.rs"]
mod platform;

const SERVICE: &str = "dev.kadr.Kadr";
const ACCOUNT: &str = "rutracker";
const HEADER: &[u8] = b"kadr-rutracker-v1\0";

/// Sensitive fields deliberately have no `Debug` implementation.
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Loads the single saved account, or `None` if no item exists.
pub fn load() -> Result<Option<Credentials>> {
    platform::load()?.map(decode).transpose()
}

/// Replaces the saved account's secret, including its username.
pub fn store(credentials: &Credentials) -> Result<()> {
    store_if(credentials, || true)
}

/// Rechecks user intent immediately before issuing the native write request.
/// A request already handed to the operating system cannot be undone here.
pub fn store_if(credentials: &Credentials, should_write: impl FnOnce() -> bool) -> Result<()> {
    platform::store(&encode(credentials), should_write)
}

/// Deletes the saved account. Repeated calls succeed if the store is accessible.
pub fn forget() -> Result<()> {
    platform::forget()
}

// A versioned, length-prefixed blob preserves arbitrary UTF-8 (including NUL)
// without putting the username into searchable keychain metadata.
fn encode(credentials: &Credentials) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    bytes.extend_from_slice(&(credentials.username.len() as u64).to_be_bytes());
    bytes.extend_from_slice(credentials.username.as_bytes());
    bytes.extend_from_slice(credentials.password.as_bytes());
    bytes
}

fn decode(bytes: Vec<u8>) -> Result<Credentials> {
    let corrupt = || Error::CorruptData("RuTracker credentials");
    let bytes = bytes.strip_prefix(HEADER).ok_or_else(corrupt)?;
    let length = bytes.get(..8).ok_or_else(corrupt)?;
    let length = u64::from_be_bytes(length.try_into().map_err(|_| corrupt())?);
    let length = usize::try_from(length).map_err(|_| corrupt())?;
    let bytes = &bytes[8..];
    let username = bytes.get(..length).ok_or_else(corrupt)?;
    let password = bytes.get(length..).ok_or_else(corrupt)?;
    Ok(Credentials {
        username: String::from_utf8(username.to_vec()).map_err(|_| corrupt())?,
        password: String::from_utf8(password.to_vec()).map_err(|_| corrupt())?,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    pub(super) fn load() -> Result<Option<Vec<u8>>> {
        Err(Error::CredentialStoreUnavailable)
    }

    pub(super) fn store(_: &[u8], _: impl FnOnce() -> bool) -> Result<()> {
        Err(Error::CredentialStoreUnavailable)
    }

    pub(super) fn forget() -> Result<()> {
        Err(Error::CredentialStoreUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_roundtrip_without_losing_utf8_or_nul() {
        for (username, password) in [("", ""), ("пользователь\0", "🔒\0\npassword")] {
            let credentials = Credentials {
                username: username.into(),
                password: password.into(),
            };
            let loaded = decode(encode(&credentials)).unwrap();
            assert_eq!(loaded.username, username);
            assert_eq!(loaded.password, password);
        }
    }

    #[test]
    fn rejects_corrupt_and_future_payloads_without_exposing_them() {
        let mut invalid_utf8 = HEADER.to_vec();
        invalid_utf8.extend_from_slice(&0_u64.to_be_bytes());
        invalid_utf8.push(0xff);
        let mut invalid_length = HEADER.to_vec();
        invalid_length.extend_from_slice(&u64::MAX.to_be_bytes());
        for bytes in [
            Vec::new(),
            b"private-password".to_vec(),
            b"kadr-rutracker-v2\0".to_vec(),
            HEADER.to_vec(),
            invalid_utf8,
            invalid_length,
        ] {
            let Err(error) = decode(bytes) else {
                panic!("corrupt secret was accepted");
            };
            assert_eq!(error, Error::CorruptData("RuTracker credentials"));
            assert_eq!(error.to_string(), "invalid stored RuTracker credentials");
            assert!(std::error::Error::source(&error).is_none());
        }
    }
}
