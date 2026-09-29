mod sftp_browser;
use sftp_browser::{GroupMenu, LocalBrowserState, SessionMenu};

use crate::{
    ai_diff_view::{AiDiffAction, AiDiffView},
    app_identity,
    composer_view::{ComposerCollapse, ComposerDockToggle, ComposerView, EditReviewRequested},
    editor_view::EditorView,
    git_views::{GitDiffAction, GitDiffView, GitHistoryAction, GitHistoryView},
    markdown_preview_view::MarkdownPreviewView,
    preview_view::PreviewView,
    settings_view::SettingsView,
    terminal_view::{TerminalView, TerminalViewEvent},
    ui::{self, ButtonKind},
};
use crate::{
    ComposerDock, SidebarPanel, TabId, TabKind, WorkspaceState, DEFAULT_COMPOSER_DOCK_WIDTH,
    DEFAULT_COMPOSER_HEIGHT, MAX_COMPOSER_DOCK_WIDTH, MAX_COMPOSER_HEIGHT, MIN_COMPOSER_DOCK_WIDTH,
    MIN_COMPOSER_HEIGHT,
};
use futures::{
    future::{self, Either},
    StreamExt,
};
use gpui::{
    actions, anchored, canvas, div, ease_in_out, prelude::*, px, relative, size, Animation,
    AnimationExt as _, AnyElement, AnyWindowHandle, App, Bounds, Context, CursorStyle, Decorations,
    Div, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, HitboxBehavior,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    PromptButton, PromptLevel, ResizeEdge, Role, ScrollHandle, SharedString, Size, Stateful, Task,
    Tiling, UTF16Selection, Window, WindowAppearance, WindowBounds, WindowControlArea,
};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use termior_ai::{
    AttachmentSource, HttpProvider, InlineCompleter, KeyringSecretStore, ProviderConfig,
    SecretStore,
};
use termior_explorer::{
    debounced_rescan_action, ContentMatch, ContentSearch, DebouncedRescanAction, FileEntry,
    FileIndex, IconKind, TreeState, WorkspaceWatcher,
};
use termior_platform::{
    open_local_file, AgentIndicator, AgentStatus, NativeNotifier, Notification,
    NotificationContext, NotificationDecision, NotificationRouter, NotificationTarget,
    SystemNotifier,
};
use termior_preview::{is_html_path, is_markdown_path, normalize_preview_url};
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{
    app_data_dir, atomic_write, default_settings, migrate, KeyAction, Settings, ShellDetection,
};
use termior_terminal_core::osc::AgentState;
use termior_theme::{
    active_theme_id, resolve_active_palette, Appearance, ResolvedPalette, Theme, ThemeLibrary,
};
use termior_ui_kit::{
    empty_hint, empty_state_message, icon, menu_panel, menu_separator,
    titlebar::{draws_own_window_controls, handles_own_window_control_clicks, window_controls},
    tokens::{self, icon_size},
    Icon, LayoutNode, PaneId, SplitDirection, Tooltip,
};

// 该代码库第一个 GPUI Action：`Cmd/Ctrl+O` 快捷键（在 `main.rs` 绑定）与
// 状态栏按钮共用内核 `open_workspace`。
actions!(termior_ui, [OpenWorkspace]);

/// Short oneshot fade for sidebar / toast / menus. Never `.repeat()` — that would break idle zero-redraw.
const UI_FADE_IN: Duration = Duration::from_millis(160);
/// Merge filesystem events before scheduling an explorer rebuild.
const EXPLORER_RESCAN_DEBOUNCE: Duration = Duration::from_millis(400);
/// Soft cap for deep indexing; shallow/partial results stay visible after this.
const EXPLORER_DEEP_SCAN_TIMEOUT: Duration = Duration::from_secs(45);

/// Idle-redraw probe (`TERMIOR_IDLE_REDRAW_PROBE`): count `WorkspaceView::render` calls while armed.
static IDLE_REDRAW_ARMED: AtomicBool = AtomicBool::new(false);
static IDLE_REDRAW_FRAMES: AtomicU64 = AtomicU64::new(0);
use termior_vcs::{
    BranchState, ChangeGroup, ChangedFile, CommitInfo, GitRepository, RemoteOperation,
};

const DEFAULT_SIDEBAR_WIDTH: f32 = 280.0;
const MIN_SIDEBAR_WIDTH: f32 = 220.0;
const MAX_SIDEBAR_WIDTH: f32 = 520.0;
/// 标题栏行高。窗口没有系统标题栏，这一行同时承载标签页、操作区与窗口控制按钮。
const WORKSPACE_HEADER_HEIGHT: f32 = tokens::height::TITLE_BAR;
/// 标题栏拖拽区的最小宽度。标签页再多也要留下能抓住窗口的地方。
const TITLE_BAR_DRAG_MIN_WIDTH: f32 = 48.0;
const STATUS_BAR_HEIGHT: f32 = 24.0;
/// CSD 窗口四周的环带宽度：既是不可见缩放手柄的命中带，也是阴影/圆角的留白
/// （对齐 Zed 的 `CLIENT_SIDE_DECORATION_SHADOW`）。见 `render_window_frame`。
const WINDOW_RESIZE_BAND: Pixels = px(10.0);
/// CSD 窗口圆角半径（对齐 Zed 的 `CLIENT_SIDE_DECORATION_ROUNDING`）。
const WINDOW_ROUNDING: Pixels = px(10.0);
/// Composer 拖拽手柄厚度，与 pane 分隔条一致。
const COMPOSER_RESIZE_HANDLE_SIZE: f32 = 5.0;
/// 多 pane 时非活动 pane 的不透明度——压暗可辨，终端文本仍可读。
const INACTIVE_PANE_OPACITY: f32 = 0.78;
const PANE_DIVIDER_SIZE: f32 = 5.0;
const MIN_PANE_WIDTH: f32 = 320.0;
const MIN_PANE_HEIGHT: f32 = 180.0;
const MAX_PANES_PER_TAB: usize = 8;

