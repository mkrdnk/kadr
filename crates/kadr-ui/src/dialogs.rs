use gpui::{FontWeight, MouseButton, rgba};

use super::*;

impl KadrView {
    pub(super) fn run_search(&mut self, open_result: bool, cx: &mut Context<Self>) {
        let Some(inputs) = &self.inputs else { return };
        let query = inputs.search.read(cx).value().trim().to_owned();
        if open_result
            && query == self.search_last
            && let Some(result) = self.model.results.get(self.search_cursor)
        {
            self.run_command(&Command::Emit(UiEvent::OpenMedia(result.id.clone())), cx);
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
        let state_visible = self.show_state(Section::Search, !self.model.results.is_empty());
        self.search_scroll
            .scroll_to_item(self.search_cursor + 1 + usize::from(state_visible));
        cx.notify();
    }

    pub(super) fn overlay(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Option<Div> {
        if self.model.preparing {
            return Some(self.modal_layer(None, self.preparation_dialog(width, height, cx), cx));
        }
        self.modal_from?;
        let dialog = match self.model.section {
            Section::Search => self.search_dialog(width, height, cx),
            Section::Sources => self.sources_dialog(width, height, cx),
            _ => return None,
        };
        Some(self.modal_layer(Some(Command::CloseOverlay), dialog, cx))
    }

    fn modal_layer(&self, close: Option<Command>, dialog: Div, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("scrim")
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x08080ad1))
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if let Some(close) = &close {
                                this.run_command(close, cx);
                            }
                        }),
                    ),
            )
            .child(
                dialog
                    .relative()
                    .occlude()
                    .when_some(self.modal_focus.handle(), |dialog, focus| {
                        dialog.track_focus(focus).tab_group().tab_stop(false)
                    }),
            )
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
        let header = div()
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
            );
        let content = div()
            .id("search-results")
            .track_scroll(&self.search_scroll)
            .overflow_y_scroll()
            .px(px(7.))
            .pb(px(8.))
            .child(
                div()
                    .px(px(9.))
                    .py(px(12.))
                    .text_size(px(10.))
                    .text_color(rgb(DIM))
                    .child(message),
            )
            .when(
                self.show_state(Section::Search, !results.is_empty()),
                |content| {
                    if matches!(state, SectionState::Empty(_)) {
                        content.child(div().px(px(9.)).pb(px(14.)).child(body(state.message())))
                    } else {
                        content.child(
                            div()
                                .px(px(7.))
                                .pb(px(8.))
                                .child(self.state_card(Section::Search, cx)),
                        )
                    }
                },
            )
            .children(results.iter().enumerate().map(|(index, media)| {
                let selected = index == self.search_cursor;
                let event = UiEvent::OpenMedia(media.id.clone());
                let enabled = self.accepts(&event);
                self.poster_action(("search-result", index), Command::Emit(event), enabled, cx)
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .h(px(58.))
                    .px(px(9.))
                    .rounded(px(5.))
                    .hover(|row| row.bg(rgb(HOVER)))
                    .when(selected, |row| row.bg(rgb(HOVER)))
                    .child(self.poster_image(media.poster_url.as_deref(), media.kind, 31.))
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
            }));
        let footer = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.))
            .min_h(px(35.))
            .px(px(14.))
            .py(px(7.))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(hint("↑ ↓", "Выбрать"))
            .child(hint("↵", "Найти / открыть"))
            .child(hint("Esc", "Закрыть"));
        modal::surface(width, height, 640., 480., header, content, footer)
    }

    fn preparation_dialog(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        let media = self.model.media.as_ref();
        let transfer = self.model.transfer.as_ref();
        let percent = transfer
            .and_then(|transfer| transfer.done.zip(transfer.total))
            .filter(|(_, total)| *total > 0)
            .map(|(done, total)| (done.min(total) as f32 / total as f32).clamp(0., 1.));
        let speed = transfer
            .and_then(|transfer| transfer.download_bps)
            .map(|bytes| format!("{:.1} МБ/с", bytes as f64 / 1_000_000.));
        let header = div()
            .px(px(20.))
            .py(px(15.))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(eyebrow("ПОДГОТОВКА ПОТОКА"));
        let content = div()
            .id("preparation-body")
            .overflow_y_scroll()
            .p(px(20.))
            .child(
                div()
                    .flex()
                    .gap(px(20.))
                    .when_some(media, |content, media| {
                        content.child(self.poster_image(
                            media.poster_url.as_deref(),
                            media.kind,
                            82.,
                        ))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(19.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(
                                        media
                                            .map_or(
                                                "Запуск воспроизведения",
                                                |media| media.title.as_str(),
                                            )
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
                                        .child(
                                            div().h_full().w(gpui::relative(value)).bg(rgb(ACCENT)),
                                        ),
                                )
                            })
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
                    ),
            );
        let footer = div()
            .flex()
            .justify_end()
            .p(px(14.))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(self.button(
                "cancel-preparation",
                "Отменить",
                Command::Emit(UiEvent::Stop),
                false,
                true,
                cx,
            ));
        modal::surface(width, height, 470., 320., header, content, footer)
    }
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
