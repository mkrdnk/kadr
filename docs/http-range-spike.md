# P0: loopback HTTP Range spike

`kadr-media` — воспроизводимый автономный прототип, не torrent engine.
Выбран HTTP/1.1 Hyper + Tokio: штатный HTTP parser, потоковое тело с
backpressure, без собственного разбора HTTP и без сборки GPUI.

## Запуск

Из корня workspace, явно выбрав неизменяемый локальный обычный файл:

```sh
cargo run --locked --quiet -p kadr-media --example http_range -- "/absolute/path/to/local-file"
```

Единственная строка stdout — **приватный bearer URL** вида
`http://127.0.0.1:<ephemeral-port>/<random-token>`. Это сознательное исключение
для копирования URL в локальный плеер. Не публикуйте URL, не сохраняйте stdout
в общедоступные логи. `--quiet` нужен, чтобы сам Cargo не печатал команду
запуска с личным путём файла в stderr. Example путь не печатает; сервер
сам ничего не логирует. Ctrl+C вызывает явный `shutdown().await`.

## Решение и контракты

- Сервер всегда сам делает bind `127.0.0.1:0`. Других адресов в API нет.
  256-битный токен генерируется OS CSPRNG отдельно для каждой сессии.
  Разрешён один origin-form path в представлении штатного Hyper/http parser:
  без query, пользовательских путей файлов и URL. Endpoint не выполняет
  percent-decoding или нормализацию traversal. При этом HTTP-библиотека
  может нормализовать некорректный request-target до проверки endpoint:
  например, отбросить fragment `#...`. Побайтовое соответствие raw URI
  не гарантируется; токен всё равно обязателен. Чужие токены/traversal — `404`.
  Только `GET`/`HEAD`; остальные методы на разрешённом endpoint — `405`.
- `200`, `206`, `416`, `Content-Length`, `Accept-Ranges`, `Content-Range`;
  closed/open/suffix single ranges. Некорректные, неизвестные units,
  обратные границы и multipart ranges игнорируются (`200`).
  Корректные неудовлетворимые ranges, включая zero suffix и range пустого
  источника, получают `416` с `bytes */length`. Огромные decimal значения
  обрабатываются без переполнения. `HEAD` игнорирует `Range`.
  Валидаторов нет: любой `If-Range` приводит к полному `200`.
  `Cache-Control: no-store`; MIME намеренно `application/octet-stream`.
- `ByteSource` сообщает неизменяемую длину и выполняет асинхронный
  `read_at(offset, max_len)`. Готовый `LocalFile` держит открытый дескриптор:
  замена имени/симлинка после открытия не меняет источник.
  На Unix открытие использует `O_NONBLOCK`, затем проверяет тип открытого
  дескриптора: случайно выбранный FIFO не зависает в ожидании writer.
  Positional `FileExt::read_at` (Unix) / `seek_read` (Windows) не разделяют
  seek cursor. Размер одного чтения ограничен 64 KiB; целый файл не
  загружается в RAM. Исходный файл нельзя менять во время сессии.
- Управляемый тестовый источник использует `watch`: недоступные байты
  ожидаются без polling/busy loop. Каждый read ограничен
  `ServerOptions::read_timeout` (по умолчанию 10 секунд).
  Таймаут — на одно чтение, не общий deadline воспроизведения.
  Drop read future отменяет ожидание; закрытие клиента, timeout и остановка
  сервера освобождают его. Данные не подменяются нулями и не объявляются
  полностью полученными при short read/EOF/error. После отправки заголовков
  ошибка рвёт HTTP body/соединение; клиент обязан считать неполный
  `Content-Length` ошибкой. Ошибка до flush заголовков может закрыть соединение
  вообще без ответа; отдельного протокола сообщения о недоступности нет.
- `Server::shutdown().await` отменяет сессию, закрывает listener/соединения
  и join-ит все HTTP-задачи. `Drop` только запрашивает остановку; для
  детерминированной очистки нужен явный `shutdown`.
- Отдельная transport read-ahead pump читает socket в ограниченный Tokio
  duplex buffer (64 KiB), а Hyper разбирает только этот поток. Это не HTTP
  parser: pump обнаруживает EOF/error даже когда Hyper перестал опрашивать
  чтение из-за pipelining или незавершённого request body. При заполнении
  буфера соединение закрывается, а не блокирует монитор EOF. Так что слишком
  большой непрочитанный pipeline/request payload может быть отклонён без
  HTTP-ответа. TCP write-half close клиента также считается отключением:
  half-close для продолжения получения ответа прототип не поддерживает.
  Обе transport futures живут внутри connection task и отменяются вместе с ней.

## Проверка

```sh
cargo check -p kadr-media --all-targets --locked
cargo test -p kadr-media --locked
cargo fmt -p kadr-media -- --check
cargo clippy -p kadr-media --all-targets --locked -- -D warnings
```

Тесты работают с собственными фиксированными байтами/временными файлами и
реальными HTTP socket requests на ephemeral loopback, без внешней сети/медиа.
Покрыты парсер, GET/HEAD/206/416, пустой источник, перемотки в обе стороны,
конкурентные positional reads, токены/traversal, timeout/wakeup, клиентская
отмена (в том числе pipeline, незавершённый request body и частичный ответ),
переполнение input buffer, ошибки источника, усечение файла, отказ для FIFO,
Drop и shutdown. FIFO проверяется в дочернем процессе с таймаутом: регрессия
блокирующего OS open не должна навсегда заблокировать сам тестовый runtime.
Нужна возможность bind loopback в sandbox.

## Границы P0, не NET-02 / production

Нет BitTorrent, piece verification, torrent priorities/reprioritization при
seek (NET-02), переключения источников, UI, запуска/IPC плеера и измерений
производительности. Прототип проверяет HTTP и границу ожидания байтов; не
закрывает production NET-03/04/08 целиком.

Нет rate limits, глобальных лимитов соединений/памяти, hardening против
локального DoS, TLS, remote streaming, MIME detection, validators, multipart
и resume/session persistence. URL — capability: любой локальный процесс,
получивший его, может прочитать выбранный источник. Токен не заменяет
изоляцию от вредоносных процессов на той же машине.

Файловые операции идут в blocking pool Tokio: **уже начавшееся системное
чтение обычного файла невозможно принудительно прервать средствами Rust**.
Shutdown не ждёт такое чтение после drop async future; blocking pool может
закончить его позже. Это ограничение относится к зависшему OS/storage I/O,
а не к ожиданию недоступных байтов (оно отменяемо и проверено тестами).
Проверено на Linux; macOS требует самостоятельного прогона.
Windows не является целью v1.0.
