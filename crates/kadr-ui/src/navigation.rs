//! Sidebar presentation. The parent owns collapse state and source dialogs.

use gpui::{
    Context, Div, FontWeight, Render, SharedString, Stateful, Window, div, prelude::*, px, rgb,
    rgba,
};

use super::{
    ACCENT, BORDER, Command, DIM, ELEVATED, HOVER, KadrView, MUTED, SIDEBAR, Section, TEXT, icon,
};

impl KadrView {
    pub(super) fn navigation(&self, width: f32, collapsed: bool, cx: &mut Context<Self>) -> Div {
        let page = self.page();
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(sidebar_width(width, collapsed)))
            .px(px(if collapsed { 8. } else { 12. }))
            .pt(px(16.))
            .pb(px(12.))
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(0x2a2a2e))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(collapsed, |header| header.justify_center())
                    .h(px(38.))
                    .mb(px(16.))
                    .when(!collapsed, |header| {
                        header.child(
                            self.action("brand", Command::Navigate(Section::Home), true, cx)
                                .flex()
                                .items_center()
                                .px_1()
                                .rounded(px(5.))
                                .border_1()
                                .border_color(rgba(0x00000000))
                                .text_size(px(27.))
                                .font_weight(FontWeight::BOLD)
                                .child("kadr")
                                .child(div().text_color(rgb(ACCENT)).child(".")),
                        )
                    })
                    .child(with_tooltip(
                        self.action("toggle-sidebar", Command::ToggleSidebar, true, cx)
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(34.))
                            .flex_shrink_0()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(rgba(0x00000000))
                            .hover(|style| style.bg(rgb(HOVER)))
                            .child(icon("panel-left", 18., MUTED)),
                        if collapsed {
                            "Развернуть боковую панель"
                        } else {
                            "Свернуть боковую панель"
                        },
                    )),
            )
            .child(with_tooltip(
                self.action("open-search", Command::OpenSearch, true, cx)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(36.))
                    .px(px(if collapsed { 0. } else { 8. }))
                    .mb(px(14.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(ELEVATED))
                    .text_color(rgb(MUTED))
                    .text_size(px(12.))
                    .when(collapsed, |item| item.justify_center())
                    .child(icon("search", 17., MUTED))
                    .when(!collapsed, |item| {
                        item.child(div().flex_1().min_w_0().child("Поиск")).child(
                            div()
                                .text_size(px(10.))
                                .text_color(rgb(DIM))
                                .child(shortcut("K")),
                        )
                    })
                    .hover(|item| item.bg(rgb(HOVER))),
                format!("Поиск · {}", shortcut("K")),
            ))
            .children(
                [Section::Home, Section::Movies, Section::Shows]
                    .into_iter()
                    .map(|section| self.navigation_item(section, page == section, collapsed, cx)),
            )
            .child(
                div()
                    .mt(px(if collapsed { 14. } else { 22. }))
                    .mb(px(7.))
                    .when(collapsed, |separator| {
                        separator.mx_2().border_t_1().border_color(rgb(BORDER))
                    })
                    .when(!collapsed, |label| {
                        label
                            .px_2()
                            .text_size(px(10.))
                            .text_color(rgb(DIM))
                            .child("БИБЛИОТЕКА")
                    }),
            )
            .children(
                [Section::Library, Section::History, Section::Transfers]
                    .into_iter()
                    .map(|section| self.navigation_item(section, page == section, collapsed, cx)),
            )
            .child(div().flex_1().min_h(px(20.)))
            .child(with_tooltip(
                self.action("open-source", Command::OpenSource, true, cx)
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .h(px(36.))
                    .my(px(1.))
                    .px(px(if collapsed { 0. } else { 8. }))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(rgba(0x00000000))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .when(collapsed, |item| item.justify_center())
                    .child(icon("link", 17., MUTED))
                    .when(!collapsed, |item| {
                        item.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child("Открыть источник…"),
                        )
                    })
                    .hover(|item| item.bg(rgb(HOVER)).text_color(rgb(TEXT))),
                format!("Открыть источник… · {}", shortcut("O")),
            ))
            .child(self.navigation_item(
                Section::Settings,
                page == Section::Settings,
                collapsed,
                cx,
            ))
    }

    fn navigation_item(
        &self,
        section: Section,
        selected: bool,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg = if selected { ACCENT } else { MUTED };
        with_tooltip(
            self.action(section.id(), Command::Navigate(section), true, cx)
                .flex()
                .items_center()
                .gap(px(9.))
                .h(px(36.))
                .my(px(1.))
                .px(px(if collapsed { 0. } else { 8. }))
                .rounded(px(5.))
                .border_1()
                .border_color(rgba(0x00000000))
                .text_size(px(12.))
                .text_color(rgb(fg))
                .when(collapsed, |item| item.justify_center())
                .when(selected, |item| item.bg(rgb(ELEVATED)))
                .hover(move |item| {
                    item.bg(rgb(if selected { HOVER } else { 0x242428 }))
                        .text_color(rgb(if selected { ACCENT } else { TEXT }))
                })
                .child(icon(navigation_icon(section), 17., fg))
                .when(!collapsed, |item| {
                    item.child(div().flex_1().min_w_0().truncate().child(section.title()))
                }),
            section.title(),
        )
    }
}

pub(super) fn sidebar_width(width: f32, collapsed: bool) -> f32 {
    if collapsed {
        60.
    } else if width <= 900. {
        174.
    } else if width <= 1120. {
        190.
    } else {
        220.
    }
}

fn navigation_icon(section: Section) -> &'static str {
    match section {
        Section::Home => "home",
        Section::Movies => "film",
        Section::Shows => "tv",
        Section::Search => "search",
        Section::Library => "bookmark",
        Section::History => "clock",
        Section::Transfers => "download",
        Section::Sources => "link",
        Section::Settings => "settings",
    }
}

fn shortcut(key: &str) -> String {
    format!(
        "{} {key}",
        if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl"
        }
    )
}

fn with_tooltip(button: Stateful<Div>, label: impl Into<SharedString>) -> Stateful<Div> {
    let label = label.into();
    button.tooltip(move |_, cx| cx.new(|_| NavigationTooltip(label.clone())).into())
}

struct NavigationTooltip(SharedString);

impl Render for NavigationTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(ELEVATED))
            .text_size(px(12.))
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource;

    #[test]
    fn collapsed_width_is_independent_of_window_width() {
        for width in [480., 760., 900., 1120., 1600.] {
            assert_eq!(sidebar_width(width, true), 60.);
        }
    }

    #[test]
    fn expanded_sidebar_stays_available_at_compact_window_sizes() {
        for (width, expected) in [
            (760., 174.),
            (900., 174.),
            (901., 190.),
            (1120., 190.),
            (1121., 220.),
        ] {
            assert_eq!(sidebar_width(width, false), expected);
        }
    }

    #[test]
    fn every_navigation_glyph_is_embedded() {
        let assets = super::super::icons::IconAssets;
        for section in Section::ALL {
            let path = format!("kadr/{}.svg", navigation_icon(section));
            assert!(assets.load(&path).unwrap().is_some(), "{path}");
        }
        assert!(assets.load("kadr/panel-left.svg").unwrap().is_some());
    }
}
