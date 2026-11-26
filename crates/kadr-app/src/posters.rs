//! A narrowly scoped GPUI image client, not a general-purpose URL fetcher.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures_util::{StreamExt, future::BoxFuture};
use gpui::http_client::{AsyncBody, HttpClient, Request, Response, Url, anyhow};
use tokio::sync::Notify;

static ENABLED: AtomicBool = AtomicBool::new(false);
static CHANGED: OnceLock<Notify> = OnceLock::new();

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
    if !enabled {
        CHANGED.get_or_init(Notify::new).notify_waiters();
    }
}

fn allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.host_str().is_some_and(|host| {
            host == "archive.org" || host.ends_with(".archive.org") || host == "static.tvmaze.com"
        })
}

pub struct Posters {
    runtime: Arc<tokio::runtime::Runtime>,
    client: reqwest::Client,
}

impl Posters {
    pub fn new() -> std::io::Result<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()?,
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .user_agent("kadr/0.1 (local desktop; public catalogue posters)")
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !allowed(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| std::io::Error::other("poster client initialization failed"))?;
        Ok(Self { runtime, client })
    }
}

impl HttpClient for Posters {
    fn type_name(&self) -> &'static str {
        "kadr::Posters"
    }
    fn user_agent(&self) -> Option<&gpui::http_client::http::HeaderValue> {
        None
    }
    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, gpui::http_client::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        let url = request.uri().to_string();
        let get = request.method() == gpui::http_client::Method::GET;
        let task = self.runtime.spawn(async move {
            let disabled = CHANGED.get_or_init(Notify::new).notified();
            tokio::pin!(disabled);
            // Register before checking the shared flag: notify_waiters does
            // not retain a permit for a future that has not been registered.
            disabled.as_mut().enable();
            if !get || !ENABLED.load(Ordering::Acquire) {
                return Err(anyhow!("poster requests are disabled"));
            }
            let url = Url::parse(&url).map_err(|_| anyhow!("invalid poster URL"))?;
            if !allowed(&url) {
                return Err(anyhow!("poster origin is not allowed"));
            }
            let fetch = async {
                let reply = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| anyhow!("poster unavailable"))?;
                if !reply.status().is_success() {
                    return Err(anyhow!("poster unavailable"));
                }
                let mime = reply
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !matches!(
                    mime.as_str(),
                    "image/jpeg" | "image/png" | "image/webp" | "image/gif"
                ) {
                    return Err(anyhow!("unsupported poster format"));
                }
                let mut chunks = reply.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(chunk) = chunks.next().await {
                    let chunk = chunk.map_err(|_| anyhow!("poster read failed"))?;
                    if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                        return Err(anyhow!("poster too large"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Response::builder()
                    .status(200)
                    .header("Content-Type", mime)
                    .body(AsyncBody::from(bytes))
                    .map_err(|_| anyhow!("invalid poster response"))
            };
            tokio::select! {
                biased;
                () = &mut disabled => Err(anyhow!("poster request cancelled")),
                result = fetch => result,
            }
        });
        Box::pin(async move { task.await.map_err(|_| anyhow!("poster request stopped"))? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn poster_policy_rejects_arbitrary_and_active_origins() {
        assert!(allowed(
            &Url::parse("https://archive.org/services/img/item").unwrap()
        ));
        assert!(allowed(
            &Url::parse("https://static.tvmaze.com/uploads/images/medium.jpg").unwrap()
        ));
        for input in [
            "http://archive.org/a",
            "https://archive.org.evil.test/a",
            "https://user:secret@archive.org/a",
            "https://127.0.0.1/a",
            "https://archive.org:8443/a",
        ] {
            assert!(!allowed(&Url::parse(input).unwrap()));
        }
    }
}
