use kadr_storage::rutracker_credentials::{self as vault, Credentials};
use std::{sync::Mutex, thread};
use tokio::sync::{MappedMutexGuard, Mutex as AsyncMutex, MutexGuard};

use super::*;

const LOGIN_REQUIRED: &str =
    "У раздачи нет доступного magnet. Войдите в RuTracker, чтобы скачать .torrent.";
const SEARCH_LOGIN_REQUIRED: &str =
    "RuTracker требует вход для поиска раздач. Войдите, чтобы продолжить поиск.";

#[derive(Clone, Default)]
pub(super) struct Vault(Arc<Mutex<()>>);

impl Vault {
    async fn call<T: Send + 'static>(
        &self,
        cancel: &CancellationToken,
        operation: impl FnOnce() -> kadr_storage::Result<T> + Send + 'static,
    ) -> Option<kadr_storage::Result<T>> {
        // An OS prompt may outlive this request. Do not block the engine or its
        // runtime's shutdown on it, and allow at most one vault operation at a time.
        let gate = self.0.clone();
        let stop = cancel.clone();
        let (output, result) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("kadr-credential-store".into())
            .spawn(move || {
                if let Ok(_guard) = gate.try_lock()
                    && !stop.is_cancelled()
                {
                    let _ = output.send(operation());
                }
            })
            .ok()?;
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            output = result => output.ok(),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Account(Arc<AsyncMutex<Option<rutracker::Client>>>);

impl Account {
    async fn client(
        &self,
        cancel: &CancellationToken,
    ) -> Result<MappedMutexGuard<'_, rutracker::Client>, rutracker::Error> {
        let mut session = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(rutracker::Error::Cancelled),
            session = self.0.lock() => session,
        };
        if session.is_none() {
            *session = Some(rutracker::Client::new()?);
        }
        Ok(MutexGuard::map(session, |session| {
            session.as_mut().expect("client initialized")
        }))
    }

    async fn sign_in_saved(
        &self,
        vault: &Vault,
        login_prompt: &'static str,
        cancel: &CancellationToken,
    ) -> Result<(), PrepareError> {
        let loaded = vault.call(cancel, vault::load).await;
        if cancel.is_cancelled() {
            return Err(rutracker::Error::Cancelled.into());
        }
        let credentials = match loaded {
            Some(Ok(Some(credentials))) => credentials,
            Some(Ok(None)) => return Err(PrepareError::Login(login_prompt)),
            _ => {
                return Err(PrepareError::Login(
                    "Не удалось прочитать пароль из системного хранилища. Войдите в RuTracker вручную.",
                ));
            }
        };
        self.client(cancel)
            .await?
            .login(&credentials.username, &credentials.password, cancel)
            .await?;
        Ok(())
    }

    pub(super) async fn search(
        &self,
        vault: &Vault,
        media: &Media,
        episode: Option<&Episode>,
        cancel: &CancellationToken,
    ) -> Result<Vec<rutracker::Offer>, PrepareError> {
        let result = self
            .client(cancel)
            .await?
            .search(media, episode, cancel)
            .await;
        if matches!(result, Err(rutracker::Error::AuthenticationRequired)) {
            self.client(cancel).await?.logout();
            self.sign_in_saved(vault, SEARCH_LOGIN_REQUIRED, cancel)
                .await?;
            return match self
                .client(cancel)
                .await?
                .search(media, episode, cancel)
                .await
            {
                Err(rutracker::Error::AuthenticationRequired) => {
                    Err(PrepareError::Login(SEARCH_LOGIN_REQUIRED))
                }
                result => result.map_err(Into::into),
            };
        }
        result.map_err(Into::into)
    }
}

#[derive(Clone)]
pub(super) enum Pending {
    Discovery,
    Source(SelectionRequest),
}

#[derive(Clone)]
pub(super) struct SelectionRequest {
    topic_id: u64,
    restore: Option<Restore>,
}

#[derive(Clone)]
struct Restore {
    info_hash: String,
    file_id: Option<usize>,
    target: PlaybackTarget,
}

impl SelectionRequest {
    pub(super) fn new(topic_id: u64) -> Self {
        Self {
            topic_id,
            restore: None,
        }
    }

    pub(super) fn restore(
        topic_id: u64,
        info_hash: String,
        file_id: Option<usize>,
        target: PlaybackTarget,
    ) -> Self {
        Self {
            topic_id,
            restore: Some(Restore {
                info_hash,
                file_id,
                target,
            }),
        }
    }

    fn check_hash(&self, info_hash: &str) -> Result<(), PrepareError> {
        if self
            .restore
            .as_ref()
            .is_some_and(|saved| saved.info_hash != info_hash)
        {
            return Err(PrepareError::failed(
                "Содержимое раздачи изменилось. Для просмотра выберите источник заново.",
            ));
        }
        Ok(())
    }
}

