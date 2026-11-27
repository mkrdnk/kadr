use kadr_storage::rutracker_credentials::{self as vault, Credentials};
use std::{future::Future, sync::Mutex, thread};
use tokio::sync::{MappedMutexGuard, Mutex as AsyncMutex, MutexGuard};

use super::*;

const LOGIN_REQUIRED: &str =
    "RuTracker требует вход для доступа к раздаче. После входа kadr проверит её вложение.";
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

#[derive(Clone)]
pub(super) struct Account {
    session: Arc<AsyncMutex<Option<rutracker::Client>>>,
    proxy: Option<String>,
}

impl Account {
    pub(super) fn new(proxy: Option<String>) -> Self {
        Self {
            session: Arc::new(AsyncMutex::new(None)),
            proxy,
        }
    }

    async fn client(
        &self,
        cancel: &CancellationToken,
    ) -> Result<MappedMutexGuard<'_, rutracker::Client>, rutracker::Error> {
        let mut session = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(rutracker::Error::Cancelled),
            session = self.session.lock() => session,
        };
        if session.is_none() {
            *session = Some(rutracker::Client::new_with_proxy(self.proxy.as_deref())?);
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

    pub(super) async fn lookup_magnet(
        &self,
        topic_id: u64,
        cancel: &CancellationToken,
    ) -> Result<Option<String>, rutracker::Error> {
        self.client(cancel).await?.magnet(topic_id, cancel).await
    }

    pub(super) async fn search_quiet(
        &self,
        query: &MediaQuery,
        cancel: &CancellationToken,
    ) -> Result<rutracker::SearchOutcome, rutracker::Error> {
        self.client(cancel)
            .await?
            .search_outcome(query, cancel)
            .await
    }

    pub(super) async fn search_after_login_required(
        &self,
        vault: &Vault,
        query: &MediaQuery,
        cancel: &CancellationToken,
    ) -> Result<rutracker::SearchOutcome, PrepareError> {
        self.client(cancel).await?.logout();
        self.sign_in_saved(vault, SEARCH_LOGIN_REQUIRED, cancel)
            .await?;
        match self.search_quiet(query, cancel).await {
            Err(rutracker::Error::AuthenticationRequired) => {
                Err(PrepareError::Login(SEARCH_LOGIN_REQUIRED))
            }
            result => result.map_err(Into::into),
        }
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
    magnet: Option<String>,
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
            magnet: None,
            restore: None,
        }
    }

    pub(super) fn with_magnet(topic_id: u64, magnet: String) -> Self {
        Self {
            magnet: Some(magnet),
            ..Self::new(topic_id)
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
            magnet: None,
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
            rutracker::Error::Timeout => Self::failed(
                "RuTracker не ответил вовремя. Проверьте доступ к сайту и адрес прокси в настройках сети.",
            ),
            rutracker::Error::Network => Self::failed(
                "Не удалось подключиться к RuTracker. Проверьте доступ к сайту и настройки прокси.",
            ),
            rutracker::Error::Http(403) => Self::failed(
                "Не удалось открыть RuTracker: доступ запрещён. Проверьте доступ к сайту и настройки прокси.",
            ),
            rutracker::Error::InvalidResponse => Self::failed(
                "Не удалось разобрать ответ RuTracker: сайт вернул неожиданные данные.",
            ),
            rutracker::Error::RateLimited => {
                Self::failed("RuTracker ограничил запросы. Повторите поиск позже.")
            }
            error => Self::Failed(format!("RuTracker: {error}")),
        }
    }
}

enum SelectionSource {
    Magnet(String),
    Torrent(Vec<u8>),
}

