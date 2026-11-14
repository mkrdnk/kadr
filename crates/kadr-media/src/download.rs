//! Sequential saving of a single HTTP response, without positional range support.

use std::{fmt, io};

use bytes::Bytes;
use reqwest::{
    Method, Response, StatusCode,
    header::{CONTENT_RANGE, TRANSFER_ENCODING},
};
use tokio_util::sync::CancellationToken;

use crate::remote::{
    Endpoint, MAX_READ, bounded, check_identity, invalid, length_header, network_error, request,
    unsupported, validated_url,
};

// Retain no more than one bounded HTTP transport chunk between calls. This is
// independent of the advertised file length and bounds even future transports.
const MAX_TRANSPORT_CHUNK: usize = 512 * 1024;

/// One full GET of an app-approved HTTP(S) resource, for sequential saving.
///
/// Unlike `RemoteSource`, this does not need validators or Range support: every
/// byte comes from the same response. It deliberately does not implement
/// `ByteSource` (which promises arbitrary positional reads).
///
/// The consumer must write chunks into a temporary file and publish only after
/// receiving validated EOF. Dropping this object closes any outstanding body.
pub struct HttpDownload {
    response: Option<Response>,
    length: u64,
    remaining: u64,
    pending: Bytes,
    cancel: CancellationToken,
    failed: bool,
}

impl fmt::Debug for HttpDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpDownload")
            .field("resource", &"[redacted]")
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl HttpDownload {
    /// Start a new full GET, never HEAD or Range. Only a 200 response with an
    /// explicit known Content-Length and identity encoding is supported.
    ///
    /// This shares URL, DNS pinning, redirect, proxy, header-limit and error
    /// privacy policies with `RemoteSource`. The token cancels opening and reads.
    pub async fn open(url: impl AsRef<str>, cancel: CancellationToken) -> io::Result<Self> {
        let url = validated_url(url.as_ref())?;
        let (response, length) = bounded(&cancel, async {
            let endpoint = Endpoint::resolve(url, false).await?;
            let mut redirects = 0;
            let (mut response, _) = request(endpoint, Method::GET, None, &mut redirects).await?;
            if response.status() != StatusCode::OK {
                return Err(invalid("remote download requires a full 200 response"));
            }
            check_identity(response.headers())?;
            if response.headers().contains_key(CONTENT_RANGE)
                || response.headers().contains_key(TRANSFER_ENCODING)
            {
                return Err(invalid("remote download has ambiguous response framing"));
            }
            let length = length_header(response.headers())?
                .ok_or_else(|| unsupported("remote download length is unknown"))?;
            // A length-driven saver need not request a chunk for an empty file.
            if length == 0 && response.chunk().await.map_err(network_error)?.is_some() {
                return Err(invalid("remote download exceeds declared length"));
            }
            Ok((response, length))
        })
        .await?;
        Ok(Self {
            response: (length != 0).then_some(response),
            length,
            remaining: length,
            pending: Bytes::new(),
            cancel,
            failed: false,
        })
    }

    pub fn len(&self) -> u64 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Return the next sequential bytes, at most min(max_len, 64 KiB).
    ///
    /// An empty buffer means validated EOF, never temporary unavailability.
    /// The final nonempty chunk is withheld until EOF is validated, so a saver
    /// that stops after `len()` bytes cannot miss a final body/framing error.
    /// Zero max_len is invalid without advancing or poisoning the download.
    /// Read/cancellation errors poison the download and release its body.
    /// Dropping this future preserves the cursor and any buffered bytes.
    pub async fn next_chunk(&mut self, max_len: usize) -> io::Result<Bytes> {
        if self.failed {
            return Err(invalid("remote download is no longer usable"));
        }
        if max_len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero-sized remote download read",
            ));
        }
        let cancel = self.cancel.clone();
        let result = bounded(&cancel, self.next(max_len.min(MAX_READ))).await;
        if result.is_err() {
            self.response = None;
            self.pending = Bytes::new();
            self.failed = true;
        }
        result
    }

    async fn next(&mut self, limit: usize) -> io::Result<Bytes> {
        if self.remaining == 0 {
            return Ok(Bytes::new());
        }
        let response = self
            .response
            .as_mut()
            .ok_or_else(|| invalid("remote download body missing"))?;
        while self.pending.is_empty() {
            self.pending = response
                .chunk()
                .await
                .map_err(network_error)?
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated remote download")
                })?;
            if self.pending.len() > MAX_TRANSPORT_CHUNK {
                return Err(invalid("remote download transport chunk exceeds limit"));
            }
            if self.pending.len() as u64 > self.remaining {
                return Err(invalid("remote download exceeds declared length"));
            }
        }
        let size = limit.min(self.pending.len());
        if size as u64 == self.remaining {
            // Do not advance the cursor until this await succeeds. If the
            // caller drops its future here, pending still holds the final bytes.
            while let Some(extra) = response.chunk().await.map_err(network_error)? {
                if !extra.is_empty() {
                    return Err(invalid("remote download exceeds declared length"));
                }
            }
            self.response = None;
        }
        self.remaining -= size as u64;
        Ok(self.pending.split_to(size))
    }
}
