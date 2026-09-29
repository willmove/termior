//! 各类标签页的创建与打开：编辑器、Markdown/Web 预览、AI/Git 差异与历史。

use super::commands::CommandMode;
use super::helpers::{new_preview_view, single_pane};
use super::workspace_data::load_vcs;
use super::*;

impl WorkspaceView {
    pub(super) fn create_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self
            .model
            .new_tab(TabKind::Editor, t!("ws.tab_untitled"), false);
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
        self.focus_active_pane(window, cx);
        cx.notify();
    }
    pub(super) fn open_editor(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            self.focus_active_pane(window, cx);
            cx.notify();
            return;
        }
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| t!("ws.tab_editor").into());
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
        // 打开后立即聚焦编辑器，保证可以立即输入（含从 Markdown 预览切回源码）。
        self.focus_active_pane(window, cx);
        cx.notify();
    }
    pub(super) fn request_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.command_message = Some(t!("ws.no_dev_server").to_string());
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
            .map(str::to_owned)
            .unwrap_or_else(|| t!("ws.tab_markdown").into());
        let id = self.model.new_tab(
            TabKind::Markdown,
            tf!("ws.tab_preview", "title" => source_title),
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
    pub(super) fn create_preview_url(
        &mut self,
        url: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self
            .model
            .new_tab(TabKind::Preview, t!("ws.tab_web_preview"), false);
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
    /// 源码/预览 切换：Markdown 编辑器与其实时预览是同一份文档的两个视图。
    /// 在编辑器上点“Preview”打开/聚焦预览标签；在预览上点“Source”回到对应编辑器标签。
    pub(super) fn toggle_markdown_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.model.active;
        let active_kind = active
            .and_then(|id| self.model.tabs.iter().find(|tab| tab.id == id))
            .map(|tab| tab.kind);
        match active_kind {
            Some(TabKind::Markdown) => {
                // 回到源文件：优先激活已有的编辑器标签，缺失时按路径重新打开。
                let resource = active
                    .and_then(|id| self.model.tabs.iter().find(|tab| tab.id == id))
                    .and_then(|tab| tab.resource.clone());
                let editor_id = resource.as_deref().and_then(|resource| {
                    self.model
                        .tabs
                        .iter()
                        .find(|tab| {
                            tab.kind == TabKind::Editor && tab.resource.as_deref() == Some(resource)
                        })
                        .map(|tab| tab.id)
                });
                if let Some(editor_id) = editor_id {
                    self.activate_runtime(editor_id, cx);
                    self.focus_active_pane(window, cx);
                    cx.notify();
                } else if let Some(resource) = resource {
                    self.open_editor(PathBuf::from(resource), window, cx);
                }
            }
            Some(TabKind::Editor) => {
                // 打开（或聚焦）当前 Markdown 文件的实时预览。
                let _ = self.request_markdown_preview(cx);
            }
            _ => {}
        }
    }
    pub(super) fn open_ai_diff(
        &mut self,
        summary: termior_ai::EditProposalSummary,
        cx: &mut Context<Self>,
    ) {
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
            .map(|name| tf!("ws.tab_ai_diff", "name" => name))
            .unwrap_or_else(|| t!("ws.tab_ai_diff_plain"));
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
    pub(super) fn open_git_diff(
        &mut self,
        path: String,
        group: ChangeGroup,
        cx: &mut Context<Self>,
    ) {
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
            .unwrap_or_else(|error| tf!("ws.diff_load_failed", "error" => error).to_string());
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| tf!("ws.tab_git_diff", "name" => name))
            .unwrap_or_else(|| t!("ws.tab_git_diff_plain"));
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
                        .map(|_| t!("ws.hunk_staged").to_string())
                        .map_err(|error| error.to_string()),
                ),
                GitDiffAction::UnstageHunk { path, patch } => (
                    path,
                    ChangeGroup::Staged,
                    repo.unstage_hunk(&patch)
                        .map(|_| t!("ws.hunk_unstaged").to_string())
                        .map_err(|error| error.to_string()),
                ),
                GitDiffAction::StageFile(path) => {
                    let result = repo
                        .stage_file(&path)
                        .map(|_| t!("ws.file_staged").to_string())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Staged, result)
                }
                GitDiffAction::UnstageFile(path) => {
                    let result = repo
                        .unstage_file(&path)
                        .map(|_| t!("ws.file_unstaged").to_string())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Unstaged, result)
                }
                GitDiffAction::DiscardFile(path) => {
                    let result = repo
                        .discard_file(&path)
                        .map(|_| t!("ws.working_tree_discarded").to_string())
                        .map_err(|error| error.to_string());
                    (path, ChangeGroup::Unstaged, result)
                }
            },
            Err(error) => (String::new(), ChangeGroup::Unstaged, Err(error)),
        };
        let view = view.clone();
        let root = self.model.active_project_dir().to_path_buf();
        let auth = self.workspace_auth.clone();
        self.vcs_scan_generation = self.vcs_scan_generation.saturating_add(1);
        let generation = self.vcs_scan_generation;
        let task = cx.background_executor().spawn(async move {
            let snapshot = load_vcs(&root, &auth);
            let group = snapshot
                .status
                .iter()
                .find(|file| file.path == path && file.group == preferred_group)
                .or_else(|| snapshot.status.iter().find(|file| file.path == path))
                .map(|file| file.group);
            let patch = group.and_then(|group| {
                GitRepository::open(&root, &auth)
                    .ok()
                    .and_then(|repo| repo.diff_file(&path, group == ChangeGroup::Staged).ok())
            });
            (snapshot, group, patch)
        });
        cx.spawn(async move |workspace, cx| {
            let (snapshot, group, patch) = task.await;
            let _ = workspace.update(cx, |workspace, cx| {
                if generation == workspace.vcs_scan_generation
                    && snapshot.root == workspace.model.active_project_dir()
                {
                    workspace.apply_vcs_snapshot(snapshot, cx);
                }
                view.update(cx, |view, cx| {
                    view.update_after_action(result, patch, group, cx)
                });
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn open_git_history(&mut self, cx: &mut Context<Self>) {
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
        let commits = self.vcs_history.clone();
        let first_commit = commits.first().map(|commit| commit.id.clone());
        let entity = cx.new(|cx| GitHistoryView::new(commits, Vec::new(), cx));
        cx.subscribe(
            &entity,
            |workspace, history, action: &GitHistoryAction, cx| match action {
                GitHistoryAction::SelectCommit(commit) => {
                    workspace.schedule_commit_files(history.clone(), commit.clone(), cx);
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
                        workspace.command_message = Some(t!("ws.no_origin_url").to_string());
                    }
                }
            },
        )
        .detach();
        let id = self
            .model
            .new_tab(TabKind::GitHistory, t!("ws.tab_git_history"), false);
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::GitHistory(entity.clone())),
        });
        self.activate_runtime(id, cx);
        if let Some(commit) = first_commit {
            self.schedule_commit_files(entity, commit, cx);
        }
        self.refresh_vcs_data(cx);
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
            .unwrap_or_else(|error| {
                tf!("ws.commit_diff_load_failed", "error" => error).to_string()
            });
        let title = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| tf!("ws.tab_commit", "name" => name))
            .unwrap_or_else(|| t!("ws.tab_commit_plain"));
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
}
