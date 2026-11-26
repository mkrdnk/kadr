use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::time::timeout;
use url::Url;

use crate::client::builder::{authorized_url, check_url};
use crate::{Error, TopicId, http, search, topic};

const READ_DEADLINE: Duration = Duration::from_secs(1);
const LOGIN_FORM: &str = r#"
    <form id="login-form-full" action="login.php" method="post">
        <input name="login_username">
        <input name="login_password" type="password">
    </form>
"#;

#[test]
fn parser_debug_and_trace_logging_is_disabled_at_compile_time() {
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Info);
}

// The server deliberately leaves the connection open after writing its fixture.
// Dropping the channel sender releases it; no EOF or terminating chunk is sent
// unless explicitly present in the fixture.
struct LoopbackServer {
    release: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.release.take();
        self.thread.take().unwrap().join().unwrap();
    }
}

async fn loopback_response(wire: &[u8]) -> (reqwest::Response, LoopbackServer) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let wire = wire.to_vec();
    let (release, released) = mpsc::channel();
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() <= 8192, "unexpectedly large fixture request");
        }
        stream.write_all(&wire).unwrap();
        // A deadline bounds cleanup even if a test fails before releasing us.
        let _ = released.recv_timeout(Duration::from_secs(5));
    });
    let server = LoopbackServer {
        release: Some(release),
        thread: Some(thread),
    };
    let response = client
        .get(format!("http://{address}/fixture"))
        .send()
        .await
        .unwrap();
    (response, server)
}

#[test]
fn url_allowlist_accepts_only_the_fixed_https_origin() {
    for input in [
        "https://rutracker.org/",
        "https://rutracker.org:443/forum/login.php",
        "https://rutracker.org/forum/tracker.php?nm=fixture",
        "https://RUTRACKER.ORG/forum/viewtopic.php?t=42",
    ] {
        let url = Url::parse(input).unwrap();
        assert!(authorized_url(&url), "{input}");
        assert!(check_url(&url).is_ok(), "{input}");
    }

    for input in [
        "http://rutracker.org/",
        "https://rutracker.org:444/",
        "https://rutracker.org.evil.example/",
        "https://evil.example/rutracker.org",
        "https://sub.rutracker.org/",
        "https://rutracker.org./",
        "https://api.t-ru.org/",
        "https://user@rutracker.org/",
        "https://user:password@rutracker.org/",
        "https://rutracker.org@evil.example/",
        "https://127.0.0.1/",
        "https://[::1]/",
        "file:///forum/tracker.php",
    ] {
        let url = Url::parse(input).unwrap();
        assert!(!authorized_url(&url), "{input}");
        assert!(
            matches!(check_url(&url), Err(Error::InvalidArgument(_))),
            "{input}"
        );
    }
}

#[tokio::test]
async fn advertised_body_over_cap_is_rejected_without_reading_or_eof() {
    let (response, server) =
        loopback_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n").await;
    assert_eq!(response.content_length(), Some(9));

    let result = timeout(READ_DEADLINE, http::read_bytes_capped(response, 8)).await;
    drop(server);
    assert!(matches!(result, Ok(Err(Error::TooLarge))), "{result:?}");
}

#[tokio::test]
async fn oversized_chunk_is_rejected_before_eof() {
    let (response, server) = loopback_response(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\n123456789\r\n",
    )
    .await;
    assert_eq!(response.content_length(), None);

    let result = timeout(READ_DEADLINE, http::read_bytes_capped(response, 8)).await;
    // Release only after the read has completed or timed out. Reading the whole
    // response before checking the cap must time out, not pass thanks to EOF.
    drop(server);
    assert!(matches!(result, Ok(Err(Error::TooLarge))), "{result:?}");
}

#[tokio::test]
async fn cumulative_chunked_body_over_cap_is_rejected_before_eof() {
    let (response, server) = loopback_response(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n1234\r\n5\r\n56789\r\n",
    )
    .await;

    let result = timeout(READ_DEADLINE, http::read_bytes_capped(response, 8)).await;
    drop(server);
    assert!(matches!(result, Ok(Err(Error::TooLarge))), "{result:?}");
}

