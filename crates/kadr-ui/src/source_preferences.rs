//! Editable provider configuration. No environment reads, services or engine types.
use super::*;
use crate::input::TextInput;
use gpui::Entity;

struct EndpointInputs {
    name: Entity<TextInput>,
    url: Entity<TextInput>,
    env: Entity<TextInput>,
}

struct Endpoint {
    id: usize,
    draft: TorznabDraft,
    inputs: Option<EndpointInputs>,
}

pub(super) struct SourcePreferencesEditor {
    commons_enabled: bool,
    rutracker_enabled: bool,
    popcorn_enabled: bool,
    endpoints: Vec<Endpoint>,
    next_id: usize,
    resolution: String,
    language: String,
    inputs: Option<(Entity<TextInput>, Entity<TextInput>)>,
}

impl Default for SourcePreferencesEditor {
    fn default() -> Self {
        Self::from(SourcePreferencesDraft::default())
    }
}

impl From<SourcePreferencesDraft> for SourcePreferencesEditor {
    fn from(draft: SourcePreferencesDraft) -> Self {
        Self {
            commons_enabled: draft.commons_enabled,
            rutracker_enabled: draft.rutracker_enabled,
            popcorn_enabled: draft.popcorn_enabled,
            next_id: draft.torznab.len(),
            endpoints: draft
                .torznab
                .into_iter()
                .enumerate()
                .map(|(id, draft)| Endpoint {
                    id,
                    draft,
                    inputs: None,
                })
                .collect(),
            resolution: draft.preferred_resolution.unwrap_or_default(),
            language: draft.preferred_language.unwrap_or_default(),
            inputs: None,
        }
    }
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn api_key_env(value: &str) -> Result<Option<String>, &'static str> {
    let Some(value) = optional(value) else {
        return Ok(None);
    };
    if value.strip_prefix("KADR_TORZNAB_").is_some_and(|suffix| {
        !suffix.is_empty()
            && value.len() <= 128
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    }) {
        Ok(Some(value))
    } else {
        Err(
            "Укажите только имя переменной KADR_TORZNAB_… (латинские буквы, цифры, _), не значение ключа.",
        )
    }
}

fn endpoint_draft(
    name: &str,
    url: &str,
    env: &str,
    enabled: bool,
) -> Result<TorznabDraft, &'static str> {
    if name.trim().is_empty() || url.trim().is_empty() {
        return Err("Для каждого Torznab укажите имя и URL или удалите пустую строку.");
    }
    Ok(TorznabDraft {
        name: name.trim().to_owned(),
        base_url: url.trim().to_owned(),
        api_key_env: api_key_env(env)?,
        enabled,
    })
}

impl SourcePreferencesEditor {
    pub(super) fn new(draft: SourcePreferencesDraft, cx: &mut Context<KadrView>) -> Self {
        let mut editor = Self::from(draft);
        editor.ensure_inputs(cx);
        editor
    }

    pub(super) fn ensure_inputs(&mut self, cx: &mut Context<KadrView>) {
        if self.inputs.is_none() {
            self.inputs = Some((
                cx.new(|cx| TextInput::new(self.resolution.clone(), "Например, 1080p", cx)),
                cx.new(|cx| TextInput::new(self.language.clone(), "Например, ru", cx)),
            ));
        }
        for endpoint in &mut self.endpoints {
            if endpoint.inputs.is_none() {
                endpoint.inputs = Some(EndpointInputs {
                    name: cx.new(|cx| {
                        TextInput::new(endpoint.draft.name.clone(), "Имя провайдера", cx)
                    }),
                    url: cx.new(|cx| {
                        TextInput::new(
                            endpoint.draft.base_url.clone(),
                            "https://indexer.example/api",
                            cx,
                        )
                    }),
                    env: cx.new(|cx| {
                        TextInput::new(
                            endpoint.draft.api_key_env.clone().unwrap_or_default(),
                            "KADR_TORZNAB_INDEXER",
                            cx,
                        )
                    }),
                });
            }
        }
    }

    fn append(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.endpoints.push(Endpoint {
            id,
            draft: TorznabDraft {
                enabled: true,
                ..TorznabDraft::default()
            },
            inputs: None,
        });
    }

    pub(super) fn add(&mut self, cx: &mut Context<KadrView>) {
        self.append();
        self.ensure_inputs(cx);
    }

