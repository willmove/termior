//! 键盘事件分发、快捷键映射与输入法（IME）处理。

use super::commands::CommandMode;
use super::helpers::{explorer_entry_visible, utf16_to_byte};
use super::*;

impl WorkspaceView {
    pub(super) fn start_content_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.content_search_generation = self.content_search_generation.saturating_add(1);
        let generation = self.content_search_generation;
        self.content_matches.clear();
        self.content_searching = true;
        self.command_message = Some(t!("ws.searching_workspace").to_string());
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
                        workspace.command_message = Some(
                            tn!(workspace.content_matches.len(), "ws.searching_progress")
                                .to_string(),
                        );
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
                    Ok(_) if workspace.content_matches.is_empty() => {
                        t!("ws.no_content_matches").to_string()
                    }
                    Ok(_) => tn!(workspace.content_matches.len(), "ws.content_matches").to_string(),
                    Err(error) => error,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    pub(super) fn git_repository(&self) -> Result<GitRepository, String> {
        GitRepository::open(self.model.active_project_dir(), &self.workspace_auth)
            .map_err(|error| error.to_string())
    }
    pub(super) fn stage_all(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn run_remote(&mut self, operation: RemoteOperation, cx: &mut Context<Self>) {
        let root = self.model.active_project_dir().to_path_buf();
        let auth = self.workspace_auth.clone();
        let task = cx.background_executor().spawn(async move {
            GitRepository::open(root, &auth).and_then(|repo| repo.remote(operation))
        });
        self.command_message =
            Some(tf!("ws.running_remote_op", "operation" => format!("{operation:?}")).to_string());
        cx.spawn(async move |workspace, cx| {
            let result = task.await;
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.command_message = Some(match result {
                    Ok(output) if output.trim().is_empty() => {
                        tf!("ws.remote_op_completed", "operation" => format!("{operation:?}"))
                            .to_string()
                    }
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
            || (!self.active_is_sftp_browser()
                && (self.model.sidebar_panel != SidebarPanel::Explorer
                    || !self.model.sidebar_visible))
            || !self.focus_handle.is_focused(window)
        {
            return false;
        }
        if let Some((tab_id, profile, path)) = self.active_remote_explorer_context() {
            // Never route remote Explorer navigation into the retained local tree.
            let key = event.keystroke.key.as_str();
            if key == "escape" {
                self.cancel_remote_explorer(tab_id, cx);
                return true;
            }
            if key == "left" {
                if let Some(parent) = termior_ssh::sftp::parent(&path) {
                    self.schedule_remote_explorer_scan(tab_id, profile, parent, cx);
                }
                return true;
            }
            let entries = self
                .remote_explorers
                .get(&tab_id)
                .and_then(|state| state.listing.as_ref())
                .filter(|listing| listing.cwd == path || path == ".")
                .map(|listing| listing.entries.clone())
                .unwrap_or_default();
            if entries.is_empty() {
                return matches!(
                    key,
                    "up" | "down" | "home" | "end" | "right" | "enter" | "return" | "space"
                );
            }
            let selected = self
                .remote_explorer_selected
                .as_ref()
                .and_then(|path| entries.iter().position(|entry| &entry.path == path))
                .unwrap_or(0);
            let index = match key {
                "up" => selected.saturating_sub(1),
                "down" => (selected + 1).min(entries.len() - 1),
                "home" => 0,
                "end" => entries.len() - 1,
                "right" | "enter" | "return" | "space" => {
                    if entries[selected].is_dir {
                        self.schedule_remote_explorer_scan(
                            tab_id,
                            profile,
                            entries[selected].path.clone(),
                            cx,
                        );
                    }
                    return true;
                }
                _ => return false,
            };
            self.remote_explorer_selected = Some(entries[index].path.clone());
            cx.notify();
            return true;
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
                    self.open_editor(entry.path, window, cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }
    pub(super) fn global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape"
            && (self.ssh_context_menu.take().is_some() | self.ssh_group_menu.take().is_some())
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.handle_command_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if self.handle_sftp_local_key(event, window, cx) {
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
            KeyAction::NewTerminalTab => self.request_new_terminal(None, window, cx),
            KeyAction::NewPrivateTerminal => self.create_terminal(true, window, cx),
            KeyAction::NewEditorTab => self.create_editor(window, cx),
            KeyAction::NewPreviewTab => self.request_preview(window, cx),
            KeyAction::ClosePaneOrTab => self.close_active(window, cx),
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
            KeyAction::ToggleSidebar => self.toggle_sidebar(cx),
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
                self.set_composer_visible(!self.model.composer_visible, window, cx);
            }
            KeyAction::AskAiAboutSelection => self.attach_active_selection(cx),
            KeyAction::CommitStaged => {
                self.model.sidebar_panel = SidebarPanel::SourceControl;
                self.model.sidebar_visible = true;
                self.begin_command(CommandMode::GitCommit, cx);
            }
            KeyAction::OpenSettings => {
                let _ = self.open_settings_window(cx);
            }
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
