//! 本地文件浏览器动作：上下文菜单、增删改名与脏编辑器保护。

use super::commands::CommandMode;
use super::helpers::{dir_is_non_empty, reveal_in_system_file_manager};
use super::*;

#[derive(Debug, Clone)]
pub(super) struct ExplorerContextMenu {
    pub(super) target: ExplorerContextTarget,
    pub(super) position: Point<Pixels>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExplorerContextTarget {
    Workspace,
    Directory(PathBuf),
    File(PathBuf),
    RemoteWorkspace(String),
    RemoteDirectory(String),
    RemoteFile(String),
}

impl ExplorerContextTarget {
    pub(super) fn local_path(&self, root: &Path) -> Option<PathBuf> {
        match self {
            Self::Workspace => Some(root.to_path_buf()),
            Self::Directory(path) | Self::File(path) => Some(path.clone()),
            Self::RemoteWorkspace(_) | Self::RemoteDirectory(_) | Self::RemoteFile(_) => None,
        }
    }

    pub(super) fn remote_path(&self) -> Option<&str> {
        match self {
            Self::RemoteWorkspace(path) | Self::RemoteDirectory(path) | Self::RemoteFile(path) => {
                Some(path)
            }
            _ => None,
        }
    }

    pub(super) fn is_remote(&self) -> bool {
        self.remote_path().is_some()
    }

