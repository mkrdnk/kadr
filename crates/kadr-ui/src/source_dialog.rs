//! Explicit source selection and manual import. Playback locators stay in the controller.
use super::*;
use gpui::FontWeight;

#[derive(Default)]
pub(super) struct SourceDialogState {
    importing: bool,
    attach: bool,
    selected: Option<usize>,
    manual_selection: bool,
    expanded: Option<usize>,
    diagnostics: bool,
}

impl SourceDialogState {
    pub(super) fn show_picker(&mut self) {
        self.importing = false;
        self.reset();
    }

    pub(super) fn reset(&mut self) {
        self.selected = None;
        self.manual_selection = false;
        self.expanded = None;
        self.diagnostics = false;
    }

    fn reconcile(&mut self, rows: &[&SourceRow]) {
        if !self.manual_selection || !rows.iter().any(|row| Some(row.index) == self.selected) {
            self.manual_selection = false;
            self.selected = rows
                .iter()
                .find(|row| row.recommended && !row.possible_match)
                .or_else(|| rows.iter().find(|row| !row.possible_match))
                .map(|row| row.index);
        }
        if !rows.iter().any(|row| Some(row.index) == self.expanded) {
            self.expanded = None;
        }
    }
}

#[derive(Clone)]
pub(super) enum SourceDialogCommand {
    OpenImport,
    BackToSources,
    Select(usize),
    ToggleDetails(usize),
    ToggleDiagnostics,
    ToggleAttach,
    ImportClipboard,
    ImportTorrent,
    Watch,
}

impl KadrView {
    pub(super) fn sync_source_dialog(&mut self) {
        if !self.model.files.is_empty() && !self.source_dialog.manual_selection {
            self.source_dialog.selected = None;
            return;
        }
        // Preserve engine indices across sorting, filtering and partial results.
        let rows: Vec<_> = self
            .model
            .sources
            .iter()
            .filter(|row| self.source_quality.matches(row))
            .collect();
        self.source_dialog.reconcile(&rows);
    }

    pub(super) fn run_source_dialog_command(
        &mut self,
        command: &SourceDialogCommand,
        cx: &mut Context<Self>,
    ) {
        match command {
            SourceDialogCommand::OpenImport => {
                self.source_dialog.importing = true;
                self.source_dialog.attach = self.detail_open && self.model.media.is_some();
                self.show_overlay(Section::Sources, cx);
            }
            SourceDialogCommand::BackToSources => self.source_dialog.importing = false,
            SourceDialogCommand::Select(index) => {
                if self.visible_sources().iter().any(|row| row.index == *index)
                    && self.model.accepts(&UiEvent::ChooseSource(*index))
                {
                    self.source_dialog.selected = Some(*index);
                    self.source_dialog.manual_selection = true;
                }
            }
            SourceDialogCommand::ToggleDetails(index) => {
                self.source_dialog.expanded =
                    (self.source_dialog.expanded != Some(*index)).then_some(*index);
            }
            SourceDialogCommand::ToggleDiagnostics => {
                self.source_dialog.diagnostics = !self.source_dialog.diagnostics;
            }
            SourceDialogCommand::ToggleAttach => {
                self.source_dialog.attach = !self.source_dialog.attach;
            }
            SourceDialogCommand::ImportClipboard | SourceDialogCommand::ImportTorrent => {
                if self.source_dialog.importing {
                    self.run_command(
                        &Command::Emit(UiEvent::ImportSource {
                            torrent: matches!(command, SourceDialogCommand::ImportTorrent),
                            attach: self.source_dialog.attach && self.model.media.is_some(),
                        }),
                        cx,
                    );
                }
            }
            SourceDialogCommand::Watch => {
                if let Some(index) = self.source_dialog.selected {
                    self.source_dialog.manual_selection = false;
                    self.run_command(&Command::Emit(UiEvent::WatchSource(index)), cx);
                } else {
                    self.run_command(&Command::Emit(UiEvent::Play), cx);
                }
            }
        }
        cx.notify();
    }