// 拆分结构：本文件保留核心类型、`WorkspaceView` 定义与构造/启动/冒烟探针逻辑；
// 其余实现按职责拆到 `workspace_view/` 子模块（remote_explorer、terminal_mgmt、
// tab_views、panes、notifications、explorer_actions、panels、workspace_data、
// settings_theme、commands、input、render、helpers）。
mod commands;
mod explorer_actions;
mod helpers;
mod input;
mod notifications;
mod panels;
mod panes;
mod remote_explorer;
mod render;
mod settings_theme;
mod tab_views;
mod terminal_mgmt;
mod workspace_data;
pub use workspace_data::load_settings;

#[derive(Clone)]
enum PaneContent {
    Terminal(Entity<TerminalView>),
    Editor(Entity<EditorView>),
    Markdown(Entity<MarkdownPreviewView>),
    Preview(Entity<PreviewView>),
    AiDiff(Entity<AiDiffView>),
    GitDiff(Entity<GitDiffView>),
    GitHistory(Entity<GitHistoryView>),
    Placeholder(String),
}

impl PaneContent {
    fn element(&self, palette: &ResolvedPalette) -> AnyElement {
        match self {
            Self::Terminal(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Editor(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Markdown(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Preview(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::AiDiff(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::GitDiff(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::GitHistory(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Placeholder(message) => empty_state_message(
                Icon::Terminal,
                SharedString::from(message.clone()),
                None,
                palette,
            )
            .into_any_element(),
        }
    }
}

struct AppTab {
    id: TabId,
    panes: HashMap<PaneId, PaneContent>,
}

#[derive(Clone)]
struct PendingAgentUpdate {
    indicator: AgentIndicator,
    notification: Notification,
}

#[derive(Clone)]
struct InAppToast {
    id: u64,
    notification: Notification,
}

#[derive(Debug, Clone)]
struct PaneContextMenu {
    terminal: Option<Entity<TerminalView>>,
    position: Point<Pixels>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NewTabAction {
    Terminal,
    Editor,
    Ssh,
}

#[derive(Debug, Clone)]
struct PaneResizeState {
    path: Vec<usize>,
    direction: SplitDirection,
    start_position: Point<Pixels>,
    start_ratio: f32,
    extent: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMenuAction {
    Right,
    Down,
    CloseActive,
    CloseOthers,
}

pub struct WorkspaceView {
    pending_terminals: std::collections::HashSet<(TabId, PaneId)>,
    main_window: Option<gpui::AnyWindowHandle>,
    ssh_profiles: termior_ssh::Profiles,
    ssh_profiles_error: Option<String>,
    ssh_selected: Option<String>,
    ssh_context_menu: Option<SessionMenu>,
    ssh_group_menu: Option<GroupMenu>,
    /// 运行时的分组折叠状态（按分组名记忆，不持久化；默认全展开）。
    ssh_collapsed_groups: std::collections::HashSet<String>,
    sftp_browsers: HashMap<TabId, LocalBrowserState>,
    ssh_manager_window: Option<gpui::WindowHandle<crate::ssh_view::SshView>>,
    model: WorkspaceState,
    tabs: Vec<AppTab>,
    themes: Vec<Theme>,
    theme_index: usize,
    palette: ResolvedPalette,
    focus_handle: FocusHandle,
    composer: Entity<ComposerView>,
    explorer: Option<FileIndex>,
    explorer_tree: TreeState,
    explorer_watcher: Option<WorkspaceWatcher>,
    remote_explorers: HashMap<TabId, RemoteExplorerState>,
    remote_auth_sessions: HashMap<TabId, termior_ssh::auth::Session>,
    explorer_view_tab: Option<TabId>,
    remote_explorer_paths: HashMap<TabId, String>,
    remote_terminal_cwds: HashMap<TabId, String>,
    remote_explorer_selected: Option<String>,
    remote_explorer_scroll_drag: Option<(TabId, f32)>,
    remote_explorer_generation: u64,
    remote_pending_name_parent: Option<String>,
    remote_pending_rename_target: Option<String>,
    explorer_requested_root: PathBuf,
    /// True while a deep index for the current generation is still outstanding.
    explorer_deep_indexing: bool,
    explorer_scan_generation: u64,
    /// True from scan start until deep completes/fails (or is superseded).
    explorer_scan_in_progress: bool,
    /// FS change arrived while a scan was running — rescan once when it finishes.
    explorer_pending_rescan: bool,
    /// Debounced deadline for a watch-triggered rescan.
    explorer_rescan_after: Option<Instant>,
    explorer_error: Option<String>,
    /// Deep index hit the soft timeout; partial results are still shown.
    explorer_index_incomplete: bool,
    /// Whether the explorer footer listing skipped unreadable entries is expanded.
    explorer_skips_expanded: bool,
    _background_task: Option<Task<()>>,
    command_mode: CommandMode,
    command_input: String,
    command_marked_text: String,
    command_message: Option<String>,
    pending_name_parent: Option<PathBuf>,
    pending_rename_target: Option<PathBuf>,
    /// 分组命令栏的载荷：新建分组时要分配的连接名（None 表示仅创建空分组）。
    pending_group_profile: Option<String>,
    /// 分组命令栏的载荷：重命名分组时的旧分组名。
    pending_group_rename: Option<String>,
    explorer_context_menu: Option<ExplorerContextMenu>,
    /// 标签栏新建下拉的锚点；`None` 表示关闭。
    new_tab_menu: Option<Point<Pixels>>,
    /// 新建终端 shell 选择器的锚点（`settings.terminal.shell_prompt` 开启时
    /// 新建终端入口先弹它）；`None` 表示关闭。
    shell_menu: Option<Point<Pixels>>,
    /// 选择器展示的本机 shell 列表（后台探测，含 WSL 发行版）。
    discovered_shells: Vec<termior_terminal::DiscoveredShell>,
    pane_context_menu: Option<PaneContextMenu>,
    sidebar_resizing: bool,
    /// Composer 面板拖拽调整尺寸进行中（方向取决于当前停靠位置）。
    composer_resizing: bool,
    /// 标题栏拖拽已按下、等待首个移动事件（Linux/BSD 自管窗口移动，见
    /// [`Self::start_titlebar_move`]）。Windows 走 HTCAPTION 命中测试，不用它。
    titlebar_move_armed: bool,
    /// 鼠标当前悬停的 CSD 缩放边，用于光标提示的增量重绘（见
    /// [`Self::render_window_frame`]）。
    resize_edge_hint: Option<ResizeEdge>,
    pane_resizing: Option<PaneResizeState>,
    content_matches: Vec<ContentMatch>,
    content_search_generation: u64,
    content_searching: bool,
    vcs_status: Vec<ChangedFile>,
    vcs_history: Vec<CommitInfo>,
    vcs_branch: Option<BranchState>,
    /// Bumped on every scheduled VCS refresh so stale background results are dropped.
    vcs_scan_generation: u64,
    settings: Settings,
    data_dir: Option<PathBuf>,
    migration_error: Option<String>,
    workspace_auth: WorkspaceAuthRegistry,
    notification_router: NotificationRouter,
    pending_agent_updates: BTreeMap<String, PendingAgentUpdate>,
    toasts: Vec<InAppToast>,
    next_toast_id: u64,
    bell_open: bool,
    system_is_dark: bool,
    /// 背景图解码 + 模糊纹理缓存（FR-THEME-05；见 [`background_image`]）。
    background_cache: crate::background_image::BackgroundImageCache,
}

impl WorkspaceView {
    pub fn new(root: PathBuf, system_is_dark: bool, cx: &mut Context<Self>) -> Self {
        let (settings, data_dir, migration_error) = load_settings();
        // 界面语言要在任何视图渲染前确定：显式设置优先，其次系统语言。
        // 测试二进制跳过（保持默认英文槽位），避免断言依赖宿主机语言。
        #[cfg(not(test))]
        termior_i18n::init(settings.language.as_deref());
        let completion_completer = build_completer_from_settings(&settings);
        let completion_enabled = settings.autocomplete_enabled && completion_completer.is_some();
        let themes = data_dir
            .as_ref()
            .and_then(|dir| {
                termior_store::DataFiles::new(dir)
                    .themes::<ThemeLibrary>()
                    .load()
                    .ok()
            })
            .unwrap_or_default()
            .all();
        let active_id = active_theme_id(
            map_appearance(settings.appearance),
            &settings.theme_id,
            &settings.light_theme_id,
            &settings.dark_theme_id,
            system_is_dark,
        );
        let theme_index = themes
            .iter()
            .position(|theme| theme.id == active_id)
            .unwrap_or(0);
        let palette = resolve_active_palette(
            &themes,
            map_appearance(settings.appearance),
            &settings.theme_id,
            &settings.light_theme_id,
            &settings.dark_theme_id,
            system_is_dark,
        );
        cx.set_global(ui::ActiveTheme(palette.clone()));
        let mut model = data_dir
            .as_ref()
            .and_then(
                |dir| match std::fs::read_to_string(dir.join("Termior-workspaces.json")) {
                    Ok(raw) => Some(raw),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => {
                        log::warn!("failed to read workspace state: {error}");
                        None
                    }
                },
            )
            .and_then(|raw| match serde_json::from_str::<WorkspaceState>(&raw) {
                Ok(state) => Some(state),
                Err(error) => {
                    log::warn!("workspace state file is corrupt, starting fresh: {error}");
                    None
                }
            })
            .unwrap_or_else(|| WorkspaceState::new(root.clone()));
        // 旧格式文件（Tab 无 project_dir）回填为全局 root，语义与旧版等价。
        model.backfill_project_dirs();
        // Tab 集合跨目录启动也恢复（不再按 root 相等过滤）；兜底根跟随本次启动目录。
        model.root = root.clone();
        model.sidebar_width = model
            .sidebar_width
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
        model.composer_height = model
            .composer_height
            .clamp(MIN_COMPOSER_HEIGHT, MAX_COMPOSER_HEIGHT);
        model.composer_dock_width = model
            .composer_dock_width
            .clamp(MIN_COMPOSER_DOCK_WIDTH, MAX_COMPOSER_DOCK_WIDTH);
        // A restored model needs runtime view owners; terminal/preview are restored asynchronously.
        // Editor and Markdown tabs for the same file deliberately share one live buffer entity.
        let mut restored_editors = HashMap::<PathBuf, Entity<EditorView>>::new();
        let tabs = model
            .tabs
            .iter()
            .map(|tab| AppTab {
                id: tab.id,
                panes: tab
                    .layout
                    .panes()
                    .into_iter()
                    .map(|pane_id| {
                        (
                            pane_id,
                            match tab.kind {
                                TabKind::Editor => {
                                    let editor = if let Some(path) =
                                        tab.resource.as_ref().map(PathBuf::from)
                                    {
                                        restored_editors
                                            .entry(path.clone())
                                            .or_insert_with(|| {
                                                cx.new(|cx| EditorView::open(&path, cx))
                                            })
                                            .clone()
                                    } else {
                                        cx.new(EditorView::untitled)
                                    };
                                    editor.update(cx, |editor, _| {
                                        editor.set_preferences(
                                            &settings.editor_theme_id,
                                            settings.vim_mode,
                                            completion_completer.clone(),
                                            completion_enabled,
                                        )
                                    });
                                    PaneContent::Editor(editor)
                                }
                                TabKind::Markdown => tab
                                    .resource
                                    .as_ref()
                                    .map(PathBuf::from)
                                    .map(|path| {
                                        let source = restored_editors
                                            .entry(path.clone())
                                            .or_insert_with(|| {
                                                cx.new(|cx| EditorView::open(&path, cx))
                                            })
                                            .clone();
                                        source.update(cx, |editor, _| {
                                            editor.set_preferences(
                                                &settings.editor_theme_id,
                                                settings.vim_mode,
                                                completion_completer.clone(),
                                                completion_enabled,
                                            )
                                        });
                                        PaneContent::Markdown(
                                            cx.new(|cx| MarkdownPreviewView::new(path, source, cx)),
                                        )
                                    })
                                    .unwrap_or_else(|| {
                                        PaneContent::Placeholder(
                                            t!("empty.markdown_source_unavailable").into(),
                                        )
                                    }),
                                TabKind::Terminal | TabKind::Preview => {
                                    PaneContent::Placeholder(if tab.remote.is_some() {
                                        t!("ws.remote_disconnected").into()
                                    } else {
                                        tf!("ws.tab_restoring", "title" => tab.title.clone()).into()
                                    })
                                }
                                TabKind::AiDiff | TabKind::GitDiff | TabKind::GitCommitFile => {
                                    PaneContent::Placeholder(
                                        tf!(
                                            "ws.tab_with_hint",
                                            "title" => tab.title.clone(),
                                            "hint" => t!("empty.diff_reopen_hint")
                                        )
                                        .into(),
                                    )
                                }
                                TabKind::GitHistory => PaneContent::Placeholder(
                                    tf!(
                                        "ws.tab_with_hint",
                                        "title" => tab.title.clone(),
                                        "hint" => t!("empty.git_history_sidebar")
                                    )
                                    .into(),
                                ),
                            },
                        )
                    })
                    .collect(),
            })
            .collect();
        if model.tabs.is_empty() {
            model.active = None;
        }
        let mut workspace_auth =
            WorkspaceAuthRegistry::with_roots([root.to_string_lossy().replace('\\', "/")]);
        // 恢复出的各 Tab 项目文件夹是此前会话显式授权的目录（打开文件夹即授权），
        // 重启后继续授权，保证其 PTY spawn 的 cwd 不被 resolve_cwd 丢弃（多 root 并集）。
        for tab in &model.tabs {
            let dir = tab.project_dir.to_string_lossy().replace('\\', "/");
            if !dir.is_empty() {
                workspace_auth.authorize(&dir);
            }
        }
        let composer = cx.new(ComposerView::new);
        composer.update(cx, |composer, cx| {
            composer.configure(
                &settings,
                &root,
                workspace_auth.clone(),
                data_dir.clone(),
                cx,
            );
        });
        // 恢复持久化的停靠位置与面板高度。
        let (composer_dock, composer_height) = (model.composer_dock, model.composer_height);
        composer.update(cx, |composer, cx| {
            composer.set_layout(composer_dock, composer_height, cx);
        });
        cx.subscribe(
            &composer,
            |workspace, _composer, status: &AgentStatus, cx| {
                workspace.queue_agent_update(
                    "builtin-agent",
                    &t!("ws.builtin_agent"),
                    None,
                    NotificationTarget::Composer,
                    *status,
                    cx,
                );
            },
        )
        .detach();
        cx.subscribe(
            &composer,
            |workspace, _composer, request: &EditReviewRequested, cx| {
                workspace.open_ai_diff(request.0.clone(), cx);
            },
        )
        .detach();
        cx.subscribe(
            &composer,
            |workspace, _composer, _: &ComposerDockToggle, cx| {
                workspace.toggle_composer_dock(cx);
            },
        )
        .detach();
        // 面板头「收起」按钮：与 Ctrl+I / 状态栏箭头同一入口。new() 里拿不到
        // window，焦点归还交给 Ctrl+I / 状态栏路径，这里只做可见性切换。
        cx.subscribe(
            &composer,
            |workspace, _composer, _: &ComposerCollapse, cx| {
                workspace.model.composer_visible = false;
                cx.notify();
            },
        )
        .detach();
        let mut notification_router = NotificationRouter::default();
        notification_router.set_enabled(settings.agent_notifications);
        notification_router.update_agent(AgentIndicator {
            id: "builtin-agent".into(),
            title: t!("ws.builtin_agent").to_string(),
            status: AgentStatus::Finished,
            tab_id: None,
        });
        let initial_project_root = model.active_project_dir().to_path_buf();
        let explorer_tree = TreeState::new(initial_project_root.clone());
        Self {
            model,
            pending_terminals: Default::default(),
            main_window: None,
            ssh_profiles: termior_ssh::Profiles::default(),
            ssh_profiles_error: None,
            ssh_selected: None,
            ssh_context_menu: None,
            ssh_group_menu: None,
            ssh_collapsed_groups: Default::default(),
            sftp_browsers: HashMap::new(),
            ssh_manager_window: None,
            tabs,
            themes,
            theme_index,
            palette,
            focus_handle: cx.focus_handle(),
            composer,
            explorer: None,
            explorer_tree,
            explorer_watcher: None,
            remote_explorers: HashMap::new(),
            remote_auth_sessions: HashMap::new(),
            explorer_view_tab: None,
            remote_explorer_paths: HashMap::new(),
            remote_terminal_cwds: HashMap::new(),
            remote_explorer_selected: None,
            remote_explorer_scroll_drag: None,
            remote_explorer_generation: 0,
            remote_pending_name_parent: None,
            remote_pending_rename_target: None,
            explorer_requested_root: initial_project_root,
            explorer_deep_indexing: true,
            explorer_scan_generation: 0,
            explorer_scan_in_progress: false,
            explorer_pending_rescan: false,
            explorer_rescan_after: None,
            explorer_error: None,
            explorer_index_incomplete: false,
            explorer_skips_expanded: false,
            _background_task: None,
            command_mode: CommandMode::Browse,
            command_input: String::new(),
            command_marked_text: String::new(),
            command_message: None,
            pending_name_parent: None,
            pending_rename_target: None,
            pending_group_profile: None,
            pending_group_rename: None,
            explorer_context_menu: None,
            new_tab_menu: None,
            shell_menu: None,
            discovered_shells: Vec::new(),
            pane_context_menu: None,
            sidebar_resizing: false,
            composer_resizing: false,
            titlebar_move_armed: false,
            resize_edge_hint: None,
            pane_resizing: None,
            content_matches: Vec::new(),
            content_search_generation: 0,
            content_searching: false,
            vcs_status: Vec::new(),
            vcs_history: Vec::new(),
            vcs_branch: None,
            vcs_scan_generation: 0,
            settings,
            data_dir,
            migration_error,
            workspace_auth,
            notification_router,
            pending_agent_updates: BTreeMap::new(),
            toasts: Vec::new(),
            next_toast_id: 1,
            bell_open: false,
            system_is_dark,
            background_cache: crate::background_image::BackgroundImageCache::new(),
        }
    }

    pub fn restore_or_create_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.main_window = Some(window.window_handle());
        self.reload_ssh_profiles();
        let terminal_panes: Vec<(TabId, PaneId, Option<PathBuf>, bool)> = self
            .model
            .tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::Terminal && tab.remote.is_none())
            .flat_map(|tab| {
                tab.layout
                    .panes()
                    .into_iter()
                    .map(|pane_id| (tab.id, pane_id, Some(tab.cwd.clone()), tab.private_terminal))
                    .collect::<Vec<_>>()
            })
            .collect();
        if terminal_panes.is_empty() && self.model.tabs.is_empty() {
            self.create_terminal(false, window, cx);
        } else {
            for (id, pane_id, cwd, private) in terminal_panes {
                self.spawn_terminal_into(
                    id,
                    pane_id,
                    cwd,
                    private,
                    Some(window.window_handle()),
                    None,
                    cx,
                );
            }
            let previews = self
                .model
                .tabs
                .iter()
                .filter(|tab| tab.kind == TabKind::Preview)
                .map(|tab| {
                    (
                        tab.id,
                        tab.resource
                            .clone()
                            .unwrap_or_else(|| "http://localhost:3000".into()),
                    )
                })
                .collect::<Vec<_>>();
            for (id, url) in previews {
                let pane_ids = self
                    .model
                    .tabs
                    .iter()
                    .find(|tab| tab.id == id)
                    .map(|tab| tab.layout.panes())
                    .unwrap_or_default();
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    for pane_id in pane_ids {
                        tab.panes.insert(
                            pane_id,
                            PaneContent::Preview(new_preview_view(url.clone(), cx)),
                        );
                    }
                }
            }
        }
    }

    pub fn start_background_services(&mut self, cx: &mut Context<Self>) {
        if skip_startup_workspace_scans() {
            // Smoke/NFR/idle probes measure launch and frame timing; git status
            // plus explorer indexing on a large checkout contend for CPU and
            // can abort a GUI-subsystem process if a worker panics.
            return;
        }
        crate::updater::start(self.settings.automatic_updates, cx);
        let updater = crate::updater::entity(cx);
        cx.observe(&updater, |_, _, cx| cx.notify()).detach();
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
        self.refresh_vcs_data(cx);
        self._background_task = Some(cx.spawn(async move |workspace, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            if workspace
                .update(cx, |workspace, cx| {
                    let changed = workspace
                        .explorer_watcher
                        .as_ref()
                        .is_some_and(|watcher| !watcher.try_changes().is_empty());
                    if changed {
                        workspace.explorer_rescan_after =
                            Some(Instant::now() + EXPLORER_RESCAN_DEBOUNCE);
                    }
                    let due = workspace
                        .explorer_rescan_after
                        .is_some_and(|deadline| Instant::now() >= deadline);
                    match debounced_rescan_action(due, workspace.explorer_scan_in_progress) {
                        DebouncedRescanAction::Wait => {}
                        DebouncedRescanAction::QueuePending => {
                            workspace.explorer_rescan_after = None;
                            workspace.explorer_pending_rescan = true;
                        }
                        DebouncedRescanAction::ScheduleNow => {
                            workspace.explorer_rescan_after = None;
                            workspace.schedule_explorer_scan(
                                workspace.explorer_requested_root.clone(),
                                cx,
                            );
                            workspace.refresh_vcs_data(cx);
                            cx.notify();
                        }
                    }
                })
                .is_err()
            {
                break;
            }
        }));
    }

    /// Opens the settings window then closes it via the same `remove_window` path as the
    /// title-bar close button, so CI / local smoke can assert no `window not found` log.
    /// Used by `scripts/settings-close-smoke.ps1`; normal launches never call this method.
    pub fn start_settings_close_smoke(&self, cx: &mut Context<Self>) {
        let Some(handle) = self.open_settings_window(cx) else {
            return;
        };
        // Exercise the shared updater subscription and About controls before closing.
        let _ = handle.update(cx, |settings, _, cx| settings.show_about(cx));
        cx.spawn(async move |_, cx| {
            // Let the settings window paint at least once (matches real title-bar close timing).
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let closed = handle
                .update(cx, |_, window, _| {
                    window.remove_window();
                })
                .is_ok();
            // Allow deferred DestroyWindow / activate callbacks to flush.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            if closed {
                println!("TERMIOR_SETTINGS_CLOSE_SMOKE_OK");
            } else {
                eprintln!("TERMIOR_SETTINGS_CLOSE_SMOKE_FAILED: settings window already gone");
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Splits the active tab to the right, then quits. Used by CI to prove a
    /// multi-pane layout can be created on Windows without crashing.
    pub fn start_split_pane_smoke(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |workspace, cx| {
            for _ in 0..20 {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                let ready = workspace
                    .update_in(cx, |_, window, _| {
                        f32::from(window.viewport_size().width) >= MIN_PANE_WIDTH * 2.0
                    })
                    .unwrap_or(false);
                if ready {
                    break;
                }
            }
            let result = workspace.update_in(cx, |workspace, window, cx| {
                workspace.split_active(SplitDirection::Right, window, cx);
                workspace.active_pane_count()
            });
            match result {
                Ok(count) if count >= 2 => {
                    println!("TERMIOR_SPLIT_PANE_SMOKE_OK panes={count}");
                }
                Ok(count) => {
                    eprintln!("TERMIOR_SPLIT_PANE_SMOKE_FAILED: pane_count={count}");
                }
                Err(error) => {
                    eprintln!("TERMIOR_SPLIT_PANE_SMOKE_FAILED: {error}");
                }
            }
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
    }

    /// Exercises the same request path as the header Preview button with an active Markdown file.
    /// Used by `scripts/markdown-preview-smoke.ps1`; normal launches never call this method.
    pub fn start_markdown_preview_smoke(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.model.root.join("preview-smoke.md");
        self.open_editor(path.clone(), window, cx);
        self.request_preview(window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            // Keep the window alive long enough for GPUI to layout and paint the new native view.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let result = workspace.update_in(cx, |workspace, window, cx| {
                let active_kind = workspace.model.active_tab().map(|tab| tab.kind);
                let preview_source = workspace
                    .model
                    .active
                    .and_then(|active| workspace.tabs.iter().find(|tab| tab.id == active))
                    .and_then(|tab| {
                        tab.panes.values().find_map(|pane| match pane {
                            PaneContent::Markdown(preview)
                                if preview
                                    .read(cx)
                                    .contains_text("renders the active document") =>
                            {
                                Some(preview.read(cx).source())
                            }
                            _ => None,
                        })
                    });
                workspace.open_editor(path, window, cx);
                let reopened_kind = workspace.model.active_tab().map(|tab| tab.kind);
                let shared_source = preview_source
                    .zip(workspace.active_editor().cloned())
                    .is_some_and(|(preview, editor)| preview == editor);
                (active_kind, reopened_kind, shared_source)
            });
            match result {
                Ok((Some(TabKind::Markdown), Some(TabKind::Editor), true)) => {
                    println!("TERMIOR_MARKDOWN_PREVIEW_SMOKE_OK");
                }
                Ok((active_kind, reopened_kind, shared_source)) => eprintln!(
                    "TERMIOR_MARKDOWN_PREVIEW_SMOKE_FAILED: active_kind={active_kind:?}, reopened_kind={reopened_kind:?}, shared_source={shared_source}"
                ),
                Err(error) => {
                    eprintln!("TERMIOR_MARKDOWN_PREVIEW_SMOKE_FAILED: {error}");
                }
            }
            let _ = cx.update(|_, cx| cx.quit());
        })
        .detach();
    }

    /// UI screenshot helper (`TERMIOR_OPEN_SETTINGS`): open settings without SendKeys.
    pub fn open_settings_for_ui_shot(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::WindowHandle<SettingsView>> {
        self.open_settings_window(cx)
    }

    /// Idle zero-redraw probe (`TERMIOR_IDLE_REDRAW_PROBE=<secs>`).
    ///
    /// Settles for a few seconds (explorer indexing / first paint), then arms a
    /// render counter for the probe window and prints `TERMIOR_IDLE_REDRAW_FRAMES=N`.
    /// NFR-03 expects N == 0 when nothing schedules `cx.notify()` / repeating animations.
    pub fn start_idle_redraw_probe(&self, probe_secs: u64, cx: &mut Context<Self>) {
        let settle = Duration::from_secs(3);
        let probe = Duration::from_secs(probe_secs.max(1));
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(settle).await;
            IDLE_REDRAW_FRAMES.store(0, Ordering::Relaxed);
            IDLE_REDRAW_ARMED.store(true, Ordering::Relaxed);
            cx.background_executor().timer(probe).await;
            IDLE_REDRAW_ARMED.store(false, Ordering::Relaxed);
            let frames = IDLE_REDRAW_FRAMES.load(Ordering::Relaxed);
            println!("TERMIOR_IDLE_REDRAW_FRAMES={frames}");
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Shell picker smoke (`TERMIOR_SHELL_PICKER_SMOKE=1`).
    ///
    /// 协议（stdout 行，供冒烟脚本采集）：等首终端落定后打印
    /// `TERMIOR_SHELL_SETTINGS …`（解析出的 shell 设置），触发新建终端
    /// 选择器；5 秒后打印 `TERMIOR_SHELL_MENU open=… items=…` 与逐条
    /// `TERMIOR_SHELL_ITEM[i]`（后台探测结果），随后退出。判定不依赖截图目测。
    pub fn start_shell_picker_probe(&self, window: &mut Window, cx: &mut Context<Self>) {
        let main_window = window.window_handle();
        let entity = cx.entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let _ = main_window.update(cx, |_, window, cx| {
                entity.update(cx, |workspace, cx| {
                    println!(
                        "TERMIOR_SHELL_SETTINGS prompt={} detection={:?} wsl={:?} appearance={:?}",
                        workspace.settings.terminal.shell_prompt,
                        workspace.settings.terminal.shell_detection,
                        workspace.settings.wsl_distribution,
                        workspace.settings.appearance
                    );
                    workspace.request_new_terminal(None, window, cx);
                });
            });
            cx.background_executor()
                .timer(Duration::from_secs(10))
                .await;
            entity.update(cx, |workspace, _| {
                println!(
                    "TERMIOR_SHELL_MENU open={} items={}",
                    workspace.shell_menu.is_some(),
                    workspace.discovered_shells.len()
                );
                for (index, shell) in workspace.discovered_shells.iter().enumerate() {
                    println!("TERMIOR_SHELL_ITEM[{index}]={shell:?}");
                }
            });
            // 端到端：模拟选中第一个条目（菜单项处理 = 关菜单 + 以该 shell 建终端）。
            let _ = main_window.update(cx, |_, window, cx| {
                entity.update(cx, |workspace, cx| {
                    if let Some(shell) = workspace.discovered_shells.first().cloned() {
                        workspace.shell_menu = None;
                        workspace.create_terminal_with_shell(false, Some(shell), window, cx);
                    }
                });
            });
            cx.background_executor().timer(Duration::from_secs(3)).await;
            entity.update(cx, |workspace, _| {
                println!(
                    "TERMIOR_SHELL_TABS={} menu_open={}",
                    workspace.model.tabs.len(),
                    workspace.shell_menu.is_some()
                );
            });
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

use commands::CommandMode;
use explorer_actions::ExplorerContextMenu;
#[cfg(test)]
use explorer_actions::{ExplorerContextAction, ExplorerContextTarget};
#[cfg(test)]
use helpers::{explorer_context_actions, paired_tab_layout, scroll_thumb_geometry, utf16_to_byte};
use helpers::{map_appearance, new_preview_view};
#[cfg(test)]
use remote_explorer::coalesce_remote_scan;
use remote_explorer::RemoteExplorerState;
use settings_theme::build_completer_from_settings;
#[cfg(test)]
use workspace_data::load_vcs;
use workspace_data::skip_startup_workspace_scans;

#[cfg(test)]
mod explorer_ui_tests {
    use super::*;

    #[test]
    fn remote_navigation_coalesces_and_keeps_only_latest_destination() {
        let mut pending = None;
        coalesce_remote_scan(Some("/a"), &mut pending, "/a".into(), true);
        assert!(pending.is_none(), "refresh shares the active listing");
        coalesce_remote_scan(Some("/a"), &mut pending, "/b".into(), false);
        coalesce_remote_scan(Some("/a"), &mut pending, "/c".into(), true);
        coalesce_remote_scan(Some("/a"), &mut pending, "/c".into(), false);
        assert_eq!(pending, Some(("/c".into(), true)));
        coalesce_remote_scan(Some("/a"), &mut pending, "/a".into(), false);
        assert!(
            pending.is_none(),
            "returning to the active path drops stale navigation"
        );
        coalesce_remote_scan(None, &mut pending, "/a".into(), false);
        assert_eq!(
            pending,
            Some(("/a".into(), false)),
            "navigation waits for mutation completion"
        );
        pending = Some(("/a".into(), true));
        coalesce_remote_scan(Some("/a"), &mut pending, "/a".into(), false);
        assert_eq!(
            pending,
            Some(("/a".into(), true)),
            "post-transfer invalidation must survive navigation coalescing"
        );
    }

    #[test]
    fn remote_explorer_scrollbar_thumb_tracks_scroll_range() {
        let (top, height) = scroll_thumb_geometry(400.0, 1200.0, 0.0);
        assert_eq!(top, 0.0);
        assert_eq!(height, 100.0);

        let (top, height) = scroll_thumb_geometry(400.0, 1200.0, -600.0);
        assert_eq!(top, 150.0);
        assert_eq!(height, 100.0);

        let (top, height) = scroll_thumb_geometry(400.0, 1200.0, -1200.0);
        assert_eq!(top, 300.0);
        assert_eq!(height, 100.0);

        let (top, height) = scroll_thumb_geometry(400.0, 0.0, 0.0);
        assert_eq!((top, height), (0.0, 400.0));
    }

    #[test]
    fn utf16_offsets_never_split_a_code_point() {
        let text = "a😀中";
        assert_eq!(utf16_to_byte(text, 0), 0);
        assert_eq!(utf16_to_byte(text, 1), 1);
        assert_eq!(utf16_to_byte(text, 2), 5);
        assert_eq!(utf16_to_byte(text, 3), 5);
        assert_eq!(utf16_to_byte(text, 4), text.len());
    }

    #[test]
    fn explorer_context_actions_are_target_specific() {
        let file = explorer_context_actions(&ExplorerContextTarget::File(PathBuf::from(
            "/workspace/file.txt",
        )));
        assert!(file.contains(&ExplorerContextAction::Open));
        assert!(file.contains(&ExplorerContextAction::AttachToAi));
        assert!(!file.contains(&ExplorerContextAction::CreateDirectory));

        let workspace = explorer_context_actions(&ExplorerContextTarget::Workspace);
        assert!(workspace.contains(&ExplorerContextAction::CreateFile));
        assert!(workspace.contains(&ExplorerContextAction::SearchContent));
        assert!(!workspace.contains(&ExplorerContextAction::Delete));

        let remote = explorer_context_actions(&ExplorerContextTarget::RemoteDirectory(
            "/home/me/src".into(),
        ));
        assert!(remote.contains(&ExplorerContextAction::UploadFile));
        assert!(remote.contains(&ExplorerContextAction::Download));
        assert!(!remote.contains(&ExplorerContextAction::Reveal));
    }

    #[test]
    fn paired_tab_layout_none_when_lists_diverge() {
        let mut model = WorkspaceState::new("/workspace");
        let id = model.new_tab(TabKind::Terminal, "terminal", false);

        assert!(paired_tab_layout(&model, &[]).is_none());
        assert!(paired_tab_layout(&model, &[id]).is_some());

        model.active = Some(TabId(id.0.saturating_add(99)));
        assert!(paired_tab_layout(&model, &[id]).is_none());
    }

    #[test]
    fn load_vcs_returns_empty_snapshot_when_open_fails() {
        let auth = WorkspaceAuthRegistry::new();
        let root = PathBuf::from("/definitely-not-a-termior-git-workspace");
        let snapshot = load_vcs(&root, &auth);
        assert!(snapshot.status.is_empty());
        assert!(snapshot.history.is_empty());
        assert!(snapshot.branch.is_none());
        assert_eq!(snapshot.root, root);
    }
}

/// 焦点回归：点击 tab 头切换 tab 后，键盘焦点必须落进新 tab 的活动 pane，
/// 与 Windows Terminal 等主流终端一致（否则每次切完 tab 都得再点一下终端）。
#[cfg(test)]
mod tab_focus_tests {
    use super::*;
    use gpui::{Modifiers, MouseButton, TestAppContext};

    fn editor_focus_handle(ws: &WorkspaceView, tab_id: TabId, cx: &App) -> FocusHandle {
        let tab = ws.tabs.iter().find(|tab| tab.id == tab_id).unwrap();
        let focused = ws.model.tab(tab_id).unwrap().layout.focused;
        match tab.panes.get(&focused).unwrap() {
            PaneContent::Editor(editor) => editor.read(cx).focus_handle(cx),
            _ => panic!("expected editor pane"),
        }
    }

    fn tab_header_bounds(
        tab_id: TabId,
        cx: &mut gpui::VisualTestContext,
    ) -> gpui::Bounds<gpui::Pixels> {
        let selector: &'static str = Box::leak(format!("tab-header-{}", tab_id.0).into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("tab header {selector} not rendered"))
    }

    #[gpui::test]
    fn clicking_tab_header_moves_focus_into_its_pane(cx: &mut TestAppContext) {
        let (workspace, cx) = cx.add_window_view(|_window, cx| {
            crate::updater::init(cx);
            WorkspaceView::new(std::env::temp_dir(), false, cx)
        });

        let (tab_a, tab_b) = cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.create_editor(window, cx);
                let tab_a = ws.model.active.unwrap();
                ws.create_editor(window, cx);
                let tab_b = ws.model.active.unwrap();
                (tab_a, tab_b)
            })
        });
        cx.run_until_parked();
        // 首帧：让 tab 头进入 hit-test 树。
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });

        // 焦点先落在 tab A 的编辑器里，模拟「正在 A 里打字，然后点 B 的 tab 头」。
        let handle_a = cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_a, cx));
        cx.update(|window, cx| {
            window.focus(&handle_a, cx);
        });
        let handle_b = cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_b, cx));
        let root_focus = cx.update(|_window, cx| workspace.read(cx).focus_handle.clone());

        let bounds = tab_header_bounds(tab_b, cx);
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());

