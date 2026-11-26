# Remote HTTP sources

`kadr_media::RemoteSource` is the byte-source adapter for an **app-approved**
HTTP(S) input. The application validates/selects the source, opens it, and hands
that source to the loopback server or atomic save workflow. An incoming streaming
request must never supply an upstream URL. This is not an open proxy.

```rust,ignore
let cancel = tokio_util::sync::CancellationToken::new();
let source = kadr_media::RemoteSource::open(approved_url, cancel.clone()).await?;
let mime = source.content_type(); // Option<&str>; lowercased MIME essence
// Hand source to the server/save workflow. Cancel when the session ends.
```

## HTTP contract

- `open` repeats `kadr-core`'s strict lexical HTTP(S) validation, rejects userinfo,
  and removes fragments. URL/reference length is limited to 16 KiB.
- HEAD discovers the exact `Content-Length`. Unsupported HEAD (405/501), or a
  successful HEAD without length, falls back to GET `Range: bytes=0-0`. A 206
  probe must return exactly that byte and a known total in `Content-Range`.
  A range-ignoring 200 can supply its length without downloading its body.
  Empty resources work with HEAD length zero or a 416 probe `bytes */0`.
- Nonempty sources require a strong ETag or valid Last-Modified date. Weak ETag
  alone and missing validators are unsupported rather than silently mixing
  bytes from different versions. Strong ETag is preferred for `If-Match`;
  otherwise reads send `If-Unmodified-Since`. Every successful response must
  retain **all** opening validators, and its total length must match.
- Reads use absolute bounded ranges, return at most 64 KiB, and never cache the
  entire file. A 206 must match the exact requested start, end and total;
  multipart, unknown totals, extra bytes and truncated bodies fail.
- A 200 is accepted **only** if this read requests the complete file and that
  file fits within the 64 KiB bound. Other 200 responses produce an explicit
  unsupported-range error without consuming their bodies.
- Known resource changes (validator/total-length mismatch, 412/416, or a new
  redirect during reads) permanently invalidate the source. Reopen explicitly.
  The final opening URL is pinned; reads do not follow later redirects.
- Requests require `Accept-Encoding: identity`; decoding is disabled even when
  other workspace crates enable compression features. Encoded responses fail.
- MIME is optional and sanitized: only an ASCII token/token essence up to 127
  bytes, lowercased, without parameters. This adapter does not change the
  existing server's MIME policy; the application/server must opt in separately.

HTTP validators depend on upstream honesty. Last-Modified has second-level
granularity and cannot detect same-second edits if the server reuses its date;
strong ETags are the preferred guarantee. No client can detect an origin that
changes bytes while deliberately preserving all validators and lengths.

## Networking and resource limits

- Opening follows at most five total HTTP(S) redirects, including HEAD fallback.
  Redirect locations undergo lexical validation before URL resolution.
- DNS answers are checked and pinned into the endpoint's reqwest client. Once a
  public address has been reached, no redirect can target private, loopback,
  link-local, shared-address, multicast, reserved or special-use address space.
  Mixed public/nonpublic DNS answers fail closed. Direct user-approved local
  addresses (including localhost test servers) remain allowed.
- Proxies from the environment are disabled. Only HTTP/1 is used, with at most
  64 response headers, and accepted header names/values total at most 16 KiB.
  Reqwest/hyper also bounds parsing buffers before this post-parse check;
  this is not a 16 KiB pre-allocation wire-header limit.
- One 30-second deadline covers each complete open/read operation, including
  DNS, redirects and body consumption. Connect timeout is 10 seconds.
  Cancelling the shared token aborts opening and every concurrent read. Dropping
  a read future drops its request/body; no background download is spawned.
- Each read allocates only its bounded output plus HTTP transport buffers.
  There is no persistent remote cache. Saving should copy this ByteSource
  incrementally through the application's atomic save operation.

DNS pinning closes a check-then-second-resolution rebinding gap for the checked
endpoint. It is **not** a general network sandbox: OS routes, VPNs, NAT/NAT64,
transparent proxies, origin behavior, and future special-address assignments
can change reachability. User-approved local inputs intentionally permit local
network access. Do not expose source approval/opening to untrusted stream callers.
TLS uses reqwest 0.13.5's rustls backend and normal certificate verification.

## Privacy and checks

The source's custom Debug output redacts its URL. Returned errors are static,
contain no upstream error chain, and never include URLs, query tokens, redirect
locations, or response bodies. The adapter performs no logging.

`tests/remote.rs` uses real loopback TCP sockets with controlled HEAD/GET/redirect
responses, without external network access. Private unit tests check the public
redirect address guard and the full-operation timeout using paused Tokio time.

## Sequential saving without Range or validators

`kadr_media::HttpDownload` is a separate, sequential API for saving an approved
HTTP source when positional `RemoteSource` is unavailable:

```rust,ignore
let mut download = kadr_media::HttpDownload::open(approved_url, cancel.clone()).await?;
let length = download.len(); // exact, immutable length; use for disk checks
loop {
    let bytes = download.next_chunk(64 * 1024).await?;
    if bytes.is_empty() {
        break; // validated EOF
    }
    // Write to the atomic saver’s private temporary file, never final output.
    temporary_file.write_all(&bytes).await?;
}
// Flush/sync and publish without clobbering only after successful completion.
```

Opening sends one full GET (no HEAD and no Range) and requires status 200,
explicit `Content-Length`, and identity content encoding. No ETag or modification
date is needed: all returned bytes belong to that single HTTP response. A server
that ignores Range or has no validators can therefore still be saved. Saving
starts a new request; bytes buffered inside an external player cannot be reused.

This type **does not implement ByteSource**: its mutable `next_chunk` API is
sequential, and cannot be mistaken for arbitrary positional reading. It must not
be supplied directly to the loopback range server. Each call returns at most
64 KiB (or the caller's smaller limit). Retained data is at most one transport
chunk, explicitly capped at 512 KiB, plus bounded HTTP buffers. Neither a large
advertised length nor the full file is allocated in memory.

The final nonempty chunk is withheld until response EOF is validated. This
ensures length-driven savers cannot publish a file before the last framing/body
error is known. Short bodies and excess decoded body bytes fail; ambiguous
Content-Range/Transfer-Encoding responses are rejected. HTTP Content-Length
defines the framed body: transport bytes after that body are not part of the
download and are never appended to saved output.

URL validation, safe redirects (maximum five), DNS pinning, no-proxy behavior,
identity encoding, header limits, redacted diagnostics, and connection timeout
all reuse the same implementation as `RemoteSource`. Opening and **each chunk
read** have a 30-second deadline. There is no 30-second whole-download deadline:
large saves can take longer, while stalled reads remain bounded. Cancellation
or a read error poisons the download and releases its response. Dropping the
download closes the body; dropping just a `next_chunk` future preserves its
cursor and pending bytes for a retry.

Unknown-length/chunked-only origins are explicitly unsupported, rather than
inventing a length or weakening the atomic saver's disk-space/accounting rules.
Supporting those requires a separately designed unknown-length saving path.
`tests/download.rs` covers full-GET saving with loopback origins, without public
network access.
