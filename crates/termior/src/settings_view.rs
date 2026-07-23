use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Bounds, Context, FocusHandle, Focusable,
    InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    StatefulInteractiveElement, UTF16Selection, WeakEntity, Window,
};
use std::ops::Range;
use std::path::PathBuf;
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, HttpProvider, KeyringSecretStore, ProviderConfig,
    SecretStore,
};
use termior_store::settings::Appearance;
use termior_store::{
    atomic_write, default_keymap, DataFiles, KeyAction, Platform, Settings, UserKeyBinding,
};
use termior_theme::ThemeLibrary;
use termior_ui::SettingsPage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    FontFamily,
    FontSize,
    LineHeight,
    LetterSpacing,
    Scrollback,
    Instructions,
    Model,
    BaseUrl,
    ApiKey,
    BackgroundOpacity,
    BackgroundBlur,
    AgentName,
    AgentPrompt,
    AgentTools,
    AgentIcon,
    AgentColor,
}

type SaveCallback = Box<dyn Fn(&Settings, &mut App)>;
type ThemePreviewCallback = Box<dyn Fn(&termior_theme::Theme, &str, Appearance, &mut App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectMenu {
    ApplicationTheme,
    EditorTheme,
    Appearance,
}

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
    capture_shortcut: Option<KeyAction>,
    themes: ThemeLibrary,
    agents: AgentDefinitionStore,
    agent_index: usize,
    status: String,
    on_save: Option<SaveCallback>,
    on_theme_preview: Option<ThemePreviewCallback>,
    select_menu: Option<SelectMenu>,
    /// 渲染期缓存的当前主题色板(每帧从全局刷新,供 edit_row 等辅助方法使用)。
    palette: termior_theme::ResolvedPalette,
    /// 进行中的异步任务(ping / rfd 对话框)。保存为字段而非 `.detach()`,
    /// 这样在设置窗口关闭、`SettingsView` 被 drop 时任务会随实体一并取消,
    /// 避免任务在 ~10s 后回写状态并触发对已销毁窗口的 `cx.notify()`,
    /// 产生 `window not found` / `无效的窗口句柄` 错误。
    pending_tasks: Vec<gpui::Task<()>>,
}

