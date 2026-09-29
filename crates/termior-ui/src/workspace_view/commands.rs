//! 命令模式：重命名、新建、分组等内联命令栏的交互与执行。

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandMode {
    Browse,
    FindFile,
    SearchContent,
    CreateFile,
    CreateDirectory,
    Rename,
    Move,
    GitCommit,
    GitCreateBranch,
    GitSwitchBranch,
    PreviewUrl,
    RemotePath,
    NewGroup,
    RenameGroup,
}

impl CommandMode {
    pub(super) fn label(self) -> SharedString {
        match self {
            Self::Browse => t!("ws.cmd_browse"),
            Self::FindFile => t!("ws.cmd_find_file"),
            Self::SearchContent => t!("ws.cmd_search_content"),
            Self::CreateFile => t!("ws.cmd_create_file"),
            Self::CreateDirectory => t!("ws.cmd_create_directory"),
            Self::Rename => t!("ws.cmd_rename"),
            Self::Move => t!("ws.cmd_move"),
            Self::GitCommit => t!("ws.cmd_git_commit"),
            Self::GitCreateBranch => t!("ws.cmd_git_create_branch"),
            Self::GitSwitchBranch => t!("ws.cmd_git_switch_branch"),
            Self::PreviewUrl => t!("ws.cmd_preview_url"),
            Self::RemotePath => t!("ws.cmd_remote_path"),
            Self::NewGroup => t!("ws.cmd_new_group"),
            Self::RenameGroup => t!("ws.cmd_rename_group"),
        }
    }
}

impl WorkspaceView {
    pub(super) fn begin_command(&mut self, mode: CommandMode, cx: &mut Context<Self>) {
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
        self.remote_pending_name_parent = None;
        self.remote_pending_rename_target = None;
        self.explorer_context_menu = None;
        cx.notify();
    }
    pub(super) fn begin_name_command(
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
            CommandMode::CreateFile => t!("ws.hint_create_file").to_string(),
            CommandMode::CreateDirectory => t!("ws.hint_create_directory").to_string(),
            CommandMode::Rename => t!("ws.hint_rename").to_string(),
            CommandMode::Move => t!("ws.hint_move").to_string(),
            _ => String::new(),
        });
        self.pending_name_parent = Some(parent);
        self.pending_rename_target = target;
        self.explorer_context_menu = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
    pub(super) fn begin_group_command(
        &mut self,
        mode: CommandMode,
        prefill: &str,
        profile: Option<String>,
        rename: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_mode = mode;
        self.command_input = prefill.to_owned();
        self.command_marked_text.clear();
        self.command_message = Some(match mode {
            CommandMode::NewGroup => t!("ws.hint_new_group").to_string(),
            CommandMode::RenameGroup => t!("ws.hint_rename_group").to_string(),
            _ => String::new(),
        });
        self.pending_group_profile = profile;
        self.pending_group_rename = rename;
        self.ssh_context_menu = None;
        self.ssh_group_menu = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }
    pub(super) fn handle_command_key(
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
                self.pending_group_profile = None;
                self.pending_group_rename = None;
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
        if self.command_mode == CommandMode::RemotePath {
            if let Some((id, profile, _)) = self.active_remote_explorer_context() {
                self.scan_remote_directory(id, profile, input, false, cx);
                self.command_mode = CommandMode::Browse;
            }
            return;
        }
        if matches!(
            self.command_mode,
            CommandMode::NewGroup | CommandMode::RenameGroup
        ) {
            let mode = self.command_mode;
            let profile = self.pending_group_profile.clone();
            let rename = self.pending_group_rename.clone();
            match mode {
                CommandMode::NewGroup => self.create_group(input, profile, cx),
                CommandMode::RenameGroup => {
                    if let Some(old) = rename {
                        self.rename_group(old, input, cx);
                    }
                }
                _ => unreachable!(),
            }
            self.command_mode = CommandMode::Browse;
            self.command_input.clear();
            self.command_marked_text.clear();
            self.pending_group_profile = None;
            self.pending_group_rename = None;
            return;
        }
        if self.command_mode == CommandMode::SearchContent {
            self.start_content_search(input, cx);
            return;
        }
        if matches!(
            self.command_mode,
            CommandMode::CreateFile
                | CommandMode::CreateDirectory
                | CommandMode::Rename
                | CommandMode::Move
        ) {
            if let Some((tab_id, profile, root)) = self.active_remote_explorer_context() {
                let parent = self.remote_pending_name_parent.clone().unwrap_or(root);
                let operation = match self.command_mode {
                    CommandMode::CreateFile => termior_ssh::sftp::join(&parent, &input)
                        .map(|path| termior_ssh::sftp::Operation::CreateFile { path }),
                    CommandMode::CreateDirectory => termior_ssh::sftp::join(&parent, &input)
                        .map(|path| termior_ssh::sftp::Operation::CreateDirectory { path }),
                    CommandMode::Rename => self
                        .remote_pending_rename_target
                        .clone()
                        .ok_or_else(|| {
                            termior_ssh::Error::Invalid(t!("ws.select_remote_first").to_string())
                        })
                        .and_then(|from| {
                            termior_ssh::sftp::join(&parent, &input)
                                .map(|to| termior_ssh::sftp::Operation::Rename { from, to })
                        }),
                    CommandMode::Move => self
                        .remote_pending_rename_target
                        .clone()
                        .ok_or_else(|| {
                            termior_ssh::Error::Invalid(t!("ws.select_remote_first").to_string())
                        })
                        .and_then(|from| {
                            let to = if input.starts_with('/') {
                                input.clone()
                            } else {
                                format!("{}/{}", parent.trim_end_matches('/'), input)
                            };
                            termior_ssh::quote_sftp_path(&to)
                                .map(|_| termior_ssh::sftp::Operation::Rename { from, to })
                        }),
                    _ => unreachable!(),
                };
                match operation {
                    Ok(operation) => {
                        if !self.run_remote_explorer_operation(
                            tab_id,
                            profile,
                            operation,
                            t!("ws.remote_operation_completed").to_string(),
                            cx,
                        ) {
                            return;
                        }
                        self.command_mode = CommandMode::Browse;
                        self.command_input.clear();
                        self.command_marked_text.clear();
                        self.remote_pending_name_parent = None;
                        self.remote_pending_rename_target = None;
                    }
                    Err(error) => self.command_message = Some(error.to_string()),
                }
                return;
            }
        }
        let completed_mode = self.command_mode;
        let result = match self.command_mode {
            CommandMode::RemotePath => return,
            CommandMode::FindFile => {
                let path = self.explorer.as_ref().and_then(|index| {
                    index
                        .fuzzy(&input, 1)
                        .first()
                        .map(|hit| index.root().join(&hit.path))
                });
                if let Some(path) = path {
                    self.open_editor(path, window, cx);
                    Ok(())
                } else {
                    Err(t!("ws.no_matching_file").to_string())
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
                    self.open_editor(path, window, cx);
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
                        Err(t!("ws.save_before_rename").to_string())
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
                    None => Err(t!("ws.select_first").to_string()),
                }
            }
            CommandMode::Move => Err(t!("ws.move_remote_only").to_string()),
            CommandMode::NewGroup | CommandMode::RenameGroup => {
                unreachable!("group commands complete before command dispatch")
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
                        0 => t!("ws.no_content_matches").to_string(),
                        count => tn!(count, "ws.content_matches").to_string(),
                    });
                    self.command_marked_text.clear();
                } else {
                    self.command_message = Some(t!("ws.done").to_string());
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
                        | CommandMode::Move
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
}
