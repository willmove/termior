//! Persistent business state for tabs, panes, sidebar, settings and Composer mounting (FR-WS).

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use termior_ui_kit::{LayoutError, PaneId, PaneLayout, SplitDirection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TabId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabKind {
    Terminal,
    Editor,
    Preview,
    Markdown,
    AiDiff,
    GitDiff,
    GitHistory,
    GitCommitFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub id: TabId,
    pub kind: TabKind,
    pub title: String,
    pub cwd: PathBuf,
    /// 项目文件夹锚点：Explorer/Git/状态栏跟随它。只被"打开文件夹"与新建继承改变，
    /// 不随 shell cd（OSC 7）漂移。旧格式工作区文件无此字段，恢复时回填为 root。
    #[serde(default)]
    pub project_dir: PathBuf,
    /// File or URL represented by this tab, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Private terminal tabs do not inherit the active tab's cwd or application environment.
    #[serde(default)]
    pub private_terminal: bool,
    pub layout: PaneLayout,
    /// View owners increment this for meaningful state changes; tab switching never does.
    pub state_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarPanel {
    Explorer,
    SourceControl,
    GitHistory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsPage {
    General,
    Models,
    Themes,
    Shortcuts,
    Agents,
    About,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub root: PathBuf,
    pub tabs: Vec<TabState>,
    pub active: Option<TabId>,
    pub sidebar_visible: bool,
    pub sidebar_panel: SidebarPanel,
    /// Persisted width of the activity bar plus sidebar panel. Older workspace
    /// files predate this field, so serde restores the product default.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    pub composer_visible: bool,
    pub localhost_preview: Option<String>,
    pub ai_tools_running: usize,
    next_tab_id: u64,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum WorkspaceError {
    #[error("tab not found: {0:?}")]
    TabNotFound(TabId),
    #[error(transparent)]
    Layout(#[from] LayoutError),
}

impl WorkspaceState {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            tabs: Vec::new(),
            active: None,
            sidebar_visible: true,
            sidebar_panel: SidebarPanel::Explorer,
            sidebar_width: default_sidebar_width(),
            composer_visible: true,
            localhost_preview: None,
            ai_tools_running: 0,
            next_tab_id: 1,
        }
    }

    pub fn tab(&self, id: TabId) -> Option<&TabState> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut TabState> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn active_tab(&self) -> Option<&TabState> {
        self.tab(self.active?)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut TabState> {
        let id = self.active?;
        self.tab_mut(id)
    }

    pub fn new_tab(&mut self, kind: TabKind, title: impl Into<String>, private: bool) -> TabId {
        let valid = |dir: &Path| !dir.as_os_str().is_empty();
        let (cwd, project_dir) = if private {
            (self.root.clone(), self.root.clone())
        } else {
            let active = self.active_tab();
            (
                active
                    .map(|tab| tab.cwd.clone())
                    .filter(|dir| valid(dir))
                    .unwrap_or_else(|| self.root.clone()),
                active
                    .map(|tab| tab.project_dir.clone())
                    .filter(|dir| valid(dir))
                    .unwrap_or_else(|| self.root.clone()),
            )
        };
        let id = TabId(self.next_tab_id);
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        self.tabs.push(TabState {
            id,
            kind,
            title: title.into(),
            cwd,
            project_dir,
            resource: None,
            private_terminal: kind == TabKind::Terminal && private,
            layout: PaneLayout::new(),
            state_generation: 0,
        });
        self.active = Some(id);
        id
    }

    pub fn switch_to(&mut self, id: TabId) -> Result<(), WorkspaceError> {
        if self.tabs.iter().any(|tab| tab.id == id) {
            self.active = Some(id);
            Ok(())
        } else {
            Err(WorkspaceError::TabNotFound(id))
        }
    }

    pub fn switch_index(&mut self, one_based: usize) -> Result<(), WorkspaceError> {
        let tab = self
            .tabs
            .get(one_based.saturating_sub(1))
            .ok_or(WorkspaceError::TabNotFound(TabId(one_based as u64)))?;
        self.active = Some(tab.id);
        Ok(())
    }

    pub fn cycle_tab(&mut self, backwards: bool) {
        if self.tabs.is_empty() {
            self.active = None;
            return;
        }
        let current = self
            .active
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .unwrap_or(0);
        let delta = if backwards { self.tabs.len() - 1 } else { 1 };
        self.active = Some(self.tabs[(current + delta) % self.tabs.len()].id);
    }

    pub fn close_active_pane_or_tab(&mut self) -> Result<Option<PaneId>, WorkspaceError> {
        let id = self.active.ok_or(WorkspaceError::TabNotFound(TabId(0)))?;
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == id)
            .ok_or(WorkspaceError::TabNotFound(id))?;
        if self.tabs[index].layout.panes().len() > 1 {
            return Ok(Some(self.tabs[index].layout.close_focused()?));
        }
        self.tabs.remove(index);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(self.tabs[index.min(self.tabs.len() - 1)].id)
        };
        Ok(None)
    }

    /// Close a specific pane in a specific tab (e.g. its terminal process exited).
    ///
    /// Like [`Self::close_active_pane_or_tab`], but targets any tab/pane pair:
    /// closing one of several panes returns `Ok(Some(pane))`; closing the tab's
    /// final pane removes the whole tab and returns `Ok(None)`, moving `active`
    /// to a neighbouring tab when the removed tab was active.
    pub fn close_pane(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
    ) -> Result<Option<PaneId>, WorkspaceError> {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or(WorkspaceError::TabNotFound(tab_id))?;
        if self.tabs[index].layout.panes().len() > 1 {
            return Ok(Some(self.tabs[index].layout.close_pane(pane_id)?));
        }
        self.tabs.remove(index);
        if self.active == Some(tab_id) {
            self.active = if self.tabs.is_empty() {
                None
            } else {
                Some(self.tabs[index.min(self.tabs.len() - 1)].id)
            };
        }
        Ok(None)
    }

    pub fn split_active(&mut self, direction: SplitDirection) -> Result<PaneId, WorkspaceError> {
        let tab = self
            .active_tab_mut()
            .ok_or(WorkspaceError::TabNotFound(TabId(0)))?;
        Ok(tab.layout.split_focused(direction))
    }

    pub fn close_other_panes(&mut self) -> Result<Vec<PaneId>, WorkspaceError> {
        let tab = self
            .active_tab_mut()
            .ok_or(WorkspaceError::TabNotFound(TabId(0)))?;
        Ok(tab.layout.close_other_panes())
    }

    pub fn set_active_cwd(&mut self, cwd: impl Into<PathBuf>) {
        if let Some(tab) = self.active_tab_mut() {
            tab.cwd = cwd.into();
        }
    }

    /// 重定向活动 Tab 的项目文件夹（"打开文件夹"的 Tab 作用域语义）。
    /// cwd 一并置为新目录：下一次 split/新建继承立即正确，不依赖 OSC 7 回环。
    pub fn set_active_project_dir(&mut self, dir: impl Into<PathBuf>) {
        if let Some(tab) = self.active_tab_mut() {
            let dir = dir.into();
            tab.cwd = dir.clone();
            tab.project_dir = dir;
        }
    }

    /// 活动 Tab 的项目文件夹（Explorer/Git/状态栏的跟随根）。
    /// 无活动 Tab 或字段为空（未回填的旧数据）时回落全局兜底根。
    pub fn active_project_dir(&self) -> &Path {
        self.active_tab()
            .map(|tab| tab.project_dir.as_path())
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(self.root.as_path())
    }

    /// 旧格式工作区文件（Tab 无 `project_dir`）恢复后调用：空的 `project_dir`
    /// 回填为全局 root，语义与旧版"全局唯一文件夹"等价。
    pub fn backfill_project_dirs(&mut self) {
        let root = self.root.clone();
        for tab in &mut self.tabs {
            if tab.project_dir.as_os_str().is_empty() {
                tab.project_dir = root.clone();
            }
        }
    }

    pub fn cwd_breadcrumbs(&self) -> Vec<String> {
        self.active_tab()
            .map(|tab| {
                tab.cwd
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn tab_for_path(&self, path: &Path) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|tab| tab.cwd == path)
            .map(|tab| tab.id)
    }
}

fn default_sidebar_width() -> f32 {
    280.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_eight_tab_kinds_roundtrip() {
        let mut ws = WorkspaceState::new("/workspace");
        for kind in [
            TabKind::Terminal,
            TabKind::Editor,
            TabKind::Preview,
            TabKind::Markdown,
            TabKind::AiDiff,
            TabKind::GitDiff,
            TabKind::GitHistory,
            TabKind::GitCommitFile,
        ] {
            ws.new_tab(kind, format!("{kind:?}"), false);
        }
        let json = serde_json::to_string(&ws).unwrap();
        let restored: WorkspaceState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tabs.len(), 8);
    }

    #[test]
    fn switching_preserves_state_and_new_tab_inherits_cwd() {
        let mut ws = WorkspaceState::new("/workspace");
        let first = ws.new_tab(TabKind::Terminal, "terminal", false);
        ws.set_active_cwd("/workspace/app");
        ws.active_tab_mut().unwrap().state_generation = 9;
        let second = ws.new_tab(TabKind::Editor, "editor", false);
        assert_eq!(ws.active_tab().unwrap().cwd, Path::new("/workspace/app"));
        ws.switch_to(first).unwrap();
        assert_eq!(ws.active_tab().unwrap().state_generation, 9);
        ws.switch_to(second).unwrap();
    }

    #[test]
    fn private_terminal_uses_workspace_root_and_persists_privacy() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "terminal", false);
        ws.set_active_cwd("/workspace/app");
        ws.new_tab(TabKind::Terminal, "private", true);
        let private = ws.active_tab().unwrap();
        assert_eq!(private.cwd, Path::new("/workspace"));
        assert!(private.private_terminal);

        let restored: WorkspaceState =
            serde_json::from_str(&serde_json::to_string(&ws).unwrap()).unwrap();
        assert!(restored.active_tab().unwrap().private_terminal);
    }

    #[test]
    fn close_pane_then_tab() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "terminal", false);
        ws.split_active(SplitDirection::Right).unwrap();
        ws.close_active_pane_or_tab().unwrap();
        assert_eq!(ws.tabs.len(), 1);
        ws.close_active_pane_or_tab().unwrap();
        assert!(ws.tabs.is_empty());
    }

    #[test]
    fn close_active_with_stale_active_id_returns_error() {
        let mut ws = WorkspaceState::new("/workspace");
        let id = ws.new_tab(TabKind::Terminal, "terminal", false);
        ws.active = Some(TabId(id.0.saturating_add(99)));
        assert_eq!(
            ws.close_active_pane_or_tab(),
            Err(WorkspaceError::TabNotFound(TabId(id.0.saturating_add(99))))
        );
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].id, id);
    }

    #[test]
    fn tab_lookup_is_independent_of_active() {
        let mut ws = WorkspaceState::new("/workspace");
        let first = ws.new_tab(TabKind::Terminal, "one", false);
        let second = ws.new_tab(TabKind::Editor, "two", false);
        assert_eq!(ws.active, Some(second));
        assert_eq!(ws.tab(first).map(|tab| tab.title.as_str()), Some("one"));
        assert!(ws.tab(TabId(999)).is_none());
    }

    #[test]
    fn close_specific_pane_in_inactive_tab() {
        let mut ws = WorkspaceState::new("/workspace");
        let first = ws.new_tab(TabKind::Terminal, "one", false);
        let first_pane = ws.active_tab().unwrap().layout.focused;
        let second_pane = ws.split_active(SplitDirection::Right).unwrap();
        let second = ws.new_tab(TabKind::Terminal, "two", false);

        // Closing an unfocused pane of a background tab keeps that tab and its focus.
        assert_eq!(
            ws.close_pane(first, second_pane).unwrap(),
            Some(second_pane)
        );
        let first_tab = ws.tabs.iter().find(|tab| tab.id == first).unwrap();
        assert_eq!(first_tab.layout.panes(), vec![first_pane]);
        assert_eq!(first_tab.layout.focused, first_pane);
        assert_eq!(ws.active, Some(second));

        // Closing a background tab's final pane removes the tab, active stays.
        assert_eq!(ws.close_pane(first, first_pane).unwrap(), None);
        assert!(ws.tabs.iter().all(|tab| tab.id != first));
        assert_eq!(ws.active, Some(second));

        // Closing the active tab's final pane moves active to a neighbour.
        let last_pane = ws.active_tab().unwrap().layout.focused;
        assert_eq!(ws.close_pane(second, last_pane).unwrap(), None);
        assert!(ws.tabs.is_empty());
        assert_eq!(ws.active, None);
    }

    #[test]
    fn close_other_panes_keeps_the_focused_pane() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Editor, "editor", false);
        let second = ws.split_active(SplitDirection::Right).unwrap();
        ws.split_active(SplitDirection::Down).unwrap();
        ws.active_tab_mut().unwrap().layout.focus(second).unwrap();

        let removed = ws.close_other_panes().unwrap();

        assert_eq!(removed.len(), 2);
        assert_eq!(ws.active_tab().unwrap().layout.panes(), vec![second]);
    }

    #[test]
    fn legacy_workspace_json_restores_default_sidebar_width() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "terminal", false);
        let mut value = serde_json::to_value(&ws).unwrap();
        value.as_object_mut().unwrap().remove("sidebar_width");

        let restored: WorkspaceState = serde_json::from_value(value).unwrap();

        assert_eq!(restored.sidebar_width, 280.0);
    }

    #[test]
    fn new_tab_inherits_active_tab_project_dir() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "a", false);
        ws.set_active_project_dir("/proj-a");

        let second = ws.new_tab(TabKind::Terminal, "b", false);

        let tab = ws.tabs.iter().find(|tab| tab.id == second).unwrap();
        assert_eq!(tab.project_dir, PathBuf::from("/proj-a"));
    }

    #[test]
    fn private_terminal_uses_workspace_root_project_dir() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "a", false);
        ws.set_active_project_dir("/proj-a");

        let private = ws.new_tab(TabKind::Terminal, "priv", true);

        let tab = ws.tabs.iter().find(|tab| tab.id == private).unwrap();
        assert_eq!(tab.project_dir, PathBuf::from("/workspace"));
        assert_eq!(tab.cwd, PathBuf::from("/workspace"));
    }

    #[test]
    fn new_tab_without_active_tab_falls_back_to_root() {
        let mut ws = WorkspaceState::new("/workspace");

        let id = ws.new_tab(TabKind::Editor, "doc", false);

        let tab = ws.tabs.iter().find(|tab| tab.id == id).unwrap();
        assert_eq!(tab.project_dir, PathBuf::from("/workspace"));
    }

    #[test]
    fn set_active_project_dir_moves_cwd_and_project_dir() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "a", false);

        ws.set_active_project_dir("/proj-b");

        let tab = ws.active_tab().unwrap();
        assert_eq!(tab.project_dir, PathBuf::from("/proj-b"));
        assert_eq!(tab.cwd, PathBuf::from("/proj-b"));
    }

    #[test]
    fn set_active_project_dir_leaves_other_tabs_untouched() {
        let mut ws = WorkspaceState::new("/workspace");
        let first = ws.new_tab(TabKind::Terminal, "a", false);
        ws.set_active_project_dir("/proj-a");
        ws.new_tab(TabKind::Terminal, "b", false);

        ws.set_active_project_dir("/proj-b");

        let first_tab = ws.tabs.iter().find(|tab| tab.id == first).unwrap();
        assert_eq!(first_tab.project_dir, PathBuf::from("/proj-a"));
        assert_eq!(first_tab.cwd, PathBuf::from("/proj-a"));
    }

    #[test]
    fn active_project_dir_falls_back_to_root_without_tabs() {
        let ws = WorkspaceState::new("/workspace");
        assert_eq!(ws.active_project_dir(), Path::new("/workspace"));
    }

    #[test]
    fn legacy_workspace_json_backfills_project_dir_from_root() {
        let mut ws = WorkspaceState::new("/legacy-root");
        ws.new_tab(TabKind::Terminal, "a", false);
        ws.new_tab(TabKind::Editor, "doc", false);
        let mut value = serde_json::to_value(&ws).unwrap();
        // 旧格式：Tab 没有 project_dir 字段。
        for tab in value["tabs"].as_array_mut().unwrap() {
            tab.as_object_mut().unwrap().remove("project_dir");
        }

        let mut restored: WorkspaceState = serde_json::from_value(value).unwrap();
        restored.backfill_project_dirs();

        assert!(restored
            .tabs
            .iter()
            .all(|tab| tab.project_dir == Path::new("/legacy-root")));
        assert_eq!(restored.active_project_dir(), Path::new("/legacy-root"));
    }

    #[test]
    fn shell_cd_drift_does_not_move_project_anchor() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "a", false);
        ws.set_active_project_dir("/proj-a");

        // OSC 7 同步只更新 cwd，不得拖动 project_dir。
        ws.set_active_cwd("/proj-a/src/deep");

        let tab = ws.active_tab().unwrap();
        assert_eq!(tab.cwd, PathBuf::from("/proj-a/src/deep"));
        assert_eq!(tab.project_dir, PathBuf::from("/proj-a"));
        assert_eq!(ws.active_project_dir(), Path::new("/proj-a"));
    }
}
