//! Real loopback sockets only: no external network is needed by these tests.
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use kadr_media::{ByteSource, RemoteSource, Server, ServerOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const DATA: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const ETAG: &str = "\"version-one\"";
const DATE: &str = "Wed, 21 Oct 2015 07:28:00 GMT";

struct Reply {
    wire: Vec<u8>,
    delay: Duration,
    after_write: Duration,
}

fn reply(status: &str, headers: &str, body: &[u8]) -> Reply {
    let mut wire = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    wire.extend_from_slice(body);
    Reply {
        wire,
        delay: Duration::ZERO,
        after_write: Duration::ZERO,
    }
}

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Mock {
    async fn new(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/movie?token=never-print-me",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    socket = listener.accept() => {
                        let (mut socket, _) = socket.unwrap();
                        let requests = recorded.clone();
                        let handler = handler.clone();
                        connections.spawn(async move {
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let byte = socket.read_u8().await.unwrap();
                                request.push(byte);
                                assert!(request.len() < 32 * 1024);
                            }
                            let request = String::from_utf8(request).unwrap();
                            requests.lock().unwrap().push(request.clone());
                            let response = handler(&request);
                            tokio::time::sleep(response.delay).await;
                            let _ = socket.write_all(&response.wire).await;
                            tokio::time::sleep(response.after_write).await;
                            let _ = socket.shutdown().await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }

    async fn source(&self) -> RemoteSource {
        RemoteSource::open(&self.url, CancellationToken::new())
            .await
            .unwrap()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn header<'a>(request: &'a str, key: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(key).then_some(value.trim())
    })
}

fn head(length: usize) -> Reply {
    reply(
        "200 OK",
        &format!(
            "Content-Length: {length}\r\nETag: {ETAG}\r\nContent-Type: Video/MP4; charset=binary\r\n"
        ),
        b"",
    )
}

fn range_response(request: &str, data: &[u8]) -> Reply {
    assert_eq!(header(request, "accept-encoding"), Some("identity"));
    let range = header(request, "range")
        .unwrap()
        .strip_prefix("bytes=")
        .unwrap();
    let (start, end) = range.split_once('-').unwrap();
    let (start, end): (usize, usize) = (start.parse().unwrap(), end.parse().unwrap());
    reply(
        "206 Partial Content",
        &format!(
            "Content-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: {ETAG}\r\n",
            end - start + 1,
            data.len()
        ),
        &data[start..=end],
    )
}

#[tokio::test]
async fn head_length_positional_reads_eof_mime_and_redaction() {
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(DATA.len())
        } else {
            assert_eq!(header(request, "if-match"), Some(ETAG));
            range_response(request, DATA)
        }
    })
    .await;
    let source = RemoteSource::open(format!("{}#discarded", mock.url), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(source.len(), DATA.len() as u64);
    assert_eq!(source.content_type(), Some("video/mp4"));
    let debug = format!("{source:?}");
    assert!(!debug.contains("token"));
    assert!(!debug.contains("127.0.0.1"));
    assert_eq!(source.read_at(10, 5).await.unwrap(), &DATA[10..15]);
    assert_eq!(source.read_at(34, 100).await.unwrap(), &DATA[34..]);
    assert!(source.read_at(36, 10).await.unwrap().is_empty());
    assert!(
        source
            .read_at(u64::MAX, usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        source.read_at(0, 0).await.unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(
        mock.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| !r.contains("discarded"))
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn remote_source_streams_through_existing_loopback_server() {
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(DATA.len())
        } else {
            range_response(request, DATA)
        }
    })
    .await;
    let source = Arc::new(mock.source().await);
    let server = Server::start(source, ServerOptions::default())
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let response = client
        .get(server.private_url())
        .header("Range", "bytes=10-14")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.bytes().await.unwrap(), &DATA[10..15]);
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
    assert_eq!(
        header(&mock.requests.lock().unwrap()[1], "range"),
        Some("bytes=10-14")
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn reads_are_bounded_and_concurrent() {
    let data = Arc::new(vec![0x5a; 200_000]);
    let mock = Mock::new(move |request| {
        if request.starts_with("HEAD ") {
            head(data.len())
        } else {
            range_response(request, &data)
        }
    })
    .await;
    let source = mock.source().await;
    let (a, b) = tokio::join!(
        source.read_at(0, usize::MAX),
        source.read_at(100_000, 70_000)
    );
    assert_eq!(a.unwrap().len(), 65_536);
    assert_eq!(b.unwrap().len(), 65_536);
}

#[tokio::test]
async fn unsupported_head_and_missing_head_length_fall_back_to_probe() {
    for mode in 0..3 {
        let mock = Mock::new(move |request| {
            if request.starts_with("HEAD ") {
                match mode {
                    0 => reply("405 Method Not Allowed", "Content-Length: 0\r\n", b""),
                    1 => reply("501 Not Implemented", "Content-Length: 0\r\n", b""),
                    _ => reply("200 OK", "", b""),
                }
            } else {
                range_response(request, DATA)
            }
        })
        .await;
        let source = mock.source().await;
        assert_eq!(source.len(), DATA.len() as u64);
        assert_eq!(source.read_at(5, 4).await.unwrap(), &DATA[5..9]);
        assert_eq!(
            header(&mock.requests.lock().unwrap()[1], "range"),
            Some("bytes=0-0")
        );
    }
}

#[tokio::test]
async fn fallback_probes_reject_bad_ranges_and_do_not_consume_ignored_bodies() {
    for variant in 0..6 {
        let mock = Mock::new(move |request| {
            if request.starts_with("HEAD ") {
                reply("405 Not Allowed", "Content-Length: 0\r\n", b"")
            } else {
                let (headers, body): (&str, &[u8]) = match variant {
                    0 => ("Content-Range: bytes 1-1/36\r\nContent-Length: 1\r\n", b"1"),
                    1 => ("Content-Range: bytes 0-0/*\r\nContent-Length: 1\r\n", b"0"),
                    2 => ("Content-Range: bytes 0-0/36\r\nContent-Length: 1\r\n", b""),
                    3 => ("Content-Range: bytes 0-0/36\r\n", b"01"),
                    4 => ("Content-Range: bytes 0-0/0\r\nContent-Length: 1\r\n", b"0"),
                    _ => ("Content-Range: bytes 0-0/36\r\nContent-Length: 1\r\nContent-Type: Multipart/Byteranges; boundary=x\r\n", b"0"),
                };
                reply("206 Partial Content", &format!("{headers}ETag: {ETAG}\r\n"), body)
            }
        }).await;
        assert!(
            RemoteSource::open(&mock.url, CancellationToken::new())
                .await
                .is_err()
        );
    }
    // Keep a million-byte body pending. Opening must return after headers.
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            reply("405 Not Allowed", "Content-Length: 0\r\n", b"")
        } else {
            let mut response = reply(
                "200 OK",
                &format!("Content-Length: 1000000\r\nETag: {ETAG}\r\n"),
                b"",
            );
            response.after_write = Duration::from_secs(60);
            response
        }
    })
    .await;
    assert_eq!(
        timeout(Duration::from_secs(2), mock.source())
            .await
            .unwrap()
            .len(),
        1_000_000
    );
}

