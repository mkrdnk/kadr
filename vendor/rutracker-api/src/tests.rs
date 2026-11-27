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
const LOGIN_FORM: &str = include_str!("fixtures/search/login.html");
const EMPTY_SEARCH: &str = include_str!("fixtures/search/empty.html");
const CHANGED_SEARCH: &str = include_str!("fixtures/search/changed-layout.html");
const MALFORMED_SEARCH: &str = include_str!("fixtures/search/malformed-rows.html");
const MIXED_SEARCH: &str = include_str!("fixtures/search/mixed.html");
const FIELDSET_TOPIC: &str = include_str!("fixtures/topic/fieldset.html");
const TABLE_TOPIC: &str = include_str!("fixtures/topic/table.html");
const TOPIC_URL: &str = "https://rutracker.org/forum/viewtopic.php?t=42";
const MAGNET: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
const MAGNET_ANCHOR: &str = r#"<a class="magnet-link" href="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&amp;dn=Fixture">Magnet</a>"#;

#[test]
fn parser_debug_and_trace_logging_is_disabled_at_compile_time() {
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Info);
}

// The server deliberately leaves the connection open after writing its fixture.
// Dropping the channel sender releases it; no EOF or terminating chunk is sent
// unless explicitly present in the fixture.
pub(crate) struct LoopbackServer {
    release: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.release.take();
        self.thread.take().unwrap().join().unwrap();
    }
}

pub(crate) async fn loopback_response(wire: &[u8]) -> (reqwest::Response, LoopbackServer) {
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
            401 => assert!(matches!(result, Err(Error::NotAuthenticated))),
            403 => assert!(matches!(result, Err(Error::Server(403)))),
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
        format!("{LOGIN_FORM}{EMPTY_SEARCH}"),
        format!("{LOGIN_FORM}{CHANGED_SEARCH}"),
        format!("{LOGIN_FORM}{MIXED_SEARCH}"),
    ] {
        assert!(matches!(
            search::parser::parse(&html, 1, &base),
            Err(Error::NotAuthenticated)
        ));
    }
}

#[test]
fn search_accepts_a_legitimate_empty_table() {
    let base = Url::parse("https://rutracker.org").unwrap();
    let parsed = search::parser::parse(EMPTY_SEARCH, 1, &base).unwrap();
    assert!(parsed.results.is_empty());
    assert_eq!(parsed.total_count, 0);
    assert_eq!(parsed.total_pages, 0);
    assert_eq!(parsed.page, 1);
}

#[test]
fn search_rejects_unknown_html_instead_of_reporting_no_results() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for html in [
        "",
        "<html><body>Temporary maintenance</body></html>",
        "<table id='tor-tbl'></table>",
        "<table id='tor-tbl'><tr><th>Title</th></tr></table>",
        "<table id='tor-tbl'><tr><td>Unreadable results</td></tr></table>",
        "<p class='med bold'>Unrelated number: 0</p><table id='tor-tbl'></table>",
        "<p class='med bold'>Результатов поиска: unreadable</p><table id='tor-tbl'></table>",
        "<p>Ничего не найдено</p>",
    ] {
        assert!(
            matches!(
                search::parser::parse(html, 1, &base),
                Err(Error::Parse { .. })
            ),
            "{html}"
        );
    }
}

#[test]
fn search_rejects_positive_totals_without_usable_rows() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for html in [
        CHANGED_SEARCH.to_owned(),
        CHANGED_SEARCH.replace("tor-tbl", "new-table"),
        "<p class='med bold'>Результатов поиска: 51</p>".to_owned(),
        format!("<p class='med bold'>Результатов поиска: 51</p>{EMPTY_SEARCH}"),
    ] {
        // Even an out-of-range page must not turn an unreadable response into
        // a cacheable empty success when the server advertises matches.
        for page in [1, 3] {
            assert!(matches!(
                search::parser::parse(&html, page, &base),
                Err(Error::Parse { .. })
            ));
        }
    }
}

#[test]
fn search_rejects_malformed_result_rows_even_without_a_counter() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for html in [
        MALFORMED_SEARCH.to_owned(),
        MALFORMED_SEARCH.replace("</tbody>", "<tr><td>Ничего не найдено</td></tr></tbody>"),
        format!("<p class='med bold'>Результатов поиска: 0</p>{MALFORMED_SEARCH}"),
    ] {
        assert!(matches!(
            search::parser::parse(&html, 1, &base),
            Err(Error::Parse { .. })
        ));
    }
}

