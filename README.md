# kadr

Локальное desktop-приложение для поиска фильмов и сериалов и воспроизведения
через внешний плеер. Целевые платформы — Linux (Wayland/X11) и macOS.

Требования: [SPEC.md](SPEC.md). Выполненные работы и оставшиеся проверки:
[DONE.md](DONE.md).

## Текущее состояние

Реализован функциональный MVP: поиск фильмов и сериалов, карточки и эпизоды,
реальные разрешённые HTTP-источники, ручные HTTP(S)/magnet/`.torrent`,
выбор торрент-файла, воспроизведение до завершения загрузки, библиотека,
избранное, история, продолжение, ограниченный кэш и постоянное сохранение.

Для mpv сохраняются реальные IPC-измерения и подтверждённое EOF-завершение.
VLC управляется, но явно отмечает недоступную телеметрию. Браузер получает
только локальный поток: контейнер/кодек могут не поддерживаться, автоматический
прогресс не обещается. История открытия не подменяется подтверждением просмотра.

Linux проверен сквозными тестами и реальными GUI-сценариями в X11/Wayland,
включая каталог → источник → видимый кадр mpv и нативные file/save dialogs.
На macOS 15 arm64 прошли сборка, ordinary tests, native render и unsigned DMG
smoke, реальные mpv/VLC и сквозные HTTP/torrent/save/resume-проверки.
Версионированные unsigned CI-кандидаты собираются после обоих platform gates.
Точный статус и релизные ограничения указаны в [DONE.md](DONE.md).
Подписанный публичный выпуск не объявляется: требуются Apple credentials и
юридическая проверка third-party notices и ограниченной declared-SPDX policy,
описанных в
[документации дистрибуции](docs/distribution.md).

## Готовые сборки 0.1.0