    pub(super) fn remove(&mut self, id: usize) {
        self.endpoints.retain(|endpoint| endpoint.id != id);
    }

    pub(super) fn toggle(&mut self, id: usize) {
        if let Some(endpoint) = self.endpoints.iter_mut().find(|endpoint| endpoint.id == id) {
            endpoint.draft.enabled = !endpoint.draft.enabled;
        }
    }

    pub(super) fn toggle_commons(&mut self) {
        self.commons_enabled = !self.commons_enabled;
    }

    pub(super) fn toggle_rutracker(&mut self) {
        self.rutracker_enabled = !self.rutracker_enabled;
    }

    pub(super) fn toggle_popcorn(&mut self) {
        self.popcorn_enabled = !self.popcorn_enabled;
    }

    pub(super) fn draft(&self, cx: &gpui::App) -> Result<SourcePreferencesDraft, &'static str> {
        self.snapshot(|input| input.read(cx).value().to_owned())
    }

    fn snapshot(
        &self,
        read: impl Fn(&Entity<TextInput>) -> String,
    ) -> Result<SourcePreferencesDraft, &'static str> {
        let mut torznab = Vec::new();
        for endpoint in &self.endpoints {
            let draft = if let Some(inputs) = &endpoint.inputs {
                endpoint_draft(
                    &read(&inputs.name),
                    &read(&inputs.url),
                    &read(&inputs.env),
                    endpoint.draft.enabled,
                )?
            } else {
                endpoint_draft(
                    &endpoint.draft.name,
                    &endpoint.draft.base_url,
                    endpoint.draft.api_key_env.as_deref().unwrap_or_default(),
                    endpoint.draft.enabled,
                )?
            };
            // URL validation belongs to the controller; never echo its contents in errors.
            torznab.push(draft);
        }
        let (resolution, language) = self.inputs.as_ref().map_or_else(
            || (optional(&self.resolution), optional(&self.language)),
            |(resolution, language)| (optional(&read(resolution)), optional(&read(language))),
        );
        Ok(SourcePreferencesDraft {
            commons_enabled: self.commons_enabled,
            rutracker_enabled: self.rutracker_enabled,
            popcorn_enabled: self.popcorn_enabled,
            torznab,
            preferred_resolution: resolution,
            preferred_language: language,
        })
    }
}

impl KadrView {
    pub(super) fn source_preference_fields(&self, cx: &mut Context<Self>) -> Div {
        let editor = &self.source_preferences;
        let (resolution, language) = editor.inputs.as_ref().expect("inputs initialized");
        panel()
            .gap_4()
            .child(eyebrow("ПОИСК ИСТОЧНИКОВ"))
            .child(body(COMMONS_DISCLOSURE))
            .child(body(MOVIE_PREFETCH_DISCLOSURE))
            .child(self.button(
                "enable-commons",
                if editor.commons_enabled { "Wikimedia Commons: включён" } else { "Wikimedia Commons: выключен" },
                Command::ToggleCommons, editor.commons_enabled, true, cx,
            ))
            .child(body("Добавьте URL API своего поставщика Torznab. Разрешён HTTPS, а для локального сервера — также HTTP. Поиск обращается только к включённым поставщикам после сохранения настроек."))
            .child(self.button(
                "enable-rutracker",
                if editor.rutracker_enabled { "RuTracker: включён" } else { "RuTracker: выключен" },
                Command::ToggleRuTracker, editor.rutracker_enabled, true, cx,
            ))
            .child(body(POPCORN_DISCLOSURE))
            .child(self.button(
                "enable-popcorn",
                if editor.popcorn_enabled { "Popcorn API / YTS: включён" } else { "Popcorn API / YTS: выключен" },
                Command::TogglePopcorn, editor.popcorn_enabled, true, cx,
            ))
            .children(editor.endpoints.iter().map(|endpoint| {
                let inputs = endpoint.inputs.as_ref().expect("inputs initialized");
                panel()
                    .id(("torznab-endpoint", endpoint.id))
                    .child(field("Имя Torznab", inputs.name.clone()).id(("torznab-name", endpoint.id)))
                    .child(field("Базовый URL Torznab", inputs.url.clone()).id(("torznab-url", endpoint.id)))
                    .child(field("Переменная окружения с API-ключом (необязательно)", inputs.env.clone()).id(("torznab-env", endpoint.id)))
                    .child(div().flex().flex_wrap().gap_2()
                        .child(self.button(
                            ("enable-torznab", endpoint.id),
                            if endpoint.draft.enabled { "Включён" } else { "Выключен" },
                            Command::ToggleTorznab(endpoint.id), endpoint.draft.enabled, true, cx,
                        ))
                        .child(self.button(
                            ("remove-torznab", endpoint.id), "Удалить",
                            Command::RemoveTorznab(endpoint.id), false, true, cx,
                        )))
            }))
            .child(self.button("add-torznab", "Добавить Torznab", Command::AddTorznab, false, true, cx))
            .child(body("Здесь вводится только имя переменной KADR_TORZNAB_…, никогда не сам API-ключ. Задайте значение вне приложения в окружении процесса. Пустое поле — без ключа."))
            .child(field("Предпочтительное разрешение (пусто — любое)", resolution.clone()).id("preferred-resolution"))
            .child(field("Предпочтительный язык (пусто — любой)", language.clone()).id("preferred-language"))
            .child(self.button("save-source-preferences", "Сохранить настройки источников", Command::SaveSourcePreferences, true, true, cx))
            .when(!self.source_preferences_status.is_empty(), |panel| {
                panel.child(body(self.source_preferences_status.clone()))
            })
    }
}

