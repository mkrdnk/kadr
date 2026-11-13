# P0: внешние mpv и VLC

Реальные адаптеры находятся в `kadr-media::players` (Unix). Это самостоятельный
IO-прототип, без зависимости от `kadr-core`, GPUI, torrent engine или storage.
Проверена Linux-часть P0; macOS пока **не проверена**. Это не закрытие PLAY-09:
сохранение истории и выбор интервала опроса остаются задачей вызывающей сессии.

## Что поддерживается

| Возможность | mpv | VLC |
|---|---|---|
| Запуск отдельного процесса, HTTP(S) или локальный файл | Да | Да |
| Resume | `--start=<seconds>` | `--start-time=<seconds>` |
| Позиция | JSON IPC `get_property time-pos` | Явный `Telemetry::Unsupported` |
| Длительность | JSON IPC `get_property duration` | Явный `Telemetry::Unsupported` |
| Graceful stop | JSON IPC `quit` | `oldrc` Unix socket `quit` |
| Завершение | OS child exit / `ExitStatus` | OS child exit / `ExitStatus` |
| Headless smoke | `--vo=null --ao=null` | `--intf=dummy --vout=dummy --aout=dummy` |

`Telemetry::Seconds(f64)` — только конечное неотрицательное число, реально
сообщённое плеером. `Unavailable` — поддерживаемое свойство пока недоступно
(загрузка, live stream, завершённый mpv). Это **не нулевая позиция**.
`Unsupported` — возможностей адаптера недостаточно для надёжной телеметрии.
Ни таймер, ни время жизни процесса никогда не превращаются в прогресс.

VLC `oldrc` выдаёт неструктурированные строки, приветствия и уведомления без
request ID. Этот spike не притворяется, что один полученный integer надёжно
соответствует `get_time` или `get_length`. RC используется только для выхода.
Для будущего полноценного VLC history нужны отдельный проверенный
структурированный интерфейс (например, libVLC) и платформенный spike.
Resume VLC проверен как передача CLI-флага и косвенно по раннему EOF известного
файла, а не как обратное считывание позиции.

## Контракт и жизненный цикл

```rust
use std::time::Duration;
use kadr_media::players::{
    PlayRequest, PlayerKind, PlayerOptions, PlayerSession, Telemetry,
};
use tokio_util::sync::CancellationToken;

async fn example() -> std::io::Result<()> {
let cancel = CancellationToken::new();
let kind = PlayerKind::Mpv;
let mut session = PlayerSession::launch(
    kind,
    PlayRequest {
        media: "http://127.0.0.1:12345/private-capability".into(),
        resume: Some(Duration::from_secs(30)),
    },
    PlayerOptions::for_player(kind),
    &cancel,
).await?;
let snapshot = session.snapshot(&cancel).await;
// Save only actual Seconds values, not Unavailable/Unsupported.
// Regardless of a telemetry error, join the process before runtime shutdown.
let stopped = session.stop().await;
snapshot?;
stopped?;
Ok(())
}
```

- `PlayerOptions::executable`: имя в PATH либо явный путь. Для macOS можно
  указать `/Applications/VLC.app/Contents/MacOS/VLC`; путь в bundle не
  обнаруживается автоматически. Отсутствующий executable даёт `NotFound`
  с подсказкой проверить установку/права.
- `try_wait()` не блокируется; `snapshot()` содержит `exit: Option<ExitStatus>`.
  Процесс мог завершиться с ошибкой или по закрытию окна: это не доказательство
  достижения конца видео, и нельзя автоматически ставить «просмотрено».
- Нет фонового опроса: вызывающий код сохраняет только реально полученные
  данные. После выхода mpv последняя позиция не запрашивается; её нужно
  запоминать по мере воспроизведения.
- Дефолты: connect 5 s, IPC command 1 s, stop grace 2 s и kill/reap 2 s.
  Нулевые timeout отвергаются. Snapshot делает максимум два запроса; при
  transport EOF дополнительно ждёт child exit не дольше command timeout.
  Startup failure/cancellation дополнительно ограниченно убивает/собирает child.
- Ошибочный, отменённый, оборванный или просроченный IPC-запрос закрывает
  соединение. Повторного использования частичного/запоздалого ответа нет.
  Subsequent snapshot возвращает `NotConnected`, пока процесс ещё жив;
  `stop()` остаётся доступным.
- `stop()` идемпотентен, сначала посылает quit с command timeout, затем ждёт
  grace period, при необходимости делает kill и ограниченный wait.
  Отброшенный future можно повторить. Drop сессии/незавершённого launch
  использует Tokio `kill_on_drop`; сбор child требует работающего runtime.
  **Предпочтителен явный `stop().await` до остановки runtime.**
- Если процесс не отвечает даже на SIGKILL из-за состояния ядра, bounded wait
  может вернуть ошибку: невозможно одновременно гарантировать deadline и
  синхронный reap зависшего OS process. Сохраняется kill-on-drop.