    pub(super) fn kind(&self) -> ExplorerContextTargetKind {
        match self {
            Self::Workspace | Self::RemoteWorkspace(_) => ExplorerContextTargetKind::Workspace,
            Self::Directory(_) | Self::RemoteDirectory(_) => ExplorerContextTargetKind::Directory,
            Self::File(_) | Self::RemoteFile(_) => ExplorerContextTargetKind::File,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExplorerContextTargetKind {
    Workspace,
    Directory,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExplorerContextAction {
    Open,
    CreateFile,
    CreateDirectory,
    Rename,
    Move,
    Delete,
    Reveal,
    AttachToAi,
    FindFile,
    SearchContent,
    UploadFile,
    UploadDirectory,
    Download,
    Refresh,
}

impl WorkspaceView {
    pub(super) fn attach_active_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(terminal) = self.focused_terminal().cloned() {
            let attachment = terminal.read(cx).selection_attachment();
            match attachment {
                Some(attachment) => self.attach_terminal_output(attachment, cx),
                // 无选区时把 Ctrl+L 还给 shell（清屏）；macOS 的 ⌘L 不与之冲突。
                None if !cfg!(target_os = "macos") => {
                    if let Err(error) = terminal.read(cx).write_input(b"\x0c") {
                        log::warn!("PTY write error: {error}");
                    }
                }
                None => {}
            }
            return;
        }
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
    /// 把终端片段连同命令元数据附加到 Composer（FR-ATERM-05）。
    pub(super) fn attach_terminal_output(
        &mut self,
        attachment: crate::terminal_view::TerminalAttachment,
        cx: &mut Context<Self>,
    ) {
        self.composer.update(cx, |composer, cx| {
            composer.attach_terminal_output(attachment.label, attachment.text, attachment.command);
            cx.notify();
        });
        self.model.composer_visible = true;
        cx.notify();
    }
    fn attach_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| {
            composer.attach_file(path);
            cx.notify();
        });
        self.model.composer_visible = true;
        cx.notify();
    }
    pub(super) fn explorer_skipped_footer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let skipped = self.explorer.as_ref()?.skipped();
        if skipped.is_empty() {
            return None;
        }
        let count = skipped.len();
        let expanded = self.explorer_skips_expanded;
        let chevron = if expanded { "▾" } else { "▸" };
        let summary = format!("{chevron} {}", tn!(count, "explorer.skipped"));
        let details = expanded.then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pt_1()
                .children(skipped.iter().map(|entry| {
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_xs()
                                .text_color(ui::color(self.palette.foreground))
                                .child(SharedString::from(entry.display_path())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(ui::muted(&self.palette))
                                .child(entry.reason.as_str()),
                        )
                }))
        });
        Some(
            div()
                .id("explorer-skipped")
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(ui::border(&self.palette))
                .bg(ui::color(self.palette.chrome))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.explorer_skips_expanded = !this.explorer_skips_expanded;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(ui::muted(&self.palette))
                        .child(SharedString::from(summary)),
                )
                .children(details)
                .into_any_element(),
        )
    }
    pub(super) fn show_explorer_context_menu(
        &mut self,
        target: ExplorerContextTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &target {
            ExplorerContextTarget::Workspace | ExplorerContextTarget::RemoteWorkspace(_) => {}
            ExplorerContextTarget::Directory(path) | ExplorerContextTarget::File(path) => {
                self.explorer_tree.select(path.clone());
            }
            ExplorerContextTarget::RemoteDirectory(path)
            | ExplorerContextTarget::RemoteFile(path) => {
                self.remote_explorer_selected = Some(path.clone());
            }
        }
        self.command_mode = CommandMode::Browse;
        self.command_marked_text.clear();
        self.explorer_context_menu = Some(ExplorerContextMenu { target, position });
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
    pub(super) fn handle_explorer_context_action(
        &mut self,
        action: ExplorerContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.explorer_context_menu.take().map(|menu| menu.target) else {
            return;
        };
        if target.is_remote() {
            self.handle_remote_explorer_context_action(target, action, window, cx);
            return;
        }
        let root = self.explorer_requested_root.clone();
        match action {
            ExplorerContextAction::Open => {
                if let ExplorerContextTarget::File(path) = target {
                    self.open_editor(path, window, cx);
                }
            }
            ExplorerContextAction::CreateFile | ExplorerContextAction::CreateDirectory => {
                let parent = match target {
                    ExplorerContextTarget::Directory(path) => path,
                    ExplorerContextTarget::File(path) => {
                        path.parent().map(Path::to_path_buf).unwrap_or(root)
                    }
                    ExplorerContextTarget::Workspace => root,
                    ExplorerContextTarget::RemoteWorkspace(_)
                    | ExplorerContextTarget::RemoteDirectory(_)
                    | ExplorerContextTarget::RemoteFile(_) => return,
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
                    ExplorerContextTarget::RemoteWorkspace(_)
                    | ExplorerContextTarget::RemoteDirectory(_)
                    | ExplorerContextTarget::RemoteFile(_) => return,
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
            ExplorerContextAction::Move => return,
            ExplorerContextAction::Delete => self.delete_selected_with_prompt(window, cx),
            ExplorerContextAction::Reveal => {
                let Some(path) = target.local_path(&root) else {
                    return;
                };
                self.command_message = reveal_in_system_file_manager(
                    &path,
                    target.kind() == ExplorerContextTargetKind::File,
                )
                .err()
                .map(|error| tf!("ws.reveal_failed", "error" => error).to_string())
                .or_else(|| Some(tf!("ws.revealed", "path" => path.display()).to_string()));
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
            ExplorerContextAction::UploadFile
            | ExplorerContextAction::UploadDirectory
            | ExplorerContextAction::Download => return,
            ExplorerContextAction::Refresh => {
                self.refresh_workspace_data(cx);
                self.command_message = Some(t!("ws.explorer_refresh_scheduled").to_string());
            }
        }
        cx.notify();
    }
    pub(super) fn has_dirty_editor_under(&self, target: &Path, cx: &App) -> bool {
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
    pub(super) fn retarget_open_editors(
        &mut self,
        old_path: &Path,
        new_path: &Path,
        cx: &mut Context<Self>,
    ) {
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
                    .map(str::to_owned)
                    .unwrap_or_else(|| t!("ws.tab_editor").into());
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
            self.command_message = Some(t!("ws.select_first").to_string());
            cx.notify();
            return;
        };
        if self.has_dirty_editor_under(&path, cx) {
            self.command_message = Some(t!("ws.save_before_delete").to_string());
            cx.notify();
            return;
        }
        let Ok(relative) = path
            .strip_prefix(self.explorer_tree.root())
            .map(Path::to_path_buf)
        else {
            self.command_message = Some(t!("ws.path_outside_root").to_string());
            cx.notify();
            return;
        };

        let detail = tf!("ws.delete_confirm", "path" => path.display()).to_string();
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("ws.delete_title"),
            Some(&detail),
            &[
                PromptButton::ok(t!("ws.dialog_delete")),
                PromptButton::cancel(t!("ws.dialog_cancel")),
            ],
            cx,
        );
        let recursive_prompt = (path.is_dir() && dir_is_non_empty(&path)).then(|| {
            (
                t!("ws.delete_nonempty_title").to_string(),
                tf!("ws.delete_nonempty_body", "path" => path.display()).to_string(),
            )
        });
        let window_handle = window.window_handle();
        self.command_message = Some(t!("ws.delete_waiting").to_string());
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
                                PromptButton::ok(t!("ws.delete_all")),
                                PromptButton::cancel(t!("ws.dialog_cancel")),
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
                    workspace.command_message = Some(t!("ws.delete_canceled").to_string());
                    cx.notify();
                    return;
                }
                match workspace.explorer_tree.delete(&relative) {
                    Ok(()) => {
                        workspace.close_tabs_for_deleted_path(&path, cx);
                        workspace.explorer_tree.clear_selection();
                        workspace.command_message =
                            Some(tf!("ws.deleted", "path" => path.display()).to_string());
                        workspace.refresh_workspace_data(cx);
                    }
                    Err(error) => {
                        workspace.command_message =
                            Some(tf!("ws.delete_failed", "error" => error).to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn close_tabs_for_deleted_path(&mut self, deleted: &Path, cx: &mut Context<Self>) {
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
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
            } else {
                self.follow_active_tab_project(cx);
            }
        }
    }
}