fn field(label: &'static str, input: Entity<TextInput>) -> Div {
    column()
        .gap_2()
        .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child(label))
        .child(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_input_accepts_only_an_optional_prefixed_identifier() {
        for value in ["", "  "] {
            assert_eq!(api_key_env(value), Ok(None));
        }
        assert_eq!(
            api_key_env(" KADR_TORZNAB_ONE_2 "),
            Ok(Some("KADR_TORZNAB_ONE_2".into()))
        );
        for value in [
            "KADR_TORZNAB_",
            "OTHER_KEY",
            "KADR_TORZNAB_X=secret",
            "$KADR_TORZNAB_X",
            "KADR_TORZNAB_Я",
            "KADR_TORZNAB_A-B",
        ] {
            assert!(api_key_env(value).is_err());
            assert_eq!(
                api_key_env(value).unwrap_err(),
                api_key_env("secret-value").unwrap_err()
            );
        }
    }

    #[test]
    fn endpoint_editor_normalizes_fields_without_reading_or_substituting_secrets() {
        assert_eq!(
            endpoint_draft(
                " One ",
                " https://one.example/api ",
                " KADR_TORZNAB_ONE ",
                false
            )
            .unwrap(),
            TorznabDraft {
                name: "One".into(),
                base_url: "https://one.example/api".into(),
                api_key_env: Some("KADR_TORZNAB_ONE".into()),
                enabled: false,
            }
        );
        assert_eq!(
            endpoint_draft("Two", "https://two.example", "", true)
                .unwrap()
                .api_key_env,
            None
        );
        assert!(endpoint_draft("", "https://one.example", "", true).is_err());
        assert!(endpoint_draft("One", "", "", false).is_err());
        assert!(endpoint_draft("One", "https://one.example", "secret-value", true).is_err());
    }

    #[test]
    fn commons_editor_roundtrips_actual_draft_and_toggles_independently() {
        // Test editing the loaded draft without launching a native platform
        // event loop in a Cargo test thread (AppKit also needs the main thread).
        let snapshot = |editor: &SourcePreferencesEditor| {
            editor.snapshot(|_| unreachable!("no live inputs")).unwrap()
        };
        for commons_enabled in [true, false] {
            let loaded = SourcePreferencesDraft {
                commons_enabled,
                rutracker_enabled: !commons_enabled,
                popcorn_enabled: false,
                torznab: vec![TorznabDraft {
                    name: "One".into(),
                    base_url: "https://one.example/api".into(),
                    api_key_env: Some("KADR_TORZNAB_ONE".into()),
                    enabled: true,
                }],
                preferred_resolution: Some("1080p".into()),
                preferred_language: Some("ru".into()),
            };
            let mut editor = SourcePreferencesEditor::from(loaded.clone());
            assert_eq!(snapshot(&editor), loaded);
            editor.toggle_commons();
            let toggled = SourcePreferencesDraft {
                commons_enabled: !commons_enabled,
                ..loaded.clone()
            };
            assert_eq!(snapshot(&editor), toggled);
            editor.toggle_rutracker();
            editor.toggle(0);
            let draft = snapshot(&editor);
            assert_eq!(draft.commons_enabled, !commons_enabled);
            assert_eq!(draft.rutracker_enabled, commons_enabled);
            assert!(!draft.torznab[0].enabled);
            // Loading persisted settings replaces unsaved toggle state.
            editor = SourcePreferencesEditor::from(loaded.clone());
            assert_eq!(snapshot(&editor), loaded);
        }
    }

    #[test]
    fn popcorn_editor_roundtrips_both_states_and_only_toggles_its_own_flag() {
        for popcorn_enabled in [false, true] {
            let loaded = SourcePreferencesDraft {
                popcorn_enabled,
                torznab: vec![TorznabDraft {
                    name: "One".into(),
                    base_url: "https://one.example/api".into(),
                    enabled: true,
                    ..TorznabDraft::default()
                }],
                preferred_resolution: Some("1080p".into()),
                preferred_language: Some("ru".into()),
                ..SourcePreferencesDraft::default()
            };
            let snapshot = |editor: &SourcePreferencesEditor| {
                editor.snapshot(|_| unreachable!("no live inputs")).unwrap()
            };
            let mut editor = SourcePreferencesEditor::from(loaded.clone());
            assert_eq!(snapshot(&editor), loaded);
            editor.toggle_popcorn();
            assert_eq!(
                snapshot(&editor),
                SourcePreferencesDraft {
                    popcorn_enabled: !popcorn_enabled,
                    ..loaded.clone()
                }
            );
            editor.toggle_commons();
            editor.toggle_rutracker();
            editor.toggle(0);
            assert_eq!(snapshot(&editor).popcorn_enabled, !popcorn_enabled);
            // A persisted-settings acknowledgement replaces unsaved state.
            editor = SourcePreferencesEditor::from(loaded.clone());
            assert_eq!(snapshot(&editor), loaded);
        }
    }

    #[test]
    fn loaded_editor_preserves_independent_provider_flags_and_preferences() {
        let editor = SourcePreferencesEditor::from(SourcePreferencesDraft {
            commons_enabled: true,
            rutracker_enabled: false,
            popcorn_enabled: true,
            torznab: vec![
                TorznabDraft {
                    name: "One".into(),
                    enabled: true,
                    ..TorznabDraft::default()
                },
                TorznabDraft {
                    name: "Two".into(),
                    enabled: false,
                    ..TorznabDraft::default()
                },
            ],
            preferred_resolution: Some("1080p".into()),
            preferred_language: Some("ru".into()),
        });
        assert!(editor.commons_enabled);
        assert!(!editor.rutracker_enabled);
        assert!(editor.popcorn_enabled);
        assert!(editor.endpoints[0].draft.enabled);
        assert!(!editor.endpoints[1].draft.enabled);
        assert_eq!(editor.resolution, "1080p");
        assert_eq!(editor.language, "ru");
    }

    #[test]
    fn removing_and_adding_endpoints_preserves_other_control_ids() {
        let mut editor = SourcePreferencesEditor::default();
        editor.append();
        editor.append();
        editor.append();
        editor.remove(1);
        editor.append();
        assert_eq!(
            editor
                .endpoints
                .iter()
                .map(|endpoint| endpoint.id)
                .collect::<Vec<_>>(),
            [0, 2, 3]
        );
        editor.toggle(2);
        assert!(!editor.endpoints[1].draft.enabled);
        assert!(editor.endpoints[0].draft.enabled);
        assert!(editor.endpoints[2].draft.enabled);
        let previous = editor.rutracker_enabled;
        editor.toggle_rutracker();
        assert_eq!(editor.rutracker_enabled, !previous);
        assert!(editor.endpoints[0].draft.enabled);
        editor.toggle(1); // A stale control cannot toggle a different endpoint.
        assert!(!editor.endpoints[1].draft.enabled);
    }

    #[test]
    fn optional_preferences_do_not_invent_values() {
        assert_eq!(optional("  "), None);
        assert_eq!(optional(" 2160p "), Some("2160p".into()));
        assert_eq!(optional(" ru "), Some("ru".into()));
    }
}
