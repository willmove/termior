use gpui::{
    canvas, div, prelude::*, AnyElement, App, Bounds, Context, FocusHandle, Focusable,
    InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    StatefulInteractiveElement, UTF16Selection, WeakEntity, Window,
};
use std::ops::Range;
use std::path::PathBuf;
use termior_ai::{HttpProvider, KeyringSecretStore, ProviderConfig, SecretStore};
use termior_store::settings::Appearance;
use termior_store::{atomic_write, default_keymap, Platform, Settings};
use termior_ui::SettingsPage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    FontFamily,
    FontSize,
    LetterSpacing,
    Scrollback,
    Instructions,
    Model,
    BaseUrl,
    ApiKey,
}

type SaveCallback = Box<dyn Fn(&Settings, &mut App)>;

pub struct SettingsView {
    page: SettingsPage,
    settings: Settings,
    migration_error: Option<String>,
    data_dir: Option<PathBuf>,
    profile_index: usize,
    focus_handle: FocusHandle,
    edit_field: Option<EditField>,
    draft: String,
    cursor: usize,
    marked_text: String,
    credential_present: bool,
    status: String,
    on_save: Option<SaveCallback>,
}

impl SettingsView {
    pub fn new(
        settings: Settings,
        migration_error: Option<String>,
        data_dir: Option<PathBuf>,
        on_save: Option<SaveCallback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            page: SettingsPage::General,
            settings,
            migration_error,
            data_dir,
            profile_index: 0,
            focus_handle: cx.focus_handle(),
            edit_field: None,
            draft: String::new(),
            cursor: 0,
            marked_text: String::new(),
            credential_present: false,
            status: String::new(),
            on_save,
        };
        view.refresh_credential_state();
        view
    }

    fn set_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.commit_edit();
        self.page = page;
        cx.notify();
    }

    fn profile(&self) -> Option<&termior_store::ModelProviderSettings> {
        self.settings.models.profiles.get(self.profile_index)
    }

    fn profile_mut(&mut self) -> Option<&mut termior_store::ModelProviderSettings> {
        self.settings.models.profiles.get_mut(self.profile_index)
    }

    fn profile_key(&self) -> Option<String> {
        self.profile()
            .map(|profile| format!("provider:{}", profile.id))
    }

    fn refresh_credential_state(&mut self) {
        self.credential_present = self
            .profile_key()
            .and_then(|key| KeyringSecretStore::new().get(&key).ok().flatten())
            .is_some();
    }

    fn begin_edit(&mut self, field: EditField, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit();
        self.edit_field = Some(field);
        self.draft = self.value_for(field);
        if field == EditField::ApiKey {
            self.draft.clear();
        }
        self.cursor = self.draft.chars().count();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn value_for(&self, field: EditField) -> String {
        match field {
            EditField::FontFamily => self.settings.terminal.font_family.clone(),
            EditField::FontSize => self.settings.terminal.font_size.to_string(),
            EditField::LetterSpacing => self.settings.terminal.letter_spacing.to_string(),
            EditField::Scrollback => self.settings.terminal.scrollback_lines.to_string(),
            EditField::Instructions => self.settings.custom_instructions.clone(),
            EditField::Model => self.profile().map(|p| p.model.clone()).unwrap_or_default(),
            EditField::BaseUrl => self
                .profile()
                .map(|p| p.base_url.clone())
                .unwrap_or_default(),
            EditField::ApiKey => {
                if self.credential_present {
                    "•••••••• (stored in OS keychain)".into()
                } else {
                    "Click to add a key".into()
                }
            }
        }
    }

    fn commit_edit(&mut self) {
        let Some(field) = self.edit_field.take() else {
            return;
        };
        let value = self.draft.trim().to_owned();
        let result: Result<(), String> = match field {
            EditField::FontFamily => {
                if !value.is_empty() {
                    self.settings.terminal.font_family = value;
                }
                Ok(())
            }
            EditField::FontSize => value
                .parse::<u8>()
                .map(|value| self.settings.terminal.font_size = value)
                .map_err(|error| error.to_string()),
            EditField::LetterSpacing => value
                .parse::<f32>()
                .map(|value| self.settings.terminal.letter_spacing = value)
                .map_err(|error| error.to_string()),
            EditField::Scrollback => value
                .parse::<u32>()
                .map(|value| self.settings.terminal.scrollback_lines = value)
                .map_err(|error| error.to_string()),
            EditField::Instructions => {
                self.settings.custom_instructions = self.draft.clone();
                Ok(())
            }
            EditField::Model => {
                if let Some(profile) = self.profile_mut() {
                    profile.model = value;
                }
                Ok(())
            }
            EditField::BaseUrl => {
                if let Some(profile) = self.profile_mut() {
                    profile.base_url = value;
                }
                Ok(())
            }
            EditField::ApiKey => {
                if !value.is_empty() {
                    let key = self.profile_key().unwrap_or_default();
                    let stored = KeyringSecretStore::new()
                        .set(&key, &value)
                        .map_err(|error| error.to_string());
                    if stored.is_ok() {
                        self.credential_present = true;
                    }
                    stored
                } else {
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            self.status = format!("Invalid value: {error}");
        }
        self.draft.clear();
        self.cursor = 0;
        self.marked_text.clear();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        if let Err(error) = self.settings.validate() {
            self.status = error.to_string();
            cx.notify();
            return;
        }
        let result = self.data_dir.as_ref().map_or_else(
            || Err("application data directory is unavailable".to_owned()),
            |dir| {
                std::fs::create_dir_all(dir)
                    .map_err(|error| error.to_string())
                    .and_then(|()| {
                        serde_json::to_string_pretty(&self.settings)
                            .map_err(|error| error.to_string())
                    })
                    .and_then(|json| {
                        atomic_write(&dir.join("Termior-settings.json"), &json)
                            .map_err(|error| error.to_string())
                    })
            },
        );
        match result {
            Ok(()) => {
                self.status = "Settings saved".into();
                if let Some(on_save) = &self.on_save {
                    on_save(&self.settings, cx);
                }
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
        cx.notify();
    }

    fn cycle_profile(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.commit_edit();
        let len = self.settings.models.profiles.len();
        if len > 0 {
            self.profile_index =
                (self.profile_index as isize + delta).rem_euclid(len as isize) as usize;
        }
        self.refresh_credential_state();
        self.status.clear();
        cx.notify();
    }

    fn toggle_provider(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.enabled = !profile.enabled;
        }
        cx.notify();
    }

    fn make_active_chat(&mut self, cx: &mut Context<Self>) {
        self.settings.models.active_chat_profile = self.profile().map(|p| p.id.clone());
        cx.notify();
    }

    fn make_active_completion(&mut self, cx: &mut Context<Self>) {
        self.settings.models.active_completion_profile = self.profile().map(|p| p.id.clone());
        cx.notify();
    }

    fn ping_provider(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        let Some(profile) = self.profile().cloned() else {
            return;
        };
        let api_key = self
            .profile_key()
            .and_then(|key| KeyringSecretStore::new().get(&key).ok().flatten());
        self.status = "Checking provider…".into();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    ProviderConfig::from_settings(&profile, api_key)
                        .and_then(HttpProvider::new)
                        .and_then(|provider| provider.ping())
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.status = match result {
                    Ok(duration) => format!("Reachable in {} ms", duration.as_millis()),
                    Err(error) => format!("Provider check failed: {error}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cycle_app_theme(&mut self, cx: &mut Context<Self>) {
        let themes = termior_theme::builtin_themes();
        let index = themes
            .iter()
            .position(|theme| theme.id == self.settings.theme_id)
            .unwrap_or(0);
        self.settings.theme_id = themes[(index + 1) % themes.len()].id.clone();
        cx.notify();
    }

    fn cycle_editor_theme(&mut self, cx: &mut Context<Self>) {
        let themes = termior_editor::builtin_editor_themes();
        let index = themes
            .iter()
            .position(|theme| theme.id == self.settings.editor_theme_id)
            .unwrap_or(0);
        self.settings.editor_theme_id = themes[(index + 1) % themes.len()].id.clone();
        cx.notify();
    }

    fn cycle_appearance(&mut self, cx: &mut Context<Self>) {
        self.settings.appearance = match self.settings.appearance {
            Appearance::Light => Appearance::Dark,
            Appearance::Dark => Appearance::FollowSystem,
            Appearance::FollowSystem => Appearance::Light,
        };
        cx.notify();
    }

    fn install_hooks(&mut self, cx: &mut Context<Self>) {
        self.status = match termior_hooks::install() {
            Ok(result) => format!("Claude Code hooks installed ({} added)", result.added),
            Err(error) => format!("Hook install failed: {error}"),
        };
        cx.notify();
    }

    fn uninstall_hooks(&mut self, cx: &mut Context<Self>) {
        self.status = match termior_hooks::uninstall() {
            Ok(result) => format!("Claude Code hooks removed ({})", result.removed),
            Err(error) => format!("Hook uninstall failed: {error}"),
        };
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edit_field.is_none() {
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" | "return" => self.commit_edit(),
            "escape" => {
                self.edit_field = None;
                self.draft.clear();
            }
            "backspace" if self.cursor > 0 => {
                let start = char_to_byte(&self.draft, self.cursor - 1);
                let end = char_to_byte(&self.draft, self.cursor);
                self.draft.replace_range(start..end, "");
                self.cursor -= 1;
            }
            "delete" if self.cursor < self.draft.chars().count() => {
                let start = char_to_byte(&self.draft, self.cursor);
                let end = char_to_byte(&self.draft, self.cursor + 1);
                self.draft.replace_range(start..end, "");
            }
            "left" => self.cursor = self.cursor.saturating_sub(1),
            "right" => self.cursor = (self.cursor + 1).min(self.draft.chars().count()),
            "home" => self.cursor = 0,
            "end" => self.cursor = self.draft.chars().count(),
            _ => return,
        }
        cx.notify();
    }

    fn display_edit(&self, field: EditField) -> String {
        if self.edit_field == Some(field) {
            let mut value = if field == EditField::ApiKey {
                "•".repeat(self.draft.chars().count())
            } else {
                self.draft.clone()
            };
            let byte = char_to_byte(&value, self.cursor);
            value.insert_str(
                byte,
                if self.marked_text.is_empty() {
                    "▏"
                } else {
                    "▏…"
                },
            );
            value
        } else {
            self.value_for(field)
        }
    }

    fn edit_row(
        &self,
        label: &'static str,
        field: EditField,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(gpui::rgba(0x9aa6b7ff))
                    .child(label),
            )
            .child(
                div()
                    .id(SharedString::from(format!("edit-{field:?}")))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(if self.edit_field == Some(field) {
                        gpui::rgba(0x4f8fefff)
                    } else {
                        gpui::rgba(0x3a4658ff)
                    })
                    .cursor_text()
                    .child(SharedString::from(self.display_edit(field)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                            this.begin_edit(field, window, cx)
                        }),
                    ),
            )
            .into_any_element()
    }

    fn button(
        label: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgba(0x293241ff))
            .cursor_pointer()
            .child(label.into())
    }

    fn general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let autocomplete = self.settings.autocomplete_enabled;
        let vim = self.settings.vim_mode;
        let dotfiles = self.settings.show_dotfiles;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children([
                self.edit_row("Terminal font family", EditField::FontFamily, cx),
                self.edit_row("Font size (8–32)", EditField::FontSize, cx),
                self.edit_row("Letter spacing (-2–8)", EditField::LetterSpacing, cx),
                self.edit_row("Scrollback rows (200–50,000)", EditField::Scrollback, cx),
                self.edit_row("Global custom instructions", EditField::Instructions, cx),
            ])
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Self::button(
                            format!("Autocomplete: {}", on_off(autocomplete)),
                            "autocomplete",
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.settings.autocomplete_enabled =
                                    !this.settings.autocomplete_enabled;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::button(format!("Vim: {}", on_off(vim)), "vim").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.settings.vim_mode = !this.settings.vim_mode;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::button(format!("Dotfiles: {}", on_off(dotfiles)), "dotfiles")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.show_dotfiles = !this.settings.show_dotfiles;
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn models_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(profile) = self.profile() else {
            return div().child("No provider profiles").into_any_element();
        };
        let active_chat = self.settings.models.active_chat_profile.as_deref() == Some(&profile.id);
        let active_completion =
            self.settings.models.active_completion_profile.as_deref() == Some(&profile.id);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Self::button("‹", "previous-provider").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.cycle_profile(-1, cx)),
                    ))
                    .child(div().flex_1().text_lg().child(SharedString::from(format!(
                        "{}  ({}/{})",
                        profile.display_name,
                        self.profile_index + 1,
                        self.settings.models.profiles.len()
                    ))))
                    .child(Self::button("›", "next-provider").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.cycle_profile(1, cx)),
                    )),
            )
            .children([
                self.edit_row("Model", EditField::Model, cx),
                self.edit_row("Base URL", EditField::BaseUrl, cx),
                self.edit_row("API key (never written to settings)", EditField::ApiKey, cx),
            ])
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Self::button(
                            format!("Enabled: {}", on_off(profile.enabled)),
                            "provider-enabled",
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.toggle_provider(cx)),
                        ),
                    )
                    .child(
                        Self::button(
                            if active_chat {
                                "✓ Default chat"
                            } else {
                                "Use for chat"
                            },
                            "active-chat",
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.make_active_chat(cx)),
                        ),
                    )
                    .child(
                        Self::button(
                            if active_completion {
                                "✓ Default completion"
                            } else {
                                "Use for completion"
                            },
                            "active-completion",
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.make_active_completion(cx)),
                        ),
                    )
                    .child(
                        Self::button("Test connection", "ping-provider").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.ping_provider(cx)),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn themes_page(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Self::button(
                format!("Application theme: {}", self.settings.theme_id),
                "app-theme",
            ).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.cycle_app_theme(cx))))
            .child(Self::button(
                format!("Editor theme: {}", self.settings.editor_theme_id),
                "editor-theme",
            ).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.cycle_editor_theme(cx))))
            .child(Self::button(
                format!("Appearance: {:?}", self.settings.appearance),
                "appearance",
            ).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.cycle_appearance(cx))))
            .child("Custom themes can be imported/exported as JSON; background image opacity and blur are persisted in the theme settings.")
            .into_any_element()
    }

    fn shortcuts_page(&self) -> AnyElement {
        let rows = default_keymap().into_iter().map(|entry| {
            let current = self.settings.keymap.bindings.get(&entry.action);
            let label = current
                .map(|binding| {
                    let mut parts = Vec::new();
                    if binding.primary {
                        parts.push(if cfg!(target_os = "macos") {
                            "Cmd"
                        } else {
                            "Ctrl"
                        });
                    }
                    if binding.shift {
                        parts.push("Shift");
                    }
                    if binding.alt {
                        parts.push("Alt");
                    }
                    parts.push(&binding.key);
                    parts.join("+")
                })
                .unwrap_or_else(|| entry.binding.display(Platform::current()));
            div()
                .flex()
                .justify_between()
                .py_1()
                .child(SharedString::from(format!("{:?}", entry.action)))
                .child(SharedString::from(label))
        });
        div()
            .flex()
            .flex_col()
            .children(rows)
            .child(
                div()
                    .pt_3()
                    .text_xs()
                    .child("Rebinding uses the persisted keymap and rejects conflicting chords."),
            )
            .into_any_element()
    }

    fn agents_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let hook_status = termior_hooks::status()
            .map(|status| {
                if status.fully_installed {
                    "installed"
                } else {
                    "not installed"
                }
            })
            .unwrap_or("unavailable");
        div().flex().flex_col().gap_3()
            .child(SharedString::from(format!("Claude Code hooks: {hook_status}")))
            .child(div().flex().gap_2()
                .child(Self::button("Install hooks", "install-hooks").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.install_hooks(cx))))
                .child(Self::button("Uninstall hooks", "uninstall-hooks").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.uninstall_hooks(cx)))))
            .child(Self::button(format!("Agent notifications: {}", on_off(self.settings.agent_notifications)), "agent-notifications").on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| { this.settings.agent_notifications = !this.settings.agent_notifications; cx.notify(); })))
            .child("Custom agents persist an independent system prompt, icon color, and explicit tool subset in Termior-ai-agents.json.")
            .into_any_element()
    }

    fn page_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Models => self.models_page(cx),
            SettingsPage::Themes => self.themes_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(),
            SettingsPage::Agents => self.agents_page(cx),
            SettingsPage::About => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(format!("Termior {}", env!("CARGO_PKG_VERSION")))
                .child("Apache-2.0 · No account · No telemetry · Offline with local providers")
                .child(SharedString::from(format!(
                    "Migration status: {}",
                    self.migration_error.as_deref().unwrap_or("OK")
                )))
                .into_any_element(),
        }
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = SettingsInputHandler {
            view: cx.entity().downgrade(),
        };
        let tabs = [
            SettingsPage::General,
            SettingsPage::Models,
            SettingsPage::Themes,
            SettingsPage::Shortcuts,
            SettingsPage::Agents,
            SettingsPage::About,
        ]
        .into_iter()
        .map(|page| {
            let active = self.page == page;
            div()
                .id(SharedString::from(format!("settings-{page:?}")))
                .px_3()
                .py_2()
                .rounded_md()
                .bg(if active {
                    gpui::rgba(0x4f8fefff)
                } else {
                    gpui::rgba(0x242c38ff)
                })
                .cursor_pointer()
                .child(SharedString::from(format!("{page:?}")))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.set_page(page, cx)),
                )
        });
        div()
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::rgba(0x11151cff))
            .text_color(gpui::white())
            .child(
                canvas(
                    move |bounds, window, cx| {
                        window.handle_input(&input_focus, handler.clone(), cx);
                        bounds
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .children(tabs)
                    .child(div().flex_1())
                    .child(Self::button("Save", "save-settings").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.save(cx)),
                    )),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .p_5()
                    .text_sm()
                    .child(self.page_body(cx)),
            )
            .when(!self.status.is_empty(), |root| {
                root.child(
                    div()
                        .px_5()
                        .pb_3()
                        .text_xs()
                        .child(SharedString::from(self.status.clone())),
                )
            })
    }
}

#[derive(Clone)]
struct SettingsInputHandler {
    view: WeakEntity<SettingsView>,
}

impl InputHandler for SettingsInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let position = view
            .draft
            .chars()
            .take(view.cursor)
            .collect::<String>()
            .encode_utf16()
            .count();
        Some(UTF16Selection {
            range: position..position,
            reversed: false,
        })
    }
    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.upgrade()?;
        let len = view.read(cx).marked_text.encode_utf16().count();
        (len > 0).then_some(0..len)
    }
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                if view.edit_field.is_some() {
                    let byte = char_to_byte(&view.draft, view.cursor);
                    view.draft.insert_str(byte, text);
                    view.cursor += text.chars().count();
                    view.marked_text.clear();
                    cx.notify();
                }
            });
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = text.into();
                cx.notify();
            });
        }
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "On"
    } else {
        "Off"
    }
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}
