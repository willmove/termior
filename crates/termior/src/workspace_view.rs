use crate::ai_diff_view::{AiDiffAction, AiDiffView};
use crate::app_identity;
use crate::composer_view::{ComposerView, EditReviewRequested};
use crate::editor_view::EditorView;
use crate::git_views::{GitDiffAction, GitDiffView, GitHistoryAction, GitHistoryView};
use crate::markdown_preview_view::MarkdownPreviewView;
use crate::preview_view::PreviewView;
use crate::settings_view::SettingsView;
use crate::terminal_view::{TerminalView, TerminalViewEvent};
use crate::ui::{self, ButtonKind};
use futures::StreamExt;
use gpui::{
    anchored, canvas, div, prelude::*, px, relative, size, AnyElement, AnyWindowHandle, App,
    Bounds, Context, CursorStyle, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, PromptButton, PromptLevel, Role, SharedString, Task, UTF16Selection, Window,
    WindowAppearance, WindowBounds,
};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use termior_ai::AttachmentSource;
use termior_ai::{HttpProvider, InlineCompleter, KeyringSecretStore, ProviderConfig, SecretStore};
use termior_explorer::{
    ContentMatch, ContentSearch, FileEntry, FileIndex, IconKind, TreeState, WorkspaceWatcher,
};
use termior_platform::{
    AgentIndicator, AgentStatus, NativeNotifier, Notification, NotificationContext,
    NotificationDecision, NotificationRouter, NotificationTarget, SystemNotifier,
};
use termior_preview::{is_markdown_path, normalize_preview_url};
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{
    app_data_dir, atomic_write, default_settings, migrate, KeyAction, Settings, ShellDetection,
};
use termior_terminal_core::osc::AgentState;
use termior_theme::{Appearance, ResolvedPalette, Theme, ThemeLibrary};
use termior_ui::{SidebarPanel, TabId, TabKind, WorkspaceState};
use termior_ui_kit::{LayoutNode, PaneId, SplitDirection};
use termior_vcs::{
    BranchState, ChangeGroup, ChangedFile, CommitInfo, GitRepository, RemoteOperation,
};

