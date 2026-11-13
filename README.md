# kadr

Локальное desktop-приложение для поиска фильмов и сериалов и воспроизведения
через внешний плеер. Целевые платформы — Linux (Wayland/X11) и macOS.

Требования: [SPEC.md](SPEC.md). Выполненные работы и оставшиеся проверки:
[DONE.md](DONE.md).

## Текущее состояние

P0/P1 и начало P2: GPUI-каркас с навигацией, SQLite-настройками и ручным
HTTP(S)-воспроизведением в mpv/VLC. Для mpv измеренная позиция сохраняется
локально и восстанавливается; VLC явно показывает отсутствие надёжной
телеметрии. Это ещё не готовый MVP: каталога, торрент-сессий в UI,
полноценной библиотеки, очистки кэша и релизных пакетов нет.

Linux-проверки включают реальный HTTP/IPC/SQLite-сценарий и внешние headless
mpv/VLC. Проверки lifecycle не заменяют визуальную приёмку на реальном
desktop. macOS пока не проверена. Точные результаты и ограничения
графического окружения — в [DONE.md](DONE.md).

Workspace содержит:

- `crates/kadr-app` — окно, контроллер и отдельный worker SQLite/плееров.
- `crates/kadr-ui` — GPUI-экраны без дискового/сетевого IO.
- `crates/kadr-core` — проверка источников, stable ID, эпизоды и состояния.
- `crates/kadr-media` — защищённый loopback HTTP Range и mpv/VLC IPC.
- `crates/kadr-storage` — стандартные каталоги, SQLite, настройки/история/прогресс.
- `crates/kadr-torrent` — отдельный local-only librqbit spike: private `.torrent`,
  loopback-пиры и передача проверенных частей через существующий Range-сервер.

Контракты и команды:
[desktop-сценарий](docs/desktop.md),
[HTTP Range](docs/http-range-spike.md),
[плееры](docs/player-spike.md),
[storage](docs/storage.md),
[модели](docs/core-models.md),
[UI](docs/ui-shell.md).
Исследование кандидатов BitTorrent — в
[docs/bittorrent-spike.md](docs/bittorrent-spike.md), исполняемый
[local-only spike и измерения](docs/torrent-integration.md).

## Сборка и запуск

Нужен актуальный stable Rust с Cargo и системные зависимости GPUI 0.2.2.
Зависимости Rust зафиксированы в `Cargo.lock`; команды выполняются из корня
репозитория. Проверено с Rust/Cargo 1.99.1 на Fedora 43 и 1.100.0 на Debian 12, x86_64;
минимальная поддерживаемая версия Rust пока не определена.

```sh
cargo build --locked -p kadr-app
cargo run --locked -p kadr-app
```

Приложение показывает окно `kadr` с пятью разделами, начиная с «Источники».
Скопируйте разрешённую HTTP(S)-ссылку и нажмите «Вставить ссылку», затем
«Продолжить / смотреть» или «С начала». Плеер выбирается в «Настройках».
Приватный URL не отображается и не записывается в БД; для следующего запуска
пока нужно вставить ту же ссылку. Файлы `.torrent` и magnet проверяются,
но торрент-сеть из desktop ещё не запускается.
Закрытие последнего окна или `Ctrl+Q` / `Cmd+Q` завершает приложение.
Для просмотра нужен отдельно установленный mpv или VLC; для открытия UI они
не нужны. Браузерный запуск пока не подключён, предупреждение об ограничениях
показывается явно. Приложение ничего не устанавливает автоматически.

### Совместимость зависимостей

В `Cargo.lock` зафиксированы `mio 1.2.4` и `libc 0.2.186`: транзитивный
`xattr 0.2.3` из GPUI использует удалённый в новых версиях `libc`
Linux-алиас `ENOATTR`, а более новый `mio 1.2.5` уже требует новый `libc`.
Это временное ограничение совместимости, без локальных копий библиотек
и без дополнительных прямых зависимостей.

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
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Отдельные проверки с установленными внешними плеерами:

```sh
cargo test --locked -p kadr-media --test players_smoke -- --ignored --nocapture
cargo test --locked -p kadr-media --test video_smoke -- --ignored --nocapture
cargo test --locked -p kadr-app backend::tests::real_http_mpv_resume -- --ignored --nocapture
```

Они используют собственные PCM-данные и сгенерированное FFmpeg MPEG4/AAC-видео,
loopback, без скачивания медиа. Для `video_smoke` нужен также `ffmpeg`.
Обычный `cargo test` оставляет эти четыре теста ignored, а не выдаёт их за
подтверждённое воспроизведение.

Изолированная X11 lifecycle-проверка (Xvfb/Openbox; пиксели не проверяет):

```sh
python3 scripts/smoke-x11.py target/debug/kadr-app
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