#[test]
fn search_malformed_row_cannot_be_hidden_by_an_empty_marker() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for row in [
        "<tr class='hl-tr'><td>Missing link</td></tr>",
        "<tr id='tr-42'><td>Missing link</td></tr>",
        "<tr><td class='t-title-col'>Missing link</td></tr>",
        "<tr><td class='tor-size'>700 MiB</td></tr>",
        "<tr><td><a class='tLink'>Missing id</a></td></tr>",
        "<tr><td><a class='tLink' data-topic_id='invalid'>Bad id</a></td></tr>",
        "<tr><td><a class='tLink' data-topic_id='0'>Zero id</a></td></tr>",
        "<tr><td><a class='tLink' data-topic_id='42'> </a></td></tr>",
    ] {
        let html = EMPTY_SEARCH.replace("</tbody>", &format!("{row}</tbody>"));
        assert!(
            matches!(
                search::parser::parse(&html, 1, &base),
                Err(Error::Parse { .. })
            ),
            "{row}"
        );
    }
}

#[test]
fn search_accepts_an_explicit_zero_counter_with_a_results_table() {
    let base = Url::parse("https://rutracker.org").unwrap();
    let html = "<p class='med bold'>Результатов поиска: 0</p><table id='tor-tbl'></table>";
    let parsed = search::parser::parse(html, 1, &base).unwrap();
    assert!(parsed.results.is_empty());
    assert_eq!(parsed.total_count, 0);
    assert_eq!(parsed.total_pages, 0);
}

#[test]
fn search_keeps_usable_partial_results_without_inventing_metadata() {
    let base = Url::parse("https://rutracker.org").unwrap();
    for html in [
        MIXED_SEARCH.to_owned(),
        MIXED_SEARCH.replace("<p class=\"med bold\">Результатов поиска: 51</p>", ""),
    ] {
        let parsed = search::parser::parse(&html, 1, &base).unwrap();
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].id, TopicId::new(42));
        assert_eq!(parsed.results[0].title, "Фильм & «Тест» — HD");
        assert_eq!(parsed.results[0].author.as_deref(), Some("Fixture author"));
        assert_eq!(parsed.results[0].category, "Фильмы");
        assert_eq!(parsed.results[0].size, Some(734_003_200));
        assert_eq!(parsed.results[0].seeds, Some(12));
        assert_eq!(parsed.results[1].id, TopicId::new(45));
        assert_eq!(parsed.results[1].size, None);
        assert_eq!(parsed.results[1].seeds, None);
        assert_eq!(parsed.results[1].registered.timestamp(), 1_700_000_001);
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
    let id = TopicId::new(42);
    let parsed = topic::parser::parse(FIELDSET_TOPIC, id, TOPIC_URL).unwrap();
    assert_eq!(parsed.id, id);
    assert_eq!(parsed.url, TOPIC_URL);
    assert_eq!(parsed.title, "Тема & «Тест»");
    assert_eq!(parsed.author.as_deref(), Some("Original author"));
    assert_eq!(parsed.size, 4096);
    assert!(parsed.description_html.contains("Original description"));
    assert!(!parsed.description_html.contains("Reply"));
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
fn topic_accepts_table_attachment_and_preserves_dom_attribute_value() {
    let parsed = topic::parser::parse(TABLE_TOPIC, TopicId::new(42), TOPIC_URL).unwrap();
    assert_eq!(
        parsed.magnet.as_deref(),
        Some(
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Table&tr=https%3A%2F%2Ftracker.example%2Fannounce"
        )
    );
    let html = FIELDSET_TOPIC.replace("&amp;dn=Fixture", "&amp;dn=One%26Two&amp;amp;literal=1");
    let parsed = topic::parser::parse(&html, TopicId::new(42), TOPIC_URL).unwrap();
    assert_eq!(
        parsed.magnet,
        Some(format!("{MAGNET}&dn=One%26Two&amp;literal=1"))
    );
}

#[test]
fn topic_missing_magnet_never_uses_body_sidebar_reply_or_synthetic_hash() {
    let html = FIELDSET_TOPIC.replace(MAGNET_ANCHOR, "<a href='dl.php?t=42'>Torrent</a>");
    let parsed = topic::parser::parse(&html, TopicId::new(42), TOPIC_URL).unwrap();
    assert!(parsed.magnet.is_none());
    // A visible hash is metadata, not permission to synthesize a magnet.
    assert!(parsed.info_hash.is_some());

    let html = TABLE_TOPIC.replace(
        r#"<a class="magnet-link" href="magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&amp;dn=Table&#x26;tr=https%3A%2F%2Ftracker.example%2Fannounce">Magnet</a>"#,
        "",
    );
    assert!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL)
            .unwrap()
            .magnet
            .is_none()
    );
}