const DEFAULT_SIDEBAR_WIDTH: f32 = 280.0;
const MIN_SIDEBAR_WIDTH: f32 = 220.0;
const MAX_SIDEBAR_WIDTH: f32 = 520.0;
const WORKSPACE_HEADER_HEIGHT: f32 = 40.0;
const STATUS_BAR_HEIGHT: f32 = 24.0;
const COMPOSER_MAX_HEIGHT: f32 = 330.0;
const PANE_DIVIDER_SIZE: f32 = 5.0;
const MIN_PANE_WIDTH: f32 = 320.0;
const MIN_PANE_HEIGHT: f32 = 180.0;
const MAX_PANES_PER_TAB: usize = 8;

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
    fn element(&self) -> AnyElement {
        match self {
            Self::Terminal(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Editor(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Markdown(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Preview(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::AiDiff(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::GitDiff(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::GitHistory(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Placeholder(message) => div()
                .flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(SharedString::from(message.clone()))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandMode {
    Browse,
    FindFile,
    SearchContent,
    CreateFile,
    CreateDirectory,
    Rename,
    GitCommit,
    GitCreateBranch,
    GitSwitchBranch,
    PreviewUrl,
}

impl CommandMode {
    fn label(self) -> &'static str {
        match self {
            Self::Browse => "Browse",
            Self::FindFile => "Find file",
            Self::SearchContent => "Search content",
            Self::CreateFile => "Create file",
            Self::CreateDirectory => "Create directory",
            Self::Rename => "Rename",
            Self::GitCommit => "Git commit",
            Self::GitCreateBranch => "Create branch",
            Self::GitSwitchBranch => "Switch branch",
            Self::PreviewUrl => "Web preview URL",
        }
    }
}

#[derive(Debug, Clone)]
struct ExplorerContextMenu {
    target: ExplorerContextTarget,
    position: Point<Pixels>,
}

#[derive(Debug, Clone)]
struct PaneResizeState {
    path: Vec<usize>,
    direction: SplitDirection,
    start_position: Point<Pixels>,
    start_ratio: f32,
    extent: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExplorerContextTarget {
    Workspace,
    Directory(PathBuf),
    File(PathBuf),
}

impl ExplorerContextTarget {
    fn path(&self, root: &Path) -> PathBuf {
        match self {
            Self::Workspace => root.to_path_buf(),
            Self::Directory(path) | Self::File(path) => path.clone(),
        }
    }

    fn kind(&self) -> ExplorerContextTargetKind {
        match self {
            Self::Workspace => ExplorerContextTargetKind::Workspace,
            Self::Directory(_) => ExplorerContextTargetKind::Directory,
            Self::File(_) => ExplorerContextTargetKind::File,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplorerContextTargetKind {
    Workspace,
    Directory,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplorerContextAction {
    Open,
    CreateFile,
    CreateDirectory,
    Rename,
    Delete,
    Reveal,
    AttachToAi,
    FindFile,
    SearchContent,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SplitMenuAction {
    Smart,
    Right,
    Down,
    CloseActive,
    CloseOthers,
}

pub struct WorkspaceView {
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
    explorer_requested_root: PathBuf,
    explorer_loading: bool,
    explorer_scan_generation: u64,
    explorer_error: Option<String>,
    _background_task: Option<Task<()>>,
    command_mode: CommandMode,
    command_input: String,
    command_marked_text: String,
    command_message: Option<String>,
    pending_name_parent: Option<PathBuf>,
    pending_rename_target: Option<PathBuf>,
    explorer_context_menu: Option<ExplorerContextMenu>,
    split_menu_open: bool,
    theme_menu_open: bool,
    sidebar_resizing: bool,
    pane_resizing: Option<PaneResizeState>,
    content_matches: Vec<ContentMatch>,
    content_search_generation: u64,
    content_searching: bool,
    vcs_status: Vec<ChangedFile>,
    vcs_history: Vec<CommitInfo>,
    vcs_branch: Option<BranchState>,
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
        let theme_index = themes
            .iter()
            .position(|theme| theme.id == settings.theme_id)
            .unwrap_or(0);
        let palette = themes[theme_index].resolve(
            match settings.appearance {
                termior_store::settings::Appearance::Light => Appearance::Light,
                termior_store::settings::Appearance::Dark => Appearance::Dark,
                termior_store::settings::Appearance::FollowSystem => Appearance::FollowSystem,
            },
            system_is_dark,
        );
        cx.set_global(ui::ActiveTheme(palette.clone()));
        let mut model = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("Termior-workspaces.json")).ok())
            .and_then(|raw| serde_json::from_str::<WorkspaceState>(&raw).ok())
            .filter(|state| state.root == root)
            .unwrap_or_else(|| WorkspaceState::new(root.clone()));
        model.sidebar_width = model
            .sidebar_width
            .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
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
                                            "Markdown preview source is unavailable".into(),
                                        )
                                    }),
                                TabKind::Terminal | TabKind::Preview => {
                                    PaneContent::Placeholder(format!("Restoring {}…", tab.title))
                                }
                                TabKind::AiDiff | TabKind::GitDiff | TabKind::GitCommitFile => {
                                    PaneContent::Placeholder(format!(
                                        "{} · reopen the source item to refresh this diff",
                                        tab.title
                                    ))
                                }
                                TabKind::GitHistory => PaneContent::Placeholder(format!(
                                    "{} · history is available in the sidebar",
                                    tab.title
                                )),
                            },
                        )
                    })
                    .collect(),
            })
            .collect();
        if model.tabs.is_empty() {
            model.active = None;
        }
        let workspace_auth =
            WorkspaceAuthRegistry::with_roots([root.to_string_lossy().replace('\\', "/")]);
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
        cx.subscribe(
            &composer,
            |workspace, _composer, status: &AgentStatus, cx| {
                workspace.queue_agent_update(
                    "builtin-agent",
                    "Built-in Agent",
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
        let mut notification_router = NotificationRouter::default();
        notification_router.set_enabled(settings.agent_notifications);
        notification_router.update_agent(AgentIndicator {
            id: "builtin-agent".into(),
            title: "Built-in Agent".into(),
            status: AgentStatus::Finished,
            tab_id: None,
        });
        let explorer_tree = TreeState::new(root.clone());
        let (vcs_status, vcs_history, vcs_branch) = load_vcs(&root, &workspace_auth);
        Self {
            model,
            tabs,
            themes,
            theme_index,
            palette,
            focus_handle: cx.focus_handle(),
            composer,
            explorer: None,
            explorer_tree,
            explorer_watcher: None,
            explorer_requested_root: root,
            explorer_loading: true,
            explorer_scan_generation: 0,
            explorer_error: None,
            _background_task: None,
            command_mode: CommandMode::Browse,
            command_input: String::new(),
            command_marked_text: String::new(),
            command_message: None,
            pending_name_parent: None,
            pending_rename_target: None,
            explorer_context_menu: None,
            split_menu_open: false,
            theme_menu_open: false,
            sidebar_resizing: false,
            pane_resizing: None,
            content_matches: Vec::new(),
            content_search_generation: 0,
            content_searching: false,
            vcs_status,
            vcs_history,
            vcs_branch,
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
        let terminal_panes: Vec<(TabId, PaneId, Option<PathBuf>, bool)> = self
            .model
            .tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::Terminal)
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
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
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
                        workspace
                            .schedule_explorer_scan(workspace.explorer_requested_root.clone(), cx);
                        workspace.refresh_vcs_data();
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        }));
    }

    /// Exercises the same request path as the header Preview button with an active Markdown file.
    /// Used by `scripts/markdown-preview-smoke.ps1`; normal launches never call this method.
    pub(crate) fn start_markdown_preview_smoke(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.model.root.join("preview-smoke.md");
        self.open_editor(path.clone(), cx);
        self.request_preview(window, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            // Keep the window alive long enough for GPUI to layout and paint the new native view.
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let result = workspace.update_in(cx, |workspace, _, cx| {
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
                workspace.open_editor(path, cx);
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

    fn schedule_explorer_scan(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if !root.is_dir() {
            self.explorer_loading = false;
            self.explorer_error = Some(format!(
                "Explorer root is not a directory: {}",
                root.display()
            ));
            return;
        }

        self.explorer_requested_root = root.clone();
        self.explorer_tree.set_root(root.clone());
        self.explorer_loading = true;
        self.explorer_error = None;
        self.explorer_scan_generation = self.explorer_scan_generation.saturating_add(1);
        let generation = self.explorer_scan_generation;
        let show_dotfiles = self.settings.show_dotfiles;
        let scan = cx
            .background_executor()
            .spawn(async move { FileIndex::build(root, show_dotfiles) });
        cx.spawn(async move |workspace, cx| {
            let result = scan.await;
            let _ = workspace.update(cx, |workspace, cx| {
                if generation != workspace.explorer_scan_generation {
                    return;
                }
                workspace.explorer_loading = false;
                match result {
                    Ok(index) if index.root() == workspace.explorer_requested_root => {
                        let root_changed = workspace
                            .explorer
                            .as_ref()
                            .map_or(true, |current| current.root() != index.root());
                        if root_changed || workspace.explorer_watcher.is_none() {
                            workspace.explorer_watcher = WorkspaceWatcher::watch(index.root()).ok();
                        }
                        let paths = index
                            .entries()
                            .iter()
                            .filter(|entry| !entry.is_dir)
                            .map(|entry| entry.relative.clone())
                            .collect();
                        workspace
                            .composer
                            .update(cx, |composer, _| composer.set_workspace_paths(paths));
                        workspace.explorer = Some(index);
                        workspace.explorer_error = None;
                    }
                    Ok(_) => return,
                    Err(error) => {
                        workspace.explorer_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_workspace_data(&mut self, cx: &mut Context<Self>) {
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
        self.refresh_vcs_data();
    }

    fn refresh_vcs_data(&mut self) {
        let (status, history, branch) = load_vcs(&self.model.root, &self.workspace_auth);
        self.vcs_status = status;
        self.vcs_history = history;
        self.vcs_branch = branch;
    }

    fn open_workspace_picker(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The blocking `rfd::FileDialog` must never run inside a GPUI event
        // handler: on Windows its modal message loop re-enters the foreground
        // executor while the `App` RefCell is still borrowed, aborting the
        // process. Await the async dialog instead and hop back into the view.
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title("Open a Termior workspace")
                .pick_folder()
                .await
            else {
                return;
            };
            let root = folder.path().to_path_buf();
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                if !root.is_dir() || root == workspace.model.root {
                    return;
                }
                let system_is_dark = matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                );
                *workspace = Self::new(root, system_is_dark, cx);
                workspace.restore_or_create_runtime(window, cx);
                workspace.start_background_services(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn create_terminal(&mut self, private: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.model.new_tab(
            TabKind::Terminal,
            if private {
                "Private terminal"
            } else {
                "Terminal"
            },
            private,
        );
        let cwd = self.model.active_tab().map(|tab| tab.cwd.clone());
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Placeholder("Starting terminal…".into())),
        });
        self.activate_runtime(id, cx);
        self.spawn_terminal_into(
            id,
            PaneId(1),
            cwd,
            private,
            Some(window.window_handle()),
            cx,
        );
        cx.notify();
    }

    fn spawn_terminal_into(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        cwd: Option<PathBuf>,
        private: bool,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        let palette = self.palette.clone();
        let terminal_settings = self.settings.terminal.clone();
        let keymap = self.settings.keymap.clone();
        let workspace_auth = self.workspace_auth.clone();
        let shell_program = match &terminal_settings.shell_detection {
            ShellDetection::Auto => None,
            ShellDetection::Manual { path } if !path.trim().is_empty() => Some(path.clone()),
            ShellDetection::Manual { .. } => None,
        };
        let config = termior_terminal::PtySessionConfig {
            shell_program,
            shell_integration: true,
            inherit_environment: !private,
            cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
            workspace_auth: Some(workspace_auth),
            wsl_distribution: self.settings.wsl_distribution.clone(),
            ..Default::default()
        };
        let spawn_task = cx
            .background_executor()
            .spawn(async move { termior_terminal::TerminalBridge::spawn(&config) });
        cx.spawn(async move |workspace, cx| {
            let bridge = match spawn_task.await {
                Ok(bridge) => bridge,
                Err(error) => {
                    let _ = workspace.update(cx, |workspace, cx| {
                        if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                            if let Some(pane) = tab.panes.get_mut(&pane_id) {
                                *pane =
                                    PaneContent::Placeholder(format!("Terminal failed: {error}"));
                            }
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = workspace.update(cx, |workspace, cx| {
                let entity = cx.new(|cx| {
                    TerminalView::from_bridge(bridge, palette, terminal_settings, keymap, cx)
                });
                let terminal_focus = entity.read(cx).focus_handle(cx);
                let agent_id = format!("terminal-agent-{}-{}", tab_id.0, pane_id.0);
                let agent_title = workspace
                    .model
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| format!("{} · terminal agent", tab.title))
                    .unwrap_or_else(|| format!("Terminal {} agent", tab_id.0));
                cx.subscribe(
                    &entity,
                    move |workspace, _terminal, state: &AgentState, cx| {
                        workspace.queue_agent_update(
                            &agent_id,
                            &agent_title,
                            Some(tab_id.0),
                            NotificationTarget::Tab(tab_id.0),
                            terminal_agent_status(*state),
                            cx,
                        );
                    },
                )
                .detach();
                cx.subscribe(
                    &entity,
                    move |workspace, _terminal, event: &TerminalViewEvent, cx| {
                        // A shell that exits (e.g. via `exit`) closes its pane — and the
                        // whole tab when it was the tab's final pane — regardless of
                        // which pane is focused.
                        if let TerminalViewEvent::Exited(code) = event {
                            workspace.handle_terminal_exited(
                                tab_id,
                                pane_id,
                                *code,
                                window_handle,
                                cx,
                            );
                            cx.notify();
                            return;
                        }
                        let focused = workspace
                            .model
                            .tabs
                            .iter()
                            .find(|tab| tab.id == tab_id)
                            .is_some_and(|tab| tab.layout.focused == pane_id);
                        if focused {
                            if let Some(tab) =
                                workspace.model.tabs.iter_mut().find(|tab| tab.id == tab_id)
                            {
                                match event {
                                    TerminalViewEvent::TitleChanged(title) => {
                                        tab.title =
                                            title.clone().unwrap_or_else(|| "Terminal".to_owned());
                                    }
                                    TerminalViewEvent::Bell => {
                                        log::debug!("terminal bell in tab {}", tab_id.0);
                                    }
                                    TerminalViewEvent::Exited(_) => {}
                                }
                            }
                        }
                        cx.notify();
                    },
                )
                .detach();
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    if let Some(pane) = tab.panes.get_mut(&pane_id) {
                        *pane = PaneContent::Terminal(entity);
                    }
                }
                let should_focus = workspace.model.active == Some(tab_id)
                    && workspace
                        .model
                        .active_tab()
                        .is_some_and(|tab| tab.layout.focused == pane_id);
                if should_focus {
                    if let Some(window_handle) = window_handle {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            window.focus(&terminal_focus, cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_editor(&mut self, cx: &mut Context<Self>) {
        let id = self.model.new_tab(TabKind::Editor, "Untitled", false);
        let (completer, completion_enabled) = self.completion_config();
        let editor = cx.new(EditorView::untitled);
        editor.update(cx, |editor, _| {
            editor.set_preferences(
                &self.settings.editor_theme_id,
                self.settings.vim_mode,
                completer,
                completion_enabled,
            )
        });
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Editor(editor)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn open_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let resource = path.to_string_lossy().into_owned();
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::Editor && tab.resource.as_deref() == Some(resource.as_str())
            })
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Editor")
            .to_owned();
        let entity = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::Markdown && tab.resource.as_deref() == Some(resource.as_str())
            })
            .and_then(|model_tab| self.tabs.iter().find(|tab| tab.id == model_tab.id))
            .and_then(|tab| {
                tab.panes.values().find_map(|pane| match pane {
                    PaneContent::Markdown(preview) => Some(preview.read(cx).source()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| cx.new(|cx| EditorView::open(&path, cx)));
        let (completer, completion_enabled) = self.completion_config();
        entity.update(cx, |editor, _| {
            editor.set_preferences(
                &self.settings.editor_theme_id,
                self.settings.vim_mode,
                completer,
                completion_enabled,
            )
        });
        if entity.read(cx).path().is_none() {
            log::warn!("could not open editor file: {}", path.display());
        }
        let id = self.model.new_tab(TabKind::Editor, title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(resource);
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Editor(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn request_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.request_markdown_preview(cx) {
            self.request_web_preview(window, cx);
        }
    }

    fn request_markdown_preview(&mut self, cx: &mut Context<Self>) -> bool {
        let markdown_source = self.active_editor().and_then(|editor| {
            editor
                .read(cx)
                .path()
                .filter(|path| is_markdown_path(path))
                .map(|path| (path.to_path_buf(), editor.clone()))
        });
        if let Some((path, source)) = markdown_source {
            self.create_markdown_preview(path, source, cx);
            true
        } else {
            false
        }
    }

    fn request_web_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self
            .active_terminal()
            .and_then(|terminal| terminal.read(cx).localhost_urls().last().cloned())
        {
            self.create_preview_url(url, window, cx);
            return;
        }
        self.command_mode = CommandMode::PreviewUrl;
        self.command_input.clear();
        self.command_marked_text.clear();
        self.command_message = Some(
            "No local dev server was detected. Enter an http(s) URL, then press Enter.".into(),
        );
        self.model.sidebar_visible = true;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn create_markdown_preview(
        &mut self,
        path: PathBuf,
        source: Entity<EditorView>,
        cx: &mut Context<Self>,
    ) {
        let resource = path.to_string_lossy().into_owned();
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::Markdown && tab.resource.as_deref() == Some(resource.as_str())
            })
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }

        let source_title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown");
        let id = self.model.new_tab(
            TabKind::Markdown,
            format!("Preview · {source_title}"),
            false,
        );
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(resource);
        }
        let preview = cx.new(|cx| MarkdownPreviewView::new(path, source, cx));
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Markdown(preview)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn create_preview_url(&mut self, url: String, _window: &mut Window, cx: &mut Context<Self>) {
        let id = self.model.new_tab(TabKind::Preview, "Web Preview", false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(url.clone());
        }
        let preview = new_preview_view(url, cx);
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Preview(preview)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn open_ai_diff(&mut self, summary: termior_ai::EditProposalSummary, cx: &mut Context<Self>) {
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::AiDiff && tab.resource.as_deref() == Some(summary.id.as_str())
            })
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }
        let title = Path::new(&summary.path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("AI Diff · {name}"))
            .unwrap_or_else(|| "AI Diff".into());
        let proposal_id = summary.id.clone();
        let entity = cx.new(|_| AiDiffView::new(summary));
        cx.subscribe(&entity, |workspace, diff, action: &AiDiffAction, cx| {
            let result = match action {
                AiDiffAction::Apply {
                    proposal_id,
                    accepted_hunks,
                } => workspace.composer.update(cx, |composer, cx| {
                    composer.apply_reviewed_edit(proposal_id, accepted_hunks, cx)
                }),
                AiDiffAction::RejectAll { proposal_id } => {
                    workspace.composer.update(cx, |composer, cx| {
                        composer.reject_reviewed_edit(proposal_id, cx)
                    })
                }
            };
            diff.update(cx, |diff, cx| diff.set_result(result, cx));
            workspace.refresh_workspace_data(cx);
            cx.notify();
        })
        .detach();
        let id = self.model.new_tab(TabKind::AiDiff, title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(proposal_id);
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::AiDiff(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn open_git_diff(&mut self, path: String, group: ChangeGroup, cx: &mut Context<Self>) {
        let resource = format!("{group:?}:{path}");
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::GitDiff && tab.resource.as_deref() == Some(resource.as_str())
            })
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }
        let patch = self
            .git_repository()
            .and_then(|repo| {
                repo.diff_file(&path, group == ChangeGroup::Staged)
                    .map_err(|error| error.to_string())
            })
            .unwrap_or_else(|error| format!("Could not load diff: {error}"));
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("Git Diff · {name}"))
            .unwrap_or_else(|| "Git Diff".into());
        let entity = cx.new(|_| GitDiffView::working(path.clone(), group, patch));
        cx.subscribe(&entity, |workspace, diff, action: &GitDiffAction, cx| {
            workspace.handle_git_diff_action(&diff, action.clone(), cx)
        })
        .detach();
        let id = self.model.new_tab(TabKind::GitDiff, title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(resource);
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::GitDiff(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn handle_git_diff_action(
        &mut self,
        view: &Entity<GitDiffView>,
        action: GitDiffAction,
        cx: &mut Context<Self>,
    ) {
        let (path, preferred_group, result) = match self.git_repository() {
            Ok(repo) => match action {
                GitDiffAction::StageHunk { path, patch } => (
                    path,
                    ChangeGroup::Unstaged,
                    repo.stage_hunk(&patch)
                        .map(|_| "Hunk staged".to_owned())
                        .map_err(|error| error.to_string()),
                ),
                GitDiffAction::UnstageHunk { path, patch } => (
                    path,
                    ChangeGroup::Staged,
                    repo.unstage_hunk(&patch)
                        .map(|_| "Hunk unstaged".to_owned())
                        .map_err(|error| error.to_string()),
                ),
                GitDiffAction::StageFile(path) => {
                    let result = repo
                        .stage_file(&path)
                        .map(|_| "File staged".to_owned())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Staged, result)
                }
                GitDiffAction::UnstageFile(path) => {
                    let result = repo
                        .unstage_file(&path)
                        .map(|_| "File unstaged".to_owned())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Unstaged, result)
                }
                GitDiffAction::DiscardFile(path) => {
                    let result = repo
                        .discard_file(&path)
                        .map(|_| "Working-tree changes discarded".to_owned())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Unstaged, result)
                }
            },
            Err(error) => (String::new(), ChangeGroup::Unstaged, Err(error)),
        };
        self.refresh_vcs_data();
        let group = self
            .vcs_status
            .iter()
            .find(|file| file.path == path && file.group == preferred_group)
            .or_else(|| self.vcs_status.iter().find(|file| file.path == path))
            .map(|file| file.group);
        let patch = group.and_then(|group| {
            self.git_repository()
                .ok()
                .and_then(|repo| repo.diff_file(&path, group == ChangeGroup::Staged).ok())
        });
        view.update(cx, |view, cx| {
            view.update_after_action(result, patch, group, cx)
        });
        cx.notify();
    }

    fn open_git_history(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| tab.kind == TabKind::GitHistory)
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }
        self.refresh_vcs_data();
        let commits = self.vcs_history.clone();
        let files = commits
            .first()
            .and_then(|commit| {
                self.git_repository()
                    .ok()
                    .and_then(|repo| repo.commit_files(&commit.id).ok())
            })
            .unwrap_or_default();
        let entity = cx.new(|cx| GitHistoryView::new(commits, files, cx));
        cx.subscribe(
            &entity,
            |workspace, history, action: &GitHistoryAction, cx| match action {
                GitHistoryAction::SelectCommit(commit) => {
                    let files = workspace
                        .git_repository()
                        .ok()
                        .and_then(|repo| repo.commit_files(commit).ok())
                        .unwrap_or_default();
                    history.update(cx, |history, cx| {
                        history.set_commit_files(commit.clone(), files, cx)
                    });
                }
                GitHistoryAction::OpenFile { commit, path } => {
                    workspace.open_git_commit_file(commit.clone(), path.clone(), cx)
                }
                GitHistoryAction::OpenRemote(commit) => {
                    if let Some(url) = workspace
                        .git_repository()
                        .ok()
                        .and_then(|repo| repo.remote_commit_url(commit))
                    {
                        cx.open_url(&url);
                    } else {
                        workspace.command_message =
                            Some("No supported origin URL for this commit".into());
                    }
                }
            },
        )
        .detach();
        let id = self
            .model
            .new_tab(TabKind::GitHistory, "Git History", false);
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::GitHistory(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn open_git_commit_file(&mut self, commit: String, path: String, cx: &mut Context<Self>) {
        let resource = format!("{commit}:{path}");
        if let Some(id) = self
            .model
            .tabs
            .iter()
            .find(|tab| {
                tab.kind == TabKind::GitCommitFile
                    && tab.resource.as_deref() == Some(resource.as_str())
            })
            .map(|tab| tab.id)
        {
            self.activate_runtime(id, cx);
            cx.notify();
            return;
        }
        let patch = self
            .git_repository()
            .and_then(|repo| {
                repo.diff_commit_file(&commit, &path)
                    .map_err(|error| error.to_string())
            })
            .unwrap_or_else(|error| format!("Could not load commit diff: {error}"));
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("Commit · {name}"))
            .unwrap_or_else(|| "Commit File".into());
        let entity = cx.new(|_| GitDiffView::commit_file(path, patch));
        let id = self.model.new_tab(TabKind::GitCommitFile, title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(resource);
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::GitDiff(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn pane_area_size(&self, window: &Window) -> (f32, f32) {
        let viewport = window.viewport_size();
        let sidebar_width = if self.model.sidebar_visible {
            self.model.sidebar_width
        } else {
            0.0
        };
        let composer_height = if self.model.composer_visible {
            COMPOSER_MAX_HEIGHT
        } else {
            0.0
        };
        (
            (f32::from(viewport.width) - sidebar_width).max(0.0),
            (f32::from(viewport.height)
                - WORKSPACE_HEADER_HEIGHT
                - STATUS_BAR_HEIGHT
                - composer_height)
                .max(0.0),
        )
    }

    fn active_pane_count(&self) -> usize {
        self.model
            .active_tab()
            .map(|tab| tab.layout.panes().len())
            .unwrap_or(0)
    }

    fn can_split_active(&self, direction: SplitDirection, window: &Window) -> bool {
        let Some(tab) = self.model.active_tab() else {
            return false;
        };
        let (width, height) = self.pane_area_size(window);
        tab.layout.can_split_focused(
            direction,
            width,
            height,
            PANE_DIVIDER_SIZE,
            MIN_PANE_WIDTH,
            MIN_PANE_HEIGHT,
            MAX_PANES_PER_TAB,
        )
    }

    fn preferred_split_direction(&self, window: &Window) -> Option<SplitDirection> {
        let can_right = self.can_split_active(SplitDirection::Right, window);
        let can_down = self.can_split_active(SplitDirection::Down, window);
        match (can_right, can_down) {
            (true, false) => Some(SplitDirection::Right),
            (false, true) => Some(SplitDirection::Down),
            (false, false) => None,
            (true, true) => {
                let tab = self.model.active_tab()?;
                let (width, height) = self.pane_area_size(window);
                let extent = tab
                    .layout
                    .focused_extent(width, height, PANE_DIVIDER_SIZE)?;
                if extent.width >= extent.height {
                    Some(SplitDirection::Right)
                } else {
                    Some(SplitDirection::Down)
                }
            }
        }
    }

    fn split_unavailable_message(&self, direction: Option<SplitDirection>) -> String {
        if self.model.active_tab().is_none() {
            return "Open a tab before splitting a pane.".into();
        }
        if self.active_pane_count() >= MAX_PANES_PER_TAB {
            return format!("This tab has reached the limit of {MAX_PANES_PER_TAB} panes.");
        }
        match direction {
            Some(SplitDirection::Right) => {
                "The focused pane is too narrow to split right. Resize the window or close another pane."
                    .into()
            }
            Some(SplitDirection::Down) => {
                "The focused pane is too short to split down. Resize the window or close another pane."
                    .into()
            }
            None => "The focused pane is too small to split. Resize the window or close another pane."
                .into(),
        }
    }

    fn show_split_unavailable(
        &mut self,
        direction: Option<SplitDirection>,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_toast(
            Notification {
                title: "Cannot split pane".into(),
                body: self.split_unavailable_message(direction),
                target: NotificationTarget::Global,
                status: AgentStatus::Attention,
            },
            cx,
        );
    }

    fn smart_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_menu_open = false;
        if let Some(direction) = self.preferred_split_direction(window) {
            self.split_active(direction, window, cx);
        } else {
            self.show_split_unavailable(None, cx);
        }
    }

    fn split_active(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_menu_open = false;
        if !self.can_split_active(direction, window) {
            self.show_split_unavailable(Some(direction), cx);
            return;
        }
        let Some(active) = self.model.active else {
            return;
        };
        let Ok(pane_id) = self.model.split_active(direction) else {
            return;
        };
        let kind = self.model.active_tab().map(|tab| tab.kind);
        let pane = match kind {
            Some(TabKind::Editor) => {
                let (completer, completion_enabled) = self.completion_config();
                let editor = cx.new(EditorView::untitled);
                editor.update(cx, |editor, _| {
                    editor.set_preferences(
                        &self.settings.editor_theme_id,
                        self.settings.vim_mode,
                        completer,
                        completion_enabled,
                    )
                });
                PaneContent::Editor(editor)
            }
            Some(TabKind::Terminal) => PaneContent::Placeholder("Starting split terminal…".into()),
            _ => PaneContent::Placeholder("Split view".into()),
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active) {
            tab.panes.insert(pane_id, pane);
        } else {
            return;
        }
        if kind == Some(TabKind::Terminal) {
            let cwd = self.model.active_tab().map(|tab| tab.cwd.clone());
            let private = self
                .model
                .active_tab()
                .is_some_and(|tab| tab.private_terminal);
            self.spawn_terminal_into(
                active,
                pane_id,
                cwd,
                private,
                Some(window.window_handle()),
                cx,
            );
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_menu_open = false;
        if self.active_pane_count() <= 1 {
            self.enqueue_toast(
                Notification {
                    title: "Cannot close pane".into(),
                    body: "This is the only pane in the tab.".into(),
                    target: NotificationTarget::Global,
                    status: AgentStatus::Attention,
                },
                cx,
            );
            return;
        }
        let Some(tab_id) = self.model.active else {
            return;
        };
        let Ok(Some(pane_id)) = self.model.close_active_pane_or_tab() else {
            return;
        };
        self.remove_terminal_agent(tab_id, pane_id);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.panes.remove(&pane_id);
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn close_other_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split_menu_open = false;
        let Some(tab_id) = self.model.active else {
            return;
        };
        let Ok(removed) = self.model.close_other_panes() else {
            return;
        };
        for pane_id in &removed {
            self.remove_terminal_agent(tab_id, *pane_id);
        }
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            for pane_id in removed {
                tab.panes.remove(&pane_id);
            }
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn handle_split_menu_action(
        &mut self,
        action: SplitMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SplitMenuAction::Smart => self.smart_split(window, cx),
            SplitMenuAction::Right => self.split_active(SplitDirection::Right, window, cx),
            SplitMenuAction::Down => self.split_active(SplitDirection::Down, window, cx),
            SplitMenuAction::CloseActive => self.close_active_pane(window, cx),
            SplitMenuAction::CloseOthers => self.close_other_panes(window, cx),
        }
    }

    /// 关闭指定标签页(标签栏 ✕ 按钮/中键点击),不要求它是活动标签。
    fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.model.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let pane_ids = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for pane_id in pane_ids {
            self.remove_terminal_agent(id, pane_id);
        }
        self.model.tabs.remove(index);
        self.tabs.retain(|tab| tab.id != id);
        if self.model.active == Some(id) {
            self.model.active = if self.model.tabs.is_empty() {
                None
            } else {
                Some(self.model.tabs[index.min(self.model.tabs.len() - 1)].id)
            };
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
            }
        }
        cx.notify();
    }

    fn close_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.model.active else { return };
        let pane_ids = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.panes.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        if let Ok(removed_pane) = self.model.close_active_pane_or_tab() {
            if let Some(pane_id) = removed_pane {
                self.remove_terminal_agent(id, pane_id);
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.panes.remove(&pane_id);
                }
            } else {
                for pane_id in pane_ids {
                    self.remove_terminal_agent(id, pane_id);
                }
                self.tabs.retain(|tab| tab.id != id);
            }
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
            }
            cx.notify();
        }
    }

    fn activate_runtime(&mut self, id: TabId, _cx: &mut Context<Self>) {
        let _ = self.model.switch_to(id);
        // No per-pane activation hook is needed now that the embedded WebView is gone
        // (ADR 0002): preview tabs render a placeholder and have no surface to show/hide.
    }

    /// A terminal's shell exited: close its pane, and the whole tab when the pane
    /// was the tab's last one. Runs regardless of which pane is focused — the shell
    /// may have exited in a split (`exit`) or in a background tab on its own.
    fn handle_terminal_exited(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        exit_code: Option<i32>,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if let Some(code) = exit_code {
            log::info!(
                "terminal pane {} in tab {} exited with code {code}",
                pane_id.0,
                tab_id.0
            );
        }
        let had_focus = self.model.active == Some(tab_id)
            && self
                .model
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .is_some_and(|tab| tab.layout.focused == pane_id);
        let closed = match self.model.close_pane(tab_id, pane_id) {
            Ok(closed) => closed,
            Err(error) => {
                log::warn!("could not close exited terminal pane: {error}");
                return;
            }
        };
        self.remove_terminal_agent(tab_id, pane_id);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.panes.remove(&pane_id);
        }
        match closed {
            Some(_) => {
                // The exited pane had keyboard focus: hand it to the surviving pane.
                if had_focus {
                    let focus = self.active_pane_focus_handle(cx);
                    if let (Some(handle), Some(focus)) = (window_handle, focus) {
                        let _ = cx.update_window(handle, |_, window, cx| {
                            window.focus(&focus, cx);
                        });
                    }
                }
            }
            None => {
                self.tabs.retain(|tab| tab.id != tab_id);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                }
            }
        }
    }

    fn active_pane_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let model_tab = self.model.active_tab()?;
        match tab.panes.get(&model_tab.layout.focused)? {
            PaneContent::Terminal(terminal) => Some(terminal.read(cx).focus_handle(cx)),
            PaneContent::Editor(editor) => Some(editor.read(cx).focus_handle(cx)),
            PaneContent::GitHistory(history) => Some(history.read(cx).focus_handle(cx)),
            PaneContent::Markdown(_)
            | PaneContent::Preview(_)
            | PaneContent::AiDiff(_)
            | PaneContent::GitDiff(_)
            | PaneContent::Placeholder(_) => None,
        }
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = self.active_pane_focus_handle(cx) {
            window.focus(&focus, cx);
        }
    }

    fn active_terminal(&self) -> Option<&Entity<TerminalView>> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.active_tab()?.layout.focused;
        tab.panes
            .get(&focused)
            .and_then(|pane| match pane {
                PaneContent::Terminal(entity) => Some(entity),
                _ => None,
            })
            .or_else(|| {
                tab.panes.values().find_map(|pane| match pane {
                    PaneContent::Terminal(entity) => Some(entity),
                    _ => None,
                })
            })
    }

    fn active_editor(&self) -> Option<&Entity<EditorView>> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.active_tab()?.layout.focused;
        tab.panes
            .get(&focused)
            .and_then(|pane| match pane {
                PaneContent::Editor(entity) => Some(entity),
                _ => None,
            })
            .or_else(|| {
                tab.panes.values().find_map(|pane| match pane {
                    PaneContent::Editor(entity) => Some(entity),
                    _ => None,
                })
            })
    }

    /// 复用 FR-PROV 的 Provider 与密钥配置，构造一个行内补全 [`InlineCompleter`]。
    ///
    /// 遵循与 Composer 聊天 profile 相同的规则：profile 必须启用；非本地 profile 必须在
    /// OS 钥匙串中存有 key（密钥永不落盘，INV-5）。配置缺失或校验失败时返回 `None`，
    /// 编辑器侧静默不补全（FR-EDIT-05「请求失败/超时静默降级」）。
    fn build_completer(&self) -> Option<std::sync::Arc<InlineCompleter>> {
        build_completer_from_settings(&self.settings)
    }

    /// 当前补全配置：(completer, enabled)。enabled 为 false 或 completer 不可用时
    /// 返回 `(None, false)`，确保编辑器侧无论如何都不会发起请求。
    fn completion_config(&self) -> (Option<std::sync::Arc<InlineCompleter>>, bool) {
        if !self.settings.autocomplete_enabled {
            return (None, false);
        }
        match self.build_completer() {
            Some(completer) => (Some(completer), true),
            None => (None, false),
        }
    }

    fn queue_agent_update(
        &mut self,
        id: &str,
        title: &str,
        tab_id: Option<u64>,
        target: NotificationTarget,
        status: AgentStatus,
        cx: &mut Context<Self>,
    ) {
        self.pending_agent_updates.insert(
            id.to_owned(),
            PendingAgentUpdate {
                indicator: AgentIndicator {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    status,
                    tab_id,
                },
                notification: Notification {
                    title: title.to_owned(),
                    body: agent_status_message(status).to_owned(),
                    target,
                    status,
                },
            },
        );
        cx.notify();
    }

    fn remove_terminal_agent(&mut self, tab_id: TabId, pane_id: PaneId) {
        let id = format!("terminal-agent-{}-{}", tab_id.0, pane_id.0);
        self.pending_agent_updates.remove(&id);
        self.notification_router.remove_agent(&id);
    }

    fn process_agent_updates(&mut self, window: &Window, cx: &mut Context<Self>) {
        let updates = std::mem::take(&mut self.pending_agent_updates);
        let context = NotificationContext {
            window_focused: window.is_window_active(),
            active_tab: self.model.active.map(|id| id.0),
            composer_visible: self.model.composer_visible,
        };
        for update in updates.into_values() {
            if !self.notification_router.update_agent(update.indicator) {
                continue;
            }
            match self
                .notification_router
                .route(&update.notification, context)
            {
                NotificationDecision::Suppress => {}
                NotificationDecision::InAppToast => self.enqueue_toast(update.notification, cx),
                NotificationDecision::System => {
                    let notification = update.notification;
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(error) = NativeNotifier.notify(&notification) {
                                log::warn!("system notification failed: {error}");
                            }
                        })
                        .detach();
                }
            }
        }
    }

    fn enqueue_toast(&mut self, notification: Notification, cx: &mut Context<Self>) {
        const MAX_TOASTS: usize = 4;
        if self.toasts.len() >= MAX_TOASTS {
            self.toasts.remove(0);
        }
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.saturating_add(1);
        self.toasts.push(InAppToast { id, notification });
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |workspace, cx| {
            timer.await;
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.toasts.retain(|toast| toast.id != id);
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_bell(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bell_open = !self.bell_open;
        cx.notify();
    }

    fn open_agent_surface(&mut self, tab_id: Option<u64>, cx: &mut Context<Self>) {
        let target = tab_id.map_or(NotificationTarget::Composer, NotificationTarget::Tab);
        self.open_notification_target(target, cx);
        self.bell_open = false;
        cx.notify();
    }

    fn open_notification_target(&mut self, target: NotificationTarget, cx: &mut Context<Self>) {
        match target {
            NotificationTarget::Tab(tab_id) => {
                let id = TabId(tab_id);
                if self.model.tabs.iter().any(|tab| tab.id == id) {
                    self.activate_runtime(id, cx);
                }
            }
            NotificationTarget::Composer => self.model.composer_visible = true,
            NotificationTarget::Global => {}
        }
        cx.notify();
    }

    fn attach_active_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.active_editor().and_then(|editor| {
            let editor = editor.read(cx);
            editor
                .selected_text()
                .map(|text| (editor.title().to_owned(), text))
        });
        if let Some((label, text)) = selection {
            self.composer.update(cx, |composer, cx| {
                composer.attach_selection(AttachmentSource::Editor, label, text);
                cx.notify();
            });
            self.model.composer_visible = true;
            cx.notify();
        }
    }

    fn attach_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| {
            composer.attach_file(path);
            cx.notify();
        });
        self.model.composer_visible = true;
        cx.notify();
    }

    fn show_explorer_context_menu(
        &mut self,
        target: ExplorerContextTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &target {
            ExplorerContextTarget::Workspace => {}
            ExplorerContextTarget::Directory(path) | ExplorerContextTarget::File(path) => {
                self.explorer_tree.select(path.clone());
            }
        }
        self.command_mode = CommandMode::Browse;
        self.command_marked_text.clear();
        self.explorer_context_menu = Some(ExplorerContextMenu { target, position });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn handle_explorer_context_action(
        &mut self,
        action: ExplorerContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.explorer_context_menu.take().map(|menu| menu.target) else {
            return;
        };
        let root = self.explorer_requested_root.clone();
        match action {
            ExplorerContextAction::Open => {
                if let ExplorerContextTarget::File(path) = target {
                    self.open_editor(path, cx);
                }
            }
            ExplorerContextAction::CreateFile | ExplorerContextAction::CreateDirectory => {
                let parent = match target {
                    ExplorerContextTarget::Directory(path) => path,
                    ExplorerContextTarget::File(path) => {
                        path.parent().map(Path::to_path_buf).unwrap_or(root)
                    }
                    ExplorerContextTarget::Workspace => root,
                };
                if action == ExplorerContextAction::CreateFile {
                    self.begin_name_command(
                        CommandMode::CreateFile,
                        parent,
                        None,
                        "untitled",
                        window,
                        cx,
                    );
                } else {
                    self.begin_name_command(
                        CommandMode::CreateDirectory,
                        parent,
                        None,
                        "New Folder",
                        window,
                        cx,
                    );
                }
            }
            ExplorerContextAction::Rename => {
                let path = match target {
                    ExplorerContextTarget::Directory(path) | ExplorerContextTarget::File(path) => {
                        path
                    }
                    ExplorerContextTarget::Workspace => return,
                };
                let parent = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| root.clone());
                let prefill = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned();
                self.begin_name_command(
                    CommandMode::Rename,
                    parent,
                    Some(path),
                    &prefill,
                    window,
                    cx,
                );
            }
            ExplorerContextAction::Delete => self.delete_selected_with_prompt(window, cx),
            ExplorerContextAction::Reveal => {
                let path = target.path(&root);
                self.command_message = reveal_in_system_file_manager(
                    &path,
                    target.kind() == ExplorerContextTargetKind::File,
                )
                .err()
                .map(|error| format!("Could not reveal path: {error}"))
                .or_else(|| Some(format!("Revealed {}", path.display())));
            }
            ExplorerContextAction::AttachToAi => {
                if let ExplorerContextTarget::File(path) = target {
                    self.attach_file(path, cx);
                }
            }
            ExplorerContextAction::FindFile => self.begin_command(CommandMode::FindFile, cx),
            ExplorerContextAction::SearchContent => {
                self.begin_command(CommandMode::SearchContent, cx)
            }
            ExplorerContextAction::Refresh => {
                self.refresh_workspace_data(cx);
                self.command_message = Some("Explorer refresh scheduled".into());
            }
        }
        cx.notify();
    }

    fn has_dirty_editor_under(&self, target: &Path, cx: &App) -> bool {
        self.tabs.iter().any(|tab| {
            tab.panes.values().any(|pane| match pane {
                PaneContent::Editor(editor) => {
                    let editor = editor.read(cx);
                    editor.is_dirty()
                        && editor
                            .path()
                            .is_some_and(|path| path == target || path.starts_with(target))
                }
                _ => false,
            })
        })
    }

    fn retarget_open_editors(&mut self, old_path: &Path, new_path: &Path, cx: &mut Context<Self>) {
        let updates = self
            .model
            .tabs
            .iter()
            .filter_map(|tab| {
                if !matches!(tab.kind, TabKind::Editor | TabKind::Markdown) {
                    return None;
                }
                let resource = PathBuf::from(tab.resource.as_deref()?);
                let updated = if resource == old_path {
                    new_path.to_path_buf()
                } else {
                    let suffix = resource.strip_prefix(old_path).ok()?;
                    new_path.join(suffix)
                };
                Some((tab.id, updated))
            })
            .collect::<Vec<_>>();

        for (id, path) in updates {
            if let Some(tab) = self.model.tabs.iter_mut().find(|tab| tab.id == id) {
                tab.title = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Editor")
                    .to_owned();
                tab.resource = Some(path.to_string_lossy().into_owned());
            }
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                for pane in tab.panes.values_mut() {
                    match pane {
                        PaneContent::Editor(editor) => {
                            editor.update(cx, |editor, _| {
                                let Some(current) = editor.path().map(Path::to_path_buf) else {
                                    return;
                                };
                                let retargeted = if current == old_path {
                                    Some(new_path.to_path_buf())
                                } else {
                                    current
                                        .strip_prefix(old_path)
                                        .ok()
                                        .map(|suffix| new_path.join(suffix))
                                };
                                if let Some(retargeted) = retargeted {
                                    editor.set_path_after_rename(retargeted);
                                }
                            });
                        }
                        PaneContent::Markdown(preview) => preview.update(cx, |preview, cx| {
                            preview.set_path_after_rename(path.clone(), cx)
                        }),
                        _ => {}
                    }
                }
            }
        }
    }

    fn delete_selected_with_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.explorer_tree.selected().map(Path::to_path_buf) else {
            self.command_message = Some("Select a file or directory first".into());
            cx.notify();
            return;
        };
        if self.has_dirty_editor_under(&path, cx) {
            self.command_message = Some("Save open files under this path before deleting".into());
            cx.notify();
            return;
        }
        let Ok(relative) = path
            .strip_prefix(self.explorer_tree.root())
            .map(Path::to_path_buf)
        else {
            self.command_message = Some("Selected path is outside the Explorer root".into());
            cx.notify();
            return;
        };

        let detail = format!("Delete {}?", path.display());
        let answer = window.prompt(
            PromptLevel::Warning,
            "Delete Explorer item",
            Some(&detail),
            &[PromptButton::ok("Delete"), PromptButton::cancel("Cancel")],
            cx,
        );
        let recursive_prompt = (path.is_dir() && dir_is_non_empty(&path)).then(|| {
            (
                "Delete non-empty directory".to_owned(),
                format!(
                    "{} and all of its contents will be permanently deleted.",
                    path.display()
                ),
            )
        });
        let window_handle = window.window_handle();
        self.command_message = Some("Waiting for delete confirmation…".into());
        cx.spawn(async move |workspace, cx| {
            let first_confirmed = matches!(answer.await, Ok(0));
            let confirmed = match (first_confirmed, recursive_prompt) {
                (true, Some((title, detail))) => {
                    let second = cx.update_window(window_handle, |_, window, cx| {
                        window.prompt(
                            PromptLevel::Warning,
                            &title,
                            Some(&detail),
                            &[
                                PromptButton::ok("Delete all"),
                                PromptButton::cancel("Cancel"),
                            ],
                            cx,
                        )
                    });
                    match second {
                        Ok(second) => matches!(second.await, Ok(0)),
                        Err(_) => false,
                    }
                }
                (true, None) => true,
                (false, _) => false,
            };
            let _ = workspace.update(cx, |workspace, cx| {
                if !confirmed {
                    workspace.command_message = Some("Delete canceled".into());
                    cx.notify();
                    return;
                }
                match workspace.explorer_tree.delete(&relative) {
                    Ok(()) => {
                        workspace.close_tabs_for_deleted_path(&path);
                        workspace.explorer_tree.clear_selection();
                        workspace.command_message = Some(format!("Deleted {}", path.display()));
                        workspace.refresh_workspace_data(cx);
                    }
                    Err(error) => {
                        workspace.command_message = Some(format!("Delete failed: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_tabs_for_deleted_path(&mut self, deleted: &Path) {
        let removed = self
            .model
            .tabs
            .iter()
            .filter(|tab| {
                matches!(tab.kind, TabKind::Editor | TabKind::Markdown)
                    && tab
                        .resource
                        .as_deref()
                        .map(Path::new)
                        .is_some_and(|path| path == deleted || path.starts_with(deleted))
            })
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return;
        }
        self.model.tabs.retain(|tab| !removed.contains(&tab.id));
        self.tabs.retain(|tab| !removed.contains(&tab.id));
        if self.model.active.is_some_and(|id| removed.contains(&id)) {
            self.model.active = self.model.tabs.last().map(|tab| tab.id);
        }
    }

    fn start_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_resizing = true;
        self.explorer_context_menu = None;
        if event.click_count >= 2 {
            self.model.sidebar_width = DEFAULT_SIDEBAR_WIDTH;
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn resize_sidebar(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_resizing {
            self.model.sidebar_width =
                f32::from(event.position.x).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
            cx.notify();
        }
        if let Some(resize) = self.pane_resizing.clone() {
            let delta = match resize.direction {
                SplitDirection::Right => f32::from(event.position.x - resize.start_position.x),
                SplitDirection::Down => f32::from(event.position.y - resize.start_position.y),
            };
            let ratio = (resize.start_ratio + delta / resize.extent).clamp(0.1, 0.9);
            if let Some(tab) = self.model.active_tab_mut() {
                let _ = tab.layout.resize_split(&resize.path, ratio);
                tab.state_generation = tab.state_generation.saturating_add(1);
            }
            cx.notify();
        }
    }

    fn start_pane_resize(
        &mut self,
        path: Vec<usize>,
        direction: SplitDirection,
        ratio: f32,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let extent = match direction {
            SplitDirection::Right => f32::from(viewport.width),
            SplitDirection::Down => f32::from(viewport.height),
        }
        .max(1.0);
        self.pane_resizing = Some(PaneResizeState {
            path,
            direction,
            start_position: event.position,
            start_ratio: ratio,
            extent,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn stop_sidebar_resize(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_resizing || self.pane_resizing.is_some() {
            self.sidebar_resizing = false;
            self.pane_resizing = None;
            self.persist_workspace();
            cx.notify();
        }
    }

    fn persist_workspace(&self) {
        if let Some(dir) = &self.data_dir {
            if let Ok(raw) = serde_json::to_string_pretty(&self.model) {
                let _ = atomic_write(&dir.join("Termior-workspaces.json"), &raw);
            }
        }
    }

    fn select_theme(&mut self, theme_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(theme_index) = self.themes.iter().position(|theme| theme.id == theme_id) else {
            return;
        };
        self.system_is_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        self.theme_index = theme_index;
        self.settings.theme_id = self.themes[theme_index].id.clone();
        self.palette =
            self.themes[theme_index].resolve(self.resolved_appearance(), self.system_is_dark);
        self.theme_menu_open = false;
        self.propagate_palette(cx);
        cx.notify();
    }

    fn apply_theme_preferences(
        &mut self,
        theme: &Theme,
        editor_theme_id: &str,
        appearance: termior_store::settings::Appearance,
        cx: &mut Context<Self>,
    ) {
        self.settings.theme_id = theme.id.clone();
        self.settings.editor_theme_id = editor_theme_id.to_owned();
        self.settings.appearance = appearance;
        self.theme_index = if let Some(index) = self
            .themes
            .iter()
            .position(|candidate| candidate.id == theme.id)
        {
            self.themes[index] = theme.clone();
            index
        } else {
            self.themes.push(theme.clone());
            self.themes.len() - 1
        };
        self.palette =
            self.themes[self.theme_index].resolve(self.resolved_appearance(), self.system_is_dark);
        self.propagate_palette(cx);
        let (completer, completion_enabled) = self.completion_config();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor.set_preferences(
                            editor_theme_id,
                            self.settings.vim_mode,
                            completer.clone(),
                            completion_enabled,
                        )
                    });
                }
            }
        }
        cx.notify();
    }

    /// 设置中的外观三态 → 主题解析用的外观。
    fn resolved_appearance(&self) -> Appearance {
        match self.settings.appearance {
            termior_store::settings::Appearance::Light => Appearance::Light,
            termior_store::settings::Appearance::Dark => Appearance::Dark,
            termior_store::settings::Appearance::FollowSystem => Appearance::FollowSystem,
        }
    }

    /// 主题变更后同步全局色板并推送到所有已打开的终端(终端 16 色随主题走)。
    fn propagate_palette(&mut self, cx: &mut Context<Self>) {
        ui::set_palette(cx, self.palette.clone());
        let palette = self.palette.clone();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Terminal(terminal) = pane {
                    terminal.update(cx, |terminal, cx| terminal.set_palette(palette.clone(), cx));
                }
            }
        }
    }

    fn open_settings(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_window(cx);
    }

    fn open_settings_window(&self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let migration_error = self.migration_error.clone();
        let data_dir = self.data_dir.clone();
        let workspace = cx.entity().downgrade();
        let preview_workspace = workspace.clone();
        let on_save = Box::new(move |settings: &Settings, app: &mut gpui::App| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(app, |workspace, cx| {
                    workspace.apply_settings(settings.clone(), cx);
                    cx.notify();
                });
            }
        });
        let on_theme_preview = Box::new(
            move |theme: &Theme,
                  editor_theme_id: &str,
                  appearance: termior_store::settings::Appearance,
                  app: &mut gpui::App| {
                if let Some(workspace) = preview_workspace.upgrade() {
                    workspace.update(app, |workspace, cx| {
                        workspace.apply_theme_preferences(theme, editor_theme_id, appearance, cx);
                    });
                }
            },
        );
        let bounds = Bounds::centered(None, size(px(760.0), px(520.0)), cx);
        let _ = cx.open_window(
            app_identity::window_options(WindowBounds::Windowed(bounds)),
            |window, cx| {
                // 接管设置窗口的关闭流程。默认情况下点击标题栏关闭按钮会走
                // Win32 `DefWindowProcW(WM_CLOSE)` → `DestroyWindow`,随后 GPUI 在
                // `WindowsWindow::drop` 里又对同一 HWND 调一次 `DestroyWindow`,
                // 在已销毁的句柄上触发 `window not found` / `无效的窗口句柄`
                // (主窗口关闭时这些日志会被随后的 `cx.quit()` 吞掉,但关闭子窗口时
                // 应用仍存活,错误就暴露出来了)。
                //
                // 这里返回 `false` 阻止 Win32 自行销毁窗口,与 Zed 自身
                // (`zed/src/zed.rs`) 关闭子窗口的做法一致:改由 GPUI 的 `remove_window`
                // 走它自己的清理路径——`WindowsWindow::drop` 唯一一次 `DestroyWindow`,
                // 避免对死句柄的二次操作。
                //
                // 必须在回调里**同步**调用 `remove_window`:该回调运行在 GPUI 的
                // `update_window` 内部,返回后 `update_window_id` 的 `trail` 会立即
                // 检查 `window.removed` 并在同一帧移除窗口。若用 `cx.spawn` 异步执行,
                // 窗口会多存活到下一个 vsync,期间 `on_request_frame` 仍会对已标记
                // removed 的窗口 `handle.update().log_err()`,打出一条 `window not found`。
                window.on_window_should_close(cx, |window, _cx| {
                    window.remove_window();
                    false
                });
                cx.new(|cx| {
                    SettingsView::new(
                        settings,
                        migration_error,
                        data_dir,
                        Some(on_save),
                        Some(on_theme_preview),
                        cx,
                    )
                })
            },
        );
    }

    fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings = settings;
        if let Some(library) = self.data_dir.as_ref().and_then(|dir| {
            termior_store::DataFiles::new(dir)
                .themes::<ThemeLibrary>()
                .load()
                .ok()
        }) {
            self.themes = library.all();
        }
        self.notification_router
            .set_enabled(self.settings.agent_notifications);
        self.theme_index = self
            .themes
            .iter()
            .position(|theme| theme.id == self.settings.theme_id)
            .unwrap_or(0);
        self.palette =
            self.themes[self.theme_index].resolve(self.resolved_appearance(), self.system_is_dark);
        self.propagate_palette(cx);
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
        let (completer, completion_enabled) = self.completion_config();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor.set_preferences(
                            &self.settings.editor_theme_id,
                            self.settings.vim_mode,
                            completer.clone(),
                            completion_enabled,
                        )
                    });
                } else if let PaneContent::Terminal(terminal) = pane {
                    terminal.update(cx, |terminal, cx| {
                        terminal.set_settings(&self.settings.terminal, &self.settings.keymap, cx)
                    });
                }
            }
        }
        self.composer.update(cx, |composer, cx| {
            composer.configure(
                &self.settings,
                &self.model.root,
                self.workspace_auth.clone(),
                self.data_dir.clone(),
                cx,
            );
        });
    }

    fn begin_command(&mut self, mode: CommandMode, cx: &mut Context<Self>) {
        self.command_mode = mode;
        self.command_input.clear();
        self.command_marked_text.clear();
        self.command_message = None;
        if mode == CommandMode::SearchContent {
            self.content_matches.clear();
            self.content_search_generation = self.content_search_generation.saturating_add(1);
            self.content_searching = false;
        }
        self.pending_name_parent = None;
        self.pending_rename_target = None;
        self.explorer_context_menu = None;
        cx.notify();
    }

    fn begin_name_command(
        &mut self,
        mode: CommandMode,
        parent: PathBuf,
        target: Option<PathBuf>,
        prefill: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_mode = mode;
        self.command_input = prefill.to_owned();
        self.command_marked_text.clear();
        self.command_message = Some(match mode {
            CommandMode::CreateFile => "Type a file name, then press Enter".into(),
            CommandMode::CreateDirectory => "Type a directory name, then press Enter".into(),
            CommandMode::Rename => "Edit the name, then press Enter".into(),
            _ => String::new(),
        });
        self.pending_name_parent = Some(parent);
        self.pending_rename_target = target;
        self.explorer_context_menu = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn handle_command_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.command_mode == CommandMode::Browse {
            return false;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                self.command_mode = CommandMode::Browse;
                self.command_input.clear();
                self.command_marked_text.clear();
                self.content_matches.clear();
                self.content_search_generation = self.content_search_generation.saturating_add(1);
                self.content_searching = false;
                self.pending_name_parent = None;
                self.pending_rename_target = None;
            }
            "backspace" => {
                self.command_marked_text.clear();
                self.command_input.pop();
                if self.command_mode == CommandMode::SearchContent {
                    self.content_matches.clear();
                }
            }
            "enter" | "return" => self.execute_command(window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    fn execute_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.command_input.trim().to_owned();
        if input.is_empty() {
            return;
        }
        if self.command_mode == CommandMode::SearchContent {
            self.start_content_search(input, cx);
            return;
        }
        let completed_mode = self.command_mode;
        let result = match self.command_mode {
            CommandMode::FindFile => {
                let path = self.explorer.as_ref().and_then(|index| {
                    index
                        .fuzzy(&input, 1)
                        .first()
                        .map(|hit| index.root().join(&hit.path))
                });
                if let Some(path) = path {
                    self.open_editor(path, cx);
                    Ok(())
                } else {
                    Err("No matching file".to_owned())
                }
            }
            CommandMode::SearchContent => {
                unreachable!("content search starts before command dispatch")
            }
            CommandMode::CreateFile => {
                let result = if let Some(parent) = self.pending_name_parent.as_ref() {
                    parent
                        .strip_prefix(self.explorer_tree.root())
                        .map_err(|error| error.to_string())
                        .and_then(|relative| {
                            self.explorer_tree
                                .create_file_in(relative, &input)
                                .map_err(|error| error.to_string())
                        })
                } else {
                    self.explorer_tree
                        .create_file(&input)
                        .map_err(|error| error.to_string())
                };
                result.map(|path| {
                    self.explorer_tree.select(path.clone());
                    self.open_editor(path, cx);
                })
            }
            CommandMode::CreateDirectory => {
                let result = if let Some(parent) = self.pending_name_parent.as_ref() {
                    parent
                        .strip_prefix(self.explorer_tree.root())
                        .map_err(|error| error.to_string())
                        .and_then(|relative| {
                            self.explorer_tree
                                .create_directory_in(relative, &input)
                                .map_err(|error| error.to_string())
                        })
                } else {
                    self.explorer_tree
                        .create_directory(&input)
                        .map_err(|error| error.to_string())
                };
                result.map(|path| self.explorer_tree.select(path))
            }
            CommandMode::Rename => {
                let selected = self
                    .pending_rename_target
                    .clone()
                    .or_else(|| self.explorer_tree.selected().map(Path::to_path_buf));
                match selected {
                    Some(path) if self.has_dirty_editor_under(&path, cx) => {
                        Err("Save open files under this path before renaming".to_owned())
                    }
                    Some(path) => {
                        let relative = path
                            .strip_prefix(self.explorer_tree.root())
                            .map(Path::to_path_buf)
                            .map_err(|error| error.to_string());
                        relative.and_then(|relative| {
                            self.explorer_tree
                                .rename(relative, &input)
                                .map_err(|error| error.to_string())
                                .map(|renamed| {
                                    self.retarget_open_editors(&path, &renamed, cx);
                                    self.explorer_tree.select(renamed);
                                })
                        })
                    }
                    None => Err("Select a file or directory first".to_owned()),
                }
            }
            CommandMode::GitCommit => self.git_repository().and_then(|repo| {
                repo.commit(&input)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
            CommandMode::GitCreateBranch => self.git_repository().and_then(|repo| {
                repo.create_branch(&input, true)
                    .map_err(|error| error.to_string())
            }),
            CommandMode::GitSwitchBranch => self.git_repository().and_then(|repo| {
                repo.switch_branch(&input)
                    .map_err(|error| error.to_string())
            }),
            CommandMode::PreviewUrl => normalize_preview_url(&input)
                .map_err(|error| error.to_string())
                .map(|url| self.create_preview_url(url, window, cx)),
            CommandMode::Browse => Ok(()),
        };
        match result {
            Ok(()) => {
                if completed_mode == CommandMode::SearchContent {
                    self.command_message = Some(match self.content_matches.len() {
                        0 => "No content matches".into(),
                        1 => "1 content match".into(),
                        count => format!("{count} content matches"),
                    });
                    self.command_marked_text.clear();
                } else {
                    self.command_message = Some("Done".into());
                    self.command_mode = CommandMode::Browse;
                    self.command_input.clear();
                    self.command_marked_text.clear();
                    self.pending_name_parent = None;
                    self.pending_rename_target = None;
                }
                if matches!(
                    completed_mode,
                    CommandMode::CreateFile
                        | CommandMode::CreateDirectory
                        | CommandMode::Rename
                        | CommandMode::GitCommit
                        | CommandMode::GitCreateBranch
                        | CommandMode::GitSwitchBranch
                ) {
                    self.refresh_workspace_data(cx);
                }
            }
            Err(error) => self.command_message = Some(error),
        }
    }

    fn start_content_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.content_search_generation = self.content_search_generation.saturating_add(1);
        let generation = self.content_search_generation;
        self.content_matches.clear();
        self.content_searching = true;
        self.command_message = Some("Searching workspace…".into());
        let paths = self
            .explorer
            .as_ref()
            .map(|index| {
                index
                    .entries()
                    .iter()
                    .filter(|entry| !entry.is_dir)
                    .map(|entry| entry.path.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<ContentMatch>();
        let search = cx.background_executor().spawn(async move {
            let mut sent = 0usize;
            ContentSearch::default()
                .search_paths(&query, paths, |hit| {
                    sent += 1;
                    sender.unbounded_send(hit).is_ok() && sent < 200
                })
                .map_err(|error| error.to_string())
        });
        cx.spawn(async move |workspace, cx| {
            while let Some(hit) = receiver.next().await {
                let accepted = workspace
                    .update(cx, |workspace, cx| {
                        if generation != workspace.content_search_generation {
                            return false;
                        }
                        workspace.content_matches.push(hit);
                        workspace.command_message = Some(format!(
                            "Searching… {} match{}",
                            workspace.content_matches.len(),
                            if workspace.content_matches.len() == 1 {
                                ""
                            } else {
                                "es"
                            }
                        ));
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !accepted {
                    break;
                }
            }
            let result = search.await;
            let _ = workspace.update(cx, |workspace, cx| {
                if generation != workspace.content_search_generation {
                    return;
                }
                workspace.content_searching = false;
                workspace.command_message = Some(match result {
                    Ok(_) if workspace.content_matches.is_empty() => "No content matches".into(),
                    Ok(_) if workspace.content_matches.len() == 1 => "1 content match".into(),
                    Ok(_) => format!("{} content matches", workspace.content_matches.len()),
                    Err(error) => error,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn git_repository(&self) -> Result<GitRepository, String> {
        GitRepository::open(&self.model.root, &self.workspace_auth)
            .map_err(|error| error.to_string())
    }

    fn stage_all(&mut self, cx: &mut Context<Self>) {
        let result = self.git_repository().and_then(|repo| {
            for file in self
                .vcs_status
                .iter()
                .filter(|file| file.group != ChangeGroup::Staged)
            {
                repo.stage_file(&file.path)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        });
        self.command_message = result.err();
        self.refresh_workspace_data(cx);
        cx.notify();
    }

    fn run_remote(&mut self, operation: RemoteOperation, cx: &mut Context<Self>) {
        let root = self.model.root.clone();
        let auth = self.workspace_auth.clone();
        let task = cx.background_executor().spawn(async move {
            GitRepository::open(root, &auth).and_then(|repo| repo.remote(operation))
        });
        self.command_message = Some(format!("Running {operation:?}…"));
        cx.spawn(async move |workspace, cx| {
            let result = task.await;
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.command_message = Some(match result {
                    Ok(output) if output.trim().is_empty() => format!("{operation:?} completed"),
                    Ok(output) => output.trim().to_owned(),
                    Err(error) => error.to_string(),
                });
                workspace.refresh_workspace_data(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_explorer_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.command_mode != CommandMode::Browse
            || self.model.sidebar_panel != SidebarPanel::Explorer
            || !self.model.sidebar_visible
            || !self.focus_handle.is_focused(window)
        {
            return false;
        }
        let mut entries = self
            .explorer
            .as_ref()
            .map(|index| {
                index
                    .entries()
                    .iter()
                    .filter(|entry| explorer_entry_visible(entry, &self.explorer_tree))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| a.relative.cmp(&b.relative));
        if entries.is_empty() {
            return false;
        }
        let selected = self
            .explorer_tree
            .selected()
            .and_then(|path| entries.iter().position(|entry| entry.path == path))
            .unwrap_or(0);
        match event.keystroke.key.as_str() {
            "up" => self
                .explorer_tree
                .select(entries[selected.saturating_sub(1)].path.clone()),
            "down" => self
                .explorer_tree
                .select(entries[(selected + 1).min(entries.len() - 1)].path.clone()),
            "home" => self.explorer_tree.select(entries[0].path.clone()),
            "end" => self
                .explorer_tree
                .select(entries.last().expect("entries is non-empty").path.clone()),
            "right" => {
                let entry = &entries[selected];
                if entry.is_dir && !self.explorer_tree.is_expanded(&entry.path) {
                    self.explorer_tree.toggle_expanded(entry.path.clone());
                } else if entry.is_dir {
                    if let Some(child) = entries
                        .iter()
                        .skip(selected + 1)
                        .find(|child| child.path.parent() == Some(entry.path.as_path()))
                    {
                        self.explorer_tree.select(child.path.clone());
                    }
                }
            }
            "left" => {
                let entry = &entries[selected];
                if entry.is_dir && self.explorer_tree.is_expanded(&entry.path) {
                    self.explorer_tree.toggle_expanded(entry.path.clone());
                } else if let Some(parent) = entry.path.parent() {
                    if let Some(parent_entry) = entries.iter().find(|item| item.path == parent) {
                        self.explorer_tree.select(parent_entry.path.clone());
                    }
                }
            }
            "enter" | "return" | "space" => {
                let entry = entries[selected].clone();
                if entry.is_dir {
                    self.explorer_tree.toggle_expanded(entry.path);
                } else {
                    self.open_editor(entry.path, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn global_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.handle_command_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.handle_explorer_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        let Some(action) = self.configured_action(event) else {
            return;
        };
        match action {
            KeyAction::NewTerminalTab => self.create_terminal(false, window, cx),
            KeyAction::NewPrivateTerminal => self.create_terminal(true, window, cx),
            KeyAction::NewEditorTab => self.create_editor(cx),
            KeyAction::NewPreviewTab => self.request_preview(window, cx),
            KeyAction::ClosePaneOrTab => self.close_active(cx),
            KeyAction::GotoTab1 => {
                let _ = self.model.switch_index(1);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                    self.focus_active_pane(window, cx);
                }
            }
            KeyAction::CycleTabs | KeyAction::CycleTabsReverse => {
                self.model.cycle_tab(action == KeyAction::CycleTabsReverse);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                    self.focus_active_pane(window, cx);
                }
            }
            KeyAction::SplitRight => self.split_active(SplitDirection::Right, window, cx),
            KeyAction::SplitDown => self.split_active(SplitDirection::Down, window, cx),
            KeyAction::FocusPanePrev | KeyAction::FocusPaneNext => {
                if let Some(tab) = self.model.active_tab_mut() {
                    tab.layout
                        .focus_relative(if action == KeyAction::FocusPanePrev {
                            -1
                        } else {
                            1
                        });
                }
                self.focus_active_pane(window, cx);
            }
            KeyAction::InlineSearch => {
                if let Some(editor) = self.active_editor().cloned() {
                    editor.update(cx, EditorView::open_search);
                } else if let Some(terminal) = self.active_terminal().cloned() {
                    terminal.update(cx, TerminalView::open_search);
                }
            }
            KeyAction::ToggleSidebar => {
                self.model.sidebar_visible = !self.model.sidebar_visible;
            }
            KeyAction::FocusExplorer => {
                self.model.sidebar_panel = SidebarPanel::Explorer;
                self.model.sidebar_visible = true;
                window.focus(&self.focus_handle, cx);
            }
            KeyAction::FileFinder => {
                self.model.sidebar_panel = SidebarPanel::Explorer;
                self.model.sidebar_visible = true;
                self.begin_command(CommandMode::FindFile, cx);
            }
            KeyAction::SourceControlPanel => {
                self.model.sidebar_panel = SidebarPanel::SourceControl;
                self.model.sidebar_visible = true;
            }
            KeyAction::ToggleComposer => {
                self.model.composer_visible = !self.model.composer_visible;
            }
            KeyAction::AskAiAboutSelection => self.attach_active_selection(cx),
            KeyAction::CommitStaged => {
                self.model.sidebar_panel = SidebarPanel::SourceControl;
                self.model.sidebar_visible = true;
                self.begin_command(CommandMode::GitCommit, cx);
            }
            KeyAction::OpenSettings => self.open_settings_window(cx),
            KeyAction::Undo | KeyAction::Redo => {
                if let Some(editor) = self.active_editor().cloned() {
                    if action == KeyAction::Undo {
                        editor.update(cx, EditorView::undo);
                    } else {
                        editor.update(cx, EditorView::redo);
                    }
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn configured_action(&self, event: &KeyDownEvent) -> Option<KeyAction> {
        let modifiers = event.keystroke.modifiers;
        self.settings
            .keymap
            .bindings
            .iter()
            .find_map(|(action, binding)| {
                let primary_matches = if binding.primary {
                    if cfg!(target_os = "macos") {
                        modifiers.platform && !modifiers.control
                    } else {
                        modifiers.control
                    }
                } else {
                    modifiers.control && !modifiers.platform
                };
                (primary_matches
                    && modifiers.shift == binding.shift
                    && modifiers.alt == binding.alt
                    && event.keystroke.key.eq_ignore_ascii_case(&binding.key))
                .then_some(*action)
            })
    }

    fn layout_element(
        &self,
        node: &LayoutNode,
        panes: &HashMap<PaneId, PaneContent>,
        focused: PaneId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.layout_element_at(node, panes, focused, Vec::new(), cx)
    }

    fn layout_element_at(
        &self,
        node: &LayoutNode,
        panes: &HashMap<PaneId, PaneContent>,
        focused: PaneId,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let workspace_accent = self.palette.accent;
        match node {
            LayoutNode::Pane { id } => {
                let pane_id = *id;
                let content = panes.get(id).map(PaneContent::element).unwrap_or_else(|| {
                    div()
                        .flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .child("Pane unavailable")
                        .into_any_element()
                });
                div()
                    .id(SharedString::from(format!("pane-{}", id.0)))
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .when(*id == focused, |pane| {
                        pane.border_1().border_color(gpui_color(workspace_accent))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, window, cx| {
                            if let Some(tab) = workspace.model.active_tab_mut() {
                                let _ = tab.layout.focus(pane_id);
                            }
                            workspace.focus_active_pane(window, cx);
                            cx.notify();
                        }),
                    )
                    .child(content)
                    .into_any_element()
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let mut first_path = path.clone();
                first_path.push(0);
                let mut second_path = path.clone();
                second_path.push(1);
                let first = self.layout_element_at(first, panes, focused, first_path, cx);
                let second = self.layout_element_at(second, panes, focused, second_path, cx);
                let ratio = ratio.clamp(0.1, 0.9);
                let resize_path = path.clone();
                let resize_direction = *direction;
                let resize_handle = div()
                    .id(SharedString::from(format!(
                        "pane-divider-{}",
                        path.iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join("-")
                    )))
                    .flex_shrink_0()
                    .bg(ui::border(&self.palette))
                    .hover({
                        let accent = gpui_color(self.palette.accent);
                        move |style| style.bg(accent)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, event, window, cx| {
                            workspace.start_pane_resize(
                                resize_path.clone(),
                                resize_direction,
                                ratio,
                                event,
                                window,
                                cx,
                            )
                        }),
                    );
                match direction {
                    SplitDirection::Right => div()
                        .flex()
                        .flex_row()
                        .size_full()
                        .child(
                            div()
                                .h_full()
                                .w(relative(ratio))
                                .flex_shrink_0()
                                .child(first),
                        )
                        .child(
                            resize_handle
                                .w(px(5.0))
                                .h_full()
                                .cursor(CursorStyle::ResizeColumn),
                        )
                        .child(div().h_full().flex_1().child(second))
                        .into_any_element(),
                    SplitDirection::Down => div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(
                            div()
                                .w_full()
                                .h(relative(ratio))
                                .flex_shrink_0()
                                .child(first),
                        )
                        .child(
                            resize_handle
                                .w_full()
                                .h(px(5.0))
                                .cursor(CursorStyle::ResizeRow),
                        )
                        .child(div().w_full().flex_1().child(second))
                        .into_any_element(),
                }
            }
        }
    }

    fn sidebar_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let command_bar = (self.command_mode != CommandMode::Browse).then(|| {
            div()
                .px_2()
                .py_1()
                .mb_1()
                .rounded_md()
                .border_1()
                .border_color(gpui_color(self.palette.accent))
                .bg(gpui_color(self.palette.surface[1]))
                .text_xs()
                .child(SharedString::from(format!(
                    "{}: {}{}▏",
                    self.command_mode.label(),
                    self.command_input,
                    if self.command_marked_text.is_empty() {
                        ""
                    } else {
                        "…"
                    }
                )))
        });
        let message = self.command_message.clone().map(|message| {
            div()
                .px_2()
                .py_1()
                .text_xs()
                .child(SharedString::from(message))
        });
        let content = match self.model.sidebar_panel {
            SidebarPanel::Explorer => {
                let toolbar = div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .mb_1()
                    .child(sidebar_button(
                        "Find",
                        "explorer-find",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::FindFile, cx),
                    ))
                    .child(sidebar_button(
                        "Search",
                        "explorer-search",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::SearchContent, cx),
                    ))
                    .child(sidebar_button(
                        "+F",
                        "explorer-new-file",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::CreateFile, cx),
                    ))
                    .child(sidebar_button(
                        "+D",
                        "explorer-new-dir",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::CreateDirectory, cx),
                    ))
                    .child(sidebar_button(
                        "↻",
                        "explorer-refresh",
                        &self.palette,
                        cx,
                        |this, cx| this.refresh_workspace_data(cx),
                    ));

                let root_label = self
                    .explorer_requested_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.explorer_requested_root.display().to_string());
                let active_path = self
                    .model
                    .active_tab()
                    .and_then(|tab| tab.resource.as_deref())
                    .map(PathBuf::from);
                let icon_surface = gpui_color(self.palette.surface[1]);
                let icon_panel = gpui_color(self.palette.surface[2]);
                let (visible_entries, visible_entry_count) = self
                    .explorer
                    .as_ref()
                    .map(|index| {
                        let mut entries = index
                            .entries()
                            .iter()
                            .filter(|entry| explorer_entry_visible(entry, &self.explorer_tree))
                            .collect::<Vec<_>>();
                        entries.sort_by(|a, b| a.relative.cmp(&b.relative));
                        let count = entries.len();
                        let visible = entries.into_iter().take(500).cloned().collect::<Vec<_>>();
                        (visible, count)
                    })
                    .unwrap_or_else(|| (Vec::new(), 0));
                let tree_content_width = explorer_content_width(&visible_entries);

                let rows = if self.command_mode == CommandMode::FindFile {
                    self.explorer
                        .as_ref()
                        .map(|index| {
                            index
                                .fuzzy(&self.command_input, 80)
                                .into_iter()
                                .map(|hit| {
                                    let path = index.root().join(&hit.path);
                                    div()
                                        .id(SharedString::from(format!("find-{}", hit.path)))
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(SharedString::from(hit.path))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.open_editor(path.clone(), cx)
                                            }),
                                        )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                } else if self.command_mode == CommandMode::SearchContent {
                    self.content_matches
                        .iter()
                        .map(|hit| {
                            let path = hit.path.clone();
                            div()
                                .id(SharedString::from(format!(
                                    "search-{}-{}",
                                    path.display(),
                                    hit.line_number
                                )))
                                .px_2()
                                .py_1()
                                .text_xs()
                                .cursor_pointer()
                                .child(SharedString::from(format!(
                                    "{}:{}  {}",
                                    path.strip_prefix(&self.explorer_requested_root)
                                        .unwrap_or(&path)
                                        .display(),
                                    hit.line_number,
                                    hit.line
                                )))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.open_editor(path.clone(), cx)
                                    }),
                                )
                        })
                        .collect()
                } else {
                    visible_entries
                        .into_iter()
                        .map(|entry| {
                            let path = entry.path.clone();
                            let right_path = path.clone();
                            let selected = self.explorer_tree.selected() == Some(path.as_path());
                            let active = active_path.as_ref() == Some(&path);
                            let expanded = self.explorer_tree.is_expanded(&path);
                            let is_dir = entry.is_dir;
                            let icon = entry.icon;
                            let label = entry
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or(&entry.relative)
                                .to_owned();
                            div()
                                .id(SharedString::from(format!("file-{}", entry.relative)))
                                .ml(px(entry.depth as f32 * 12.0))
                                .w_full()
                                .min_w(px(tree_content_width))
                                .px_1()
                                .py(px(2.0))
                                .rounded_sm()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |row| row.bg(ui::selected_wash(&self.palette)))
                                .when(active, |row| row.bg(ui::alpha(self.palette.accent, 0.35)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .whitespace_nowrap()
                                        .child(explorer_icon(
                                            icon,
                                            expanded,
                                            icon_surface,
                                            icon_panel,
                                        ))
                                        .child(SharedString::from(label)),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _event, window, cx| {
                                        this.explorer_context_menu = None;
                                        this.explorer_tree.select(path.clone());
                                        if is_dir {
                                            this.explorer_tree.toggle_expanded(path.clone());
                                            window.focus(&this.focus_handle, cx);
                                        } else {
                                            this.open_editor(path.clone(), cx);
                                        }
                                        cx.notify();
                                    }),
                                )
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.show_explorer_context_menu(
                                            if is_dir {
                                                ExplorerContextTarget::Directory(right_path.clone())
                                            } else {
                                                ExplorerContextTarget::File(right_path.clone())
                                            },
                                            event.position,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                        })
                        .collect::<Vec<_>>()
                };
                let background_target = ExplorerContextTarget::Workspace;
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(toolbar)
                    .child(
                        div()
                            .px_1()
                            .pb_1()
                            .text_xs()
                            .text_color(ui::muted(&self.palette))
                            .child(SharedString::from(format!(
                                "{}{}",
                                root_label,
                                if self.explorer_loading {
                                    "  · indexing…"
                                } else if visible_entry_count > 500 {
                                    "  · showing first 500"
                                } else {
                                    ""
                                }
                            ))),
                    )
                    .children(self.explorer_error.clone().map(|error| {
                        div()
                            .px_1()
                            .py_1()
                            .text_xs()
                            .text_color(gpui_color(self.palette.status[3]))
                            .child(SharedString::from(error))
                    }))
                    .child(
                        div()
                            .id("explorer-scroll")
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_x_scroll()
                            .overflow_y_scroll()
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.show_explorer_context_menu(
                                        background_target.clone(),
                                        event.position,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                            .children(rows)
                            .children(
                                (self.explorer.is_some()
                                    && !self.explorer_loading
                                    && self
                                        .explorer
                                        .as_ref()
                                        .is_some_and(|index| index.entries().is_empty()))
                                .then(|| {
                                    div()
                                        .p_2()
                                        .text_xs()
                                        .text_color(ui::muted(&self.palette))
                                        .child("This workspace has no visible files")
                                }),
                            ),
                    )
                    .into_any_element()
            }
            SidebarPanel::SourceControl => {
                let branch = self
                    .vcs_branch
                    .as_ref()
                    .and_then(|state| state.name.clone())
                    .unwrap_or_else(|| "No repository".into());
                let toolbar = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .mb_1()
                    .child(sidebar_button(
                        "All+",
                        "git-stage-all",
                        &self.palette,
                        cx,
                        |this, cx| this.stage_all(cx),
                    ))
                    .child(sidebar_button(
                        "Commit",
                        "git-commit",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitCommit, cx),
                    ))
                    .child(sidebar_button(
                        "Fetch",
                        "git-fetch",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::Fetch, cx),
                    ))
                    .child(sidebar_button(
                        "Pull",
                        "git-pull",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::PullFfOnly, cx),
                    ))
                    .child(sidebar_button(
                        "Push",
                        "git-push",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::Push, cx),
                    ))
                    .child(sidebar_button(
                        "+Branch",
                        "git-new-branch",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitCreateBranch, cx),
                    ))
                    .child(sidebar_button(
                        "Switch",
                        "git-switch-branch",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitSwitchBranch, cx),
                    ));
                let rows = self
                    .vcs_status
                    .iter()
                    .take(80)
                    .map(|file| {
                        let path = file.path.clone();
                        let group = file.group;
                        div()
                            .id(SharedString::from(format!("git-{group:?}-{path}")))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .child(SharedString::from(format!(
                                "{:?}  {}",
                                file.group, file.path
                            )))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.open_git_diff(path.clone(), group, cx)
                                }),
                            )
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .child(SharedString::from(format!("Branch: {branch}")))
                    .child(toolbar)
                    .children(rows)
                    .into_any_element()
            }
            SidebarPanel::GitHistory => {
                let rows = self
                    .vcs_history
                    .iter()
                    .take(60)
                    .map(|commit| {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .child(SharedString::from(format!(
                                "{}  {}",
                                &commit.id[..7.min(commit.id.len())],
                                commit.summary
                            )))
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .child(sidebar_button(
                        "Open full history",
                        "git-open-history",
                        &self.palette,
                        cx,
                        |this, cx| this.open_git_history(cx),
                    ))
                    .children(rows)
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .children(command_bar)
            .children(message)
            .child(content)
            .into_any_element()
    }

    fn sync_terminal_context(&mut self, cx: &mut Context<Self>) -> (String, Option<String>) {
        let terminal = self.active_terminal().cloned();
        let cwd = terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).latest_cwd().map(str::to_owned));
        let preview = terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).localhost_urls().last().cloned());
        let recent_output = terminal
            .as_ref()
            .map(|terminal| terminal.read(cx).recent_text())
            .unwrap_or_default();
        if let Some(cwd) = &cwd {
            self.model.set_active_cwd(cwd);
            let path = Path::new(cwd);
            if path.is_dir() && self.explorer_requested_root != path {
                self.schedule_explorer_scan(path.to_path_buf(), cx);
            }
        }
        let resolved_cwd = cwd.unwrap_or_else(|| self.model.root.to_string_lossy().into_owned());
        self.composer.update(cx, |composer, _| {
            composer.update_terminal_context(resolved_cwd.clone(), recent_output)
        });
        (resolved_cwd, preview)
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for WorkspaceView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        if self.command_mode == CommandMode::Browse {
            return None;
        }
        let start = utf16_to_byte(&self.command_input, range_utf16.start);
        let end = utf16_to_byte(&self.command_input, range_utf16.end);
        actual_range.replace(range_utf16);
        self.command_input.get(start..end).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        (self.command_mode != CommandMode::Browse).then(|| {
            let position = self.command_input.encode_utf16().count();
            UTF16Selection {
                range: position..position,
                reversed: false,
            }
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let prefix_len = self
            .command_input
            .len()
            .saturating_sub(self.command_marked_text.len());
        let start = self.command_input[..prefix_len].encode_utf16().count();
        let len = self.command_marked_text.encode_utf16().count();
        (len > 0).then_some(start..start + len)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.command_marked_text.clear();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_mode == CommandMode::Browse {
            return;
        }
        if let Some(range) = range_utf16 {
            let start = utf16_to_byte(&self.command_input, range.start);
            let end = utf16_to_byte(&self.command_input, range.end);
            if start <= end && end <= self.command_input.len() {
                self.command_input.replace_range(start..end, text);
            } else {
                self.command_input.push_str(text);
            }
        } else {
            let keep = self
                .command_input
                .len()
                .saturating_sub(self.command_marked_text.len());
            self.command_input.truncate(keep);
            self.command_input.push_str(text);
        }
        self.command_marked_text.clear();
        if self.command_mode == CommandMode::SearchContent {
            self.content_matches.clear();
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_mode != CommandMode::Browse {
            let range = range_utf16
                .map(|range| {
                    utf16_to_byte(&self.command_input, range.start)
                        ..utf16_to_byte(&self.command_input, range.end)
                })
                .unwrap_or_else(|| {
                    self.command_input
                        .len()
                        .saturating_sub(self.command_marked_text.len())
                        ..self.command_input.len()
                });
            self.command_input.replace_range(range, new_text);
            self.command_marked_text = new_text.to_owned();
            if self.command_mode == CommandMode::SearchContent {
                self.content_matches.clear();
            }
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(
            Point::new(element_bounds.left(), element_bounds.top()),
            size(px(1.0), px(18.0)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.command_input.encode_utf16().count())
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        (self.command_mode != CommandMode::Browse)
            .then(|| self.command_input.encode_utf16().count())
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.command_mode != CommandMode::Browse
    }
}

impl WorkspaceView {
    /// 计算当前背景图层（FR-THEME-05）。渲染帧同步调用，不阻塞。
    ///
    /// - 无有效路径 → [`BackgroundLayer::None`]（优雅回退到 `palette.background` 纯色）。
    /// - `blur == 0` → [`BackgroundLayer::Path`]：直接交给 `gpui::img(path)`，走 gpui 自带资源缓存。
    /// - `blur > 0` → [`BackgroundLayer::Blurred`]：用 [`BackgroundImageCache`] 的离屏模糊纹理；
    ///   若缓存未就绪则返回 `Blurred(None)`（首帧先留白，后台解码完成后 `cx.notify` 触发重绘）。
    ///
    /// 调用方应在渲染前先调 [`Self::request_background_decode`] 触发后台解码（键变化才真正跑）。
    fn background_layer(&self) -> BackgroundLayer {
        let Some(path) = self
            .settings
            .background
            .image_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
        else {
            return BackgroundLayer::None;
        };
        let opacity = self.settings.background.opacity.clamp(0.0, 1.0);
        let blur = self.settings.background.blur.clamp(0.0, 64.0);
        if blur <= 0.0 {
            BackgroundLayer::Path { path, opacity }
        } else {
            BackgroundLayer::Blurred {
                opacity,
                texture: self.background_cache.current(),
            }
        }
    }

    /// 若背景键（路径/模糊半径）变化，启动后台解码 + 模糊，完成后回填缓存并重绘。
    /// 键不变则空操作（满足 spec「解码一次缓存」「不做逐帧后处理」）。
    fn request_background_decode(&mut self, cx: &mut Context<Self>) {
        let path = match self
            .settings
            .background
            .image_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
        {
            Some(path) => path,
            None => {
                // 无背景图：清空缓存键，下次配置了图才会解码。
                if self.background_cache.current_key().is_some() {
                    self.background_cache.begin(PathBuf::new(), 0.0);
                }
                return;
            }
        };
        let blur = self.settings.background.blur.clamp(0.0, 64.0);
        if !self.background_cache.needs(&path, blur) {
            return;
        }
        self.background_cache.begin(path.clone(), blur);
        let path_for_task = path.clone();
        // 后台线程解码 + 模糊（CPU 密集，不阻塞 UI 线程）；完成后回填缓存并重绘。
        // oneshot channel 适合「线程 → async 单次回传」；线程侧 `send` 不阻塞。
        cx.spawn(async move |this, cx| {
            let (tx, rx) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let result = crate::background_image::decode_and_blur(&path_for_task, blur);
                let _ = tx.send(result);
            });
            let result = rx.await.ok().flatten();
            this.update(cx, |this, cx| {
                let blur = this.settings.background.blur.clamp(0.0, 64.0);
                if this.background_cache.store(&path, blur, result) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// 背景图层计算结果（见 [`WorkspaceView::background_layer`]）。
enum BackgroundLayer {
    /// 无背景图：渲染层只保留纯色背景。
    None,
    /// 未模糊：直接用文件路径，由 gpui 资源缓存异步加载（现有路径）。
    Path { path: PathBuf, opacity: f32 },
    /// 已模糊：用离屏缓存的 [`gpui::RenderImage`] 纹理；`texture` 为 `None` 表示尚未解码完成。
    Blurred {
        opacity: f32,
        texture: Option<std::sync::Arc<gpui::RenderImage>>,
    },
}

/// 把 [`BackgroundLayer`] 转成可绘制的背景元素（铺满、Cover、按透明度叠加）。
/// `None` / 未就绪的 `Blurred(None)` 返回 `None`，由渲染层保留纯色背景（优雅回退）。
fn background_layer_element(layer: BackgroundLayer) -> Option<gpui::AnyElement> {
    match layer {
        BackgroundLayer::None => None,
        BackgroundLayer::Path { path, opacity } => Some(
            gpui::img(path)
                .absolute()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(opacity)
                .into_any_element(),
        ),
        BackgroundLayer::Blurred { opacity, texture } => texture.map(|image| {
            gpui::img(image)
                .absolute()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(opacity)
                .into_any_element()
        }),
    }
}

impl gpui::Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let system_is_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        if self.system_is_dark != system_is_dark {
            self.system_is_dark = system_is_dark;
            if self.settings.appearance == termior_store::settings::Appearance::FollowSystem {
                self.palette = self.themes[self.theme_index]
                    .resolve(Appearance::FollowSystem, self.system_is_dark);
                self.propagate_palette(cx);
            }
        }
        self.process_agent_updates(window, cx);
        let (cwd, preview_url) = self.sync_terminal_context(cx);
        let p = self.palette.clone();
        let markdown_preview_available = self
            .active_editor()
            .is_some_and(|editor| editor.read(cx).path().is_some_and(is_markdown_path));
        let active = self.model.active;
        let tab_buttons = self
            .model
            .tabs
            .iter()
            .map(|tab| {
                let id = tab.id;
                let selected = active == Some(id);
                let close_hover = ui::hover_wash(&p);
                div()
                    .id(SharedString::from(format!("tab-{}", id.0)))
                    .relative()
                    .h_full()
                    .pl_3()
                    .pr_2()
                    .min_w(px(110.0))
                    .max_w(px(210.0))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    // 激活 tab 与下方内容区同色（连成一体），非激活 tab 沉入标题栏底色。
                    .bg(if selected {
                        gpui_color(p.background)
                    } else {
                        gpui_color(p.surface[2])
                    })
                    .text_color(if selected {
                        gpui_color(p.foreground)
                    } else {
                        gpui_color_alpha(p.foreground, 0.55)
                    })
                    .when(!selected, |d| {
                        d.hover(move |style| style.bg(gpui_color(p.surface[1])))
                    })
                    // 激活 tab 顶部 accent 条 + tab 间细分隔线，划清每个 tab 的边界。
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(px(2.0))
                                .bg(gpui_color(p.accent)),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .right_0()
                            .top(px(10.0))
                            .bottom(px(10.0))
                            .w(px(1.0))
                            .bg(gpui_color_alpha(p.foreground, 0.12)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .child(SharedString::from(tab.title.clone())),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tab-close-{}", id.0)))
                            .flex_shrink_0()
                            .w(px(18.0))
                            .h(px(18.0))
                            .mr_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .text_xs()
                            .text_color(gpui_color_alpha(p.foreground, 0.5))
                            .hover(move |style| {
                                style.bg(close_hover).text_color(gpui_color(p.foreground))
                            })
                            .child("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _event, _window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(id, cx);
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, window, cx| {
                            this.activate_runtime(id, cx);
                            this.focus_active_pane(window, cx);
                            cx.notify();
                        }),
                    )
                    // 中键关闭：桌面端标签页的通用约定。
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _event, _window, cx| {
                            cx.stop_propagation();
                            this.close_tab(id, cx);
                        }),
                    )
            })
            .collect::<Vec<_>>();
        let new_tab_button = div()
            .id("new-tab")
            .flex_shrink_0()
            .w(px(26.0))
            .h(px(26.0))
            .my_auto()
            .ml_1()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .text_color(gpui_color_alpha(p.foreground, 0.7))
            .hover({
                let wash = ui::hover_wash(&p);
                move |style| style.bg(wash).text_color(gpui_color(p.foreground))
            })
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    this.create_terminal(false, window, cx);
                }),
            );

        let active_content = active
            .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
            .map(|tab| {
                let layout = &self.model.active_tab().expect("active tab exists").layout;
                self.layout_element(&layout.root, &tab.panes, layout.focused, cx)
            })
            .unwrap_or_else(|| {
                div()
                    .flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child("No tabs. Press Ctrl/Cmd+T for a terminal.")
                    .into_any_element()
            });

        let sidebar = if self.model.sidebar_visible {
            div()
                .flex()
                .flex_row()
                .w(px(self.model.sidebar_width))
                .h_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w(px(44.0))
                        .items_center()
                        .gap_2()
                        .pt_2()
                        .bg(gpui_color(p.surface[0]))
                        .border_r_1()
                        .border_color(ui::border(&p))
                        .children(
                            [
                                ("▱", SidebarPanel::Explorer),
                                ("⑂", SidebarPanel::SourceControl),
                                ("◷", SidebarPanel::GitHistory),
                            ]
                            .into_iter()
                            .map(|(label, panel)| {
                                let selected = self.model.sidebar_panel == panel;
                                div()
                                    .id(SharedString::from(format!("sidebar-{panel:?}")))
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(if selected {
                                        gpui_color(p.accent)
                                    } else {
                                        ui::muted(&p)
                                    })
                                    .when(selected, |button| button.bg(ui::selected_wash(&p)))
                                    .when(!selected, |button| {
                                        let wash = ui::hover_wash(&p);
                                        button.hover(move |style| style.bg(wash))
                                    })
                                    .child(label)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.model.sidebar_panel = panel;
                                            this.model.sidebar_visible = true;
                                            cx.notify();
                                        }),
                                    )
                            }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .p_2()
                        .child(self.sidebar_content(cx)),
                )
                .child(
                    div()
                        .id("sidebar-resize-handle")
                        .w(px(5.0))
                        .h_full()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(ui::border(&p))
                        .cursor(CursorStyle::ResizeColumn)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::start_sidebar_resize)),
                )
                .into_any_element()
        } else {
            div().w(px(0.0)).into_any_element()
        };

        let theme_name = self.themes[self.theme_index].name.clone();
        // 触发后台解码（键变化才真正跑）；再同步取当前图层（命中即用，否则优雅回退）。
        self.request_background_decode(cx);
        let background_layer = self.background_layer();
        let workspace_name = self
            .model
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Workspace")
            .to_owned();
        let agent_indicators = self
            .notification_router
            .bell_items()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let bell_label = if agent_indicators.is_empty() {
            "🔔".to_owned()
        } else {
            format!("🔔 {}", agent_indicators.len())
        };
        let bell_needs_attention = agent_indicators.iter().any(|indicator| {
            matches!(
                indicator.status,
                AgentStatus::Attention | AgentStatus::Error
            )
        });
        let bell_rows = agent_indicators
            .into_iter()
            .map(|indicator| {
                let tab_id = indicator.tab_id;
                div()
                    .id(SharedString::from(format!("agent-state-{}", indicator.id)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(ui::border(&p))
                    .child(SharedString::from(indicator.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui_color(status_color(&p, indicator.status)))
                            .child(status_label(indicator.status)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, _window, cx| {
                            workspace.open_agent_surface(tab_id, cx)
                        }),
                    )
            })
            .collect::<Vec<_>>();
        let bell_panel = self.bell_open.then(|| {
            div()
                .id("agent-bell-panel")
                .flex()
                .flex_col()
                .bg(gpui_color(p.surface[2]))
                .border_b_1()
                .border_color(ui::border(&p))
                .child(div().px_3().py_2().text_sm().child("Agent activity"))
                .children(bell_rows)
        });
        let toast_elements = self
            .toasts
            .clone()
            .into_iter()
            .map(|toast| {
                let id = toast.id;
                let target = toast.notification.target;
                let color = status_color(&p, toast.notification.status);
                div()
                    .id(SharedString::from(format!("agent-toast-{id}")))
                    .mx_3()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui_color(color))
                    .bg(gpui_color(p.surface[1]))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(SharedString::from(toast.notification.title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(gpui_color(color))
                                    .child(status_label(toast.notification.status)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(SharedString::from(toast.notification.body)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, _window, cx| {
                            workspace.toasts.retain(|toast| toast.id != id);
                            workspace.open_notification_target(target, cx);
                        }),
                    )
            })
            .collect::<Vec<_>>();
        let explorer_context_menu = self.explorer_context_menu.clone().map(|menu| {
            let target_kind = menu.target.kind();
            anchored().position(menu.position).child(
                div()
                    .id("explorer-context-menu")
                    .w(px(220.0))
                    .occlude()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(ui::border(&p))
                    .bg(gpui_color(p.surface[2]))
                    .shadow_md()
                    .children(explorer_context_actions(target_kind).iter().copied().map(
                        |action| {
                            div()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_xs()
                                .cursor_pointer()
                                .hover({
                                    let wash = ui::hover_wash(&p);
                                    move |style| style.bg(wash)
                                })
                                .child(explorer_context_action_label(action))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _event, window, cx| {
                                        cx.stop_propagation();
                                        this.handle_explorer_context_action(action, window, cx);
                                    }),
                                )
                        },
                    )),
            )
        });
        let can_split_right = self.can_split_active(SplitDirection::Right, window);
        let can_split_down = self.can_split_active(SplitDirection::Down, window);
        let can_smart_split = can_split_right || can_split_down;
        let has_multiple_panes = self.active_pane_count() > 1;
        let split_description = if can_smart_split {
            "Split the focused pane using the best available direction".to_owned()
        } else {
            self.split_unavailable_message(None)
        };
        let split_focus_ring = ui::focus_ring(&p);
        let split_hover = ui::hover_wash(&p);
        let split_button = div()
            .id("split-smart")
            .px_2()
            .py_1()
            .rounded_l_md()
            .text_xs()
            .opacity(if can_smart_split { 1.0 } else { 0.45 })
            .focusable()
            .tab_stop(can_smart_split)
            .role(Role::Button)
            .aria_label("Split focused pane")
            .aria_description(SharedString::from(split_description))
            .focus_visible(move |style| style.border_1().border_color(split_focus_ring))
            .child("Split")
            .when(can_smart_split, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(split_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _event, window, cx| {
                            cx.stop_propagation();
                            this.smart_split(window, cx);
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                            cx.stop_propagation();
                            this.smart_split(window, cx);
                        }
                    }))
            });
        let split_menu_toggle = div()
            .id("split-menu-toggle")
            .px_1()
            .py_1()
            .rounded_r_md()
            .text_xs()
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .role(Role::Button)
            .aria_label("Open split pane menu")
            .aria_expanded(self.split_menu_open)
            .focus_visible(move |style| style.border_1().border_color(split_focus_ring))
            .hover(move |style| style.bg(split_hover))
            .child("▾")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    cx.stop_propagation();
                    this.explorer_context_menu = None;
                    this.theme_menu_open = false;
                    this.split_menu_open = !this.split_menu_open;
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                    cx.stop_propagation();
                    this.explorer_context_menu = None;
                    this.theme_menu_open = false;
                    this.split_menu_open = !this.split_menu_open;
                    cx.notify();
                }
            }));
        let split_menu = self.split_menu_open.then(|| {
            let menu_x = (f32::from(window.viewport_size().width) - 330.0).max(0.0);
            anchored()
                .position(Point::new(px(menu_x), px(WORKSPACE_HEADER_HEIGHT)))
                .child(
                    div()
                        .id("split-pane-menu")
                        .w(px(270.0))
                        .occlude()
                        .p_1()
                        .rounded_md()
                        .border_1()
                        .border_color(ui::border(&p))
                        .bg(gpui_color(p.surface[2]))
                        .shadow_md()
                        .when(!can_smart_split, |menu| {
                            menu.child(
                                div().px_2().py_1().mb_1().text_xs().opacity(0.65).child(
                                    SharedString::from(self.split_unavailable_message(None)),
                                ),
                            )
                        })
                        .child(split_menu_item(
                            "Smart split",
                            "split-menu-smart",
                            can_smart_split,
                            SplitMenuAction::Smart,
                            &p,
                            cx,
                        ))
                        .child(split_menu_item(
                            "Split right",
                            "split-menu-right",
                            can_split_right,
                            SplitMenuAction::Right,
                            &p,
                            cx,
                        ))
                        .child(split_menu_item(
                            "Split down",
                            "split-menu-down",
                            can_split_down,
                            SplitMenuAction::Down,
                            &p,
                            cx,
                        ))
                        .child(div().h(px(1.0)).my_1().bg(ui::border(&p)))
                        .child(split_menu_item(
                            "Close focused pane",
                            "split-menu-close",
                            has_multiple_panes,
                            SplitMenuAction::CloseActive,
                            &p,
                            cx,
                        ))
                        .child(split_menu_item(
                            "Keep only focused pane",
                            "split-menu-close-others",
                            has_multiple_panes,
                            SplitMenuAction::CloseOthers,
                            &p,
                            cx,
                        )),
                )
        });
        let theme_menu = self.theme_menu_open.then(|| {
            let menu_x = (f32::from(window.viewport_size().width) - 300.0).max(0.0);
            anchored()
                .position(Point::new(px(menu_x), px(WORKSPACE_HEADER_HEIGHT)))
                .child(
                    div()
                        .id("application-theme-menu")
                        .w(px(250.0))
                        .occlude()
                        .p_1()
                        .rounded_md()
                        .border_1()
                        .border_color(ui::border(&p))
                        .bg(gpui_color(p.surface[2]))
                        .shadow_md()
                        .children(self.themes.iter().map(|theme| {
                            let theme_id = theme.id.clone();
                            let selected = theme.id == self.settings.theme_id;
                            let option_palette =
                                theme.resolve(self.resolved_appearance(), self.system_is_dark);
                            div()
                                .id(SharedString::from(format!("theme-option-{}", theme.id)))
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_sm()
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(selected, |item| item.bg(ui::selected_wash(&p)))
                                .when(!selected, |item| {
                                    let wash = ui::hover_wash(&p);
                                    item.hover(move |style| style.bg(wash))
                                })
                                .child(
                                    div()
                                        .w(px(12.0))
                                        .h(px(12.0))
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(ui::border(&option_palette))
                                        .bg(gpui_color(option_palette.accent)),
                                )
                                .child(div().flex_1().child(SharedString::from(theme.name.clone())))
                                .child(if selected { "✓" } else { "" })
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.select_theme(&theme_id, window, cx);
                                    }),
                                )
                        })),
                )
        });
        let input_focus = self.focus_handle.clone();
        let input_entity = cx.entity();
        div()
            .id("workspace-root")
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::global_key))
            .on_mouse_move(cx.listener(Self::resize_sidebar))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_sidebar_resize))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    let closed_explorer_menu = this.explorer_context_menu.take().is_some();
                    let closed_split_menu = std::mem::take(&mut this.split_menu_open);
                    let closed_theme_menu = std::mem::take(&mut this.theme_menu_open);
                    if closed_explorer_menu || closed_split_menu || closed_theme_menu {
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(p.background))
            .text_color(gpui_color(p.foreground))
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &input_focus,
                            ElementInputHandler::new(bounds, input_entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(
                background_layer_element(background_layer),
                |root, element| root.child(element),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(WORKSPACE_HEADER_HEIGHT))
                    .bg(gpui_color(p.surface[2]))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .overflow_hidden()
                            .children(tab_buttons)
                            .child(new_tab_button),
                    )
                    .child(
                        // 头部右侧操作区：统一间距与主题化按钮。
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_shrink_0()
                            .gap_1()
                            .px_2()
                            .child(
                                ui::button(
                                    "open-workspace",
                                    SharedString::from(workspace_name),
                                    ButtonKind::Ghost,
                                    &p,
                                )
                                .max_w(px(180.0))
                                .overflow_hidden()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::open_workspace_picker),
                                ),
                            )
                            .child(header_button(
                                "+ Terminal",
                                "new-terminal",
                                &p,
                                cx,
                                |this, _, window, cx| this.create_terminal(false, window, cx),
                            ))
                            .child(header_button(
                                "+ Editor",
                                "new-editor",
                                &p,
                                cx,
                                |this, _, _, cx| this.create_editor(cx),
                            ))
                            .children(markdown_preview_available.then(|| {
                                header_button(
                                    "Preview Markdown",
                                    "preview-markdown",
                                    &p,
                                    cx,
                                    |this, _, _, cx| {
                                        let _ = this.request_markdown_preview(cx);
                                    },
                                )
                            }))
                            .child(header_button(
                                "Web Preview",
                                "new-web-preview",
                                &p,
                                cx,
                                |this, _, window, cx| this.request_web_preview(window, cx),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(ui::border(&p))
                                    .bg(gpui_color(p.surface[2]))
                                    .child(split_button)
                                    .child(div().w(px(1.0)).h(px(16.0)).bg(ui::border(&p)))
                                    .child(split_menu_toggle),
                            )
                            .child(
                                ui::button(
                                    "theme-select",
                                    SharedString::from(format!("{theme_name}  ▾")),
                                    ButtonKind::Ghost,
                                    &p,
                                )
                                .text_color(ui::muted(&p))
                                .aria_label("Select application theme")
                                .aria_expanded(self.theme_menu_open)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.explorer_context_menu = None;
                                        this.split_menu_open = false;
                                        this.theme_menu_open = !this.theme_menu_open;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                ui::button(
                                    "agent-bell",
                                    SharedString::from(bell_label),
                                    if bell_needs_attention {
                                        ButtonKind::Primary
                                    } else {
                                        ButtonKind::Ghost
                                    },
                                    &p,
                                )
                                .when(bell_needs_attention, |button| {
                                    button
                                        .bg(gpui_color(p.status[2]))
                                        .text_color(ui::on_color(p.status[2]))
                                })
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::toggle_bell)),
                            )
                            .child(
                                ui::button("settings", "⚙", ButtonKind::Ghost, &p)
                                    .text_sm()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::open_settings),
                                    ),
                            ),
                    ),
            )
            .children(bell_panel)
            .children(toast_elements)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .w_full()
                    .child(sidebar)
                    .child(div().flex_1().size_full().child(active_content)),
            )
            .when(self.model.composer_visible, |root| {
                root.child(self.composer.clone())
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(STATUS_BAR_HEIGHT))
                    .px_3()
                    .bg(gpui_color(p.surface[2]))
                    .border_t_1()
                    .border_color(ui::border(&p))
                    .text_xs()
                    .text_color(ui::muted(&p))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(cwd)),
                    )
                    .child(SharedString::from(format!(
                        "AI tools: {}",
                        self.model.ai_tools_running
                    )))
                    .when_some(preview_url, |bar, url| {
                        let label = SharedString::from(format!("Open Web Preview: {url}"));
                        bar.child(
                            ui::button("open-detected-preview", label, ButtonKind::Subtle, &p)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |workspace, _, window, cx| {
                                        workspace.create_preview_url(url.clone(), window, cx)
                                    }),
                                ),
                        )
                    }),
            )
            .children(explorer_context_menu)
            .children(split_menu)
            .children(theme_menu)
    }
}

