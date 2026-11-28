//! Shared poster presentation. Callers own layout, metadata and commands.

use gpui::{
    Context, Div, ElementId, ObjectFit, SharedString, Stateful, div, img, prelude::*, px, rgb, rgba,
};

use super::{ACCENT, Command, KadrView, MUTED, MediaKind, PANEL, TEXT, icon};

const POSTER_GROUP: &str = "poster-card";

impl KadrView {
    /// Transparent interactive shell for both poster grids and compact rows.
    ///
    /// Keep secondary actions outside this shell, so they remain independent
    /// keyboard stops rather than nested click targets.
    pub(super) fn poster_action(
        &self,
        id: impl Into<ElementId>,
        command: Command,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.action(id, command, enabled, cx)
            .min_w_0()
            .border_1()
            .border_color(rgba(0x00000000))
            .rounded_md()
            .text_color(rgb(TEXT))
            .when(enabled, |card| {
                card.group(POSTER_GROUP)
                    .hover(|style| style.text_color(rgb(ACCENT)))
                    .focus(|style| style.border_color(rgb(ACCENT)).text_color(rgb(ACCENT)))
            })
    }

    /// A fixed 2:3 poster with no badge, overlay, or hover background.
    pub(super) fn poster_image(&self, url: Option<&str>, kind: MediaKind, width: f32) -> Div {
        let url = poster_url(
            url,
            self.preferences_loaded
                && self.applied_catalog_enabled
                && self.model.preferences.catalog_enabled,
        );
        let glyph = match kind {
            MediaKind::Movie => "film",
            MediaKind::Series => "tv",
        };
        div()
            .w(px(width))
            .h(px(width * 1.5))
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgba(0xffffff12))
            .group_hover(POSTER_GROUP, |style| style.border_color(rgb(ACCENT)))
            .bg(rgb(PANEL))
            .flex()
            .items_center()
            .justify_center()
            .when_some(url, |poster, url| {
                poster.child(
                    img(url.to_owned())
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .with_loading(move || icon(glyph, 24., MUTED).into_any_element())
                        .with_fallback(move || icon(glyph, 24., MUTED).into_any_element()),
                )
            })
            .when(url.is_none(), |poster| {
                poster.child(icon(glyph, 24., MUTED))
            })
    }
}

/// Inherit the shell's hover/focus color; constrain even unbroken long titles.
pub(super) fn poster_title(title: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .w_full()
        .truncate()
        .text_size(px(12.))
        .child(title.into())
}

fn poster_url(url: Option<&str>, enabled: bool) -> Option<&str> {
    url.filter(|url| enabled && (url.starts_with("https://") || url.starts_with("http://")))
}

#[cfg(test)]
mod tests {
    use super::poster_url;

    #[test]
    fn poster_requests_require_enabled_http_images() {
        for url in [
            "https://example.com/poster.jpg",
            "http://example.com/poster.jpg",
        ] {
            assert_eq!(poster_url(Some(url), true), Some(url));
            assert_eq!(poster_url(Some(url), false), None);
        }
        for url in [
            "",
            "file:///poster.jpg",
            "data:image/png;base64,abc",
            "/poster.jpg",
        ] {
            assert_eq!(poster_url(Some(url), true), None);
        }
        assert_eq!(poster_url(None, true), None);
    }
}
