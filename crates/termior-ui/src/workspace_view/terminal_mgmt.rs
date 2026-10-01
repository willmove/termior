//! 终端与 SSH 会话的创建、重连与生命周期管理。

use super::helpers::{single_pane, terminal_agent_status};
use super::*;

impl WorkspaceView {
    pub(super) fn create_terminal(
        &mut self,
        private: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.create_terminal_with_shell(private, None, window, cx);
    }
    /// 新建终端入口（+ 按钮 / Ctrl+T / 新建菜单项）。`settings.terminal.shell_prompt`
    /// 开启时先弹 shell 选择器（锚点优先取触发点，否则窗口中央），本次选择
    /// 不改默认；关闭时直接按默认创建。
    pub(super) fn request_new_terminal(
        &mut self,
        position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.terminal.shell_prompt {
            self.create_terminal(false, window, cx);
            return;
        }
        self.close_chrome_menus();
        let anchor = position.unwrap_or_else(|| {
            let viewport = window.viewport_size();
            gpui::point(viewport.width / 2.0, viewport.height / 2.0)
        });
        self.shell_menu = Some(anchor);
        // shell 列表可能过期（安装/卸载/新增 WSL 发行版），每次打开都刷新。
        self.refresh_discovered_shells(cx);
        cx.notify();
    }
    /// 后台探测本机 shell：原生条目即时回填；WSL 枚举（wsl.exe 子进程，
    /// 带超时）完成后再追加，避免个别环境下 wsl.exe 慢/挂时整个列表空转。
    fn refresh_discovered_shells(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let native = cx
                .background_executor()
                .spawn(async move { termior_terminal::discover_native_shells() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.discovered_shells = native;
                cx.notify();
            });
            let wsl = cx
                .background_executor()
                .spawn(async move { termior_terminal::discover_wsl_shells() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.discovered_shells.extend(wsl);
                cx.notify();
            });
        })
        .detach();
    }
    /// 以指定 shell 创建终端（选择器选中项）；`shell = None` 走设置默认。
    /// override 的 Native 会清空 `wsl_distribution`、Wsl 会覆盖之，避免
    /// 「全局 WSL + 手选原生 shell」拼出矛盾组合。
    pub(super) fn create_terminal_with_shell(
        &mut self,
        private: bool,
        shell: Option<termior_terminal::DiscoveredShell>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.model.new_tab(
            TabKind::Terminal,
            if private {
                t!("ws.tab_private_terminal")
            } else {
                t!("ws.tab_terminal")
            },
            private,
        );
        let cwd = self.model.active_tab().map(|tab| tab.cwd.clone());
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Placeholder(
                t!("empty.starting_terminal").into(),
            )),
        });
        self.activate_runtime(id, cx);
        self.spawn_terminal_into(
            id,
            PaneId(1),
            cwd,
            private,
            Some(window.window_handle()),
            shell,
            cx,
        );
        cx.notify();
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_terminal_into(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        cwd: Option<PathBuf>,
        private: bool,
        window_handle: Option<AnyWindowHandle>,
        shell_override: Option<termior_terminal::DiscoveredShell>,
        cx: &mut Context<Self>,
    ) {
        if self
            .model
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .is_some_and(|tab| {
                tab.remote
                    .as_ref()
                    .is_some_and(sftp_browser::is_browser_connection)
            })
        {
            self.start_sftp_browser(tab_id, cx);
            return;
        }
        if !self.pending_terminals.insert((tab_id, pane_id)) {
            return;
        }
        let palette = self.palette.clone();
        let terminal_settings = self.settings.terminal.clone();
        let keymap = self.settings.keymap.clone();
        let workspace_auth = self.workspace_auth.clone();
        // shell 选择：选择器的本次手选覆盖默认（Native 清空 WSL、Wsl 覆盖）；
        // 无覆盖时按设置推导（FR-TERM-05）。
        let settings_shell_program = match &terminal_settings.shell_detection {
            ShellDetection::Auto => None,
            ShellDetection::Manual { path } if !path.trim().is_empty() => Some(path.clone()),
            ShellDetection::Manual { .. } => None,
        };
        let (shell_program, wsl_distribution) = match &shell_override {
            Some(termior_terminal::DiscoveredShell::Native { program }) => {
                (Some(program.clone()), None)
            }
            Some(termior_terminal::DiscoveredShell::Wsl { distribution }) => {
                (None, Some(distribution.clone()))
            }
            None => (
                settings_shell_program,
                self.settings.wsl_distribution.clone(),
            ),
        };
        let mut remote = self
            .model
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.remote.clone());
        let tunnel_plan = self.plan_tunnels(tab_id, pane_id, remote.as_mut(), cx);
        let auth_session = match remote
            .as_ref()
            .map(|remote| self.ensure_remote_auth(tab_id, &remote.profile, cx))
            .transpose()
        {
            Ok(session) => session,
            Err(error) => {
                self.pending_terminals.remove(&(tab_id, pane_id));
                self.command_message =
                    Some(tf!("ws.ssh_auth_unavailable", "error" => error).to_string());
                cx.notify();
                return;
            }
        };
        let config = termior_terminal::PtySessionConfig {
            remote,
            auth_session,
            shell_program,
            shell_integration: true,
            inherit_environment: !private,
            cwd: cwd.map(|path| path.to_string_lossy().into_owned()),
            workspace_auth: Some(workspace_auth),
            wsl_distribution,
            ..Default::default()
        };
        let spawn_task = cx.background_executor().spawn(async move {
            let mut config = config;
            // 预检本地端口：已占用的转发不交给 OpenSSH，直接在状态栏标为失败。
            let tunnels = tunnel_plan.map(|forwards| {
                let (requested, entries) = super::tunnels::preflight(&forwards);
                if let Some(remote) = config.remote.as_mut() {
                    remote.profile.forwards = requested;
                }
                entries
            });
            termior_terminal::TerminalBridge::spawn(&config).map(|bridge| (bridge, tunnels))
        });
        cx.spawn(async move |workspace, cx| {
            let (bridge, tunnels) = match spawn_task.await {
                Ok(spawned) => spawned,
                Err(error) => {
                    let _ = workspace.update(cx, |workspace, cx| {
                        workspace.pending_terminals.remove(&(tab_id, pane_id));
                        if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                            if let Some(pane) = tab.panes.get_mut(&pane_id) {
                                *pane = PaneContent::Placeholder(
                                    tf!(
                                        "ws.terminal_failed_detail",
                                        "reason" => t!("empty.terminal_failed"),
                                        "error" => error
                                    )
                                    .into(),
                                );
                            }
                        }
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.pending_terminals.remove(&(tab_id, pane_id));
                if !workspace
                    .tabs
                    .iter()
                    .any(|tab| tab.id == tab_id && tab.panes.contains_key(&pane_id))
                {
                    return;
                }
                let entity = cx.new(|cx| {
                    TerminalView::from_bridge(bridge, palette, terminal_settings, keymap, cx)
                });
                let register_tunnels = tunnels;
                let terminal_focus = entity.read(cx).focus_handle(cx);
                let agent_id = format!("terminal-agent-{}-{}", tab_id.0, pane_id.0);
                let agent_title = workspace
                    .model
                    .tabs
                    .iter()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| tf!("ws.terminal_agent_title", "title" => tab.title.clone()))
                    .unwrap_or_else(|| tf!("ws.terminal_agent_fallback", "id" => tab_id.0))
                    .to_string();
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
                cx.subscribe(
                    &entity,
                    move |workspace, _terminal, event: &TerminalViewEvent, cx| {
                        // A shell that exits (e.g. via `exit`) closes its pane — and the
                        // whole tab when it was the tab's final pane — regardless of
                        // which pane is focused.
                        if let TerminalViewEvent::Exited(code) = event {
                            workspace.handle_terminal_exited(
                                tab_id,
                                pane_id,
                                *code,
                                window_handle,
                                cx,
                            );
                            cx.notify();
                            return;
                        }
                        let focused = workspace
                            .model
                            .tabs
                            .iter()
                            .find(|tab| tab.id == tab_id)
                            .is_some_and(|tab| tab.layout.focused == pane_id);
                        if focused {
                            if let Some(tab) =
                                workspace.model.tabs.iter_mut().find(|tab| tab.id == tab_id)
                            {
                                match event {
                                    TerminalViewEvent::TitleChanged(title) => {
                                        if tab.remote.is_none() {
                                            tab.title = title
                                                .clone()
                                                .unwrap_or_else(|| t!("ws.tab_terminal").into());
                                        }
                                    }
                                    TerminalViewEvent::Bell => {
                                        log::debug!("terminal bell in tab {}", tab_id.0);
                                    }
                                    TerminalViewEvent::Exited(_) => {}
                                }
                            }
                        }
                        cx.notify();
                    },
                )
                .detach();
                if let Some(tab) = workspace.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                    if let Some(pane) = tab.panes.get_mut(&pane_id) {
                        *pane = PaneContent::Terminal(entity);
                    }
                }
                if let Some(entries) = register_tunnels {
                    workspace.register_tunnels(tab_id, pane_id, entries, cx);
                }
                if workspace.model.active == Some(tab_id) {
                    workspace.follow_active_tab_project(cx);
                }
                let should_focus = workspace.model.active == Some(tab_id)
                    && workspace
                        .model
                        .active_tab()
                        .is_some_and(|tab| tab.layout.focused == pane_id);
                if should_focus {
                    if let Some(window_handle) = window_handle {
                        let _ = cx.update_window(window_handle, |_, window, cx| {
                            window.focus(&terminal_focus, cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn reconnect_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.model.active_tab().filter(|tab| tab.remote.is_some()) else {
            return;
        };
        if tab
            .remote
            .as_ref()
            .is_some_and(|remote| remote.transfer.is_some())
        {
            self.open_ssh_manager(window, cx);
            return;
        }
        let (id, pane, cwd) = (tab.id, tab.layout.focused, tab.project_dir.clone());
        if self.pending_terminals.contains(&(id, pane)) {
            return;
        }
        if let Some(terminal) = self.active_terminal() {
            if !terminal.read(cx).has_exited() {
                return;
            }
        }
        self.close_remote_explorer(id);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.panes
                .insert(pane, PaneContent::Placeholder(t!("ws.connecting").into()));
        }
        self.spawn_terminal_into(
            id,
            pane,
            Some(cwd),
            false,
            Some(window.window_handle()),
            None,
            cx,
        );
    }
    pub(super) fn reload_ssh_profiles(&mut self) {
        match self
            .data_dir
            .as_ref()
            .map(|dir| termior_ssh::Profiles::load(dir))
            .transpose()
        {
            Ok(profiles) => {
                self.ssh_profiles = profiles.unwrap_or_default();
                self.ssh_profiles_error = None;
                self.ssh_collapsed_groups
                    .retain(|name| self.ssh_profiles.groups.iter().any(|g| g == name));
                // The prompt can change the preference without reopening the manager.
                for tab in &mut self.model.tabs {
                    if let Some(remote) = &mut tab.remote {
                        if let Some(saved) = self.ssh_profiles.connections.iter().find(|p| {
                            p.name == remote.profile.name
                                && termior_ssh::credentials::same_target(p, &remote.profile)
                        }) {
                            remote.profile.use_saved_credentials = saved.use_saved_credentials;
                            if let Some(session) = self.remote_auth_sessions.get(&tab.id) {
                                session.set_remember(saved.use_saved_credentials);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                self.ssh_profiles_error = Some(error.to_string());
            }
        }
    }
    pub(super) fn connect_remote(
        &mut self,
        connection: termior_ssh::Connection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!(
            "{} · {}",
            if connection.transfer.is_some() {
                t!("ws.sftp_transfer")
            } else if connection.kind == termior_ssh::SessionKind::Shell {
                SharedString::from("SSH")
            } else {
                SharedString::from("SFTP")
            },
            connection.profile.name
        );
        let id = self.model.new_tab(TabKind::Terminal, &title, false);
        if let Some(tab) = self.model.active_tab_mut() {
            tab.remote = Some(connection);
        }
        self.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Placeholder(t!("ws.connecting").into())),
        });
        self.activate_runtime(id, cx);
        self.spawn_terminal_into(
            id,
            PaneId(1),
            Some(self.model.active_project_dir().to_path_buf()),
            false,
            Some(window.window_handle()),
            None,
            cx,
        );
        self.persist_workspace();
        if self.active_is_sftp_browser() {
            window.focus(&self.focus_handle, cx);
        }
        window.activate_window();
        cx.notify();
    }
    pub fn open_ssh_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = self.ssh_manager_window {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return;
            }
        }
        let workspace = cx.entity().downgrade();
        let main_window = window.window_handle();
        let dir = self.data_dir.clone();
        let connect = Box::new(move |connection: termior_ssh::Connection, app: &mut App| {
            let _ = main_window.update(app, |_, window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.connect_remote(connection, window, cx);
                });
            });
        });
        let bounds = Bounds::centered(None, size(px(860.), px(620.)), cx);
        match cx.open_window(
            app_identity::window_options(WindowBounds::Windowed(bounds)),
            |window, cx| {
                let view = cx.new(|cx| crate::ssh_view::SshView::new(dir, connect, cx));
                window.set_window_title(&t!("ws.ssh_manager_title"));
                window.on_window_should_close(cx, |window, _| {
                    window.remove_window();
                    false
                });
                window.focus(&view.read(cx).focus_handle(cx), cx);
                view
            },
        ) {
            Ok(handle) => {
                if let Ok(view) = handle.entity(cx) {
                    cx.subscribe(
                        &view,
                        |this, _, _: &crate::ssh_view::ProfilesChanged, cx| {
                            this.reload_ssh_profiles();
                            cx.notify();
                        },
                    )
                    .detach();
                }
                self.ssh_manager_window = Some(handle);
            }
            Err(error) => {
                self.command_message =
                    Some(tf!("ws.ssh_manager_error", "error" => error).to_string())
            }
        }
    }
    /// A terminal's shell exited: close its pane, and the whole tab when the pane
    /// was the tab's last one. Runs regardless of which pane is focused — the shell
    /// may have exited in a split (`exit`) or in a background tab on its own.
    fn handle_terminal_exited(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        exit_code: Option<i32>,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) {
        if let Some(remote) = self
            .model
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.remote.clone())
        {
            if remote.transfer.is_some() {
                // Successful or interrupted transfers can both modify directories.
                for tab in &self.model.tabs {
                    if tab
                        .remote
                        .as_ref()
                        .is_some_and(|r| r.profile == remote.profile)
                    {
                        if let Some(state) = self.remote_explorers.get_mut(&tab.id) {
                            state.client.invalidate();
                            state.listing = None;
                            if state.request.is_some() {
                                let path = self
                                    .remote_explorer_paths
                                    .get(&tab.id)
                                    .cloned()
                                    .unwrap_or_else(|| ".".into());
                                state.pending = Some((path, true));
                            }
                        }
                    }
                }
                if let Some((id, profile, path)) = self.active_remote_explorer_context() {
                    if profile == remote.profile {
                        if let Some(browser) = self.sftp_browsers.get(&id) {
                            self.scan_sftp_local(id, browser.path.clone(), cx);
                        }
                    }
                    if profile == remote.profile
                        && !self
                            .remote_explorers
                            .get(&id)
                            .is_some_and(|s| s.request.is_some())
                    {
                        self.scan_remote_directory(id, profile, path, true, cx);
                    }
                }
            }
            // Preserve output but terminate the background SFTP transport.
            if !self.remote_runtime_connected(tab_id, cx) {
                self.close_remote_explorer(tab_id);
            }
            // 承载隧道的会话结束：状态栏立即改为"已断开"。
            let _ = self.refresh_tunnels(cx);
            cx.notify();
            return;
        }
        if let Some(code) = exit_code {
            log::info!(
                "terminal pane {} in tab {} exited with code {code}",
                pane_id.0,
                tab_id.0
            );
        }
        let had_focus = self.model.active == Some(tab_id)
            && self
                .model
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .is_some_and(|tab| tab.layout.focused == pane_id);
        let closed = match self.model.close_pane(tab_id, pane_id) {
            Ok(closed) => closed,
            Err(error) => {
                log::warn!("could not close exited terminal pane: {error}");
                return;
            }
        };
        self.remove_terminal_agent(tab_id, pane_id);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.panes.remove(&pane_id);
        }
        match closed {
            Some(_) => {
                // The exited pane had keyboard focus: hand it to the surviving pane.
                if had_focus {
                    let focus = self.active_pane_focus_handle(cx);
                    if let (Some(handle), Some(focus)) = (window_handle, focus) {
                        let _ = cx.update_window(handle, |_, window, cx| {
                            window.focus(&focus, cx);
                        });
                    }
                }
            }
            None => {
                self.tabs.retain(|tab| tab.id != tab_id);
                if let Some(active) = self.model.active {
                    self.activate_runtime(active, cx);
                } else {
                    self.follow_active_tab_project(cx);
                }
            }
        }
    }
}
