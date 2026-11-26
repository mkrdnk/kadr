# Ход реализации kadr

Источник требований: [SPEC.md](SPEC.md). Галочка означает выполненную
и проверенную работу в указанном объёме, а не готовность всего этапа.
Непроверенные платформы и частичные результаты отмечаются отдельно.

Каждый законченный пункт фиксируется отдельным логически цельным коммитом
вместе с применимыми тестами и обновлением этого журнала. Незавершённые
прототипы и неподтверждённые платформы не помечаются выполненными.

## P0 — технические spike

- [x] Минимальный Rust workspace с `kadr-app` и зависимостью GPUI 0.2.2.
- [x] Окно GPUI: сборка, отображение и закрытие подтверждены на Linux / XWayland.
- [x] Native Wayland: создание toplevel и передача буферов подтверждены trace.
- [x] Linux: регрессионный тест завершения с ошибкой при отсутствии дисплея.
- [x] Linux / XWayland: реальный рендер и выход при закрытии окна, код 0.
- [x] Сборка и графическая smoke-проверка Linux / Wayland (native client, nested Weston).
- [x] Сборка и графическая smoke-проверка в отдельной нативной X11-сессии (Xorg dummy, исправленный GPUI).
- [x] Сборка и графическая smoke-проверка macOS 15 arm64 (CI + просмотр native screenshots).
- [x] Документарное сравнение BitTorrent API, лицензий и рисков упаковки.
- [x] BitTorrent pieces/seek/teardown на Linux/macOS; librqbit выбран и используется production engine.
- [x] Прототип mpv IPC: запуск, позиция, длительность, возобновление, завершение (Linux).
- [x] Прототип VLC: запуск/resume/завершение, телеметрия явно недоступна (Linux).
- [x] Прототип HTTP Range на Linux: GET/HEAD, 206/416, seek, таймауты и отмена.
- [x] Прогон HTTP Range-прототипа на macOS (workspace tests и настоящий mpv/torrent seek).
- [x] Исходные Linux loopback-измерения pieces/Range/shutdown и решение по движку (docs/torrent-integration.md); это не CPU/RAM/WAN SLO.

## P1 — каркас

- [x] Разделение приложения на крейты по мере появления реализаций (продолжается).
- [x] Окно приложения с навигацией и основными экранами (ранний каркас).
- [x] Настройки и стандартные пользовательские каталоги (storage API и полный UI).
- [x] SQLite, версионированные миграции, сохранение настроек между запусками.
- [x] Подтверждённый запуск на Linux и macOS.

## P2 — медиаядро

- [x] Ручной ввод и проверка HTTP(S), magnet, `.torrent` (вставка/picker, реальный engine IO после consent).
- [x] Метаданные торрента, выбор видеофайла, распознавание эпизодов (Linux).
- [x] Буферизация, приоритеты частей и seek без полной загрузки (Linux).
- [x] Защищённый loopback HTTP-сервер с Range.
- [x] mpv/VLC, браузер с предупреждением об ограничениях (Linux).
- [x] Состояния сессии, сетевые лимиты, отмена и освобождение ресурсов (Linux).
- [x] Сквозная проверка воспроизведения разрешённого тестового видео (Linux).

## P3 — каталог

- [x] Выбор разрешённых API метаданных и источников, правила атрибуции.
- [x] Поиск, карточки фильмов/сериалов, сезоны и эпизоды (Linux UI).
- [x] Встроенный реальный адаптер поиска разрешённых источников.
- [x] Ручной источник со связью с каталогом и без неё.
- [x] Loading/empty/success/error и отмена длительных операций.
- [x] Сквозной поиск и запуск разрешённого источника (Linux native GUI).

## P4 — библиотека

- [x] Избранное, история, стабильные идентификаторы локальных элементов.
- [x] Прогресс и возобновление по фильму/эпизоду (mpv; VLC явно unavailable).
- [x] Ограниченный кэш, LRU неактивных данных, защита активных/сохранённых файлов.
- [x] Сохранение файла вне временного кэша с обработкой прерывания и нехватки места.
- [x] Тесты прогресса, политики кэша и сохранения.

## P5 — релиз

- [x] CI: сборки, тесты, real playback/save и native render на Linux/macOS.
- [x] Единый зелёный workflow с опубликованными unsigned CI-кандидатами обоих targets.
- [x] Smoke-проверки в Wayland и X11 (Linux software rendering).
- [x] Оптимизированные Flatpak и AppImage: strict package builds и Linux smoke.
- [x] macOS DMG: optimized unsigned candidate построен, смонтирован и запущен; native pixels и exit 0 подтверждены.
- [x] Объявленные targets: Linux x86_64, macOS arm64; baseline и требования в docs/distribution.md.
- [ ] Подпись и notarization macOS с внешними Apple credentials.
- [x] Документация зависимостей, приватности и сетевого поведения.
- [ ] Все 13 критериев приёмки из §10 SPEC.md проверены на целевых платформах.

## Итерация 1 — минимальное окно GPUI

**Статус: минимальный прототип работает на Linux / XWayland.**
Сборочный блокер устранён. Полная платформенная приёмка P0 ещё не завершена;
остальная функциональность MVP не реализована.

Сделано:

- Workspace с единственным крейтом `kadr-app`, GPUI закреплён как `=0.2.2`,
  в `Cargo.lock` выбраны совместимые `mio 1.2.4` и `libc 0.2.186`,
  `target/` исключён из Git. Новых прямых зависимостей и vendoring нет.
