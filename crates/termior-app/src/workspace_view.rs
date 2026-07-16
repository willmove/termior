use crate::composer_view::ComposerView;
use crate::editor_view::EditorView;
use crate::preview_view::PreviewView;
use crate::settings_view::SettingsView;
use crate::terminal_view::TerminalView;
use gpui::{
    div, prelude::*, px, size, AnyElement, Bounds, Context, Entity, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Window, WindowBounds, WindowOptions,
};
use std::path::{Path, PathBuf};
use termior_ai::AttachmentSource;
use termior_explorer::FileIndex;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{app_data_dir, atomic_write, default_settings, migrate, Settings};
use termior_theme::{builtin_themes, Appearance, ResolvedPalette, Theme};
use termior_ui::{SidebarPanel, TabId, TabKind, WorkspaceState};
use termior_ui_kit::{LayoutNode, SplitDirection};
use termior_vcs::{ChangedFile, CommitInfo, GitRepository};

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
    panes: Vec<PaneContent>,
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
    vcs_status: Vec<ChangedFile>,
    vcs_history: Vec<CommitInfo>,
    settings: Settings,
    data_dir: Option<PathBuf>,
    migration_error: Option<String>,
    workspace_auth: WorkspaceAuthRegistry,
}

impl WorkspaceView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (settings, data_dir, migration_error) = load_settings();
        let themes = builtin_themes();
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
                panes: vec![match tab.kind {
                    TabKind::Editor => {
                        let editor = cx.new(EditorView::untitled);
                        editor.update(cx, |editor, _| {
                            editor.set_preferences(&settings.editor_theme_id, settings.vim_mode)
                        });
                        PaneContent::Editor(editor)
                    }
                    _ => PaneContent::Placeholder(format!("Restoring {}…", tab.title)),
                }],
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
        let explorer = FileIndex::build(&root, settings.show_dotfiles).ok();
        let (vcs_status, vcs_history) = load_vcs(&root, &workspace_auth);
        Self {
            model,
            tabs,
            themes,
            theme_index,
            palette,
            focus_handle: cx.focus_handle(),
            composer,
            explorer,
            vcs_status,
            vcs_history,
            settings,
            data_dir,
            migration_error,
            workspace_auth,
        }
    }

    pub fn restore_or_create_terminal(&mut self, cx: &mut Context<Self>) {
        let terminal_ids: Vec<TabId> = self
            .model
            .tabs
            .iter()
            .filter(|tab| tab.kind == TabKind::Terminal)
            .map(|tab| tab.id)
            .collect();
        if terminal_ids.is_empty() && self.model.tabs.is_empty() {
            self.create_terminal(false, cx);
        } else {
            for id in terminal_ids {
                let cwd = self
                    .model
                    .tabs
                    .iter()
                    .find(|tab| tab.id == id)
                    .map(|tab| tab.cwd.clone());
                self.spawn_terminal_into(id, 0, cwd, cx);
            }
        }
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
            panes: vec![PaneContent::Placeholder("Starting terminal…".into())],
        });
        self.activate_runtime(id, cx);
        self.spawn_terminal_into(id, 0, cwd, cx);
        cx.notify();
    }

    fn spawn_terminal_into(
        &mut self,
        tab_id: TabId,
        pane_index: usize,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let palette = self.palette.clone();
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
                            if let Some(pane) = tab.panes.get_mut(pane_index) {
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
                let entity = cx.new(|cx| TerminalView::from_bridge(bridge, palette, cx));
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    if let Some(pane) = tab.panes.get_mut(pane_index) {
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
            panes: vec![PaneContent::Editor(editor)],
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
        self.tabs.push(AppTab {
            id,
            panes: vec![PaneContent::Editor(entity)],
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
        let preview = cx.new(|cx| PreviewView::new(url, window, cx));
        self.tabs.push(AppTab {
            id,
            panes: vec![PaneContent::Preview(preview)],
        });
        self.activate_runtime(id, cx);
        cx.notify();
    }

    fn split_active(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(active) = self.model.active else {
            return;
        };
        if self.model.split_active(direction).is_err() {
            return;
        }
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
        let pane_index = if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active) {
            tab.panes.push(pane);
            tab.panes.len() - 1
        } else {
            return;
        };
        if kind == Some(TabKind::Terminal) {
            let cwd = self.model.active_tab().map(|tab| tab.cwd.clone());
            self.spawn_terminal_into(active, pane_index, cwd, cx);
        }
        cx.notify();
    }

    fn close_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.model.active else { return };
        let pane_count = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.panes.len())
            .unwrap_or(0);
        if self.model.close_active_pane_or_tab().is_ok() {
            if pane_count > 1 {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.panes.pop();
                }
            } else {
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
            for pane in &tab.panes {
                if let PaneContent::Preview(preview) = pane {
                    preview.update(cx, |preview, cx| preview.set_active(tab.id == id, cx));
                }
            }
        }
    }

    fn active_terminal(&self) -> Option<&Entity<TerminalView>> {
        let active = self.model.active?;
        self.tabs
            .iter()
            .find(|tab| tab.id == active)?
            .panes
            .iter()
            .find_map(|pane| match pane {
                PaneContent::Terminal(entity) => Some(entity),
                _ => None,
            })
    }

    fn active_editor(&self) -> Option<&Entity<EditorView>> {
        let active = self.model.active?;
        self.tabs
            .iter()
            .find(|tab| tab.id == active)?
            .panes
            .iter()
            .find_map(|pane| match pane {
                PaneContent::Editor(entity) => Some(entity),
                _ => None,
            })
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
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| {
                cx.new(|cx| {
                    SettingsView::new(settings, migration_error, data_dir, Some(on_save), cx)
                })
            },
        );
    }

    fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings = settings;
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
            for pane in &tab.panes {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor
                            .set_preferences(&self.settings.editor_theme_id, self.settings.vim_mode)
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

    fn global_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.to_ascii_lowercase();
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if modifiers.control && key == "tab" {
            self.model.cycle_tab(modifiers.shift);
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !primary {
            return;
        }
        let handled = match (key.as_str(), modifiers.shift) {
            ("t", false) => {
                self.create_terminal(false, cx);
                true
            }
            ("r", false) => {
                self.create_terminal(true, cx);
                true
            }
            ("e", false) => {
                self.create_editor(cx);
                true
            }
            ("e", true) => {
                self.model.sidebar_panel = SidebarPanel::Explorer;
                self.model.sidebar_visible = true;
                cx.notify();
                true
            }
            ("p", false) => {
                self.create_preview(window, cx);
                true
            }
            ("w", false) => {
                self.close_active(cx);
                true
            }
            ("b", false) => {
                self.model.sidebar_visible = !self.model.sidebar_visible;
                cx.notify();
                true
            }
            ("i", false) => {
                self.model.composer_visible = !self.model.composer_visible;
                cx.notify();
                true
            }
            ("l", false) => {
                self.attach_active_selection(cx);
                true
            }
            ("g", false) => {
                self.model.sidebar_panel = SidebarPanel::SourceControl;
                self.model.sidebar_visible = true;
                cx.notify();
                true
            }
            ("d", false) => {
                self.split_active(SplitDirection::Right, cx);
                true
            }
            ("d", true) => {
                self.split_active(SplitDirection::Down, cx);
                true
            }
            ("[", false) => {
                if let Some(tab) = self.model.active_tab_mut() {
                    tab.layout.focus_relative(-1);
                }
                cx.notify();
                true
            }
            ("]", false) => {
                if let Some(tab) = self.model.active_tab_mut() {
                    tab.layout.focus_relative(1);
                }
                cx.notify();
                true
            }
            (",", false) => {
                self.open_settings_window(cx);
                true
            }
            (digit, false)
                if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() && digit != "0" =>
            {
                let _ = self.model.switch_index(digit.parse().unwrap_or(1));
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                }
                cx.notify();
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn sidebar_content(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.model.sidebar_panel {
            SidebarPanel::Explorer => {
                let rows = self
                    .explorer
                    .as_ref()
                    .map(|index| {
                        index
                            .entries()
                            .iter()
                            .filter(|entry| !entry.is_dir)
                            .take(80)
                            .map(|entry| {
                                let path = entry.path.clone();
                                div()
                                    .id(SharedString::from(format!("file-{}", entry.relative)))
                                    .px_2()
                                    .py_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(SharedString::from(entry.relative.clone()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let path = path.clone();
                                            move |this, _event, _window, cx| {
                                                this.open_editor(path.clone(), cx)
                                            }
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.attach_file(path.clone(), cx)
                                        }),
                                    )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                div().flex().flex_col().children(rows).into_any_element()
            }
            SidebarPanel::SourceControl => {
                let rows = self
                    .vcs_status
                    .iter()
                    .take(80)
                    .map(|file| {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .child(SharedString::from(format!(
                                "{:?}  {}",
                                file.group, file.path
                            )))
                    })
                    .collect::<Vec<_>>();
                div().flex().flex_col().children(rows).into_any_element()
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
        }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                let direction = self
                    .model
                    .active_tab()
                    .and_then(|tab| match &tab.layout.root {
                        LayoutNode::Split { direction, .. } => Some(*direction),
                        _ => None,
                    });
                let panes = tab
                    .panes
                    .iter()
                    .map(PaneContent::element)
                    .collect::<Vec<_>>();
                let mut row = div().flex().size_full();
                row = match direction {
                    Some(SplitDirection::Down) => row.flex_col(),
                    _ => row.flex_row(),
                };
                row.children(panes)
            })
            .unwrap_or_else(|| {
                div()
                    .flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child("No tabs. Press Ctrl/Cmd+T for a terminal.")
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
        let agent_states = self
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter().map(move |pane| (tab.id, pane)))
            .filter_map(|(tab_id, pane)| match pane {
                PaneContent::Terminal(terminal) => terminal
                    .read(cx)
                    .agent_state()
                    .map(|state| format!("{}:{state:?}", tab_id.0)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let bell_label = if agent_states.is_empty() {
            "🔔".to_owned()
        } else {
            format!("🔔 {}", agent_states.join(" · "))
        };
        div()
            .id("workspace-root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::global_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(p.background))
            .text_color(gpui_color(p.foreground))
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
                            .text_xs()
                            .child(SharedString::from(bell_label)),
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
) -> (Vec<ChangedFile>, Vec<CommitInfo>) {
    match GitRepository::open(root, workspace_auth) {
        Ok(repo) => (
            repo.status().unwrap_or_default(),
            repo.history(100, None).unwrap_or_default(),
        ),
        Err(_) => (Vec::new(), Vec::new()),
    }
}