impl SettingsView {
    pub fn new(
        settings: Settings,
        migration_error: Option<String>,
        data_dir: Option<PathBuf>,
        on_save: Option<SaveCallback>,
        on_theme_preview: Option<ThemePreviewCallback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let themes = data_dir
            .as_ref()
            .and_then(|dir| DataFiles::new(dir).themes().load().ok())
            .unwrap_or_default();
        let agents = data_dir
            .as_ref()
            .and_then(|dir| DataFiles::new(dir).agents().load().ok())
            .unwrap_or_default();
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
            capture_shortcut: None,
            themes,
            agents,
            agent_index: 0,
            status: String::new(),
            on_save,
            on_theme_preview,
            select_menu: None,
            palette: termior_theme::default_theme().resolve(termior_theme::Appearance::Dark, true),
            pending_tasks: Vec::new(),
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

    fn agent(&self) -> Option<&AgentDefinition> {
        self.agents.agents.get(self.agent_index)
    }

    fn agent_mut(&mut self) -> Option<&mut AgentDefinition> {
        self.agents.agents.get_mut(self.agent_index)
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

    /// 跟踪一个 fire-and-forget 的异步任务,使其在设置窗口关闭时随实体一起被取消。
    /// 丢弃已完成的任务,避免无界增长。
    fn track_task(&mut self, task: gpui::Task<()>) {
        self.pending_tasks.retain(|task| !task.is_ready());
        self.pending_tasks.push(task);
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
            EditField::LineHeight => self.settings.terminal.line_height.to_string(),
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
            EditField::BackgroundOpacity => self.settings.background.opacity.to_string(),
            EditField::BackgroundBlur => self.settings.background.blur.to_string(),
            EditField::AgentName => self.agent().map(|a| a.name.clone()).unwrap_or_default(),
            EditField::AgentPrompt => self
                .agent()
                .map(|a| a.system_prompt.clone())
                .unwrap_or_default(),
            EditField::AgentTools => self.agent().map(|a| a.tools.join(", ")).unwrap_or_default(),
            EditField::AgentIcon => self.agent().map(|a| a.icon.clone()).unwrap_or_default(),
            EditField::AgentColor => self.agent().map(|a| a.color.clone()).unwrap_or_default(),
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
            EditField::LineHeight => value
                .parse::<f32>()
                .map(|value| self.settings.terminal.line_height = value)
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
            EditField::BackgroundOpacity => value
                .parse::<f32>()
                .map(|value| self.settings.background.opacity = value)
                .map_err(|error| error.to_string()),
            EditField::BackgroundBlur => value
                .parse::<f32>()
                .map(|value| self.settings.background.blur = value)
                .map_err(|error| error.to_string()),
            EditField::AgentName => {
                if let Some(agent) = self.agent_mut() {
                    agent.name = value;
                }
                Ok(())
            }
            EditField::AgentPrompt => {
                if let Some(agent) = self.agent_mut() {
                    agent.system_prompt = value;
                }
                Ok(())
            }
            EditField::AgentTools => {
                if let Some(agent) = self.agent_mut() {
                    agent.tools = value
                        .split(',')
                        .map(str::trim)
                        .filter(|tool| !tool.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                Ok(())
            }
            EditField::AgentIcon => {
                if let Some(agent) = self.agent_mut() {
                    agent.icon = value;
                }
                Ok(())
            }
            EditField::AgentColor => {
                if let Some(agent) = self.agent_mut() {
                    agent.color = value;
                }
                Ok(())
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
        let result = self
            .data_dir
            .as_ref()
            .map_or_else(
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
            )
            .and_then(|()| {
                let dir = self
                    .data_dir
                    .as_ref()
                    .ok_or_else(|| "application data directory is unavailable".to_owned())?;
                let files = DataFiles::new(dir);
                files
                    .themes()
                    .save(&self.themes)
                    .map_err(|error| error.to_string())?;
                files
                    .agents()
                    .save(&self.agents)
                    .map_err(|error| error.to_string())
            });
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
        let task = cx.spawn(async move |view, cx| {
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
        });
        self.track_task(task);
        cx.notify();
    }

    fn toggle_select_menu(&mut self, menu: SelectMenu, cx: &mut Context<Self>) {
        self.select_menu = (self.select_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn preview_theme_preferences(&self, cx: &mut Context<Self>) {
        let Some(theme) = self
            .themes
            .all()
            .into_iter()
            .find(|theme| theme.id == self.settings.theme_id)
        else {
            return;
        };
        if let Some(callback) = &self.on_theme_preview {
            callback(
                &theme,
                &self.settings.editor_theme_id,
                self.settings.appearance,
                cx,
            );
            return;
        }

        let appearance = match self.settings.appearance {
            Appearance::Light => termior_theme::Appearance::Light,
            Appearance::Dark => termior_theme::Appearance::Dark,
            Appearance::FollowSystem => termior_theme::Appearance::FollowSystem,
        };
        crate::ui::set_palette(cx, theme.resolve(appearance, true));
    }

    fn select_app_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        self.settings.theme_id = theme_id;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        cx.notify();
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
        // Blocking dialogs inside GPUI handlers deadlock/panic on Windows
        // (re-entrant dispatch while the `App` RefCell is borrowed), so the
        // async dialog is awaited before touching the view again.
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .add_filter("Theme JSON", &["json"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                view.status = match std::fs::read_to_string(path)
                    .map_err(|error| error.to_string())
                    .and_then(|json| view.themes.import(&json).map_err(|error| error.to_string()))
                {
                    Ok(theme) => {
                        view.settings.theme_id = theme.id;
                        view.preview_theme_preferences(cx);
                        "Theme imported and previewed; save settings to keep it".into()
                    }
                    Err(error) => format!("Theme import failed: {error}"),
                };
                cx.notify();
            });
        });
        self.track_task(task);
    }

    fn export_theme(&mut self, cx: &mut Context<Self>) {
        let Some(theme) = self
            .themes
            .all()
            .into_iter()
            .find(|theme| theme.id == self.settings.theme_id)
        else {
            self.status = "Selected theme was not found".into();
            cx.notify();
            return;
        };
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .set_file_name(format!("{}.json", theme.id))
                .save_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let status = match ThemeLibrary::export(&theme)
                .map_err(|error| error.to_string())
                .and_then(|json| atomic_write(&path, &json).map_err(|error| error.to_string()))
            {
                Ok(()) => format!("Theme exported to {}", path.display()),
                Err(error) => format!("Theme export failed: {error}"),
            };
            let _ = view.update(cx, |view, cx| {
                view.status = status;
                cx.notify();
            });
        });
        self.track_task(task);
    }

    fn select_background(&mut self, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |view, cx| {
            let Some(handle) = rfd::AsyncFileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                view.settings.background.image_path = Some(path.to_string_lossy().into_owned());
                cx.notify();
            });
        });
        self.track_task(task);
    }

    fn clear_background(&mut self, cx: &mut Context<Self>) {
        self.settings.background.image_path = None;
        cx.notify();
    }

    fn new_agent(&mut self, cx: &mut Context<Self>) {
        let next = (1..)
            .find(|index| {
                !self
                    .agents
                    .agents
                    .iter()
                    .any(|agent| agent.id == format!("custom-agent-{index}"))
            })
            .unwrap_or(1);
        self.agents.agents.push(AgentDefinition {
            id: format!("custom-agent-{next}"),
            name: format!("Custom Agent {next}"),
            system_prompt: "You are a focused coding assistant.".into(),
            tools: Vec::new(),
            icon: "agent".into(),
            color: "#4f8fef".into(),
        });
        self.agent_index = self.agents.agents.len() - 1;
        cx.notify();
    }

    fn cycle_agent(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.commit_edit();
        let len = self.agents.agents.len();
        if len > 0 {
            self.agent_index =
                (self.agent_index as isize + delta).rem_euclid(len as isize) as usize;
        }
        cx.notify();
    }

    fn remove_agent(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        if self.agent_index < self.agents.agents.len() {
            self.agents.agents.remove(self.agent_index);
            self.agent_index = self
                .agent_index
                .min(self.agents.agents.len().saturating_sub(1));
        }
        cx.notify();
    }

    fn select_editor_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        self.settings.editor_theme_id = theme_id;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        cx.notify();
    }

    fn select_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = appearance;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
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
        if let Some(action) = self.capture_shortcut {
            let key = event.keystroke.key.as_str();
            if key == "escape" {
                self.capture_shortcut = None;
                self.status = "Shortcut capture cancelled".into();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if matches!(key, "control" | "shift" | "alt" | "meta" | "command") {
                return;
            }
            let modifiers = event.keystroke.modifiers;
            let has_required_modifier = if cfg!(target_os = "macos") {
                modifiers.platform || modifiers.control
            } else {
                modifiers.control
            };
            if !has_required_modifier {
                self.status = "Shortcuts must include Cmd/Ctrl".into();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            if key.eq_ignore_ascii_case("s")
                && !modifiers.shift
                && !modifiers.alt
                && (modifiers.control || modifiers.platform)
            {
                self.status = "Cmd/Ctrl+S is reserved for saving editor files".into();
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let binding = UserKeyBinding {
                primary: if cfg!(target_os = "macos") {
                    modifiers.platform
                } else {
                    true
                },
                shift: modifiers.shift,
                alt: modifiers.alt,
                key: event.keystroke.key.clone(),
            };
            self.status = match self.settings.keymap.rebind(action, binding) {
                Ok(()) => {
                    self.capture_shortcut = None;
                    format!("Updated {action:?}")
                }
                Err(error) => error.to_string(),
            };
            cx.stop_propagation();
            cx.notify();
            return;
        }
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

    fn capture_shortcut(&mut self, action: KeyAction, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit();
        self.capture_shortcut = Some(action);
        self.status = format!("Press the new Cmd/Ctrl shortcut for {action:?}");
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn edit_row(
        &self,
        label: &'static str,
        field: EditField,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&p))
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
                        crate::ui::color(p.accent)
                    } else {
                        crate::ui::border(&p)
                    })
                    .bg(crate::ui::color(p.surface[1]))
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
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        crate::ui::button(id, label, crate::ui::ButtonKind::Subtle, p)
            .px_3()
            .py_2()
            .text_sm()
    }

    fn select_button(
        label: &str,
        value: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        open: bool,
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w(px(360.0))
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if open {
                crate::ui::color(p.accent)
            } else {
                crate::ui::border(p)
            })
            .bg(crate::ui::color(p.surface[1]))
            .text_sm()
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(crate::ui::muted(p))
                            .child(SharedString::from(label.to_owned())),
                    )
                    .child(value.into()),
            )
            .child(if open { "▴" } else { "▾" })
            .hover({
                let wash = crate::ui::hover_wash(p);
                move |style| style.bg(wash)
            })
    }

    fn select_option(
        label: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        selected: bool,
        p: &termior_theme::ResolvedPalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .w_full()
            .px_3()
            .py_2()
            .rounded_sm()
            .text_sm()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_2()
            .when(selected, |option| option.bg(crate::ui::selected_wash(p)))
            .when(!selected, |option| {
                let wash = crate::ui::hover_wash(p);
                option.hover(move |style| style.bg(wash))
            })
            .child(if selected { "✓" } else { " " })
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
                self.edit_row("Line height (0.8–3)", EditField::LineHeight, cx),
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
                            &self.palette,
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
                        Self::button(format!("Vim: {}", on_off(vim)), "vim", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.settings.vim_mode = !this.settings.vim_mode;
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        Self::button(
                            format!("Dotfiles: {}", on_off(dotfiles)),
                            "dotfiles",
                            &self.palette,
                        )
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
                    .child(
                        Self::button("‹", "previous-provider", &self.palette).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.cycle_profile(-1, cx)),
                        ),
                    )
                    .child(div().flex_1().text_lg().child(SharedString::from(format!(
                        "{}  ({}/{})",
                        profile.display_name,
                        self.profile_index + 1,
                        self.settings.models.profiles.len()
                    ))))
                    .child(
                        Self::button("›", "next-provider", &self.palette).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.cycle_profile(1, cx)),
                        ),
                    ),
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
                            &self.palette,
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
                            &self.palette,
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
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.make_active_completion(cx)),
                        ),
                    )
                    .child(
                        Self::button("Test connection", "ping-provider", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.ping_provider(cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn themes_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let background = self
            .settings
            .background
            .image_path
            .as_deref()
            .unwrap_or("None")
            .to_owned();
        let app_themes = self.themes.all();
        let app_theme_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.theme_id.clone());
        let app_menu_open = self.select_menu == Some(SelectMenu::ApplicationTheme);
        let app_theme_menu = app_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.surface[2]))
                .shadow_md()
                .children(app_themes.into_iter().map(|theme| {
                    let theme_id = theme.id.clone();
                    let selected = theme.id == self.settings.theme_id;
                    Self::select_option(
                        theme.name,
                        SharedString::from(format!("app-theme-option-{}", theme.id)),
                        selected,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_app_theme(theme_id.clone(), cx);
                        }),
                    )
                }))
        });

        let editor_themes = termior_editor::builtin_editor_themes();
        let editor_theme_name = editor_themes
            .iter()
            .find(|theme| theme.id == self.settings.editor_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.editor_theme_id.clone());
        let editor_menu_open = self.select_menu == Some(SelectMenu::EditorTheme);
        let editor_theme_menu = editor_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.surface[2]))
                .shadow_md()
                .children(editor_themes.into_iter().map(|theme| {
                    let theme_id = theme.id.clone();
                    let selected = theme.id == self.settings.editor_theme_id;
                    Self::select_option(
                        theme.name,
                        SharedString::from(format!("editor-theme-option-{}", theme.id)),
                        selected,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_editor_theme(theme_id.clone(), cx);
                        }),
                    )
                }))
        });

        let appearance_menu_open = self.select_menu == Some(SelectMenu::Appearance);
        let appearance_label = match self.settings.appearance {
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
            Appearance::FollowSystem => "Follow system",
        };
        let appearance_menu = appearance_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.surface[2]))
                .shadow_md()
                .children(
                    [
                        (Appearance::Light, "Light"),
                        (Appearance::Dark, "Dark"),
                        (Appearance::FollowSystem, "Follow system"),
                    ]
                    .into_iter()
                    .map(|(appearance, label)| {
                        Self::select_option(
                            label,
                            SharedString::from(format!("appearance-option-{appearance:?}")),
                            appearance == self.settings.appearance,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.select_appearance(appearance, cx);
                            }),
                        )
                    }),
                )
        });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Self::select_button(
                            "Application theme",
                            app_theme_name,
                            "app-theme-select",
                            app_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::ApplicationTheme, cx);
                            }),
                        ),
                    )
                    .when_some(app_theme_menu, |select, menu| select.child(menu)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Self::select_button(
                            "Editor theme",
                            editor_theme_name,
                            "editor-theme-select",
                            editor_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::EditorTheme, cx);
                            }),
                        ),
                    )
                    .when_some(editor_theme_menu, |select, menu| select.child(menu)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Self::select_button(
                            "Appearance",
                            appearance_label,
                            "appearance-select",
                            appearance_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::Appearance, cx);
                            }),
                        ),
                    )
                    .when_some(appearance_menu, |select, menu| select.child(menu)),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Self::button("Import theme", "import-theme", &self.palette).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.import_theme(cx)),
                        ),
                    )
                    .child(
                        Self::button("Export theme", "export-theme", &self.palette).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.export_theme(cx)),
                        ),
                    ),
            )
            .child(SharedString::from(format!("Background: {background}")))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Self::button("Choose image", "background-image", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.select_background(cx)),
                            ),
                    )
                    .child(
                        Self::button("Clear image", "background-clear", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.clear_background(cx)),
                            ),
                    ),
            )
            .child(self.edit_row("Background opacity (0–1)", EditField::BackgroundOpacity, cx))
            .child(self.edit_row("Background blur (0–64)", EditField::BackgroundBlur, cx))
            .into_any_element()
    }

    fn shortcuts_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = default_keymap().into_iter().map(|entry| {
            let action = entry.action;
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
                .id(SharedString::from(format!("shortcut-{action:?}")))
                .flex()
                .justify_between()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(self.capture_shortcut == Some(action), |row| {
                    row.bg(crate::ui::selected_wash(&self.palette))
                })
                .child(SharedString::from(format!("{:?}", entry.action)))
                .child(SharedString::from(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.capture_shortcut(action, window, cx)
                    }),
                )
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
        let agent_controls = if self.agents.agents.is_empty() {
            div().child("No custom agents yet").into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(SharedString::from(format!(
                    "Custom agent {}/{} · {}",
                    self.agent_index + 1,
                    self.agents.agents.len(),
                    self.agent()
                        .map(|agent| agent.id.as_str())
                        .unwrap_or_default()
                )))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Self::button("‹", "previous-agent", &self.palette).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cycle_agent(-1, cx)),
                            ),
                        )
                        .child(
                            Self::button("›", "next-agent", &self.palette).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cycle_agent(1, cx)),
                            ),
                        )
                        .child(
                            Self::button("Remove", "remove-agent", &self.palette).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.remove_agent(cx)),
                            ),
                        ),
                )
                .children([
                    self.edit_row("Name", EditField::AgentName, cx),
                    self.edit_row("System prompt", EditField::AgentPrompt, cx),
                    self.edit_row("Tools (comma separated)", EditField::AgentTools, cx),
                    self.edit_row("Icon", EditField::AgentIcon, cx),
                    self.edit_row("Color", EditField::AgentColor, cx),
                ])
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(SharedString::from(format!(
                "Claude Code hooks: {hook_status}"
            )))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Self::button("Install hooks", "install-hooks", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.install_hooks(cx)),
                            ),
                    )
                    .child(
                        Self::button("Uninstall hooks", "uninstall-hooks", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.uninstall_hooks(cx)),
                            ),
                    ),
            )
            .child(
                Self::button(
                    format!(
                        "Agent notifications: {}",
                        on_off(self.settings.agent_notifications)
                    ),
                    "agent-notifications",
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.settings.agent_notifications = !this.settings.agent_notifications;
                        cx.notify();
                    }),
                ),
            )
            .child(
                Self::button("New custom agent", "new-agent", &self.palette).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.new_agent(cx)),
                ),
            )
            .child(agent_controls)
            .into_any_element()
    }

    fn page_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Models => self.models_page(cx),
            SettingsPage::Themes => self.themes_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(cx),
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
        self.palette = crate::ui::palette(cx);
        let p = self.palette.clone();
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
                    crate::ui::color(p.accent)
                } else {
                    crate::ui::color(p.surface[1])
                })
                .text_color(if active {
                    crate::ui::on_color(p.accent)
                } else {
                    crate::ui::muted(&p)
                })
                .when(!active, |tab| {
                    let wash = crate::ui::hover_wash(&p);
                    tab.hover(move |style| style.bg(wash))
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.select_menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(crate::ui::color(p.background))
            .text_color(crate::ui::color(p.foreground))
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |_, _, window, cx| {
                        window.handle_input(&input_focus, handler, cx);
                    },
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
                    .child(
                        Self::button("Save", "save-settings", &self.palette).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.save(cx)),
                        ),
                    ),
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
