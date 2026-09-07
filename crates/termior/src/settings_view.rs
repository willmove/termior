use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Bounds, ClipboardItem, Context, FocusHandle,
    Focusable, Font, InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, StatefulInteractiveElement, TextRun, UTF16Selection,
    WeakEntity, Window,
};
use std::{ops::Range, path::PathBuf, time::Duration};

const SETTINGS_NAV_WIDTH: f32 = 180.0;
const SETTINGS_CONTENT_MAX_WIDTH: f32 = 640.0;
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, HttpProvider, KeyringSecretStore, ProviderConfig,
    SecretStore,
};
use termior_store::{
    atomic_write, default_keymap, settings::Appearance, DataFiles, KeyAction, Platform, Settings,
    UserKeyBinding,
};
use termior_theme::{
    resolve_active_palette, themes_for_native_appearance, NativeAppearance, ThemeLibrary,
};
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
type ThemePreviewCallback = Box<dyn Fn(&Settings, &mut App)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectMenu {
    ApplicationTheme,
    LightTheme,
    DarkTheme,
    EditorTheme,
    Appearance,
    ProviderProfile,
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
    /// 选区锚点（字符下标）。`None` 或与 cursor 重合时视为无选区；
    /// 有值时选区为 anchor..cursor，方向由二者大小决定。
    anchor: Option<usize>,
    /// 鼠标拖拽划选的固定端（字符下标）；仅在按住左键拖拽期间有效。
    drag_anchor: Option<usize>,
    /// 当前编辑字段文本内容区的 bounds（paint 期由 canvas 记录，供鼠标命中测试）。
    edit_bounds: Option<Bounds<Pixels>>,
    /// 测量编辑文本用的字体；render 期从窗口环境样式捕获，
    /// 保证与输入框实际渲染继承的字体一致。
    edit_font: Font,
    marked_text: String,
    credential_present: bool,
    /// 已输入但尚未写入钥匙串的 API key（需显式 Save API key）。
    pending_api_key: Option<String>,
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
    /// 进行中的异步任务(ping / rfd 对话框 / 防抖保存)。保存为字段而非 `.detach()`,
    /// 这样在设置窗口关闭、`SettingsView` 被 drop 时任务会随实体一并取消,
    /// 避免任务在 ~10s 后回写状态并触发对已销毁窗口的 `cx.notify()`,
    /// 产生 `window not found` / `无效的窗口句柄` 错误。
    pending_tasks: Vec<gpui::Task<()>>,
    /// 防抖保存代数；递增可取消尚未触发的写盘。
    save_generation: u64,
    /// `commit_edit` 改了 settings 后置位，由调用方 `schedule_save`。
    settings_dirty: bool,
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
            anchor: None,
            drag_anchor: None,
            edit_bounds: None,
            edit_font: Font::default(),
            marked_text: String::new(),
            credential_present: false,
            pending_api_key: None,
            capture_shortcut: None,
            themes,
            agents,
            agent_index: 0,
            status: String::new(),
            on_save,
            on_theme_preview,
            select_menu: None,
            palette: termior_theme::default_theme().palette().clone(),
            pending_tasks: Vec::new(),
            save_generation: 0,
            settings_dirty: false,
        };
        view.refresh_credential_state();
        view
    }

    fn set_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        self.page = page;
        cx.notify();
    }

    fn page_label(page: SettingsPage) -> &'static str {
        match page {
            SettingsPage::General => "General",
            SettingsPage::Models => "Models",
            SettingsPage::Themes => "Themes",
            SettingsPage::Shortcuts => "Shortcuts",
            SettingsPage::Agents => "Agents",
            SettingsPage::About => "About",
        }
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
        self.schedule_save_if_dirty(cx);
        self.edit_field = Some(field);
        self.draft = self.value_for(field);
        if field == EditField::ApiKey {
            self.draft.clear();
        }
        self.cursor = self.draft.chars().count();
        self.anchor = None;
        self.drag_anchor = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 当前选区（字符区间，start <= end）；无选区时返回 None。
    fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// 删除选区并把光标折叠到选区起点；返回是否确有选区被删除。
    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        let start = char_to_byte(&self.draft, range.start);
        let end = char_to_byte(&self.draft, range.end);
        self.draft.replace_range(start..end, "");
        self.cursor = range.start;
        self.anchor = None;
        true
    }

    /// 在光标处插入文本（替换当前选区）。单行字段丢弃换行符。
    fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.edit_field.is_none() {
            return;
        }
        let text = text.replace(['\r', '\n'], "");
        if text.is_empty() {
            return;
        }
        self.delete_selection();
        self.anchor = None;
        let byte = char_to_byte(&self.draft, self.cursor);
        self.draft.insert_str(byte, &text);
        self.cursor += text.chars().count();
        self.marked_text.clear();
        cx.notify();
    }

    /// 读取剪贴板并把文本粘贴进当前编辑字段。Windows 上打开剪贴板会向
    /// wndproc 回派 sent message，若在 entity 更新中直接读会重入崩溃，
    /// 因此推迟到更新结束之后（与 Composer 的 schedule_attach_clipboard 相同）。
    fn paste_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                return;
            };
            let _ = entity.update(cx, |this, cx| this.insert_text(&text, cx));
        });
    }

    /// 把当前选区复制到剪贴板；`cut` 为真时同时删除选区。
    fn copy_selection(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(range) = self.selection() else {
            return;
        };
        let text: String = self
            .draft
            .chars()
            .skip(range.start)
            .take(range.end - range.start)
            .collect();
        if text.is_empty() {
            return;
        }
        if cut && self.delete_selection() {
            cx.notify();
        }
        cx.defer(move |cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
    }

    /// 鼠标按下编辑字段：进入编辑并把光标/选区落到点击处。
    /// 单击定位光标，双击选词，三击全选；随后按住拖拽从对应端划选。
    fn begin_mouse_edit(
        &mut self,
        field: EditField,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_edit(field, window, cx);
        let index = self.char_index_at(event.position, window);
        match event.click_count {
            2 => {
                let range = self.word_range_at(index);
                self.anchor = Some(range.start);
                self.cursor = range.end;
                self.drag_anchor = Some(range.start);
            }
            3.. => {
                self.anchor = Some(0);
                self.cursor = self.draft.chars().count();
                self.drag_anchor = Some(0);
            }
            _ => {
                self.cursor = index;
                self.anchor = None;
                self.drag_anchor = Some(index);
            }
        }
        cx.notify();
    }

    /// 拖拽划选：固定端停在按下处，游标端跟随指针。
    fn drag_edit_to(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = self.char_index_at(position, window);
        if self.cursor != index || self.anchor.is_none() {
            self.cursor = index;
            self.anchor = Some(self.drag_anchor.unwrap_or(index));
            cx.notify();
        }
    }

    /// 把窗口坐标换算成编辑草稿的字符下标。API key 按掩码测量，与实际渲染一致。
    fn char_index_at(&self, position: Point<Pixels>, window: &mut Window) -> usize {
        let Some(field) = self.edit_field else {
            return 0;
        };
        let Some(bounds) = self.edit_bounds else {
            return self.draft.chars().count();
        };
        let text: String = if field == EditField::ApiKey {
            "•".repeat(self.draft.chars().count())
        } else {
            self.draft.clone()
        };
        if text.is_empty() {
            return 0;
        }
        let run = TextRun {
            len: text.len(),
            font: self.edit_font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().layout_line(
            &text,
            px(termior_ui_kit::tokens::font_size::BODY),
            &[run],
            None,
        );
        let local = position - bounds.origin;
        // 单行文本：只用 x 命中最接近的字符边界。
        let byte = line.closest_index_for_x(local.x).min(text.len());
        text[..byte].chars().count()
    }

    /// 双击选词的词边界：字母数字以及 URL/型号里常见的符号算词内字符。
    fn word_range_at(&self, index: usize) -> Range<usize> {
        let chars: Vec<char> = self.draft.chars().collect();
        let is_word =
            |c: char| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '+' | '=');
        let mut start = index.min(chars.len());
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = index.min(chars.len());
        while end < chars.len() && is_word(chars[end]) {
            end += 1;
        }
        start..end
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
                if self.pending_api_key.is_some() {
                    "•••••••• (unsaved — click Save API key)".into()
                } else if self.credential_present {
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
                // 敏感凭据不随字段失焦写入钥匙串，等显式 Save API key。
                if value.is_empty() {
                    Ok(())
                } else {
                    self.pending_api_key = Some(value);
                    self.status =
                        "API key ready — click Save API key to store it in the OS keychain".into();
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
        match result {
            Err(error) => self.status = format!("Invalid value: {error}"),
            Ok(()) if field != EditField::ApiKey => {
                self.settings_dirty = true;
            }
            Ok(()) => {}
        }
        self.draft.clear();
        self.cursor = 0;
        self.anchor = None;
        self.drag_anchor = None;
        self.marked_text.clear();
    }

    fn schedule_save_if_dirty(&mut self, cx: &mut Context<Self>) {
        if self.settings_dirty {
            self.settings_dirty = false;
            self.schedule_save(cx);
        }
    }

    /// 防抖写盘；逐字符编辑不会每次都落盘。
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        let task = cx.spawn(async move |view, cx| {
            cx.background_executor().timer(SETTINGS_SAVE_DEBOUNCE).await;
            let _ = view.update(cx, |view, cx| {
                if view.save_generation == generation {
                    view.persist(cx);
                }
            });
        });
        self.track_task(task);
    }

    /// 立即写盘（关闭窗口或换页前 flush）。
    pub fn flush_save(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        self.settings_dirty = false;
        self.save_generation = self.save_generation.wrapping_add(1);
        self.persist(cx);
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
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
                if self.status.starts_with("Save failed")
                    || self.status.starts_with("Invalid")
                    || self.status.is_empty()
                    || self.status == "Settings saved"
                {
                    self.status.clear();
                }
                if let Some(on_save) = &self.on_save {
                    on_save(&self.settings, cx);
                }
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
        cx.notify();
    }

    fn save_api_key(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        let Some(value) = self.pending_api_key.take() else {
            self.status = if self.credential_present {
                "API key already stored in the OS keychain".into()
            } else {
                "Enter an API key first".into()
            };
            cx.notify();
            return;
        };
        let key = self.profile_key().unwrap_or_default();
        match KeyringSecretStore::new().set(&key, &value) {
            Ok(()) => {
                self.credential_present = true;
                self.status = "API key saved to the OS keychain".into();
            }
            Err(error) => {
                self.pending_api_key = Some(value);
                self.status = format!("Could not save API key: {error}");
            }
        }
        cx.notify();
    }

    /// 仅写盘、不依赖 GPUI（窗口 Drop 时的兜底）。
    fn persist_to_disk(&self) -> Result<(), String> {
        self.settings
            .validate()
            .map_err(|error| error.to_string())?;
        let dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| "application data directory is unavailable".to_owned())?;
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let json =
            serde_json::to_string_pretty(&self.settings).map_err(|error| error.to_string())?;
        atomic_write(&dir.join("Termior-settings.json"), &json)
            .map_err(|error| error.to_string())?;
        let files = DataFiles::new(dir);
        files
            .themes()
            .save(&self.themes)
            .map_err(|error| error.to_string())?;
        files
            .agents()
            .save(&self.agents)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn select_profile(&mut self, index: usize, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        if index < self.settings.models.profiles.len() {
            self.profile_index = index;
        }
        self.select_menu = None;
        self.refresh_credential_state();
        self.status.clear();
        cx.notify();
    }

    fn toggle_provider(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.enabled = !profile.enabled;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// 设为默认聊天模型（Agent 面板使用）。Composer 只会选用已启用的 profile，
    /// 因此设默认时自动启用，避免「设了默认却仍不可用」的静默失效。
    fn make_active_chat(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profile().map(|p| p.id.clone()) else {
            return;
        };
        if let Some(profile) = self.profile_mut() {
            profile.enabled = true;
        }
        self.settings.models.active_chat_profile = Some(id);
        self.schedule_save(cx);
        cx.notify();
    }

    /// 设为默认补全模型（内联补全使用）。与聊天默认同理，设默认时自动启用。
    fn make_active_completion(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.profile().map(|p| p.id.clone()) else {
            return;
        };
        if let Some(profile) = self.profile_mut() {
            profile.enabled = true;
        }
        self.settings.models.active_completion_profile = Some(id);
        self.schedule_save(cx);
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
        if let Some(callback) = &self.on_theme_preview {
            callback(&self.settings, cx);
            return;
        }

        let themes = self.themes.all();
        let appearance = match self.settings.appearance {
            Appearance::Light => termior_theme::Appearance::Light,
            Appearance::Dark => termior_theme::Appearance::Dark,
            Appearance::FollowSystem => termior_theme::Appearance::FollowSystem,
        };
        crate::ui::set_palette(
            cx,
            resolve_active_palette(
                &themes,
                appearance,
                &self.settings.theme_id,
                &self.settings.light_theme_id,
                &self.settings.dark_theme_id,
                true,
            ),
        );
    }

    fn select_app_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        let Some(theme) = self
            .themes
            .all()
            .into_iter()
            .find(|theme| theme.id == theme_id)
        else {
            return;
        };
        self.settings.theme_id = theme.id.clone();
        self.settings.appearance = match theme.native_appearance {
            NativeAppearance::Light => Appearance::Light,
            NativeAppearance::Dark => Appearance::Dark,
        };
        match theme.native_appearance {
            NativeAppearance::Light => self.settings.light_theme_id = theme.id,
            NativeAppearance::Dark => self.settings.dark_theme_id = theme.id,
        }
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn select_pair_theme(
        &mut self,
        slot: NativeAppearance,
        theme_id: String,
        cx: &mut Context<Self>,
    ) {
        match slot {
            NativeAppearance::Light => self.settings.light_theme_id = theme_id,
            NativeAppearance::Dark => self.settings.dark_theme_id = theme_id,
        }
        self.settings.appearance = Appearance::FollowSystem;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
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
                        view.settings.theme_id = theme.id.clone();
                        view.settings.appearance = match theme.native_appearance {
                            NativeAppearance::Light => Appearance::Light,
                            NativeAppearance::Dark => Appearance::Dark,
                        };
                        match theme.native_appearance {
                            NativeAppearance::Light => {
                                view.settings.light_theme_id = theme.id;
                            }
                            NativeAppearance::Dark => {
                                view.settings.dark_theme_id = theme.id;
                            }
                        }
                        view.preview_theme_preferences(cx);
                        view.schedule_save(cx);
                        "Theme imported".into()
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
                view.schedule_save(cx);
                cx.notify();
            });
        });
        self.track_task(task);
    }

    fn clear_background(&mut self, cx: &mut Context<Self>) {
        self.settings.background.image_path = None;
        self.schedule_save(cx);
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
        self.schedule_save(cx);
        cx.notify();
    }

    fn cycle_agent(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
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
        self.schedule_save(cx);
        cx.notify();
    }

    fn select_editor_theme(&mut self, theme_id: String, cx: &mut Context<Self>) {
        self.settings.editor_theme_id = theme_id;
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn select_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.settings.appearance = appearance;
        match appearance {
            Appearance::Light => {
                self.settings.theme_id = self.settings.light_theme_id.clone();
            }
            Appearance::Dark => {
                self.settings.theme_id = self.settings.dark_theme_id.clone();
            }
            Appearance::FollowSystem => {}
        }
        self.select_menu = None;
        self.preview_theme_preferences(cx);
        self.schedule_save(cx);
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
                    self.schedule_save(cx);
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
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if primary && !modifiers.shift && !modifiers.alt {
            match event.keystroke.key.as_str() {
                "v" => self.paste_from_clipboard(cx),
                "a" => {
                    self.anchor = Some(0);
                    self.cursor = self.draft.chars().count();
                    cx.notify();
                }
                "c" => self.copy_selection(false, cx),
                "x" => self.copy_selection(true, cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        let shift = modifiers.shift;
        let extend = |this: &mut Self, target: usize| {
            this.anchor.get_or_insert(this.cursor);
            this.cursor = target;
        };
        match event.keystroke.key.as_str() {
            "enter" | "return" => {
                self.commit_edit();
                self.schedule_save_if_dirty(cx);
            }
            "escape" => {
                self.edit_field = None;
                self.draft.clear();
                self.anchor = None;
            }
            "backspace" if self.selection().is_some() || self.cursor > 0 => {
                if !self.delete_selection() {
                    let start = char_to_byte(&self.draft, self.cursor - 1);
                    let end = char_to_byte(&self.draft, self.cursor);
                    self.draft.replace_range(start..end, "");
                    self.cursor -= 1;
                }
                self.anchor = None;
            }
            "delete" if self.selection().is_some() || self.cursor < self.draft.chars().count() => {
                if !self.delete_selection() {
                    let start = char_to_byte(&self.draft, self.cursor);
                    let end = char_to_byte(&self.draft, self.cursor + 1);
                    self.draft.replace_range(start..end, "");
                }
                self.anchor = None;
            }
            "left" if shift => extend(self, self.cursor.saturating_sub(1)),
            "right" if shift => extend(self, (self.cursor + 1).min(self.draft.chars().count())),
            "home" if shift => extend(self, 0),
            "end" if shift => extend(self, self.draft.chars().count()),
            "left" => {
                self.cursor = self
                    .selection()
                    .map_or_else(|| self.cursor.saturating_sub(1), |range| range.start);
                self.anchor = None;
            }
            "right" => {
                self.cursor = self.selection().map_or_else(
                    || (self.cursor + 1).min(self.draft.chars().count()),
                    |range| range.end,
                );
                self.anchor = None;
            }
            "home" => {
                self.cursor = 0;
                self.anchor = None;
            }
            "end" => {
                self.cursor = self.draft.chars().count();
                self.anchor = None;
            }
            _ => return,
        }
        cx.notify();
    }

    /// 编辑中的输入框内容 + 光标。光标用 ASCII `|` 字符而不是 `▏`（U+258F）：
    /// 后者在 Windows 上经字体回退渲染成一个很宽的空白，还让鼠标命中测试
    /// 产生偏差。`|` 在任何 UI 字体里都是窄竖线，随文本流布局。
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
                    "|"
                } else {
                    "|…"
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

    /// 编辑中的输入框内容：无选区时整段文本加光标；有选区时高亮选中部分。
    /// 编辑期间在文本区铺一层透明 canvas，paint 时记录内容 bounds，供鼠标命中测试。
    fn edit_display(&self, field: EditField, cx: &Context<Self>) -> AnyElement {
        if self.edit_field != Some(field) {
            return div()
                .child(SharedString::from(self.value_for(field)))
                .into_any_element();
        }
        let bounds_probe = {
            let view = cx.entity().downgrade();
            canvas(
                |_, _, _| (),
                move |bounds, _, _, cx| {
                    let _ = view.update(cx, |view, _| view.edit_bounds = Some(bounds));
                },
            )
            .absolute()
            .size_full()
        };
        let chars: Vec<char> = if field == EditField::ApiKey {
            "•".repeat(self.draft.chars().count()).chars().collect()
        } else {
            self.draft.chars().collect()
        };
        let p = &self.palette;
        let run = |text: String, highlighted: bool| -> Option<AnyElement> {
            if text.is_empty() {
                return None;
            }
            let mut element = div();
            if highlighted {
                element = element
                    .bg(crate::ui::selected_wash(p))
                    .rounded_sm()
                    .text_color(crate::ui::color(p.foreground));
            }
            Some(element.child(SharedString::from(text)).into_any_element())
        };
        if let Some(range) = self.selection() {
            let prefix = chars[..range.start].iter().collect();
            let selected = chars[range.start..range.end].iter().collect();
            let suffix = chars[range.end..].iter().collect();
            div()
                .flex()
                .flex_row()
                .items_baseline()
                .children(run(prefix, false))
                .children(run(selected, true))
                .children(run(suffix, false))
                .child(bounds_probe)
                .into_any_element()
        } else {
            // 无选区：单一文本子节点（含 `|` 光标字符）。不做 flex 拆段加
            // 独立光标元素——拆段曾在真实字体下被压成逐字换行的竖排。
            div()
                .child(SharedString::from(self.display_edit(field)))
                .child(bounds_probe)
                .into_any_element()
        }
    }

    fn edit_row(
        &self,
        label: &'static str,
        description: &'static str,
        field: EditField,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = self.palette.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(label))
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&p))
                    .child(description),
            )
            .child(
                termior_ui_kit::input_field(&p, self.edit_field == Some(field))
                    .id(SharedString::from(format!("edit-{field:?}")))
                    .child(self.edit_display(field, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.begin_mouse_edit(field, event, window, cx)
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                            if this.edit_field == Some(field)
                                && event.pressed_button == Some(MouseButton::Left)
                            {
                                this.drag_edit_to(event.position, window, cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _event: &MouseUpEvent, _window, cx| {
                            if this.drag_anchor.take().is_some() {
                                cx.notify();
                            }
                        }),
                    ),
            )
            .into_any_element()
    }

    fn section(
        &self,
        title: &'static str,
        description: &'static str,
        children: impl IntoIterator<Item = AnyElement>,
    ) -> AnyElement {
        let p = &self.palette;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(description),
                    ),
            )
            .children(children)
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
            .bg(crate::ui::color(p.elevated))
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

    fn pair_theme_menu(
        &self,
        slot: NativeAppearance,
        selected_id: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let themes = self.themes.all();
        let candidates = themes_for_native_appearance(&themes, slot);
        div()
            .w(px(360.0))
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(crate::ui::border(&self.palette))
            .bg(crate::ui::color(self.palette.overlay))
            .shadow_md()
            .children(candidates.into_iter().map(|theme| {
                let theme_id = theme.id.clone();
                let selected = theme.id == selected_id;
                Self::select_option(
                    theme.name.clone(),
                    SharedString::from(format!("pair-theme-option-{}-{}", slot.as_str(), theme.id)),
                    selected,
                    &self.palette,
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_pair_theme(slot, theme_id.clone(), cx);
                    }),
                )
            }))
    }

    fn general_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let autocomplete = self.settings.autocomplete_enabled;
        let vim = self.settings.vim_mode;
        let dotfiles = self.settings.show_dotfiles;
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.section(
                "Terminal",
                "Defaults for new terminal panes. Changes apply after auto-save.",
                [
                    self.edit_row(
                        "Font family",
                        "Typeface used for terminal glyphs.",
                        EditField::FontFamily,
                        cx,
                    ),
                    self.edit_row(
                        "Font size",
                        "Point size between 8 and 32.",
                        EditField::FontSize,
                        cx,
                    ),
                    self.edit_row(
                        "Line height",
                        "Multiplier for line spacing (0.8–3.0).",
                        EditField::LineHeight,
                        cx,
                    ),
                    self.edit_row(
                        "Letter spacing",
                        "Extra glyph spacing in logical pixels (−2–8).",
                        EditField::LetterSpacing,
                        cx,
                    ),
                    self.edit_row(
                        "Scrollback rows",
                        "How many lines of history each terminal keeps (200–50,000).",
                        EditField::Scrollback,
                        cx,
                    ),
                ],
            ))
            .child(
                self.section(
                    "Editor & explorer",
                    "Cross-cutting preferences for editing and the file tree.",
                    [
                        self.edit_row(
                            "Custom instructions",
                            "Optional guidance appended to built-in agent prompts.",
                            EditField::Instructions,
                            cx,
                        ),
                        div()
                            .flex()
                            .flex_wrap()
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
                                        this.schedule_save(cx);
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
                                            this.schedule_save(cx);
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
                                        this.schedule_save(cx);
                                        cx.notify();
                                    }),
                                ),
                            )
                            .into_any_element(),
                    ],
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
        // 下拉菜单里直接标注每个 profile 的状态，不必逐个翻看就能找到
        // Agent 面板当前用的是哪一个。
        let profile_menu_open = self.select_menu == Some(SelectMenu::ProviderProfile);
        let profile_menu =
            profile_menu_open.then(|| {
                div()
                    .w(px(360.0))
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(&self.palette))
                    .bg(crate::ui::color(self.palette.overlay))
                    .shadow_md()
                    .children(self.settings.models.profiles.iter().enumerate().map(
                        |(index, entry)| {
                            let mut tags = Vec::new();
                            if self.settings.models.active_chat_profile.as_deref()
                                == Some(entry.id.as_str())
                            {
                                tags.push("chat ✓");
                            }
                            if self.settings.models.active_completion_profile.as_deref()
                                == Some(entry.id.as_str())
                            {
                                tags.push("completion ✓");
                            }
                            if !entry.enabled {
                                tags.push("disabled");
                            }
                            let label = if tags.is_empty() {
                                format!("{} · {}", entry.display_name, entry.model)
                            } else {
                                format!(
                                    "{} · {} · {}",
                                    entry.display_name,
                                    entry.model,
                                    tags.join(" · ")
                                )
                            };
                            Self::select_option(
                                SharedString::from(label),
                                SharedString::from(format!("provider-option-{}", entry.id)),
                                index == self.profile_index,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.select_profile(index, cx);
                                }),
                            )
                        },
                    ))
            });
        let chat_profile_label = self
            .settings
            .models
            .active_chat_profile
            .as_deref()
            .and_then(|id| self.settings.models.profiles.iter().find(|p| p.id == id))
            .map(|p| {
                format!(
                    "{} · {}{}",
                    p.display_name,
                    p.model,
                    // Composer 只选启用的 profile；标注禁用态，提示为何 Agent 面板没生效。
                    if p.enabled { "" } else { " (disabled)" }
                )
            })
            .unwrap_or_else(|| "Not set — the Agent panel cannot run".into());
        let completion_profile_label = self
            .settings
            .models
            .active_completion_profile
            .as_deref()
            .and_then(|id| self.settings.models.profiles.iter().find(|p| p.id == id))
            .map(|p| format!("{} · {}", p.display_name, p.model))
            .unwrap_or_else(|| "Not set".into());
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                self.section(
                    "Provider",
                    "Pick a profile to edit below. Credentials stay in the OS keychain and are never written to settings JSON.",
                    [div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            Self::select_button(
                                "Provider",
                                SharedString::from(format!(
                                    "{} ({}/{})",
                                    profile.display_name,
                                    self.profile_index + 1,
                                    self.settings.models.profiles.len()
                                )),
                                "provider-select",
                                profile_menu_open,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_select_menu(SelectMenu::ProviderProfile, cx);
                                }),
                            ),
                        )
                        .when_some(profile_menu, |select, menu| select.child(menu))
                        .into_any_element()],
                ),
            )
            .child(
                self.section(
                    "Default models",
                    "Which profile the Agent panel (chat) and inline completion use. Setting a default also enables the profile.",
                    [
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div().text_sm().child(SharedString::from(format!(
                                    "Chat (Agent panel): {chat_profile_label}"
                                ))),
                            )
                            .child(
                                div().text_sm().child(SharedString::from(format!(
                                    "Completion: {completion_profile_label}"
                                ))),
                            )
                            .into_any_element(),
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
                                        "✓ Use for chat"
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
                                        "✓ Use for completion"
                                    } else {
                                        "Use for completion"
                                    },
                                    "active-completion",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.make_active_completion(cx)
                                    }),
                                ),
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .child(self.section(
                "Endpoint",
                "Model id and base URL for the selected provider profile.",
                [
                    self.edit_row(
                        "Model",
                        "Provider model identifier used for requests.",
                        EditField::Model,
                        cx,
                    ),
                    self.edit_row(
                        "Base URL",
                        "HTTPS endpoint for the provider API.",
                        EditField::BaseUrl,
                        cx,
                    ),
                ],
            ))
            .child(
                self.section(
                    "Credentials",
                    "Saving an API key and testing connectivity require explicit actions.",
                    [
                        self.edit_row(
                            "API key",
                            "Never written to settings files — stored only in the OS keychain.",
                            EditField::ApiKey,
                            cx,
                        ),
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Self::button("Save API key", "save-api-key", &self.palette)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.save_api_key(cx)),
                                    ),
                            )
                            .child(
                                Self::button("Test connection", "ping-provider", &self.palette)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.ping_provider(cx)),
                                    ),
                            )
                            .into_any_element(),
                    ],
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
        let follow_system = self.settings.appearance == Appearance::FollowSystem;
        let app_theme_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.theme_id)
            .map(|theme| format!("{} · {}", theme.name, theme.native_appearance.as_str()))
            .unwrap_or_else(|| self.settings.theme_id.clone());
        let app_menu_open = self.select_menu == Some(SelectMenu::ApplicationTheme);
        let app_theme_menu = app_menu_open.then(|| {
            div()
                .w(px(360.0))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&self.palette))
                .bg(crate::ui::color(self.palette.overlay))
                .shadow_md()
                .children(app_themes.iter().map(|theme| {
                    let theme_id = theme.id.clone();
                    let selected = theme.id == self.settings.theme_id;
                    let label = format!("{} · {}", theme.name, theme.native_appearance.as_str());
                    Self::select_option(
                        label,
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

        let light_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.light_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.light_theme_id.clone());
        let dark_name = app_themes
            .iter()
            .find(|theme| theme.id == self.settings.dark_theme_id)
            .map(|theme| theme.name.clone())
            .unwrap_or_else(|| self.settings.dark_theme_id.clone());
        let light_menu_open = self.select_menu == Some(SelectMenu::LightTheme);
        let dark_menu_open = self.select_menu == Some(SelectMenu::DarkTheme);
        let light_theme_menu = light_menu_open.then(|| {
            self.pair_theme_menu(NativeAppearance::Light, &self.settings.light_theme_id, cx)
        });
        let dark_theme_menu = dark_menu_open.then(|| {
            self.pair_theme_menu(NativeAppearance::Dark, &self.settings.dark_theme_id, cx)
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
                .bg(crate::ui::color(self.palette.overlay))
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
                .bg(crate::ui::color(self.palette.overlay))
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

        let mut appearance_items: Vec<AnyElement> = vec![
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
                .when_some(app_theme_menu, |select, menu| select.child(menu))
                .into_any_element(),
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
                .when_some(appearance_menu, |select, menu| select.child(menu))
                .into_any_element(),
        ];
        if follow_system {
            appearance_items.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&self.palette))
                            .child(SharedString::from(
                                "Light slot lists light-native themes only; dark slot lists dark-native themes only.",
                            )),
                    )
                    .child(
                        Self::select_button(
                            "Light theme",
                            light_name,
                            "light-theme-select",
                            light_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::LightTheme, cx);
                            }),
                        ),
                    )
                    .when_some(light_theme_menu, |select, menu| select.child(menu))
                    .child(
                        Self::select_button(
                            "Dark theme",
                            dark_name,
                            "dark-theme-select",
                            dark_menu_open,
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_select_menu(SelectMenu::DarkTheme, cx);
                            }),
                        ),
                    )
                    .when_some(dark_theme_menu, |select, menu| select.child(menu))
                    .into_any_element(),
            );
        }
        appearance_items.push(
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
                .when_some(editor_theme_menu, |select, menu| select.child(menu))
                .into_any_element(),
        );
        appearance_items.push(
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
                )
                .into_any_element(),
        );

        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.section(
                "Appearance",
                "Each theme has a native light or dark look. Choosing a theme locks appearance to that native side. Follow system pairs one light theme with one dark theme.",
                appearance_items,
            ))
            .child(self.section(
                "Background",
                "Optional wallpaper behind the workspace chrome.",
                [
                    div()
                        .text_sm()
                        .child(SharedString::from(format!("Image: {background}")))
                        .into_any_element(),
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
                        )
                        .into_any_element(),
                    self.edit_row(
                        "Background opacity",
                        "0 = invisible overlay, 1 = fully opaque.",
                        EditField::BackgroundOpacity,
                        cx,
                    ),
                    self.edit_row(
                        "Background blur",
                        "Gaussian blur radius in logical pixels (0–64).",
                        EditField::BackgroundBlur,
                        cx,
                    ),
                ],
            ))
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
        self.section(
            "Keymap",
            "Click a row, then press a new Cmd/Ctrl chord. Conflicting bindings are rejected.",
            [div().flex().flex_col().children(rows).into_any_element()],
        )
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
                    self.edit_row(
                        "Name",
                        "Display name in the Composer agent switcher.",
                        EditField::AgentName,
                        cx,
                    ),
                    self.edit_row(
                        "System prompt",
                        "Instructions prepended to every chat with this agent.",
                        EditField::AgentPrompt,
                        cx,
                    ),
                    self.edit_row(
                        "Tools",
                        "Comma-separated tool ids this agent may call.",
                        EditField::AgentTools,
                        cx,
                    ),
                    self.edit_row(
                        "Icon",
                        "Icon key shown next to the agent name.",
                        EditField::AgentIcon,
                        cx,
                    ),
                    self.edit_row(
                        "Color",
                        "Accent color hex for the agent chip.",
                        EditField::AgentColor,
                        cx,
                    ),
                ])
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                self.section(
                    "Claude Code hooks",
                    "Install or remove Claude Code hooks that notify Termior about agent activity.",
                    [
                        div()
                            .text_sm()
                            .child(SharedString::from(format!("Status: {hook_status}")))
                            .into_any_element(),
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
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .child(
                self.section(
                    "Notifications",
                    "Desktop toasts when an agent finishes or needs attention.",
                    [Self::button(
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
                            this.schedule_save(cx);
                            cx.notify();
                        }),
                    )
                    .into_any_element()],
                ),
            )
            .child(
                self.section(
                    "Custom agents",
                    "Local agent profiles available in the Composer switcher.",
                    [
                        Self::button("New custom agent", "new-agent", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.new_agent(cx)),
                            )
                            .into_any_element(),
                        agent_controls,
                    ],
                ),
            )
            .into_any_element()
    }

    fn page_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Models => self.models_page(cx),
            SettingsPage::Themes => self.themes_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(cx),
            SettingsPage::Agents => self.agents_page(cx),
            SettingsPage::About => self.section(
                "About",
                "Build identity and settings migration status.",
                [
                    div()
                        .text_sm()
                        .child(format!("Termior {}", env!("CARGO_PKG_VERSION")))
                        .into_any_element(),
                    div()
                        .text_sm()
                        .text_color(crate::ui::muted(&self.palette))
                        .child(
                            "Apache-2.0 · No account · No telemetry · Offline with local providers",
                        )
                        .into_any_element(),
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(&self.palette))
                        .child(SharedString::from(format!(
                            "Migration status: {}",
                            self.migration_error.as_deref().unwrap_or("OK")
                        )))
                        .into_any_element(),
                ],
            ),
        }
    }
}

