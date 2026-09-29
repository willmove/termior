//! User-owned graphical SFTP sessions. Remote operations reuse the workspace's
//! persistent SFTP client; transfers retain the existing cancellable PTY jobs.
use super::*;
use termior_ssh::{Connection, Profile, SessionKind, Transfer};

pub(super) fn is_browser_connection(connection: &Connection) -> bool {
    connection.kind == SessionKind::Sftp && connection.transfer.is_none()
}

/// 连接/分组变更的统一落盘路径：load → mutate → 原子 save → 同步两个视图。
/// 成功时以 `message` 提示，失败时把错误写进命令栏消息。
async fn apply_profiles_mutation(
    workspace: gpui::WeakEntity<WorkspaceView>,
    cx: &mut gpui::AsyncApp,
    dir: Option<PathBuf>,
    message: String,
    mutate: impl FnOnce(&mut termior_ssh::Profiles) -> Result<(), String>,
) {
    let result = (|| -> Result<(), String> {
        let dir = dir
            .as_ref()
            .ok_or_else(|| t!("sftp.error.data_dir_unavailable").to_string())?;
        let mut profiles = termior_ssh::Profiles::load(dir).map_err(|e| e.to_string())?;
        mutate(&mut profiles)?;
        profiles.save(dir).map_err(|e| e.to_string())?;
        Ok(())
    })();
    let _ = workspace.update(cx, |this, cx| {
        this.reload_ssh_profiles();
        if let Some(handle) = this.ssh_manager_window {
            let _ = handle.update(cx, |view, _, cx| view.refresh_saved_profiles(cx));
        }
        this.command_message = Some(result.map(|_| message).unwrap_or_else(|e| e));
        cx.notify();
    });
}

#[derive(Clone)]
pub(super) struct SessionMenu {
    profile: Profile,
    position: Point<Pixels>,
    /// 第二阶段面板：为连接挑选目标分组。
    phase: SessionMenuPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionMenuPhase {
    Session,
    GroupPicker,
}

/// 分组表头的右键菜单目标。
#[derive(Clone)]
pub(super) struct GroupMenu {
    name: String,
    position: Point<Pixels>,
}

pub(super) struct LocalBrowserState {
    pub connected: bool,
    transfers: Vec<TabId>,
    pub(super) local_focused: bool,
    pub(super) path: PathBuf,
    entries: Vec<termior_explorer::DirectoryEntry>,
    selected: Option<PathBuf>,
    scroll: ScrollHandle,
    generation: u64,
    loading: bool,
    error: Option<String>,
}

#[derive(Clone)]
pub(super) struct LocalFileDrag {
    pub path: PathBuf,
}
#[derive(Clone)]
pub(super) struct RemoteFileDrag {
    pub tab_id: TabId,
    pub path: String,
    pub is_dir: bool,
    pub name: String,
}
pub(super) struct FileDragPreview(pub String);
impl Render for FileDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(gpui::rgb(0x263445))
            .text_color(gpui::rgb(0xffffff))
            .text_sm()
            .child(self.0.clone())
    }
}

pub(super) fn display_size(size: Option<u64>, directory: bool) -> String {
    if directory {
        return "—".into();
    }
    match size {
        Some(n) if n >= 1024 * 1024 * 1024 => {
            format!("{:.1} GB", n as f64 / (1024. * 1024. * 1024.))
        }
        Some(n) if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
        Some(n) if n >= 1024 => format!("{:.1} KB", n as f64 / 1024.),
        Some(n) => format!("{n} B"),
        None => "—".into(),
    }
}

impl WorkspaceView {
    pub(super) fn active_is_sftp_browser(&self) -> bool {
        self.model
            .active_tab()
            .and_then(|tab| tab.remote.as_ref())
            .is_some_and(is_browser_connection)
    }

