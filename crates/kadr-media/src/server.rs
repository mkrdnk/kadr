use std::{
    convert::Infallible,
    future::poll_fn,
    io,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Frame, Incoming},
    header::{
        ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, IF_RANGE,
        RANGE,
    },
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use rand::RngCore;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, DuplexStream, WriteHalf},
    net::{TcpListener, tcp::OwnedReadHalf},
    task::JoinHandle,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    ByteSource,
    range::{Selection, select_range},
};

const CHUNK_SIZE: usize = 64 * 1024;
const INPUT_BUFFER_SIZE: usize = 64 * 1024;
type Body = UnsyncBoxBody<Bytes, io::Error>;

#[derive(Clone, Copy)]
pub struct ServerOptions {
    /// Maximum wait for each source read, including byte availability.
    pub read_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(10),
        }
    }
}

/// A single-source session listening only on an ephemeral IPv4 loopback port.
///
/// Call `shutdown().await` to stop and join all connection tasks. Drop also
/// requests cancellation, but cannot wait for cleanup to finish.
pub struct Server {
    address: SocketAddr,
    path: String,
    stop: CancellationToken,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    pub async fn start(source: Arc<dyn ByteSource>, options: ServerOptions) -> io::Result<Self> {
        Self::start_with_content_type(source, options, "application/octet-stream").await
    }

    /// MIME comes from the session owner, never an incoming request. Active
    /// HTML/SVG content must not execute under the local streaming origin.
    pub async fn start_with_content_type(
        source: Arc<dyn ByteSource>,
        options: ServerOptions,
        content_type: &str,
    ) -> io::Result<Self> {
        let content_type = media_type(content_type);
        let mut random = [0; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| io::Error::other("session token generation failed"))?;
        let path = format!(
            "/{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let stop = CancellationToken::new();
        let task = tokio::spawn(serve(
            listener,
            source,
            path.clone(),
            options,
            stop.clone(),
            content_type,
        ));
        Ok(Self {
            address,
            path,
            stop,
            task: Some(task),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Private bearer URL. Only disclose it deliberately to a local player.
    pub fn private_url(&self) -> String {
        format!("http://{}{}", self.address, self.path)
    }

    pub async fn shutdown(mut self) -> io::Result<()> {
        self.stop.cancel();
        self.task
            .take()
            .expect("server task exists until shutdown")
            .await
            .map_err(io::Error::other)?
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

async fn serve(
    listener: TcpListener,
    source: Arc<dyn ByteSource>,
    path: String,
    options: ServerOptions,
    stop: CancellationToken,
    content_type: &'static str,
) -> io::Result<()> {
    let mut connections = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break Ok(()),
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                if connections.len() >= 32 {
                    drop(socket);
                    continue;
                }
                let source = source.clone();
                let path = path.clone();
                let stop = stop.clone();
                connections.spawn(async move {
                    let (http_io, transport_io) = tokio::io::duplex(INPUT_BUFFER_SIZE);
                    let (socket_read, mut socket_write) = socket.into_split();
                    let (mut transport_read, transport_write) = tokio::io::split(transport_io);
                    let service = service_fn(|request| {
                        let response = respond(request, &path, source.clone(), options, stop.clone(), content_type);
                        async move { Ok::<_, Infallible>(response) }
                    });
                    let connection = async {
                        http1::Builder::new()
                            .timer(hyper_util::rt::TokioTimer::new())
                            .header_read_timeout(Duration::from_secs(10))
                            .max_headers(32)
                            .serve_connection(TokioIo::new(http_io), service)
                            .await
                    };
                    let input = forward_input(socket_read, transport_write);
                    let output = tokio::io::copy(&mut transport_read, &mut socket_write);
                    tokio::pin!(input, output);
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => {},
                        // Never log errors: a request can contain the private token.
                        _ = connection => {
                            // Hyper may finish with bytes still in the output
                            // bridge (notably Connection: close responses).
                            tokio::select! {
                                () = stop.cancelled() => {},
                                _ = &mut output => {},
                            }
                        },
                        _ = &mut input => {},
                        _ = &mut output => {},
                    }
                });
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
        }
    };
    // Dropping each connection also drops its body and pending source future.
    stop.cancel();
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    result
}

/// Detect transport EOF even when Hyper has buffered a pipelined request or is
/// waiting for a request body. In those states Hyper need not poll socket reads.
/// Never wait on a full read-ahead buffer: close on overflow, otherwise EOF
/// detection could itself get stuck behind application backpressure.
async fn forward_input(
    mut socket: OwnedReadHalf,
    mut transport: WriteHalf<DuplexStream>,
) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let read = socket.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        try_forward_chunk(&mut transport, &buffer[..read]).await?;
    }
}

async fn try_forward_chunk(
    transport: &mut WriteHalf<DuplexStream>,
    mut pending: &[u8],
) -> io::Result<()> {
    while !pending.is_empty() {
        // DuplexStream can also yield Pending when its cooperative task budget
        // runs out. Disable budgeting for this one immediate write so only a
        // genuinely full buffer triggers overflow-close. Socket reads still
        // yield normally, keeping the pump cooperative.
        let written = tokio::task::unconstrained(poll_fn(|cx| {
            match Pin::new(&mut *transport).poll_write(cx, pending) {
                Poll::Pending => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request input buffer exceeded",
                ))),
                ready => ready,
            }
        }))
        .await?;
        if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        pending = &pending[written..];
    }
    Ok(())
}