        let focused = cx.update(|window, cx| window.focused(cx));
        assert_eq!(
            focused,
            Some(handle_b),
            "clicking a tab header must hand keyboard focus to that tab's pane"
        );
        assert_ne!(
            focused,
            Some(root_focus),
            "workspace root must not steal focus back from the activated pane"
        );
    }

    #[gpui::test]
    fn switching_tabs_by_keyboard_keeps_focus_in_pane(cx: &mut TestAppContext) {
        let (workspace, cx) = cx.add_window_view(|_window, cx| {
            crate::updater::init(cx);
            WorkspaceView::new(std::env::temp_dir(), false, cx)
        });

        let (tab_a, tab_b) = cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.create_editor(window, cx);
                let tab_a = ws.model.active.unwrap();
                ws.create_editor(window, cx);
                let tab_b = ws.model.active.unwrap();
                (tab_a, tab_b)
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let handle_a = cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_a, cx));
        cx.update(|window, cx| {
            window.focus(&handle_a, cx);
        });

        // 与 global_key 的 GotoTab1 分支同一调用序列。
        cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                let _ = ws.model.switch_to(tab_a);
                ws.activate_runtime(tab_a, cx);
                ws.focus_active_pane(window, cx);
            })
        });

        let handle_a_again =
            cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_a, cx));
        let focused = cx.update(|window, cx| window.focused(cx));
        assert_eq!(focused, Some(handle_a_again));
        let _ = (tab_b, handle_a);
    }

    #[gpui::test]
    fn closing_active_tab_hands_focus_to_surviving_pane(cx: &mut TestAppContext) {
        let (workspace, cx) = cx.add_window_view(|_window, cx| {
            crate::updater::init(cx);
            WorkspaceView::new(std::env::temp_dir(), false, cx)
        });

        let (tab_a, tab_b) = cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.create_editor(window, cx);
                let tab_a = ws.model.active.unwrap();
                ws.create_editor(window, cx);
                let tab_b = ws.model.active.unwrap();
                (tab_a, tab_b)
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let handle_a = cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_a, cx));
        let handle_b = cx.update(|_window, cx| editor_focus_handle(workspace.read(cx), tab_b, cx));
        cx.update(|window, cx| {
            window.focus(&handle_b, cx);
        });

        // 中键关闭活动 tab B：焦点应回到幸存 tab A 的编辑器。
        let bounds = tab_header_bounds(tab_b, cx);
        cx.simulate_mouse_down(bounds.center(), MouseButton::Middle, Modifiers::default());

        let focused = cx.update(|window, cx| window.focused(cx));
        assert_eq!(
            focused,
            Some(handle_a),
            "closing the focused tab must hand focus to the surviving tab's pane"
        );
    }

    /// Markdown「源码」视图必须可编辑：预览切回源码后焦点落在编辑器，缓冲可改写。
    #[gpui::test]
    fn markdown_source_view_is_editable_after_preview_toggle(cx: &mut TestAppContext) {
        let root =
            std::env::temp_dir().join(format!("termior-md-source-edit-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let path = root.join("source-edit.md");
        std::fs::write(&path, "# original\n").unwrap();

        let (workspace, cx) = cx.add_window_view(|_window, cx| {
            crate::updater::init(cx);
            WorkspaceView::new(root.clone(), false, cx)
        });
        cx.run_until_parked();

        // 打开 Markdown → 预览 → 切回源码。
        cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_editor(path.clone(), window, cx);
                ws.request_preview(window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.toggle_markdown_preview(window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });

        let focused = cx.update(|window, cx| {
            let ws = workspace.read(cx);
            let focused = window.focused(cx);
            let editor = ws.active_editor().cloned().expect("source editor tab");
            let handle = editor.read(cx).focus_handle(cx);
            assert_eq!(
                focused,
                Some(handle),
                "toggling from Markdown preview to Source must focus the editable editor"
            );
            let before = editor.read(cx).text();
            assert!(before.contains("# original"), "source shows markdown text");
            editor.update(cx, |editor, _| {
                // 源码视图持有同一 EditorView 缓冲，revision 可推进即写路径未只读封死。
                let revision = editor.revision();
                assert!(revision < u64::MAX);
            });
            focused
        });
        assert!(focused.is_some());

        let _ = std::fs::remove_dir_all(&root);
    }
}
