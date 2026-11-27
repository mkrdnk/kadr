//! Login flow against `/forum/login.php`.

use reqwest::Method;
use scraper::Html;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::http;
use crate::parse_util::{element_text, has_challenge, has_login_form, selector};

/// The literal "Вход" submit-button value, sent as `login=%C2%F5%EE%E4`
/// (cp1251-encoded percent-escape).
const LOGIN_BUTTON_VALUE: &str = "Вход";

pub(crate) async fn login(client: &Client, username: &str, password: &str) -> Result<()> {
    if username.is_empty() || password.is_empty() {
        return Err(Error::InvalidArgument(
            "username and password must be non-empty".into(),
        ));
    }

    let body = http::cp1251_form_body([
        ("login_username", username),
        ("login_password", password),
        ("login", LOGIN_BUTTON_VALUE),
    ])
    .map_err(|_| Error::Authorization("unsupported credential encoding".into()))?;

    let url = client.url("/forum/login.php")?;

    let resp = http::send(
        client.http(),
        Method::POST,
        &url,
        Some(http::Body::Form(&body)),
    )
    .await?;

    read_response(resp).await
}

async fn read_response(resp: reqwest::Response) -> Result<()> {
    let resp = http::check_status(resp).await?;
    let html = http::read_html(resp).await?;
    parse_login_response(&html)
}

fn parse_login_response(html: &str) -> Result<()> {
    let doc = Html::parse_document(html);

    // A captcha/challenge is not evidence that the supplied credentials were
    // rejected, even if the response also contains the credential form.
    if has_challenge(&doc) {
        return Err(Error::parse("login", "unsupported login challenge"));
    }

    if has_login_form(&doc) {
        return Err(Error::Authorization("login form returned".into()));
    }

    // Require actual populated DOM markers, not names in scripts, comments or
    // arbitrary response text.
    if doc
        .select(selector!(
            "#logged-in-username, .logged-in-username, \
             #logged-in-as-uname, .logged-in-as-uname"
        ))
        .any(|marker| !element_text(marker).is_empty())
    {
        return Ok(());
    }

    Err(Error::parse("login", "unrecognized login response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN_FORM: &str = include_str!("fixtures/search/login.html");
    const SUCCESS: &str = "<span id='logged-in-username'>Fixture user</span>";

    #[test]
    fn login_success_requires_a_populated_dom_marker() {
        for html in [SUCCESS, "<b class='logged-in-as-uname'>Fixture user</b>"] {
            assert!(parse_login_response(html).is_ok(), "{html}");
        }
        for html in [
            "logged-in-username",
            "<script>var marker = 'logged-in-as-uname';</script>",
            "<!-- <span id=logged-in-username>user</span> -->",
            "<span data-marker='logged-in-username'>user</span>",
            "<span id='logged-in-username'> \n\t </span>",
            "<form id=login-form-full>Challenge</form>",
            "<h1>Checking your browser</h1>",
            "",
        ] {
            assert!(
                matches!(parse_login_response(html), Err(Error::Parse { .. })),
                "{html}"
            );
        }
    }

    #[test]
    fn returned_login_form_is_distinct_from_an_unsupported_challenge() {
        assert!(matches!(
            parse_login_response(LOGIN_FORM),
            Err(Error::Authorization(_))
        ));
        assert!(matches!(
            parse_login_response(&format!("{SUCCESS}{LOGIN_FORM}")),
            Err(Error::Authorization(_))
        ));
        for challenge in [
            "<input name=cap_code>",
            "<div class=g-recaptcha></div>",
            "<input name=cf-turnstile-response>",
        ] {
            assert!(matches!(
                parse_login_response(&format!("{LOGIN_FORM}{challenge}")),
                Err(Error::Parse { .. })
            ));
        }
    }

    #[tokio::test]
    async fn successful_http_status_still_requires_a_recognized_login_response() {
        for (body, expected) in [
            (SUCCESS, "success"),
            (LOGIN_FORM, "authorization"),
            ("<h1>Checking your browser</h1>", "parse"),
        ] {
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
            let result = read_response(response).await;
            drop(server);
            match expected {
                "success" => assert!(result.is_ok()),
                "authorization" => assert!(matches!(result, Err(Error::Authorization(_)))),
                "parse" => assert!(matches!(result, Err(Error::Parse { .. }))),
                _ => unreachable!(),
            }
        }
    }

    #[tokio::test]
    async fn login_checks_status_before_interpreting_body() {
        for body in [SUCCESS, LOGIN_FORM, "<h1>Checking your browser</h1>"] {
            for status in [401, 403, 429, 503] {
                let wire = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
                let result = read_response(response).await;
                drop(server);
                match status {
                    401 => assert!(matches!(result, Err(Error::NotAuthenticated))),
                    429 => assert!(matches!(result, Err(Error::RateLimited))),
                    _ => assert!(matches!(result, Err(Error::Server(code)) if code == status)),
                }
            }
        }
    }

    #[tokio::test]
    async fn login_response_is_bounded_before_parsing() {
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            http::MAX_RESPONSE_BYTES + 1
        );
        let (response, server) = crate::tests::loopback_response(wire.as_bytes()).await;
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(1), read_response(response)).await;
        drop(server);
        assert!(matches!(result, Ok(Err(Error::TooLarge))), "{result:?}");
    }
}