impl Drop for SettingsView {
    fn drop(&mut self) {
        self.commit_edit();
        let _ = self.persist_to_disk();
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.palette = crate::ui::palette(cx);
        // 捕获环境文本样式里的字体，鼠标命中测试用它做与渲染一致的文本测量。
        let style = window.text_style();
        self.edit_font = Font {
            family: style.font_family.clone(),
            weight: style.font_weight,
            style: style.font_style,
            features: style.font_features.clone(),
            fallbacks: style.font_fallbacks.clone(),
        };
        let p = self.palette.clone();
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = SettingsInputHandler {
            view: cx.entity().downgrade(),
        };
        let nav = [
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
                .id(SharedString::from(format!("settings-nav-{page:?}")))
                .w_full()
                .px_3()
                .py_2()
                .rounded_md()
                .when(active, |item| item.bg(crate::ui::selected_wash(&p)))
                .text_color(if active {
                    crate::ui::color(p.foreground)
                } else {
                    crate::ui::muted(&p)
                })
                .when(!active, |item| {
                    let wash = crate::ui::hover_wash(&p);
                    item.hover(move |style| style.bg(wash))
                })
                .cursor_pointer()
                .child(Self::page_label(page))
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
                    .w(px(SETTINGS_NAV_WIDTH))
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_4()
                    .border_r_1()
                    .border_color(crate::ui::border(&p))
                    .bg(crate::ui::color(p.panel))
                    .children(nav),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("settings-scroll")
                            .flex_1()
                            .overflow_y_scroll()
                            .px_6()
                            .py_5()
                            .child(
                                div().flex().justify_center().w_full().child(
                                    div()
                                        .w_full()
                                        .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
                                        .text_sm()
                                        .child(self.page_body(cx)),
                                ),
                            ),
                    )
                    .when(!self.status.is_empty(), |root| {
                        root.child(
                            div().flex().justify_center().w_full().px_6().pb_3().child(
                                div()
                                    .w_full()
                                    .max_w(px(SETTINGS_CONTENT_MAX_WIDTH))
                                    .text_xs()
                                    .text_color(crate::ui::muted(&p))
                                    .child(SharedString::from(self.status.clone())),
                            ),
                        )
                    }),
            )
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
        let utf16 = |char_index: usize| {
            view.draft
                .chars()
                .take(char_index)
                .collect::<String>()
                .encode_utf16()
                .count()
        };
        match view.selection() {
            Some(range) => Some(UTF16Selection {
                range: utf16(range.start)..utf16(range.end),
                reversed: view.anchor.is_some_and(|anchor| anchor > view.cursor),
            }),
            None => {
                let position = utf16(view.cursor);
                Some(UTF16Selection {
                    range: position..position,
                    reversed: false,
                })
            }
        }
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
                    view.insert_text(text, cx);
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

#[cfg(test)]
mod edit_tests {
    use super::*;
    use gpui::TestAppContext;