impl Drop for WorkspaceView {
    fn drop(&mut self) {
        if let Some(dir) = &self.data_dir {
            if let Ok(raw) = serde_json::to_string_pretty(&self.model) {
                let _ = atomic_write(&dir.join("Termior-workspaces.json"), &raw);
            }
            if let Ok(raw) = serde_json::to_string_pretty(&self.settings) {
                let _ = atomic_write(&dir.join("Termior-settings.json"), &raw);
            }
        }
    }
}

fn single_pane(content: PaneContent) -> HashMap<PaneId, PaneContent> {
    HashMap::from([(PaneId(1), content)])
}

fn new_preview_view(url: String, cx: &mut Context<WorkspaceView>) -> Entity<PreviewView> {
    cx.new(|_| PreviewView::new(url))
}

fn explorer_entry_visible(entry: &FileEntry, tree: &TreeState) -> bool {
    let root = tree.root();
    let mut parent = entry.path.parent();
    while let Some(path) = parent {
        if path == root {
            return true;
        }
        if !path.starts_with(root) || !tree.is_expanded(path) {
            return false;
        }
        parent = path.parent();
    }
    false
}

fn explorer_content_width(entries: &[FileEntry]) -> f32 {
    entries
        .iter()
        .map(|entry| {
            let name = entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&entry.relative);
            entry.depth as f32 * 12.0
                + 42.0
                + name
                    .chars()
                    .map(|character| if character.is_ascii() { 7.0 } else { 12.0 })
                    .sum::<f32>()
        })
        .fold(1.0, f32::max)
}

