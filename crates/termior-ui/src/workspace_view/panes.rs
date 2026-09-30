//! 分栏（pane）布局与标签页生命周期：拆分、关闭、激活与焦点。

use super::commands::CommandMode;
use super::helpers::path_breadcrumb;
use super::*;

impl WorkspaceView {
    fn pane_area_size(&self, window: &Window) -> (f32, f32) {
        let viewport = window.viewport_size();
        let sidebar_width = if self.model.sidebar_visible {
            self.model.sidebar_width
        } else {
            0.0
        };
        // Composer 按当前停靠位置的实际尺寸（含手柄）预留；拖动时随高度/宽度实时收缩。
        let (composer_width, composer_height) = if self.model.composer_visible {
            match self.model.composer_dock {
                ComposerDock::Bottom => (
                    0.0,
                    self.model.composer_height + COMPOSER_RESIZE_HANDLE_SIZE,
                ),
                ComposerDock::Right => (
                    self.model.composer_dock_width + COMPOSER_RESIZE_HANDLE_SIZE,
                    0.0,
                ),
            }
        } else {
            (0.0, 0.0)
        };
        (
            (f32::from(viewport.width) - sidebar_width - composer_width).max(0.0),
            (f32::from(viewport.height)
                - WORKSPACE_HEADER_HEIGHT
                - STATUS_BAR_HEIGHT
                - composer_height)
                .max(0.0),
        )
    }
    pub(super) fn active_pane_count(&self) -> usize {
        self.model
            .active_tab()
            .map(|tab| tab.layout.panes().len())
            .unwrap_or(0)
    }
    pub(super) fn can_split_active(&self, direction: SplitDirection, window: &Window) -> bool {
        let Some(tab) = self.model.active_tab() else {
            return false;
        };
        if tab
            .remote
            .as_ref()
            .is_some_and(|remote| remote.kind == termior_ssh::SessionKind::Sftp)
        {
            return false;
        }
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
    fn split_unavailable_message(&self, direction: Option<SplitDirection>) -> String {
        if self
            .model
            .active_tab()
            .and_then(|tab| tab.remote.as_ref())
            .is_some_and(|remote| remote.transfer.is_some())
        {
            return t!("ws.split_transfer_unavailable").to_string();
        }
        if self.model.active_tab().is_none() {
            return t!("ws.split_no_tab").to_string();
        }
        if self.active_pane_count() >= MAX_PANES_PER_TAB {
            return tf!("ws.split_pane_limit", "count" => MAX_PANES_PER_TAB).to_string();
        }
        match direction {
            Some(SplitDirection::Right) => t!("ws.split_too_narrow").to_string(),
            Some(SplitDirection::Down) => t!("ws.split_too_short").to_string(),
            None => t!("ws.split_too_small").to_string(),
        }
    }
    fn show_split_unavailable(
        &mut self,
        direction: Option<SplitDirection>,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_toast(
            Notification {
                title: t!("ws.split_unavailable_title").to_string(),
                body: self.split_unavailable_message(direction),
                target: NotificationTarget::Global,
                status: AgentStatus::Attention,
            },
            cx,
        );
    }
    pub(super) fn close_chrome_menus(&mut self) {
        self.new_tab_menu = None;
        self.shell_menu = None;
        self.pane_context_menu = None;
        self.ssh_context_menu = None;
        self.ssh_group_menu = None;
        self.explorer_context_menu = None;
    }
    pub(super) fn split_active(
        &mut self,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chrome_menus();
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
            Some(TabKind::Terminal) => {
                PaneContent::Placeholder(t!("empty.starting_split_terminal").into())
            }
            _ => PaneContent::Placeholder(t!("empty.split_view").into()),
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
                None,
                cx,
            );
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }
    fn close_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_chrome_menus();
        if self.active_pane_count() <= 1 {
            self.enqueue_toast(
                Notification {
                    title: t!("ws.close_pane_title").to_string(),
                    body: t!("ws.only_pane").to_string(),
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
        if !self.remote_runtime_connected(tab_id, cx) {
            self.close_remote_explorer(tab_id);
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }
    fn close_other_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_chrome_menus();
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
        if !self.remote_runtime_connected(tab_id, cx) {
            self.close_remote_explorer(tab_id);
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }
    pub(super) fn handle_split_menu_action(
        &mut self,
        action: SplitMenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            SplitMenuAction::Right => self.split_active(SplitDirection::Right, window, cx),
            SplitMenuAction::Down => self.split_active(SplitDirection::Down, window, cx),
            SplitMenuAction::CloseActive => self.close_active_pane(window, cx),
            SplitMenuAction::CloseOthers => self.close_other_panes(window, cx),
        }
    }
    /// 关闭指定标签页(标签栏 ✕ 按钮/中键点击),不要求它是活动标签。
    pub(super) fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
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
        self.close_remote_explorer(id);
        self.model.tabs.remove(index);
        self.sftp_browsers.remove(&id);
        self.tabs.retain(|tab| tab.id != id);
        if self.model.active == Some(id) {
            self.model.active = if self.model.tabs.is_empty() {
                None
            } else {
                Some(self.model.tabs[index.min(self.model.tabs.len() - 1)].id)
            };
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
                // 关闭的是活动 tab：被关 pane 可能正持有键盘焦点（句柄随实体消亡），
                // 显式把焦点交给幸存的活动 pane，否则输入会落到 workspace root 上。
                self.focus_active_pane(window, cx);
            } else {
                self.follow_active_tab_project(cx);
            }
        }
        cx.notify();
    }
    pub(super) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                self.close_remote_explorer(id);
                self.sftp_browsers.remove(&id);
                self.tabs.retain(|tab| tab.id != id);
            }
            if !self.remote_runtime_connected(id, cx) {
                self.close_remote_explorer(id);
            }
            if let Some(active) = self.model.active {
                self.activate_runtime(active, cx);
                // 整个 tab 被关掉时，焦点随被关 pane 消亡；交给幸存的活动 pane。
                self.focus_active_pane(window, cx);
            } else {
                self.follow_active_tab_project(cx);
            }
            cx.notify();
        }
    }
    pub(super) fn activate_runtime(&mut self, id: TabId, cx: &mut Context<Self>) {
        let _ = self.model.switch_to(id);
        // No per-pane activation hook is needed now that the embedded WebView is gone
        // (ADR 0002): preview tabs render a placeholder and have no surface to show/hide.
        self.follow_active_tab_project(cx);
        if let Some(browser) = self.sftp_browsers.get(&id) {
            self.scan_sftp_local(id, browser.path.clone(), cx);
        }
    }
    /// Explorer/Git 跟随活动 Tab 的项目文件夹；同根则不动，避免无意义重扫。
    /// 切 Tab 重扫复用可取消/世代作废扫描，过期结果不会覆盖新根。
    /// 无活动 Tab 时 `active_project_dir()` 回落兑底根，与状态栏 pill 保持一致。
    pub(super) fn follow_active_tab_project(&mut self, cx: &mut Context<Self>) {
        // Some callers switch the model before activate_runtime. Track the view
        // owner separately so unfinished names never cross local/remote tabs.
        if self.explorer_view_tab != self.model.active {
            self.explorer_view_tab = self.model.active;
            self.explorer_context_menu = None;
            self.ssh_context_menu = None;
            self.remote_pending_name_parent = None;
            self.remote_pending_rename_target = None;
            self.remote_explorer_selected = None;
            if matches!(
                self.command_mode,
                CommandMode::CreateFile
                    | CommandMode::CreateDirectory
                    | CommandMode::Rename
                    | CommandMode::Move
                    | CommandMode::RemotePath
            ) {
                self.command_mode = CommandMode::Browse;
                self.command_input.clear();
                self.command_marked_text.clear();
            }
        }
        if let Some((tab_id, profile, path)) = self.active_remote_explorer_context() {
            // Tab switching must not reopen a failed authentication prompt.
            if self
                .remote_explorers
                .get(&tab_id)
                .map_or(true, |state| state.error.is_none())
            {
                self.schedule_remote_explorer_scan(tab_id, profile, path, cx);
            }
            cx.notify();
            return;
        }
        let project_dir = self.model.active_project_dir().to_path_buf();
        if project_dir.is_dir() && self.explorer_requested_root != project_dir {
            self.schedule_explorer_scan(project_dir, cx);
            self.refresh_vcs_data(cx);
        }
    }
    pub(super) fn active_pane_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let model_tab = self.model.tab(active)?;
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
    pub(super) fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_area_hidden() || self.active_is_sftp_browser() {
            window.focus(&self.focus_handle, cx);
            return;
        }
        if let Some(focus) = self.active_pane_focus_handle(cx) {
            window.focus(&focus, cx);
        }
    }
    /// 焦点 pane 恰好是终端时返回它（不回退到同 tab 的其他终端）。
    pub(super) fn focused_terminal(&self) -> Option<&Entity<TerminalView>> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.tab(active)?.layout.focused;
        match tab.panes.get(&focused)? {
            PaneContent::Terminal(entity) => Some(entity),
            _ => None,
        }
    }
    pub(super) fn active_terminal(&self) -> Option<&Entity<TerminalView>> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.tab(active)?.layout.focused;
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
    pub(super) fn active_editor(&self) -> Option<&Entity<EditorView>> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.tab(active)?.layout.focused;
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
    fn focused_pane_content(&self) -> Option<&PaneContent> {
        let active = self.model.active?;
        let tab = self.tabs.iter().find(|tab| tab.id == active)?;
        let focused = self.model.tab(active)?.layout.focused;
        tab.panes.get(&focused)
    }
    pub(super) fn status_context_label(&self, cwd: &str, cx: &App) -> String {
        match self.focused_pane_content() {
            Some(PaneContent::Terminal(_)) => path_breadcrumb(cwd),
            Some(PaneContent::Editor(editor)) => {
                let editor = editor.read(cx);
                let (line, column) = editor.cursor_line_col();
                let path = editor
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| editor.title().to_owned());
                format!("{path} · {}:{}", line + 1, column + 1)
            }
            Some(PaneContent::Markdown(_)) => t!("ws.context_markdown_preview").to_string(),
            Some(PaneContent::Preview(_)) => t!("ws.web_preview").to_string(),
            Some(PaneContent::AiDiff(_)) => t!("ws.context_ai_diff").to_string(),
            Some(PaneContent::GitDiff(_)) => t!("ws.context_git_diff").to_string(),
            Some(PaneContent::GitHistory(_)) => t!("ws.context_git_history").to_string(),
            Some(PaneContent::Placeholder(message)) => message.clone(),
            None => path_breadcrumb(cwd),
        }
    }
    pub(super) fn handle_new_tab_action(
        &mut self,
        action: NewTabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_chrome_menus();
        match action {
            NewTabAction::Terminal => self.request_new_terminal(None, window, cx),
            NewTabAction::Editor => self.create_editor(window, cx),
            NewTabAction::Ssh => self.open_ssh_manager(window, cx),
        }
    }
}