pub(super) enum PrepareError {
    Login(&'static str),
    Failed(String),
}

impl PrepareError {
    fn failed(message: &str) -> Self {
        Self::Failed(message.into())
    }
}

impl From<rutracker::Error> for PrepareError {
    fn from(error: rutracker::Error) -> Self {
        match error {
            rutracker::Error::AuthenticationRequired => Self::Login(LOGIN_REQUIRED),
            rutracker::Error::Authorization => Self::Login(
                "RuTracker не принял вход. Проверьте данные. Если сайт требует CAPTCHA, используйте ручное открытие .torrent.",
            ),
            error => Self::Failed(format!("RuTracker: {error}")),
        }
    }
}

impl Worker {
    pub(super) fn reset_tracker_prompt(&mut self) {
        if self.pending_tracker.take().is_some() {
            self.tracker_login(None, false);
        }
    }

    fn tracker_login(&self, message: Option<&str>, pending: bool) {
        self.send(Event::RuTrackerLogin {
            message: message.map(str::to_owned),
            pending,
        });
    }

    pub(super) fn tracker_allowed(&self) -> bool {
        self.settings.catalog_enabled && self.settings.network.torrents_enabled && !self.headless
    }

    pub(super) fn request_tracker_discovery_login(&mut self, message: &str) {
        self.pending_tracker = Some(Pending::Discovery);
        self.tracker_login(Some(message), false);
    }

    pub(super) async fn select_tracker(
        &mut self,
        request: SelectionRequest,
        cancel: CancellationToken,
    ) {
        self.reset_tracker_prompt();
        self.selection = None;
        if !self.episode_explicit {
            self.episode = None;
        }
        self.send(Event::TorrentFiles(Vec::new()));
        self.send(Event::SourceSelected {
            kind: None,
            title: String::new(),
            playable: false,
            suggested_filename: String::new(),
        });
        self.pending_tracker = Some(Pending::Source(request));
        self.load_tracker_selection(true, cancel).await;
    }

    pub(super) async fn load_tracker_selection(
        &mut self,
        try_magnet_and_saved_login: bool,
        cancel: CancellationToken,
    ) {
        let Some(Pending::Source(request)) = self.pending_tracker.clone() else {
            return;
        };
        if !self.tracker_allowed() {
            self.send(Event::SourcesState(AsyncState::Error(
                "Для RuTracker включите сетевой каталог и BitTorrent в настройках.".into(),
            )));
            return;
        }
        self.send(Event::SourcesState(AsyncState::Loading));
        let result = self
            .prepare_tracker(&request, try_magnet_and_saved_login, &cancel)
            .await;
        if cancel.is_cancelled() {
            return;
        }
        match result {
            Ok((kind, prepared)) => {
                self.reset_tracker_prompt();
                let (file_id, target) = request.restore.map_or((None, None), |restore| {
                    (restore.file_id, Some(restore.target))
                });
                self.finish_selection(Selection {
                    origin: Origin::RuTracker {
                        topic_id: request.topic_id,
                        kind,
                        prepared,
                    },
                    file_id,
                    title: "RuTracker · BitTorrent".into(),
                    filename: "video.mp4".into(),
                    target,
                });
                self.status(
                    Level::Info,
                    "Метаданные RuTracker получены. Проверьте выбранный видеофайл.",
                );
            }
            Err(PrepareError::Login(message)) => {
                self.publish_source_offers();
                self.tracker_login(Some(message), false);
            }
            Err(PrepareError::Failed(message)) => {
                self.tracker_login(None, false);
                self.send(Event::SourcesState(AsyncState::Error(message)));
            }
        }
    }