fn explorer_icon(
    icon: IconKind,
    expanded: bool,
    surface_bg: gpui::Rgba,
    panel_bg: gpui::Rgba,
) -> gpui::Div {
    if icon == IconKind::Folder {
        let color = gpui::rgba(0xe5c07bff);
        let folder = div()
            .relative()
            .w(px(18.0))
            .h(px(16.0))
            .flex_shrink_0()
            .child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .left(px(2.0))
                    .w(px(7.0))
                    .h(px(5.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color)
                    .bg(panel_bg),
            )
            .child(
                div()
                    .absolute()
                    .top(px(5.0))
                    .left(px(1.0))
                    .w(px(15.0))
                    .h(px(10.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color)
                    .bg(panel_bg),
            );
        return if expanded {
            folder.child(
                div()
                    .absolute()
                    .top(px(8.0))
                    .left(px(0.0))
                    .w(px(17.0))
                    .h(px(7.0))
                    .rounded_sm()
                    .border_1()
                    .border_color(color)
                    .bg(surface_bg),
            )
        } else {
            folder
        };
    }

    let (label, color) = match icon {
        IconKind::Rust => ("R", gpui::rgba(0xd08770ff)),
        IconKind::JavaScript => ("J", gpui::rgba(0xebcb8bff)),
        IconKind::TypeScript => ("T", gpui::rgba(0x5e81acff)),
        IconKind::Python => ("P", gpui::rgba(0x81a1c1ff)),
        IconKind::Go => ("G", gpui::rgba(0x88c0d0ff)),
        IconKind::Java => ("J", gpui::rgba(0xbf616aff)),
        IconKind::Html => ("H", gpui::rgba(0xd08770ff)),
        IconKind::Css => ("C", gpui::rgba(0xb48eadff)),
        IconKind::Json => ("{", gpui::rgba(0xa3be8cff)),
        IconKind::Markdown => ("M", gpui::rgba(0x81a1c1ff)),
        IconKind::Image => ("I", gpui::rgba(0xb48eadff)),
        IconKind::Config => ("C", gpui::rgba(0x8fbcbbff)),
        IconKind::File | IconKind::Folder => ("", gpui::rgba(0x9aa6b7ff)),
    };
    div()
        .relative()
        .w(px(18.0))
        .h(px(16.0))
        .flex_shrink_0()
        .child(
            div()
                .absolute()
                .top(px(1.0))
                .left(px(1.0))
                .w(px(12.0))
                .h(px(14.0))
                .rounded_sm()
                .border_1()
                .border_color(color)
                .bg(surface_bg),
        )
        .child(
            div()
                .absolute()
                .top(px(5.0))
                .left(px(4.0))
                .text_size(px(7.0))
                .line_height(px(7.0))
                .text_color(color)
                .child(label),
        )
}

fn explorer_context_actions(kind: ExplorerContextTargetKind) -> &'static [ExplorerContextAction] {
    match kind {
        ExplorerContextTargetKind::File => &[
            ExplorerContextAction::Open,
            ExplorerContextAction::Rename,
            ExplorerContextAction::Delete,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::AttachToAi,
            ExplorerContextAction::Refresh,
        ],
        ExplorerContextTargetKind::Directory => &[
            ExplorerContextAction::CreateFile,
            ExplorerContextAction::CreateDirectory,
            ExplorerContextAction::Rename,
            ExplorerContextAction::Delete,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::Refresh,
        ],
        ExplorerContextTargetKind::Workspace => &[
            ExplorerContextAction::CreateFile,
            ExplorerContextAction::CreateDirectory,
            ExplorerContextAction::FindFile,
            ExplorerContextAction::SearchContent,
            ExplorerContextAction::Reveal,
            ExplorerContextAction::Refresh,
        ],
    }
}