#[tokio::test]
async fn empty_resources_need_no_validator() {
    for fallback in [false, true] {
        let mock = Mock::new(move |request| {
            if !fallback {
                reply("200 OK", "Content-Length: 0\r\n", b"")
            } else if request.starts_with("HEAD ") {
                reply("405 Not Allowed", "Content-Length: 0\r\n", b"")
            } else {
                reply(
                    "416 Range Not Satisfiable",
                    "Content-Range: bytes */0\r\nContent-Length: 0\r\n",
                    b"",
                )
            }
        })
        .await;
        let source = mock.source().await;
        assert!(source.is_empty());
        assert!(source.read_at(0, 100).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn range_ignoring_server_only_allows_exact_entire_bounded_read() {
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(DATA.len())
        } else {
            reply(
                "200 OK",
                &format!("Content-Length: {}\r\nETag: {ETAG}\r\n", DATA.len()),
                DATA,
            )
        }
    })
    .await;
    let source = mock.source().await;
    assert_eq!(source.read_at(0, 100).await.unwrap(), DATA);
    assert_eq!(
        source.read_at(1, 100).await.unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        source.read_at(0, 4).await.unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );

    let large = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(1_000_000)
        } else {
            let mut response = reply(
                "200 OK",
                &format!("Content-Length: 1000000\r\nETag: {ETAG}\r\n"),
                b"",
            );
            response.after_write = Duration::from_secs(60);
            response
        }
    })
    .await;
    let source = large.source().await;
    assert_eq!(
        timeout(Duration::from_secs(2), source.read_at(0, usize::MAX))
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
}

