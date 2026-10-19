pub struct OpenSshManager;
impl gpui::EventEmitter<OpenSshManager> for SettingsView {}

use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Bounds, ClipboardItem, Context, FocusHandle,
    Focusable, Font, InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, StatefulInteractiveElement, TextRun, UTF16Selection,
    WeakEntity, Window,
};
use std::{
    ops::Range,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SETTINGS_NAV_WIDTH: f32 = 180.0;
const SETTINGS_CONTENT_MAX_WIDTH: f32 = 640.0;
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);
use crate::SettingsPage;
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, HttpProvider, KeyringSecretStore, ProviderConfig,
    SecretStore,
};
use termior_store::{
    atomic_write, default_keymap, settings::Appearance, DataFiles, DiskUsage, KeyAction, Platform,
    RetentionPolicy, Settings, ShellDetection, UserKeyBinding,
};
use termior_terminal::DiscoveredShell;
use termior_theme::{
    resolve_active_palette, themes_for_native_appearance, NativeAppearance, ThemeLibrary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    FontFamily,
    FontSize,
    LineHeight,
    LetterSpacing,
    Scrollback,
    ShellPath,
    Instructions,
    AddModel,
    BaseUrl,
    ApiKey,
    BackgroundOpacity,
    BackgroundBlur,
    AgentName,
    AgentPrompt,
    AgentIcon,
    AgentColor,
    RetentionDays,
    RetentionCapacity,
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
    Language,
    Shell,
    ProviderProfile,
    AgentProfile,
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
    /// IME 候选窗锚点（编辑中文本字段的探针每帧刷新）。
    ime_anchor: crate::ime_anchor::ImeAnchor,
    credential_present: bool,
    /// 已输入但尚未写入钥匙串的 API key（需显式 Save API key）。
    pending_api_key: Option<String>,
    credential_drafts: std::collections::HashMap<String, String>,
    agent_appearance_open: bool,
    confirm_agent_removal: bool,
    connection_check: Option<String>,
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
    /// 后台探测的本机 shell 列表（shell 下拉与 WSL 勾选用）；含 wsl.exe
    /// 子进程调用，异步填充。
    discovered_shells: Vec<DiscoveredShell>,
    /// 内容区滚动容器句柄 + 拖拽滚动条状态（配置项多时右侧出现滚动条）。
    scroll: gpui::ScrollHandle,
    dragging_scroll: bool,
    agent_usage: Option<DiskUsage>,
    agent_usage_error: Option<String>,
    cleanup_running: bool,
    cleanup_cancel: Option<Arc<AtomicBool>>,
    lifecycle_hooks: termior_hooks::HookCatalog,
    hook_runs: Vec<termior_hooks::HookRunRecord>,
    hook_load_error: Option<String>,
    mcp_servers: termior_agent_host::McpServerCatalog,
    mcp_load_error: Option<String>,
    _updater_subscription: gpui::Subscription,
}

// 拆分结构：本文件保留核心类型与 `SettingsView` 字段；
// 各设置页以及编辑、保存、共用控件放在 `settings_view/` 子模块。
mod about;
mod agents;
mod edit;
mod general;
mod helpers;
mod hooks;
mod mcp;
mod models;
mod persist;
mod shortcuts;
mod themes;
mod widgets;

#[cfg(test)]
mod edit_tests;
#[cfg(test)]
mod hooks_tests;
#[cfg(test)]
mod mcp_tests;

use edit::SettingsInputHandler;
use helpers::{bytes_to_mib, char_to_byte, on_off};