fn explorer_context_action_label(action: ExplorerContextAction) -> &'static str {
    match action {
        ExplorerContextAction::Open => "Open",
        ExplorerContextAction::CreateFile => "New File…",
        ExplorerContextAction::CreateDirectory => "New Folder…",
        ExplorerContextAction::Rename => "Rename…",
        ExplorerContextAction::Delete => "Delete…",
        ExplorerContextAction::Reveal => "Show in File Manager",
        ExplorerContextAction::AttachToAi => "Attach to AI",
        ExplorerContextAction::FindFile => "Find File…",
        ExplorerContextAction::SearchContent => "Search in Files…",
        ExplorerContextAction::Refresh => "Refresh",
    }
}

fn dir_is_non_empty(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

fn reveal_in_system_file_manager(path: &Path, select_file: bool) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer");
        if select_file {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        command.spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        if select_file {
            command.arg("-R").arg(path);
        } else {
            command.arg(path);
        }
        command.spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let target = if select_file {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        Command::new("xdg-open").arg(target).spawn().map(|_| ())
    }
}

fn utf16_to_byte(text: &str, utf16_offset: usize) -> usize {
    let mut utf16_count = 0;
    for (byte, character) in text.char_indices() {
        if utf16_count >= utf16_offset {
            return byte;
        }
        utf16_count += character.len_utf16();
    }
    text.len()
}

fn sidebar_button(
    label: &'static str,
    id: &'static str,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &mut Context<WorkspaceView>) + 'static,
) -> impl IntoElement {
    ui::button(id, label, ButtonKind::Subtle, p).on_mouse_down(
        MouseButton::Left,
        cx.listener(move |workspace, _event, window, cx| {
            window.focus(&workspace.focus_handle, cx);
            listener(workspace, cx);
        }),
    )
}

fn header_button(
    label: &'static str,
    id: &'static str,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &MouseDownEvent, &mut Window, &mut Context<WorkspaceView>)
        + 'static,
) -> impl IntoElement {
    ui::button(id, label, ButtonKind::Subtle, p)
        .on_mouse_down(MouseButton::Left, cx.listener(listener))
}

fn split_menu_item(
    label: &'static str,
    id: &'static str,
    enabled: bool,
    action: SplitMenuAction,
    p: &ResolvedPalette,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let focus_ring = ui::focus_ring(p);
    let hover_bg = ui::hover_wash(p);
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_sm()
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .focusable()
        .tab_stop(enabled)
        .role(Role::Button)
        .aria_label(label)
        .aria_description(if enabled {
            "Activate this split pane command"
        } else {
            "Unavailable for the focused pane"
        })
        .focus_visible(move |style| style.border_1().border_color(focus_ring))
        .child(label)
        .when(enabled, |item| {
            item.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event, window, cx| {
                        cx.stop_propagation();
                        this.handle_split_menu_action(action, window, cx);
                    }),
                )
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "return" | "space") {
                        cx.stop_propagation();
                        this.handle_split_menu_action(action, window, cx);
                    }
                }))
        })
}