    pub(super) fn sources_dialog(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        if self.source_dialog.importing {
            return self.import_dialog(width, height, cx);
        }
        let busy = self.source_loading();
        let has_rows = !self.model.sources.is_empty();
        let subtitle = self.source_dialog_subtitle();
        let header = self.source_header("ИСТОЧНИКИ", "Выбор источника", subtitle.as_deref(), cx);
        let content = div()
            .id("source-picker-body")
            .overflow_y_scroll()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(self.compact_discovery_status(cx))
            .when(has_rows, |body| body.child(self.compact_source_toolbar(cx)))
            .when(self.rutracker_login.is_some(), |body| {
                body.child(self.rutracker_login_form(cx))
            })
            .children(
                self.visible_source_groups()
                    .into_iter()
                    .map(|(group, rows)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .when(group == SourceGroup::Possible, |group| {
                                group.child(eyebrow("ВОЗМОЖНЫЕ СОВПАДЕНИЯ")).child(body(
                                    "Проверьте название, год и выпуск перед просмотром.",
                                ))
                            })
                            .children(rows.into_iter().map(|row| self.compact_source_row(row, cx)))
                    }),
            )
            .when(has_rows && self.visible_sources().is_empty(), |content| {
                content.child(body(
                    "Нет источников с выбранным качеством. Выберите «Все».",
                ))
            })
            .child(self.torrent_files(cx))
            .when(self.model.source.is_some(), |content| {
                content.child(body(if self.model.can_play() {
                    "Источник готов. Нажмите «Смотреть»."
                } else if !self.model.files.is_empty() {
                    "Выберите видеофайл для просмотра."
                } else {
                    "Источник проверяется. Дождитесь готовности или выберите другой."
                }))
            })
            .when(self.browser_playback_started(), |content| {
                content.child(browser_warning())
            });
        let can_watch = self.source_dialog.selected.map_or_else(
            || self.model.can_play(),
            |index| self.model.accepts(&UiEvent::WatchSource(index)),
        );
        let footer = self
            .source_footer(cx)
            .child(
                self.button(
                    "import-manual-source",
                    "Свой источник",
                    Command::SourceDialog(SourceDialogCommand::OpenImport),
                    false,
                    self.model.accepts(&UiEvent::PasteSource),
                    cx,
                )
                .mr_auto(),
            )
            .child(self.button(
                "cancel-source-dialog",
                if busy {
                    "Отменить поиск"
                } else {
                    "Отмена"
                },
                Command::CloseOverlay,
                false,
                true,
                cx,
            ))
            .child(self.button(
                "watch-selected-source",
                "Смотреть",
                Command::SourceDialog(SourceDialogCommand::Watch),
                true,
                can_watch,
                cx,
            ));
        modal::surface(width, height, 840., 640., header, content, footer)
    }

    fn source_dialog_subtitle(&self) -> Option<String> {
        let media = self.model.media.as_ref()?;
        Some(
            match self
                .model
                .selected_episode
                .filter(|_| media.kind == MediaKind::Series)
            {
                Some((season, episode)) => format!("{} · S{season:02}E{episode:02}", media.title),
                None => media.title.clone(),
            },
        )
    }

    fn browser_playback_started(&self) -> bool {
        self.model.default_player == PlayerChoice::Browser
            && self.model.playback_active
            && self.model.source.is_some()
    }

    fn source_loading(&self) -> bool {
        self.model.source_discovery_starting
            || self
                .model
                .source_discovery
                .as_ref()
                .is_some_and(|d| d.pending > 0)
            || matches!(
                self.model.states[Section::Sources.index()],
                SectionState::Loading(_)
            )
    }

    fn source_header(
        &self,
        label: &'static str,
        title: &str,
        subtitle: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(20.))
            .py(px(15.))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(eyebrow(label))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(19.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(public_label(title)),
                    )
                    .when_some(subtitle, |header, subtitle| {
                        header.child(
                            div()
                                .truncate()
                                .text_size(px(12.))
                                .text_color(rgb(MUTED))
                                .child(public_label(subtitle)),
                        )
                    }),
            )
            .child(
                self.button(
                    "close-source-dialog",
                    "Esc · Закрыть",
                    Command::CloseOverlay,
                    false,
                    true,
                    cx,
                )
                .text_size(px(11.)),
            )
    }

    fn source_footer(&self, _cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .p(px(14.))
            .border_t_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL))
    }

    fn compact_discovery_heading(&self) -> &'static str {
        let discovery = self.model.source_discovery.as_ref();
        let has_rows = !self.model.sources.is_empty();
        let state = &self.model.states[Section::Sources.index()];
        let all_failed = discovery.is_some_and(|discovery| {
            discovery.pending == 0
                && discovery.succeeded == 0
                && !discovery.failures.is_empty()
                && !has_rows
        });
        let empty_partial = discovery.is_some_and(|discovery| {
            discovery.pending == 0
                && discovery.succeeded > 0
                && !discovery.failures.is_empty()
                && !has_rows
        });
        if matches!(state, SectionState::Error(_) | SectionState::Unavailable(_)) {
            state.heading()
        } else if self.source_loading() {
            "Поиск и проверка источников…"
        } else if all_failed {
            "Все провайдеры завершились с ошибкой"
        } else if empty_partial {
            "Часть провайдеров недоступна · источники не найдены"
        } else if let Some(discovery) = discovery {
            discovery.heading(has_rows)
        } else if has_rows {
            "Выберите источник для просмотра"
        } else if self.model.source.is_some() || !self.model.files.is_empty() {
            "Ручной источник"
        } else {
            "Источники не найдены"
        }
    }

    fn compact_discovery_status(&self, cx: &mut Context<Self>) -> Div {
        let discovery = self.model.source_discovery.as_ref();
        let has_rows = !self.model.sources.is_empty();
        let heading = self.compact_discovery_heading();
        div()
            .flex()
            .flex_col()
            .gap(px(7.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(12.)).child(heading))
                    .child(self.button(
                        "retry-source-dialog",
                        "Повторить",
                        Command::Emit(UiEvent::Retry(Section::Sources)),
                        false,
                        self.model.accepts(&UiEvent::Retry(Section::Sources)),
                        cx,
                    ))
                    .child(self.button(
                        "toggle-source-diagnostics",
                        if self.source_dialog.diagnostics { "Скрыть диагностику" } else { "Диагностика" },
                        Command::SourceDialog(SourceDialogCommand::ToggleDiagnostics),
                        false,
                        true,
                        cx,
                    )),
            )
            .when(!has_rows && !self.source_loading() && self.model.source.is_none(), |status| {
                status.child(body("Повторите поиск, проверьте поставщиков в настройках или добавьте свой источник."))
            })
            .when_some(discovery, |status, discovery| {
                status
                    .child(
                        div().text_size(px(11.)).text_color(rgb(MUTED)).child(format!(
                            "Найдено: {} · Ожидается: {} · Успешно: {} · Ошибок: {}",
                            self.model.sources.len(), discovery.pending,
                            discovery.succeeded, discovery.failures.len()
                        )),
                    )
                    .when(self.source_dialog.diagnostics, |status| {
                        status
                            .children(discovery.notices.iter().map(|message| body(message.clone())))
                            .children(discovery.failures.iter().map(|message| {
                                div().text_color(rgb(WARNING)).child(message.clone())
                            }))
                    })
            })
            .when(self.source_dialog.diagnostics && discovery.is_none(), |status| {
                status.child(body("Нет отчёта поставщиков. Проверьте разрешения и настройки источников."))
            })
    }

    fn compact_source_toolbar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .children(
                SourceQuality::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(i, quality)| {
                        self.button(
                            ("picker-quality", i),
                            quality.label(),
                            Command::SourceQuality(quality),
                            quality == self.source_quality,
                            true,
                            cx,
                        )
                        .h(px(29.))
                        .px(px(10.))
                        .text_size(px(11.))
                    }),
            )
            .child(
                self.button(
                    "picker-source-order",
                    if self.source_order_seeds {
                        "По сидам ↓"
                    } else {
                        "Порядок провайдеров"
                    },
                    Command::ToggleSourceOrder,
                    false,
                    true,
                    cx,
                )
                .h(px(29.))
                .px(px(10.))
                .text_size(px(11.)),
            )
    }

    fn compact_source_row(&self, row: &SourceRow, cx: &mut Context<Self>) -> Div {
        let selected = self.source_dialog.selected == Some(row.index);
        let expanded = self.source_dialog.expanded == Some(row.index);
        div()
            .rounded(px(5.))
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { BORDER }))
            .bg(rgb(if selected { ELEVATED } else { PANEL }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .child(
                        self.action(
                            ("picker-source", row.index),
                            Command::SourceDialog(SourceDialogCommand::Select(row.index)),
                            self.model.accepts(&UiEvent::ChooseSource(row.index)),
                            cx,
                        )
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(10.))
                        .p(px(11.))
                        .child(div().text_color(rgb(ACCENT)).child(if selected { "●" } else { "○" }))
                        .child(metric(row.quality.as_deref().unwrap_or("Качество —"), TEXT))
                        .child(metric(row.video_codec.as_deref().unwrap_or("Кодек —"), MUTED))
                        .child(metric(row.language.as_deref().unwrap_or("Язык —"), MUTED))
                        .child(metric(&row.size.map(format_size).unwrap_or_else(|| "Размер —".into()), MUTED))
                        .child(metric(
                            &row.seeds.map_or_else(|| "Сиды —".into(), |n| format!("{n} сидов")),
                            if row.seeds.is_some_and(|n| n > 0) { ACCENT } else { MUTED },
                        ))
                        .child(metric(&row.origin, MUTED))
                        .when(row.recommended, |row| row.child(metric("Рекомендуемый", ACCENT))),
                    )
                    .child(
                        self.button(
                            ("picker-source-details", row.index),
                            if expanded { "Свернуть" } else { "Детали" },
                            Command::SourceDialog(SourceDialogCommand::ToggleDetails(row.index)),
                            false, true, cx,
                        ).h(px(29.)).px(px(8.)).mr(px(8.)).text_size(px(10.)),
                    ),
            )
            .when(expanded, |details| {
                details.child(
                    div()
                        .px(px(14.))
                        .pb(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(div().text_size(px(12.)).child(public_label(&row.title)))
                        .child(body(format!(
                            "{} · Аудио: {} · Пиры: {}",
                            row.kind.label(), public_label(row.audio_codec.as_deref().unwrap_or("—")),
                            row.peers.map_or_else(|| "—".into(), |n| n.to_string())
                        )))
                        .when_some(row.license.as_ref(), |details, credit| details.child(body(credit.clone())))
                        .when(row.rutracker || row.possible_match, |details| {
                            details.child(body("Метаданные не проверены. Совпадение и права на просмотр подтвердите самостоятельно."))
                        }),
                )
            })
    }

    fn import_dialog(&self, width: f32, height: f32, cx: &mut Context<Self>) -> Div {
        let content = div()
            .id("manual-import-body")
            .overflow_y_scroll()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(body("Добавьте HTTP(S) / magnet-ссылку из буфера обмена или выберите .torrent на компьютере. Ссылка не отображается в интерфейсе."))
            .when_some(self.model.media.as_ref(), |content, media| {
                content
                    .child(eyebrow("ПРИВЯЗКА"))
                    .child(body(public_label(&media.title)))
                    .child(self.button(
                        "import-attachment",
                        if self.source_dialog.attach { "● К этой карточке" } else { "○ Без привязки к карточке" },
                        Command::SourceDialog(SourceDialogCommand::ToggleAttach),
                        self.source_dialog.attach,
                        self.model.accepts(&UiEvent::PasteSource),
                        cx,
                    ))
            })
            .when(self.model.media.is_none(), |content| {
                content.child(body("Источник будет добавлен без привязки к карточке."))
            })
            .child(
                div().flex().flex_wrap().gap(px(8.))
                    .child(self.button(
                        "import-clipboard", "Вставить HTTP / magnet",
                        Command::SourceDialog(SourceDialogCommand::ImportClipboard), true,
                        self.model.accepts(&UiEvent::PasteSource), cx,
                    ))
                    .child(self.button(
                        "import-torrent", "Выбрать .torrent",
                        Command::SourceDialog(SourceDialogCommand::ImportTorrent), false,
                        self.model.accepts(&UiEvent::OpenTorrent), cx,
                    )),
            )
            .when(self.source_loading(), |content| {
                content.child(body("Проверяем источник… Можно отменить операцию."))
            })
            .when(matches!(self.model.states[Section::Sources.index()], SectionState::Error(_)), |content| {
                content.child(body("Не удалось добавить источник. Проверьте ссылку или выберите другой файл."))
            })
            .when(self.model.source.is_some(), |content| {
                // SourcePreview.title may be derived from a private locator.
                content.child(body("Источник добавлен.")).child(self.torrent_files(cx))
                    .when(self.model.accepts(&UiEvent::UnlinkMedia), |content| content.child(
                        div().flex().child(self.button(
                            "import-unlink", "Снять привязку", Command::Emit(UiEvent::UnlinkMedia),
                            false, true, cx,
                        ))
                    ))
            })
            .when(self.rutracker_login.is_some(), |content| {
                content.child(self.rutracker_login_form(cx))
            })
            .when(self.browser_playback_started(), |content| {
                content.child(browser_warning())
            });
        let footer = self
            .source_footer(cx)
            .when(!self.model.sources.is_empty(), |footer| {
                footer.child(self.button(
                    "import-back",
                    "К источникам",
                    Command::SourceDialog(SourceDialogCommand::BackToSources),
                    false,
                    true,
                    cx,
                ))
            })
            .child(div().flex_1())
            .child(self.button(
                "cancel-import",
                "Отмена",
                Command::CloseOverlay,
                false,
                true,
                cx,
            ))
            .child(self.button(
                "watch-import",
                "Смотреть",
                Command::Emit(UiEvent::Play),
                true,
                self.model.can_play(),
                cx,
            ));
        modal::surface(
            width,
            height,
            640.,
            520.,
            self.source_header("СВОЙ ИСТОЧНИК", "Добавить источник", None, cx),
            content,
            footer,
        )
    }
}

