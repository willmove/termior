use crate::app_identity;
use crate::composer_view::ComposerView;
use crate::editor_view::EditorView;
use crate::preview_view::PreviewView;
use crate::settings_view::SettingsView;
use crate::terminal_view::TerminalView;
use gpui::{
    div, prelude::*, px, relative, size, AnyElement, Bounds, Context, Entity, FocusHandle,
    Focusable, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Task, Window, WindowBounds,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;
use termior_ai::AttachmentSource;
use termior_explorer::{
    ContentMatch, ContentSearch, FileEntry, FileIndex, TreeState, WorkspaceWatcher,
};
use termior_platform::{
    AgentIndicator, AgentStatus, NativeNotifier, Notification, NotificationContext,
    NotificationDecision, NotificationRouter, NotificationTarget, SystemNotifier,
};
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{app_data_dir, atomic_write, default_settings, migrate, KeyAction, Settings};
use termior_terminal_core::osc::AgentState;
use termior_theme::{Appearance, ResolvedPalette, Theme, ThemeLibrary};
use termior_ui::{SidebarPanel, TabId, TabKind, WorkspaceState};
use termior_ui_kit::{LayoutNode, PaneId, SplitDirection};
use termior_vcs::{
    BranchState, ChangeGroup, ChangedFile, CommitInfo, GitRepository, RemoteOperation,
};

#[derive(Clone)]
enum PaneContent {
    Terminal(Entity<TerminalView>),
    Editor(Entity<EditorView>),
    Preview(Entity<PreviewView>),
    Placeholder(String),
}

impl PaneContent {
    fn element(&self) -> AnyElement {
        match self {
            Self::Terminal(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Editor(entity) => div().size_full().child(entity.clone()).into_any_element(),
            Self::Preview(entity) => div().size_full().child(entity.clone()).into_any_element(),
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
    _background_task: Option<Task<()>>,
    command_mode: CommandMode,
    command_input: String,
    command_message: Option<String>,
    content_matches: Vec<ContentMatch>,
    confirm_delete: Option<PathBuf>,
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
}

impl WorkspaceView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (settings, data_dir, migration_error) = load_settings();
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
            true,
        );
        let mut model = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join("Termior-workspaces.json")).ok())
            .and_then(|raw| serde_json::from_str::<WorkspaceState>(&raw).ok())
            .filter(|state| state.root == root)
            .unwrap_or_else(|| WorkspaceState::new(root.clone()));
        // A restored model needs runtime view owners; terminal/preview are restored asynchronously.
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
                                TabKind::Editor | TabKind::Markdown => {
                                    let editor = tab
                                        .resource
                                        .as_ref()
                                        .map(PathBuf::from)
                                        .map(|path| cx.new(|cx| EditorView::open(&path, cx)))
                                        .unwrap_or_else(|| cx.new(EditorView::untitled));
                                    editor.update(cx, |editor, _| {
                                        editor.set_preferences(
                                            &settings.editor_theme_id,
                                            settings.vim_mode,
                                        )
                                    });
                                    PaneContent::Editor(editor)
                                }
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
        let mut notification_router = NotificationRouter::default();
        notification_router.set_enabled(settings.agent_notifications);
        notification_router.update_agent(AgentIndicator {
            id: "builtin-agent".into(),
            title: "Built-in Agent".into(),
            status: AgentStatus::Finished,
            tab_id: None,
        });
        let explorer = FileIndex::build(&root, settings.show_dotfiles).ok();
        let explorer_tree = TreeState::new(root.clone());
        let explorer_watcher = WorkspaceWatcher::watch(&root).ok();
        let (vcs_status, vcs_history, vcs_branch) = load_vcs(&root, &workspace_auth);
        Self {
            model,
            tabs,
            themes,
            theme_index,
            palette,
            focus_handle: cx.focus_handle(),
            composer,
            explorer,
            explorer_tree,
            explorer_watcher,
            _background_task: None,
            command_mode: CommandMode::Browse,
            command_input: String::new(),
            command_message: None,
            content_matches: Vec::new(),
            confirm_delete: None,
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
        }
    }

    pub fn restore_or_create_runtime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal_panes: Vec<(TabId, PaneId, Option<PathBuf>)> = self
            .model
            .tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::Terminal)
            .flat_map(|tab| {
                tab.layout
                    .panes()
                    .into_iter()
                    .map(|pane_id| (tab.id, pane_id, Some(tab.cwd.clone())))
                    .collect::<Vec<_>>()
            })
            .collect();
        if terminal_panes.is_empty() && self.model.tabs.is_empty() {
            self.create_terminal(false, cx);
        } else {
            for (id, pane_id, cwd) in terminal_panes {
                self.spawn_terminal_into(id, pane_id, cwd, cx);
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
                            PaneContent::Preview(
                                cx.new(|cx| PreviewView::new(url.clone(), window, cx)),
                            ),
                        );
                    }
                }
            }
        }
    }

    pub fn start_background_services(&mut self, cx: &mut Context<Self>) {
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
                        workspace.refresh_workspace_data();
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        }));
    }

    fn refresh_workspace_data(&mut self) {
        if let Some(index) = self.explorer.as_mut() {
            if let Err(error) = index.refresh() {
                self.command_message = Some(format!("Explorer refresh failed: {error}"));
            }
        }
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
        let Some(root) = rfd::FileDialog::new()
            .set_title("Open a Termior workspace")
            .pick_folder()
        else {
            return;
        };
        if !root.is_dir() || root == self.model.root {
            return;
        }
        *self = Self::new(root, cx);
        self.restore_or_create_runtime(window, cx);
        self.start_background_services(cx);
        cx.notify();
    }

    fn create_terminal(&mut self, private: bool, cx: &mut Context<Self>) {
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
        self.spawn_terminal_into(id, PaneId(1), cwd, cx);
        cx.notify();
    }

    fn spawn_terminal_into(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let palette = self.palette.clone();
        let terminal_settings = self.settings.terminal.clone();
        let keymap = self.settings.keymap.clone();
        let workspace_auth = self.workspace_auth.clone();
        cx.spawn(async move |workspace, cx| {
            let config = termior_terminal::PtySessionConfig {
                cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
                workspace_auth: Some(workspace_auth),
                ..Default::default()
            };
            let bridge = match termior_terminal::TerminalBridge::spawn(&config) {
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
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    if let Some(pane) = tab.panes.get_mut(&pane_id) {
                        *pane = PaneContent::Terminal(entity);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_editor(&mut self, cx: &mut Context<Self>) {
        let id = self.model.new_tab(TabKind::Editor, "Untitled", false);
        let editor = cx.new(EditorView::untitled);
        editor.update(cx, |editor, _| {
            editor.set_preferences(&self.settings.editor_theme_id, self.settings.vim_mode)
        });
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Editor(editor)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn open_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Editor")
            .to_owned();
        let entity = cx.new(|cx| EditorView::open(&path, cx));
        entity.update(cx, |editor, _| {
            editor.set_preferences(&self.settings.editor_theme_id, self.settings.vim_mode)
        });
        if entity.read(cx).path().is_none() {
            log::warn!("could not open editor file: {}", path.display());
        }
        let id = self.model.new_tab(TabKind::Editor, title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(path.to_string_lossy().into_owned());
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Editor(entity)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn create_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self
            .active_terminal()
            .and_then(|terminal| terminal.read(cx).localhost_urls().last().cloned())
            .unwrap_or_else(|| "http://localhost:3000".into());
        let id = self.model.new_tab(TabKind::Preview, "Preview", false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.resource = Some(url.clone());
        }
        let preview = cx.new(|cx| PreviewView::new(url, window, cx));
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Preview(preview)),
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn split_active(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(active) = self.model.active else {
            return;
        };
        let Ok(pane_id) = self.model.split_active(direction) else {
            return;
        };
        let kind = self.model.active_tab().map(|tab| tab.kind);
        let pane = match kind {
            Some(TabKind::Editor) => {
                let editor = cx.new(EditorView::untitled);
                editor.update(cx, |editor, _| {
                    editor.set_preferences(&self.settings.editor_theme_id, self.settings.vim_mode)
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
            self.spawn_terminal_into(active, pane_id, cwd, cx);
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

    fn activate_runtime(&mut self, id: TabId, cx: &mut Context<Self>) {
        let _ = self.model.switch_to(id);
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Preview(preview) = pane {
                    preview.update(cx, |preview, cx| preview.set_active(tab.id == id, cx));
                }
            }
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

    fn cycle_theme(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme_index = (self.theme_index + 1) % self.themes.len();
        self.palette = self.themes[self.theme_index].resolve(Appearance::Dark, true);
        self.settings.theme_id = self.themes[self.theme_index].id.clone();
        cx.notify();
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
        let on_save = Box::new(move |settings: &Settings, app: &mut gpui::App| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(app, |workspace, cx| {
                    workspace.apply_settings(settings.clone(), cx);
                    cx.notify();
                });
            }
        });
        let bounds = Bounds::centered(None, size(px(760.0), px(520.0)), cx);
        let _ = cx.open_window(
            app_identity::window_options(WindowBounds::Windowed(bounds)),
            |_window, cx| {
                cx.new(|cx| {
                    SettingsView::new(settings, migration_error, data_dir, Some(on_save), cx)
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
        self.palette = self.themes[self.theme_index].resolve(
            match self.settings.appearance {
                termior_store::settings::Appearance::Light => Appearance::Light,
                termior_store::settings::Appearance::Dark => Appearance::Dark,
                termior_store::settings::Appearance::FollowSystem => Appearance::FollowSystem,
            },
            true,
        );
        if let Some(index) = self.explorer.as_mut() {
            let _ = index.set_show_dotfiles(self.settings.show_dotfiles);
        }
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor
                            .set_preferences(&self.settings.editor_theme_id, self.settings.vim_mode)
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
        self.command_message = None;
        cx.notify();
    }

    fn handle_command_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.command_mode == CommandMode::Browse {
            return false;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => {
                self.command_mode = CommandMode::Browse;
                self.command_input.clear();
            }
            "backspace" => {
                self.command_input.pop();
            }
            "enter" | "return" => self.execute_command(cx),
            _ if !event.keystroke.modifiers.control
                && !event.keystroke.modifiers.platform
                && !event.keystroke.modifiers.alt =>
            {
                if let Some(text) = &event.keystroke.key_char {
                    self.command_input.push_str(text);
                }
            }
            _ => {}
        }
        cx.notify();
        true
    }

    fn execute_command(&mut self, cx: &mut Context<Self>) {
        let input = self.command_input.trim().to_owned();
        if input.is_empty() {
            return;
        }
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
                self.content_matches.clear();
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
                ContentSearch::default()
                    .search_paths(&input, paths, |hit| {
                        self.content_matches.push(hit);
                        self.content_matches.len() < 200
                    })
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            CommandMode::CreateFile => self
                .explorer_tree
                .create_file(&input)
                .map(|path| self.open_editor(path, cx))
                .map_err(|error| error.to_string()),
            CommandMode::CreateDirectory => self
                .explorer_tree
                .create_directory(&input)
                .map(|_| ())
                .map_err(|error| error.to_string()),
            CommandMode::Rename => {
                let selected = self.explorer_tree.selected().map(Path::to_path_buf);
                match selected.and_then(|path| {
                    path.strip_prefix(self.explorer_tree.root())
                        .ok()
                        .map(Path::to_path_buf)
                }) {
                    Some(relative) => self
                        .explorer_tree
                        .rename(relative, &input)
                        .map(|_| ())
                        .map_err(|error| error.to_string()),
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
            CommandMode::Browse => Ok(()),
        };
        match result {
            Ok(()) => {
                self.command_message = Some("Done".into());
                self.command_mode = CommandMode::Browse;
                self.command_input.clear();
                self.refresh_workspace_data();
            }
            Err(error) => self.command_message = Some(error),
        }
    }

    fn git_repository(&self) -> Result<GitRepository, String> {
        GitRepository::open(&self.model.root, &self.workspace_auth)
            .map_err(|error| error.to_string())
    }

    fn toggle_git_file(&mut self, path: &str, group: ChangeGroup, cx: &mut Context<Self>) {
        let result = self.git_repository().and_then(|repo| match group {
            ChangeGroup::Staged => repo.unstage_file(path).map_err(|error| error.to_string()),
            ChangeGroup::Unstaged | ChangeGroup::Untracked => {
                repo.stage_file(path).map_err(|error| error.to_string())
            }
        });
        self.command_message = result.err();
        self.refresh_workspace_data();
        cx.notify();
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
        self.refresh_workspace_data();
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
                workspace.refresh_workspace_data();
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.explorer_tree.selected().map(Path::to_path_buf) else {
            self.command_message = Some("Select a file or directory first".into());
            cx.notify();
            return;
        };
        if self.confirm_delete.as_ref() != Some(&selected) {
            self.confirm_delete = Some(selected);
            self.command_message = Some("Click Delete again to confirm".into());
            cx.notify();
            return;
        }
        self.confirm_delete = None;
        let result = selected
            .strip_prefix(self.explorer_tree.root())
            .map_err(|error| error.to_string())
            .and_then(|relative| {
                self.explorer_tree
                    .delete(relative)
                    .map_err(|error| error.to_string())
            });
        self.command_message = result.err();
        self.refresh_workspace_data();
        cx.notify();
    }

    fn global_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.handle_command_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        let Some(action) = self.configured_action(event) else {
            return;
        };
        match action {
            KeyAction::NewTerminalTab => self.create_terminal(false, cx),
            KeyAction::NewPrivateTerminal => self.create_terminal(true, cx),
            KeyAction::NewEditorTab => self.create_editor(cx),
            KeyAction::NewPreviewTab => self.create_preview(window, cx),
            KeyAction::ClosePaneOrTab => self.close_active(cx),
            KeyAction::GotoTab1 => {
                let _ = self.model.switch_index(1);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                }
            }
            KeyAction::CycleTabs | KeyAction::CycleTabsReverse => {
                self.model.cycle_tab(action == KeyAction::CycleTabsReverse);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                }
            }
            KeyAction::SplitRight => self.split_active(SplitDirection::Right, cx),
            KeyAction::SplitDown => self.split_active(SplitDirection::Down, cx),
            KeyAction::FocusPanePrev | KeyAction::FocusPaneNext => {
                if let Some(tab) = self.model.active_tab_mut() {
                    tab.layout
                        .focus_relative(if action == KeyAction::FocusPanePrev {
                            -1
                        } else {
                            1
                        });
                }
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
                        pane.border_1().border_color(gpui::rgba(0x4f8fefff))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, _window, cx| {
                            if let Some(tab) = workspace.model.active_tab_mut() {
                                let _ = tab.layout.focus(pane_id);
                            }
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
                let first = self.layout_element(first, panes, focused, cx);
                let second = self.layout_element(second, panes, focused, cx);
                let ratio = ratio.clamp(0.1, 0.9);
                match direction {
                    SplitDirection::Right => div()
                        .flex()
                        .flex_row()
                        .size_full()
                        .child(div().h_full().w(relative(ratio)).child(first))
                        .child(div().h_full().flex_1().child(second))
                        .into_any_element(),
                    SplitDirection::Down => div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(div().w_full().h(relative(ratio)).child(first))
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
                .border_color(gpui::rgba(0x4f8fefff))
                .text_xs()
                .child(SharedString::from(format!(
                    "{:?}: {}▏",
                    self.command_mode, self.command_input
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
                    .child(sidebar_button("Find", "explorer-find", cx, |this, cx| {
                        this.begin_command(CommandMode::FindFile, cx)
                    }))
                    .child(sidebar_button(
                        "Search",
                        "explorer-search",
                        cx,
                        |this, cx| this.begin_command(CommandMode::SearchContent, cx),
                    ))
                    .child(sidebar_button("+F", "explorer-new-file", cx, |this, cx| {
                        this.begin_command(CommandMode::CreateFile, cx)
                    }))
                    .child(sidebar_button("+D", "explorer-new-dir", cx, |this, cx| {
                        this.begin_command(CommandMode::CreateDirectory, cx)
                    }))
                    .child(sidebar_button("Ren", "explorer-rename", cx, |this, cx| {
                        this.begin_command(CommandMode::Rename, cx)
                    }))
                    .child(sidebar_button("Del", "explorer-delete", cx, |this, cx| {
                        this.delete_selected(cx)
                    }));

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
                } else if self.command_mode == CommandMode::SearchContent
                    && !self.content_matches.is_empty()
                {
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
                                    path.strip_prefix(&self.model.root)
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
                    self.explorer
                        .as_ref()
                        .map(|index| {
                            let mut entries = index.entries().to_vec();
                            entries.sort_by(|a, b| a.relative.cmp(&b.relative));
                            entries
                                .into_iter()
                                .filter(|entry| explorer_entry_visible(entry, &self.explorer_tree))
                                .take(500)
                                .map(|entry| {
                                    let path = entry.path.clone();
                                    let attach_path = path.clone();
                                    let selected =
                                        self.explorer_tree.selected() == Some(path.as_path());
                                    let expanded = self.explorer_tree.is_expanded(&path);
                                    let label = entry
                                        .path
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or(&entry.relative)
                                        .to_owned();
                                    div()
                                        .id(SharedString::from(format!("file-{}", entry.relative)))
                                        .pl(px(4.0 + entry.depth as f32 * 12.0))
                                        .pr_1()
                                        .py_1()
                                        .text_xs()
                                        .cursor_pointer()
                                        .when(selected, |row| row.bg(gpui::rgba(0x36588088)))
                                        .child(SharedString::from(format!(
                                            "{}{}",
                                            if entry.is_dir {
                                                if expanded {
                                                    "▾ "
                                                } else {
                                                    "▸ "
                                                }
                                            } else {
                                                "  "
                                            },
                                            label
                                        )))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _event, _window, cx| {
                                                this.explorer_tree.select(path.clone());
                                                this.confirm_delete = None;
                                                if entry.is_dir {
                                                    this.explorer_tree
                                                        .toggle_expanded(path.clone());
                                                } else {
                                                    this.open_editor(path.clone(), cx);
                                                }
                                                cx.notify();
                                            }),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(move |this, _event, _window, cx| {
                                                this.attach_file(attach_path.clone(), cx)
                                            }),
                                        )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                div()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .children(rows)
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
                    .child(sidebar_button("All+", "git-stage-all", cx, |this, cx| {
                        this.stage_all(cx)
                    }))
                    .child(sidebar_button("Commit", "git-commit", cx, |this, cx| {
                        this.begin_command(CommandMode::GitCommit, cx)
                    }))
                    .child(sidebar_button("Fetch", "git-fetch", cx, |this, cx| {
                        this.run_remote(RemoteOperation::Fetch, cx)
                    }))
                    .child(sidebar_button("Pull", "git-pull", cx, |this, cx| {
                        this.run_remote(RemoteOperation::PullFfOnly, cx)
                    }))
                    .child(sidebar_button("Push", "git-push", cx, |this, cx| {
                        this.run_remote(RemoteOperation::Push, cx)
                    }))
                    .child(sidebar_button(
                        "+Branch",
                        "git-new-branch",
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitCreateBranch, cx),
                    ))
                    .child(sidebar_button(
                        "Switch",
                        "git-switch-branch",
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
                                    this.toggle_git_file(&path, group, cx)
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
                div().flex().flex_col().children(rows).into_any_element()
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
            if path.is_dir()
                && self
                    .explorer
                    .as_ref()
                    .map_or(true, |index| index.root() != path)
            {
                self.explorer = FileIndex::build(path, self.settings.show_dotfiles).ok();
                self.explorer_tree.set_root(path.to_path_buf());
                self.explorer_watcher = WorkspaceWatcher::watch(path).ok();
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

impl gpui::Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.process_agent_updates(window, cx);
        let (cwd, preview_url) = self.sync_terminal_context(cx);
        let p = self.palette.clone();
        let active = self.model.active;
        let tab_buttons = self
            .model
            .tabs
            .iter()
            .map(|tab| {
                let id = tab.id;
                let selected = active == Some(id);
                div()
                    .id(SharedString::from(format!("tab-{}", id.0)))
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .bg(if selected {
                        gpui_color(p.surface[1])
                    } else {
                        gpui_color(p.surface[2])
                    })
                    .child(SharedString::from(tab.title.clone()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.activate_runtime(id, cx);
                            cx.notify();
                        }),
                    )
            })
            .collect::<Vec<_>>();

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
                .w(px(260.0))
                .h_full()
                .border_r_1()
                .border_color(gpui_color(p.surface[1]))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w(px(44.0))
                        .items_center()
                        .gap_2()
                        .pt_2()
                        .children(
                            [
                                ("E", SidebarPanel::Explorer),
                                ("G", SidebarPanel::SourceControl),
                                ("H", SidebarPanel::GitHistory),
                            ]
                            .into_iter()
                            .map(|(label, panel)| {
                                div()
                                    .id(SharedString::from(format!("sidebar-{panel:?}")))
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
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
                        .overflow_hidden()
                        .p_2()
                        .child(self.sidebar_content(cx)),
                )
                .into_any_element()
        } else {
            div().w(px(0.0)).into_any_element()
        };

        let theme_name = self.themes[self.theme_index].name.clone();
        let background_image = self
            .settings
            .background
            .image_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .map(|path| (path, self.settings.background.opacity.clamp(0.0, 1.0)));
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
                    .border_color(gpui_color(p.surface[1]))
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
                .border_color(gpui_color(p.surface[1]))
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
        div()
            .id("workspace-root")
            .relative()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::global_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(p.background))
            .text_color(gpui_color(p.foreground))
            .when_some(background_image, |root, (path, opacity)| {
                root.child(
                    gpui::img(path)
                        .absolute()
                        .size_full()
                        .object_fit(gpui::ObjectFit::Cover)
                        .opacity(opacity),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(40.0))
                    .bg(gpui_color(p.surface[2]))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_1()
                            .h_full()
                            .children(tab_buttons),
                    )
                    .child(
                        div()
                            .id("open-workspace")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .child(SharedString::from(workspace_name))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::open_workspace_picker),
                            ),
                    )
                    .child(header_button("+T", "new-terminal", cx, |this, _, _, cx| {
                        this.create_terminal(false, cx)
                    }))
                    .child(header_button("+E", "new-editor", cx, |this, _, _, cx| {
                        this.create_editor(cx)
                    }))
                    .child(header_button(
                        "Preview",
                        "new-preview",
                        cx,
                        |this, _, window, cx| this.create_preview(window, cx),
                    ))
                    .child(header_button(
                        "Split",
                        "split-right",
                        cx,
                        |this, _, _, cx| this.split_active(SplitDirection::Right, cx),
                    ))
                    .child(
                        div()
                            .id("theme-cycle")
                            .px_2()
                            .cursor_pointer()
                            .child(SharedString::from(theme_name))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::cycle_theme)),
                    )
                    .child(
                        div()
                            .id("agent-bell")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(if bell_needs_attention {
                                gpui_color(p.status[2])
                            } else {
                                gpui_color(p.surface[2])
                            })
                            .text_xs()
                            .child(SharedString::from(bell_label))
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::toggle_bell)),
                    )
                    .child(
                        div()
                            .id("settings")
                            .px_3()
                            .cursor_pointer()
                            .child("⚙")
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::open_settings)),
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
                    .h(px(24.0))
                    .px_3()
                    .bg(gpui_color(p.surface[2]))
                    .text_xs()
                    .child(SharedString::from(cwd))
                    .child(SharedString::from(format!(
                        "AI tools: {}",
                        self.model.ai_tools_running
                    )))
                    .when_some(preview_url, |bar, url| {
                        bar.child(SharedString::from(format!("Open in preview: {url}")))
                    }),
            )
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

fn sidebar_button(
    label: &'static str,
    id: &'static str,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &mut Context<WorkspaceView>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_1()
        .py_1()
        .rounded_sm()
        .bg(gpui::rgba(0x293241dd))
        .text_xs()
        .cursor_pointer()
        .child(label)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |workspace, _event, _window, cx| listener(workspace, cx)),
        )
}

fn header_button(
    label: &'static str,
    id: &'static str,
    cx: &mut Context<WorkspaceView>,
    listener: impl Fn(&mut WorkspaceView, &MouseDownEvent, &mut Window, &mut Context<WorkspaceView>)
        + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .child(label)
        .on_mouse_down(MouseButton::Left, cx.listener(listener))
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
    gpui::rgba(((color.r as u32) << 24) | ((color.g as u32) << 16) | ((color.b as u32) << 8) | 0xff)
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
