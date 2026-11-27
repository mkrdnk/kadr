use std::{sync::Arc, time::Duration};

use url::Url;

use crate::client::Client;
use crate::error::{Error, Result};

const BASE_URL: &str = "https://rutracker.org";

/// Desktop transport builder. The authorized origin cannot be overridden.
#[derive(Clone)]
pub struct ClientBuilder {
    timeout: Duration,
    connect_timeout: Duration,
    proxy: Option<reqwest::Proxy>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientBuilder {
    /// Create an HTTPS-only builder for the authorized domain.
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(15),
            connect_timeout: Duration::from_secs(5),
            proxy: None,
        }
    }

    /// Set the per-request deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the connection deadline.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Use an explicit proxy instead of a direct connection. The desktop adapter
    /// validates the endpoint and supplies `reqwest::Proxy::all`; environment
    /// proxies remain disabled. Cloned builders create independent cookie jars.
    pub fn proxy(mut self, proxy: reqwest::Proxy) -> Self {
        self.proxy = Some(proxy);
        self
    }

    /// Build a fresh, in-memory cookie session with normal TLS verification.
    pub fn build(self) -> Result<Client> {
        let mut http = reqwest::Client::builder()
            .user_agent("kadr/0.1 (local desktop; explicit user requests)")
            .timeout(self.timeout)
            .connect_timeout(self.connect_timeout)
            .cookie_provider(Arc::new(super::cookies::SilentJar::default()))
            .https_only(true)
            .referer(false)
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("redirect limit")
                } else if authorized_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("redirect outside authorized HTTPS origin")
                }
            }));
        if let Some(proxy) = self.proxy {
            http = http.proxy(proxy);
        }
        let http = http.build()?;
        Ok(Client::from_parts(http, Url::parse(BASE_URL)?))
    }
}

pub(crate) fn authorized_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("rutracker.org")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

pub(crate) fn check_url(url: &Url) -> Result<()> {
    if authorized_url(url) {
        Ok(())
    } else {
        Err(Error::InvalidArgument("unauthorized origin".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    #[test]
    fn only_authorized_https_origin_is_allowed() {
        for url in [
            "https://rutracker.org/forum/index.php",
            "https://rutracker.org:443/forum/login.php",
        ] {
            assert!(authorized_url(&Url::parse(url).unwrap()));
        }
        for url in [
            "http://rutracker.org/",
            "https://rutracker.org:444/",
            "https://rutracker.org.evil.example/",
            "https://evil.example/",
            "https://user@rutracker.org/",
            "https://user:password@rutracker.org/",
            "https://api.t-ru.org/",
        ] {
            assert!(!authorized_url(&Url::parse(url).unwrap()));
        }
    }

    async fn rejecting_proxy(socks: bool) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                if socks {
                    let mut greeting = [0; 3];
                    stream.read_exact(&mut greeting).await.unwrap();
                    assert_eq!(greeting, [5, 1, 0]);
                    stream.write_all(&[5, 0]).await.unwrap();
                    let mut request = [0; 5];
                    stream.read_exact(&mut request).await.unwrap();
                    assert_eq!(&request[..4], &[5, 1, 0, 3]);
                    let mut host = vec![0; request[4] as usize];
                    stream.read_exact(&mut host).await.unwrap();
                    assert_eq!(host, b"rutracker.org");
                    assert_eq!(stream.read_u16().await.unwrap(), 443);
                    stream
                        .write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                } else {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                        assert!(request.len() <= 8192);
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert_eq!(
                        request.lines().next(),
                        Some("CONNECT rutracker.org:443 HTTP/1.1")
                    );
                    assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
                    stream
                        .write_all(b"HTTP/1.1 502 Fixture\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .unwrap();
                }
            }
        });
        let endpoint = format!("{}://{address}", if socks { "socks5h" } else { "http" });
        (endpoint, task)
    }

    #[tokio::test]
    async fn explicit_proxy_covers_anonymous_login_and_authenticated_download() {
        for socks in [false, true] {
            let (endpoint, proxy) = rejecting_proxy(socks).await;
            let builder = ClientBuilder::new().proxy(reqwest::Proxy::all(endpoint).unwrap());
            let anonymous = builder.clone().build().unwrap();
            assert_eq!(anonymous.base_url(), "https://rutracker.org/");
            assert!(!format!("{anonymous:?}").contains("127.0.0.1"));
            assert!(matches!(
                timeout(Duration::from_secs(2), anonymous.get_magnet_link(42_u64))
                    .await
                    .unwrap(),
                Err(Error::Http(_))
            ));
            let candidate = builder.clone().build().unwrap();
            assert!(matches!(
                timeout(
                    Duration::from_secs(2),
                    candidate.login("fixture-user", "fixture-password"),
                )
                .await
                .unwrap(),
                Err(Error::Http(_))
            ));
            // Exercise the authenticated transport without live login or
            // weakening TLS. The proxy rejects before any TLS/application data.
            let authenticated = crate::client::AuthenticatedClient {
                inner: builder.build().unwrap(),
            };
            assert!(matches!(
                timeout(
                    Duration::from_secs(2),
                    authenticated.download_torrent(42_u64),
                )
                .await
                .unwrap(),
                Err(Error::Http(_))
            ));
            timeout(Duration::from_secs(2), proxy)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn unavailable_explicit_proxy_never_connects_directly() {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = format!("https://{}/fixture", origin.local_addr().unwrap());
        let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = unused.local_addr().unwrap();
        drop(unused);
        for scheme in ["http", "socks5h"] {
            let proxy = reqwest::Proxy::all(format!("{scheme}://{address}")).unwrap();
            let client = ClientBuilder::new().proxy(proxy).build().unwrap();
            assert!(
                timeout(
                    Duration::from_secs(2),
                    client.http().get(&destination).send()
                )
                .await
                .unwrap()
                .is_err()
            );
            assert!(
                timeout(Duration::from_millis(50), origin.accept())
                    .await
                    .is_err()
            );
        }
    }
}
