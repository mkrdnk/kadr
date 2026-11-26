//! A narrowly scoped GPUI image client, not a general-purpose URL fetcher.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures_util::{StreamExt, future::BoxFuture};
use gpui::http_client::{AsyncBody, HttpClient, Request, Response, Url, anyhow};
use kadr_engine::Event;
use tokio::sync::Notify;

#[derive(Default)]
struct PolicyState {
    enabled: AtomicBool,
    changed: Notify,
}

/// Desktop-owned policy shared by the controller and GPUI image client.
/// Disabled until the engine confirms settings; each instance is independent.
#[derive(Clone, Default)]
pub struct PosterPolicy(Arc<PolicyState>);

impl PosterPolicy {
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Release);
        if !enabled {
            self.0.changed.notify_waiters();
        }
    }

    pub fn apply_event(&self, event: &Event) {
        match event {
            Event::Ready { settings, .. }
            | Event::PreferencesSaved(settings)
            | Event::PreferencesRejected { settings, .. } => {
                self.set_enabled(settings.catalog_enabled);
            }
            _ => {}
        }
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
    policy: PosterPolicy,
}

impl Posters {
    pub fn new(policy: PosterPolicy) -> std::io::Result<Self> {
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
        Ok(Self {
            runtime,
            client,
            policy,
        })
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
        let policy = self.policy.clone();
        let url = request.uri().to_string();
        let get = request.method() == gpui::http_client::Method::GET;
        let task = self.runtime.spawn(async move {
            let disabled = policy.0.changed.notified();
            tokio::pin!(disabled);
            // Register before checking the shared flag: notify_waiters does
            // not retain a permit for a future that has not been registered.
            disabled.as_mut().enable();
            if !get || !policy.0.enabled.load(Ordering::Acquire) {
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
    use kadr_engine::{Level, Settings};

    #[test]
    fn poster_policy_uses_confirmed_settings_and_is_not_global() {
        let policy = PosterPolicy::default();
        let other = PosterPolicy::default();
        let enabled = || policy.0.enabled.load(Ordering::Acquire);
        assert!(!enabled());
        policy.apply_event(&Event::Ready {
            settings: Settings::default(),
            cache: None,
        });
        assert!(enabled());
        assert!(!other.0.enabled.load(Ordering::Acquire));

        let disabled_settings = Settings {
            catalog_enabled: false,
            ..Settings::default()
        };
        policy.apply_event(&Event::PreferencesSaved(disabled_settings));
        assert!(!enabled());
        policy.apply_event(&Event::Status(Level::Info, "Unrelated feedback".into()));
        assert!(!enabled());
        policy.apply_event(&Event::PreferencesRejected {
            settings: Settings::default(),
            message: "Retain confirmed settings".into(),
        });
        assert!(enabled());
        policy.set_enabled(false);
        assert!(!enabled());
    }

    #[test]
    fn disabling_posters_cancels_registered_requests_even_after_reenable() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let policy = PosterPolicy::default();
            policy.set_enabled(true);
            let disabled = policy.0.changed.notified();
            tokio::pin!(disabled);
            disabled.as_mut().enable();
            policy.set_enabled(false);
            policy.set_enabled(true);
            tokio::time::timeout(Duration::from_secs(1), disabled)
                .await
                .expect("registered request must be cancelled");
        });
    }

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
