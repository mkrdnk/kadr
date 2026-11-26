use std::sync::Mutex;

use reqwest::{cookie::CookieStore, header::HeaderValue};
use url::Url;

use super::builder::authorized_url;

/// reqwest's default Jar calls cookie_store::store_response_cookies, which
/// logs complete non-Secure cookies at debug level. Use the same RFC6265
/// implementation's non-logging parse/insert path instead, not a log filter.
#[derive(Default)]
pub(super) struct SilentJar(Mutex<cookie_store::CookieStore>);

impl CookieStore for SilentJar {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        if !authorized_url(url) {
            return;
        }
        let Ok(mut store) = self.0.lock() else { return };
        for header in headers {
            if header.as_bytes().len() > 4_096 {
                continue;
            }
            if let Ok(value) = header.to_str() {
                let _ = store.parse(value, url);
                // Bound the session even if the trusted service changes behavior.
                if store.iter_any().count() > 64 {
                    store.clear();
                    break;
                }
            }
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        if !authorized_url(url) {
            return None;
        }
        let store = self.0.lock().ok()?;
        let value = store
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if value.is_empty() {
            return None;
        }
        let mut header = HeaderValue::from_str(&value).ok()?;
        header.set_sensitive(true);
        Some(header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_memory_only_origin_and_path_scoped_and_expire() {
        let jar = SilentJar::default();
        let origin = Url::parse("https://rutracker.org/forum/login.php").unwrap();
        let headers = [
            HeaderValue::from_static("session=test-only; Path=/forum; HttpOnly"),
            HeaderValue::from_static("foreign=value; Domain=evil.example; Path=/"),
        ];
        jar.set_cookies(&mut headers.iter(), &origin);
        let cookie = jar.cookies(&origin).unwrap();
        assert_eq!(cookie.to_str().unwrap(), "session=test-only");
        assert!(cookie.is_sensitive());
        assert!(
            jar.cookies(&Url::parse("https://evil.example/forum/").unwrap())
                .is_none()
        );
        assert!(
            jar.cookies(&Url::parse("http://rutracker.org/forum/").unwrap())
                .is_none()
        );
        assert!(
            jar.cookies(&Url::parse("https://rutracker.org/elsewhere").unwrap())
                .is_none()
        );
        assert!(SilentJar::default().cookies(&origin).is_none());
        let expired = [HeaderValue::from_static("session=; Path=/forum; Max-Age=0")];
        jar.set_cookies(&mut expired.iter(), &origin);
        assert!(jar.cookies(&origin).is_none());
    }
}
