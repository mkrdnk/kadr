//! Internal utilities shared between HTML parsers.

/// Compile a CSS [`scraper::Selector`] once per call site and reuse it.
///
/// The selector is stored in a `static LazyLock` local to each macro
/// expansion — no global cache, no `Mutex`, no allocation per parse. Each
/// distinct CSS literal in the codebase produces exactly one `Selector` for
/// the lifetime of the process.
///
/// Panics on the first invocation if `$css` is not a valid selector. Since
/// every call site uses a string literal validated by tests, this is a
/// programmer error and never reachable in production.
macro_rules! selector {
    ($css:literal) => {{
        static SEL: ::std::sync::LazyLock<::scraper::Selector> = ::std::sync::LazyLock::new(|| {
            ::scraper::Selector::parse($css)
                .unwrap_or_else(|e| panic!("invalid CSS selector {:?}: {e}", $css))
        });
        &*SEL
    }};
}

pub(crate) use selector;

/// Known interstitials can retain parts of the normal page underneath them.
pub(crate) fn has_challenge(doc: &scraper::Html) -> bool {
    doc.select(selector!(
        "form#challenge-form, #cf-challenge-running, \
         input[name='cap_code'], input[name='captcha'], \
         [name='g-recaptcha-response'], [name='h-captcha-response'], \
         [name='cf-turnstile-response'], .g-recaptcha, .h-captcha, .cf-turnstile"
    ))
    .next()
    .is_some()
}

/// Recognize the full login page, not the quick-login widget on public pages
/// or an unrelated element reusing the form ID.
pub(crate) fn has_login_form(doc: &scraper::Html) -> bool {
    doc.select(selector!("form#login-form-full")).any(|form| {
        form.select(selector!("input[name='login_username']"))
            .next()
            .is_some()
            && form
                .select(selector!("input[name='login_password'][type='password' i]"))
                .next()
                .is_some()
    })
}

/// Collapse all runs of ASCII / Unicode whitespace into single spaces and
/// trim leading/trailing whitespace.
pub(crate) fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for word in s.split_whitespace() {
        if !first {
            out.push(' ');
        }
        out.push_str(word);
        first = false;
    }
    out
}

/// Concatenate text content of `el` and pass through [`collapse_ws`]. Common
/// idiom in the HTML parsers — a row cell's visible text.
pub(crate) fn element_text(el: scraper::ElementRef<'_>) -> String {
    collapse_ws(&el.text().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `selector!` panics on first invocation if the literal is invalid.
    /// Smoke-test all parser entry points to force initialization of every
    /// `LazyLock<Selector>` they use, so a malformed selector is caught by
    /// `cargo test` instead of in production.
    #[test]
    fn parsers_initialise_all_selectors() {
        use crate::models::TopicId;
        use url::Url;

        let base = Url::parse("https://rutracker.org").unwrap();
        let _ = crate::search::parser::parse(include_str!("fixtures/search/mixed.html"), 1, &base)
            .unwrap();
        let _ = crate::topic::parser::parse(
            include_str!("fixtures/topic/fieldset.html"),
            TopicId::new(42),
            "https://rutracker.org/forum/viewtopic.php?t=42",
        )
        .unwrap();
    }

    #[test]
    fn collapse_ws_trims_and_collapses() {
        assert_eq!(collapse_ws("  foo   bar  "), "foo bar");
        assert_eq!(collapse_ws("\n\tfoo\n\tbar\t"), "foo bar");
        assert_eq!(collapse_ws(""), "");
    }

    #[test]
    fn login_detection_requires_both_inputs_in_the_same_form() {
        for html in [
            include_str!("fixtures/search/login.html"),
            "<form id=login-form-full><input name=login_username><input name=login_password type=PASSWORD></form>",
        ] {
            assert!(
                has_login_form(&scraper::Html::parse_document(html)),
                "{html}"
            );
        }
        for html in [
            "<form id=login-form-full>Challenge</form>",
            "<form id=login-form-quick><input name=login_username><input name=login_password type=password></form>",
            "<form><input name=login_username></form>",
            "<form><input name=login_username><input name=login_password></form>",
            "<form><input name=login_username></form><form><input name=login_password type=password></form>",
            "<input name=login_username><input name=login_password type=password>",
            "<script>var marker = 'login-form-full login_username login_password';</script>",
        ] {
            assert!(
                !has_login_form(&scraper::Html::parse_document(html)),
                "{html}"
            );
        }
    }
}
