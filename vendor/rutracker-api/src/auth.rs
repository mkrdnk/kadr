//! Login flow against `/forum/login.php`.

use reqwest::Method;

use crate::client::Client;
use crate::error::{Error, Result};
use crate::http;

/// The literal "Вход" submit-button value, sent as `login=%C2%F5%EE%E4`
/// (cp1251-encoded percent-escape).
const LOGIN_BUTTON_VALUE: &str = "Вход";

/// HTML markers indicating a successful login. Each is checked in turn.
const LOGIN_SUCCESS_MARKERS: &[&str] = &["logged-in-username", "logged-in-as-uname"];

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

    let resp = http::check_status(resp).await?;
    let status = resp.status();
    let html = http::read_html(resp).await?;

    if LOGIN_SUCCESS_MARKERS.iter().any(|m| html.contains(m)) {
        return Ok(());
    }

    Err(Error::Authorization(format!(
        "login failed (status {status})"
    )))
}
