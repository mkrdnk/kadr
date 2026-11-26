//! Sequential HTTP downloads over real loopback sockets, never external services.
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use kadr_media::HttpDownload;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const DATA: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const WAIT: Duration = Duration::from_secs(3);
const CHUNK_LIMIT: usize = 64 * 1024;

struct Reply {
    wire: Vec<u8>,
    before_headers: Option<Arc<Notify>>,
    tail: Option<(Arc<Notify>, Vec<u8>)>,
    hold_open: bool,
}

fn reply(status: &str, headers: &str, body: &[u8]) -> Reply {
    let mut wire = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    wire.extend_from_slice(body);
    Reply {
        wire,
        before_headers: None,
        tail: None,
        hold_open: false,
    }
}

fn full(body: &[u8]) -> Reply {
    reply(
        "200 OK",
        &format!("Content-Length: {}\r\n", body.len()),
        body,
    )
}

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    requested: Arc<Notify>,
    closed: Arc<Notify>,
    task: JoinHandle<()>,
}

// Gates race against peer closure so cancellation tests can observe the socket
// being released without first allowing the server to finish its response.
async fn gate_or_close(socket: &mut TcpStream, gate: &Notify) -> bool {
    let mut byte = [0];
    tokio::select! {
        _ = gate.notified() => true,
        result = socket.read(&mut byte) => {
            assert!(matches!(result, Ok(0)) || result.is_err());
            false
        }
    }
}

impl Mock {
    async fn new(handler: impl Fn(&str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/private-movie?token=never-print-me",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requested = Arc::new(Notify::new());
        let closed = Arc::new(Notify::new());
        let recorded = requests.clone();
        let request_signal = requested.clone();
        let close_signal = closed.clone();
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let recorded = recorded.clone();
                        let requested = request_signal.clone();
                        let closed = close_signal.clone();
                        let handler = handler.clone();
                        connections.spawn(async move {
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                request.push(socket.read_u8().await.unwrap());
                                assert!(request.len() < 32 * 1024);
                            }
                            let request = String::from_utf8(request).unwrap();
                            recorded.lock().unwrap().push(request.clone());
                            requested.notify_one();
                            let response = handler(&request);
                            if let Some(gate) = response.before_headers
                                && !gate_or_close(&mut socket, &gate).await
                            {
                                closed.notify_one();
                                return;
                            }
                            if socket.write_all(&response.wire).await.is_err() {
                                closed.notify_one();
                                return;
                            }
                            if let Some((gate, tail)) = response.tail {
                                if !gate_or_close(&mut socket, &gate).await {
                                    closed.notify_one();
                                    return;
                                }
                                if socket.write_all(&tail).await.is_err() {
                                    closed.notify_one();
                                    return;
                                }
                            }
                            if response.hold_open {
                                let mut byte = [0];
                                let result = socket.read(&mut byte).await;
                                assert!(matches!(result, Ok(0)) || result.is_err());
                                closed.notify_one();
                            }
                            let _ = socket.shutdown().await;
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self {
            url,
            requests,
            requested,
            closed,
            task,
        }
    }

    async fn open(&self) -> HttpDownload {
        timeout(
            WAIT,
            HttpDownload::open(&self.url, CancellationToken::new()),
        )
        .await
        .expect("opening a loopback download stalled")
        .unwrap()
    }

    async fn wait_for_request(&self) {
        timeout(WAIT, async {
            while self.requests.lock().unwrap().is_empty() {
                self.requested.notified().await;
            }
        })
        .await
        .expect("the origin did not receive a request");
    }

    async fn wait_for_close(&self) {
        timeout(WAIT, self.closed.notified())
            .await
            .expect("the unfinished upstream body was not closed");
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        // Dropping the JoinSet also aborts all accepted connection tasks.
        self.task.abort();
    }
}

fn header<'a>(request: &'a str, key: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(key).then_some(value.trim())
    })
}

fn assert_full_get(request: &str) {
    assert!(request.starts_with("GET "), "{request}");
    assert_eq!(header(request, "accept-encoding"), Some("identity"));
    for name in [
        "range",
        "if-range",
        "if-match",
        "if-unmodified-since",
        "authorization",
    ] {
        assert_eq!(header(request, name), None, "{name}: {request}");
    }
    assert!(!request.contains("discarded-fragment"));
}

