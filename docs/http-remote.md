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