impl SettingsView {
    pub fn new(
        settings: Settings,
        migration_error: Option<String>,
        data_dir: Option<PathBuf>,
        on_save: Option<SaveCallback>,
        on_theme_preview: Option<ThemePreviewCallback>,
        cx: &mut Context<Self>,
    ) -> Self {
        crate::updater::init(cx);
        let themes = data_dir
            .as_ref()
            .and_then(|dir| DataFiles::new(dir).themes().load().ok())
            .unwrap_or_default();
        let agents = data_dir
            .as_ref()
            .and_then(|dir| DataFiles::new(dir).agents().load().ok())
            .unwrap_or_default();
        let mut view = Self {
            _updater_subscription: cx.observe(&crate::updater::entity(cx), |_, _, cx| cx.notify()),
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
            // IME 候选窗锚点（编辑中文本字段的探针每帧刷新）。
            ime_anchor: crate::ime_anchor::ImeAnchor::new(),
            credential_present: false,
            pending_api_key: None,
            credential_drafts: Default::default(),
            agent_appearance_open: false,
            confirm_agent_removal: false,
            connection_check: None,
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
            discovered_shells: Vec::new(),
            scroll: gpui::ScrollHandle::new(),
            dragging_scroll: false,
            agent_usage: None,
            agent_usage_error: None,
            cleanup_running: false,
            cleanup_cancel: None,
            lifecycle_hooks: termior_hooks::HookCatalog::default(),
            hook_runs: Vec::new(),
            hook_load_error: None,
            mcp_servers: termior_agent_host::McpServerCatalog::default(),
            mcp_load_error: None,
        };
        view.reload_mcp_servers();
        view.refresh_credential_state();
        // shell 探测两段式：原生条目（PATH 检查，即时）先到，WSL 枚举
        // （wsl.exe 子进程，带超时）完成后再追加（FR-TERM-05）。
        let discover_task = cx.spawn(async move |view, cx| {
            let native = cx
                .background_executor()
                .spawn(async move { termior_terminal::discover_native_shells() })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.discovered_shells = native;
                cx.notify();
            });
            let wsl = cx
                .background_executor()
                .spawn(async move { termior_terminal::discover_wsl_shells() })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.discovered_shells.extend(wsl);
                cx.notify();
            });
        });
        view.track_task(discover_task);
        view
    }

    fn set_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        self.page = page;
        self.select_menu = None;
        self.confirm_agent_removal = false;
        if page == SettingsPage::Agents {
            self.agent_usage = None;
            self.agent_usage_error = None;
            self.refresh_agent_usage(cx);
        }
        if page == SettingsPage::Hooks {
            self.reload_lifecycle_hooks();
        }
        if page == SettingsPage::Mcp {
            self.reload_mcp_servers();
        }
        cx.notify();
    }

    pub(crate) fn show_about(&mut self, cx: &mut Context<Self>) {
        self.set_page(SettingsPage::About, cx);
    }

    fn page_label(page: SettingsPage) -> SharedString {
        match page {
            SettingsPage::General => t!("settings.page.general"),
            SettingsPage::Models => t!("settings.page.models"),
            SettingsPage::Themes => t!("settings.page.themes"),
            SettingsPage::Shortcuts => t!("settings.page.shortcuts"),
            SettingsPage::Agents => t!("settings.page.agents"),
            SettingsPage::Hooks => t!("settings.page.hooks"),
            SettingsPage::Mcp => t!("settings.page.mcp"),
            SettingsPage::About => t!("settings.page.about"),
        }
    }

    fn page_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            SettingsPage::General => self.general_page(cx),
            SettingsPage::Models => self.models_page(cx),
            SettingsPage::Themes => self.themes_page(cx),
            SettingsPage::Shortcuts => self.shortcuts_page(cx),
            SettingsPage::Agents => self.agents_page(cx),
            SettingsPage::Hooks => self.hooks_page(cx),
            SettingsPage::Mcp => self.mcp_page(cx),
            SettingsPage::About => self.about_page(cx),
        }
    }
}

impl Drop for SettingsView {
    fn drop(&mut self) {
        if let Some(flag) = &self.cleanup_cancel {
            flag.store(true, Ordering::Release);
        }
        self.commit_edit();
        if let Err(error) = self.persist_to_disk() {
            log::warn!("failed to persist settings on close: {error}");
        }
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
            SettingsPage::Hooks,
            SettingsPage::Mcp,
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
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.dragging_scroll && e.pressed_button == Some(MouseButton::Left) {
                    termior_ui_kit::scroll_to_pointer(&this.scroll, e.position.y);
                    cx.notify();
                } else {
                    this.dragging_scroll = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.dragging_scroll = false),
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
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("settings-scroll")
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
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
                            .child(
                                termior_ui_kit::scrollbar("settings-scrollbar", &self.scroll, &p)
                                    .debug_selector(|| "settings-scrollbar".into())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, e: &MouseDownEvent, _, cx| {
                                            this.dragging_scroll = true;
                                            termior_ui_kit::scroll_to_pointer(
                                                &this.scroll,
                                                e.position.y,
                                            );
                                            cx.notify();
                                        }),
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