- Добавлено окно с названием `kadr`, пометкой P0, ограничением минимального
  размера, действиями выхода и завершением при закрытии последнего окна.
- Все пути выхода используют очередь главного потока: это предотвращает
  потерю раннего `quit()` при старте Linux event loop и реальную панику
  `RefCell already borrowed` при закрытии X11-окна внутри callback GPUI.
- Для Wayland заголовок явно задаётся через `set_window_title`:
  одной настройки `TitlebarOptions` в GPUI 0.2.2 оказалось недостаточно.
- Добавлен интеграционный тест запуска настоящего бинарника без дисплея:
  ожидаются код 1 и понятное сообщение; при зависании процесс убивается
  и собирается по таймауту 10 секунд.
- Добавлен README с командами запуска, системными требованиями и ручной
  smoke-проверкой. Содержимое `SPEC.md` и `LICENSE` сохранено без изменений.

Проверки и ограничения:

| Проверка | Результат |
|---|---|
| `cargo metadata --locked --no-deps --format-version 1` | Успешно: workspace содержит только `kadr-app`, прямая зависимость — GPUI |
| `CARGO_BUILD_JOBS=2 cargo build --locked -p kadr-app` | Успешно после исправления зависимостей и lifecycle |
| `CARGO_BUILD_JOBS=2 cargo check --locked --workspace --all-targets` | Успешно |
| `cargo fmt --all -- --check` | Успешно с локальным rustfmt в `PATH` |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Успешно с локальным Clippy в `PATH`, `CARGO_BUILD_JOBS=2` |
| `CARGO_BUILD_JOBS=2 cargo test --locked --workspace` | Успешно: 1 интеграционный тест; последняя проверка завершилась за 0.13 с |
| `cargo run --locked -p kadr-app` с удалённым `WAYLAND_DISPLAY` | XWayland: окно найдено, содержимое проверено по снимку только этого окна; `WM_DELETE_WINDOW` завершил процесс с кодом 0 |
| `WAYLAND_DEBUG=1 timeout --signal=TERM 4s cargo run --locked -p kadr-app` | Native Wayland: подтверждены `set_app_id`, `set_title`, attach/commit и frame callbacks, без panic / protocol error; процесс остановлен таймаутом (124), не обычным закрытием окна |
| Resize и клавиатурные сочетания выхода | Ручная проверка остаётся открытой: попытки внешней X11-автоматизации не подтвердили resize и синтетический `Ctrl+Q` |
| Визуальный результат и обычное закрытие native Wayland | Не проверены автоматически: доступ к захвату/управлению окнами GNOME ограничен; защитные настройки не менялись |
| macOS, нативная X11-сессия | Не проверены: доступно только окружение Linux / Wayland с XWayland |

Окружение: Fedora 43, x86_64, Rust/Cargo 1.99.1. В системном toolchain
изначально отсутствовали `fmt` и `clippy`; для проверок соответствующие
Fedora RPM версии 1.99.1-2 распакованы только в игнорируемый `target/agent-tools`
и добавлены в `PATH` отдельных команд. Подписи RPM проверены через `rpmkeys`.
Системная установка пакетов и изменение настроек не выполнялись;
mpv/VLC не установлены и не нужны для этой итерации.

История устранённого блокера: первая сборка падала с E0425 в цепочке
`gpui → gpui_http_client → zed-async-tar → xattr 0.2.3` из-за удалённого
`libc::ENOATTR`. Понижение только `libc` невозможно с `mio 1.2.5`,
требующим `libc >= 0.2.190`; согласованный подбор обеих версий исправил сборку.
Первый `cargo check` до исправления был остановлен, последующие проверки
прошли. При обновлениях необходимо сохранять совместимость этой цепочки
либо проверить upstream-исправление.

Известное внешнее предупреждение: `proc-macro-error2 2.0.1` реэкспортирует
приватный `proc_macro` (future incompatibility, E0365). Сейчас сборка
и Clippy проходят; перед обновлением Rust нужно проверить исправление upstream.

## P0 — исследование BitTorrent-движков

Добавлено [сравнение librqbit и libtorrent-rasterbar](docs/bittorrent-spike.md)
с первичными источниками, конкретными API, ограничениями bindings и планом
локального multi-file эксперимента.

Первым кандидатом для воспроизводимого spike рекомендован `librqbit 9.1.0`:
готовый Rust stream API уменьшает FFI и сложность платформенной сборки.
`libtorrent-rasterbar` остаётся альтернативой с более прямым управлением
приоритетами pieces. Это не окончательный выбор движка.

Проверены опубликованные API/README и сведения о версиях/лицензиях.
Зависимости проекта не менялись, BitTorrent-сессии не запускались,
сборка embedding и Linux/macOS runtime не проверены. Требования NET-01/02/09
и полный BitTorrent-пункт P0 по этому исследованию не закрываются.

## Итерация 2 — автономный HTTP Range-прототип

**Статус: P0-прототип реализован и проверен на Linux.**
Добавлен реальный `kadr-media`, но UI, torrent engine и production-медиасессии
ещё не подключены. Контракты, команда запуска и ограничения:
[docs/http-range-spike.md](docs/http-range-spike.md).

Сделано:

- Hyper/Tokio HTTP/1.1, bind только `127.0.0.1:0`, отдельный 256-битный
  токен из OS CSPRNG на один заранее выбранный источник.