/// 从设置构造行内补全 [`InlineCompleter`]（与聊天 profile 同规则；密钥只读钥匙串，INV-5）。
/// 复用于 `WorkspaceView::new`（此时 `self` 尚未建成）与 `build_completer`。
fn build_completer_from_settings(settings: &Settings) -> Option<std::sync::Arc<InlineCompleter>> {
    if !settings.autocomplete_enabled {
        return None;
    }
    let profile = settings
        .models
        .active_completion_profile
        .as_deref()
        .and_then(|id| {
            settings
                .models
                .profiles
                .iter()
                .find(|profile| profile.id == id)
        })
        .filter(|profile| profile.enabled)?;
    let api_key = KeyringSecretStore::new()
        .get(&format!("provider:{}", profile.id))
        .ok()
        .flatten();
    if !profile.local && api_key.is_none() {
        return None;
    }
    let config = ProviderConfig::from_settings(profile, api_key).ok()?;
    let provider = HttpProvider::new(config).ok()?;
    Some(std::sync::Arc::new(InlineCompleter::new(
        Box::new(provider),
        profile.model.clone(),
    )))
}

fn terminal_agent_status(state: AgentState) -> AgentStatus {
    match state {
        AgentState::Started => AgentStatus::Started,
        AgentState::Working => AgentStatus::Working,
        AgentState::Attention => AgentStatus::Attention,
        AgentState::Finished => AgentStatus::Finished,
        AgentState::Exited => AgentStatus::Exited,
    }
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Started => "started",
        AgentStatus::Working => "working",
        AgentStatus::Attention => "attention",
        AgentStatus::Finished => "finished",
        AgentStatus::Exited => "exited",
        AgentStatus::Error => "error",
    }
}