## Безопасность и ограничения

- Только `tokio::process::Command` с отдельными argv, без shell и без
  интерполяции `$()`, кавычек, пробелов и `;`. Перед media стоит `--`.
  Executable должен быть доверенным установленным плеером; это не sandbox для
  произвольного бинарника/playlist/контейнера. Не доверяйте выбор бинарника
  удалённому источнику.
- IPC — Unix socket в непредсказуемом коротком `/tmp/kadr-player-*` каталоге,
  **созданном сразу с mode 0700**, а не chmod после создания. Нет публичного
  TCP RC-порта. Каталог/socket удаляются при Drop; SIGKILL самого kadr может
  оставить каталог/плеер. Другие процессы того же UID всё ещё могут читать
  argv и управлять сокетом — Unix permissions не изолируют один UID.
- mpv отключает пользовательский config/scripts и ytdl. VLC игнорирует config,
  не переиспользует чужой процесс, отключает media library и metadata network
  access. VLC `--rc-fake-tty` обязателен при null stdin в проверенной версии.
  Это не исчерпывающий аудит всех плагинов VLC и всех сетевых протоколов.
- Child stdin/stdout/stderr — null: диагностический вывод плеера не заполняет
  pipe и не раскрывает private URL в логах kadr. Ошибки адаптера не включают
  media/socket paths. Это уменьшает детализацию ошибок codec/network:
  диагностика требует отдельного осознанного запуска с несекретным fixture.
- JSON replies/events ограничены 64 KiB на строку; весь обмен, включая поток
  событий без ответа, ограничен deadline. Ответы сопоставляются по request ID.
  Никакой телеметрии из чужого ответа или wall clock.
- Управляется только прямой child, не дерево произвольных wrapper-процессов.
  Передавайте прямой бинарник mpv/VLC. Нет pause/seek/UI tracks API,
  автоматического browser launch, persistence, player installation или
  проверки номера версии на runtime. Media compatibility определяется самим
  плеером. Headless PCM smoke не доказывает графический рендер или A/V sync.

## Проверенные версии и платформы

| Среда | Фактическое свидетельство |
|---|---|
| Debian 12, Linux x86_64, uid 1000 | Rust/Cargo 1.100.0; Python 3.11.6 для fake process/IPC tests |
| Debian `mpv 0.35.1`, FFmpeg `5.1.9-0+deb12u1` | Реальный headless запуск WAV и MPEG4/AAC MP4, JSON time-pos/duration, resume, HTTP Range, quit и natural EOF |
| Debian VLC `3.0.24 Vetinari`, revision `3.0.24-rc1-0-g6de05adcba` | Реальный `oldrc` Unix socket, WAV/MP4 HTTP, start-time, quit, natural EOF; Unsupported телеметрия; установлен `vdummy` output plugin |
| macOS | Не запускалось и не собиралось; Unix API выбран совместимым по форме, но это не runtime evidence |
| Иные версии mpv / VLC, VLC 4, Windows | Не проверены; обещания совместимости отсутствуют |

Точные подтверждённые версии выше — не объявление минимальных версий.
Для macOS нужны те же fake/real тесты с установленными плеерами и явными
executable paths/PATH, затем отдельный оконный smoke/закрытие окна.

## Воспроизведение проверок