- GET/HEAD, single closed/open/suffix Range, `200/206/416`, необходимые
  заголовки, пустые файлы и безопасная обработка огромных decimal границ.
- Потоковые positional reads без общего seek cursor и без чтения всего файла
  в память. Открытый дескриптор закрепляет файл за сессией; замена пути не
  переключает разрешённый источник. Unix FIFO отвергается без блокировки open.
- Отменяемое ожидание недоступных байтов, timeout на чтение и явный shutdown.
  Усечение файла и ошибки источника обрывают body вместо успешного неполного
  ответа.
- Независимое обнаружение transport EOF исправляет найденный на ревью случай,
  когда Hyper не замечал отключение при buffered pipeline/неполном request
  body. Добавлены регрессии, ограниченный read-ahead и корректный output drain.
- CLI example выводит приватный URL только по явному запуску; путь и URL не
  попадают в stderr. Ctrl+C останавливает сессию и освобождает порт.
- Сохранены совместимые версии GPUI/libc/mio. Новые пакеты в lockfile —
  `httpdate 1.0.3` и `tokio-macros 2.7.2`; остальные HTTP/runtime-зависимости
  уже присутствовали в графе GPUI, но теперь явно объявлены для `kadr-media`.

Проверки окончательной интегрированной версии:

| Проверка | Результат |
|---|---|
| `cargo fmt --all -- --check` | Успешно с локальным rustfmt в `PATH` |
| `CARGO_BUILD_JOBS=2 cargo build --locked --workspace --all-targets` | Успешно, включая GPUI и HTTP example |
| `CARGO_BUILD_JOBS=2 cargo test --locked --workspace` | Успешно: 28 тестов — 1 startup, 7 unit и 20 HTTP/socket/file |
| `CARGO_BUILD_JOBS=2 cargo check --locked --workspace --all-targets` | Успешно |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Успешно с локальным Clippy в `PATH`, `CARGO_BUILD_JOBS=2` |
| Документированный `cargo run --locked --quiet -p kadr-media --example http_range -- <file>` | Linux smoke: точные байты `206` через границу 64 KiB; HEAD игнорирует Range; `416` и `404` для неверного токена/query |
| Полный GET с `Connection: close` | Проверено ровно 1 MiB собственных данных, без усечения при output drain |
| Ctrl+C и ошибка отсутствующего файла | Код 0 при остановке, порт закрыт; приватный URL/путь отсутствуют в stderr; ошибка открытия не раскрывает путь |
| Независимое ревью | Найденные cancellation/normalization проблемы исправлены либо явно ограничены контрактом; повторная статическая проверка transport без новых замечаний |

Прототип не закрывает целиком NET-02/03/04/08 и критерий приёмки seek
в реальном торренте: нет piece verification/приоритетов, UI и внешнего плеера.
macOS не проверена. Half-close считается отключением; заполнение input buffer
закрывает соединение. Raw URI может нормализоваться HTTP-библиотекой; токен
проверяется в parsed path. Зависшее OS file I/O нельзя принудительно отменить.
Нет глобальных лимитов сессий/соединений, MIME detection и измерений SLO.
Это ограничения P0, а не обещания готового production-сервера.

Сохраняется ранее описанное внешнее future-incompatibility warning
`proc-macro-error2 2.0.1`; оно не блокирует текущие проверки.

## Итерация 3 — воспроизводимое окружение и CI

- Добавлена CI-матрица Linux/macOS с `fmt`, `check`, Clippy, тестами и сборкой
  всех targets. Конфигурация ещё не запускалась на GitHub Actions; macOS
  не считается проверенной.
- `python3 scripts/smoke-x11.py target/debug/kadr-app` запускает отдельные
  Xvfb/Openbox без XWayland и проверяет появление окна, resize до 1000×720,
  обычное закрытие через WM и `Ctrl+Q` (оба выхода — код 0).
  Настройки теста изолированы от пользовательских.
- Проверено в Cloud Runner Debian 12, x86_64, Rust 1.100.0:
  `CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
  cargo test --locked --workspace` — все прежние 28 тестов прошли.
- **Ограничение:** lifecycle X11 проверен, но первый снимок Xvfb с Mesa 22.3.6
  содержит чёрную клиентскую область. Отображение содержимого не подтверждено;
  графическая приёмка X11 остаётся открытой. Скрипт не выдаёт map окна за
  проверку пикселей.

## Итерация 4 — локальное хранилище

Добавлен `kadr-storage`: стандартные XDG/macOS-каталоги, настройки, bundled
SQLite и две транзакционные миграции. Неизвестная будущая схема и повреждённые
значения отклоняются без сброса данных. Новые каталоги/БД приватны на Unix.
API синхронный и предназначен только для фонового worker.

Также реализованы API избранного, истории и прогресса по отдельному
произведению/сезону/серии. Используются непрозрачные ID, без хранения исходных
URL/путей. Автопрогресс браузера отклоняется; запуск сам по себе не создаёт
прогресс. Это ещё не готовая библиотека в UI.