[Полный зелёный CI и артефакты](https://github.com/mkrdnk/kadr/actions/runs/43561951626)
для проверенного кода `d78b61b`:

- [Linux x86_64 — AppImage и Flatpak](https://github.com/mkrdnk/kadr/actions/runs/43561951626/artifacts/14100902780).
- [macOS arm64 — DMG](https://github.com/mkrdnk/kadr/actions/runs/43561951626/artifacts/14101425537).

В архивах есть SHA-256 и `.requirements.txt`. CI хранит артефакты семь дней;
после этого доступны исходники и воспроизводимые команды сборки. Это
**unsigned-кандидаты**, не notarized-релиз. mpv/VLC устанавливаются отдельно.
На macOS фактические runtime-проверки выполнены на 15 arm64; deployment target
13.0 не означает отдельного тестирования macOS 13.

## Структура проекта

Workspace содержит:

- `crates/kadr-app` — desktop-точка входа, GPUI-контроллер, нативные диалоги и постеры.
- `crates/kadr-ui` — GPUI-экраны без дискового/сетевого IO.
- `crates/kadr-engine` — независимый от UI движок: публичные команды/события,
  прикладные сценарии и отдельный worker SQLite/плееров.
- `crates/kadr-core` — проверка источников, stable ID, эпизоды и состояния.
- `crates/kadr-catalog` — Internet Archive и TVmaze; метаданные и источники разделены.
- `crates/kadr-cache` — durable reservations, LRU, активные leases и atomic saves.
- `crates/kadr-media` — защищённый loopback HTTP Range и mpv/VLC IPC.
- `crates/kadr-storage` — стандартные каталоги, SQLite, настройки/история/прогресс.
- `crates/kadr-torrent` — librqbit engine с magnet metadata, лимитами и verified
  streaming; прежний local-only spike оставлен для воспроизводимых регрессий.

Интерфейс обращается к движку только через API `kadr-engine`, а не напрямую
к хранилищу, каталогу или плеерам. Движок не зависит от GPUI и собирается и
тестируется отдельно: `cargo test --locked -p kadr-engine`.
Граница и правила её изменения описаны в [docs/desktop.md](docs/desktop.md).

Контракты и команды:
[desktop-сценарий](docs/desktop.md),
[HTTP Range](docs/http-range-spike.md),
[плееры](docs/player-spike.md),
[storage](docs/storage.md),
[модели](docs/core-models.md),
[UI](docs/ui-shell.md).
[Каталоги и лицензии](docs/catalog.md),
[torrent engine](docs/torrent-engine.md),
[кэш и сохранение](docs/cache.md),
[remote HTTP](docs/http-remote.md).
Исследование кандидатов BitTorrent — в
[docs/bittorrent-spike.md](docs/bittorrent-spike.md), исполняемый
[local-only spike и измерения](docs/torrent-integration.md).

## Сборка и запуск

Нужен актуальный stable Rust с Cargo и системные зависимости GPUI 0.2.2.
GPUI подключается напрямую из crates.io через Cargo, без локального override.
Зависимости Rust зафиксированы в `Cargo.lock`; команды выполняются из корня
репозитория. Проверено с Rust/Cargo 1.99.1 на Fedora 43 и 1.100.0 на Debian 12, x86_64;
минимальная поддерживаемая версия Rust пока не определена.

```sh
cargo build --locked -p kadr-app
cargo run --locked -p kadr-app
```

При наличии GNU Make можно использовать корневой `Makefile`:

```sh
make
make release
make run
```

`make` и `make build` собирают debug-бинарник, `make release` — оптимизированный.
`make run` собирает и запускает приложение через Cargo. Путь к Cargo можно
переопределить переменной `CARGO`, например `make CARGO='cargo +stable' release`.

Приложение показывает окно `kadr` с пятью разделами, начиная с «Источники».
В «Поиске» введите название, откройте карточку и запросите источники. Для
сериала сначала выберите эпизод. Метаданные не гарантируют наличия источника;
Archive-адаптер предлагает файлы только с разрешённой декларацией лицензии.
Используйте только контент, на просмотр и распространение которого у вас есть права.

Ручной путь: скопируйте HTTP(S)/magnet и нажмите «Вставить ссылку» либо
откройте `.torrent`. Для BitTorrent сначала включите сеть в «Настройках»:
по умолчанию она выключена, отдача частей при включении остаётся paused.
Выберите видеофайл; неоднозначный сезон/эпизод можно указать вручную.
«Продолжить / смотреть» восстанавливает измеренную позицию, «С начала» её
не использует. «Открыть в…» меняет плеер без перезапуска торрент-источника.

Полные ссылки и пути намеренно сохраняются в приватной локальной SQLite для
повторного открытия из библиотеки, но не в обычных логах. БД не зашифрована;
очистка истории не является forensic erase. Сохранённые вне кэша файлы
остаются после очистки. Существующие файлы не перезаписываются: выберите новое имя.

Кэш консервативно резервирует место на всю раздачу до создания payload,
хотя загружается выбранный файл и необходимые соседние pieces. Если раздача
не помещается в лимит или на диск, выберите другую либо измените настройки.
Смена каталога кэша очищает прежние неактивные данные, не постоянные копии.
Закрытие последнего окна или `Ctrl+Q` / `Cmd+Q` завершает приложение.
Для просмотра нужен отдельно установленный mpv или VLC; для открытия UI они
не нужны. Приложение ничего не устанавливает автоматически.

### Совместимость зависимостей

В `Cargo.lock` зафиксированы `mio 1.2.4` и `libc 0.2.186`: транзитивный
`xattr 0.2.3` из GPUI использует удалённый в новых версиях `libc`
Linux-алиас `ENOATTR`, а более новый `mio 1.2.5` уже требует новый `libc`.
Это временное ограничение совместимости. GPUI 0.2.2 дополнительно закреплён
точной версией `=0.2.2`, но используется опубликованный crate без изменений.
Прежние локальные поправки выбора X11 visual и явных `f32`-типов grid-литералов
больше не применяются. На Fedora 43 с Rust 1.99.1 проверены сборка, запуск
Wayland и читаемый рендеринг XWayland; это не заменяет native X11-проверку.
Известная проблема чёрного окна на Debian 12 с software Vulkan остаётся
риском опубликованной версии. До повторной native X11-проверки `vendor/gpui`
сохранён как отключённая резервная копия, не участвующая в сборке:
[provenance, ограничения и ADR](docs/adr-gpui-x11.md).

Используйте `--locked`. При обновлении зависимостей отдельно проверяйте,
что upstream устранил эту несовместимость, и повторяйте проверки Linux/macOS.
Текущая сборка также сообщает future-incompatibility warning в транзитивном
`proc-macro-error2 2.0.1`; он не блокирует используемый Rust, но требует
проверки перед обновлением toolchain.

### Linux

Нужна графическая сессия и драйвер с поддержкой Vulkan. Сборка включает
оба бэкенда GPUI: Wayland и X11. Для сборки нужны системный компилятор,
`pkg-config` и development-пакеты Fontconfig, Wayland, XCB и xkbcommon
(включая X11-интеграцию).

Названия основных системных пакетов:

| | Fedora | Debian/Ubuntu |
|---|---|---|
| Инструменты | `gcc`, `pkgconf-pkg-config` | `build-essential`, `pkg-config` |
| Шрифты | `fontconfig-devel` | `libfontconfig1-dev` |
| Wayland | `wayland-devel` | `libwayland-dev` |
| X11/XCB | `libxcb-devel` | `libxcb1-dev`, `libx11-xcb-dev` |
| Клавиатура | `libxkbcommon-devel`, `libxkbcommon-x11-devel` | `libxkbcommon-dev`, `libxkbcommon-x11-dev` |
| Vulkan runtime | `vulkan-loader` и драйвер GPU | `libvulkan1` и драйвер GPU |

Это ориентир для установки, а не проверенная матрица чистых дистрибутивов.
Транзитивные зависимости могут потребовать дополнительные инструменты;
сверяйтесь с ошибкой сборки и
[документацией GPUI 0.2.2](https://docs.rs/crate/gpui/0.2.2/source/README.md).
Приложение не устанавливает системные пакеты автоматически.

Для нативных файловых диалогов нужен работающий `xdg-desktop-portal` с
подходящим backend (`xdg-desktop-portal-gtk`, GNOME/KDE/wlr).
Для сборки всего workspace также нужны CMake/Clang (native crypto dependencies).
В Flatpak для atomic saves предоставлен явный доступ к домашнему каталогу,
а для внешних плееров — host-command permission; это не строгая изоляция от host.

В сессии Wayland с доступным XWayland бэкенды можно проверить отдельно:

```sh
# Wayland: WAYLAND_DISPLAY должен указывать на доступный compositor.
cargo run --locked -p kadr-app

# X11/XWayland: DISPLAY должен указывать на доступный X-сервер.
env -u WAYLAND_DISPLAY cargo run --locked -p kadr-app
```

Наличие переменных окружения само по себе не подтверждает успешный запуск.
Для smoke-проверки нужно увидеть содержимое окна, изменить его размер,
закрыть окно и убедиться, что процесс завершился.
XWayland-проверка не заменяет проверку в отдельной нативной X11-сессии.

### macOS

Нужны Xcode с инструментами сборки и Metal SDK. Сборку, открытие окна
и завершение приложения необходимо отдельно проверить на macOS;
результаты Linux эту проверку не заменяют.
Требования и настройка Xcode описаны в
[README GPUI 0.2.2](https://docs.rs/crate/gpui/0.2.2/source/README.md).
Минимальная поддерживаемая версия macOS для kadr пока не утверждена.

## Проверки

```sh
cargo fmt --all -- --check
python3 scripts/check-architecture-test.py
python3 scripts/check-architecture.py
cargo test --locked -p kadr-engine
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked --workspace -- --ignored --test-threads=1
```

Проверки Rust workspace также доступны как `make fmt`, `make check`,
`make clippy` и `make test`. Цель `fmt` только проверяет форматирование,
не изменяя файлы; opt-in `--ignored`-тесты запускаются отдельно.

Отдельные проверки с установленными внешними плеерами:

```sh
cargo test --locked -p kadr-media --test players_smoke -- --ignored --nocapture
cargo test --locked -p kadr-media --test video_smoke -- --ignored --nocapture
cargo test --locked -p kadr-engine tests::real_ -- --ignored --nocapture --test-threads=1
```

Они используют собственные PCM-данные и сгенерированное FFmpeg MPEG4/AAC-видео,
loopback, без скачивания медиа. Для `video_smoke` нужен также `ffmpeg`.
Opt-in tests требуют установленных плееров/FFmpeg; live-каталог требует сети.
Обычный `cargo test` оставляет их ignored, а не выдаёт за подтверждённый
runtime-прогон.

Изолированная X11 lifecycle-проверка (Xvfb/Openbox; пиксели не проверяет):

```sh
python3 scripts/smoke-x11.py target/debug/kadr-app
python3 scripts/smoke-xorg.py --binary target/debug/kadr-app --artifacts target/smoke-xorg
```

Native Wayland через nested Weston X11 (нужен дополнительно Weston):

```sh
python3 scripts/smoke-wayland.py --binary target/debug/kadr-app --timeout 30
```

Это реальный Wayland-клиент без `DISPLAY`, не native X11 и не проверка
hardware desktop. `--screenshot` сохраняет снимок для отдельной визуальной
проверки и требует ImageMagick.

Для `fmt` и `clippy` нужны соответствующие компоненты Rust toolchain.
На Linux интеграционный тест запускает настоящий бинарник без дисплея
и проверяет, что приложение возвращает ошибку вместо зависания.
Тесты `kadr-media` проверяют Range, точность байтов, таймауты, отмену и
остановку на реальных loopback-соединениях с собственными тестовыми данными.
Внешние медиа, trackers и установленные плееры для обычных тестов не нужны;
player IPC-тесты используют контролируемые Python-процессы (`python3`).
Отображение окна и взаимодействие с оконным менеджером требуют отдельной
графической smoke-проверки; этот тест их не заменяет.
