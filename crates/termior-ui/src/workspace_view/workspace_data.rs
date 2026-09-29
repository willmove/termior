//! 工作区数据：Explorer/VCS 扫描调度、工作区切换与持久化。

use super::*;

pub fn load_settings() -> (Settings, Option<PathBuf>, Option<String>) {
    let data_dir = app_data_dir().ok();
    let Some(dir) = data_dir.as_ref() else {
        return (
            default_settings(),
            None,
            Some(t!("ws.data_dir_error").to_string()),
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

pub(super) struct VcsSnapshot {
    pub(super) root: PathBuf,
    pub(super) status: Vec<ChangedFile>,
    pub(super) history: Vec<CommitInfo>,
    pub(super) branch: Option<BranchState>,
}

pub(super) fn skip_startup_workspace_scans() -> bool {
    [
        "TERMIOR_SMOKE_TEST",
        "TERMIOR_MARKDOWN_PREVIEW_SMOKE_TEST",
        "TERMIOR_SETTINGS_CLOSE_SMOKE_TEST",
        "TERMIOR_SPLIT_PANE_SMOKE",
        "TERMIOR_NFR_MEASURE",
        "TERMIOR_IDLE_REDRAW_PROBE",
        "TERMIOR_SHELL_PICKER_SMOKE",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
}

/// Git status/history/branch snapshot. Must not run on the GPUI thread: `status` plus
/// `history(100)` walk the object database and freeze the UI on large repositories.
pub(super) fn load_vcs(root: &Path, workspace_auth: &WorkspaceAuthRegistry) -> VcsSnapshot {
    let (status, history, branch) = match GitRepository::open(root, workspace_auth) {
        Ok(repo) => (
            repo.status().unwrap_or_default(),
            repo.history(100, None).unwrap_or_default(),
            repo.branch_state().ok(),
        ),
        Err(_) => (Vec::new(), Vec::new(), None),
    };
    VcsSnapshot {
        root: root.to_path_buf(),
        status,
        history,
        branch,
    }
}

impl WorkspaceView {
    pub(super) fn schedule_explorer_scan(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if !root.is_dir() {
            self.explorer_deep_indexing = false;
            self.explorer_scan_in_progress = false;
            self.explorer_pending_rescan = false;
            self.explorer_rescan_after = None;
            self.explorer_index_incomplete = false;
            self.explorer = None;
            self.explorer_skips_expanded = false;
            self.explorer_error = Some(t!("explorer.root_invalid").into());
            cx.notify();
            return;
        }

        self.explorer_requested_root = root.clone();
        self.explorer_tree.set_root(root.clone());
        self.explorer_deep_indexing = true;
        self.explorer_scan_in_progress = true;
        self.explorer_pending_rescan = false;
        self.explorer_rescan_after = None;
        self.explorer_index_incomplete = false;
        self.explorer_error = None;
        self.explorer_skips_expanded = false;
        self.explorer_scan_generation = self.explorer_scan_generation.saturating_add(1);
        let generation = self.explorer_scan_generation;
        let show_dotfiles = self.settings.show_dotfiles;

        let shallow_root = root.clone();
        let shallow = cx
            .background_executor()
            .spawn(async move { FileIndex::build_shallow(shallow_root, show_dotfiles) });
        let deep = cx
            .background_executor()
            .spawn(async move { FileIndex::build(root, show_dotfiles) });

        cx.spawn(async move |workspace, cx| {
            let shallow_result = shallow.await;
            let continue_deep = workspace
                .update(cx, |workspace, cx| {
                    if generation != workspace.explorer_scan_generation {
                        return false;
                    }
                    match shallow_result {
                        Ok(index) => {
                            workspace.apply_explorer_index(index, cx);
                            // Keep deep-indexing indicator on while the full walk runs.
                            workspace.explorer_deep_indexing = true;
                        }
                        Err(_) => {
                            workspace.explorer = None;
                            workspace.explorer_skips_expanded = false;
                            workspace.explorer_deep_indexing = false;
                            workspace.explorer_error = Some(t!("explorer.root_invalid").into());
                            workspace.finish_explorer_scan(cx);
                        }
                    }
                    cx.notify();
                    generation == workspace.explorer_scan_generation
                        && workspace.explorer_scan_in_progress
                })
                .unwrap_or(false);

            if !continue_deep {
                return;
            }

            let timeout = cx.background_executor().timer(EXPLORER_DEEP_SCAN_TIMEOUT);
            match future::select(Box::pin(deep), Box::pin(timeout)).await {
                Either::Left((deep_result, _)) => {
                    let _ = workspace.update(cx, |workspace, cx| {
                        if generation != workspace.explorer_scan_generation {
                            return;
                        }
                        match deep_result {
                            Ok(index) => {
                                workspace.apply_explorer_index(index, cx);
                                workspace.explorer_index_incomplete = false;
                                workspace.explorer_deep_indexing = false;
                                workspace.finish_explorer_scan(cx);
                            }
                            Err(_) => {
                                // Keep any shallow results; only fail hard if we have none.
                                if workspace.explorer.is_none() {
                                    workspace.explorer_error =
                                        Some(t!("explorer.root_invalid").into());
                                }
                                workspace.explorer_deep_indexing = false;
                                workspace.finish_explorer_scan(cx);
                            }
                        }
                        cx.notify();
                    });
                }
                Either::Right((_, deep_fut)) => {
                    let _ = workspace.update(cx, |workspace, cx| {
                        if generation != workspace.explorer_scan_generation {
                            return;
                        }
                        // Soft timeout: keep visible results, stop blocking on indexing.
                        workspace.explorer_deep_indexing = false;
                        workspace.explorer_index_incomplete = workspace.explorer.is_some();
                        cx.notify();
                    });
                    let deep_result = deep_fut.await;
                    let _ = workspace.update(cx, |workspace, cx| {
                        if generation != workspace.explorer_scan_generation {
                            return;
                        }
                        if let Ok(index) = deep_result {
                            workspace.apply_explorer_index(index, cx);
                            workspace.explorer_index_incomplete = false;
                        }
                        workspace.explorer_deep_indexing = false;
                        workspace.finish_explorer_scan(cx);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }
    fn apply_explorer_index(&mut self, index: FileIndex, cx: &mut Context<Self>) {
        let root_changed = self
            .explorer
            .as_ref()
            .map_or(true, |current| current.root() != index.root());
        if root_changed || self.explorer_watcher.is_none() {
            self.explorer_watcher = WorkspaceWatcher::watch(index.root()).ok();
        }
        let paths = index
            .entries()
            .iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.relative.clone())
            .collect();
        self.composer
            .update(cx, |composer, _| composer.set_workspace_paths(paths));
        if index.skipped().is_empty() {
            self.explorer_skips_expanded = false;
        }
        self.explorer = Some(index);
        self.explorer_error = None;
    }
    fn finish_explorer_scan(&mut self, cx: &mut Context<Self>) {
        self.explorer_scan_in_progress = false;
        if self.explorer_pending_rescan {
            self.explorer_pending_rescan = false;
            let root = self.explorer_requested_root.clone();
            self.schedule_explorer_scan(root, cx);
            self.refresh_vcs_data(cx);
        }
    }
    pub(super) fn refresh_workspace_data(&mut self, cx: &mut Context<Self>) {
        if self.refresh_remote_explorer(cx) {
            return;
        }
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
        self.refresh_vcs_data(cx);
    }
    pub(super) fn refresh_vcs_data(&mut self, cx: &mut Context<Self>) {
        let root = self.model.active_project_dir().to_path_buf();
        self.vcs_scan_generation = self.vcs_scan_generation.saturating_add(1);
        let generation = self.vcs_scan_generation;
        let auth = self.workspace_auth.clone();
        let root_for_task = root.clone();
        let task = cx.background_executor().spawn(async move {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_vcs(&root, &auth)))
                .unwrap_or_else(|_| VcsSnapshot {
                    root: root_for_task,
                    status: Vec::new(),
                    history: Vec::new(),
                    branch: None,
                })
        });
        cx.spawn(async move |workspace, cx| {
            let snapshot = task.await;
            let _ = workspace.update(cx, |workspace, cx| {
                if generation != workspace.vcs_scan_generation {
                    return;
                }
                if snapshot.root != workspace.model.active_project_dir() {
                    return;
                }
                workspace.apply_vcs_snapshot(snapshot, cx);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn apply_vcs_snapshot(&mut self, snapshot: VcsSnapshot, cx: &mut Context<Self>) {
        self.vcs_status = snapshot.status;
        self.vcs_history = snapshot.history;
        self.vcs_branch = snapshot.branch;
        let commits = self.vcs_history.clone();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::GitHistory(history) = pane {
                    history.update(cx, |history, cx| history.set_commits(commits.clone(), cx));
                }
            }
        }
    }
    pub(super) fn schedule_commit_files(
        &mut self,
        history: Entity<GitHistoryView>,
        commit: String,
        cx: &mut Context<Self>,
    ) {
        let root = self.model.active_project_dir().to_path_buf();
        let auth = self.workspace_auth.clone();
        let commit_id = commit.clone();
        let task = cx.background_executor().spawn(async move {
            GitRepository::open(&root, &auth)
                .ok()
                .and_then(|repo| repo.commit_files(&commit_id).ok())
                .unwrap_or_default()
        });
        cx.spawn(async move |workspace, cx| {
            let files = task.await;
            let _ = workspace.update(cx, |_, cx| {
                history.update(cx, |history, cx| {
                    history.set_commit_files(commit, files, cx);
                });
            });
        })
        .detach();
    }
    pub(super) fn open_workspace_picker(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_workspace(window, cx);
    }
    /// 打开/切换项目文件夹：弹出异步文件夹选择框，选中后只重定向**活动 Tab**
    /// （其他 Tab 的项目文件夹与运行时全部保留）。状态栏按钮、标题栏按钮与
    /// `Cmd/Ctrl+O` 快捷键共用此逻辑。
    pub(super) fn open_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The blocking `rfd::FileDialog` must never run inside a GPUI event
        // handler: on Windows its modal message loop re-enters the foreground
        // executor while the `App` RefCell is still borrowed, aborting the
        // process. Await the async dialog instead and hop back into the view.
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("ws.open_workspace_dialog").to_string())
                .pick_folder()
                .await
            else {
                return;
            };
            let root = folder.path().to_path_buf();
            let _ = workspace.update_in(cx, |workspace, _window, cx| {
                if root.is_dir() {
                    workspace.retarget_active_project(root, cx);
                }
            });
        })
        .detach();
    }
    /// "打开文件夹"的 Tab 作用域语义：只重定向活动 Tab 的项目文件夹。
    /// 运行中的终端 pane 注入 cd（会话不中断）；空工作区退回旧语义——设为兜底根。
    fn retarget_active_project(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.model.active.is_none() {
            // Explorer 可能还停在已关闭 Tab 的项目根上——根相同但 Explorer 滞留时也要重扫。
            if root == self.model.root && self.explorer_requested_root == root {
                return;
            }
            self.model.root = root.clone();
            self.workspace_auth
                .authorize(&root.to_string_lossy().replace('\\', "/"));
            self.schedule_explorer_scan(root, cx);
            self.refresh_vcs_data(cx);
            self.persist_workspace();
            cx.notify();
            return;
        }
        if self.model.active_project_dir() == root.as_path() {
            return;
        }
        // 文件对话框是用户的显式授权行为：直接加入注册表，不再二次弹窗。
        self.workspace_auth
            .authorize(&root.to_string_lossy().replace('\\', "/"));
        // 先收集活动 Tab 的全部运行中终端 pane，重定向后向每个 shell 注入 cd。
        let active_id = self.model.active;
        let terminals: Vec<Entity<TerminalView>> = active_id
            .and_then(|id| self.tabs.iter().find(|tab| tab.id == id))
            .map(|tab| {
                tab.panes
                    .values()
                    .filter_map(|pane| match pane {
                        PaneContent::Terminal(entity) => Some(entity.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.model.set_active_project_dir(root.clone());
        for terminal in terminals {
            let command = terminal.read(cx).cd_command_to(&root);
            if let Err(error) = terminal.read(cx).write_input(command.as_bytes()) {
                log::warn!("failed to inject cd into terminal: {error}");
            }
        }
        self.schedule_explorer_scan(root, cx);
        self.refresh_vcs_data(cx);
        self.persist_workspace();
        cx.notify();
    }
    pub(super) fn persist_workspace(&self) {
        if let Some(dir) = &self.data_dir {
            if let Ok(raw) = serde_json::to_string_pretty(&self.model) {
                let _ = atomic_write(&dir.join("Termior-workspaces.json"), &raw);
            }
        }
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
