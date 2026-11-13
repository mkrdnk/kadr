//! One owned worker for blocking SQLite IO and Tokio player IO, never the render thread.

use std::{
    ffi::OsString,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use kadr_core::source::ManualSource;
use kadr_media::players::{
    PlayRequest, PlayerKind, PlayerOptions, PlayerSession, PlayerSnapshot, Telemetry,
};
use kadr_storage::{
    AppPaths, MediaId, PlaybackProgress, PlaybackTarget, PlayerPreference, ProgressOrigin,
    Settings, SourceId, Storage,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum Event {
    Ready(PlayerPreference),
    PreferenceSaved(PlayerPreference),
    Status(Level, String),
    PlaybackActive(bool),
    LibraryCount(usize),
}

#[derive(Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

enum Command {
    SetPlayer(PlayerPreference),
    Play {
        source: ManualSource,
        stop: CancellationToken,
        resume: bool,
    },
}

pub struct Backend {
    commands: mpsc::Sender<Command>,
    shutdown: CancellationToken,
    playback_stop: Option<CancellationToken>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Backend {
    pub fn start() -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        Self::start_with(None, false)
    }

    fn start_with(
        paths: Option<AppPaths>,
        headless: bool,
    ) -> std::io::Result<(Self, UnboundedReceiver<Event>)> {
        let (commands, receiver) = mpsc::channel(16);
        let (events, output) = unbounded();
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let thread = thread::Builder::new()
            .name("kadr-media".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        runtime.block_on(work(receiver, events, worker_shutdown, paths, headless))
                    }
                    Err(_) => status(&events, Level::Error, "Не удалось запустить медиаядро."),
                }
            })?;
        Ok((
            Self {
                commands,
                shutdown,
                playback_stop: None,
                thread: Some(thread),
            },
            output,
        ))
    }

    pub fn set_player(&self, player: PlayerPreference) -> bool {
        self.commands.try_send(Command::SetPlayer(player)).is_ok()
    }

    pub fn play(&mut self, source: ManualSource, resume: bool) -> bool {
        let stop = self.shutdown.child_token();
        if self
            .commands
            .try_send(Command::Play {
                source,
                stop: stop.clone(),
                resume,
            })
            .is_err()
        {
            return false;
        }
        self.playback_stop = Some(stop);
        true
    }

    pub fn stop(&self) {
        if let Some(stop) = &self.playback_stop {
            stop.cancel();
        }
    }

    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Called after the platform event loop exits, outside GPUI's 100ms quit budget.
    pub fn join(&mut self) -> bool {
        self.cancel();
        self.thread
            .take()
            .is_none_or(|thread| thread.join().is_ok())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn status(events: &UnboundedSender<Event>, level: Level, message: impl Into<String>) {
    let _ = events.unbounded_send(Event::Status(level, message.into()));
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

struct Active {
    player: PlayerSession,
    stop: CancellationToken,
    target: PlaybackTarget,
    source_id: SourceId,
    origin: ProgressOrigin,
    progress: ProgressState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgressState {
    Unavailable,
    Saved,
    SaveFailed,
}

#[derive(Clone, Copy)]
enum End {
    Stopped,
    Exited(bool),
    ConnectionLost,
    Shutdown,
}

async fn work(
    mut commands: mpsc::Receiver<Command>,
    events: UnboundedSender<Event>,
    shutdown: CancellationToken,
    paths: Option<AppPaths>,
    headless: bool,
) {
    // These synchronous calls run exclusively on this worker.
    let storage = paths
        .map_or_else(AppPaths::discover, Ok)
        .and_then(|paths| Storage::open_default(&paths));
    let mut storage = match storage {
        Ok(storage) => storage,
        Err(_) => {
            status(
                &events,
                Level::Error,
                "Не удалось открыть хранилище. Данные не сброшены; проверьте доступ и версию БД.",
            );
            return;
        }
    };
    let mut settings = match storage.load_settings() {
        Ok(settings) => settings,
        Err(_) => {
            status(
                &events,
                Level::Error,
                "Не удалось прочитать настройки. Данные не заменены значениями по умолчанию.",
            );
            return;
        }
    };
    let _ = events.unbounded_send(Event::Ready(settings.default_player));
    publish_history(&storage, &events);
    let mut active: Option<Active> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let playback_stop = active
            .as_ref()
            .map(|active| active.stop.clone())
            .unwrap_or_else(|| shutdown.clone());
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            () = playback_stop.cancelled(), if active.is_some() => {
                finish(active.take(), &events, End::Stopped).await;
            }
            command = commands.recv() => match command {
                None => break,
                Some(Command::SetPlayer(player)) => {
                    let updated = Settings { default_player: player, ..settings.clone() };
                    match storage.save_settings(&updated) {
                        Ok(()) => {
                            settings = updated;
                            let _ = events.unbounded_send(Event::PreferenceSaved(player));
                        }
                        Err(_) => status(&events, Level::Error, "Не удалось сохранить выбор плеера."),
                    }
                }
                Some(Command::Play { source, stop, resume }) => {
                    if active.is_some() {
                        status(&events, Level::Warning, "Сначала остановите активный плеер.");
                        continue;
                    }
                    active = launch(
                        source, stop, resume, &settings, &mut storage, &events, headless,
                    ).await;
                    publish_history(&storage, &events);
                }
            },
            _ = tick.tick(), if active.is_some() => {
                let current = active.as_mut().expect("guarded active session");
                match current.player.snapshot(&current.stop).await {
                    Ok(snapshot) => {
                        match save_snapshot(&mut storage, current, &snapshot) {
                            Ok(true) => current.progress = ProgressState::Saved,
                            Ok(false) => {},
                            Err(_) => {
                                if current.progress != ProgressState::SaveFailed {
                                    status(&events, Level::Warning, "Плеер работает, но прогресс не удалось сохранить.");
                                }
                                current.progress = ProgressState::SaveFailed;
                            }
                        }
                        if let Some(exit) = snapshot.exit {
                            finish(active.take(), &events, End::Exited(exit.success())).await;
                        }
                    }
                    Err(_) => {
                        let cancelled = current.stop.is_cancelled();
                        finish(active.take(), &events,
                            if cancelled { End::Stopped } else { End::ConnectionLost }).await;
                    }
                }
            }
        }
    }
    finish(active.take(), &events, End::Shutdown).await;
}

async fn launch(
    source: ManualSource,
    stop: CancellationToken,
    restore_position: bool,
    settings: &Settings,
    storage: &mut Storage,
    events: &UnboundedSender<Event>,
    headless: bool,
) -> Option<Active> {
    let Some(url) = source.http_url() else {
        status(
            events,
            Level::Warning,
            "Источник проверен, но BitTorrent ещё не подключён к desktop-сессии. Сеть не запущена.",
        );
        let _ = events.unbounded_send(Event::PlaybackActive(false));
        return None;
    };
    if settings.default_player == PlayerPreference::Browser {
        // Opening a remote URL is intentionally not the promised loopback-browser workflow.
        status(
            events,
            Level::Warning,
            "Браузер пока не подключён: требуется локальный поток. Выберите mpv или VLC. Автопрогресс браузера недоступен.",
        );
        let _ = events.unbounded_send(Event::PlaybackActive(false));
        return None;
    }
    let source_id = match SourceId::new(source.id().as_str().to_owned()) {
        Ok(id) => id,
        Err(_) => return None,
    };
    let target = PlaybackTarget {
        media_id: MediaId::new(source.id().as_str().to_owned()).expect("core ID is storage-safe"),
        episode: None,
    };
    let resume = match storage.load_progress(&target) {
        Ok(progress) => progress
            .filter(|progress| {
                restore_position && !progress.completed && progress.position_seconds >= 1.0
            })
            .and_then(|progress| Duration::try_from_secs_f64(progress.position_seconds).ok()),
        Err(_) => {
            status(
                events,
                Level::Error,
                "Не удалось прочитать прогресс; запуск не выполнен.",
            );
            let _ = events.unbounded_send(Event::PlaybackActive(false));
            return None;
        }
    };
    let (kind, origin) = match settings.default_player {
        PlayerPreference::Mpv => (PlayerKind::Mpv, ProgressOrigin::Mpv),
        PlayerPreference::Vlc => (PlayerKind::Vlc, ProgressOrigin::Vlc),
        PlayerPreference::Browser => unreachable!("browser handled above"),
    };
    status(events, Level::Info, "Запуск внешнего плеера…");
    // The token inherits shutdown. Await cancellation cleanup instead of dropping
    // a partially completed launch after the external process has been spawned.
    let player = match PlayerSession::launch(
        kind,
        PlayRequest {
            media: OsString::from(url.as_str()),
            resume,
        },
        PlayerOptions {
            headless,
            ..PlayerOptions::for_player(kind)
        },
        &stop,
    )
    .await
    {
        Ok(player) => Some(player),
        Err(error) => {
            let message = if stop.is_cancelled() {
                "Запуск плеера отменён."
            } else if error.kind() == std::io::ErrorKind::NotFound {
                "Плеер не найден. Установите выбранный mpv/VLC или измените выбор в настройках."
            } else {
                "Не удалось запустить плеер или установить управление. Проверьте установку mpv/VLC."
            };
            status(events, Level::Error, message);
            None
        }
    };
    let Some(player) = player else {
        let _ = events.unbounded_send(Event::PlaybackActive(false));
        return None;
    };
    let history_failed = storage
        .record_opened(&target.media_id, &source_id, now())
        .is_err();
    let _ = events.unbounded_send(Event::PlaybackActive(true));
    status(
        events,
        if kind == PlayerKind::Vlc {
            Level::Warning
        } else {
            Level::Info
        },
        if kind == PlayerKind::Vlc {
            "VLC открыт. Надёжная телеметрия RC недоступна: новый прогресс не записывается."
        } else if resume.is_some() {
            "mpv открыт с последней измеренной позиции. Прогресс сохраняется локально."
        } else {
            "mpv открыт. Прогресс сохраняется локально по данным IPC."
        },
    );
    if history_failed {
        status(
            events,
            Level::Warning,
            "Плеер открыт, но историю не удалось сохранить. Прогресс отслеживается только при доступной телеметрии.",
        );
    }
    Some(Active {
        player,
        stop,
        target,
        source_id,
        origin,
        progress: ProgressState::Unavailable,
    })
}

fn save_snapshot(
    storage: &mut Storage,
    active: &Active,
    snapshot: &PlayerSnapshot,
) -> kadr_storage::Result<bool> {
    let Telemetry::Seconds(position) = snapshot.position else {
        return Ok(false);
    };
    let duration = match snapshot.duration {
        Telemetry::Seconds(duration) => Some(duration),
        _ => None,
    };
    storage.save_progress(&PlaybackProgress {
        target: active.target.clone(),
        source_id: active.source_id.clone(),
        position_seconds: position,
        duration_seconds: duration,
        updated_at: now(),
        // A successful process exit alone is not proof that the video reached EOF.
        completed: false,
        origin: active.origin,
    })?;
    Ok(true)
}

fn end_status(reason: End, progress: ProgressState, stop_failed: bool) -> (Level, String) {
    let (mut level, message) = match reason {
        End::Stopped | End::Shutdown => (Level::Info, "Воспроизведение остановлено."),
        End::Exited(true) => (Level::Info, "Плеер завершил работу."),
        End::Exited(false) => (
            Level::Error,
            "Плеер завершился с ошибкой. Проверьте источник и кодеки.",
        ),
        End::ConnectionLost => (Level::Error, "Связь с плеером потеряна."),
    };
    let mut message = message.to_owned();
    match progress {
        ProgressState::Unavailable => {}
        ProgressState::Saved => message.push_str(" Последняя измеренная позиция сохранена."),
        ProgressState::SaveFailed => {
            if matches!(level, Level::Info) {
                level = Level::Warning;
            }
            message.push_str(" Последний прогресс не удалось сохранить.");
        }
    }
    if stop_failed {
        if matches!(level, Level::Info) {
            level = Level::Warning;
        }
        message.push_str(" Остановка плеера завершилась с ошибкой управления.");
    }
    (level, message)
}

async fn finish(active: Option<Active>, events: &UnboundedSender<Event>, reason: End) {
    if let Some(mut active) = active {
        let stop_failed = active.player.stop().await.is_err();
        let _ = events.unbounded_send(Event::PlaybackActive(false));
        if !matches!(reason, End::Shutdown) {
            let (level, message) = end_status(reason, active.progress, stop_failed);
            status(events, level, message);
        }
    }
}

fn publish_history(storage: &Storage, events: &UnboundedSender<Event>) {
    if let Ok(history) = storage.history(100) {
        let _ = events.unbounded_send(Event::LibraryCount(history.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Arc, time::Instant};

    #[test]
    fn terminal_feedback_never_invents_progress_or_hides_write_failure() {
        let (_, unavailable) = end_status(End::Exited(true), ProgressState::Unavailable, false);
        assert!(!unavailable.contains("сохранена"));
        let (level, failed) = end_status(End::Exited(true), ProgressState::SaveFailed, false);
        assert!(matches!(level, Level::Warning));
        assert!(failed.contains("не удалось сохранить"));
        assert!(!failed.contains("сохранена"));
        let (_, saved) = end_status(End::Stopped, ProgressState::Saved, false);
        assert!(saved.contains("измеренная позиция сохранена"));
        let (level, message) = end_status(End::Stopped, ProgressState::Saved, true);
        assert!(matches!(level, Level::Warning));
        assert!(message.contains("ошибкой управления"));
    }

    fn paths(root: &std::path::Path) -> AppPaths {
        AppPaths::from_roots(root.join("data"), root.join("config"), root.join("cache")).unwrap()
    }

    fn wait_event(
        events: &mut UnboundedReceiver<Event>,
        predicate: impl Fn(&Event) -> bool,
    ) -> Event {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(event) = events.try_recv() {
                if predicate(&event) {
                    return event;
                }
            } else {
                thread::sleep(Duration::from_millis(20));
            }
        }
        panic!("worker did not emit expected event");
    }

    #[test]
    fn worker_persists_preference_and_browser_never_creates_progress() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        let (mut backend, mut events) = Backend::start_with(Some(paths.clone()), true).unwrap();
        wait_event(&mut events, |event| {
            matches!(event, Event::Ready(PlayerPreference::Mpv))
        });
        assert!(backend.set_player(PlayerPreference::Browser));
        wait_event(&mut events, |event| {
            matches!(event, Event::PreferenceSaved(PlayerPreference::Browser))
        });
        let source =
            ManualSource::parse_link("https://example.invalid/video.mp4?private=secret").unwrap();
        let target = PlaybackTarget {
            media_id: MediaId::new(source.id().as_str().to_owned()).unwrap(),
            episode: None,
        };
        assert!(backend.play(source, true));
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(false))
        });
        assert!(backend.join());
        let storage = Storage::open(&paths.database_path()).unwrap();
        assert_eq!(
            storage.load_settings().unwrap().default_player,
            PlayerPreference::Browser
        );
        assert!(storage.load_progress(&target).unwrap().is_none());
        assert!(storage.history(10).unwrap().is_empty());
        let (mut backend, mut events) = Backend::start_with(Some(paths), true).unwrap();
        wait_event(&mut events, |event| {
            matches!(event, Event::Ready(PlayerPreference::Browser))
        });
        assert!(backend.join());
    }

    #[test]
    fn storage_failure_is_reported_without_ready_or_silent_reset() {
        let directory = tempfile::tempdir().unwrap();
        let data_file = directory.path().join("not-a-directory");
        fs::write(&data_file, b"preserve").unwrap();
        let paths = AppPaths::from_roots(
            data_file.clone(),
            directory.path().join("config"),
            directory.path().join("cache"),
        )
        .unwrap();
        let (mut backend, mut events) = Backend::start_with(Some(paths), true).unwrap();
        wait_event(&mut events, |event| {
            matches!(event, Event::Status(Level::Error, _))
        });
        assert!(backend.join());
        assert_eq!(fs::read(data_file).unwrap(), b"preserve");
        assert!(!backend.set_player(PlayerPreference::Vlc));
    }

    fn write_wav(path: &std::path::Path) {
        let samples = 8_000_u32 * 20;
        let bytes = samples * 2;
        let mut wav = Vec::with_capacity(44 + bytes as usize);
        wav.extend(b"RIFF");
        wav.extend((36 + bytes).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes()); // PCM
        wav.extend(1_u16.to_le_bytes()); // mono
        wav.extend(8_000_u32.to_le_bytes());
        wav.extend(16_000_u32.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(bytes.to_le_bytes());
        for sample in 0..samples {
            wav.extend(
                (if sample % 40 < 20 {
                    1_000_i16
                } else {
                    -1_000_i16
                })
                .to_le_bytes(),
            );
        }
        fs::write(path, wav).unwrap();
    }

    fn wait_progress(storage: &Storage, target: &PlaybackTarget, minimum: f64) -> PlaybackProgress {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(progress) = storage.load_progress(target).unwrap()
                && progress.position_seconds >= minimum
            {
                return progress;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("mpv did not produce persisted measured progress");
    }

    #[test]
    #[ignore = "requires installed mpv; real HTTP, IPC, SQLite and resume smoke"]
    fn real_http_mpv_resume() {
        let directory = tempfile::tempdir().unwrap();
        let paths = paths(directory.path());
        let media = directory.path().join("own-test.wav");
        write_wav(&media);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(async {
            let source = kadr_media::LocalFile::open(media).await.unwrap();
            kadr_media::Server::start(Arc::new(source), Default::default())
                .await
                .unwrap()
        });
        let source = ManualSource::parse_link(&server.private_url()).unwrap();
        let target = PlaybackTarget {
            media_id: MediaId::new(source.id().as_str().to_owned()).unwrap(),
            episode: None,
        };
        let (mut backend, mut events) = Backend::start_with(Some(paths.clone()), true).unwrap();
        wait_event(&mut events, |event| matches!(event, Event::Ready(_)));
        assert!(backend.play(source.clone(), false));
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(true))
        });
        let storage = Storage::open(&paths.database_path()).unwrap();
        let first = wait_progress(&storage, &target, 3.0);
        assert_eq!(first.origin, ProgressOrigin::Mpv);
        assert!((first.duration_seconds.unwrap() - 20.0).abs() < 0.1);
        backend.stop();
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(false))
        });
        assert!(backend.join());
        let saved = storage.load_progress(&target).unwrap().unwrap();
        assert_eq!(storage.history(10).unwrap().len(), 1);

        // A new worker/DB connection simulates an app restart. The same private
        // test endpoint remains alive; no private URL is persisted in SQLite.
        let (mut backend, mut events) = Backend::start_with(Some(paths), true).unwrap();
        wait_event(&mut events, |event| matches!(event, Event::Ready(_)));
        assert!(backend.play(source.clone(), true));
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(true))
        });
        let resumed = wait_progress(&storage, &target, saved.position_seconds + 1.0);
        assert!(resumed.position_seconds < saved.position_seconds + 6.0);
        backend.stop();
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(false))
        });

        assert!(backend.play(source, false));
        wait_event(&mut events, |event| {
            matches!(event, Event::PlaybackActive(true))
        });
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut restarted = false;
        while Instant::now() < deadline {
            let progress = storage.load_progress(&target).unwrap().unwrap();
            if progress.updated_at >= resumed.updated_at
                && progress.position_seconds < saved.position_seconds
            {
                restarted = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(restarted, "explicit restart must not reuse saved position");
        // Quitting while playing must stop/reap the owned process.
        let before = Instant::now();
        assert!(backend.join());
        assert!(before.elapsed() < Duration::from_secs(10));
        runtime.block_on(server.shutdown()).unwrap();
    }
}
