use super::*;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Reject the tunnel before TLS: no origin connection, credentials or TLS bypass.
struct RejectingProxy {
    endpoint: String,
    targets: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RejectingProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RejectingProxy {
    async fn start(socks: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let targets = Arc::new(Mutex::new(Vec::new()));
        let received = targets.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let target = if socks {
                    let mut greeting = [0; 3];
                    stream.read_exact(&mut greeting).await.unwrap();
                    assert_eq!(greeting, [5, 1, 0]); // SOCKS5, no authentication.
                    stream.write_all(&[5, 0]).await.unwrap();
                    let mut request = [0; 5];
                    stream.read_exact(&mut request).await.unwrap();
                    assert_eq!(&request[..4], &[5, 1, 0, 3]); // Remote DNS.
                    let mut host = vec![0; request[4] as usize];
                    stream.read_exact(&mut host).await.unwrap();
                    let port = stream.read_u16().await.unwrap();
                    let target = format!("{}:{port}", String::from_utf8(host).unwrap());
                    received.lock().unwrap().push(target.clone());
                    stream
                        .write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0])
                        .await
                        .unwrap();
                    target
                } else {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                        assert!(request.len() <= 8192);
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
                    let line = request.lines().next().unwrap();
                    let target = line
                        .strip_prefix("CONNECT ")
                        .unwrap()
                        .strip_suffix(" HTTP/1.1")
                        .unwrap()
                        .to_owned();
                    received.lock().unwrap().push(target.clone());
                    stream
                        .write_all(b"HTTP/1.1 502 Fixture\r\nContent-Length: 0\r\n\r\n")
                        .await
                        .unwrap();
                    target
                };
                assert!(!target.is_empty());
            }
        });
        Self {
            endpoint: format!("{}://{address}", if socks { "socks5h" } else { "http" }),
            targets,
            task,
        }
    }

    fn targets(&self) -> Vec<String> {
        self.targets.lock().unwrap().clone()
    }
}

#[test]
fn proxy_construction_is_offline_and_invalid_endpoints_are_redacted() {
    assert!(Client::new(Options::default()).is_ok());
    assert!(Client::new_with_proxy(Options::default(), None).is_ok());
    assert!(rutracker::Client::new().is_ok());
    assert!(rutracker::Client::new_with_proxy(None).is_ok());
    for endpoint in [
        "http://127.0.0.1:9",
        "https://127.0.0.1:9",
        "socks5://127.0.0.1:9",
        "socks5h://127.0.0.1:9",
    ] {
        assert!(Client::new_with_proxy(Options::default(), Some(endpoint)).is_ok());
        assert!(rutracker::Client::new_with_proxy(Some(endpoint)).is_ok());
    }
    for endpoint in [
        "",
        "127.0.0.1:8080",
        "http://fixture-user:fixture-password@127.0.0.1:8080",
        "http://127.0.0.1:8080/private",
        "http://127.0.0.1:8080/?private",
        "http://127.0.0.1:8080/#private",
        "socks4://127.0.0.1:8080",
    ] {
        let trakt = Client::new_with_proxy(Options::default(), Some(endpoint));
        assert!(matches!(trakt, Err(Error::Network)));
        let rutracker = rutracker::Client::new_with_proxy(Some(endpoint));
        assert!(matches!(rutracker, Err(rutracker::Error::Network)));
    }
}

#[tokio::test]
async fn trakt_uses_explicit_connect_and_remote_dns_socks_without_fallback() {
    for socks in [false, true] {
        let proxy = RejectingProxy::start(socks).await;
        let mut client = Client::new_with_proxy(Options::default(), Some(&proxy.endpoint)).unwrap();
        client.client_id = client_id("offline-fixture");
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.details("trakt:movie:42", &CancellationToken::new()),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(Error::Network));
        assert_eq!(proxy.targets(), ["api.trakt.tv:443"]);
        let debug = format!("{client:?}");
        assert!(!debug.contains(&proxy.endpoint));
        assert!(!debug.contains("offline-fixture"));
    }
}

#[tokio::test]
async fn rutracker_anonymous_and_fresh_login_sessions_keep_the_same_proxy() {
    for socks in [false, true] {
        let proxy = RejectingProxy::start(socks).await;
        let mut client = rutracker::Client::new_with_proxy(Some(&proxy.endpoint)).unwrap();
        let cancel = CancellationToken::new();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.magnet(42, &cancel))
                .await
                .unwrap(),
            Err(rutracker::Error::Network)
        );
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(2),
                client.login("fixture-user", "fixture-password", &cancel),
            )
            .await
            .unwrap(),
            Err(rutracker::Error::Network)
        );
        assert_eq!(
            client.download(42, &cancel).await,
            Err(rutracker::Error::AuthenticationRequired)
        );
        client.logout();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), client.magnet(42, &cancel))
                .await
                .unwrap(),
            Err(rutracker::Error::Network)
        );
        assert_eq!(proxy.targets(), ["rutracker.org:443"; 3]);
    }
}

#[tokio::test]
async fn cancellation_prevents_proxy_connections() {
    let proxy = RejectingProxy::start(false).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut trakt = Client::new_with_proxy(Options::default(), Some(&proxy.endpoint)).unwrap();
    trakt.client_id = client_id("offline-fixture");
    assert_eq!(
        trakt.details("trakt:movie:42", &cancel).await,
        Err(Error::Cancelled)
    );
    let mut rutracker = rutracker::Client::new_with_proxy(Some(&proxy.endpoint)).unwrap();
    assert_eq!(
        rutracker.magnet(42, &cancel).await,
        Err(rutracker::Error::Cancelled)
    );
    assert_eq!(
        rutracker
            .login("fixture-user", "fixture-password", &cancel)
            .await,
        Err(rutracker::Error::Cancelled)
    );
    assert!(proxy.targets().is_empty());
}

#[tokio::test]
async fn unavailable_explicit_proxy_never_connects_directly() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = format!("https://{}/fixture", origin.local_addr().unwrap());
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = unused.local_addr().unwrap();
    drop(unused);
    for scheme in ["http", "socks5h"] {
        let endpoint = format!("{scheme}://{address}");
        let client = Client::new_with_proxy(Options::default(), Some(&endpoint)).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), client.http.get(&destination).send(),)
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), origin.accept())
                .await
                .is_err()
        );
    }
}
