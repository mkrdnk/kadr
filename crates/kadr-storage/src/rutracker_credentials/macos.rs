use security_framework::passwords::{
    PasswordOptions, delete_generic_password_options, generic_password,
    set_generic_password_options,
};

use super::{ACCOUNT, Error, Result, SERVICE};

// Security Framework OSStatus constants. Keeping these local avoids adding a
// direct security-framework-sys dependency solely for numeric error codes.
const ITEM_NOT_FOUND: i32 = -25300;

pub(super) fn load() -> Result<Option<Vec<u8>>> {
    match generic_password(options()) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
        Err(error) => Err(map_error(error)),
    }
}

pub(super) fn store(bytes: &[u8], should_write: impl FnOnce() -> bool) -> Result<()> {
    if !should_write() {
        return Err(Error::CredentialStoreDenied);
    }
    set_generic_password_options(bytes, options()).map_err(map_error)
}

pub(super) fn forget() -> Result<()> {
    match delete_generic_password_options(options()) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
        Err(error) => Err(map_error(error)),
    }
}

fn options() -> PasswordOptions {
    let mut options = PasswordOptions::new_generic_password(SERVICE, ACCOUNT);
    // Device storage only: do not match or update an iCloud-synchronized item.
    options.set_access_synchronized(Some(false));
    options
}

fn map_error(error: security_framework::Error) -> Error {
    match error.code() {
        -25291 => Error::CredentialStoreUnavailable, // errSecNotAvailable
        -25308 => Error::CredentialStoreLocked,      // errSecInteractionNotAllowed
        -25293 | -128 => Error::CredentialStoreDenied, // errSecAuthFailed / errSecUserCanceled
        _ => Error::CredentialStore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_classified_without_retaining_native_messages() {
        for (code, expected) in [
            (-25291, Error::CredentialStoreUnavailable),
            (-25308, Error::CredentialStoreLocked),
            (-25293, Error::CredentialStoreDenied),
            (-128, Error::CredentialStoreDenied),
            (-50, Error::CredentialStore),
        ] {
            let error = map_error(security_framework::Error::from_code(code));
            assert_eq!(error, expected);
            assert!(std::error::Error::source(&error).is_none());
        }
    }
}