    fn view_with_draft(cx: &mut TestAppContext, draft: &str) -> gpui::Entity<SettingsView> {
        cx.new(|cx| {
            let mut view = SettingsView::new(Settings::default(), None, None, None, None, cx);
            view.edit_field = Some(EditField::Model);
            view.draft = draft.to_owned();
            view.cursor = draft.chars().count();
            view
        })
    }

    #[test]
    fn paste_replaces_selection() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "abcdef");
        view.update(&mut cx, |view, cx| {
            view.cursor = 2;
            view.anchor = Some(4);
            view.insert_text("XY", cx);
            assert_eq!(view.draft, "abXYef");
            assert_eq!(view.cursor, 4);
            assert_eq!(view.anchor, None);
        });
    }

    #[test]
    fn paste_inserts_at_cursor_without_selection() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "abc");
        view.update(&mut cx, |view, cx| {
            view.cursor = 1;
            view.insert_text("XY", cx);
            assert_eq!(view.draft, "aXYbc");
            assert_eq!(view.cursor, 3);
            assert_eq!(view.anchor, None);
        });
    }

    #[test]
    fn paste_strips_line_breaks_for_single_line_fields() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "");
        view.update(&mut cx, |view, cx| {
            view.insert_text("sk-\r\n123\n", cx);
            assert_eq!(view.draft, "sk-123");
        });
    }

    #[test]
    fn collapsed_anchor_does_not_resurrect_selection_after_paste() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "ab");
        view.update(&mut cx, |view, cx| {
            // Shift+Left 再 Shift+Right 会留下 anchor == cursor 的折叠态。
            view.anchor = Some(1);
            view.cursor = 1;
            view.insert_text("X", cx);
            assert_eq!(view.draft, "aXb");
            assert_eq!(view.selection(), None);
        });
    }

    #[test]
    fn delete_selection_collapses_cursor_to_start() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "abcdef");
        view.update(&mut cx, |view, _cx| {
            view.cursor = 2;
            view.anchor = Some(5);
            assert_eq!(view.selection(), Some(2..5));
            assert!(view.delete_selection());
            assert_eq!(view.draft, "abf");
            assert_eq!(view.cursor, 2);
            assert_eq!(view.anchor, None);
        });
    }

    #[test]
    fn select_all_spans_whole_draft() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "gpt-4o");
        view.update(&mut cx, |view, _cx| {
            view.anchor = Some(0);
            assert_eq!(view.selection(), Some(0..6));
        });
    }

    #[test]
    fn double_click_word_range_covers_urls_and_symbols() {
        let mut cx = TestAppContext::single();
        let view = view_with_draft(&mut cx, "https://example.com/a b");
        view.update(&mut cx, |view, _cx| {
            // 点击 URL 内部：整段 URL 算一个词。
            assert_eq!(view.word_range_at(5), 0..21);
            // 点击 URL 与后续单词之间的空格边界：向左扩展到 URL 结尾。
            assert_eq!(view.word_range_at(21), 0..21);
            // 点击最后一个单词：只选中它。
            assert_eq!(view.word_range_at(22), 22..23);
        });
    }

    /// Composer 只选用已启用的 profile；设默认时必须连带启用，
    /// 否则「设了默认聊天模型但 Agent 面板仍不可用」会静默失效。
    #[test]
    fn make_active_chat_enables_profile() {
        let mut cx = TestAppContext::single();
        let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
        view.update(&mut cx, |view, cx| {
            assert!(!view.settings.models.profiles[0].enabled);
            view.make_active_chat(cx);
            assert!(view.settings.models.profiles[0].enabled);
            assert_eq!(
                view.settings.models.active_chat_profile.as_deref(),
                Some("anthropic")
            );
        });
    }

    #[test]
    fn select_profile_switches_index_and_clamps() {
        let mut cx = TestAppContext::single();
        let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
        view.update(&mut cx, |view, cx| {
            view.select_profile(7, cx); // DeepSeek
            assert_eq!(view.profile().map(|p| p.id.as_str()), Some("deepseek"));
            assert_eq!(view.select_menu, None);
            view.select_profile(usize::MAX, cx); // 越界时保持原索引
            assert_eq!(view.profile().map(|p| p.id.as_str()), Some("deepseek"));
        });
    }
}