fn metric(value: &str, color: u32) -> Div {
    div()
        .text_size(px(11.))
        .text_color(rgb(color))
        .child(public_label(value))
}

fn public_label(value: &str) -> String {
    let normalized = value.to_ascii_lowercase();
    if ["http:", "https:", "magnet:", "file:", "://"]
        .iter()
        .any(|scheme| normalized.contains(scheme))
    {
        "Скрытые данные источника".into()
    } else {
        value.to_owned()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} ГБ", bytes as f64 / 1_000_000_000.)
    } else {
        format!("{:.0} МБ", bytes as f64 / 1_000_000.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize, recommended: bool, possible_match: bool) -> SourceRow {
        SourceRow {
            index,
            recommended,
            possible_match,
            title: "Release".into(),
            kind: SourceKind::Magnet,
            quality: None,
            video_codec: None,
            audio_codec: None,
            language: None,
            resolution: None,
            languages: vec![],
            origin: "Provider".into(),
            license: None,
            size: None,
            seeds: None,
            peers: None,
            rutracker: false,
        }
    }

    #[test]
    fn selection_uses_engine_indices_and_preserves_explicit_choice() {
        let rows = [row(41, false, false), row(9, true, false)];
        let mut state = SourceDialogState::default();
        state.reconcile(&rows.iter().collect::<Vec<_>>());
        assert_eq!(state.selected, Some(9));
        state.selected = Some(41);
        state.manual_selection = true;
        state.reconcile(&[&rows[1], &rows[0]]);
        assert_eq!(state.selected, Some(41));
        state.reconcile(&[&rows[1]]);
        assert_eq!(state.selected, Some(9));
        state.reconcile(&[]);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn possible_matches_require_explicit_selection() {
        let rows = [row(12, true, true)];
        let mut state = SourceDialogState::default();
        state.reconcile(&[&rows[0]]);
        assert_eq!(state.selected, None);
        state.selected = Some(12);
        state.manual_selection = true;
        state.reconcile(&[&rows[0]]);
        assert_eq!(state.selected, Some(12));
    }

    #[test]
    fn recommendation_arriving_later_updates_only_automatic_preselection() {
        let rows = [row(41, false, false), row(9, true, false)];
        let mut state = SourceDialogState::default();
        state.reconcile(&[&rows[0]]);
        assert_eq!(state.selected, Some(41));
        state.reconcile(&[&rows[0], &rows[1]]);
        assert_eq!(state.selected, Some(9));
        state.reset();
        state.reconcile(&[&rows[0]]);
        state.manual_selection = true;
        state.reconcile(&[&rows[0], &rows[1]]);
        assert_eq!(state.selected, Some(41));
    }

    #[test]
    fn picker_subtitle_uses_actual_media_and_series_episode() {
        let mut view = KadrView::default();
        assert_eq!(view.source_dialog_subtitle(), None);
        view.model.media = Some(MediaCard {
            title: "Title".into(),
            kind: MediaKind::Series,
            ..MediaCard::default()
        });
        assert_eq!(view.source_dialog_subtitle().as_deref(), Some("Title"));
        view.model.selected_episode = Some((1, 2));
        assert_eq!(
            view.source_dialog_subtitle().as_deref(),
            Some("Title · S01E02")
        );
        view.model.media.as_mut().unwrap().kind = MediaKind::Movie;
        assert_eq!(view.source_dialog_subtitle().as_deref(), Some("Title"));
    }

    #[test]
    fn browser_warning_requires_an_actual_browser_source_launch() {
        let mut view = KadrView::default();
        view.model.default_player = PlayerChoice::Browser;
        assert!(!view.browser_playback_started());
        view.model.source = Some(SourcePreview {
            kind: SourceKind::Http,
            title: "Source".into(),
        });
        view.model.playback_available = true;
        assert!(!view.browser_playback_started());
        view.model.playback_active = true;
        assert!(view.browser_playback_started());
        view.model.default_player = PlayerChoice::Mpv;
        assert!(!view.browser_playback_started());
        view.model.default_player = PlayerChoice::Browser;
        view.model.source = None;
        assert!(!view.browser_playback_started());
    }

    #[test]
    fn settled_discovery_does_not_hide_later_source_errors_or_loading() {
        let mut view = KadrView::default();
        for (succeeded, failures, settled_heading) in [
            (1, vec![], "Источники не найдены"),
            (
                1,
                vec!["Provider failed".into()],
                "Часть провайдеров недоступна · источники не найдены",
            ),
            (
                0,
                vec!["Provider failed".into()],
                "Все провайдеры завершились с ошибкой",
            ),
        ] {
            view.model.source_discovery = Some(SourceDiscovery {
                succeeded,
                failures,
                ..SourceDiscovery::default()
            });
            for state in [
                SectionState::Error("Selected source failed".into()),
                SectionState::Unavailable("Sources disabled".into()),
            ] {
                let expected = state.heading();
                view.model.states[Section::Sources.index()] = state;
                assert_eq!(view.compact_discovery_heading(), expected);
                assert!(!view.source_loading());
            }
            view.model.states[Section::Sources.index()] =
                SectionState::Loading("Checking source".into());
            assert_eq!(
                view.compact_discovery_heading(),
                "Поиск и проверка источников…"
            );
            assert!(view.source_loading());
            view.model.states[Section::Sources.index()] = SectionState::Empty("Complete".into());
            assert_eq!(view.compact_discovery_heading(), settled_heading);
            assert!(!view.source_loading());
        }
    }

    #[test]
    fn source_loading_tracks_start_pending_and_manual_preparation_not_settled_discovery() {
        let mut view = KadrView::default();
        assert!(!view.source_loading());
        view.model.source_discovery_starting = true;
        assert!(view.source_loading());
        view.model.source_discovery_starting = false;
        view.model.source_discovery = Some(SourceDiscovery {
            pending: 1,
            ..SourceDiscovery::default()
        });
        assert!(view.source_loading());
        view.model.source_discovery.as_mut().unwrap().pending = 0;
        assert!(!view.source_loading());
        view.model.states[Section::Sources.index()] =
            SectionState::Loading("Checking source".into());
        assert!(view.source_loading());
        view.model.source_discovery = None;
        assert!(view.source_loading());
        view.model.states[Section::Sources.index()] = SectionState::Error("Source failed".into());
        assert!(!view.source_loading());
    }

    #[test]
    fn labels_never_render_private_locators() {
        for value in [
            "https://private.invalid/movie?token=secret",
            "Release MAGNET:?xt=urn:btih:private",
            "http://user:password@private.invalid/",
            "file:///private/video.mp4",
        ] {
            assert_eq!(public_label(value), "Скрытые данные источника");
        }
        assert_eq!(public_label("1080p · H.265 · ru"), "1080p · H.265 · ru");
    }
}