    async fn prepare_tracker(
        &self,
        request: &SelectionRequest,
        try_magnet_and_saved_login: bool,
        cancel: &CancellationToken,
    ) -> Result<(SourceKind, PreparedTorrent), PrepareError> {
        if try_magnet_and_saved_login {
            let magnet = match self
                .tracker
                .client(cancel)
                .await?
                .magnet(request.topic_id, cancel)
                .await
            {
                Ok(magnet) => magnet,
                Err(rutracker::Error::AuthenticationRequired) => None,
                Err(error) => return Err(error.into()),
            };
            if let Some(source) = magnet.and_then(|uri| ManualSource::parse_link(&uri).ok())
                && let Some(uri) = source.magnet_uri()
            {
                let options = self
                    .torrent_options()
                    .map_err(|error| PrepareError::Failed(error.to_string()))?;
                let prepared = PreparedTorrent::from_magnet(uri, options, cancel.clone()).await
                    .map_err(|_| PrepareError::failed(
                        "Не удалось получить magnet-метаданные RuTracker. Проверьте доступность пиров и повторите.",
                    ))?;
                request.check_hash(&prepared.info_hash)?;
                return Ok((SourceKind::Magnet, prepared));
            }
        }

        let downloaded = self
            .tracker
            .client(cancel)
            .await?
            .download(request.topic_id, cancel)
            .await;
        let bytes = match downloaded {
            Err(rutracker::Error::AuthenticationRequired) if try_magnet_and_saved_login => {
                self.tracker.client(cancel).await?.logout();
                self.tracker
                    .sign_in_saved(&self.tracker_vault, LOGIN_REQUIRED, cancel)
                    .await?;
                self.tracker
                    .client(cancel)
                    .await?
                    .download(request.topic_id, cancel)
                    .await?
            }
            result => result?,
        };
        let prepared = PreparedTorrent::from_bytes(bytes.into())
            .map_err(|_| PrepareError::failed("RuTracker вернул некорректный файл .torrent."))?;
        request.check_hash(&prepared.info_hash)?;
        Ok((SourceKind::TorrentFile, prepared))
    }

    pub(super) async fn sign_in_tracker(
        &mut self,
        username: String,
        password: String,
        cancel: CancellationToken,
    ) {
        if self.pending_tracker.is_none() || !self.tracker_allowed() {
            self.tracker_login(None, false);
            return;
        }
        self.tracker_login(Some("Вход в RuTracker…"), true);
        let credentials = Credentials { username, password };
        let result = match self.tracker.client(&cancel).await {
            Ok(mut client) => {
                client
                    .login(&credentials.username, &credentials.password, &cancel)
                    .await
            }
            Err(error) => Err(error),
        };
        if cancel.is_cancelled() {
            self.tracker_login(None, false);
            return;
        }
        if let Err(error) = result {
            match PrepareError::from(error) {
                PrepareError::Login(message) => self.tracker_login(Some(message), false),
                PrepareError::Failed(message) => self.tracker_login(Some(&message), false),
            }
            return;
        }
        let write = cancel.clone();
        let saved = matches!(
            self.tracker_vault
                .call(&cancel, move || {
                    vault::store_if(&credentials, || !write.is_cancelled())
                })
                .await,
            Some(Ok(()))
        );
        if cancel.is_cancelled() {
            self.tracker_login(None, false);
            return;
        }
        self.tracker_login(None, false);
        if matches!(self.pending_tracker, Some(Pending::Discovery)) {
            self.reset_tracker_prompt();
            self.find_sources(cancel);
        } else {
            self.load_tracker_selection(false, cancel).await;
        }
        if !saved {
            self.status(
                Level::Warning,
                "Вход выполнен, но системное хранилище не сохранило пароль. При следующем запуске потребуется вход.",
            );
        }
    }

    pub(super) async fn forget_tracker(&mut self, cancel: CancellationToken) {
        self.reset_tracker_prompt();
        self.tracker_search.cancel();
        self.tracker = Account::default();
        let result = self.tracker_vault.call(&cancel, vault::forget).await;
        if cancel.is_cancelled() {
            return;
        }
        match result {
            Some(Ok(())) => self.status(
                Level::Info,
                "Вход RuTracker сброшен; сохранённый пароль удалён.",
            ),
            _ => self.status(
                Level::Error,
                "Не удалось удалить пароль. Если открыт системный диалог, закройте его и повторите.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_a_topic_does_not_reuse_file_selection_after_replacement() {
        let saved = SelectionRequest::restore(
            42,
            "0123456789abcdef0123456789abcdef01234567".into(),
            Some(2),
            PlaybackTarget {
                media_id: MediaId::new("test:film").unwrap(),
                episode: None,
            },
        );
        assert!(
            saved
                .check_hash("0123456789abcdef0123456789abcdef01234567")
                .is_ok()
        );
        assert!(matches!(
            saved.check_hash("1123456789abcdef0123456789abcdef01234567"),
            Err(PrepareError::Failed(_))
        ));
    }

    #[tokio::test]
    async fn cancelling_vault_wait_keeps_worker_responsive_without_overlapping_os_prompts() {
        let vault = Vault::default();
        let cancel = CancellationToken::new();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (operation, ()) = tokio::join!(
            vault.call(&cancel, move || {
                started.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            }),
            async {
                started_rx.await.unwrap();
                cancel.cancel();
            }
        );
        assert!(operation.is_none());
        let another = vault.call(&CancellationToken::new(), || Ok(())).await;
        assert!(another.is_none(), "an OS prompt is still in progress");
        release.send(()).unwrap();
    }
}
