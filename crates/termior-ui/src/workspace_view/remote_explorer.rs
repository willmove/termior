//! 远程（SFTP）文件浏览器：目录扫描、认证、上传/下载与远程面板内容渲染。

use super::commands::CommandMode;
use super::explorer_actions::{ExplorerContextAction, ExplorerContextTarget};
use super::helpers::{
    explorer_icon, explorer_tool_button, gpui_color, remote_icon_kind, scroll_thumb_geometry,
};
use super::*;

pub(super) struct RemoteExplorerState {
    pub(super) client: termior_ssh::sftp::Client,
    pub(super) scroll: ScrollHandle,
    pub(super) listing: Option<termior_ssh::sftp::RemoteListing>,
    pub(super) request: Option<RemoteExplorerRequest>,
    pub(super) pending: Option<(String, bool)>,
    pub(super) error: Option<String>,
    pub(super) status: String,
}
impl Drop for RemoteExplorerState {
    fn drop(&mut self) {
        self.client.close();
    }
}
pub(super) struct RemoteExplorerRequest {
    pub(super) id: u64,
    // None is a mutation, never superseded by navigation.
    pub(super) path: Option<String>,
    pub(super) control: termior_ssh::sftp::RequestControl,
}
// Only retain the latest destination; repeated refreshes share the in-flight request.
pub(super) fn coalesce_remote_scan(
    active: Option<&str>,
    pending: &mut Option<(String, bool)>,
    path: String,
    force: bool,
) {
    if active == Some(path.as_str()) {
        // A transfer can invalidate the directory during its current scan.
        // Preserve that explicit post-transfer rescan, but drop stale navigation.
        if !pending.as_ref().is_some_and(|(p, f)| p == &path && *f) {
            *pending = None;
        }
    } else {
        let force = force || pending.as_ref().is_some_and(|(p, f)| p == &path && *f);
        *pending = Some((path, force));
    }
}