// Keep authentication and fallback decisions shared by the real transport and
// offline scripts. In particular, a failed topic lookup is never an absent magnet.
async fn load_selection_source<L, LF, S, SF, D, DF>(
    request: &SelectionRequest,
    mut try_saved_login: bool,
    mut lookup: L,
    mut sign_in: S,
    mut download: D,
) -> Result<SelectionSource, PrepareError>
where
    L: FnMut() -> LF,
    LF: Future<Output = Result<Option<String>, rutracker::Error>>,
    S: FnMut() -> SF,
    SF: Future<Output = Result<(), PrepareError>>,
    D: FnMut() -> DF,
    DF: Future<Output = Result<Vec<u8>, rutracker::Error>>,
{
    if let Some(magnet) = &request.magnet {
        return Ok(SelectionSource::Magnet(magnet.clone()));
    }
    loop {
        match lookup().await {
            Ok(Some(magnet)) => return Ok(SelectionSource::Magnet(magnet)),
            Ok(None) => match download().await {
                Ok(bytes) => return Ok(SelectionSource::Torrent(bytes)),
                Err(rutracker::Error::AuthenticationRequired) if try_saved_login => {}
                Err(error) => return Err(error.into()),
            },
            Err(rutracker::Error::AuthenticationRequired) if try_saved_login => {}
            Err(error) => return Err(error.into()),
        }
        try_saved_login = false;
        sign_in().await?;
        // Login may reveal a magnet even when the anonymous topic had none.
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
        self.source_preferences.rutracker_enabled
            && self.settings.catalog_enabled
            && self.settings.network.torrents_enabled
            && !self.headless
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
        try_saved_login: bool,
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
            .prepare_tracker(&request, try_saved_login, &cancel)
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
        try_saved_login: bool,
        cancel: &CancellationToken,
    ) -> Result<(SourceKind, PreparedTorrent), PrepareError> {
        let source = load_selection_source(
            request,
            try_saved_login,
            || self.tracker.lookup_magnet(request.topic_id, cancel),
            || async {
                self.tracker.client(cancel).await?.logout();
                self.tracker
                    .sign_in_saved(&self.tracker_vault, LOGIN_REQUIRED, cancel)
                    .await
            },
            || async {
                self.tracker
                    .client(cancel)
                    .await?
                    .download(request.topic_id, cancel)
                    .await
            },
        )
        .await?;
        let (kind, prepared) = match source {
            SelectionSource::Magnet(magnet) => {
                let source = ManualSource::parse_link(&magnet)
                    .map_err(|_| PrepareError::from(rutracker::Error::InvalidResponse))?;
                let uri = source
                    .magnet_uri()
                    .ok_or(rutracker::Error::InvalidResponse)?;
                let options = self
                    .torrent_options()
                    .map_err(|error| PrepareError::Failed(error.to_string()))?;
                let prepared = PreparedTorrent::from_magnet(uri, options, cancel.clone()).await
                    .map_err(|_| PrepareError::failed(
                        "Не удалось получить magnet-метаданные RuTracker. Проверьте доступность пиров и повторите.",
                    ))?;
                (SourceKind::Magnet, prepared)
            }
            SelectionSource::Torrent(bytes) => {
                let prepared = PreparedTorrent::from_bytes(bytes.into()).map_err(|_| {
                    PrepareError::failed("RuTracker вернул некорректный файл .torrent.")
                })?;
                (SourceKind::TorrentFile, prepared)
            }
        };
        request.check_hash(&prepared.info_hash)?;
        Ok((kind, prepared))
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
                PrepareError::Failed(message) => {
                    self.tracker_login(None, false);
                    self.send(Event::SourcesState(AsyncState::Error(message)));
                }
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
            self.resolve_sources(cancel, true);
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
        self.discovery.clear();
        self.publish_source_offers();
        if self.storage.clear_provider_results("rutracker").is_err() {
            self.status(Level::Warning, "Не удалось удалить кэш поиска RuTracker.");
        }
        self.tracker = Account::new(self.settings.catalog_proxy.clone());
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
    use std::{cell::RefCell, collections::VecDeque, future::ready};

    const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";

    // Neither scripts nor their results implement Debug: assertion failures must
    // not print a cached URI, downloaded bytes or credentials.
    enum Step {
        Lookup(Result<Option<String>, rutracker::Error>),
        Login(Result<(), PrepareError>),
        Download(Result<Vec<u8>, rutracker::Error>),
    }

    struct Script(RefCell<VecDeque<Step>>);

    impl Script {
        fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self(RefCell::new(steps.into_iter().collect()))
        }

        async fn run(
            &self,
            request: &SelectionRequest,
            try_saved_login: bool,
        ) -> Result<SelectionSource, PrepareError> {
            let result = load_selection_source(
                request,
                try_saved_login,
                || {
                    ready(match self.0.borrow_mut().pop_front() {
                        Some(Step::Lookup(result)) => result,
                        _ => panic!("unexpected topic lookup"),
                    })
                },
                || {
                    ready(match self.0.borrow_mut().pop_front() {
                        Some(Step::Login(result)) => result,
                        _ => panic!("unexpected saved login"),
                    })
                },
                || {
                    ready(match self.0.borrow_mut().pop_front() {
                        Some(Step::Download(result)) => result,
                        _ => panic!("unexpected torrent download"),
                    })
                },
            )
            .await;
            assert!(self.0.borrow().is_empty(), "script was not completed");
            result
        }
    }

    fn assert_magnet(result: Result<SelectionSource, PrepareError>) {
        assert!(matches!(
            result,
            Ok(SelectionSource::Magnet(uri)) if uri == MAGNET
        ));
    }

    fn assert_forbidden(result: Result<SelectionSource, PrepareError>) {
        assert!(matches!(
            result,
            Err(PrepareError::Failed(message))
                if message.starts_with("Не удалось открыть") && message.contains("доступ")
        ));
    }

    #[tokio::test]
    async fn cached_magnet_bypasses_topic_download_and_saved_login() {
        let request = SelectionRequest::with_magnet(42, MAGNET.into());
        assert_eq!(request.topic_id, 42);
        assert!(request.restore.is_none());
        for try_saved_login in [true, false] {
            assert_magnet(Script::new([]).run(&request, try_saved_login).await);
        }
    }

    #[tokio::test]
    async fn uncached_selection_reads_the_topic_including_after_manual_login() {
        for try_saved_login in [true, false] {
            let script = Script::new([Step::Lookup(Ok(Some(MAGNET.into())))]);
            assert_magnet(
                script
                    .run(&SelectionRequest::new(42), try_saved_login)
                    .await,
            );
        }
    }

    #[tokio::test]
    async fn topic_authentication_retries_magnet_after_saved_login_before_download() {
        let script = Script::new([
            Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Ok(())),
            Step::Lookup(Ok(Some(MAGNET.into()))),
        ]);
        assert_magnet(script.run(&SelectionRequest::new(42), true).await);
    }

    #[tokio::test]
    async fn download_authentication_also_retries_the_topic_after_saved_login() {
        let script = Script::new([
            Step::Lookup(Ok(None)),
            Step::Download(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Ok(())),
            Step::Lookup(Ok(Some(MAGNET.into()))),
        ]);
        assert_magnet(script.run(&SelectionRequest::new(42), true).await);
    }

    #[tokio::test]
    async fn only_a_successfully_read_topic_without_magnet_downloads_torrent() {
        for try_saved_login in [true, false] {
            let script = Script::new([
                Step::Lookup(Ok(None)),
                Step::Download(Ok(b"offline torrent fixture".to_vec())),
            ]);
            assert!(matches!(
                script.run(&SelectionRequest::new(42), try_saved_login).await,
                Ok(SelectionSource::Torrent(bytes)) if bytes == b"offline torrent fixture"
            ));
        }
        let script = Script::new([
            Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Ok(())),
            Step::Lookup(Ok(None)),
            Step::Download(Ok(Vec::new())),
        ]);
        assert!(matches!(
            script.run(&SelectionRequest::new(42), true).await,
            Ok(SelectionSource::Torrent(_))
        ));
    }

    #[tokio::test]
    async fn saved_login_is_attempted_at_most_once_for_topic_and_download_combined() {
        for next in [
            vec![Step::Lookup(Err(rutracker::Error::AuthenticationRequired))],
            vec![
                Step::Lookup(Ok(None)),
                Step::Download(Err(rutracker::Error::AuthenticationRequired)),
            ],
        ] {
            let script = Script::new(
                [
                    Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
                    Step::Login(Ok(())),
                ]
                .into_iter()
                .chain(next),
            );
            assert!(matches!(
                script.run(&SelectionRequest::new(42), true).await,
                Err(PrepareError::Login(_))
            ));
        }
    }

    #[tokio::test]
    async fn failed_saved_login_does_not_fall_back_to_torrent() {
        let script = Script::new([
            Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Err(PrepareError::Login(LOGIN_REQUIRED))),
        ]);
        assert!(matches!(
            script.run(&SelectionRequest::new(42), true).await,
            Err(PrepareError::Login(LOGIN_REQUIRED))
        ));
    }

    #[tokio::test]
    async fn manual_login_does_not_trigger_another_saved_login() {
        for steps in [
            vec![Step::Lookup(Err(rutracker::Error::AuthenticationRequired))],
            vec![
                Step::Lookup(Ok(None)),
                Step::Download(Err(rutracker::Error::AuthenticationRequired)),
            ],
        ] {
            assert!(matches!(
                Script::new(steps)
                    .run(&SelectionRequest::new(42), false)
                    .await,
                Err(PrepareError::Login(_))
            ));
        }
    }

    #[tokio::test]
    async fn forbidden_topic_or_download_fails_without_requesting_a_password() {
        for try_saved_login in [true, false] {
            for steps in [
                vec![Step::Lookup(Err(rutracker::Error::Http(403)))],
                vec![
                    Step::Lookup(Ok(None)),
                    Step::Download(Err(rutracker::Error::Http(403))),
                ],
            ] {
                assert_forbidden(
                    Script::new(steps)
                        .run(&SelectionRequest::new(42), try_saved_login)
                        .await,
                );
            }
        }
        let script = Script::new([
            Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Ok(())),
            Step::Lookup(Err(rutracker::Error::Http(403))),
        ]);
        assert_forbidden(script.run(&SelectionRequest::new(42), true).await);
        let script = Script::new([
            Step::Lookup(Err(rutracker::Error::AuthenticationRequired)),
            Step::Login(Err(rutracker::Error::Http(403).into())),
        ]);
        assert_forbidden(script.run(&SelectionRequest::new(42), true).await);
    }

    #[tokio::test]
    async fn failed_topic_lookup_never_becomes_a_torrent_fallback_or_login() {
        for error in [
            rutracker::Error::Cancelled,
            rutracker::Error::InvalidTopic,
            rutracker::Error::Network,
            rutracker::Error::Timeout,
            rutracker::Error::RateLimited,
            rutracker::Error::Http(500),
            rutracker::Error::TooLarge,
            rutracker::Error::InvalidResponse,
        ] {
            let script = Script::new([Step::Lookup(Err(error))]);
            assert!(matches!(
                script.run(&SelectionRequest::new(42), true).await,
                Err(PrepareError::Failed(_))
            ));
        }
    }

    #[tokio::test]
    async fn quiet_prefetch_respects_cancellation_without_creating_a_session() {
        let account = Account::new(None);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            account.lookup_magnet(42, &cancel).await,
            Err(rutracker::Error::Cancelled)
        ));
        let query = MediaQuery {
            media_id: "test:film".into(),
            kind: MediaKind::Movie,
            title: "Film".into(),
            original_title: None,
            year: None,
            external: Default::default(),
            episode: None,
        };
        assert!(matches!(
            account.search_quiet(&query, &cancel).await,
            Err(rutracker::Error::Cancelled)
        ));
        assert!(account.session.lock().await.is_none());
    }

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