fn agent_status_message(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Started => "Agent started",
        AgentStatus::Working => "Agent is working",
        AgentStatus::Attention => "Agent needs your attention",
        AgentStatus::Finished => "Agent finished",
        AgentStatus::Exited => "Agent exited",
        AgentStatus::Error => "Agent encountered an error",
    }
}

fn status_color(palette: &ResolvedPalette, status: AgentStatus) -> termior_theme::Color {
    match status {
        AgentStatus::Started | AgentStatus::Working => palette.status[0],
        AgentStatus::Finished => palette.status[1],
        AgentStatus::Attention => palette.status[2],
        AgentStatus::Error => palette.status[3],
        AgentStatus::Exited => palette.foreground,
    }
}

fn gpui_color(color: termior_theme::Color) -> gpui::Rgba {
    ui::color(color)
}

/// 带透明度的主题色（tab 非激活文字、分隔线等弱化元素用）。
fn gpui_color_alpha(color: termior_theme::Color, alpha: f32) -> gpui::Rgba {
    ui::alpha(color, alpha)
}

fn load_settings() -> (Settings, Option<PathBuf>, Option<String>) {
    let data_dir = app_data_dir().ok();
    let Some(dir) = data_dir.as_ref() else {
        return (
            default_settings(),
            None,
            Some("Could not resolve application data directory".into()),
        );
    };
    let path = dir.join("Termior-settings.json");
    match std::fs::read_to_string(path) {
        Ok(raw) => match migrate(&raw) {
            Ok(settings) => (settings, data_dir, None),
            Err(error) => (default_settings(), data_dir, Some(error.to_string())),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (default_settings(), data_dir, None)
        }
        Err(error) => (default_settings(), data_dir, Some(error.to_string())),
    }
}

fn load_vcs(
    root: &Path,
    workspace_auth: &WorkspaceAuthRegistry,
) -> (Vec<ChangedFile>, Vec<CommitInfo>, Option<BranchState>) {
    match GitRepository::open(root, workspace_auth) {
        Ok(repo) => (
            repo.status().unwrap_or_default(),
            repo.history(100, None).unwrap_or_default(),
            repo.branch_state().ok(),
        ),
        Err(_) => (Vec::new(), Vec::new(), None),
    }
}

#[cfg(test)]
mod explorer_ui_tests {
    use super::*;

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
        let file = explorer_context_actions(ExplorerContextTargetKind::File);
        assert!(file.contains(&ExplorerContextAction::Open));
        assert!(file.contains(&ExplorerContextAction::AttachToAi));
        assert!(!file.contains(&ExplorerContextAction::CreateDirectory));

        let workspace = explorer_context_actions(ExplorerContextTargetKind::Workspace);
        assert!(workspace.contains(&ExplorerContextAction::CreateFile));
        assert!(workspace.contains(&ExplorerContextAction::SearchContent));
        assert!(!workspace.contains(&ExplorerContextAction::Delete));
    }
}
