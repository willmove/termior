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
    /// File or URL represented by this tab, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
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
            composer_visible: true,
            localhost_preview: None,
            ai_tools_running: 0,
            next_tab_id: 1,
        }
    }

    pub fn active_tab(&self) -> Option<&TabState> {
        let id = self.active?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut TabState> {
        let id = self.active?;
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn new_tab(&mut self, kind: TabKind, title: impl Into<String>, private: bool) -> TabId {
        let cwd = if private {
            self.root.clone()
        } else {
            self.active_tab()
                .map(|tab| tab.cwd.clone())
                .unwrap_or_else(|| self.root.clone())
        };
        let id = TabId(self.next_tab_id);
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        self.tabs.push(TabState {
            id,
            kind,
            title: title.into(),
            cwd,
            resource: None,
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
        let tab = self.active_tab_mut().expect("active tab exists");
        if tab.layout.panes().len() > 1 {
            return Ok(Some(tab.layout.close_focused()?));
        }
        let index = self.tabs.iter().position(|tab| tab.id == id).unwrap();
        self.tabs.remove(index);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(self.tabs[index.min(self.tabs.len() - 1)].id)
        };
        Ok(None)
    }

    pub fn split_active(&mut self, direction: SplitDirection) -> Result<PaneId, WorkspaceError> {
        let tab = self
            .active_tab_mut()
            .ok_or(WorkspaceError::TabNotFound(TabId(0)))?;
        Ok(tab.layout.split_focused(direction))
    }

    pub fn set_active_cwd(&mut self, cwd: impl Into<PathBuf>) {
        if let Some(tab) = self.active_tab_mut() {
            tab.cwd = cwd.into();
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
    fn close_pane_then_tab() {
        let mut ws = WorkspaceState::new("/workspace");
        ws.new_tab(TabKind::Terminal, "terminal", false);
        ws.split_active(SplitDirection::Right).unwrap();
        ws.close_active_pane_or_tab().unwrap();
        assert_eq!(ws.tabs.len(), 1);
        ws.close_active_pane_or_tab().unwrap();
        assert!(ws.tabs.is_empty());
    }
}