#[test]
fn topic_does_not_search_poster_sidebar_or_later_post_for_an_attachment() {
    let sidebar = r#"<table><tr><td class="message"><div class="post_body">Sidebar description</div><fieldset class="attach"><a class="magnet-link" href="magnet:?xt=urn:btih:dddddddddddddddddddddddddddddddddddddddd">Sidebar magnet</a></fieldset></td></tr></table>"#;
    let html = FIELDSET_TOPIC.replace(
        "<span class=\"nick nick-author\">Original author</span>",
        sidebar,
    );
    assert_eq!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL)
            .unwrap()
            .magnet,
        Some(format!("{MAGNET}&dn=Fixture"))
    );
    // A reply's attachment cannot make an unsupported original post usable.
    let html = FIELDSET_TOPIC.replacen("class=\"attach\"", "class=\"unknown\"", 1);
    assert!(matches!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL),
        Err(Error::Parse { .. })
    ));
}

#[test]
fn topic_accepts_supported_magnet_forms_without_rewriting_attributes() {
    for magnet in [
        MAGNET.to_owned(),
        format!("MAGNET:?xt=urn:btih:{}&dn=Case", "A".repeat(40)),
        format!("magnet:?xt=urn:btih:{}", "A".repeat(32)),
        format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32)),
        format!("magnet:?xt=urn%3Abtih%3A{}&dn=Encoded", "a".repeat(40)),
    ] {
        // A valid href is sufficient; a CSS class is not the URI's source.
        let html = FIELDSET_TOPIC.replace(MAGNET_ANCHOR, &format!("<a href='{magnet}'>Magnet</a>"));
        let parsed = topic::parser::parse(&html, TopicId::new(42), TOPIC_URL).unwrap();
        assert_eq!(parsed.magnet, Some(magnet));
    }
}

#[test]
fn topic_invalid_advertised_magnet_is_not_missing_and_never_uses_reply() {
    for anchor in [
        "<a class='magnet-link'>No href</a>",
        "<a class='magnet-link' href=''>Empty href</a>",
        "<a class='magnet-link' href='https://example.org'>Wrong scheme</a>",
        "<a class='magnet-link' href='javascript:alert(1)'>Wrong scheme</a>",
        "<a href='magnet:'>No query</a>",
        "<a href='magnet:?dn=OnlyName'>No hash</a>",
        "<a href='magnet:?xt=urn:btih:not-a-hash'>Bad hash</a>",
        "<a href='magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567#fragment'>Fragment</a>",
        "<a href='magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&#10;'>Control</a>",
    ] {
        let html = FIELDSET_TOPIC.replace(MAGNET_ANCHOR, anchor);
        assert!(
            matches!(
                topic::parser::parse(&html, TopicId::new(42), TOPIC_URL),
                Err(Error::Parse { .. })
            ),
            "{anchor}"
        );
    }
}

#[test]
fn topic_uses_only_first_unquoted_attachment_in_original_message() {
    let quoted = r#"<blockquote><fieldset class="attach"><a class="magnet-link" href="magnet:?xt=urn:btih:dddddddddddddddddddddddddddddddddddddddd">Quoted</a></fieldset></blockquote>"#;
    let html = FIELDSET_TOPIC.replace(
        "Original description",
        &format!("{quoted}Original description"),
    );
    assert_eq!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL)
            .unwrap()
            .magnet,
        Some(format!("{MAGNET}&dn=Fixture"))
    );
    let html = FIELDSET_TOPIC.replace(MAGNET_ANCHOR, "").replace(
        "</fieldset>",
        &format!("</fieldset><fieldset class='attach'>{MAGNET_ANCHOR}</fieldset>"),
    );
    assert!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL)
            .unwrap()
            .magnet
            .is_none()
    );
}

#[test]
fn quoted_original_permalink_cannot_turn_a_reply_into_the_original_post() {
    let html = FIELDSET_TOPIC.replacen(
        "class=\"p-link small\" href=\"viewtopic.php?t=42\"",
        "class=\"p-link small\" href=\"viewtopic.php?p=100#100\"",
        1,
    );
    let html = html.replace(
        "Original description",
        r#"<blockquote><div class="post_head"><a class="p-link" href="viewtopic.php?t=42">Quoted original</a></div></blockquote>Reply description"#,
    );
    assert!(matches!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL),
        Err(Error::Parse { .. })
    ));
}

