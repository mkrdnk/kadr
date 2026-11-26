//! A narrowly scoped GPUI image client, not a general-purpose URL fetcher.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use futures_util::{StreamExt, future::BoxFuture};
use gpui::http_client::{AsyncBody, HttpClient, Request, Response, Url, anyhow};
use kadr_engine::{Event, catalog::is_public_catalog_image as allowed};
use tokio::sync::Notify;

#[path = "poster_cache.rs"]
mod cache;

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

pub struct Posters {
    runtime: Arc<tokio::runtime::Runtime>,
    client: reqwest::Client,
    policy: PosterPolicy,
    cache: Arc<cache::Cache>,
}

impl Posters {
    pub fn new(policy: PosterPolicy) -> std::io::Result<Self> {
        Self::with_cache_directory(policy, &kadr_engine::catalog::artwork_cache_directory()?)
    }

    fn with_cache_directory(
        policy: PosterPolicy,
        directory: &std::path::Path,
    ) -> std::io::Result<Self> {
        let cache = Arc::new(cache::Cache::open(directory)?);
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
            cache,
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
        let cache = self.cache.clone();
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
                let image = cached_image(cache, url.as_str().to_owned(), async {
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
                        if bytes.len() + chunk.len() > cache::MAX_IMAGE_BYTES {
                            return Err(anyhow!("poster too large"));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    cache::Image::new(&mime, bytes).map_err(|_| anyhow!("invalid poster image"))
                })
                .await?;
                image_response(image)
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

async fn cached_image(
    cache: Arc<cache::Cache>,
    url: String,
    fetch: impl std::future::Future<Output = gpui::http_client::Result<cache::Image>>,
) -> gpui::http_client::Result<cache::Image> {
    let cached = {
        let cache = cache.clone();
        let url = url.clone();
        tokio::task::spawn_blocking(move || cache.get(&url))
            .await
            .map_err(|_| anyhow!("poster cache stopped"))?
            .map_err(|_| anyhow!("poster cache unavailable"))?
    };
    if let Some(image) = cached {
        return Ok(image);
    }
    let image = fetch.await?;
    tokio::task::spawn_blocking(move || {
        cache.put(&url, &image)?;
        Ok(image)
    })
    .await
    .map_err(|_| anyhow!("poster cache stopped"))?
    .map_err(|_: std::io::Error| anyhow!("poster cache unavailable"))
}

fn image_response(image: cache::Image) -> gpui::http_client::Result<Response<AsyncBody>> {
    Response::builder()
        .status(200)
        .header("Content-Type", image.mime)
        .body(AsyncBody::from(image.bytes))
        .map_err(|_| anyhow!("invalid poster response"))
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cached_pipeline_persists_before_return_and_does_not_fetch_a_hit() {
        let temp = cache::TestDirectory::new();
        let path = temp.0.join("catalog-images-v1");
        let cache = Arc::new(cache::Cache::open(&path).unwrap());
        let url = "https://media.trakt.tv/movies/1398468/medium/poster.webp";
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let image = cached_image(cache.clone(), url.into(), async {
                Ok(cache::Image::new("image/webp", b"RIFF\x04\0\0\0WEBP".to_vec()).unwrap())
            })
            .await
            .unwrap();
            assert_eq!(cache.get(url).unwrap().unwrap().bytes, image.bytes);
            let reopened = Arc::new(cache::Cache::open(&path).unwrap());
            let hit = cached_image(reopened, url.into(), async {
                panic!("a validated persistent hit must not poll the network future")
            })
            .await
            .unwrap();
            assert_eq!(hit.bytes, image.bytes);
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cache_failures_never_return_uncached_bytes() {
        let temp = cache::TestDirectory::new();
        let path = temp.0.join("catalog-images-v1");
        let cache = Arc::new(cache::Cache::open(&path).unwrap());
        let url = "https://media.trakt.tv/movies/1398468/medium/poster.webp";
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let failed_store = cached_image(cache.clone(), url.into(), async {
                // Corrupting the ownership marker must fail closed, not adopt
                // or clean up files in an unrecognized staging namespace.
                std::fs::write(
                    path.join("staging-v1/kadr-artwork-owned-v1/unrelated"),
                    b"unrelated",
                )
                .unwrap();
                Ok(cache::Image::new("image/webp", b"RIFF\x04\0\0\0WEBP".to_vec()).unwrap())
            })
            .await;
            assert!(failed_store.is_err());
            assert!(cache.get(url).unwrap().is_none());
            assert_eq!(
                std::fs::read(path.join("staging-v1/kadr-artwork-owned-v1/unrelated")).unwrap(),
                b"unrelated"
            );
            std::fs::rename(&path, temp.0.join("moved")).unwrap();
            let failed_lookup = cached_image(cache, url.into(), async {
                panic!("an unavailable cache must fail before network")
            })
            .await;
            assert!(failed_lookup.is_err());
        });
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn http_client_gates_cached_images_on_settings_method_and_origin() {
        let temp = cache::TestDirectory::new();
        let policy = PosterPolicy::default();
        let mut client =
            Posters::with_cache_directory(policy.clone(), &temp.0.join("catalog-images-v1"))
                .unwrap();
        // A broken cache regression must never contact the real CDN in tests.
        client.client = reqwest::Client::builder()
            .no_proxy()
            .resolve("media.trakt.tv", ([127, 0, 0, 1], 9).into())
            .resolve("image.tmdb.org", ([127, 0, 0, 1], 9).into())
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let url = "https://media.trakt.tv/movies/1398468/medium/poster.webp";
        let old_origin = "https://image.tmdb.org/t/p/w500/item.jpg";
        for key in [url, old_origin] {
            client
                .cache
                .put(
                    key,
                    &cache::Image::new("image/webp", b"RIFF\x04\0\0\0WEBP".to_vec()).unwrap(),
                )
                .unwrap();
        }
        assert!(client.cache.get(url).unwrap().is_some());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let request = |url: &str, method| {
            Request::builder()
                .method(method)
                .uri(url)
                .body(AsyncBody::from(Vec::<u8>::new()))
                .unwrap()
        };
        runtime.block_on(async {
            assert!(client.send(request(url, "GET")).await.is_err());
            policy.set_enabled(true);
            let hit = client.send(request(url, "GET")).await.unwrap();
            assert_eq!(hit.status(), 200);
            assert_eq!(hit.headers()["Content-Type"], "image/webp");
            assert!(client.send(request(url, "POST")).await.is_err());
            assert!(client.send(request(old_origin, "GET")).await.is_err());
            policy.set_enabled(false);
            assert!(client.send(request(url, "GET")).await.is_err());
        });
    }

    #[test]
    fn poster_policy_rejects_arbitrary_and_active_origins() {
        assert!(allowed(
            &Url::parse("https://media.trakt.tv/movies/1398468/medium/poster.webp").unwrap()
        ));
        assert!(allowed(
            &Url::parse("https://media.trakt.tv/shows/1398468/medium/fanart.webp").unwrap()
        ));
        assert!(allowed(
            &Url::parse(
                "https://walter-r2.trakt.tv/images/movies/000/001/234/posters/medium/a.jpg.webp"
            )
            .unwrap()
        ));
        for input in [
            "http://media.trakt.tv/movies/1398468/medium/poster.webp",
            "https://media.trakt.tv.evil.test/movies/1398468/medium/poster.webp",
            "https://user:secret@media.trakt.tv/movies/1398468/medium/poster.webp",
            "https://127.0.0.1/a",
            "https://media.trakt.tv:8443/movies/1398468/medium/poster.webp",
            "https://image.tmdb.org/t/p/w500/a.jpg",
            "https://archive.org/services/img/item",
        ] {
            assert!(!allowed(&Url::parse(input).unwrap()));
        }
    }
}