Проверено в интегрированном workspace на Linux:
`cargo test -p kadr-storage` — 21 тест; затем
`cargo clippy --locked -p kadr-storage --all-targets -- -D warnings` и
`cargo fmt -p kadr-storage -- --check` — успешно. Для сборок использованы
`CARGO_BUILD_JOBS=2`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`.
Документация API, временных defaults и ограничений: [docs/storage.md](docs/storage.md).
macOS runtime, очистка кэша, применение сетевых лимитов и перенос видео пока
не реализованы/не проверены.

## Итерация 5 — модели источников и эпизодов

Добавлен независимый от GPUI/IO `kadr-core`: строгая валидация ручных
HTTP(S)/magnet/`.torrent`, versioned stable ID, приватные диагностические
сообщения, безопасные относительные torrent-пути и отбор видеофайлов.
Распознаются `S01E02` и `1x02`; неоднозначные/нераспознанные имена не
подменяются догадкой, ручной выбор сохраняется. Добавлена небольшая модель
состояний источника/плеера с отдельными ключами прогресса файлов/эпизодов.

Проверено в workspace на Linux: `cargo test -p kadr-core` — 21 тест;
`cargo clippy --locked -p kadr-core --all-targets -- -D warnings` и
`cargo fmt -p kadr-core -- --check` — успешно (jobs=2, dev/test debug=0).
Файлы и сеть не открываются; лексическая проверка путей не заменяет
symlink-safe IO в будущем адаптере. Требование о выборе и исправлении
эпизодов в UI пока не закрыто. Контракты: [docs/core-models.md](docs/core-models.md).

## Итерация 6 — плееры и интегрированный desktop

- Реальные mpv JSON IPC и VLC oldrc Unix socket: отдельные argv без shell,
  приватный каталог 0700, bounded startup/commands/stop, отмена,
  graceful quit и kill/reap. mpv сообщает измеренную позицию/длительность;
  VLC явно возвращает `Unsupported`, не таймерный «прогресс».
- `kadr-ui`: пять разделов, явная недоступность ещё не подключённых адаптеров,
  preview ручного источника, кнопки запуска/остановки и выбора плеера.
- `kadr-app`: clipboard читается только при нажатии; ввод валидируется core,
  HTTP запускается в выбранном плеере. SQL и player IO — в отдельном worker.
  Выбор плеера, история и реальный прогресс mpv сохраняются в SQLite.
  Обычный запуск предлагает продолжение, отдельная кнопка — «С начала».
- Закрытие окна/выход отменяют запуск и дожидаются остановки/reaping после
  platform loop, вне 100-мс quit budget GPUI.
- Независимое ревью выявило и помогло устранить drop launch future при
  shutdown, устаревший picker, stale readiness при отказе worker и ложные
  сообщения об успешном сохранении. Добавлены регрессии generation/status.

Проверки на Debian 12 / Rust 1.100.0, jobs=2, dev/test debug=0:

| Команда/сценарий | Результат |
|---|---|
| `cargo test --locked --workspace` | 95 passed, 3 opt-in player-теста ignored |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Успешно |
| `cargo build --locked --workspace --all-targets` | Успешно |
| `cargo test --locked -p kadr-media --test players_smoke -- --ignored --nocapture` | 2 passed: реальные headless mpv 0.35.1 / VLC 3.0.24, собственный WAV по HTTP |
| `cargo test --locked -p kadr-app backend::tests::real_http_mpv_resume -- --ignored --nocapture` | Passed: HTTP → реальный mpv → SQLite → новый worker → resume → с начала → quit при активном плеере |
| `python3 scripts/smoke-x11.py target/debug/kadr-app` | Обновлённый UI: map, resize, WM_DELETE и Ctrl+Q, оба выхода 0; пиксели не утверждаются |
| Native Wayland → nested Weston X11 → Xvfb | Подтверждены configure/attach/commit, визуально проверен настоящий экран «Источники», Ctrl+Q через compositor завершил приложение с кодом 0 |

У Wayland используется `weston --backend=x11-backend.so` с реальным `wl_seat`;
headless backend Weston 10 без seat вызывает panic внутри GPUI 0.2.2
и не подходит для этой проверки. Обычная bare X11 клиентская область в
доступном Xvfb/Mesa всё ещё чёрная. Native hardware desktop и macOS не
подтверждены. Это не заменяет проверку GUI внешних плееров или декодирования
видео: headless smoke использует собственный PCM WAV.

Каталог, список библиотеки, browser launch, torrent lifecycle в UI,
cache enforcement и сохранение видео не готовы. Полные URL в БД не хранятся:
для продолжения нужно снова вставить ту же ссылку. Сетевые/cache настройки
пока сохранённая конфигурация, не действующие ограничения.
Контракты: [desktop](docs/desktop.md), [player spike](docs/player-spike.md),
[UI shell](docs/ui-shell.md).

## Итерация 7 — исполняемый BitTorrent spike

Добавлен `kadr-torrent` с `librqbit = 9.1.0`: только собственные private
`.torrent` и явные loopback-пиры, без DHT, tracker announces, LSD и UPnP.
Production-движком он пока не объявляется. Адаптер выбранного файла реализует
существующий `ByteSource`, отдаёт только проверенные части через прежний
bearer HTTP Range-сервер. Источник не ждёт полного скачивания, независимые
positional reads не разделяют seek cursor.

Тестовая multi-file раздача генерируется из собственных байтов, две части
у seed заведомо повреждены/недоступны. Это детерминированно проверяет чтение
до завершения, а не гонку с быстрым полным скачиванием. Проверены metadata,
начальные/средние/конечные и конкурентные Range, перемотка назад/вперёд,
ожидание missing pieces, disconnect/timeout/HTTP shutdown/torrent stop,
освобождение listener и ошибки инициализации. До записи отклоняются
неподходящие peers и существующие symlink-компоненты в output.

Два дополнительных результата независимого ревью:

- Upstream error chains могли раскрывать абсолютный output path и имя файла.
  Теперь ошибки парсинга/файлов/stream/seek/read заменяются безопасными
  stage-сообщениями, `TorrentFile::Debug` скрывает имя. Регрессия воспроизводит
  настоящую коллизию файла с каталогом и проверяет все варианты formatting.
  Статистика также возвращается собственным numeric-only `TorrentCounters`:
  upstream fatal-error strings с именами файлов не передаются наружу.
- `disable_trackers`/`TcpOnly` **не** выключают скрытый wildcard IPv6 UDP
  tracker socket librqbit. Все sockets дополнительно привязаны к `lo`
  (Linux) / `lo0` (macOS) через upstream interface binding. При ошибке нет
  unrestricted fallback. Linux-тест в отдельном процессе проверяет
  `SO_BINDTODEVICE` реальных TCP и UDP sockets, не только TCP address.
  Проверка без root успешна; macOS-вариант конфигурации пока не проверен.

В workspace на Linux в составе `cargo test --locked --workspace` прошли
8 torrent-тестов (2 unit, 5 loopback/privacy/path и 1 network-confinement);
`cargo clippy --locked --workspace --all-targets -- -D warnings` — успешно.
После добавления зависимостей сохранены `libc 0.2.186` и `mio 1.2.4`.
Окружение проверок: Rust 1.100.0, jobs=2, dev/test debug=0.

В [docs/torrent-integration.md](docs/torrent-integration.md) зафиксированы
API, воспроизводимые команды, исходные Linux-измерения и provisional ADR.
В одном локальном прогоне first verified 1024 B после init — 43.304 ms,
HTTP Range с полным чтением ответа — 0.526–9.837 ms; это маленький fixture,
часть данных уже в кэше, **не SLO** и не доказательство оптимального
порядка piece-запросов. Upstream stop включает паузу около одной секунды.

Ограничения: нет magnet metadata exchange, публичного swarm, UI-интеграции,
сетевых/cache лимитов, LRU, постоянного сохранения или гарантии join всех
upstream tasks. Symlink preflight не защищает от TOCTOU. На каждый read
создаётся stream, а lookahead управляется librqbit; production scheduler
и throughput ещё требуют измерений. Полный P0 BitTorrent (Linux/macOS)
и P2 end-to-end torrent video пока не закрыты.

## Итерация 8 — видео, графический smoke и итоговая проверка

- Новый opt-in `video_smoke` создаёт собственное восьмисекундное MPEG4/AAC
  MP4 через FFmpeg, проверяет точные source bytes и unaligned HTTP `206`,
  затем реальные headless mpv/VLC: resume, stop и natural EOF.
  VLC headless теперь использует dummy video output, а не отключение видео.
  Визуальный первый кадр и A/V sync этим не утверждаются.
- Добавлен воспроизводимый `scripts/smoke-wayland.py`: приватные HOME/XDG,
  Xvfb + Openbox + Weston X11, app без `DISPLAY`, проверка configure →
  non-null attach → commit, Ctrl+Q exit 0 и cleanup process groups.
  Это native Wayland-клиент в nested compositor, не hardware desktop.
- Проверены указателем все пять разделов. Выбор Browser сохранён в реальной
  SQLite и остался выбран после перезапуска; предупреждение отображается.
  Неподключённый browser launch теперь отключает Play, а не только показывает
  ошибку после нажатия. Сворачивание/остановка описаны в Настройках.
- CI дополнен nested Wayland lifecycle; оба Linux smoke изолированы от
  пользовательских настроек. GitHub Actions и macOS ещё не запускались.

**Итоговый прогон текущего workspace, Debian 12 x86_64 / Rust 1.100.0:**
`CARGO_BUILD_JOBS=2`, `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`.

| Проверка | Итог |
|---|---|
| `cargo fmt --all -- --check` | Успешно |
| `cargo test --locked --workspace` | **103 passed, 4 opt-in tests ignored**, без ошибок |
| `cargo check --locked --workspace --all-targets` | Успешно |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Успешно |
| `cargo build --locked --workspace --all-targets` | Успешно |
| `cargo test --locked --workspace -- --ignored` | **Все 4 passed**: два real-player WAV smoke, generated MP4 smoke и HTTP/mpv/SQLite/restart/resume |
| `python3 scripts/smoke-x11.py target/debug/kadr-app` | Map, resize, WM_DELETE и Ctrl+Q, оба выхода 0; bare X11 pixels по-прежнему не подтверждены |
| `python3 scripts/smoke-wayland.py --binary target/debug/kadr-app --screenshot --timeout 30 --artifacts target/smoke-wayland-final` | Protocol + Ctrl+Q exit 0; итоговый снимок «Источников» визуально проверен |
| Сборка `local_torrent --help` и отдельный запуск готового CLI | Собственная private-раздача: точные 28 000 байт `206`, SIGINT exit 0, HTTP порт закрыт; нет URL/output path в stderr |
| Python AST parse обоих smoke-скриптов, `git diff --check` | Успешно |

Сохраняется внешнее future-incompatibility предупреждение
`proc-macro-error2 2.0.1`; текущие check/build/Clippy проходят.
Ручная проверка Tab/Shift-Tab/Enter/Space, file picker и настоящего
GUI-воспроизведения плееров не выполнялась. Изменения не опубликованы
на remote и не являются релизом.

### Следующий последовательный этап

1. Закрыть macOS build/runtime и bare X11 rendering; CI-матрица пока только
   добавлена, а не подтверждена удалённым запуском.
2. Спроектировать и проверить cache ownership/budget/LRU и symlink-safe
   filesystem IO до подключения публичных торрент-сессий; сохранённые
   network/cache settings пока нельзя выдавать за enforcement.
3. Подключить torrent metadata/file selection к UI, согласовать контролируемый
   lifecycle и приоритеты для файлов больше lookahead, проверить собственное
   torrent-видео до завершения загрузки и seek во внешнем плеере.
4. Подключить список библиотеки, связь с каталогом и browser-local-stream
   lifecycle. Выбрать разрешённые metadata/source API и правила атрибуции.
5. Затем выполнять сквозные P3/P4 сценарии и упаковку; Flatpak/AppImage/DMG,
   подпись/notarization и все 13 критериев приёмки ещё не закрыты.

## Итерация 9 — реализации MVP, не отдельные демонстрации

После запроса завершить весь MVP добавлены и связаны реализации:

- `kadr-catalog`: настоящий поиск Internet Archive (фильмы/видео) и TVmaze
  (сериалы, сезоны, эпизоды). Источники Archive возвращаются только после
  повторной проверки разрешённой лицензии. Метаданные TVmaze сами по себе
  не считаются разрешением на просмотр. Подтверждён live-запрос с HTTP `206`.
- `kadr-torrent::engine`: offline `.torrent`, настоящий BEP9 magnet exchange,
  получение метаданных до файловой записи, выбор файла, проверенные части,
  повторное использование после rehash, лимиты и настоящее отключение upload.
  Торрент-сеть включается пользователем отдельно; UPnP не включается.
- `kadr-cache`: durable whole-torrent reservations, RAII-защита активного
  контента, inactive LRU, fd-relative/no-follow файловые операции, проверка
  свободного места с учётом ещё не выделенных резервов. Постоянное сохранение
  потоковое, atomic/no-clobber, с отменой и очисткой временного sibling-файла.
- SQLite v3: реальные дескрипторы для повторного открытия, метаданные карточек,
  независимые per-episode связи и snapshot file-ID у прогресса, сохранённые
  копии вне кэша, редактируемые пути и включение каталога. Полные ссылки/пути
  теперь намеренно хранятся в приватной БД для resume, но не в обычных логах.
- `RemoteSource`: ограниченные HTTP Range reads с validators, cancellation,
  pinned DNS и контролем redirects; локальный сервер получил безопасные
  media MIME и bounded connections/header parsing. Для неподходящего
  HTTP-прокси mpv/VLC могут открывать исходник напрямую.

Составные реализации проверены из корневого workspace командой
`cargo test --locked -p kadr-core -p kadr-catalog -p kadr-storage -p kadr-cache
-p kadr-media -p kadr-torrent`. В частности, прошли 41 storage-тест,
25 cache-тестов, 16 torrent-тестов и проверки remote HTTP/IPC.
Live-каталог и реальные плееры запускаются отдельными opt-in тестами;
обычный прогон не выдаёт их за выполненные.

Для X11 добавлен **проверенный source fix GPUI 0.2.2**: opaque visual
соответствует существующей opaque Blade surface. Только этот блок изменён
относительно опубликованного архива; лицензия и SHA-256 сохранены в
[ADR](docs/adr-gpui-x11.md). На реальном `KadrView` получены 1478 цветов,
читаемые элементы, resize и оба штатных выхода в Xvfb/Xorg; Wayland не сломан.
Предыдущее предположение «Xorg сам исправляет rendering» отозвано: без
patch его два цвета были лишь чёрным фоном и белыми полосами resize.

Полный UI/worker и сквозные app-тесты интегрируются и проверяются следующим
коммитом. Сертификаты macOS, нативный macOS runtime и удалённый CI здесь
ещё не считаются подтверждёнными.

## Итерация 10 — полный пользовательский путь и регрессии интеграции

UI теперь не каркас: редактируемый поиск/настройки, карточки с постерами,
сезоны/эпизоды, реальные источники, native file/save dialogs, выбор файлов,
ручная коррекция эпизода, избранное/история/продолжение, статистика и сохранение.
Worker связывает все реализации; IO не выполняется в render loop.

На Linux проверено не только отдельными крейтами:

- Настоящий native Xorg: Sherlock → TVmaze details → S01E01 → честно пустые
  источники; Night of the Living Dead → Archive details/права → MP4 → настоящий
  декодированный кадр mpv → Stop. Xvfb и native Wayland также проверены с
  реальным вводом, выделением и удалением текста.
- Настоящий GTK portal Open File выбирает собственную раздачу S01E01/S01E02,
  UI распознаёт оба файла, при выключенной сети не запускает player/session.
- Native Save File: собственное HTTP-видео сохранено с точным сравнением
  1 282 057 байт, path записан в SQLite; GUI ClearCache не изменил файл/запись.
- Настоящий Firefox получает **локальный** capability URL, декодирует собственное
  видео; проверены точные GET и `206`, Stop закрывает порт. В БД есть история,
  но нет выдуманного прогресса. Найденный History-filter bug исправлен:
  schema v4 хранит actual opens отдельно для каждого target, UI использует
  `opened_at`, а Continue — только настоящую незавершённую позицию. Повторная
  native GUI проверка History/Continue и v3→v4 прошла.
- Настоящий torrent → HTTP → mpv измеряется **до завершения файла**, затем
  проверены настоящий mpv forward/backward seek и продолжение, защита active
  cache, точное verified atomic save, очистка и offline saved-copy resume.
- HTTP без validators или без Range воспроизводится напрямую; конечный файл
  с известным размером сохраняется отдельным цельным GET через общий atomic
  writer. Unknown-length/live не выдаётся за завершённый конечный файл.

Независимое интеграционное ревью помогло устранить перепутанные episode-target
при выборе другого файла, потерю queued OpenIn при teardown, старые save-progress
в новой сессии, гонку Stop после atomic publication, потерю poster opt-out,
сохранение в старом cache root и двойной physical-space admission save+torrent.
Darwin system aliases и Unicode filename aliases обработаны отдельными общими
правилами, без разрешения произвольных symlink-переходов.

Подтверждены root `fmt --check`, `check --locked --workspace --all-targets`,
Clippy `-D warnings`, обычные workspace tests, полный ignored-прогон
`cargo test --locked --workspace -- --ignored --test-threads=1` и
`build --locked --workspace --all-targets`. Ignored-прогон включает три
app-сценария, реальные mpv/VLC WAV/MP4 и live-каталог. Индекс всех 13 критериев:
[docs/acceptance.md](docs/acceptance.md).

Подготовлены реальные AppImage/Flatpak/DMG recipes, host-player integration,
native macOS renderer smoke и package notices. Итоговый оптимизированный
Linux ELF и strict AppImage/Flatpak построены; AppImage прошёл native Xorg
pixel/resize/WM-close/Ctrl+Q и native Wayland buffer/quit проверки.
SHA-256 финального Linux ELF: `efe60e657988c3ef95a150771695dff80a278e7dd0fe2ac3eb8a02b65f30a8f7`.
Строгий collector собрал 670 packages / 1222 notice/metadata entries,
0 warnings; пять точных случаев используют authenticated declared-SPDX policy
с сохранением найденных реальных notices, без выдуманных copyright-строк.
14 collector regressions, ShellCheck, AST checks и workflow actionlint прошли.

Отдельная ветка `mvp/full-desktop` опубликована без изменения remote `master`.
CI теперь может выдавать SHA/version-labelled unsigned artifacts после обоих
platform test jobs по явному `workflow_dispatch retain_artifacts=true`.
macOS runtime/DMG, Apple signing и публичный release до отдельного свидетельства
не помечаются завершёнными.

## Итерация 11 — реальные platform CI и оптимизированные пакеты

Linux CI прошёл build/check/Clippy, обычные тесты, real HTTP/torrent/save,
реальные mpv/VLC, X11 lifecycle, native Wayland и native Xorg pixel smoke.
macOS 15 arm64 подтвердил compilation/check/Clippy, полный ordinary suite,
stable HTTP/mpv/save/resume и torrent playback-before-completion/seek/save.
Настоящее окно и unsigned DMG прошли native render/lifecycle smoke; оба
CI-скриншота просмотрены: читаемый текст и элементы UI подтверждены.
Свидетельство: [CI run 43554313461](https://github.com/mkrdnk/kadr/actions/runs/43554313461).
Весь run остаётся **красным** из-за full-response HTTP save и VLC IPC startup;
оптимизированные CI-артефакты ими заблокированы, эти проверки не отключены.
Ранее устранены переносимые ошибки **тестовых fixtures**:

- socket-flood больше не предполагает, что весь payload помещается в Unix
  send buffer перед чтением; concurrent unit test проверяет точные 64 KiB;
- torrent fixtures создаются под физическим временным parent; `/var` alias
  не ослабляет production-политику отказа от symlink-выходов;
- HTTP truncation test принимает ранний abort до headers, но по-прежнему
  запрещает успешный EOF, padding и любые неправильные байты; 2000 stress runs;
- notice-collector fixtures используют физический root, как production entry
  point; все 14 тестов прошли и с явно symlinked `TMPDIR`.

Оптимизированный установленный Flatpak повторно прошёл оба настоящих host-player
smoke и завершил helper с кодом 0. Найден и устранён orphan a11y-bus, который
после завершения тестов удерживал PTY. Helper теперь создаёт отдельный private
bus/process group, не трогает существующий desktop bus, ограниченно завершает
свои activation services; пять D-Bus/PTY/signal регрессий прошли.

## Итерация 12 — закрытие platform runtime и release gates

В [run 43558661000](https://github.com/mkrdnk/kadr/actions/runs/43558661000)
**обе платформы** прошли build/check/Clippy, ordinary tests, все три реальные
HTTP/torrent/save/resume сценария, три дополнительных обязательных повтора
HTTP fallback и настоящие mpv/VLC WAV/MP4. Повторы не скрывают ошибки:
падение любого прогона делает job красным.

VLC исправлен по доказанной причине: официальный macOS build не регистрирует
`--no-one-instance` и `--no-playlist-enqueue` (upstream включает их только
для Win32/DBus/OS2). Прямой owned CLI child и private oldrc endpoint сохранены;
два одновременных VLC-сеанса и остановка только своего child подтверждены.

HTTP failure оказался ошибкой fixture: сервер обрывал конечный ответ при
`WouldBlock`. Linux воспроизведение и macOS diagnostic дали одинаковую цепочку
origin WouldBlock → transport error → честный отказ atomic save. Fixture теперь
имеет максимум 16 независимых workers, stop-aware partial writes и прежний
whole-response budget. Новая concurrency-регрессия сначала упала на прежнем
serial origin, затем обе новые регрессии прошли за 0.02 с. Production
HTTP/cache-контракты и трёхсекундная проверка отмены не ослаблены.

Строгие inventory повторены родителем: Linux **670/1222/0 warnings**,
macOS **618/1157/0 warnings**. Восемь точных macOS upstream notices сохранены
вместе с Apple SDK caveat и полными MIT/Apache/Zlib terms; ещё пять точных
деклараций прошли тот же bounded authenticated policy, теперь десять случаев
суммарно, без автоматического fallback для новых пакетов/версий.
20 regressions и независимая проверка всех 13 архивов/notice/SPDX hashes прошли.
Это engineering notice collection, не юридическое одобрение распространения.

Run остался красным **только из-за повторного использования smoke artifact
directories из Rust cache**, до запуска окон. CI-пути runtime evidence
перенесены из `target/` в свежий `$RUNNER_TEMP`, package output очищается только
в одноразовых CI checkout. Нативные пиксели/DMG ранее прошли и просмотрены;
итоговый полный workflow и оптимизированные CI-кандидаты проверяются далее.

Оба полных platform test jobs стали зелёными в
[run 43559630535](https://github.com/mkrdnk/kadr/actions/runs/43559630535):
включая fresh native Xorg/macOS pixels, strict DMG, реальные плееры и все
повторы HTTP save. macOS optimized build также завершился успешно.
Release jobs выявили отдельные ошибки bootstrap: offline metadata требовала
не скачанную release-only сборкой dev-зависимость, а четыре cold Python fixtures
упёрлись в собственный искусственный startup limit 1 с, ниже production 5 с.
CI явно выполняет locked target fetch; fake startup теперь наследует production
budget и проверяется дополнительным delayed-start тестом. Специальные короткие
timeout/cancellation cases и production deadlines не изменены.

## Итерация 13 — финальная проверка и доступные сборки

**Функциональный MVP реализован; полный CI зелёный.**
[Run 43561951626](https://github.com/mkrdnk/kadr/actions/runs/43561951626)
проверил код `d78b61beba4b05558eac587cc53975bf5e9fc859`:

- Linux Ubuntu 24.04 и macOS 15 arm64: fmt/check/Clippy/build, обычные тесты,
  настоящие HTTP/torrent/mpv/SQLite/save/resume сценарии; каждый из трёх
  дополнительных full-response save прогонов также прошёл.
- Настоящие mpv/VLC WAV/MP4 на обеих ОС; isolated ownership двух VLC-сеансов.
- Linux X11 lifecycle, native Xorg pixels и native Wayland; macOS native pixels,
  backend-ready/frame markers и штатный выход.
- Linux Ubuntu 22.04: optimized AppImage и Flatpak, AppImage lifecycle и
  установленные Flatpak host mpv/VLC. Строгий collector — 0 warnings.
- macOS: optimized arm64 DMG, minos **13.0**, SDK 15.5, mount, codesign structure,
  native optimized-window pixels и exit 0. Строгий collector — 0 warnings.
  Скачанный CI-скриншот optimized DMG отдельно просмотрен.

Артефакты сохранены на семь дней:
[Linux AppImage/Flatpak](https://github.com/mkrdnk/kadr/actions/runs/43561951626/artifacts/14100902780),
[macOS DMG](https://github.com/mkrdnk/kadr/actions/runs/43561951626/artifacts/14101425537).
Все три пакета скачаны родителем; `sha256sum -c` прошёл:

| Пакет | SHA-256 |
|---|---|
| `kadr-0.1.0-linux-x86_64-unsigned.AppImage` | `8356d1fad6ecae1cecafae2465a59839b96f03a4d6c1375ca48e24478ebb7411` |
| `kadr-0.1.0-linux-x86_64-unsigned.flatpak` | `847fbc21da42768698779ff788df048b1c736830b217d367901e3ee70fc2a4ba` |
| `kadr-0.1.0-macos-arm64-unsigned.dmg` | `94332a1b6b9ffd64c1c8a875aeffd719527f2856a14a7f74d9fa909b700590c6` |

Именно скачанный CI AppImage дополнительно прошёл native Xorg pixels/resize/
WM-close/Ctrl+Q и native Wayland buffer/quit на Debian 12. Скачанный CI Flatpak
установлен с проверкой точного commit и повторно прошёл оба host mpv/VLC smoke;
helper завершился 0 с cleanup private bus/services.

Linux CI-пакеты содержат один ELF SHA-256
`d00f036346c76351c3a98e31b34e2199813b4343efe45f3e5056df15ce6af7f1`;
полная AppImage closure требует максимум GLIBC 2.35. Это отдельная Ubuntu
сборка, а не побайтовое обещание совпадения с локальным Debian ELF.
Для Mac input executable SHA-256:
`37232d175dc552117993130063764211cbe10950ddcbb65cdb1df165e94fa3d9`.

Финальный локальный прогон того же кода также прошёл: точные workspace
fmt/check/Clippy/test/build команды, полный `--ignored --test-threads=1`
(включая live-каталог), 20 notice regressions, 5 private-bus regressions,
ShellCheck, actionlint и `git diff --check`.

**Не объявляется:** Apple Developer ID signing/notarization, legal approval,
hardware GPU matrix, отдельная ручная macOS file-dialog/browser приёмка или
официальный store/release. Эти границы и индекс всех 13 критериев остаются
явными в [docs/acceptance.md](docs/acceptance.md). Подписанных сертификатов не
выдумывали, remote `master` не меняли; финальные изменения после проверенного
кода относятся только к документации.