#[test]
fn topic_login_form_is_not_a_successful_topic() {
    for html in [
        LOGIN_FORM.to_owned(),
        format!("{LOGIN_FORM}<h1 id=\"topic-title\">Login required</h1>"),
        format!("{LOGIN_FORM}{FIELDSET_TOPIC}"),
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
fn quick_login_widget_is_not_an_authentication_failure() {
    let widget = "<form id='login-form-quick'><input name='login_username'><input type='password' name='login_password'></form>";
    let html = format!("{widget}{FIELDSET_TOPIC}");
    assert_eq!(
        topic::parser::parse(&html, TopicId::new(42), TOPIC_URL)
            .unwrap()
            .magnet,
        Some(format!("{MAGNET}&dn=Fixture"))
    );
    let base = Url::parse("https://rutracker.org").unwrap();
    assert!(
        search::parser::parse(&format!("{widget}{EMPTY_SEARCH}"), 1, &base)
            .unwrap()
            .results
            .is_empty()
    );
}

#[test]
fn challenge_cannot_masquerade_as_an_empty_search_or_missing_magnet() {
    for challenge in [
        include_str!("fixtures/topic/challenge.html"),
        "<form id='challenge-form'></form>",
        "<div class='g-recaptcha'></div>",
    ] {
        let html = format!("{challenge}{}", FIELDSET_TOPIC.replace(MAGNET_ANCHOR, ""));
        assert!(matches!(
            topic::parser::parse(&html, TopicId::new(42), TOPIC_URL),
            Err(Error::Parse { .. })
        ));
        let base = Url::parse("https://rutracker.org").unwrap();
        assert!(matches!(
            search::parser::parse(&format!("{challenge}{EMPTY_SEARCH}"), 1, &base),
            Err(Error::Parse { .. })
        ));
    }
}

#[test]
fn only_a_recognized_topic_can_report_missing_magnet() {
    for html in [
        "".to_owned(),
        "<title>Maintenance</title>".to_owned(),
        "<h1 class='maintitle'>Maintenance</h1>".to_owned(),
        // The old synthetic fixtures were too weak to prove a topic layout.
        "<h1 class='maintitle'>Fixture</h1><div class='post_body'>No public magnet</div>"
            .to_owned(),
        include_str!("fixtures/topic/challenge.html").to_owned(),
        include_str!("fixtures/topic/malformed.html").to_owned(),
        FIELDSET_TOPIC.replace("id=\"topic_main\"", "id=\"changed-layout\""),
        format!("<table id='topic_main'></table>{FIELDSET_TOPIC}"),
        format!("<h1 class='maintitle'>Duplicate heading</h1>{FIELDSET_TOPIC}"),
        FIELDSET_TOPIC.replace("id=\"post_100\"", "id=\"post_invalid\""),
        FIELDSET_TOPIC.replace("id=\"post_100\"", "id=\"changed-post\""),
        FIELDSET_TOPIC.replace("id=\"post_100\"", ""),
        FIELDSET_TOPIC.replacen(
            "class=\"p-link small\" href=\"viewtopic.php?t=42\"",
            "class=\"p-link small\" href=\"viewtopic.php?p=101#101\"",
            1,
        ),
        FIELDSET_TOPIC.replace("class=\"post_head\"", "class=\"unknown\""),
        FIELDSET_TOPIC.replace("class=\"message\"", "class=\"changed-message\""),
        FIELDSET_TOPIC.replace("class=\"post_body\"", "class=\"changed-body\""),
        FIELDSET_TOPIC.replace("class=\"attach\"", "class=\"changed-attachment\""),
        FIELDSET_TOPIC.replace("viewtopic.php?t=42", "viewtopic.php?t=43"),
        FIELDSET_TOPIC.replace("viewtopic.php?t=42", "viewtopic.php?t=42&amp;start=30"),
        FIELDSET_TOPIC.replace("viewtopic.php?t=42", "viewtopic.php?t=42&amp;t=43"),
        FIELDSET_TOPIC.replace(
            "viewtopic.php?t=42",
            "https://example.org/forum/viewtopic.php?t=42",
        ),
    ] {
        assert!(
            matches!(
                topic::parser::parse(&html, TopicId::new(42), TOPIC_URL),
                Err(Error::Parse { .. })
            ),
            "{html}"
        );
    }
}
