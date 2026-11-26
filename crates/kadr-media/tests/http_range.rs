use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{
    Request, StatusCode,
    body::Incoming,
    client::conn::http1,
    header::{CONTENT_LENGTH, CONTENT_RANGE, HeaderMap},
};
use hyper_util::rt::TokioIo;
use kadr_media::{ByteSource, LocalFile, ReadFuture, Server, ServerOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
    task::JoinHandle,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(3);
const DATA: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

struct Client {
    sender: http1::SendRequest<Empty<Bytes>>,
    task: JoinHandle<()>,
}

impl Client {
    async fn connect(server: &Server) -> Self {
        let socket = TcpStream::connect(server.local_addr()).await.unwrap();
        let (sender, connection) = http1::handshake(TokioIo::new(socket)).await.unwrap();
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        Self { sender, task }
    }

    async fn request(
        &mut self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
    ) -> hyper::Response<Incoming> {
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header("Host", "127.0.0.1");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        timeout(
            DEADLINE,
            self.sender
                .send_request(request.body(Empty::new()).unwrap()),
        )
        .await
        .unwrap()
        .unwrap()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn target(server: &Server) -> String {
    server
        .private_url()
        .parse::<hyper::Uri>()
        .unwrap()
        .path()
        .to_owned()
}

async fn request(
    server: &Server,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Bytes) {
    let mut client = Client::connect(server).await;
    let response = client.request(method, path, headers).await;
    let (parts, body) = response.into_parts();
    let body = timeout(DEADLINE, body.collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    (parts.status, parts.headers, body)
}

// Each test creates only its own bytes; no external media or network is used.
struct Fixture {
    path: PathBuf,
}

impl Fixture {
    async fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kadr-range-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        // create_new prevents overwriting anyone else's fixture on a stale run.
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .unwrap();
        file.write_all(bytes).await.unwrap();
        file.flush().await.unwrap();
        Self { path }
    }

    async fn server(&self) -> Server {
        Server::start(
            Arc::new(LocalFile::open(&self.path).await.unwrap()),
            ServerOptions::default(),
        )
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).unwrap();
    }
}

#[tokio::test]
async fn get_head_and_single_ranges_over_real_sockets() {
    let fixture = Fixture::new(DATA).await;
    let server = fixture.server().await;
    assert!(server.local_addr().ip().is_loopback());
    assert_eq!(server.local_addr().ip().to_string(), "127.0.0.1");
    assert_ne!(server.local_addr().port(), 0);
    let path = target(&server);
    let (status, headers, body) = request(&server, "GET", &path, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_LENGTH], DATA.len().to_string());
    assert_eq!(headers["accept-ranges"], "bytes");
    assert_eq!(headers["cache-control"], "no-store");
    assert!(!headers.contains_key(CONTENT_RANGE));
    assert_eq!(body, DATA);

    // Deliberately seek forwards, then backwards, through one keep-alive socket.
    let mut client = Client::connect(&server).await;
    for (range, start, end) in [
        ("bytes=20-25", 20, 25),
        ("bytes=1-4", 1, 4),
        ("bytes=30-", 30, 35),
        ("bytes=-3", 33, 35),
        ("bytes=0-999999999999999999999999999999", 0, 35),
        ("bytes=-999999999999999999999999999999", 0, 35),
        ("bytes=0-35", 0, 35),
    ] {
        let response = client.request("GET", &path, &[("Range", range)]).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers()[CONTENT_LENGTH],
            (end - start + 1).to_string()
        );
        assert_eq!(
            response.headers()[CONTENT_RANGE],
            format!("bytes {start}-{end}/{}", DATA.len()),
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            &DATA[start..=end],
        );
    }
    for range in ["bytes=1-2", "bytes=999-", "bytes=-0"] {
        let (status, headers, body) = request(&server, "HEAD", &path, &[("Range", range)]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_LENGTH], DATA.len().to_string());
        assert_eq!(headers["accept-ranges"], "bytes");
        assert!(!headers.contains_key(CONTENT_RANGE));
        assert!(body.is_empty());
    }
    drop(client);
    let (status, _, body) = request(&server, "GET", &path, &[("Connection", "close")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, DATA);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsatisfiable_and_ignored_ranges() {
    let fixture = Fixture::new(DATA).await;
    let server = fixture.server().await;
    let path = target(&server);
    for range in [
        "bytes=36-",
        "bytes=100-200",
        "bytes=-0",
        "bytes=999999999999999999999999999999999999-",
    ] {
        let (status, headers, body) = request(&server, "GET", &path, &[("Range", range)]).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers[CONTENT_RANGE], "bytes */36");
        assert_eq!(headers[CONTENT_LENGTH], "0");
        assert!(body.is_empty());
    }
    for range in ["nonsense", "items=1-2", "bytes=5-3", "bytes=0-1,3-4"] {
        let (status, _, body) = request(&server, "GET", &path, &[("Range", range)]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, DATA);
    }
    for validator in [
        "\"no-validator\"",
        "Wed, 21 Oct 2015 07:28:00 GMT",
        "invalid",
    ] {
        let (status, _, body) = request(
            &server,
            "GET",
            &path,
            &[("Range", "bytes=1-2"), ("If-Range", validator)],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, DATA);
    }
    let (status, _, body) = request(
        &server,
        "GET",
        &path,
        &[("Range", "bytes=0-1"), ("Range", "bytes=2-3")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, DATA);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn empty_source_get_head_and_ranges() {
    let fixture = Fixture::new(b"").await;
    let server = fixture.server().await;
    let path = target(&server);
    for method in ["GET", "HEAD"] {
        let (status, headers, body) = request(&server, method, &path, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[CONTENT_LENGTH], "0");
        assert!(body.is_empty());
    }
    for range in ["bytes=0-", "bytes=0-0", "bytes=-1", "bytes=-0"] {
        let (status, headers, body) = request(&server, "GET", &path, &[("Range", range)]).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(headers[CONTENT_RANGE], "bytes */0");
        assert!(body.is_empty());
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_tokens_traversal_queries_and_methods_are_rejected() {
    let fixture = Fixture::new(DATA).await;
    let server = fixture.server().await;
    let other = fixture.server().await;
    let path = target(&server);
    assert_ne!(path, target(&other));
    assert_eq!(path.len(), 65); // 256 bits of OS randomness in hexadecimal.
    for bad_path in [
        "/".to_owned(),
        "/wrong-token".to_owned(),
        target(&other),
        format!("{path}/../Cargo.toml"),
        format!("{path}/%2e%2e/%2e%2e/SPEC.md"),
        format!("/../{path}"),
        format!("/%2e%2e{path}"),
        format!("{path}?path=/etc/passwd"),
        format!("{path}?url=http://example.invalid/"),
        format!("{path}?"),
        format!("http://127.0.0.1{path}"),
    ] {
        let (status, _, body) = request(&server, "GET", &bad_path, &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.is_empty());
    }
    let (status, headers, body) = request(&server, "POST", &path, &[]).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(headers["allow"], "GET, HEAD");
    assert!(body.is_empty());
    other.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_positional_file_reads_do_not_share_seek_state() {
    let bytes: Vec<_> = (0..400_000).map(|index| (index % 251) as u8).collect();
    let fixture = Fixture::new(&bytes).await;
    let server = Arc::new(fixture.server().await);
    let mut requests = tokio::task::JoinSet::new();
    for index in 0..24 {
        let server = server.clone();
        let expected = bytes.clone();
        requests.spawn(async move {
            let start = (index * 7919) % 200_000;
            let end = start + 100_000;
            let (status, headers, body) = request(
                &server,
                "GET",
                &target(&server),
                &[("Range", &format!("bytes={start}-{end}"))],
            )
            .await;
            assert_eq!(status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(headers[CONTENT_LENGTH], "100001");
            assert_eq!(body, expected[start..=end]);
        });
    }
    while let Some(result) = requests.join_next().await {
        result.unwrap();
    }
    Arc::try_unwrap(server)
        .ok()
        .unwrap()
        .shutdown()
        .await
        .unwrap();
}

/// Controllable byte availability, independent from HTTP. watch wakes readers
/// without spinning, and the active-read guard makes cancellation observable.
struct Controlled {
    data: Bytes,
    available: watch::Sender<usize>,
    active: Arc<watch::Sender<usize>>,
}

struct ActiveRead(Arc<watch::Sender<usize>>);

impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.send_modify(|active| *active -= 1);
    }
}

impl Controlled {
    fn new(available: usize) -> Arc<Self> {
        Arc::new(Self {
            data: Bytes::from_static(DATA),
            available: watch::channel(available).0,
            active: Arc::new(watch::channel(0).0),
        })
    }

    async fn wait_active(&self, expected: usize) {
        timeout(
            DEADLINE,
            self.active
                .subscribe()
                .wait_for(|active| *active == expected),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn server(self: &Arc<Self>, read_timeout: Duration) -> Server {
        Server::start(self.clone(), ServerOptions { read_timeout })
            .await
            .unwrap()
    }
}

impl ByteSource for Controlled {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, offset: u64, max_len: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            self.active.send_modify(|active| *active += 1);
            let _guard = ActiveRead(self.active.clone());
            let mut available = self.available.subscribe();
            loop {
                let end = *available.borrow_and_update();
                let start = offset as usize;
                if end > start {
                    return Ok(self.data.slice(start..end.min(start + max_len)));
                }
                available.changed().await.map_err(io::Error::other)?;
            }
        })
    }
}

#[tokio::test]
async fn unavailable_bytes_wake_up_without_partial_success() {
    let source = Controlled::new(0);
    let server = source.server(DEADLINE).await;
    let mut client = Client::connect(&server).await;
    let response = client
        .request("GET", &target(&server), &[("Range", "bytes=5-9")])
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[CONTENT_LENGTH], "5");
    source.wait_active(1).await;
    let mut body = response.into_body();
    // Bytes must not appear before the source makes them available.
    assert!(
        timeout(Duration::from_millis(40), body.frame())
            .await
            .is_err()
    );
    source.available.send_replace(10);
    let body = timeout(DEADLINE, body.collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    assert_eq!(body, &DATA[5..10]);
    source.wait_active(0).await;
    drop(client);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn timeout_after_partial_data_aborts_body_not_successful_eof() {
    let source = Controlled::new(3);
    let server = source.server(Duration::from_millis(120)).await;
    let mut client = Client::connect(&server).await;
    let response = client.request("GET", &target(&server), &[]).await;
    assert_eq!(response.headers()[CONTENT_LENGTH], DATA.len().to_string());
    let mut body = response.into_body();
    let first = timeout(DEADLINE, body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.into_data().unwrap(), &DATA[..3]);
    source.wait_active(1).await;
    let result = timeout(DEADLINE, body.collect()).await.unwrap();
    assert!(
        result.is_err(),
        "a truncated body must not be a successful response"
    );
    source.wait_active(0).await;
    drop(client);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_source_timeout_releases_wait() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_millis(120)).await;
    let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    source.wait_active(0).await;
    // The first-body error can race header flushing, so a client may see
    // either headers plus truncated body or a closed response, never success.
    use tokio::io::AsyncReadExt;
    let mut wire = Vec::new();
    timeout(DEADLINE, socket.read_to_end(&mut wire))
        .await
        .unwrap()
        .unwrap();
    assert!(!wire.ends_with(DATA));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn disconnect_cancels_an_active_availability_wait() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    drop(socket);
    source.wait_active(0).await;
    server.shutdown().await.unwrap();
}

async fn read_response_head(socket: &mut TcpStream) {
    timeout(DEADLINE, async {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            assert!(head.len() < 64 * 1024);
            head.push(socket.read_u8().await.unwrap());
        }
        assert!(head.starts_with(b"HTTP/1.1 200"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn disconnect_with_pipelining_or_unfinished_request_body_cancels_wait() {
    for variant in ["pipelined", "partial head", "unfinished body"] {
        let source = Controlled::new(0);
        let server = source.server(Duration::from_secs(60)).await;
        let path = target(&server);
        let wire = match variant {
            "pipelined" => format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n\
                 HEAD {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            ),
            "partial head" => format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\nG"),
            _ => format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 10\r\n\r\nx"),
        };
        let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
        socket.write_all(wire.as_bytes()).await.unwrap();
        source.wait_active(1).await;
        // Consume the headers so dropping the socket is an ordinary FIN, not
        // a reset caused by discarding unread response bytes.
        read_response_head(&mut socket).await;
        drop(socket);
        source.wait_active(0).await;
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn extra_request_byte_does_not_cancel_a_live_client() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\nG",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    read_response_head(&mut socket).await;
    source.available.send_replace(DATA.len());
    let mut body = vec![0; DATA.len()];
    timeout(DEADLINE, socket.read_exact(&mut body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body, DATA);
    source.wait_active(0).await;
    drop(socket);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn disconnect_and_tcp_half_close_after_partial_response_cancel_wait() {
    for half_close in [false, true] {
        let source = Controlled::new(3);
        let server = source.server(Duration::from_secs(60)).await;
        let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
        socket
            .write_all(
                format!(
                    "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                    target(&server)
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        read_response_head(&mut socket).await;
        let mut first = [0; 3];
        timeout(DEADLINE, socket.read_exact(&mut first))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&first, &DATA[..3]);
        source.wait_active(1).await;
        if half_close {
            // This spike intentionally treats client write-half EOF as a
            // disconnect too; a truncated Content-Length remains an error.
            socket.shutdown().await.unwrap();
            source.wait_active(0).await;
            let mut rest = Vec::new();
            timeout(DEADLINE, socket.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
            assert!(rest.is_empty());
        } else {
            drop(socket);
            source.wait_active(0).await;
        }
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn input_overflow_closes_connection_instead_of_stalling_eof_monitor() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let mut socket = TcpStream::connect(server.local_addr()).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    read_response_head(&mut socket).await;
    let _ = timeout(DEADLINE, socket.write_all(&vec![b'x'; 1024 * 1024]))
        .await
        .unwrap();
    source.wait_active(0).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_server_requests_cleanup_of_pending_waits() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let address = server.local_addr();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    drop(server);
    source.wait_active(0).await;
    assert!(TcpStream::connect(address).await.is_err());
}

struct BrokenSource(u8);

impl ByteSource for BrokenSource {
    fn len(&self) -> u64 {
        5
    }

    fn read_at(&self, _: u64, _: usize) -> ReadFuture<'_> {
        Box::pin(async move {
            match self.0 {
                0 => Ok(Bytes::new()),
                1 => Ok(Bytes::from_static(b"123456")),
                _ => Err(io::Error::other("source failure")),
            }
        })
    }
}

#[tokio::test]
async fn invalid_source_reads_and_errors_never_finish_successfully() {
    for variant in 0..3 {
        let server = Server::start(Arc::new(BrokenSource(variant)), ServerOptions::default())
            .await
            .unwrap();
        let mut client = Client::connect(&server).await;
        let request = Request::builder()
            .uri(target(&server))
            .header("Host", "localhost")
            .body(Empty::new())
            .unwrap();
        // Hyper may detect the body failure before flushing the response head.
        if let Ok(response) = timeout(DEADLINE, client.sender.send_request(request))
            .await
            .unwrap()
        {
            assert_eq!(response.headers()[CONTENT_LENGTH], "5");
            assert!(
                timeout(DEADLINE, response.into_body().collect())
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        drop(client);
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shutdown_cancels_waits_joins_connections_and_releases_port() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let address = server.local_addr();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: localhost\r\n\r\n",
                target(&server)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    source.wait_active(1).await;
    timeout(DEADLINE, server.shutdown()).await.unwrap().unwrap();
    source.wait_active(0).await;
    assert!(TcpStream::connect(address).await.is_err());
    use tokio::io::AsyncReadExt;
    let mut wire = Vec::new();
    timeout(DEADLINE, socket.read_to_end(&mut wire))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn head_does_not_wait_for_unavailable_data() {
    let source = Controlled::new(0);
    let server = source.server(Duration::from_secs(60)).await;
    let (status, headers, body) =
        request(&server, "HEAD", &target(&server), &[("Range", "bytes=1-2")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[CONTENT_LENGTH], DATA.len().to_string());
    assert!(body.is_empty());
    source.wait_active(0).await;
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_truncation_aborts_response_instead_of_padding_or_success() {
    for surviving in [0, 3] {
        let fixture = Fixture::new(DATA).await;
        let source = LocalFile::open(&fixture.path).await.unwrap();
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.path)
            .await
            .unwrap()
            .set_len(surviving as u64)
            .await
            .unwrap();
        let server = Server::start(Arc::new(source), ServerOptions::default())
            .await
            .unwrap();
        let mut client = Client::connect(&server).await;
        let request = Request::builder()
            .method("GET")
            .uri(target(&server))
            .header("Host", "127.0.0.1")
            .body(Empty::new())
            .unwrap();
        // Hyper may poll the pre-EOF source error before flushing headers.
        // Bypass the strict success helper only here: incomplete headers or
        // headers followed by an errored body are both legal, never completion.
        match timeout(DEADLINE, client.sender.send_request(request))
            .await
            .unwrap()
        {
            Err(error) => assert!(
                error.is_incomplete_message(),
                "unexpected truncated-response error: {error}"
            ),
            Ok(response) => {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()[CONTENT_LENGTH], DATA.len().to_string());
                let mut body = response.into_body();
                timeout(DEADLINE, async {
                    let mut received = Vec::new();
                    loop {
                        match body.frame().await {
                            Some(Ok(frame)) => {
                                received.extend_from_slice(&frame.into_data().unwrap());
                                assert!(
                                    received.len() <= surviving,
                                    "a truncated file must not be padded"
                                );
                                assert_eq!(received, DATA[..received.len()]);
                            }
                            Some(Err(_)) => break,
                            None => panic!("a truncated file must not complete successfully"),
                        }
                    }
                })
                .await
                .unwrap();
            }
        }
        drop(client);
        server.shutdown().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn fifo_is_rejected_without_waiting_for_a_writer() {
    use std::{
        ffi::CString,
        os::unix::ffi::OsStrExt,
        process::{Command, Stdio},
        time::Instant,
    };

    const PROBE: &str = "KADR_RANGE_FIFO_PROBE";
    if let Some(path) = std::env::var_os(PROBE) {
        match LocalFile::open(path).await {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("a FIFO must not be accepted as a regular file"),
        }
        return;
    }

    let fixture = Fixture::new(b"").await;
    tokio::fs::remove_file(&fixture.path).await.unwrap();
    let path = CString::new(fixture.path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is a valid NUL-terminated string; the path belongs to this test.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    // A regression would block OS open and Tokio runtime shutdown, not just the
    // read future. Probe in a child so the timeout can actually clean it up.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fifo_is_rejected_without_waiting_for_a_writer"])
        .env(PROBE, &fixture.path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "FIFO rejection probe failed: {status}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("opening a FIFO must not wait for a writer");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_the_path_does_not_switch_the_authorized_file() {
    let fixture = Fixture::new(DATA).await;
    let replacement = Fixture::new(b"not authorized").await;
    let server = fixture.server().await;
    tokio::fs::rename(&replacement.path, &fixture.path)
        .await
        .unwrap();
    // The replacement fixture's name was moved; only clean up its remaining path.
    std::mem::forget(replacement);
    let (status, _, body) = request(&server, "GET", &target(&server), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, DATA);
    server.shutdown().await.unwrap();
}