impl WorkspaceView {
    pub(super) fn schedule_remote_explorer_scan(
        &mut self,
        tab_id: TabId,
        profile: termior_ssh::Profile,
        path: String,
        cx: &mut Context<Self>,
    ) {
        self.scan_remote_directory(tab_id, profile, path, false, cx);
    }
    pub(super) fn ensure_remote_auth(
        &mut self,
        tab_id: TabId,
        profile: &termior_ssh::Profile,
        cx: &mut Context<Self>,
    ) -> Result<termior_ssh::auth::Session, String> {
        if let Some(session) = self
            .remote_auth_sessions
            .get(&tab_id)
            .filter(|s| s.matches(profile))
        {
            return Ok(session.clone());
        }
        if let Some(session) = self
            .remote_auth_sessions
            .values()
            .find(|s| s.matches(profile))
            .cloned()
        {
            self.remote_auth_sessions.insert(tab_id, session.clone());
            return Ok(session);
        }
        let (session, mut requests) =
            termior_ssh::auth::Session::new(profile.clone()).map_err(|e| e.to_string())?;
        self.remote_auth_sessions.insert(tab_id, session.clone());
        cx.spawn(async move |workspace, cx| {
            while let Some(challenge) = requests.next().await {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.open_remote_auth_prompt(challenge, cx)
                });
            }
        })
        .detach();
        Ok(session)
    }
    fn open_remote_auth_prompt(
        &mut self,
        challenge: termior_ssh::auth::Challenge,
        cx: &mut Context<Self>,
    ) {
        let cancelled = challenge.cancelled.clone();
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        let dir = self.data_dir.clone();
        let bounds = WindowBounds::Windowed(Bounds::centered(None, size(px(620.), px(440.)), cx));
        match cx.open_window(app_identity::window_options(bounds), |window, cx| {
            let view = cx.new(|cx| crate::ssh_view::SshView::new_shared_prompt(challenge, dir, cx));
            window.set_window_title(&t!("ws.ssh_auth_window_title"));
            window.on_window_should_close(cx, |window, _| {
                window.remove_window();
                false
            });
            window.focus(&view.read(cx).focus_handle(cx), cx);
            window.activate_window();
            view
        }) {
            Ok(handle) => {
                if let Ok(view) = handle.entity(cx) {
                    cx.subscribe(
                        &view,
                        |this, _, _: &crate::ssh_view::ProfilesChanged, cx| {
                            this.reload_ssh_profiles();
                            cx.notify();
                            if let Some(manager) = this.ssh_manager_window {
                                let _ = manager.update(cx, |view, _, cx| {
                                    view.refresh_credential_preferences(cx)
                                });
                            }
                        },
                    )
                    .detach();
                }
                cx.spawn(async move |_, cx| loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let cancelled = cancelled.load(Ordering::Acquire);
                    if handle
                        .update(cx, |_, window, _| {
                            if cancelled {
                                window.remove_window();
                            }
                        })
                        .is_err()
                        || cancelled
                    {
                        break;
                    }
                })
                .detach();
            }
            Err(error) => {
                self.command_message =
                    Some(tf!("ws.ssh_auth_window_failed", "error" => error).to_string());
                cx.notify();
            }
        }
    }
    fn ensure_remote_explorer(
        &mut self,
        tab_id: TabId,
        profile: termior_ssh::Profile,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.remote_explorers.contains_key(&tab_id) {
            return true;
        }
        let auth = match self.ensure_remote_auth(tab_id, &profile, cx) {
            Ok(auth) => auth,
            Err(error) => {
                self.command_message =
                    Some(tf!("ws.ssh_authentication_error", "error" => error).to_string());
                return false;
            }
        };
        match termior_ssh::sftp::Client::with_auth(profile, Some(auth)) {
            Ok(client) => {
                self.remote_explorers.insert(
                    tab_id,
                    RemoteExplorerState {
                        client,
                        scroll: ScrollHandle::new(),
                        listing: None,
                        request: None,
                        pending: None,
                        error: None,
                        status: t!("ws.remote_status_ready").to_string(),
                    },
                );
                true
            }
            Err(error) => {
                self.command_message = Some(error.to_string());
                false
            }
        }
    }
    pub(super) fn scan_remote_directory(
        &mut self,
        tab_id: TabId,
        profile: termior_ssh::Profile,
        path: String,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.remote_runtime_connected(tab_id, cx) {
            cx.notify();
            return;
        }
        if termior_ssh::quote_sftp_path(&path).is_err() {
            self.command_message = Some(t!("ws.invalid_remote_path").to_string());
            cx.notify();
            return;
        }
        if !self.ensure_remote_explorer(tab_id, profile.clone(), cx) {
            cx.notify();
            return;
        }
        self.remote_explorer_paths.insert(tab_id, path.clone());
        let state = self.remote_explorers.get_mut(&tab_id).unwrap();
        if let Some(request) = &state.request {
            coalesce_remote_scan(request.path.as_deref(), &mut state.pending, path, force);
            cx.notify();
            return;
        }
        self.remote_explorer_generation += 1;
        let id = self.remote_explorer_generation;
        let control = termior_ssh::sftp::RequestControl::default();
        state.request = Some(RemoteExplorerRequest {
            id,
            path: Some(path.clone()),
            control: control.clone(),
        });
        state.error = None;
        state.status = control.status();
        let client = state.client.clone();
        self.remote_explorer_selected = None;
        self.watch_remote_progress(tab_id, id, cx);
        cx.spawn(async move |workspace, cx| {
            let result = client.list(&path, force, control).await;
            let _ = workspace.update(cx, |workspace, cx| {
                let Some(state) = workspace.remote_explorers.get_mut(&tab_id) else {
                    return;
                };
                if state.request.as_ref().map(|r| r.id) != Some(id) {
                    return;
                }
                state.request = None;
                let pending = state.pending.take();
                match result {
                    Ok(listing) => {
                        if pending.is_none() {
                            let changed_directory = state
                                .listing
                                .as_ref()
                                .map_or(true, |previous| previous.cwd != listing.cwd);
                            workspace
                                .remote_explorer_paths
                                .insert(tab_id, listing.cwd.clone());
                            state.listing = Some(listing);
                            if changed_directory {
                                state.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                            }
                        }
                        state.status = t!("ws.remote_status_connected").to_string();
                    }
                    Err(error) => {
                        state.error =
                            Some(tf!("ws.remote_explorer_error", "error" => error).to_string());
                        state.status = t!("ws.remote_status_request_stopped").to_string();
                        cx.notify();
                        return; // Never auto-retry failed authentication.
                    }
                }
                if let Some((path, force)) = pending {
                    workspace.scan_remote_directory(tab_id, profile, path, force, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn watch_remote_progress(&self, tab_id: TabId, id: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |workspace, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let keep_running = workspace
                .update(cx, |workspace, cx| {
                    let Some(state) = workspace.remote_explorers.get_mut(&tab_id) else {
                        return false;
                    };
                    let Some(request) = state.request.as_ref().filter(|r| r.id == id) else {
                        return false;
                    };
                    let status = request.control.status();
                    if state.status != status {
                        state.status = status;
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep_running {
                break;
            }
        })
        .detach();
    }
    pub(super) fn cancel_remote_explorer(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if let Some(state) = self.remote_explorers.get_mut(&tab_id) {
            state.pending = None;
            if let Some(request) = &state.request {
                request.control.cancel();
            }
        }
        cx.notify();
    }
    pub(super) fn close_remote_explorer(&mut self, tab_id: TabId) {
        if let Some(browser) = self.sftp_browsers.get_mut(&tab_id) {
            browser.connected = false;
        }
        self.remote_explorers.remove(&tab_id);
        self.remote_auth_sessions.remove(&tab_id);
        self.remote_explorer_paths.remove(&tab_id);
        self.remote_terminal_cwds.remove(&tab_id);
        if self
            .remote_explorer_scroll_drag
            .is_some_and(|(drag_tab, _)| drag_tab == tab_id)
        {
            self.remote_explorer_scroll_drag = None;
        }
    }
    fn begin_remote_explorer_scroll_drag(&mut self, tab_id: TabId, pointer_y: Pixels) {
        let Some(scroll) = self
            .remote_explorers
            .get(&tab_id)
            .map(|state| state.scroll.clone())
        else {
            return;
        };
        let bounds = scroll.bounds();
        let (thumb_top, thumb_height) = scroll_thumb_geometry(
            f32::from(bounds.size.height).max(1.0),
            f32::from(scroll.max_offset().y),
            f32::from(scroll.offset().y),
        );
        let pointer = f32::from(pointer_y - bounds.top());
        let pointer_offset = if pointer >= thumb_top && pointer <= thumb_top + thumb_height {
            pointer - thumb_top
        } else {
            thumb_height / 2.0
        };
        self.remote_explorer_scroll_drag = Some((tab_id, pointer_offset));
        self.scroll_remote_explorer_to_pointer(tab_id, pointer_y, pointer_offset);
    }
    fn scroll_remote_explorer_to_pointer(
        &self,
        tab_id: TabId,
        pointer_y: Pixels,
        pointer_offset: f32,
    ) {
        let Some(scroll) = self
            .remote_explorers
            .get(&tab_id)
            .map(|state| &state.scroll)
        else {
            return;
        };
        let bounds = scroll.bounds();
        let viewport_height = f32::from(bounds.size.height).max(1.0);
        let max_offset = f32::from(scroll.max_offset().y).max(0.0);
        let (_, thumb_height) =
            scroll_thumb_geometry(viewport_height, max_offset, f32::from(scroll.offset().y));
        let pointer = f32::from(pointer_y - bounds.top());
        let ratio = ((pointer - pointer_offset) / (viewport_height - thumb_height).max(1.0))
            .clamp(0.0, 1.0);
        let offset = scroll.offset();
        scroll.set_offset(gpui::point(offset.x, px(-max_offset * ratio)));
    }
    pub(super) fn active_remote_explorer_context(
        &self,
    ) -> Option<(TabId, termior_ssh::Profile, String)> {
        let tab = self.model.active_tab()?;
        let remote = tab.remote.as_ref()?;
        if remote.transfer.is_some() {
            return None;
        }
        let path = self
            .remote_explorer_paths
            .get(&tab.id)
            .or_else(|| self.remote_terminal_cwds.get(&tab.id))
            .cloned()
            .unwrap_or_else(|| ".".into());
        Some((tab.id, remote.profile.clone(), path))
    }
    pub(super) fn remote_runtime_connected(&self, tab_id: TabId, cx: &App) -> bool {
        if let Some(browser) = self.sftp_browsers.get(&tab_id) {
            return browser.connected;
        }
        self.tabs
            .iter()
            .find(|runtime| runtime.id == tab_id)
            .is_some_and(|runtime| {
                runtime.panes.values().any(|pane| match pane {
                    PaneContent::Terminal(terminal) => !terminal.read(cx).has_exited(),
                    _ => false,
                })
            })
    }
    pub(super) fn refresh_remote_explorer(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((tab_id, profile, path)) = self.active_remote_explorer_context() else {
            return false;
        };
        self.scan_remote_directory(tab_id, profile, path, true, cx);
        true
    }
    pub(super) fn run_remote_explorer_operation(
        &mut self,
        tab_id: TabId,
        profile: termior_ssh::Profile,
        operation: termior_ssh::sftp::Operation,
        success: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.remote_runtime_connected(tab_id, cx) {
            self.command_message = Some(t!("ws.reconnect_first").to_string());
            cx.notify();
            return false;
        }
        if !self.ensure_remote_explorer(tab_id, profile.clone(), cx) {
            cx.notify();
            return false;
        }
        let state = self.remote_explorers.get_mut(&tab_id).unwrap();
        if state.request.is_some() {
            self.command_message = Some(t!("ws.wait_remote_request").to_string());
            cx.notify();
            return false;
        }
        self.remote_explorer_generation += 1;
        let id = self.remote_explorer_generation;
        let control = termior_ssh::sftp::RequestControl::default();
        state.request = Some(RemoteExplorerRequest {
            id,
            path: None,
            control: control.clone(),
        });
        state.error = None;
        state.status = control.status();
        let client = state.client.clone();
        self.watch_remote_progress(tab_id, id, cx);
        cx.spawn(async move |workspace, cx| {
            let result = client.execute(&operation, control).await;
            let _ = workspace.update(cx, |workspace, cx| {
                let Some(state) = workspace.remote_explorers.get_mut(&tab_id) else {
                    return;
                };
                if state.request.as_ref().map(|r| r.id) != Some(id) {
                    return;
                }
                state.request = None;
                state.listing = None; // Interrupted writes can have partial effects.
                let pending = state.pending.take();
                match result {
                    Ok(()) => {
                        state.status = success.clone();
                        if workspace.model.active == Some(tab_id) {
                            workspace.command_message = Some(success);
                        }
                        let path = pending
                            .map(|(path, _)| path)
                            .or_else(|| workspace.remote_explorer_paths.get(&tab_id).cloned())
                            .unwrap_or_else(|| ".".into());
                        workspace.scan_remote_directory(tab_id, profile, path, true, cx);
                    }
                    Err(error) => {
                        state.error =
                            Some(tf!("ws.remote_operation_error", "error" => error).to_string());
                        state.status = t!("ws.remote_status_operation_stopped").to_string();
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        true
    }
    pub(super) fn handle_remote_explorer_context_action(
        &mut self,
        target: ExplorerContextTarget,
        action: ExplorerContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((tab_id, profile, root)) = self.active_remote_explorer_context() else {
            return;
        };
        if !self.remote_runtime_connected(tab_id, cx) {
            self.command_message = Some(t!("ws.reconnect_first").to_string());
            cx.notify();
            return;
        }
        let path = target.remote_path().unwrap_or(&root).to_owned();
        match action {
            ExplorerContextAction::CreateFile | ExplorerContextAction::CreateDirectory => {
                let parent = match &target {
                    ExplorerContextTarget::RemoteDirectory(path)
                    | ExplorerContextTarget::RemoteWorkspace(path) => path.clone(),
                    ExplorerContextTarget::RemoteFile(path) => {
                        termior_ssh::sftp::parent(path).unwrap_or(root)
                    }
                    _ => return,
                };
                self.remote_pending_name_parent = Some(parent);
                self.remote_pending_rename_target = None;
                self.begin_name_command(
                    if action == ExplorerContextAction::CreateFile {
                        CommandMode::CreateFile
                    } else {
                        CommandMode::CreateDirectory
                    },
                    PathBuf::new(),
                    None,
                    if action == ExplorerContextAction::CreateFile {
                        "untitled"
                    } else {
                        "New Folder"
                    },
                    window,
                    cx,
                );
            }
            ExplorerContextAction::Rename => {
                if matches!(target, ExplorerContextTarget::RemoteWorkspace(_)) {
                    return;
                }
                let parent = termior_ssh::sftp::parent(&path).unwrap_or_else(|| root.clone());
                let prefill = termior_ssh::sftp::file_name(&path).to_owned();
                self.remote_pending_name_parent = Some(parent);
                self.remote_pending_rename_target = Some(path);
                self.begin_name_command(
                    CommandMode::Rename,
                    PathBuf::new(),
                    None,
                    &prefill,
                    window,
                    cx,
                );
            }
            ExplorerContextAction::Move => {
                if matches!(target, ExplorerContextTarget::RemoteWorkspace(_)) {
                    return;
                }
                self.remote_pending_name_parent = termior_ssh::sftp::parent(&path);
                self.remote_pending_rename_target = Some(path.clone());
                self.begin_name_command(CommandMode::Move, PathBuf::new(), None, &path, window, cx);
            }
            ExplorerContextAction::Delete => {
                let is_dir = matches!(target, ExplorerContextTarget::RemoteDirectory(_));
                self.delete_remote_with_prompt(profile, path, is_dir, window, cx);
            }
            ExplorerContextAction::UploadFile => {
                self.choose_remote_upload(profile, path, false, window, cx)
            }
            ExplorerContextAction::UploadDirectory => {
                self.choose_remote_upload(profile, path, true, window, cx)
            }
            ExplorerContextAction::Download => {
                let recursive = matches!(target, ExplorerContextTarget::RemoteDirectory(_));
                self.choose_remote_download(profile, path, recursive, window, cx);
            }
            ExplorerContextAction::Refresh => {
                self.refresh_remote_explorer(cx);
                self.command_message = Some(t!("ws.remote_refresh_scheduled").to_string());
            }
            ExplorerContextAction::Open => {
                if matches!(target, ExplorerContextTarget::RemoteDirectory(_)) {
                    self.scan_remote_directory(tab_id, profile, path, false, cx);
                }
            }
            ExplorerContextAction::Reveal
            | ExplorerContextAction::AttachToAi
            | ExplorerContextAction::FindFile
            | ExplorerContextAction::SearchContent => {}
        }
        cx.notify();
    }
    fn delete_remote_with_prompt(
        &mut self,
        profile: termior_ssh::Profile,
        path: String,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.model.active else {
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("ws.delete_remote_title"),
            Some(&if is_dir {
                tf!("ws.delete_remote_confirm_dir", "path" => path.clone())
            } else {
                tf!("ws.delete_remote_confirm_file", "path" => path.clone())
            }),
            &[
                PromptButton::ok(t!("ws.dialog_delete")),
                PromptButton::cancel(t!("ws.dialog_cancel")),
            ],
            cx,
        );
        cx.spawn(async move |workspace, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let _ = workspace.update(cx, |workspace, cx| {
                let operation = if is_dir {
                    termior_ssh::sftp::Operation::RemoveDirectory {
                        path: path.clone(),
                        recursive: true,
                    }
                } else {
                    termior_ssh::sftp::Operation::RemoveFile { path: path.clone() }
                };
                workspace.run_remote_explorer_operation(
                    tab_id,
                    profile,
                    operation,
                    tf!("ws.delete_remote_done", "path" => path.clone()).to_string(),
                    cx,
                );
            });
        })
        .detach();
    }
    fn choose_remote_upload(
        &mut self,
        profile: termior_ssh::Profile,
        remote_directory: String,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |workspace, cx| {
            let dialog = rfd::AsyncFileDialog::new().set_title(if directory {
                t!("ws.upload_folder_dialog").to_string()
            } else {
                t!("ws.upload_file_dialog").to_string()
            });
            let selected = if directory {
                dialog.pick_folder().await
            } else {
                dialog.pick_file().await
            };
            let Some(selected) = selected else {
                return;
            };
            let local_path = selected.path().to_string_lossy().into_owned();
            let confirmed = rfd::AsyncMessageDialog::new()
                .set_title(t!("ws.upload_confirm_title").to_string())
                .set_description(t!("ws.upload_confirm_body").to_string())
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show()
                .await;
            if confirmed != rfd::MessageDialogResult::Ok {
                return;
            }
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                let connection = termior_ssh::Connection {
                    profile,
                    kind: termior_ssh::SessionKind::Sftp,
                    transfer: Some(termior_ssh::Transfer {
                        upload: true,
                        local_path,
                        remote_path: remote_directory,
                        recursive: directory,
                        resume: false,
                    }),
                };
                workspace.connect_remote(connection, window, cx);
            });
        })
        .detach();
    }
    fn choose_remote_download(
        &mut self,
        profile: termior_ssh::Profile,
        remote_path: String,
        recursive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |workspace, cx| {
            let Some(selected) = rfd::AsyncFileDialog::new()
                .set_title(t!("ws.download_folder_dialog").to_string())
                .pick_folder()
                .await
            else {
                return;
            };
            let local_path = selected.path().to_string_lossy().into_owned();
            let confirmed = rfd::AsyncMessageDialog::new()
                .set_title(t!("ws.download_confirm_title").to_string())
                .set_description(t!("ws.download_confirm_body").to_string())
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show()
                .await;
            if confirmed != rfd::MessageDialogResult::Ok {
                return;
            }
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                let connection = termior_ssh::Connection {
                    profile,
                    kind: termior_ssh::SessionKind::Sftp,
                    transfer: Some(termior_ssh::Transfer {
                        upload: false,
                        local_path,
                        remote_path,
                        recursive,
                        resume: false,
                    }),
                };
                workspace.connect_remote(connection, window, cx);
            });
        })
        .detach();
    }
    pub(super) fn remote_explorer_content(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (tab_id, profile, requested_path) = self.active_remote_explorer_context()?;
        let detailed = self.active_is_sftp_browser();
        let drop_wash = ui::selected_wash(&self.palette);
        let state = self.remote_explorers.get(&tab_id);
        let scroll = state.map(|state| state.scroll.clone()).unwrap_or_default();
        let loading = state.is_some_and(|state| state.request.is_some());
        let listing = state
            .and_then(|state| state.listing.as_ref())
            .filter(|listing| listing.cwd == requested_path || requested_path == ".");
        let root = listing
            .map(|listing| listing.cwd.as_str())
            .unwrap_or(requested_path.as_str());
        let root_for_up = root.to_owned();
        let up_profile = profile.clone();
        let toolbar = div()
            .flex()
            .flex_row()
            .gap_1()
            .mb_1()
            .when(detailed, |bar| bar.h(px(28.)).flex_shrink_0().mb_0())
            .child(explorer_tool_button(
                "remote-explorer-up",
                Icon::ChevronUp,
                t!("ws.parent_directory"),
                &self.palette,
                cx,
                move |this, cx| {
                    if let Some(parent) = termior_ssh::sftp::parent(&root_for_up) {
                        this.remote_explorer_paths.insert(tab_id, parent.clone());
                        this.schedule_remote_explorer_scan(tab_id, up_profile.clone(), parent, cx);
                    }
                },
            ))
            .child(explorer_tool_button(
                "remote-explorer-new-file",
                Icon::File,
                t!("explorer.new_file"),
                &self.palette,
                cx,
                |this, cx| this.begin_command(CommandMode::CreateFile, cx),
            ))
            .child(explorer_tool_button(
                "remote-explorer-new-dir",
                Icon::Folder,
                t!("explorer.new_directory"),
                &self.palette,
                cx,
                |this, cx| this.begin_command(CommandMode::CreateDirectory, cx),
            ))
            .child(explorer_tool_button(
                "remote-explorer-refresh",
                Icon::Refresh,
                t!("explorer.refresh"),
                &self.palette,
                cx,
                |this, cx| {
                    this.refresh_remote_explorer(cx);
                },
            ))
            .when(loading, |toolbar| {
                toolbar.child(explorer_tool_button(
                    "remote-explorer-cancel",
                    Icon::Close,
                    t!("ws.cancel_remote_request"),
                    &self.palette,
                    cx,
                    move |this, cx| this.cancel_remote_explorer(tab_id, cx),
                ))
            });

        let rows = listing
            .map(|listing| {
                listing
                    .entries
                    .iter()
                    .cloned()
                    .map(|entry| {
                        let path = entry.path.clone();
                        let right_path = path.clone();
                        let is_dir = entry.is_dir;
                        let selected = self.remote_explorer_selected.as_deref() == Some(&path);
                        let drag = sftp_browser::RemoteFileDrag {
                            tab_id,
                            path: path.clone(),
                            is_dir,
                            name: entry.name.clone(),
                        };
                        let drop_path = path.clone();
                        let external_drop_path = path.clone();
                        let debug_path = path.clone();
                        let profile = profile.clone();
                        div()
                            .id(SharedString::from(format!("remote-file-{path}")))
                            .debug_selector(move || format!("remote-file-{debug_path}"))
                            .w_full()
                            .when(detailed, |row| {
                                row.h(px(28.)).flex_shrink_0().flex().items_center()
                            })
                            .px_1()
                            .py(px(2.0))
                            .rounded_sm()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |row| row.bg(ui::selected_wash(&self.palette)))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .w_full()
                                    .min_w_0()
                                    .items_center()
                                    .gap_1()
                                    .whitespace_nowrap()
                                    .child(explorer_icon(
                                        remote_icon_kind(&entry.name, entry.is_dir),
                                        false,
                                        &self.palette,
                                    ))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .child(SharedString::from(entry.name)),
                                    )
                                    .when(detailed, |row| {
                                        row.child(div().w(px(68.)).flex_shrink_0().child(
                                            if entry.is_symlink {
                                                t!("ws.type_symlink")
                                            } else if is_dir {
                                                t!("ws.type_folder")
                                            } else {
                                                t!("ws.type_file")
                                            },
                                        ))
                                        .child(
                                            div().w(px(90.)).flex_shrink_0().text_right().child(
                                                sftp_browser::display_size(entry.size, is_dir),
                                            ),
                                        )
                                    }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    this.explorer_context_menu = None;
                                    if let Some(browser) = this.sftp_browsers.get_mut(&tab_id) {
                                        browser.local_focused = false;
                                    }
                                    this.remote_explorer_selected = Some(path.clone());
                                    window.focus(&this.focus_handle, cx);
                                    if is_dir && event.click_count == 2 {
                                        this.remote_explorer_paths.insert(tab_id, path.clone());
                                        this.schedule_remote_explorer_scan(
                                            tab_id,
                                            profile.clone(),
                                            path.clone(),
                                            cx,
                                        );
                                    } else {
                                        this.remote_explorer_selected = Some(path.clone());
                                        window.focus(&this.focus_handle, cx);
                                    }
                                    cx.notify();
                                }),
                            )
                            .when(detailed, |row| {
                                row.on_drag(drag, |drag, _, _, cx| {
                                    cx.new(|_| sftp_browser::FileDragPreview(drag.name.clone()))
                                })
                            })
                            .when(detailed && is_dir, |row| {
                                row.drag_over::<sftp_browser::LocalFileDrag>(
                                    move |style, _, _, _| style.bg(drop_wash),
                                )
                                .on_drop(cx.listener(
                                    move |this, drag: &sftp_browser::LocalFileDrag, window, cx| {
                                        cx.stop_propagation();
                                        this.upload_browser_paths(
                                            tab_id,
                                            vec![drag.path.clone()],
                                            drop_path.clone(),
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                                .on_drop(cx.listener(
                                    move |this, paths: &gpui::ExternalPaths, window, cx| {
                                        cx.stop_propagation();
                                        this.upload_browser_paths(
                                            tab_id,
                                            paths.0.to_vec(),
                                            external_drop_path.clone(),
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                            })
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    if let Some(browser) = this.sftp_browsers.get_mut(&tab_id) {
                                        browser.local_focused = false;
                                    }
                                    this.remote_explorer_selected = Some(right_path.clone());
                                    this.show_explorer_context_menu(
                                        if is_dir {
                                            ExplorerContextTarget::RemoteDirectory(
                                                right_path.clone(),
                                            )
                                        } else {
                                            ExplorerContextTarget::RemoteFile(right_path.clone())
                                        },
                                        event.position,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let target = ExplorerContextTarget::RemoteWorkspace(root.to_owned());
        let host_label = if profile.user.is_empty() {
            profile.host.clone()
        } else {
            format!("{}@{}", profile.user, profile.host)
        };
        let scrollbar_scroll = scroll.clone();
        let scrollbar_thumb = ui::alpha(self.palette.foreground, 0.55);
        let scrollbar = div()
            .id("remote-explorer-scrollbar")
            .debug_selector(|| "remote-explorer-scrollbar".into())
            .w(px(10.0))
            .h_full()
            .flex_shrink_0()
            .cursor_pointer()
            .bg(ui::alpha(self.palette.foreground, 0.08))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_remote_explorer_scroll_drag(tab_id, event.position.y);
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        let (top, height) = scroll_thumb_geometry(
                            f32::from(bounds.size.height),
                            f32::from(scrollbar_scroll.max_offset().y),
                            f32::from(scrollbar_scroll.offset().y),
                        );
                        window.paint_quad(gpui::fill(
                            Bounds::new(
                                bounds.origin + gpui::point(px(2.0), px(top)),
                                size(px(6.0), px(height)),
                            ),
                            scrollbar_thumb,
                        ));
                    },
                )
                .size_full(),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .size_full()
                .when(detailed, |content| content.flex_1().min_h_0())
                .child(toolbar)
                .child(
                    div()
                        .px_1()
                        .pb_1()
                        .when(detailed, |row| {
                            row.h(px(28.))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                        })
                        .text_xs()
                        .text_color(ui::muted(&self.palette))
                        .child(SharedString::from(if loading {
                            tf!(
                                "ws.remote_footer_loading",
                                "host" => host_label,
                                "root" => root.to_owned()
                            )
                            .to_string()
                        } else {
                            tf!(
                                "ws.remote_footer_ready",
                                "host" => host_label,
                                "root" => root.to_owned()
                            )
                            .to_string()
                        })),
                )
                .child(
                    div()
                        .px_1()
                        .pb_1()
                        .when(detailed, |row| {
                            row.h(px(22.))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                        })
                        .text_xs()
                        .text_color(ui::muted(&self.palette))
                        .child(SharedString::from(
                            state
                                .map(|s| s.status.clone())
                                .unwrap_or_else(|| t!("ws.remote_reconnect_hint").to_string()),
                        )),
                )
                .children(state.and_then(|state| state.error.clone()).map(|error| {
                    div()
                        .px_1()
                        .py_1()
                        .text_xs()
                        .text_color(gpui_color(self.palette.status[3]))
                        .child(SharedString::from(error))
                }))
                .when(detailed, |content| {
                    content.child(sftp_browser::file_columns(&self.palette))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_1()
                        .min_h(px(0.0))
                        .min_w(px(0.0))
                        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                if let Some((drag_tab, pointer_offset)) =
                                    this.remote_explorer_scroll_drag
                                {
                                    if drag_tab == tab_id {
                                        this.scroll_remote_explorer_to_pointer(
                                            tab_id,
                                            event.position.y,
                                            pointer_offset,
                                        );
                                        cx.notify();
                                    }
                                }
                            } else if event.pressed_button != Some(MouseButton::Left) {
                                this.remote_explorer_scroll_drag = None;
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.remote_explorer_scroll_drag = None),
                        )
                        .child(
                            div()
                                .id("remote-explorer-scroll")
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full()
                                .overflow_x_scroll()
                                .overflow_y_scroll()
                                .track_scroll(&scroll)
                                .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        this.show_explorer_context_menu(
                                            target.clone(),
                                            event.position,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                                .children(rows)
                                .children(
                                    (listing.is_some_and(|listing| listing.entries.is_empty())
                                        && !loading)
                                        .then(|| empty_hint(t!("ws.remote_empty"), &self.palette)),
                                ),
                        )
                        .child(scrollbar),
                )
                .into_any_element(),
        )
    }
}