fn assert_redacted(value: impl std::fmt::Display, debug: impl std::fmt::Debug) {
    let text = format!("{value} {debug:?}");
    for secret in [
        "127.0.0.1",
        "private-movie",
        "never-print-me",
        "token=",
        "upstream-private",
        "credential-private",
    ] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
}

async fn next(download: &mut HttpDownload, limit: usize) -> io::Result<bytes::Bytes> {
    timeout(WAIT, download.next_chunk(limit))
        .await
        .expect("reading a loopback download stalled")
}

#[tokio::test]
async fn sequential_full_get_needs_neither_validators_nor_range_support() {
    for metadata in [
        "",
        "ETag: W/\"weak\"\r\n",
        "ETag: invalid\r\nLast-Modified: invalid\r\n",
    ] {
        // This origin ignores ranges and always sends the entire representation.
        let mock = Mock::new(move |_| {
            reply(
                "200 OK",
                &format!(
                    "Content-Length: {}\r\nAccept-Ranges: none\r\n{metadata}",
                    DATA.len()
                ),
                DATA,
            )
        })
        .await;
        let mut download = HttpDownload::open(
            format!("{}#discarded-fragment", mock.url),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(download.len(), DATA.len() as u64);
        assert!(!download.is_empty());
        assert_redacted("download", &download);
        let mut output = Vec::new();
        for limit in [1, 7, 3, usize::MAX].into_iter().cycle() {
            let chunk = next(&mut download, limit).await.unwrap();
            assert!(chunk.len() <= limit.min(CHUNK_LIMIT));
            if chunk.is_empty() {
                break;
            }
            output.extend_from_slice(&chunk);
            assert!(output.len() <= DATA.len());
        }
        assert_eq!(output, DATA);
        assert_eq!(download.len(), DATA.len() as u64);
        assert!(next(&mut download, 1).await.unwrap().is_empty());
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_full_get(&requests[0]);
    }
}

#[tokio::test]
async fn chunks_respect_the_requested_limit_and_the_64k_cap() {
    let data: Vec<u8> = (0..250_000).map(|i| (i % 251) as u8).collect();
    let expected = data.clone();
    let mock = Mock::new(move |_| full(&data)).await;
    let mut download = mock.open().await;
    let mut output = Vec::new();
    for limit in [17, usize::MAX, 70_000, 1024].into_iter().cycle() {
        let chunk = next(&mut download, limit).await.unwrap();
        assert!(chunk.len() <= limit.min(CHUNK_LIMIT));
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
        assert!(output.len() <= expected.len());
    }
    assert_eq!(output, expected);
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_full_response_survives_the_save_stream_reader_handoff() {
    // Match the app's HttpDownload -> try_unfold -> StreamReader boundary,
    // including a body larger than the transport cap and a pause before saving.
    let data: Vec<u8> = (0..600 * 1024 + 37).map(|i| (i % 251) as u8).collect();
    let expected = data.clone();
    let mock = Mock::new(move |_| full(&data)).await;
    let download = mock.open().await;
    let length = download.len();
    let output = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let chunks = futures_util::stream::try_unfold(download, |mut download| async move {
            let bytes = download.next_chunk(CHUNK_LIMIT).await?;
            Ok::<_, io::Error>(if bytes.is_empty() {
                None
            } else {
                Some((bytes, download))
            })
        })
        .boxed();
        let mut reader = tokio_util::io::StreamReader::new(chunks);
        let mut output = Vec::new();
        for limit in [17, CHUNK_LIMIT, 503].into_iter().cycle() {
            if output.len() as u64 == length {
                break;
            }
            let remaining = (length - output.len() as u64) as usize;
            let mut buffer = vec![0; remaining.min(limit)];
            let count = reader.read(&mut buffer).await.unwrap();
            assert!(count != 0, "the full-response reader must not end early");
            output.extend_from_slice(&buffer[..count]);
            // The real atomic saver yields while blocking disk writes run.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(reader.read(&mut [0; 1]).await.unwrap(), 0);
        output
    });
    assert_eq!(timeout(WAIT, output).await.unwrap().unwrap(), expected);
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn huge_declared_length_is_not_preallocated_and_dropping_closes_the_body() {
    // Hyper reserves the top two u64 lengths for internal framing sentinels.
    // A petabyte still makes any accidental length-based allocation fail.
    const HUGE_LENGTH: u64 = 1 << 50;
    let mock = Mock::new(|_| {
        let mut response = reply(
            "200 OK",
            &format!("Content-Length: {HUGE_LENGTH}\r\n"),
            b"x",
        );
        response.hold_open = true;
        response
    })
    .await;
    let mut download = mock.open().await;
    assert_eq!(download.len(), HUGE_LENGTH);
    assert_eq!(
        next(&mut download, usize::MAX).await.unwrap(),
        b"x".as_slice()
    );
    assert!(
        timeout(Duration::from_millis(50), download.next_chunk(usize::MAX))
            .await
            .is_err(),
        "a still-open partial body must not return empty EOF"
    );
    drop(download);
    mock.wait_for_close().await;
}

#[tokio::test]
async fn empty_download_is_complete_without_waiting_for_tcp_close() {
    let mock = Mock::new(|_| {
        let mut response = full(b"");
        response.hold_open = true;
        response
    })
    .await;
    let mut download = mock.open().await;
    assert_eq!(download.len(), 0);
    assert!(download.is_empty());
    assert!(next(&mut download, 1).await.unwrap().is_empty());
    assert!(next(&mut download, usize::MAX).await.unwrap().is_empty());
    assert_eq!(
        next(&mut download, 0).await.unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(next(&mut download, 1).await.unwrap().is_empty());
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn zero_limit_does_not_consume_or_poison_a_download() {
    let mock = Mock::new(|_| full(DATA)).await;
    let mut download = mock.open().await;
    for _ in 0..2 {
        assert_eq!(
            next(&mut download, 0).await.unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    let mut output = Vec::new();
    loop {
        let chunk = next(&mut download, 5).await.unwrap();
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
        assert!(output.len() <= DATA.len());
    }
    assert_eq!(output, DATA);
}

#[tokio::test]
async fn truncated_bodies_fail_instead_of_eof_and_stay_poisoned() {
    for available in [0, 1, DATA.len() - 1] {
        let mock = Mock::new(move |_| {
            reply(
                "200 OK",
                &format!("Content-Length: {}\r\n", DATA.len()),
                &DATA[..available],
            )
        })
        .await;
        let mut download = mock.open().await;
        let mut output = Vec::new();
        loop {
            match next(&mut download, 7).await {
                Ok(chunk) => {
                    assert!(
                        !chunk.is_empty(),
                        "truncation was reported as successful EOF"
                    );
                    output.extend_from_slice(&chunk);
                    assert!(output.len() <= available);
                }
                Err(error) => {
                    assert_redacted(&error, &error);
                    break;
                }
            }
        }
        assert_eq!(output, DATA[..output.len()]);
        assert!(next(&mut download, 1).await.is_err());
        assert!(next(&mut download, usize::MAX).await.is_err());
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn content_length_defines_body_eof_not_extra_wire_bytes_or_socket_close() {
    for length in [0, 3] {
        let mock = Mock::new(move |_| {
            let mut response = reply(
                "200 OK",
                &format!("Content-Length: {length}\r\n"),
                b"abcnot-part-of-the-response-body",
            );
            response.hold_open = true;
            response
        })
        .await;
        let mut download = mock.open().await;
        let mut output = Vec::new();
        loop {
            let chunk = next(&mut download, usize::MAX).await.unwrap();
            if chunk.is_empty() {
                break;
            }
            output.extend_from_slice(&chunk);
            assert!(output.len() <= length);
        }
        assert_eq!(output, &b"abc"[..length]);
    }
}

#[tokio::test]
async fn invalid_status_encoding_length_and_ambiguous_framing_are_rejected() {
    for (status, headers, body) in [
        (
            "200 OK",
            "Content-Length: 3\r\nContent-Encoding: gzip\r\n",
            "abc",
        ),
        (
            "200 OK",
            "Content-Length: 3\r\nContent-Encoding: br\r\n",
            "abc",
        ),
        (
            "200 OK",
            "Content-Length: 3\r\nContent-Encoding: identity, gzip\r\n",
            "abc",
        ),
        ("200 OK", "", "abc"),
        (
            "200 OK",
            "Transfer-Encoding: chunked\r\n",
            "3\r\nabc\r\n0\r\n\r\n",
        ),
        (
            "200 OK",
            "Content-Length: 3\r\nTransfer-Encoding: chunked\r\n",
            "3\r\nabc\r\n0\r\n\r\n",
        ),
        (
            "200 OK",
            "Content-Length: 3\r\nContent-Range: bytes 0-2/3\r\n",
            "abc",
        ),
        (
            "200 OK",
            "Content-Length: 3\r\nContent-Length: 4\r\n",
            "abc",
        ),
        ("200 OK", "Content-Length: invalid\r\n", ""),
        ("200 OK", "Content-Length: 18446744073709551615\r\n", ""),
        ("200 OK", "Content-Length: 18446744073709551616\r\n", ""),
        ("204 No Content", "Content-Length: 0\r\n", ""),
        (
            "206 Partial Content",
            "Content-Length: 3\r\nContent-Range: bytes 0-2/3\r\n",
            "abc",
        ),
        ("404 upstream-private", "Content-Length: 0\r\n", ""),
        ("500 upstream-private", "Content-Length: 0\r\n", ""),
    ] {
        let mock = Mock::new(move |_| {
            let mut response = reply(status, headers, body.as_bytes());
            response.hold_open = true;
            response
        })
        .await;
        let error = timeout(
            WAIT,
            HttpDownload::open(&mock.url, CancellationToken::new()),
        )
        .await
        .unwrap_or_else(|_| panic!("opening stalled for {status} / {headers:?}"))
        .expect_err("unsupported response was accepted");
        assert_redacted(&error, &error);
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn explicit_identity_encoding_is_supported_and_headers_are_bounded() {
    let mock = Mock::new(|_| {
        reply(
            "200 OK",
            "Content-Length: 3\r\nContent-Encoding: identity\r\n",
            b"abc",
        )
    })
    .await;
    let mut download = mock.open().await;
    let mut output = Vec::new();
    loop {
        let chunk = next(&mut download, 3).await.unwrap();
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
        assert!(output.len() <= 3);
    }
    assert_eq!(output, b"abc");

    let huge = Mock::new(|_| {
        reply(
            "200 OK",
            &format!("Content-Length: 0\r\nX-Padding: {}\r\n", "x".repeat(17_000)),
            b"",
        )
    })
    .await;
    let error = timeout(
        WAIT,
        HttpDownload::open(&huge.url, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_redacted(&error, &error);
}

#[tokio::test]
async fn invalid_urls_and_credentials_are_rejected_before_any_request() {
    let mock = Mock::new(|_| full(DATA)).await;
    for url in [
        "file:///etc/passwd".to_owned(),
        "http:127.0.0.1".to_owned(),
        "http:///127.0.0.1/".to_owned(),
        "http://127.0.0.1/%xx".to_owned(),
        format!(" {}", mock.url),
        format!("{}/bad\\path", mock.url),
        mock.url
            .replacen("http://", "http://user:credential-private@", 1),
        mock.url.replacen("http://", "http://@", 1),
    ] {
        let error = timeout(WAIT, HttpDownload::open(&url, CancellationToken::new()))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_redacted(&error, &error);
    }
    assert!(mock.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn relative_redirects_strip_fragments_and_keep_full_get_semantics() {
    let mock = Mock::new(|request| {
        if request.starts_with("GET /private-movie?") {
            reply(
                "302 Found",
                "Location: /final#discarded-fragment\r\nContent-Length: 0\r\n",
                b"",
            )
        } else {
            full(b"abc")
        }
    })
    .await;
    let mut download = mock.open().await;
    let mut output = Vec::new();
    loop {
        let chunk = next(&mut download, 2).await.unwrap();
        if chunk.is_empty() {
            break;
        }
        output.extend_from_slice(&chunk);
        assert!(output.len() <= 3);
    }
    assert_eq!(output, b"abc");
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /final HTTP/1.1\r\n"));
    for request in requests.iter() {
        assert_full_get(request);
    }
}

#[tokio::test]
async fn bad_redirect_targets_are_not_followed_or_disclosed() {
    for location in [
        "file:///upstream-private",
        "http://user:credential-private@127.0.0.1/",
        "http://@127.0.0.1/",
        "//@127.0.0.1/",
        "http:upstream-private",
        "http:///upstream-private",
        "/bad%xx",
    ] {
        let mock = Mock::new(move |_| {
            reply(
                "302 Found",
                &format!("Location: {location}\r\nContent-Length: 0\r\n"),
                b"",
            )
        })
        .await;
        let error = timeout(
            WAIT,
            HttpDownload::open(&mock.url, CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_redacted(&error, &error);
        assert_eq!(mock.requests.lock().unwrap().len(), 1, "{location}");
    }
}

#[tokio::test]
async fn exactly_five_redirects_succeed_but_a_sixth_is_rejected() {
    for redirects in [5, 6] {
        let mock = Mock::new(move |request| {
            let step = request
                .split_whitespace()
                .nth(1)
                .unwrap()
                .trim_start_matches('/')
                .parse::<usize>()
                .unwrap_or(0);
            if step == redirects {
                full(b"")
            } else {
                reply(
                    "302 Found",
                    &format!("Location: /{}\r\nContent-Length: 0\r\n", step + 1),
                    b"",
                )
            }
        })
        .await;
        let result = timeout(
            WAIT,
            HttpDownload::open(&mock.url, CancellationToken::new()),
        )
        .await
        .unwrap();
        if redirects == 5 {
            assert!(result.unwrap().is_empty());
        } else {
            let error = result.unwrap_err();
            assert_redacted(&error, &error);
        }
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        for request in requests.iter() {
            assert_full_get(request);
        }
    }
}

#[tokio::test]
async fn transport_errors_do_not_disclose_the_private_url() {
    let mock = Mock::new(|_| {
        let mut response = full(b"");
        response.wire.clear(); // Close without an HTTP response.
        response
    })
    .await;
    let error = timeout(
        WAIT,
        HttpDownload::open(&mock.url, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_redacted(&error, &error);
}

#[tokio::test]
async fn cancellation_interrupts_open_and_releases_the_connection() {
    let gate = Arc::new(Notify::new());
    let mock = Mock::new(move |_| {
        let mut response = full(DATA);
        response.before_headers = Some(gate.clone());
        response
    })
    .await;
    let cancel = CancellationToken::new();
    let (result, ()) = timeout(WAIT, async {
        tokio::join!(HttpDownload::open(&mock.url, cancel.clone()), async {
            mock.wait_for_request().await;
            cancel.cancel();
        })
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    mock.wait_for_close().await;

    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        HttpDownload::open(&mock.url, cancel)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_of_a_pending_body_read_poisons_and_closes_the_download() {
    let gate = Arc::new(Notify::new());
    let mock = Mock::new(move |_| {
        let mut response = reply("200 OK", "Content-Length: 6\r\n", b"abc");
        response.tail = Some((gate.clone(), b"def".to_vec()));
        response
    })
    .await;
    let cancel = CancellationToken::new();
    let mut download = HttpDownload::open(&mock.url, cancel.clone()).await.unwrap();
    let mut prefix = Vec::new();
    while prefix.len() < 3 {
        let chunk = next(&mut download, 3 - prefix.len()).await.unwrap();
        assert!(!chunk.is_empty());
        prefix.extend_from_slice(&chunk);
    }
    assert_eq!(prefix, b"abc");
    // Poll and drop once to establish that the next body read really is pending.
    assert!(
        timeout(Duration::from_millis(50), download.next_chunk(3))
            .await
            .is_err()
    );
    let (result, ()) = timeout(WAIT, async {
        tokio::join!(download.next_chunk(3), async {
            tokio::task::yield_now().await;
            cancel.cancel();
        })
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert!(next(&mut download, 3).await.is_err());
    mock.wait_for_close().await;
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dropping_a_next_chunk_future_preserves_the_cursor_for_retry() {
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let mock = Mock::new(move |_| {
        let mut response = reply("200 OK", "Content-Length: 6\r\n", b"abc");
        response.tail = Some((gate.clone(), b"def".to_vec()));
        response
    })
    .await;
    let mut download = mock.open().await;
    let mut output = Vec::new();
    // Small reads also exercise retention of surplus bytes in a transport chunk.
    while output.len() < 3 {
        let chunk = next(&mut download, 1).await.unwrap();
        assert_eq!(chunk.len(), 1);
        output.extend_from_slice(&chunk);
    }
    assert_eq!(output, b"abc");
    for _ in 0..2 {
        assert!(
            timeout(Duration::from_millis(50), download.next_chunk(3))
                .await
                .is_err(),
            "partial body must wait, not report EOF"
        );
    }
    // A dropped read future must not close or restart the response.
    assert!(
        timeout(Duration::from_millis(50), mock.closed.notified())
            .await
            .is_err()
    );
    release.notify_one();
    while output.len() < 6 {
        let chunk = next(&mut download, 2).await.unwrap();
        assert!(!chunk.is_empty());
        output.extend_from_slice(&chunk);
        assert!(output.len() <= 6);
    }
    assert_eq!(output, b"abcdef");
    assert!(next(&mut download, 1).await.unwrap().is_empty());
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}
