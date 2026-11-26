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
        let _ = crate::search::parser::parse("<table id='tor-tbl'></table>", 1, &base).unwrap();
        let _ = crate::topic::parser::parse(
            "<h1 class='maintitle'>Fixture</h1><div class='post_body'></div>",
            TopicId::new(1),
            "https://rutracker.org",
        )
        .unwrap();
    }

    #[test]
    fn collapse_ws_trims_and_collapses() {
        assert_eq!(collapse_ws("  foo   bar  "), "foo bar");
        assert_eq!(collapse_ws("\n\tfoo\n\tbar\t"), "foo bar");
        assert_eq!(collapse_ws(""), "");
    }
}