#[tokio::test]
async fn exact_cap_is_accepted_for_fixed_length_and_chunked_bodies() {
    for wire in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n12345678".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n1234\r\n4\r\n5678\r\n0\r\n\r\n"
            .as_slice(),
    ] {
        let (response, server) = loopback_response(wire).await;
        let result = timeout(READ_DEADLINE, http::read_bytes_capped(response, 8)).await;
        drop(server);
        assert_eq!(result.unwrap().unwrap(), b"12345678");
    }
}

#[tokio::test]
async fn status_codes_map_to_domain_errors() {
    for status in [200, 204, 401, 403, 429, 500] {
        let wire = format!("HTTP/1.1 {status} Fixture\r\nContent-Length: 0\r\n\r\n");
        let (response, server) = loopback_response(wire.as_bytes()).await;
        let result = http::check_status(response).await;
        drop(server);
        match status {
            200 | 204 => assert_eq!(result.unwrap().status().as_u16(), status),
            401 | 403 => assert!(matches!(result, Err(Error::NotAuthenticated))),
            429 => assert!(matches!(result, Err(Error::RateLimited))),
            500 => assert!(matches!(result, Err(Error::Server(500)))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn cp1251_body_decoding_handles_explicit_charset_and_fallback() {
    let bytes = b"\xCF\xF0\xE8\xE2\xE5\xF2 & fixture";
    for content_type in [
        None,
        Some("text/html"),
        Some("text/html; charset=windows-1251"),
        Some("text/html; charset=\"CP1251\""),
        Some("text/html; charset=unknown-charset"),
    ] {
        assert_eq!(
            http::decode_body(bytes, content_type),
            "Привет & fixture",
            "{content_type:?}"
        );
    }
    assert_eq!(
        http::decode_body(
            "Привет & fixture".as_bytes(),
            Some("text/html; charset=utf-8")
        ),
        "Привет & fixture"
    );
}

#[test]
fn search_login_form_is_not_an_empty_result() {
    let base = Url::parse("https://rutracker.org").unwrap();
    // Authentication failure takes priority even if a results table remains.
    for html in [
        LOGIN_FORM.to_owned(),
        format!("{LOGIN_FORM}<table id=\"tor-tbl\"></table>"),
    ] {
        assert!(matches!(
            search::parser::parse(&html, 1, &base),
            Err(Error::NotAuthenticated)
        ));
    }
}

#[test]
fn search_accepts_a_legitimate_empty_table() {
    let html = r#"
        <table id="tor-tbl"><tbody>
            <tr><td colspan="10">Ничего не найдено</td></tr>
        </tbody></table>
    "#;
    let base = Url::parse("https://rutracker.org").unwrap();
    let parsed = search::parser::parse(html, 1, &base).unwrap();
    assert!(parsed.results.is_empty());
    assert_eq!(parsed.total_count, 0);
    assert_eq!(parsed.total_pages, 0);
    assert_eq!(parsed.page, 1);
}

#[test]
fn search_rejects_unknown_html_instead_of_reporting_no_results() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for html in ["", "<html><body>Temporary maintenance</body></html>"] {
        assert!(matches!(
            search::parser::parse(html, 1, &base),
            Err(Error::Parse { .. })
        ));
    }
}

#[test]
fn search_preserves_real_metadata_and_decodes_title_entities() {
    let html = r#"
        <p class="med bold">Результатов поиска: 51</p>
        <table id="tor-tbl"><tbody><tr>
            <td><a class="tLink" data-topic_id="42">Фильм &amp; &#171;Тест&#187; &#x2014; <em>HD</em></a></td>
            <td class="tor-size" data-ts_text="734003200">700 MiB</td>
            <td class="seedmed"><b>12</b></td>
            <td class="row4 small nowrap tCenter" data-ts_text="1700000000">date</td>
        </tr></tbody></table>
    "#;
    let base = Url::parse("https://rutracker.org").unwrap();
    let parsed = search::parser::parse(html, 2, &base).unwrap();
    assert_eq!(parsed.total_count, 51);
    assert_eq!(parsed.total_pages, 2);
    assert_eq!(parsed.page, 2);
    assert_eq!(parsed.results.len(), 1);
    let row = &parsed.results[0];
    assert_eq!(row.id, TopicId::new(42));
    assert_eq!(row.title, "Фильм & «Тест» — HD");
    assert_eq!(row.size, Some(734_003_200));
    assert_eq!(row.seeds, Some(12));
    assert_eq!(row.registered.timestamp(), 1_700_000_000);
    assert_eq!(row.url, "https://rutracker.org/forum/viewtopic.php?t=42");
}

#[test]
fn search_missing_metadata_stays_unknown_and_timestamp_is_not_size() {
    let html = r#"
        <table id="tor-tbl"><tbody><tr>
            <td><a class="tLink" data-topic_id="43">Missing metadata</a></td>
            <td class="row4 small nowrap tCenter" data-ts_text="1700000000">date</td>
        </tr></tbody></table>
    "#;
    let base = Url::parse("https://rutracker.org").unwrap();
    let parsed = search::parser::parse(html, 1, &base).unwrap();
    assert_eq!(parsed.results.len(), 1);
    let row = &parsed.results[0];
    assert_eq!(row.size, None);
    assert_eq!(row.seeds, None);
    assert_eq!(row.registered.timestamp(), 1_700_000_000);
}

#[test]
fn search_distinguishes_zero_metadata_from_unknown_metadata() {
    let html = r#"
        <table id="tor-tbl"><tbody><tr>
            <td><a class="tLink" data-topic_id="44">Zero metadata</a></td>
            <td class="tor-size" data-ts_text="0">0 B</td>
            <td class="seedmed"><b>0</b></td>
            <td data-ts_text="1700000000">date</td>
        </tr></tbody></table>
    "#;
    let base = Url::parse("https://rutracker.org").unwrap();
    let parsed = search::parser::parse(html, 1, &base).unwrap();
    assert_eq!(parsed.results.len(), 1);
    assert_eq!(parsed.results[0].size, Some(0));
    assert_eq!(parsed.results[0].seeds, Some(0));
}

#[test]
fn topic_extracts_magnet_and_decodes_html_entities() {
    let html = r#"
        <h1 id="topic-title">Тема &amp; &#171;Тест&#187;</h1>
        <div class="post_body">
            <span id="tor-hash">0123456789ABCDEF0123456789ABCDEF01234567</span>
            <a class="magnet-link" href="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&amp;dn=Fixture">Magnet</a>
            <span id="tor-size-humn" title="4096">4 KiB</span>
        </div>
    "#;
    let id = TopicId::new(42);
    let url = "https://rutracker.org/forum/viewtopic.php?t=42";
    let parsed = topic::parser::parse(html, id, url).unwrap();
    assert_eq!(parsed.id, id);
    assert_eq!(parsed.url, url);
    assert_eq!(parsed.title, "Тема & «Тест»");
    assert_eq!(parsed.size, 4096);
    assert_eq!(
        parsed.info_hash.unwrap().to_hex(),
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        parsed.magnet.as_deref(),
        Some("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Fixture")
    );
}

#[test]
fn topic_login_form_is_not_a_successful_topic() {
    for html in [
        LOGIN_FORM.to_owned(),
        format!("{LOGIN_FORM}<h1 id=\"topic-title\">Login required</h1>"),
    ] {
        assert!(matches!(
            topic::parser::parse(
                &html,
                TopicId::new(42),
                "https://rutracker.org/forum/viewtopic.php?t=42"
            ),
            Err(Error::NotAuthenticated)
        ));
    }
}

#[test]
fn only_a_recognized_topic_can_report_missing_magnet() {
    for html in [
        "",
        "<title>Maintenance</title>",
        "<h1 class='maintitle'>Maintenance</h1>",
    ] {
        assert!(matches!(
            topic::parser::parse(html, TopicId::new(1), "https://rutracker.org"),
            Err(Error::Parse { .. })
        ));
    }
    let topic = topic::parser::parse(
        "<h1 class='maintitle'>Fixture</h1><div class='post_body'>No public magnet</div>",
        TopicId::new(1),
        "https://rutracker.org",
    )
    .unwrap();
    assert!(topic.magnet.is_none());
}