fn empty(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, 0)
        .header(CACHE_CONTROL, "no-store")
        .body(
            Full::new(Bytes::new())
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .expect("constant response headers are valid")
}

fn respond(
    request: Request<Incoming>,
    path: &str,
    source: Arc<dyn ByteSource>,
    options: ServerOptions,
    stop: CancellationToken,
    content_type: &'static str,
) -> Response<Body> {
    // Authorize the parsed path; the HTTP parser can already have removed a fragment.
    // No application-level decoding, queries, absolute URLs or filenames.
    if request.uri().path() != path
        || request.uri().query().is_some()
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
    {
        return empty(StatusCode::NOT_FOUND);
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(ALLOW, "GET, HEAD".parse().unwrap());
        return response;
    }
    let length = source.len();
    let selection = if request.method() == Method::HEAD || request.headers().contains_key(IF_RANGE)
    {
        Selection::Full
    } else {
        let mut ranges = request.headers().get_all(RANGE).iter();
        match (ranges.next(), ranges.next()) {
            (Some(value), None) => value
                .to_str()
                .map(|value| select_range(value, length))
                .unwrap_or(Selection::Full),
            _ => Selection::Full,
        }
    };
    let (status, offset, count) = match selection {
        Selection::Full => (StatusCode::OK, 0, length),
        Selection::Partial { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        Selection::Unsatisfiable => {
            let mut response = empty(StatusCode::RANGE_NOT_SATISFIABLE);
            response
                .headers_mut()
                .insert(CONTENT_RANGE, format!("bytes */{length}").parse().unwrap());
            response
                .headers_mut()
                .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            return response;
        }
    };
    let body = if request.method() == Method::HEAD || count == 0 {
        Full::new(Bytes::new())
            .map_err(|never| match never {})
            .boxed_unsync()
    } else {
        let state = (source, offset, count, stop);
        let stream = stream::try_unfold(
            state,
            move |(source, offset, remaining, stop)| async move {
                if remaining == 0 {
                    return Ok(None);
                }
                let size = remaining.min(CHUNK_SIZE as u64) as usize;
                let bytes = tokio::select! {
                    biased;
                    () = stop.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "session stopped")),
                    result = tokio::time::timeout(options.read_timeout, source.read_at(offset, size)) => {
                        result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "source read timed out"))??
                    }
                };
                if bytes.is_empty() || bytes.len() > size {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "invalid source read length",
                    ));
                }
                let read = bytes.len() as u64;
                Ok(Some((
                    Frame::data(bytes),
                    (source, offset + read, remaining - read, stop),
                )))
            },
        );
        StreamBody::new(stream).boxed_unsync()
    };
    let mut response = Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, count)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, content_type)
        .header("X-Content-Type-Options", "nosniff")
        .header(CACHE_CONTROL, "no-store")
        .body(body)
        .expect("numeric response headers are valid");
    if let Selection::Partial { start, end } = selection {
        response.headers_mut().insert(
            CONTENT_RANGE,
            format!("bytes {start}-{end}/{length}").parse().unwrap(),
        );
    }
    response
}

fn media_type(value: &str) -> &'static str {
    match value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "video/ogg" | "application/ogg" => "video/ogg",
        "video/x-matroska" => "video/x-matroska",
        "video/quicktime" => "video/quicktime",
        "audio/mpeg" => "audio/mpeg",
        "audio/wav" | "audio/x-wav" => "audio/wav",
        "audio/ogg" => "audio/ogg",
        "audio/mp4" => "audio/mp4",
        "audio/webm" => "audio/webm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_passive_media_types_are_exposed_to_browsers() {
        assert_eq!(media_type("video/mp4; codecs=avc1"), "video/mp4");
        assert_eq!(media_type("text/html"), "application/octet-stream");
        assert_eq!(media_type("image/svg+xml"), "application/octet-stream");
        assert_eq!(
            media_type("video/mp4\r\nX-Bad: yes"),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn input_capacity_is_not_confused_with_cooperative_budget() {
        tokio::spawn(async {
            let (mut receiver, sender) = tokio::io::duplex(32);
            let (_, mut sender) = tokio::io::split(sender);
            while tokio::task::coop::has_budget_remaining() {
                tokio::task::coop::consume_budget().await;
            }
            try_forward_chunk(&mut sender, b"still has capacity")
                .await
                .unwrap();
            let mut bytes = [0; 18];
            receiver.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"still has capacity");
        })
        .await
        .unwrap();
    }
}