    pub(super) fn ssh_session_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = div()
            .flex()
            .flex_col()
            .min_w_0()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .child(div().text_sm().child(t!("sftp.panel_title")))
                    .child(
                        ui::button(
                            "ssh-manage",
                            t!("sftp.manage"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_ssh_manager(window, cx)),
                        ),
                    ),
            )
            .child(div().px_2().pb_2().text_xs().child(t!("sftp.session_hint")));
        if let Some(error) = &self.ssh_profiles_error {
            panel = panel.child(
                div()
                    .px_2()
                    .text_xs()
                    .child(tf!("sftp.profiles_load_failed", "error" => error)),
            );
        } else if self.ssh_profiles.connections.is_empty() && self.ssh_profiles.groups.is_empty() {
            panel = panel.child(div().px_2().text_xs().child(t!("sftp.no_connections")));
        }
        // 未分组连接在前（无表头），分组按持久化顺序随后；分组不改变存储顺序。
        let partition = termior_ssh::group_connections(
            &self.ssh_profiles.connections,
            &self.ssh_profiles.groups,
        );
        for index in &partition.ungrouped {
            panel = panel.child(self.saved_session_row(*index, false, cx));
        }
        for (group_index, (group, members)) in partition.groups.iter().enumerate() {
            let collapsed = self.ssh_collapsed_groups.contains(group);
            panel = panel.child(self.group_header_row(
                group_index,
                group,
                members.len(),
                collapsed,
                cx,
            ));
            if !collapsed {
                for index in members {
                    panel = panel.child(self.saved_session_row(*index, true, cx));
                }
            }
        }
        panel = panel.child(
            ui::button(
                "ssh-new-group",
                t!("sftp.new_group"),
                ButtonKind::Subtle,
                &self.palette,
            )
            .debug_selector(|| "ssh-new-group".into())
            .on_click(cx.listener(|this, _, window, cx| {
                this.begin_group_command(CommandMode::NewGroup, "", None, None, window, cx);
            })),
        );
        let sessions: Vec<_> = self
            .model
            .tabs
            .iter()
            .filter(|tab| tab.remote.is_some())
            .collect();
        if !sessions.is_empty() {
            panel = panel.child(
                div()
                    .mt_3()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .child(t!("sftp.open_sessions")),
            );
            for (index, tab) in sessions.into_iter().enumerate() {
                let id = tab.id;
                let wash = ui::hover_wash(&self.palette);
                panel = panel.child(
                    div()
                        .id(("ssh-open", index))
                        .h(px(28.))
                        .flex_shrink_0()
                        .px_2()
                        .flex()
                        .items_center()
                        .text_xs()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .when(self.model.active == Some(id), |row| {
                            row.bg(ui::selected_wash(&self.palette))
                        })
                        .hover(move |s| s.bg(wash))
                        .child(tab.title.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_runtime(id, cx);
                            this.focus_active_pane(window, cx);
                            cx.notify();
                        })),
                );
            }
        }
        panel.into_any_element()
    }

    /// 保存连接的单行；`grouped` 时缩进一层。索引是 connections 的真实下标，
    /// `ssh-saved-{index}` 选择器为测试保持稳定。
    fn saved_session_row(
        &self,
        index: usize,
        grouped: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let profile = self.ssh_profiles.connections[index].clone();
        let left = profile.clone();
        let right = profile.clone();
        let label = profile.name.clone();
        let selected = self.ssh_selected.as_deref() == Some(&profile.name);
        let tooltip = if profile.user.is_empty() {
            profile.host.clone()
        } else {
            format!("{}@{}", profile.user, profile.host)
        };
        let wash = ui::hover_wash(&self.palette);
        let tooltip_palette = self.palette.clone();
        div()
            .id(("ssh-saved", index))
            .debug_selector(move || format!("ssh-saved-{index}"))
            .h(px(28.))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .when(grouped, |row| row.pl(px(20.)))
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .cursor_pointer()
            .overflow_hidden()
            .hover(move |style| style.bg(wash))
            .when(selected, |row| row.bg(ui::selected_wash(&self.palette)))
            .child(ui::icon(
                Icon::Terminal,
                icon_size::SM,
                gpui_color(self.palette.accent),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(label),
            )
            .tooltip(move |_, cx| Tooltip::view(tooltip.clone(), &tooltip_palette, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.ssh_selected = Some(left.name.clone());
                    this.ssh_context_menu = None;
                    this.ssh_group_menu = None;
                    window.focus(&this.focus_handle, cx);
                    if event.click_count == 2 {
                        this.connect_remote(
                            Connection {
                                profile: left.clone(),
                                kind: SessionKind::Shell,
                                transfer: None,
                            },
                            window,
                            cx,
                        );
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.explorer_context_menu = None;
                    this.pane_context_menu = None;
                    this.ssh_group_menu = None;
                    this.ssh_selected = Some(right.name.clone());
                    this.ssh_context_menu = Some(SessionMenu {
                        profile: right.clone(),
                        position: event.position,
                        phase: SessionMenuPhase::Session,
                    });
                    window.focus(&this.focus_handle, cx);
                    cx.notify();
                }),
            )
    }

    /// 分组表头：折叠开关 + 名称 + 成员数；右键打开分组菜单。
    fn group_header_row(
        &self,
        group_index: usize,
        group: &str,
        member_count: usize,
        collapsed: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let foreground = ui::muted(&self.palette);
        let wash = ui::hover_wash(&self.palette);
        let name = group.to_owned();
        let menu_name = group.to_owned();
        div()
            .id(("ssh-group", group_index))
            .debug_selector(move || format!("ssh-group-{group_index}"))
            .h(px(tokens::height::COMPACT))
            .mt(px(tokens::space::SM))
            .flex_shrink_0()
            .w_full()
            .px_2()
            .flex()
            .items_center()
            .gap(px(tokens::space::XS))
            .text_size(px(tokens::font_size::MICRO))
            .text_color(foreground)
            .cursor_pointer()
            .overflow_hidden()
            .hover(move |style| style.bg(wash))
            .child(ui::icon(
                if collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                },
                icon_size::SM,
                foreground,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name.clone()),
            )
            .child(div().flex_shrink_0().child(format!("{member_count}")))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.ssh_context_menu = None;
                    this.ssh_group_menu = None;
                    if !this.ssh_collapsed_groups.remove(&name) {
                        this.ssh_collapsed_groups.insert(name.clone());
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.explorer_context_menu = None;
                    this.pane_context_menu = None;
                    this.ssh_context_menu = None;
                    this.ssh_group_menu = Some(GroupMenu {
                        name: menu_name.clone(),
                        position: event.position,
                    });
                    window.focus(&this.focus_handle, cx);
                    cx.notify();
                }),
            )
    }

    pub(super) fn ssh_session_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.ssh_context_menu.clone()?;
        let mut panel = menu_panel(&self.palette).id("ssh-session-menu").w(px(200.));
        match menu.phase {
            SessionMenuPhase::Session => {
                for (index, label) in [
                    t!("sftp.menu.connect_ssh"),
                    t!("sftp.menu.connect_sftp"),
                    t!("sftp.menu.rename"),
                    t!("sftp.menu.edit"),
                    t!("sftp.menu.move_to_group"),
                    t!("sftp.menu.delete"),
                ]
                .into_iter()
                .enumerate()
                {
                    let profile = menu.profile.clone();
                    let position = menu.position;
                    let wash = ui::hover_wash(&self.palette);
                    panel = panel.child(
                        div()
                            .id(("ssh-menu", index))
                            .debug_selector(move || format!("ssh-menu-item-{index}"))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .cursor_pointer()
                            .hover(move |style| style.bg(wash))
                            .child(label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    match index {
                                        0 | 1 => {
                                            this.ssh_context_menu = None;
                                            this.connect_remote(
                                                Connection {
                                                    profile: profile.clone(),
                                                    kind: if index == 0 {
                                                        SessionKind::Shell
                                                    } else {
                                                        SessionKind::Sftp
                                                    },
                                                    transfer: None,
                                                },
                                                window,
                                                cx,
                                            );
                                        }
                                        2 | 3 => {
                                            this.ssh_context_menu = None;
                                            this.open_ssh_manager(window, cx);
                                            if let Some(handle) = this.ssh_manager_window {
                                                let _ = handle.update(cx, |view, _, cx| {
                                                    view.edit_saved(&profile.name, index == 2, cx)
                                                });
                                            }
                                        }
                                        4 => {
                                            this.ssh_context_menu = Some(SessionMenu {
                                                profile: profile.clone(),
                                                position,
                                                phase: SessionMenuPhase::GroupPicker,
                                            });
                                        }
                                        _ => {
                                            this.ssh_context_menu = None;
                                            this.delete_saved_session(profile.clone(), window, cx);
                                        }
                                    }
                                    cx.notify();
                                }),
                            ),
                    );
                }
            }
            SessionMenuPhase::GroupPicker => {
                let mut targets: Vec<(String, bool)> =
                    vec![(String::new(), menu.profile.group.is_empty())];
                for group in &self.ssh_profiles.groups {
                    targets.push((group.clone(), *group == menu.profile.group));
                }
                for (index, (group, current)) in targets.iter().enumerate() {
                    let target = group.clone();
                    let profile_name = menu.profile.name.clone();
                    let wash = ui::hover_wash(&self.palette);
                    panel = panel.child(
                        div()
                            .id(("ssh-group-target", index))
                            .debug_selector(move || format!("ssh-group-target-{index}"))
                            .px_3()
                            .py_1()
                            .text_sm()
                            .cursor_pointer()
                            .hover(move |style| style.bg(wash))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(if target.is_empty() {
                                t!("sftp.ungrouped")
                            } else {
                                target.clone().into()
                            })
                            .when(*current, |item| {
                                item.text_color(gpui_color(self.palette.accent)).child("✓")
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let target = target.clone();
                                    this.ssh_context_menu = None;
                                    this.move_session_to_group(profile_name.clone(), target, cx);
                                    cx.notify();
                                }),
                            ),
                    );
                }
                let profile_name = menu.profile.name.clone();
                let wash = ui::hover_wash(&self.palette);
                panel = panel.child(
                    div()
                        .id("ssh-group-new")
                        .debug_selector(|| "ssh-group-new".into())
                        .px_3()
                        .py_1()
                        .text_sm()
                        .cursor_pointer()
                        .hover(move |style| style.bg(wash))
                        .child(t!("sftp.menu.new_group"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                let profile_name = profile_name.clone();
                                this.begin_group_command(
                                    CommandMode::NewGroup,
                                    "",
                                    Some(profile_name),
                                    None,
                                    window,
                                    cx,
                                );
                            }),
                        ),
                );
            }
        }
        Some(
            anchored()
                .position(menu.position)
                .child(panel)
                .into_any_element(),
        )
    }

    pub(super) fn ssh_group_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.ssh_group_menu.clone()?;
        let mut panel = menu_panel(&self.palette).id("ssh-group-menu").w(px(200.));
        for (index, label) in [t!("sftp.group.rename"), t!("sftp.group.delete")]
            .into_iter()
            .enumerate()
        {
            let name = menu.name.clone();
            let wash = ui::hover_wash(&self.palette);
            panel = panel.child(
                div()
                    .id(("ssh-group-action", index))
                    .debug_selector(move || format!("ssh-group-action-{index}"))
                    .px_3()
                    .py_1()
                    .text_sm()
                    .cursor_pointer()
                    .hover(move |style| style.bg(wash))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            let name = name.clone();
                            this.ssh_context_menu = None;
                            this.ssh_group_menu = None;
                            if index == 0 {
                                let payload = name.clone();
                                this.begin_group_command(
                                    CommandMode::RenameGroup,
                                    &name,
                                    None,
                                    Some(payload),
                                    window,
                                    cx,
                                );
                            } else {
                                this.delete_group(name, window, cx);
                            }
                            cx.notify();
                        }),
                    ),
            );
        }
        Some(
            anchored()
                .position(menu.position)
                .child(panel)
                .into_any_element(),
        )
    }

    fn move_session_to_group(&mut self, name: String, group: String, cx: &mut Context<Self>) {
        let message = if group.is_empty() {
            tf!("sftp.message.moved_out_of_group", "name" => name).to_string()
        } else {
            tf!(
                "sftp.message.moved_to_group",
                "name" => name,
                "group" => group
            )
            .to_string()
        };
        self.mutate_saved_profiles(
            message,
            move |profiles| {
                let index = profiles
                    .connections
                    .iter()
                    .position(|p| p.name == name)
                    .ok_or_else(|| t!("sftp.error.profiles_changed").to_string())?;
                profiles.upsert_group(&group);
                profiles.connections[index].group = group;
                Ok(())
            },
            cx,
        );
    }

    /// 新建分组；`assign` 给出连接名时同时把它移入新分组（来自连接的右键菜单）。
    pub(super) fn create_group(
        &mut self,
        group: String,
        assign: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let message = match &assign {
            Some(name) => tf!(
                "sftp.message.moved_to_group",
                "name" => name,
                "group" => group
            )
            .to_string(),
            None if self.ssh_profiles.groups.contains(&group) => {
                tf!("sftp.message.group_exists", "group" => group).to_string()
            }
            None => tf!("sftp.message.group_created", "group" => group).to_string(),
        };
        self.mutate_saved_profiles(
            message,
            move |profiles| {
                profiles.upsert_group(&group);
                if let Some(name) = assign {
                    let index = profiles
                        .connections
                        .iter()
                        .position(|p| p.name == name)
                        .ok_or_else(|| t!("sftp.error.profiles_changed").to_string())?;
                    profiles.connections[index].group = group;
                }
                Ok(())
            },
            cx,
        );
    }

    pub(super) fn rename_group(&mut self, old: String, new: String, cx: &mut Context<Self>) {
        let message = tf!(
            "sftp.message.group_renamed",
            "old" => old,
            "new" => new
        )
        .to_string();
        self.mutate_saved_profiles(
            message,
            move |profiles| {
                if old == new {
                    return Ok(());
                }
                if !profiles.groups.contains(&old) {
                    return Err(t!("sftp.error.group_missing").to_string());
                }
                // 与现有分组重名即合并：成员全部并入既有分组，旧名移除。
                if profiles.groups.contains(&new) {
                    profiles.groups.retain(|name| *name != old);
                } else if let Some(slot) = profiles.groups.iter_mut().find(|name| **name == old) {
                    *slot = new.clone();
                }
                for connection in &mut profiles.connections {
                    if connection.group == old {
                        connection.group = new.clone();
                    }
                }
                Ok(())
            },
            cx,
        );
    }

    fn delete_group(&mut self, group: String, window: &mut Window, cx: &mut Context<Self>) {
        let members = self
            .ssh_profiles
            .connections
            .iter()
            .filter(|p| p.group == group)
            .count();
        let detail = if members == 0 {
            tf!("sftp.confirm.delete_empty_group", "group" => group).to_string()
        } else {
            tf!(
                "sftp.confirm.delete_group",
                "group" => group,
                "members" => tn!(members, "sftp.connection_count")
            )
            .to_string()
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("sftp.confirm.delete_group_title"),
            Some(&detail),
            &[
                PromptButton::ok(t!("sftp.delete")),
                PromptButton::cancel(t!("sftp.cancel")),
            ],
            cx,
        );
        let dir = self.data_dir.clone();
        let label = group.clone();
        cx.spawn(async move |workspace, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            apply_profiles_mutation(
                workspace,
                cx,
                dir,
                tf!("sftp.message.group_deleted", "group" => label).to_string(),
                move |profiles| {
                    profiles.groups.retain(|name| *name != group);
                    for connection in &mut profiles.connections {
                        if connection.group == group {
                            connection.group.clear();
                        }
                    }
                    Ok(())
                },
            )
            .await;
        })
        .detach();
    }

    /// 读取-修改-原子保存 `Termior-ssh.json`，随后同步侧栏与打开的管理窗口。
    fn mutate_saved_profiles(
        &mut self,
        message: String,
        mutate: impl FnOnce(&mut termior_ssh::Profiles) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let dir = self.data_dir.clone();
        cx.spawn(async move |workspace, cx| {
            apply_profiles_mutation(workspace, cx, dir, message, mutate).await;
        })
        .detach();
    }

    fn delete_saved_session(
        &mut self,
        profile: Profile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("sftp.confirm.delete_session_title"),
            Some(tf!("sftp.confirm.delete_session", "name" => profile.name).as_ref()),
            &[
                PromptButton::ok(t!("sftp.delete")),
                PromptButton::cancel(t!("sftp.cancel")),
            ],
            cx,
        );
        let dir = self.data_dir.clone();
        cx.spawn(async move |workspace, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let result = (|| -> Result<(), String> {
                let dir = dir
                    .as_ref()
                    .ok_or_else(|| t!("sftp.error.data_dir_unavailable").to_string())?;
                let mut profiles = termior_ssh::Profiles::load(dir).map_err(|e| e.to_string())?;
                let index = profiles
                    .connections
                    .iter()
                    .position(|saved| saved == &profile)
                    .ok_or_else(|| t!("sftp.error.session_changed").to_string())?;
                profiles.connections.remove(index);
                profiles.save(dir).map_err(|e| e.to_string())?;
                if profile.use_saved_credentials
                    && !profiles.connections.iter().any(|p| {
                        p.use_saved_credentials
                            && termior_ssh::credentials::same_target(p, &profile)
                    })
                {
                    termior_ssh::credentials::delete_all(&profile).map_err(|e| {
                        tf!("sftp.error.credentials_cleanup_failed", "error" => e).to_string()
                    })?;
                }
                Ok(())
            })();
            let _ = workspace.update(cx, |this, cx| {
                this.reload_ssh_profiles();
                if let Some(handle) = this.ssh_manager_window {
                    let _ = handle.update(cx, |view, _, cx| view.refresh_saved_profiles(cx));
                }
                this.ssh_selected = None;
                this.command_message = Some(
                    result
                        .map(|_| t!("sftp.message.session_deleted").to_string())
                        .unwrap_or_else(|e| e),
                );
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn handle_sftp_local_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.active_is_sftp_browser()
            || self.command_mode != CommandMode::Browse
            || !self.focus_handle.is_focused(window)
        {
            return false;
        }
        let Some(id) = self.model.active else {
            return false;
        };
        let Some(state) = self
            .sftp_browsers
            .get_mut(&id)
            .filter(|state| state.local_focused)
        else {
            return false;
        };
        let key = event.keystroke.key.as_str();
        if matches!(key, "left" | "backspace") {
            if let Some(parent) = state.path.parent().map(Path::to_path_buf) {
                self.scan_sftp_local(id, parent, cx);
            }
            return true;
        }
        if state.entries.is_empty() {
            return matches!(
                key,
                "up" | "down" | "home" | "end" | "enter" | "return" | "right"
            );
        }
        let selected = state
            .entries
            .iter()
            .position(|entry| Some(&entry.path) == state.selected.as_ref())
            .unwrap_or(0);
        let index = match key {
            "up" => selected.saturating_sub(1),
            "down" => (selected + 1).min(state.entries.len() - 1),
            "home" => 0,
            "end" => state.entries.len() - 1,
            "enter" | "return" | "right" => {
                let entry = &state.entries[selected];
                if entry.is_dir {
                    let path = entry.path.clone();
                    self.scan_sftp_local(id, path, cx);
                }
                return true;
            }
            _ => return false,
        };
        state.selected = Some(state.entries[index].path.clone());
        state.scroll.scroll_to_item(index);
        cx.notify();
        true
    }

    pub(super) fn start_sftp_browser(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(tab) = self.model.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(remote) = tab.remote.clone() else {
            return;
        };
        let initial = tab.project_dir.clone();
        let state = self
            .sftp_browsers
            .entry(tab_id)
            .or_insert_with(|| LocalBrowserState {
                connected: true,
                transfers: Vec::new(),
                local_focused: false,
                path: initial,
                entries: Vec::new(),
                selected: None,
                scroll: ScrollHandle::new(),
                generation: 0,
                loading: false,
                error: None,
            });
        state.connected = true;
        let local_path = state.path.clone();
        self.scan_sftp_local(tab_id, local_path, cx);
        self.scan_remote_directory(tab_id, remote.profile, ".".into(), false, cx);
        cx.notify();
    }

    pub(super) fn scan_sftp_local(&mut self, tab_id: TabId, path: PathBuf, cx: &mut Context<Self>) {
        let Some(state) = self.sftp_browsers.get_mut(&tab_id) else {
            return;
        };
        state.generation += 1;
        let generation = state.generation;
        state.loading = true;
        state.error = None;
        cx.spawn(async move |workspace, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    termior_explorer::list_directory(&path).map(|entries| (path, entries))
                })
                .await;
            let _ = workspace.update(cx, |this, cx| {
                let Some(state) = this.sftp_browsers.get_mut(&tab_id) else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.loading = false;
                match result {
                    Ok((path, entries)) => {
                        if state.path != path {
                            state.scroll.set_offset(gpui::point(px(0.), px(0.)));
                        }
                        state.path = path;
                        state.entries = entries;
                        state.selected = None;
                    }
                    Err(error) => state.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose_sftp_local_directory(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        cx.spawn(async move |workspace, cx| {
            if let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title(t!("sftp.choose_local_dir").to_string())
                .pick_folder()
                .await
            {
                let _ = workspace.update(cx, |this, cx| {
                    this.scan_sftp_local(tab_id, folder.path().to_path_buf(), cx)
                });
            }
        })
        .detach();
    }

    pub(super) fn upload_browser_paths(
        &mut self,
        tab_id: TabId,
        paths: Vec<PathBuf>,
        remote_directory: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .model
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.remote.as_ref())
            .map(|r| r.profile.clone())
        else {
            return;
        };
        if !self.remote_runtime_connected(tab_id, cx) {
            return;
        }
        cx.spawn_in(window, async move |workspace, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let metadata =
                                std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                            if metadata.file_type().is_symlink() {
                                return Err(t!("sftp.error.no_symlink_upload").to_string());
                            }
                            let local_path = path
                                .to_str()
                                .ok_or_else(|| t!("sftp.error.local_name_unicode").to_string())?
                                .to_owned();
                            let transfer = Transfer {
                                upload: true,
                                local_path,
                                remote_path: remote_directory.clone(),
                                recursive: metadata.is_dir(),
                                resume: false,
                            };
                            // Validate before presenting or creating any jobs.
                            transfer.batch().map_err(|e| e.to_string())?;
                            Ok(transfer)
                        })
                        .collect::<Result<Vec<_>, String>>()
                })
                .await;
            let _ = workspace.update_in(cx, |this, window, cx| match result {
                Ok(transfers) => {
                    this.confirm_browser_transfers(tab_id, profile, transfers, window, cx)
                }
                Err(error) => {
                    this.command_message = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn download_browser_entry(
        &mut self,
        drag: &RemoteFileDrag,
        local_directory: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .model
            .tabs
            .iter()
            .find(|t| t.id == drag.tab_id)
            .and_then(|t| t.remote.as_ref())
            .map(|r| r.profile.clone())
        else {
            return;
        };
        if !self.remote_runtime_connected(drag.tab_id, cx) {
            return;
        }
        let Some(local_path) = local_directory.to_str() else {
            self.command_message = Some(t!("sftp.error.local_path_unicode").to_string());
            cx.notify();
            return;
        };
        self.confirm_browser_transfers(
            drag.tab_id,
            profile,
            vec![Transfer {
                upload: false,
                local_path: local_path.into(),
                remote_path: drag.path.clone(),
                recursive: drag.is_dir,
                resume: false,
            }],
            window,
            cx,
        );
    }

    fn confirm_browser_transfers(
        &mut self,
        source_tab: TabId,
        profile: Profile,
        transfers: Vec<Transfer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if transfers.is_empty() {
            return;
        }
        if let Some(error) = transfers.iter().find_map(|transfer| transfer.batch().err()) {
            self.command_message = Some(error.to_string());
            cx.notify();
            return;
        }
        let description = transfers
            .iter()
            .map(|t| {
                if t.upload {
                    tf!(
                        "sftp.transfer.upload_line",
                        "local" => t.local_path,
                        "host" => profile.host,
                        "remote" => t.remote_path
                    )
                    .to_string()
                } else {
                    tf!(
                        "sftp.transfer.download_line",
                        "host" => profile.host,
                        "remote" => t.remote_path,
                        "local" => t.local_path
                    )
                    .to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let answer = window.prompt(
            PromptLevel::Warning,
            &t!("sftp.confirm.transfer_title"),
            Some(&format!(
                "{description}\n\n{}",
                t!("sftp.confirm.transfer_warning")
            )),
            &[
                PromptButton::ok(t!("sftp.start_transfer")),
                PromptButton::cancel(t!("sftp.cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |workspace, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let _ = workspace.update_in(cx, |this, window, cx| {
                // A confirmation cannot resurrect a closed/disconnected source session.
                if !this.remote_runtime_connected(source_tab, cx) {
                    return;
                }
                let mut jobs = Vec::new();
                for transfer in transfers {
                    this.connect_remote(
                        Connection {
                            profile: profile.clone(),
                            kind: SessionKind::Sftp,
                            transfer: Some(transfer),
                        },
                        window,
                        cx,
                    );
                    if let Some(id) = this.model.active {
                        jobs.push(id);
                    }
                }
                if let Some(browser) = this.sftp_browsers.get_mut(&source_tab) {
                    browser.transfers.extend(jobs);
                    if browser.transfers.len() > 32 {
                        browser.transfers.drain(..browser.transfers.len() - 32);
                    }
                }
                this.activate_runtime(source_tab, cx);
                this.focus_active_pane(window, cx);
                this.persist_workspace();
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn sftp_browser_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some((tab_id, _, remote_path)) = self.active_remote_explorer_context() else {
            return div().into_any_element();
        };
        let Some(state) = self.sftp_browsers.get(&tab_id) else {
            return div()
                .p_4()
                .child(t!("sftp.not_connected"))
                .into_any_element();
        };
        let local_root = state.path.clone();
        let local_drop = local_root.clone();
        let local_refresh = local_root.clone();
        let local_up = local_root.parent().map(Path::to_path_buf);
        let local_upload_root = remote_path.clone();
        let local_selected = state.selected.clone();
        let remote_download_root = local_root.clone();
        let remote_selected = self
            .remote_explorers
            .get(&tab_id)
            .and_then(|s| s.listing.as_ref())
            .and_then(|l| {
                l.entries
                    .iter()
                    .find(|e| self.remote_explorer_selected.as_deref() == Some(&e.path))
            })
            .cloned();
        let mut local = div()
            .flex()
            .flex_col()
            .w(relative(0.42))
            .min_w(px(180.))
            .h_full()
            .min_h_0()
            .px_2()
            .border_r_1()
            .border_color(ui::border(&self.palette))
            .child(
                div()
                    .h(px(30.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .text_sm()
                    .child(t!("sftp.local_files")),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        ui::button(
                            "sftp-local-up",
                            t!("sftp.parent_dir"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(path) = &local_up {
                                this.scan_sftp_local(tab_id, path.clone(), cx);
                            }
                        })),
                    )
                    .child(
                        ui::button(
                            "sftp-local-choose",
                            t!("sftp.choose_dir"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_sftp_local_directory(tab_id, cx)
                        })),
                    )
                    .child(
                        ui::button(
                            "sftp-local-refresh",
                            t!("explorer.refresh"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.scan_sftp_local(tab_id, local_refresh.clone(), cx)
                        })),
                    )
                    .child(
                        ui::button(
                            "sftp-local-upload",
                            t!("sftp.upload_arrow"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if let Some(path) = &local_selected {
                                    this.upload_browser_paths(
                                        tab_id,
                                        vec![path.clone()],
                                        local_upload_root.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .h(px(28.))
                    .flex_shrink_0()
                    .text_xs()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(local_root.to_string_lossy().into_owned()),
            )
            .child(
                div()
                    .h(px(22.))
                    .flex_shrink_0()
                    .text_xs()
                    .child(if state.loading {
                        t!("sftp.loading_local")
                    } else {
                        t!("sftp.local_hint")
                    }),
            )
            .child(file_columns(&self.palette));
        if let Some(error) = &state.error {
            local = local.child(div().text_xs().child(error.clone()));
        }
        let wash = ui::selected_wash(&self.palette);
        let mut rows = div()
            .id("sftp-local-files")
            .debug_selector(|| "sftp-local-files".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&state.scroll)
            .drag_over::<RemoteFileDrag>(move |style, _, _, _| style.bg(wash))
            .on_drop(cx.listener(move |this, drag: &RemoteFileDrag, window, cx| {
                cx.stop_propagation();
                if drag.tab_id == tab_id {
                    this.download_browser_entry(drag, local_drop.clone(), window, cx);
                }
            }));
        for (index, entry) in state.entries.iter().enumerate() {
            let path = entry.path.clone();
            let drop_path = path.clone();
            let is_dir = entry.is_dir;
            let name = entry.name.clone();
            let drag_label = name.clone();
            let drag = LocalFileDrag { path: path.clone() };
            rows = rows.child(
                div()
                    .id(("sftp-local-file", index))
                    .debug_selector(move || format!("sftp-local-file-{index}"))
                    .h(px(28.))
                    .flex_shrink_0()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .cursor_pointer()
                    .when(state.selected.as_ref() == Some(&path), |row| row.bg(wash))
                    .child(explorer_icon(
                        remote_icon_kind(&name, is_dir),
                        false,
                        &self.palette,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(name),
                    )
                    .child(div().w(px(68.)).flex_shrink_0().child(if entry.is_symlink {
                        t!("sftp.type_symlink")
                    } else if is_dir {
                        t!("sftp.type_folder")
                    } else {
                        t!("sftp.type_file")
                    }))
                    .child(
                        div()
                            .w(px(90.))
                            .flex_shrink_0()
                            .text_right()
                            .child(display_size(Some(entry.size), is_dir)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if let Some(state) = this.sftp_browsers.get_mut(&tab_id) {
                                state.selected = Some(path.clone());
                                state.local_focused = true;
                            }
                            window.focus(&this.focus_handle, cx);
                            if is_dir && event.click_count == 2 {
                                this.scan_sftp_local(tab_id, path.clone(), cx);
                            }
                            cx.notify();
                        }),
                    )
                    .on_drag(drag, move |_, _, _, cx| {
                        cx.new(|_| FileDragPreview(drag_label.clone()))
                    })
                    .when(is_dir, |row| {
                        row.drag_over::<RemoteFileDrag>(move |style, _, _, _| style.bg(wash))
                            .on_drop(cx.listener(move |this, drag: &RemoteFileDrag, window, cx| {
                                cx.stop_propagation();
                                if drag.tab_id == tab_id {
                                    this.download_browser_entry(
                                        drag,
                                        drop_path.clone(),
                                        window,
                                        cx,
                                    );
                                }
                            }))
                    }),
            );
        }
        local = local.child(rows).child(div().py_1().text_xs().child(tf!(
            "sftp.local_footer",
            "count" => tn!(state.entries.len(), "sftp.item_count")
        )));
        let remote_drop = remote_path.clone();
        let remote_external = remote_path.clone();
        let remote = div()
            .id("sftp-remote-pane")
            .debug_selector(|| "sftp-remote-pane".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .px_2()
            .drag_over::<LocalFileDrag>(move |style, _, _, _| style.bg(wash))
            .drag_over::<gpui::ExternalPaths>(move |style, _, _, _| style.bg(wash))
            .on_drop(cx.listener(move |this, drag: &LocalFileDrag, window, cx| {
                cx.stop_propagation();
                this.upload_browser_paths(
                    tab_id,
                    vec![drag.path.clone()],
                    remote_drop.clone(),
                    window,
                    cx,
                );
            }))
            .on_drop(
                cx.listener(move |this, paths: &gpui::ExternalPaths, window, cx| {
                    cx.stop_propagation();
                    this.upload_browser_paths(
                        tab_id,
                        paths.0.to_vec(),
                        remote_external.clone(),
                        window,
                        cx,
                    );
                }),
            )
            .child(
                div()
                    .h(px(30.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child(t!("sftp.remote_files")))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                ui::button(
                                    "sftp-remote-path",
                                    t!("sftp.goto_path"),
                                    ButtonKind::Subtle,
                                    &self.palette,
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.begin_name_command(
                                            CommandMode::RemotePath,
                                            PathBuf::new(),
                                            None,
                                            &remote_path,
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                ui::button(
                                    "sftp-download",
                                    t!("sftp.download_arrow"),
                                    ButtonKind::Subtle,
                                    &self.palette,
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(entry) = &remote_selected {
                                            this.download_browser_entry(
                                                &RemoteFileDrag {
                                                    tab_id,
                                                    path: entry.path.clone(),
                                                    is_dir: entry.is_dir,
                                                    name: entry.name.clone(),
                                                },
                                                remote_download_root.clone(),
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )),
                            ),
                    ),
            )
            .children(self.remote_explorer_content(cx));
        let mut browser = div().size_full().flex().flex_col().min_h_0();
        if !state.connected {
            browser = browser.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .child(t!("sftp.disconnected_hint")),
            );
        }
        if self.command_mode != CommandMode::Browse {
            browser = browser.child(
                div()
                    .px_3()
                    .py_2()
                    .border_1()
                    .border_color(gpui_color(self.palette.accent))
                    .text_sm()
                    .child(tf!(
                        "sftp.command_bar",
                        "label" => self.command_mode.label(),
                        "input" => self.command_input,
                        "marked" => self.command_marked_text
                    )),
            );
        }
        if let Some(message) = &self.command_message {
            browser = browser.child(div().px_2().text_xs().child(message.clone()));
        }
        browser = browser.child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h_0()
                .child(local)
                .child(remote),
        );
        let mut transfers = div()
            .id("sftp-transfer-summary")
            .max_h(px(100.))
            .overflow_y_scroll()
            .flex_shrink_0();
        for id in state
            .transfers
            .iter()
            .rev()
            .filter(|id| self.model.tab(**id).is_some())
        {
            let id = *id;
            let Some(job) = self
                .model
                .tab(id)
                .and_then(|t| t.remote.as_ref())
                .and_then(|r| r.transfer.as_ref())
            else {
                continue;
            };
            let terminal = self.tabs.iter().find(|t| t.id == id).and_then(|t| {
                t.panes.values().find_map(|p| {
                    if let PaneContent::Terminal(t) = p {
                        Some(t.clone())
                    } else {
                        None
                    }
                })
            });
            let status = terminal
                .as_ref()
                .map(|t| {
                    let t = t.read(cx);
                    if !t.has_exited() {
                        t!("sftp.status.transferring")
                    } else if t.exit_code() == Some(0) {
                        t!("sftp.status.completed")
                    } else {
                        t!("sftp.status.failed")
                    }
                })
                .unwrap_or(
                    if self.pending_terminals.iter().any(|(tab, _)| *tab == id) {
                        t!("sftp.status.starting")
                    } else {
                        t!("sftp.status.start_failed")
                    },
                );
            let running = terminal.as_ref().is_some_and(|t| !t.read(cx).has_exited());
            let title = tf!(
                "sftp.transfer.title",
                "direction" => if job.upload {
                    t!("sftp.upload_short")
                } else {
                    t!("sftp.download_short")
                },
                "name" => if job.upload {
                    Path::new(&job.local_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    termior_ssh::sftp::file_name(&job.remote_path).to_owned()
                },
                "status" => status
            );
            transfers = transfers.child(
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .child(
                        ui::button(
                            ("sftp-job-view", id.0),
                            t!("sftp.view_progress"),
                            ButtonKind::Subtle,
                            &self.palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.activate_runtime(id, cx);
                                this.focus_active_pane(window, cx);
                                cx.notify();
                            },
                        )),
                    )
                    .when(running, |row| {
                        row.child(
                            ui::button(
                                ("sftp-job-cancel", id.0),
                                t!("sftp.cancel"),
                                ButtonKind::Ghost,
                                &self.palette,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    if let Some(terminal) = &terminal {
                                        terminal.update(cx, |t, _| t.disconnect());
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            );
        }
        browser.child(transfers).into_any_element()
    }
}

pub(super) fn file_columns(palette: &ResolvedPalette) -> Div {
    div()
        .h(px(26.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px_1()
        .gap_1()
        .text_xs()
        .border_b_1()
        .border_color(ui::border(palette))
        .child(div().flex_1().child(t!("sftp.column_name")))
        .child(
            div()
                .w(px(68.))
                .flex_shrink_0()
                .child(t!("sftp.column_type")),
        )
        .child(
            div()
                .w(px(90.))
                .flex_shrink_0()
                .text_right()
                .child(t!("sftp.column_size")),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};

    fn workspace(cx: &mut Context<WorkspaceView>, root: PathBuf) -> WorkspaceView {
        crate::updater::init(cx);
        let mut view = WorkspaceView::new(root.clone(), false, cx);
        view.data_dir = None;
        view.model = WorkspaceState::new(root);
        view.model.sidebar_visible = true;
        view.model.sidebar_panel = SidebarPanel::Ssh;
        view.model.composer_visible = false;
        view.tabs.clear();
        view.ssh_profiles.connections = vec![Profile {
            name: "test session".into(),
            host: "127.0.0.1".into(),
            port: Some(9),
            ..Profile::default()
        }];
        view
    }

    fn install_browser(view: &mut WorkspaceView, root: &Path) -> TabId {
        let profile = view.ssh_profiles.connections[0].clone();
        let id = view.model.new_tab(TabKind::Terminal, "SFTP test", false);
        view.model.active_tab_mut().unwrap().remote = Some(Connection {
            profile: profile.clone(),
            kind: SessionKind::Sftp,
            transfer: None,
        });
        view.explorer_view_tab = Some(id);
        view.tabs.push(AppTab {
            id,
            panes: single_pane(PaneContent::Placeholder("".into())),
        });
        view.sftp_browsers.insert(
            id,
            LocalBrowserState {
                connected: true,
                transfers: Vec::new(),
                local_focused: false,
                path: root.to_path_buf(),
                entries: termior_explorer::list_directory(root).unwrap(),
                selected: None,
                scroll: ScrollHandle::new(),
                generation: 0,
                loading: false,
                error: None,
            },
        );
        view.remote_explorer_paths.insert(id, "/".into());
        view.remote_explorers.insert(
            id,
            RemoteExplorerState {
                client: termior_ssh::sftp::Client::new(profile).unwrap(),
                scroll: ScrollHandle::new(),
                listing: Some(termior_ssh::sftp::RemoteListing {
                    cwd: "/".into(),
                    entries: vec![
                        termior_ssh::sftp::RemoteEntry {
                            name: "folder".into(),
                            path: "/folder".into(),
                            is_dir: true,
                            is_symlink: false,
                            size: None,
                        },
                        termior_ssh::sftp::RemoteEntry {
                            name: "file.txt".into(),
                            path: "/file.txt".into(),
                            is_dir: false,
                            is_symlink: false,
                            size: Some(30),
                        },
                    ],
                }),
                request: None,
                pending: None,
                error: None,
                status: "Connected".into(),
            },
        );
        id
    }

    #[gpui::test]
    fn saved_session_single_click_selects_double_click_connects(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| workspace(cx, root.path().to_path_buf()));
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let bounds = cx.debug_bounds("ssh-saved-0").unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(v.ssh_selected.as_deref(), Some("test session"));
            assert!(v.model.tabs.is_empty(), "single click must not connect");
            // Suppress actual PTY creation; verify the real mouse handler creates exactly one shell tab.
            let mut model = v.model.clone();
            let next = model.new_tab(TabKind::Terminal, "", false);
            v.pending_terminals.insert((next, PaneId(1)));
        });
        cx.simulate_event(MouseDownEvent {
            position: bounds.center(),
            button: MouseButton::Left,
            click_count: 2,
            modifiers: Modifiers::default(),
            first_mouse: false,
        });
        view.update(cx, |v, _| {
            assert_eq!(v.model.tabs.len(), 1);
            assert_eq!(
                v.model.active_tab().unwrap().remote.as_ref().unwrap().kind,
                SessionKind::Shell
            );
        });
    }

    #[gpui::test]
    fn session_context_menu_is_targeted_and_dismisses_without_connecting(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| workspace(cx, root.path().to_path_buf()));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let bounds = cx.debug_bounds("ssh-saved-0").unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(
                v.ssh_context_menu.as_ref().unwrap().profile.name,
                "test session"
            );
            assert!(v.model.tabs.is_empty());
        });
        cx.simulate_keystrokes("escape");
        view.update(cx, |v, _| assert!(v.ssh_context_menu.is_none()));
    }

    /// 磁盘-backed 夹具：写入 profiles 文件并让视图从磁盘加载。
    fn disk_workspace(
        cx: &mut Context<WorkspaceView>,
        root: PathBuf,
        profiles: termior_ssh::Profiles,
    ) -> WorkspaceView {
        profiles.save(&root).unwrap();
        let mut view = workspace(cx, root.clone());
        view.data_dir = Some(root);
        view.reload_ssh_profiles();
        view
    }

    #[gpui::test]
    fn grouped_sessions_render_under_headers_and_keep_empty_groups(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (_view, cx) = cx.add_window_view(|_, cx| {
            let mut view = workspace(cx, root.path().to_path_buf());
            view.ssh_profiles.groups = vec!["空分组".into(), "Web".into()];
            view.ssh_profiles.connections.push(Profile {
                name: "web server".into(),
                host: "10.0.0.1".into(),
                group: "Web".into(),
                ..Profile::default()
            });
            view
        });
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let ungrouped = cx.debug_bounds("ssh-saved-0").unwrap();
        let empty = cx.debug_bounds("ssh-group-0").expect("空分组也要渲染表头");
        let web = cx.debug_bounds("ssh-group-1").unwrap();
        let member = cx.debug_bounds("ssh-saved-1").unwrap();
        assert!(ungrouped.origin.y < empty.origin.y, "未分组在最前");
        assert!(empty.origin.y < web.origin.y, "分组按存储顺序排列");
        assert!(web.origin.y < member.origin.y, "成员在分组表头之后");
    }

    #[gpui::test]
    fn group_header_click_toggles_collapse(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut view = workspace(cx, root.path().to_path_buf());
            view.ssh_profiles.groups = vec!["Web".into()];
            view.ssh_profiles.connections.push(Profile {
                name: "web server".into(),
                host: "10.0.0.1".into(),
                group: "Web".into(),
                ..Profile::default()
            });
            view
        });
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("ssh-saved-1").is_some());
        let header = cx.debug_bounds("ssh-group-0").unwrap().center();
        cx.simulate_mouse_down(header, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        view.update(cx, |v, _| {
            assert!(v.ssh_collapsed_groups.contains("Web"));
        });
        assert!(
            cx.debug_bounds("ssh-saved-1").is_none(),
            "折叠后成员行不渲染"
        );
        assert!(cx.debug_bounds("ssh-group-0").is_some(), "折叠后表头保留");
        cx.simulate_mouse_down(header, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("ssh-saved-1").is_some(), "再次点击展开");
    }

    #[gpui::test]
    fn move_to_group_menu_persists_the_assignment(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let profiles = termior_ssh::Profiles {
            groups: vec!["Web".into()],
            connections: vec![Profile {
                name: "test session".into(),
                host: "127.0.0.1".into(),
                port: Some(9),
                ..Profile::default()
            }],
            ..termior_ssh::Profiles::default()
        };
        let (view, cx) =
            cx.add_window_view(|_, cx| disk_workspace(cx, root.path().to_path_buf(), profiles));
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let row = cx.debug_bounds("ssh-saved-0").unwrap().center();
        cx.simulate_mouse_down(row, MouseButton::Right, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(
                v.ssh_context_menu.as_ref().unwrap().phase,
                SessionMenuPhase::Session
            );
        });
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let move_item = cx
            .debug_bounds("ssh-menu-item-4")
            .expect("菜单包含「移动到分组…」")
            .center();
        cx.simulate_mouse_down(move_item, MouseButton::Left, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(
                v.ssh_context_menu.as_ref().unwrap().phase,
                SessionMenuPhase::GroupPicker
            );
        });
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        // 目标 0 是「未分组」，1 是 Web。
        let web = cx.debug_bounds("ssh-group-target-1").unwrap().center();
        cx.simulate_mouse_down(web, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let saved = termior_ssh::Profiles::load(root.path()).unwrap();
        assert_eq!(saved.connections[0].group, "Web");
        view.update(cx, |v, _| {
            assert_eq!(v.ssh_profiles.connections[0].group, "Web");
            assert!(v.ssh_context_menu.is_none());
        });
    }

    #[gpui::test]
    fn group_menu_renames_and_deletes_groups(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let profiles = termior_ssh::Profiles {
            groups: vec!["Web".into()],
            connections: vec![Profile {
                name: "test session".into(),
                host: "127.0.0.1".into(),
                group: "Web".into(),
                ..Profile::default()
            }],
            ..termior_ssh::Profiles::default()
        };
        let (view, cx) =
            cx.add_window_view(|_, cx| disk_workspace(cx, root.path().to_path_buf(), profiles));
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let header = cx.debug_bounds("ssh-group-0").unwrap().center();
        cx.simulate_mouse_down(header, MouseButton::Right, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(v.ssh_group_menu.as_ref().unwrap().name, "Web");
        });
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let rename = cx.debug_bounds("ssh-group-action-0").unwrap().center();
        cx.simulate_mouse_down(rename, MouseButton::Left, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(v.command_mode, CommandMode::RenameGroup);
            assert_eq!(v.pending_group_rename.as_deref(), Some("Web"));
            assert_eq!(v.command_input, "Web");
            v.command_input = "Prod".into();
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let saved = termior_ssh::Profiles::load(root.path()).unwrap();
        assert_eq!(saved.groups, vec!["Prod".to_owned()]);
        assert_eq!(saved.connections[0].group, "Prod");
        view.update(cx, |v, _| {
            assert_eq!(v.command_mode, CommandMode::Browse);
        });
        // 删除分组：成员归入未分组，分组本身消失。
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let header = cx.debug_bounds("ssh-group-0").unwrap().center();
        cx.simulate_mouse_down(header, MouseButton::Right, Modifiers::default());
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let delete = cx.debug_bounds("ssh-group-action-1").unwrap().center();
        cx.simulate_mouse_down(delete, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let (_, detail) = cx.pending_prompt().expect("删除分组需要确认");
        assert!(detail.contains("1 connection"), "{detail}");
        cx.simulate_prompt_answer("Delete");
        cx.run_until_parked();
        let saved = termior_ssh::Profiles::load(root.path()).unwrap();
        assert!(saved.groups.is_empty());
        assert_eq!(saved.connections[0].group, "");
        view.update(cx, |v, _| {
            assert!(v.ssh_profiles.groups.is_empty());
            assert!(v.ssh_collapsed_groups.is_empty(), "reload 清理失效折叠名");
        });
    }

    #[gpui::test]
    fn new_group_button_creates_empty_group_via_command_bar(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let profiles = termior_ssh::Profiles {
            connections: vec![Profile {
                name: "test session".into(),
                host: "127.0.0.1".into(),
                ..Profile::default()
            }],
            ..termior_ssh::Profiles::default()
        };
        let (view, cx) =
            cx.add_window_view(|_, cx| disk_workspace(cx, root.path().to_path_buf(), profiles));
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let button = cx.debug_bounds("ssh-new-group").unwrap().center();
        cx.simulate_mouse_down(button, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(button, MouseButton::Left, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(v.command_mode, CommandMode::NewGroup);
            assert_eq!(v.pending_group_profile, None);
            v.command_input = "Prod".into();
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let saved = termior_ssh::Profiles::load(root.path()).unwrap();
        assert_eq!(saved.groups, vec!["Prod".to_owned()]);
        assert_eq!(saved.connections.len(), 1);
        assert_eq!(saved.connections[0].group, "", "新建空分组不动既有连接");
        view.update(cx, |v, _| {
            assert_eq!(v.ssh_profiles.groups, vec!["Prod".to_owned()]);
            assert_eq!(v.command_mode, CommandMode::Browse);
        });
    }

    #[gpui::test]
    fn browser_is_two_columns_without_a_terminal_and_close_releases_state(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("local.txt"), "data").unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut v = workspace(cx, root.path().to_path_buf());
            install_browser(&mut v, root.path());
            v
        });
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let local = cx.debug_bounds("sftp-local-files").unwrap();
        let remote = cx.debug_bounds("sftp-remote-pane").unwrap();
        assert!(local.size.height > px(100.));
        assert!(remote.size.height > px(100.));
        assert!(local.right() <= remote.left());
        assert!(remote.right() <= px(1200.));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let id = v.model.active.unwrap();
                assert!(v.active_terminal().is_none());
                assert!(v.remote_runtime_connected(id, cx));
                assert!(!v.can_split_active(SplitDirection::Right, window));
                v.close_tab(id, window, cx);
                assert!(!v.remote_explorers.contains_key(&id));
                assert!(!v.sftp_browsers.contains_key(&id));
                assert!(v.remote_auth_sessions.is_empty());
            })
        });
    }

    #[gpui::test]
    fn drag_download_confirmation_keeps_destination_and_cannot_revive_closed_tab(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut v = workspace(cx, root.path().to_path_buf());
            install_browser(&mut v, root.path());
            v
        });
        cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let id = v.model.active.unwrap();
                v.download_browser_entry(
                    &RemoteFileDrag {
                        tab_id: id,
                        path: "/folder".into(),
                        name: "folder".into(),
                        is_dir: true,
                    },
                    root.path().to_path_buf(),
                    window,
                    cx,
                );
            })
        });
        let (_, detail) = cx
            .pending_prompt()
            .expect("drag must require transfer confirmation");
        assert!(detail.contains("/folder"));
        assert!(detail.contains(root.path().to_str().unwrap()));
        assert!(detail.contains("recursively"));
        cx.update(|window, cx| {
            view.update(cx, |v, cx| v.close_tab(v.model.active.unwrap(), window, cx))
        });
        cx.simulate_prompt_answer("Start transfer");
        cx.run_until_parked();
        view.update(cx, |v, _| {
            assert!(
                v.model.tabs.is_empty(),
                "confirmation must not revive a closed source"
            )
        });
    }
    #[gpui::test]
    fn cross_pane_drag_routes_upload_and_download_to_hovered_directory(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("destination")).unwrap();
        std::fs::write(root.path().join("local.txt"), "data").unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut v = workspace(cx, root.path().to_path_buf());
            install_browser(&mut v, root.path());
            v
        });
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let local_file = cx.debug_bounds("sftp-local-file-1").unwrap().center();
        let remote_folder = cx.debug_bounds("remote-file-/folder").unwrap().center();
        cx.simulate_mouse_down(local_file, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            local_file + gpui::point(px(15.), px(0.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_move(remote_folder, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(remote_folder, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let (_, detail) = cx
            .pending_prompt()
            .expect("dropping a local file starts upload confirmation");
        assert!(detail.contains("Upload"));
        assert!(detail.contains("local.txt"));
        assert!(
            detail.contains(":/folder"),
            "hovered directory, not its parent: {detail}"
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear();
        });
        let remote_file = cx.debug_bounds("remote-file-/file.txt").unwrap().center();
        let local_folder = cx.debug_bounds("sftp-local-file-0").unwrap().center();
        cx.simulate_mouse_down(remote_file, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            remote_file + gpui::point(px(15.), px(0.)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_move(local_folder, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(local_folder, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let (_, detail) = cx
            .pending_prompt()
            .expect("dropping a remote file starts download confirmation");
        assert!(detail.contains("Download"));
        assert!(detail.contains("/file.txt"));
        assert!(
            detail.contains("destination"),
            "hovered local directory: {detail}"
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        view.update(cx, |v, _| {
            assert_eq!(
                v.model.tabs.len(),
                1,
                "cancelled drags must not create jobs"
            )
        });
    }
    #[gpui::test]
    fn accepted_transfer_keeps_browser_visible_and_tracks_background_job(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut v = workspace(cx, root.path().to_path_buf());
            install_browser(&mut v, root.path());
            v
        });
        let source = cx.update(|window, cx| {
            view.update(cx, |v, cx| {
                let source = v.model.active.unwrap();
                let mut model = v.model.clone();
                let job = model.new_tab(TabKind::Terminal, "", false);
                v.pending_terminals.insert((job, PaneId(1)));
                v.remote_explorers.get_mut(&source).unwrap().request =
                    Some(RemoteExplorerRequest {
                        id: 1,
                        path: Some("/".into()),
                        control: termior_ssh::sftp::RequestControl::default(),
                    });
                v.download_browser_entry(
                    &RemoteFileDrag {
                        tab_id: source,
                        path: "/file.txt".into(),
                        is_dir: false,
                        name: "file.txt".into(),
                    },
                    root.path().to_path_buf(),
                    window,
                    cx,
                );
                source
            })
        });
        cx.simulate_prompt_answer("Start transfer");
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            assert_eq!(v.model.active, Some(source));
            assert_eq!(v.model.tabs.len(), 2);
            let jobs = &v.sftp_browsers[&source].transfers;
            assert_eq!(jobs.len(), 1);
            let transfer = v
                .model
                .tab(jobs[0])
                .unwrap()
                .remote
                .as_ref()
                .unwrap()
                .transfer
                .as_ref()
                .unwrap();
            assert_eq!(transfer.remote_path, "/file.txt");
            assert!(!transfer.upload);
            assert!(v.active_terminal().is_none());
            assert!(v.remote_runtime_connected(source, cx));
        });
    }
}