#[tokio::test]
async fn last_modified_conditions_reads_and_weak_etag_alone_is_insufficient() {
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            reply(
                "200 OK",
                &format!("Content-Length: 36\r\nLast-Modified: {DATE}\r\n"),
                b"",
            )
        } else {
            assert_eq!(header(request, "if-unmodified-since"), Some(DATE));
            reply(
                "206 Partial Content",
                &format!(
                    "Content-Length: 2\r\nContent-Range: bytes 2-3/36\r\nLast-Modified: {DATE}\r\n"
                ),
                b"23",
            )
        }
    })
    .await;
    assert_eq!(
        mock.source().await.read_at(2, 2).await.unwrap(),
        b"23".as_slice()
    );
    for validator in ["", "ETag: W/\"weak\"\r\n"] {
        let mock =
            Mock::new(move |_| reply("200 OK", &format!("Content-Length: 36\r\n{validator}"), b""))
                .await;
        assert_eq!(
            RemoteSource::open(&mock.url, CancellationToken::new())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}

#[tokio::test]
async fn changed_validators_length_or_precondition_fail_stickily() {
    for variant in 0..5 {
        let mock = Mock::new(move |request| {
            if request.starts_with("HEAD ") {
                head(36)
            } else {
                match variant {
                    0 => reply(
                        "206 Partial Content",
                        "Content-Length: 2\r\nContent-Range: bytes 0-1/36\r\nETag: \"changed\"\r\n",
                        b"01",
                    ),
                    1 => reply(
                        "206 Partial Content",
                        &format!(
                            "Content-Length: 2\r\nContent-Range: bytes 0-1/37\r\nETag: {ETAG}\r\n"
                        ),
                        b"01",
                    ),
                    2 => reply("412 Precondition Failed", "Content-Length: 0\r\n", b""),
                    3 => reply(
                        "206 Partial Content",
                        "Content-Length: 2\r\nContent-Range: bytes 0-1/36\r\n",
                        b"01",
                    ),
                    _ => reply(
                        "200 OK",
                        &format!("Content-Length: 37\r\nETag: {ETAG}\r\n"),
                        b"",
                    ),
                }
            }
        })
        .await;
        let source = mock.source().await;
        assert_eq!(
            source.read_at(0, 2).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(source.read_at(2, 2).await.is_err());
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
    }
}

#[tokio::test]
async fn malformed_range_encoding_and_short_or_excess_body_fail() {
    for variant in 0..9 {
        let mock = Mock::new(move |request| {
            if request.starts_with("HEAD ") { head(36) }
            else {
                let extra = match variant {
                    0 => "Content-Range: bytes 1-2/36\r\nContent-Length: 2\r\n",
                    1 => "Content-Range: bytes 0-1/*\r\nContent-Length: 2\r\n",
                    2 => "Content-Range: bytes 0-1/36\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n",
                    3 => "Content-Range: bytes 0-1/36\r\nContent-Length: 2\r\n",
                    4 => "Content-Range: bytes 0-1/36\r\n",
                    5 => "Content-Range: bytes 0-1/36\r\nContent-Length: 3\r\n",
                    6 => "Content-Length: 2\r\n",
                    7 => "Content-Range: bytes 0-1/36\r\nContent-Range: bytes 0-1/36\r\nContent-Length: 2\r\n",
                    _ => "Content-Range: bytes 0-1/36\r\nContent-Length: 2\r\nContent-Type: multipart/byteranges; boundary=x\r\n",
                };
                let body: &[u8] = match variant { 3 => b"0", 4 | 5 => b"012", _ => b"01" };
                reply("206 Partial Content", &format!("{extra}ETag: {ETAG}\r\n"), body)
            }
        }).await;
        assert!(
            mock.source().await.read_at(0, 2).await.is_err(),
            "variant {variant}"
        );
    }
}

#[tokio::test]
async fn open_rejects_invalid_metadata_without_reading_unbounded_bodies() {
    for headers in [
        "Content-Length: 36\r\nContent-Encoding: gzip\r\n",
        "Content-Length: 18446744073709551616\r\n",
        "Content-Length: 36\r\nETag: invalid\r\n",
        "Content-Length: 36\r\nLast-Modified: invalid\r\n",
    ] {
        let mock = Mock::new(move |_| reply("200 OK", headers, b"")).await;
        assert!(
            RemoteSource::open(&mock.url, CancellationToken::new())
                .await
                .is_err()
        );
    }
    let huge_headers = Mock::new(|_| {
        reply(
            "200 OK",
            &format!(
                "Content-Length: 36\r\nETag: {ETAG}\r\nX-Padding: {}\r\n",
                "x".repeat(17_000)
            ),
            b"",
        )
    })
    .await;
    assert!(
        RemoteSource::open(&huge_headers.url, CancellationToken::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn chunked_ranges_validate_decoded_body_length() {
    for (wire, succeeds) in [
        ("1\r\n0\r\n1\r\n1\r\n0\r\n\r\n", true),
        ("2\r\n0", false),
        ("2\r\n01\r\n1\r\n2\r\n0\r\n\r\n", false),
    ] {
        let mock = Mock::new(move |request| {
            if request.starts_with("HEAD ") {
                head(36)
            } else {
                reply(
                    "206 Partial Content",
                    &format!("Transfer-Encoding: chunked\r\nContent-Range: bytes 0-1/36\r\nETag: {ETAG}\r\n"),
                    wire.as_bytes(),
                )
            }
        }).await;
        let result = mock.source().await.read_at(0, 2).await;
        if succeeds {
            assert_eq!(result.unwrap(), b"01".as_slice());
        } else {
            assert!(result.is_err());
        }
    }
}

#[tokio::test]
async fn invalid_mime_is_not_exposed_and_modified_date_changes_are_sticky() {
    for mime in ["not-a-mime", "video/mp4/extra", "video/has space"] {
        let mock = Mock::new(move |_| {
            reply(
                "200 OK",
                &format!("Content-Length: 36\r\nETag: {ETAG}\r\nContent-Type: {mime}\r\n"),
                b"",
            )
        })
        .await;
        assert_eq!(mock.source().await.content_type(), None);
    }
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            reply("200 OK", &format!("Content-Length: 36\r\nLast-Modified: {DATE}\r\n"), b"")
        } else {
            reply("206 Partial Content", "Content-Length: 2\r\nContent-Range: bytes 0-1/36\r\nLast-Modified: Thu, 22 Oct 2015 07:28:00 GMT\r\n", b"01")
        }
    }).await;
    let source = mock.source().await;
    assert!(source.read_at(0, 2).await.is_err());
    assert!(source.read_at(2, 2).await.is_err());
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn redirects_are_validated_and_bounded() {
    let redirects = Arc::new(AtomicUsize::new(0));
    let count = redirects.clone();
    let mock = Mock::new(move |request| {
        if request.starts_with("HEAD /movie?") {
            count.fetch_add(1, Ordering::Relaxed);
            reply(
                "302 Found",
                "Location: /final#discarded\r\nContent-Length: 0\r\n",
                b"",
            )
        } else if request.starts_with("HEAD ") {
            head(DATA.len())
        } else {
            range_response(request, DATA)
        }
    })
    .await;
    let source = mock.source().await;
    assert_eq!(source.read_at(3, 3).await.unwrap(), b"345".as_slice());
    assert_eq!(redirects.load(Ordering::Relaxed), 1);
    assert!(mock.requests.lock().unwrap()[2].starts_with("GET /final "));

    for location in [
        "file:///etc/passwd",
        "http://user:secret@127.0.0.1/",
        "http://@127.0.0.1/",
        "//@127.0.0.1/",
        "http:repaired",
        "/bad%xx",
        "http:///bad",
    ] {
        let mock = Mock::new(move |_| {
            reply(
                "302 Found",
                &format!("Location: {location}\r\nContent-Length: 0\r\n"),
                b"",
            )
        })
        .await;
        let error = RemoteSource::open(&mock.url, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(mock.requests.lock().unwrap().len(), 1, "{location}");
    }
    let cycle = Mock::new(|_| {
        reply(
            "302 Found",
            "Location: /again\r\nContent-Length: 0\r\n",
            b"",
        )
    })
    .await;
    assert!(
        RemoteSource::open(&cycle.url, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(cycle.requests.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn redirect_budget_is_shared_by_head_and_probe_and_read_redirects_invalidate() {
    let mock = Mock::new(|request| {
        let path = request.split_whitespace().nth(1).unwrap();
        if request.starts_with("HEAD ") {
            if path == "/three" {
                reply("405 Not Allowed", "Content-Length: 0\r\n", b"")
            } else {
                let next = match path {
                    "/one" => "/two",
                    "/two" => "/three",
                    _ => "/one",
                };
                reply(
                    "302 Found",
                    &format!("Location: {next}\r\nContent-Length: 0\r\n"),
                    b"",
                )
            }
        } else {
            reply(
                "302 Found",
                "Location: /get-cycle\r\nContent-Length: 0\r\n",
                b"",
            )
        }
    })
    .await;
    assert!(
        RemoteSource::open(&mock.url, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 7); // 3 HEAD + 2 GET redirects, 2 terminal requests

    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(36)
        } else {
            reply(
                "302 Found",
                "Location: /changed\r\nContent-Length: 0\r\n",
                b"",
            )
        }
    })
    .await;
    let source = mock.source().await;
    assert!(source.read_at(0, 2).await.is_err());
    assert!(source.read_at(2, 2).await.is_err());
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn lexical_validation_rejects_credentials_and_non_http_without_network() {
    for url in [
        "file:///etc/passwd",
        "magnet:?xt=urn:btih:0123456789012345678901234567890123456789",
        "http://user:private@127.0.0.1/",
        "http://@127.0.0.1/",
        "http:localhost",
        "http:///localhost",
        "http://localhost/%xx",
        " http://localhost/",
        "http://localhost/a\\b",
    ] {
        let error = RemoteSource::open(url, CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!format!("{error:?}").contains("private"));
    }
}

#[tokio::test]
async fn exactly_five_redirects_succeed() {
    let mock = Mock::new(|request| {
        let step = request
            .split_whitespace()
            .nth(1)
            .unwrap()
            .trim_start_matches('/')
            .parse::<u8>()
            .unwrap_or(0);
        if step == 5 {
            head(36)
        } else {
            reply(
                "302 Found",
                &format!("Location: /{}\r\nContent-Length: 0\r\n", step + 1),
                b"",
            )
        }
    })
    .await;
    assert_eq!(mock.source().await.len(), 36);
    assert_eq!(mock.requests.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn cancellation_interrupts_open_and_all_concurrent_reads() {
    let opening = Mock::new(|_| {
        let mut response = head(36);
        response.delay = Duration::from_secs(60);
        response
    })
    .await;
    let cancel = CancellationToken::new();
    let pending = RemoteSource::open(&opening.url, cancel.clone());
    let cancelling = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
    };
    let (result, _) = tokio::join!(pending, cancelling);
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);

    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(36)
        } else {
            let mut response = range_response(request, DATA);
            response.delay = Duration::from_secs(60);
            response
        }
    })
    .await;
    let cancel = CancellationToken::new();
    let source = RemoteSource::open(&mock.url, cancel.clone()).await.unwrap();
    let cancelling = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
    };
    let (a, b, _) = timeout(Duration::from_secs(3), async {
        tokio::join!(source.read_at(0, 2), source.read_at(10, 2), cancelling)
    })
    .await
    .unwrap();
    assert_eq!(a.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(b.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(
        source.read_at(0, 2).await.unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[tokio::test]
async fn cancellation_interrupts_body_consumption_after_headers() {
    let mock = Mock::new(|request| {
        if request.starts_with("HEAD ") {
            head(36)
        } else {
            let mut response = reply(
                "206 Partial Content",
                &format!("Content-Length: 2\r\nContent-Range: bytes 0-1/36\r\nETag: {ETAG}\r\n"),
                b"0",
            );
            response.after_write = Duration::from_secs(60);
            response
        }
    })
    .await;
    let cancel = CancellationToken::new();
    let source = RemoteSource::open(&mock.url, cancel.clone()).await.unwrap();
    let cancelling = async {
        // Wait for GET, then let headers and the first byte reach read_body.
        while mock.requests.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
    };
    let (result, _) = timeout(Duration::from_secs(3), async {
        tokio::join!(source.read_at(0, 2), cancelling)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
}

#[tokio::test]
async fn dropping_a_read_closes_the_partial_upstream_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/movie", listener.local_addr().unwrap());
    let (sent_headers, headers_sent) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut head_socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(head_socket.read_u8().await.unwrap());
        }
        head_socket.write_all(&head(36).wire).await.unwrap();
        head_socket.shutdown().await.unwrap();
        drop(head_socket);

        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let response = reply(
            "206 Partial Content",
            &format!("Content-Length: 2\r\nContent-Range: bytes 0-1/36\r\nETag: {ETAG}\r\n"),
            b"0",
        );
        socket.write_all(&response.wire).await.unwrap();
        sent_headers.send(()).unwrap();
        let mut byte = [0];
        let closed = socket.read(&mut byte).await;
        assert!(matches!(closed, Ok(0)) || closed.is_err());
    });
    let source = Arc::new(
        RemoteSource::open(url, CancellationToken::new())
            .await
            .unwrap(),
    );
    let reader = source.clone();
    let read = tokio::spawn(async move { reader.read_at(0, 2).await });
    timeout(Duration::from_secs(2), headers_sent)
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    read.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(2), upstream)
        .await
        .unwrap()
        .unwrap();
}
