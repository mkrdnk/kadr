use gpui::{FontWeight, MouseButton, ObjectFit, img, rgba};

use super::*;

impl KadrView {
    pub(super) fn run_search(&mut self, open_result: bool, cx: &mut Context<Self>) {
        let Some(inputs) = &self.inputs else { return };
        let query = inputs.search.read(cx).value().trim().to_owned();
        if open_result
            && query == self.search_last
            && let Some(result) = self.model.results.get(self.search_cursor)
        {
            let command = Command::Emit(UiEvent::OpenMedia(result.id.clone()));
            self.run_command(&command, cx);
            return;
        }
        self.search_last = query;
        self.submit_search(cx);
    }

    pub(super) fn move_search_cursor(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.model.results.len();
        if count == 0 {
            return;
        }
        self.search_cursor = if down {
            (self.search_cursor + 1).min(count - 1)
        } else {
            self.search_cursor.saturating_sub(1)
        };
        self.search_scroll.scroll_to_item(self.search_cursor);
        cx.notify();
    }

    pub(super) fn overlay(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Option<Div> {
        if self.model.preparing {
            return Some(self.modal_layer(false, None, self.preparation_dialog(width, cx), cx));
        }
        self.modal_from?;
        match self.model.section {
            Section::Search => Some(self.modal_layer(
                true,
                Some(Command::CloseOverlay),
                self.search_dialog(width, height, cx),
                cx,
            )),
            Section::Sources => Some(self.modal_layer(
                false,
                Some(Command::CloseOverlay),
                self.sources_dialog(width, height, cx),
                cx,
            )),
            _ => None,
        }
    }

    fn modal_layer(
        &self,
        search: bool,
        close: Option<Command>,
        dialog: Div,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .when(search, |layer| layer.items_start().pt(px(95.)))
            .when(!search, |layer| layer.items_center())
            .child(
                div()
                    .id("scrim")
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x08080ad1))
                    .occlude()
                    .when_some(close, |scrim, close| {
                        scrim.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.run_command(&close, cx);
                            }),
                        )
                    }),
            )
            .child(dialog.relative().occlude())
    }

    fn search_dialog(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        let inputs = self
            .inputs
            .as_ref()
            .expect("inputs initialized before rendering");
        let state = &self.model.states[Section::Search.index()];
        let results = &self.model.results;
        let message = match state {
            SectionState::Loading(_) => "ПОИСК…".into(),
            SectionState::Error(_) => "ОШИБКА ПОИСКА".into(),
            SectionState::Unavailable(_) => "КАТАЛОГ НЕДОСТУПЕН".into(),
            SectionState::Empty(_) if !results.is_empty() => {
                format!("РЕЗУЛЬТАТЫ · {}", results.len())
            }
            SectionState::Empty(_) => "ФИЛЬМЫ И СЕРИАЛЫ".into(),
        };
        dialog_surface(640.0_f32.min(width - 44.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .h(px(56.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(icon("search", 20., MUTED))
                    .child(div().flex_1().min_w_0().child(inputs.search.clone()))
                    .child(
                        self.action("close-search", Command::CloseOverlay, true, cx)
                            .child(keycap("Esc")),
                    )
                    .child(
                        self.action("submit-search", Command::Search, true, cx)
                            .child(keycap("Найти")),
                    ),
            )
            .child(
                div()
                    .px(px(16.))
                    .pt(px(14.))
                    .pb(px(8.))
                    .text_size(px(10.))
                    .text_color(rgb(DIM))
                    .child(message),
            )
            .when(
                self.show_state(Section::Search, !results.is_empty()),
                |dialog| {
                    if matches!(state, SectionState::Empty(_)) {
                        dialog.child(div().px(px(16.)).pb(px(14.)).child(body(state.message())))
                    } else {
                        dialog.child(
                            div()
                                .px(px(14.))
                                .pb(px(8.))
                                .child(self.state_card(Section::Search, cx)),
                        )
                    }
                },
            )
            .child(
                div()
                    .id("search-results")
                    .track_scroll(&self.search_scroll)
                    .overflow_y_scroll()
                    .max_h(px((height - 280.).clamp(116., 294.)))
                    .px(px(7.))
                    .pb(px(8.))
                    .children(results.iter().enumerate().map(|(index, media)| {
                        let selected = index == self.search_cursor;
                        self.action(
                            ("search-result", index),
                            Command::Emit(UiEvent::OpenMedia(media.id.clone())),
                            !self.model.playback_active,
                            cx,
                        )
                        .flex()
                        .items_center()
                        .gap(px(11.))
                        .h(px(58.))
                        .px(px(9.))
                        .rounded(px(5.))
                        .hover(|row| row.bg(rgb(HOVER)))
                        .when(selected, |row| row.bg(rgb(HOVER)))
                        .child(dialog_poster(
                            media
                                .poster_url
                                .as_ref()
                                .filter(|_| self.applied_catalog_enabled),
                            31.,
                            46.,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(media.title.clone()),
                                )
                                .child(
                                    div().text_size(px(10.)).text_color(rgb(MUTED)).child(
                                        media
                                            .year
                                            .map(|year| format!("{year} · "))
                                            .unwrap_or_default()
                                            + match media.kind {
                                                MediaKind::Movie => "Фильм",
                                                MediaKind::Series => "Сериал",
                                            },
                                    ),
                                ),
                        )
                        .when(selected, |row| {
                            row.child(
                                div()
                                    .text_color(rgb(MUTED))
                                    .text_size(px(10.))
                                    .child("↵ Открыть"),
                            )
                        })
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .h(px(35.))
                    .px(px(14.))
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(hint("↑ ↓", "Выбрать"))
                    .child(hint("↵", "Найти / открыть"))
                    .child(hint("Esc", "Закрыть")),
            )
    }

    fn sources_dialog(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        let has_rows = !self.model.sources.is_empty();
        let title = self
            .model
            .media
            .as_ref()
            .map_or("Источники", |media| media.title.as_str());
        dialog_surface(880.0_f32.min(width - 44.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .h(px(74.))
                    .px(px(21.))
                    .border_b_1()
                    .border_color(rgb(HOVER))
                    .child(
                        div().min_w_0().child(eyebrow("ВЫБЕРИТЕ ИСТОЧНИК")).child(
                            div()
                                .truncate()
                                .text_size(px(19.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title.to_owned()),
                        ),
                    )
                    .child(
                        self.action("close-sources", Command::CloseOverlay, true, cx)
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(34.))
                            .flex_shrink_0()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(ELEVATED))
                            .hover(|button| button.bg(rgb(HOVER)))
                            .child(icon("close", 17., MUTED)),
                    ),
            )
            .when(has_rows, |dialog| dialog.child(self.source_toolbar(cx)))
            .child(
                div()
                    .id("sources-dialog-scroll")
                    .overflow_y_scroll()
                    .max_h(px((height - if has_rows { 198. } else { 152. }).max(170.)))
                    .p(px(16.))
                    .child(self.sources(cx)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(36.))
                    .px(px(20.))
                    .border_t_1()
                    .border_color(rgb(HOVER))
                    .child(status_dot())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(MUTED))
                            .child(format!(
                                "Плеер по умолчанию: {}",
                                self.model.default_player.label()
                            )),
                    ),
            )
    }

    fn source_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px(px(16.))
            .h(px(46.))
            .border_b_1()
            .border_color(rgb(HOVER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .p(px(2.))
                    .rounded(px(5.))
                    .bg(rgb(BACKGROUND))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .children(
                        SourceQuality::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(i, quality)| {
                                let selected = quality == self.source_quality;
                                self.action(
                                    ("source-quality", i),
                                    Command::SourceQuality(quality),
                                    true,
                                    cx,
                                )
                                .px(px(9.))
                                .h(px(25.))
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .text_size(px(11.))
                                .text_color(rgb(if selected { TEXT } else { MUTED }))
                                .when(selected, |tab| tab.bg(rgb(HOVER)))
                                .hover(|tab| tab.bg(rgb(ELEVATED)))
                                .child(quality.label())
                            }),
                    ),
            )
            .child(
                self.action("source-sort", Command::ToggleSourceOrder, true, cx)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .child(if self.source_order_seeds {
                        "Сначала больше сидов"
                    } else {
                        "Порядок источников"
                    })
                    .child(icon("chevron", 13., DIM)),
            )
    }

    fn preparation_dialog(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let media = self.model.media.as_ref();
        let transfer = self.model.transfer.as_ref();
        let percent = transfer
            .and_then(|transfer| transfer.done.zip(transfer.total))
            .filter(|(_, total)| *total > 0)
            .map(|(done, total)| (done.min(total) as f32 / total as f32).clamp(0., 1.));
        let speed = transfer
            .and_then(|transfer| transfer.download_bps)
            .map(|bytes| format!("{:.1} МБ/с", bytes as f64 / 1_000_000.));
        dialog_surface(470.0_f32.min(width - 44.))
            .flex()
            .flex_row()
            .gap(px(20.))
            .p(px(20.))
            .child(dialog_poster(
                media
                    .and_then(|media| media.poster_url.as_ref())
                    .filter(|_| self.applied_catalog_enabled),
                105.,
                157.,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .items_start()
                    .child(eyebrow("ПОДГОТОВКА ПОТОКА"))
                    .child(
                        div()
                            .text_size(px(21.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                media
                                    .map_or("Запуск воспроизведения", |media| {
                                        media.title.as_str()
                                    })
                                    .to_owned(),
                            ),
                    )
                    .child(body(self.model.default_player.label()))
                    .when_some(percent, |content, value| {
                        content.child(
                            div()
                                .w_full()
                                .h(px(4.))
                                .mt(px(6.))
                                .rounded_full()
                                .overflow_hidden()
                                .bg(rgb(BORDER))
                                .child(div().h_full().w(gpui::relative(value)).bg(rgb(ACCENT))),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .w_full()
                            .gap(px(8.))
                            .child(body(transfer.map_or(
                                "Открываем внешний плеер…",
                                |transfer| transfer.stage.as_str(),
                            )))
                            .when_some(speed, |content, speed| {
                                content.child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(ACCENT))
                                        .child(speed),
                                )
                            }),
                    )
                    .child(self.button(
                        "cancel-preparation",
                        "Отменить",
                        Command::Emit(UiEvent::Stop),
                        false,
                        true,
                        cx,
                    )),
            )
    }
}

fn dialog_surface(width: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .w(px(width))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(0x404046))
        .bg(rgb(PANEL))
        .shadow_lg()
        .overflow_hidden()
}

fn dialog_poster(url: Option<&String>, width: f32, height: f32) -> Div {
    div()
        .relative()
        .w(px(width))
        .h(px(height))
        .flex_shrink_0()
        .overflow_hidden()
        .rounded(px(4.))
        .bg(rgb(ELEVATED))
        .flex()
        .items_center()
        .justify_center()
        .child(icon("film", width.min(27.) * 0.72, DIM))
        .when_some(url.cloned(), |frame, url| {
            frame.child(
                img(url)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
        })
}

fn keycap(label: &'static str) -> Div {
    div()
        .rounded(px(3.))
        .bg(rgb(ELEVATED))
        .border_1()
        .border_color(rgb(BORDER))
        .px(px(5.))
        .py(px(2.))
        .text_size(px(10.))
        .text_color(rgb(MUTED))
        .child(label)
}

fn hint(key: &'static str, label: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .text_size(px(10.))
        .text_color(rgb(DIM))
        .child(keycap(key))
        .child(label)
}