Из корня интегрированного workspace:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo test --locked -p kadr-media --all-targets
cargo test --locked -p kadr-media --test players_smoke -- --ignored --nocapture
cargo test --locked -p kadr-media --test video_smoke -- --ignored --nocapture
cargo clippy --locked -p kadr-media --all-targets -- -D warnings
cargo run --locked -p kadr-media --example player -- mpv /absolute/own.wav 2 --headless
cargo run --locked -p kadr-media --example player -- vlc /absolute/own.wav 2 --headless
```

Без `--headless` example открывает обычный вывод плеера; Ctrl+C вызывает stop.
Example печатает только телеметрию/exit status, не media URL. Headless tests
создают собственный восьмисекундный PCM WAV и обслуживают его через существующий
tokenized `kadr-media::Server`; внешних скачиваний или дисплея не требуется.
Real tests намеренно ignored: отсутствие системных плееров не должно ломать
обычный unit test job.

### Локальные результаты этой реализации

Начальные независимые проверки, чтобы не редактировать общий `Cargo.lock`
параллельно и не собирать GPUI повторно, выполнены на временной копии
**того же `kadr-media`** с раскрытыми workspace package fields и скопированным
lockfile. После интеграции прошли и корневые workspace-команды; точные итоговые
результаты приведены в [DONE.md](../DONE.md). Ниже — отдельное первоначальное
свидетельство, а не подмена workspace-прогона.
Фактический manifest: `$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml`.

| Точная команда (с env выше) | Результат |
|---|---|
| `cargo test --offline --manifest-path "$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml" --all-targets` | 41 passed (7 unit, 20 HTTP, 14 player), 3 real smoke ignored; оба examples собраны |
| `cargo test --offline --manifest-path "$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml" --test players_smoke -- --ignored --nocapture` | 2 passed: реальные mpv/VLC, в том числе HTTP/resume/stop/EOF |
| `cargo test --offline --manifest-path "$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml" --test video_smoke -- --ignored --nocapture` | 1 passed: новый MP4 video test, оба реальных плеера, HTTP bytes/Range/resume/stop/EOF |
| `cargo clippy --offline --manifest-path "$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml" --all-targets -- -D warnings` | Passed |
| `rustfmt --check --edition 2024 crates/kadr-media/src/players.rs crates/kadr-media/tests/players.rs crates/kadr-media/tests/players_smoke.rs crates/kadr-media/examples/player.rs` | Passed |
| `cargo run --offline --quiet --manifest-path "$DELTA_SCRATCH_DIR/player-build/media/Cargo.toml" --example player -- mpv "$DELTA_SCRATCH_DIR/own-vlc.wav" 7 --headless` (затем та же команда с `vlc`) | Оба exit 0, source path не печатается |
| Те же examples с началом `0`, SIGINT после 0.6 s (Python subprocess harness) | mpv и VLC: Ctrl+C → stop, exit 0; stdout пуст, media path не попал в stderr |

Fake tests используют настоящие Python subprocess и Unix sockets:
argv и resume, private directory mode, interleaved events/wrong IDs,
Unavailable/Unsupported, malformed/oversized/negative/partial/flood/EOF replies,
IPC EOF перед child exit, connect timeout, cancellation и отбрасывание futures
launch/snapshot/stop, noisy stdio, graceful quit → kill, process reap,
natural nonzero exit, missing executable и cleanup socket directory.

## Дополнение: собственное тестовое видео MPEG4/AAC MP4

`crates/kadr-media/tests/video_smoke.rs` добавляет отдельный opt-in video smoke.
Fixture генерируется установленным FFmpeg argv, без shell, сети и чужих файлов:

```sh
ffmpeg -nostdin -hide_banner -loglevel error \
  -filter_threads 1 -filter_complex_threads 1 \
  -f lavfi -i testsrc2=size=320x180:rate=24 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -t 8 -c:v mpeg4 -q:v 4 -threads:v 2 \
  -c:a aac -b:a 64k -threads:a 1 -movflags +faststart \
  "/temporary/private-directory/own test video.mp4"
```

Генерация ограничена 20 s, при timeout FFmpeg убивается и собирается.
В проверенном окружении получен MP4 размером 954923 bytes. Существующий
tokenized HTTP server обслуживает его через обёртку `ByteSource`, которая
сверяет каждый прочитанный участок с оригинальными bytes и записывает offsets.
Отдельный реальный HTTP-запрос `Range: bytes=65549-98319` проверяет статус 206,
точный `Content-Range` и побайтовое равенство выбранного диапазона.
Статистика этого запроса очищается **до** запуска плеера: он не может
подменить доказательство, что сам плеер действительно читал MP4.

Для каждого плеера проверены:

1. Resume с 2 s, чтение настоящих MP4 bytes, процесс остаётся жив и завершается
   через явный `stop()` с кодом 0.
2. Resume с 6 s, natural EOF с кодом 0 раньше шести секунд wall time —
   быстрее, чем полное восьмисекундное воспроизведение без resume.
3. Для mpv — измеренная позиция и длительность до stop/EOF. В фактическом
   прогоне: `2.000 / 8.000 s` и `6.042 / 8.000 s`.
4. Для VLC — по-прежнему `Unsupported` position/duration. Wall time служит
   только грубой проверкой resume, не сохраняется как прогресс.

В Linux-прогоне каждый плеер прочитал не меньше 954923 проверенных bytes;
mpv при позднем resume повторно читал части файла (1274278 bytes суммарно).
Это не утверждение о конкретной стратегии запросов плеера: media seek может
использовать prefetched bytes. HTTP seek доказывается отдельной проверкой
точного 206, а восстановленная media position — настоящей телеметрией mpv.

VLC headless больше не использует `--no-video`: включён video track с
`--vout=dummy`, а dummy main interface и audio output сохранены. Таким образом
тест не отключает video pipeline ради одного аудио. Это не измерение
отрисованного кадра, визуального качества, A/V sync или torrent playback.
macOS video smoke не запускался; список проверенных codec/container пока
ограничен одним сгенерированным MPEG4/AAC MP4 и прежним PCM WAV.

Остаются: macOS evidence, графические Linux Wayland/X11/macOS проверки,
более широкий corpus video/container/codec fixtures, durable history,
выбор production VLC интерфейса и упаковка.
Полные workspace/GPUI проверки здесь не запускались.
