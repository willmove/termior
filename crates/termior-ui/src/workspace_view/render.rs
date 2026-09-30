//! 工作区视图的 GPUI 渲染、侧栏内容与背景图层。

use super::commands::CommandMode;
use super::explorer_actions::ExplorerContextTarget;
use super::helpers::{
    change_group_label, explorer_content_width, explorer_context_action_label,
    explorer_context_actions, explorer_entry_visible, explorer_icon, explorer_tool_button,
    gpui_color, gpui_color_alpha, new_tab_menu_item, paired_tab_layout, resize_edge,
    rounded_client_corners, shell_menu_item, sidebar_button, split_menu_item, status_color,
    status_label, terminal_menu_item, TerminalMenuAction,
};
use super::*;

impl WorkspaceView {
    fn layout_element(
        &self,
        node: &LayoutNode,
        panes: &HashMap<PaneId, PaneContent>,
        focused: PaneId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.layout_element_at(node, panes, focused, Vec::new(), cx)
    }
    fn layout_element_at(
        &self,
        node: &LayoutNode,
        panes: &HashMap<PaneId, PaneContent>,
        focused: PaneId,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let multi_pane = panes.len() > 1;
        match node {
            LayoutNode::Pane { id } => {
                let pane_id = *id;
                let inactive = multi_pane && *id != focused;
                let content = panes
                    .get(id)
                    .map(|pane| pane.element(&self.palette))
                    .unwrap_or_else(|| {
                        empty_state_message(
                            Icon::Terminal,
                            t!("empty.pane_unavailable"),
                            None,
                            &self.palette,
                        )
                        .into_any_element()
                    });
                div()
                    .id(SharedString::from(format!("pane-{}", id.0)))
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    // 单 pane 不画焦点框；多 pane 时压暗非活动侧，活动侧不加装饰。
                    .when(inactive, |pane| pane.opacity(INACTIVE_PANE_OPACITY))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, window, cx| {
                            if let Some(tab) = workspace.model.active_tab_mut() {
                                let _ = tab.layout.focus(pane_id);
                            }
                            workspace.focus_active_pane(window, cx);
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |workspace, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            if let Some(tab) = workspace.model.active_tab_mut() {
                                let _ = tab.layout.focus(pane_id);
                            }
                            workspace.focus_active_pane(window, cx);
                            workspace.explorer_context_menu = None;
                            workspace.new_tab_menu = None;
                            let terminal = workspace
                                .tabs
                                .iter()
                                .find(|tab| Some(tab.id) == workspace.model.active)
                                .and_then(|tab| tab.panes.get(&pane_id))
                                .and_then(|pane| match pane {
                                    PaneContent::Terminal(terminal) => Some(terminal.clone()),
                                    _ => None,
                                });
                            workspace.pane_context_menu = Some(PaneContextMenu {
                                terminal,
                                position: event.position,
                            });
                            cx.notify();
                        }),
                    )
                    .child(content)
                    .into_any_element()
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let mut first_path = path.clone();
                first_path.push(0);
                let mut second_path = path.clone();
                second_path.push(1);
                let first = self.layout_element_at(first, panes, focused, first_path, cx);
                let second = self.layout_element_at(second, panes, focused, second_path, cx);
                let ratio = ratio.clamp(0.1, 0.9);
                let resize_path = path.clone();
                let resize_direction = *direction;
                let resize_handle = div()
                    .id(SharedString::from(format!(
                        "pane-divider-{}",
                        path.iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join("-")
                    )))
                    .flex_shrink_0()
                    .bg(ui::border(&self.palette))
                    .hover({
                        let accent = gpui_color(self.palette.accent);
                        move |style| style.bg(accent)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, event, window, cx| {
                            workspace.start_pane_resize(
                                resize_path.clone(),
                                resize_direction,
                                ratio,
                                event,
                                window,
                                cx,
                            )
                        }),
                    );
                match direction {
                    SplitDirection::Right => div()
                        .flex()
                        .flex_row()
                        .size_full()
                        .child(
                            div()
                                .h_full()
                                .w(relative(ratio))
                                .flex_shrink_0()
                                .child(first),
                        )
                        .child(
                            resize_handle
                                .w(px(5.0))
                                .h_full()
                                .cursor(CursorStyle::ResizeColumn),
                        )
                        .child(div().h_full().flex_1().child(second))
                        .into_any_element(),
                    SplitDirection::Down => div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(
                            div()
                                .w_full()
                                .h(relative(ratio))
                                .flex_shrink_0()
                                .child(first),
                        )
                        .child(
                            resize_handle
                                .w_full()
                                .h(px(5.0))
                                .cursor(CursorStyle::ResizeRow),
                        )
                        .child(div().w_full().flex_1().child(second))
                        .into_any_element(),
                }
            }
        }
    }
    fn sidebar_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let command_bar = (self.command_mode != CommandMode::Browse).then(|| {
            div()
                .px_2()
                .py_1()
                .mb_1()
                .rounded_md()
                .border_1()
                .border_color(gpui_color(self.palette.accent))
                .bg(gpui_color(self.palette.elevated))
                .text_xs()
                .child(SharedString::from(format!(
                    "{}: {}{}|",
                    self.command_mode.label(),
                    self.command_input,
                    if self.command_marked_text.is_empty() {
                        ""
                    } else {
                        "…"
                    }
                )))
        });
        let message = self.command_message.clone().map(|message| {
            div()
                .px_2()
                .py_1()
                .text_xs()
                .child(SharedString::from(message))
        });
        let content = match self.model.sidebar_panel {
            SidebarPanel::Ssh => self.ssh_session_list(cx),

            SidebarPanel::Explorer => {
                if self.active_is_sftp_browser() {
                    div()
                        .p_2()
                        .text_xs()
                        .child(t!("ws.sftp_browser_hint"))
                        .into_any_element()
                } else if let Some(remote) = self.remote_explorer_content(cx) {
                    remote
                } else {
                    let toolbar = div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .mb_1()
                        .child(explorer_tool_button(
                            "explorer-find",
                            Icon::Search,
                            t!("explorer.find"),
                            &self.palette,
                            cx,
                            |this, cx| this.begin_command(CommandMode::FindFile, cx),
                        ))
                        .child(explorer_tool_button(
                            "explorer-search",
                            Icon::FileText,
                            t!("explorer.search"),
                            &self.palette,
                            cx,
                            |this, cx| this.begin_command(CommandMode::SearchContent, cx),
                        ))
                        .child(explorer_tool_button(
                            "explorer-new-file",
                            Icon::File,
                            t!("explorer.new_file"),
                            &self.palette,
                            cx,
                            |this, cx| this.begin_command(CommandMode::CreateFile, cx),
                        ))
                        .child(explorer_tool_button(
                            "explorer-new-dir",
                            Icon::Folder,
                            t!("explorer.new_directory"),
                            &self.palette,
                            cx,
                            |this, cx| this.begin_command(CommandMode::CreateDirectory, cx),
                        ))
                        .child(explorer_tool_button(
                            "explorer-refresh",
                            Icon::Refresh,
                            t!("explorer.refresh"),
                            &self.palette,
                            cx,
                            |this, cx| this.refresh_workspace_data(cx),
                        ));

                    let root_label = self
                        .explorer_requested_root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.explorer_requested_root.display().to_string());
                    let active_path = self
                        .model
                        .active_tab()
                        .and_then(|tab| tab.resource.as_deref())
                        .map(PathBuf::from);
                    let (visible_entries, visible_entry_count) = self
                        .explorer
                        .as_ref()
                        .map(|index| {
                            let mut entries = index
                                .entries()
                                .iter()
                                .filter(|entry| explorer_entry_visible(entry, &self.explorer_tree))
                                .collect::<Vec<_>>();
                            entries.sort_by(|a, b| a.relative.cmp(&b.relative));
                            let count = entries.len();
                            let visible =
                                entries.into_iter().take(500).cloned().collect::<Vec<_>>();
                            (visible, count)
                        })
                        .unwrap_or_else(|| (Vec::new(), 0));
                    let tree_content_width = explorer_content_width(&visible_entries);

                    let rows = if self.command_mode == CommandMode::FindFile {
                        self.explorer
                            .as_ref()
                            .map(|index| {
                                index
                                    .fuzzy(&self.command_input, 80)
                                    .into_iter()
                                    .map(|hit| {
                                        let path = index.root().join(&hit.path);
                                        div()
                                            .id(SharedString::from(format!("find-{}", hit.path)))
                                            .px_2()
                                            .py_1()
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(SharedString::from(hit.path))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, window, cx| {
                                                    this.open_editor(path.clone(), window, cx)
                                                }),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    } else if self.command_mode == CommandMode::SearchContent {
                        self.content_matches
                            .iter()
                            .map(|hit| {
                                let path = hit.path.clone();
                                div()
                                    .id(SharedString::from(format!(
                                        "search-{}-{}",
                                        path.display(),
                                        hit.line_number
                                    )))
                                    .px_2()
                                    .py_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(SharedString::from(format!(
                                        "{}:{}  {}",
                                        path.strip_prefix(&self.explorer_requested_root)
                                            .unwrap_or(&path)
                                            .display(),
                                        hit.line_number,
                                        hit.line
                                    )))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, window, cx| {
                                            this.open_editor(path.clone(), window, cx)
                                        }),
                                    )
                            })
                            .collect()
                    } else {
                        visible_entries
                            .into_iter()
                            .map(|entry| {
                                let path = entry.path.clone();
                                let right_path = path.clone();
                                let selected =
                                    self.explorer_tree.selected() == Some(path.as_path());
                                let active = active_path.as_ref() == Some(&path);
                                let expanded = self.explorer_tree.is_expanded(&path);
                                let is_dir = entry.is_dir;
                                let icon = entry.icon;
                                let label = entry
                                    .path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or(&entry.relative)
                                    .to_owned();
                                div()
                                    .id(SharedString::from(format!("file-{}", entry.relative)))
                                    .ml(px(entry.depth as f32 * 12.0))
                                    .w_full()
                                    .min_w(px(tree_content_width))
                                    .px_1()
                                    .py(px(2.0))
                                    .rounded_sm()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(selected, |row| row.bg(ui::selected_wash(&self.palette)))
                                    .when(active, |row| {
                                        row.bg(ui::alpha(self.palette.accent, 0.35))
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1()
                                            .whitespace_nowrap()
                                            .child(explorer_icon(icon, expanded, &self.palette))
                                            .child(SharedString::from(label)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _event, window, cx| {
                                            this.explorer_context_menu = None;
                                            this.explorer_tree.select(path.clone());
                                            if is_dir {
                                                this.explorer_tree.toggle_expanded(path.clone());
                                                window.focus(&this.focus_handle, cx);
                                            } else {
                                                this.open_editor(path.clone(), window, cx);
                                            }
                                            cx.notify();
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                cx.stop_propagation();
                                                this.show_explorer_context_menu(
                                                    if is_dir {
                                                        ExplorerContextTarget::Directory(
                                                            right_path.clone(),
                                                        )
                                                    } else {
                                                        ExplorerContextTarget::File(
                                                            right_path.clone(),
                                                        )
                                                    },
                                                    event.position,
                                                    window,
                                                    cx,
                                                );
                                            },
                                        ),
                                    )
                            })
                            .collect::<Vec<_>>()
                    };
                    let background_target = ExplorerContextTarget::Workspace;
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .child(toolbar)
                        .child(
                            div()
                                .px_1()
                                .pb_1()
                                .text_xs()
                                .text_color(ui::muted(&self.palette))
                                .child(SharedString::from(if self.explorer_deep_indexing {
                                    format!("{root_label}{}", t!("ws.explorer_footer_indexing"))
                                } else if self.explorer_index_incomplete {
                                    format!("{root_label}{}", t!("ws.explorer_footer_incomplete"))
                                } else if visible_entry_count > 500 {
                                    format!("{root_label}{}", t!("ws.explorer_footer_truncated"))
                                } else {
                                    root_label.clone()
                                })),
                        )
                        .children(self.explorer_error.clone().map(|error| {
                            div()
                                .px_1()
                                .py_1()
                                .text_xs()
                                .text_color(gpui_color(self.palette.status[3]))
                                .child(SharedString::from(error))
                        }))
                        .children(
                            (self.explorer_index_incomplete && self.explorer_error.is_none()).then(
                                || {
                                    div()
                                        .px_1()
                                        .py_1()
                                        .text_xs()
                                        .text_color(ui::muted(&self.palette))
                                        .child(t!("explorer.index_incomplete"))
                                },
                            ),
                        )
                        .child(
                            div()
                                .id("explorer-scroll")
                                .flex_1()
                                .min_h(px(0.0))
                                .overflow_x_scroll()
                                .overflow_y_scroll()
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        this.show_explorer_context_menu(
                                            background_target.clone(),
                                            event.position,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                                .children(rows)
                                .children(
                                    (self.explorer.is_some()
                                        && !self.explorer_deep_indexing
                                        && self
                                            .explorer
                                            .as_ref()
                                            .is_some_and(|index| index.entries().is_empty()))
                                    .then(|| {
                                        empty_hint(t!("empty.no_visible_files"), &self.palette)
                                    }),
                                ),
                        )
                        .children(self.explorer_skipped_footer(cx))
                        .into_any_element()
                }
            }
            SidebarPanel::SourceControl => {
                let branch = self
                    .vcs_branch
                    .as_ref()
                    .and_then(|state| state.name.clone())
                    .unwrap_or_else(|| t!("ws.no_repository").to_string());
                let toolbar = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .mb_1()
                    .child(sidebar_button(
                        t!("git.stage_all"),
                        "git-stage-all",
                        &self.palette,
                        cx,
                        |this, cx| this.stage_all(cx),
                    ))
                    .child(sidebar_button(
                        t!("git.commit"),
                        "git-commit",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitCommit, cx),
                    ))
                    .child(sidebar_button(
                        t!("git.fetch"),
                        "git-fetch",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::Fetch, cx),
                    ))
                    .child(sidebar_button(
                        t!("git.pull"),
                        "git-pull",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::PullFfOnly, cx),
                    ))
                    .child(sidebar_button(
                        t!("git.push"),
                        "git-push",
                        &self.palette,
                        cx,
                        |this, cx| this.run_remote(RemoteOperation::Push, cx),
                    ))
                    .child(sidebar_button(
                        t!("git.new_branch"),
                        "git-new-branch",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitCreateBranch, cx),
                    ))
                    .child(sidebar_button(
                        t!("git.switch_branch"),
                        "git-switch-branch",
                        &self.palette,
                        cx,
                        |this, cx| this.begin_command(CommandMode::GitSwitchBranch, cx),
                    ));
                let rows = self
                    .vcs_status
                    .iter()
                    .take(80)
                    .map(|file| {
                        let path = file.path.clone();
                        let group = file.group;
                        div()
                            .id(SharedString::from(format!("git-{group:?}-{path}")))
                            .px_2()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .child(SharedString::from(format!(
                                "{}  {}",
                                change_group_label(file.group),
                                file.path
                            )))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.open_git_diff(path.clone(), group, cx)
                                }),
                            )
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .child(tf!("ws.branch_label", "branch" => branch))
                    .child(toolbar)
                    .children(rows)
                    .into_any_element()
            }
            SidebarPanel::GitHistory => {
                let rows = self
                    .vcs_history
                    .iter()
                    .take(60)
                    .map(|commit| {
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .child(SharedString::from(format!(
                                "{}  {}",
                                &commit.id[..7.min(commit.id.len())],
                                commit.summary
                            )))
                    })
                    .collect::<Vec<_>>();
                let empty = rows.is_empty().then(|| {
                    empty_hint(t!("empty.no_git_history"), &self.palette).into_any_element()
                });
                div()
                    .flex()
                    .flex_col()
                    .child(sidebar_button(
                        t!("git.open_full_history"),
                        "git-open-history",
                        &self.palette,
                        cx,
                        |this, cx| this.open_git_history(cx),
                    ))
                    .children(rows)
                    .children(empty)
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .children(command_bar)
            .children(message)
            .child(content)
            .into_any_element()
    }
    fn sync_terminal_context(&mut self, cx: &mut Context<Self>) -> (String, Option<String>) {
        if let Some((tab_id, remote)) = self
            .model
            .active_tab()
            .and_then(|tab| tab.remote.as_ref().map(|remote| (tab.id, remote.clone())))
        {
            // A remote OSC 7 is session-scoped metadata only: it follows this
            // tab's SFTP Explorer and never mutates local cwd/project_dir.
            let reported_cwd = self
                .active_terminal()
                .and_then(|terminal| terminal.read(cx).latest_cwd().map(str::to_owned));
            if let Some(path) = reported_cwd.filter(|path| path.starts_with('/')) {
                let changed = self.remote_terminal_cwds.get(&tab_id) != Some(&path);
                if changed && termior_ssh::quote_sftp_path(&path).is_ok() {
                    self.remote_terminal_cwds.insert(tab_id, path.clone());
                    self.remote_explorer_paths.insert(tab_id, path.clone());
                    self.schedule_remote_explorer_scan(tab_id, remote.profile.clone(), path, cx);
                }
            }
            let label = tf!(
                "ws.remote_context",
                "kind" => format!("{:?}", remote.kind),
                "user" => remote.profile.user.clone(),
                "host" => remote.profile.host.clone()
            )
            .to_string();
            self.composer.update(cx, |composer, _| {
                composer.update_terminal_context(String::new(), String::new(), None)
            });
            return (label, None);
        }
        let terminal = self.active_terminal().cloned();
        let cwd = terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).latest_cwd().map(str::to_owned));
        let preview = terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).localhost_urls().last().cloned());
        let recent_output = terminal
            .as_ref()
            .map(|terminal| terminal.read(cx).recent_text())
            .unwrap_or_default();
        let command_reference = terminal
            .as_ref()
            .and_then(|terminal| terminal.read(cx).recent_commands().last().cloned());
        if let Some(cwd) = &cwd {
            // 只同步 shell 当前目录；Explorer/Git 跟随的是 project_dir，
            // shell 的 cd 漂移不得拖动项目锚点。
            self.model.set_active_cwd(cwd);
        }
        let resolved_cwd = cwd.unwrap_or_else(|| {
            self.model
                .active_project_dir()
                .to_string_lossy()
                .into_owned()
        });
        self.composer.update(cx, |composer, _| {
            composer.update_terminal_context(resolved_cwd.clone(), recent_output, command_reference)
        });
        (resolved_cwd, preview)
    }
}

impl WorkspaceView {
    /// 计算当前背景图层（FR-THEME-05）。渲染帧同步调用，不阻塞。
    ///
    /// - 无有效路径 → [`BackgroundLayer::None`]（优雅回退到 `palette.background` 纯色）。
    /// - `blur == 0` → [`BackgroundLayer::Path`]：直接交给 `gpui::img(path)`，走 gpui 自带资源缓存。
    /// - `blur > 0` → [`BackgroundLayer::Blurred`]：用 [`BackgroundImageCache`] 的离屏模糊纹理；
    ///   若缓存未就绪则返回 `Blurred(None)`（首帧先留白，后台解码完成后 `cx.notify` 触发重绘）。
    ///
    /// 调用方应在渲染前先调 [`Self::request_background_decode`] 触发后台解码（键变化才真正跑）。
    fn background_layer(&self) -> BackgroundLayer {
        let Some(path) = self
            .settings
            .background
            .image_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
        else {
            return BackgroundLayer::None;
        };
        let opacity = self.settings.background.opacity.clamp(0.0, 1.0);
        let blur = self.settings.background.blur.clamp(0.0, 64.0);
        if blur <= 0.0 {
            BackgroundLayer::Path { path, opacity }
        } else {
            BackgroundLayer::Blurred {
                opacity,
                texture: self.background_cache.current(),
            }
        }
    }

    /// 若背景键（路径/模糊半径）变化，启动后台解码 + 模糊，完成后回填缓存并重绘。
    /// 键不变则空操作（满足 spec「解码一次缓存」「不做逐帧后处理」）。
    fn request_background_decode(&mut self, cx: &mut Context<Self>) {
        let path = match self
            .settings
            .background
            .image_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
        {
            Some(path) => path,
            None => {
                // 无背景图：清空缓存键，下次配置了图才会解码。
                if self.background_cache.current_key().is_some() {
                    self.background_cache.begin(PathBuf::new(), 0.0);
                }
                return;
            }
        };
        let blur = self.settings.background.blur.clamp(0.0, 64.0);
        if !self.background_cache.needs(&path, blur) {
            return;
        }
        self.background_cache.begin(path.clone(), blur);
        let path_for_task = path.clone();
        // 后台线程解码 + 模糊（CPU 密集，不阻塞 UI 线程）；完成后回填缓存并重绘。
        // oneshot channel 适合「线程 → async 单次回传」；线程侧 `send` 不阻塞。
        cx.spawn(async move |this, cx| {
            let (tx, rx) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let result = crate::background_image::decode_and_blur(&path_for_task, blur);
                let _ = tx.send(result);
            });
            let result = rx.await.ok().flatten();
            this.update(cx, |this, cx| {
                let blur = this.settings.background.blur.clamp(0.0, 64.0);
                if this.background_cache.store(&path, blur, result) {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

/// 背景图层计算结果（见 [`WorkspaceView::background_layer`]）。
enum BackgroundLayer {
    /// 无背景图：渲染层只保留纯色背景。
    None,
    /// 未模糊：直接用文件路径，由 gpui 资源缓存异步加载（现有路径）。
    Path { path: PathBuf, opacity: f32 },
    /// 已模糊：用离屏缓存的 [`gpui::RenderImage`] 纹理；`texture` 为 `None` 表示尚未解码完成。
    Blurred {
        opacity: f32,
        texture: Option<std::sync::Arc<gpui::RenderImage>>,
    },
}

/// 把 [`BackgroundLayer`] 转成可绘制的背景元素（铺满、Cover、按透明度叠加）。
/// `None` / 未就绪的 `Blurred(None)` 返回 `None`，由渲染层保留纯色背景（优雅回退）。
fn background_layer_element(layer: BackgroundLayer) -> Option<gpui::AnyElement> {
    match layer {
        BackgroundLayer::None => None,
        BackgroundLayer::Path { path, opacity } => Some(
            gpui::img(path)
                .absolute()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(opacity)
                .into_any_element(),
        ),
        BackgroundLayer::Blurred { opacity, texture } => texture.map(|image| {
            gpui::img(image)
                .absolute()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(opacity)
                .into_any_element()
        }),
    }
}

impl gpui::Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if IDLE_REDRAW_ARMED.load(Ordering::Relaxed) {
            IDLE_REDRAW_FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        let system_is_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        if self.system_is_dark != system_is_dark {
            self.system_is_dark = system_is_dark;
            if self.settings.appearance == termior_store::settings::Appearance::FollowSystem {
                self.sync_theme_index();
                self.recompute_palette(cx);
            }
        }
        self.process_agent_updates(window, cx);
        for action in std::mem::take(&mut self.pending_composer_actions) {
            cx.defer_in(window, move |this, window, cx| {
                this.perform_key_action(action, window, cx)
            });
        }
        let (cwd, preview_url) = self.sync_terminal_context(cx);
        let p = self.palette.clone();
        let active = self.model.active;
        // 焦点 pane 是 Markdown 编辑器时才显示“预览”，与 request_markdown_preview 的判定一致
        // （分栏时标签资源与焦点 pane 可能不同，避免出现点了没反应的按钮）。
        let markdown_source_active = self
            .active_editor()
            .is_some_and(|editor| editor.read(cx).path().is_some_and(is_markdown_path));
        let markdown_preview_active = active
            .and_then(|id| self.model.tabs.iter().find(|tab| tab.id == id))
            .is_some_and(|tab| tab.kind == TabKind::Markdown);
        // 焦点 pane 是 HTML 文档时提供“在浏览器打开”入口（文件路径来自用户自己的工作区）。
        let html_file_path = self
            .active_editor()
            .and_then(|editor| editor.read(cx).path())
            .filter(|path| is_html_path(path))
            .map(|path| path.to_path_buf());
        let tab_buttons = self
            .model
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let id = tab.id;
                let selected = active == Some(id);
                let next_selected = self
                    .model
                    .tabs
                    .get(index + 1)
                    .is_some_and(|next| active == Some(next.id));
                let close_hover = ui::hover_wash(&p);
                div()
                    .id(SharedString::from(format!("tab-{}", id.0)))
                    // 测试里用 debug_bounds 定位 tab 头（release 构建下为 no-op）。
                    .debug_selector(move || format!("tab-header-{}", id.0))
                    .relative()
                    .h_full()
                    .pl_3()
                    .pr_2()
                    .min_w(px(110.0))
                    .max_w(px(210.0))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .rounded_tl(px(tokens::radius::MD))
                    .rounded_tr(px(tokens::radius::MD))
                    // 激活 tab 与下方内容区同色（连成一体），非激活 tab 沉入标题栏底色。
                    .bg(if selected {
                        gpui_color(p.background)
                    } else {
                        gpui_color(p.chrome)
                    })
                    .text_color(if selected {
                        gpui_color(p.foreground)
                    } else {
                        gpui_color_alpha(p.foreground, 0.55)
                    })
                    .when(!selected, |d| {
                        d.hover(move |style| style.bg(gpui_color(p.elevated)))
                    })
                    // 高亮线避开顶部圆角，底部保持直角以衔接内容区。
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .left(px(tokens::radius::MD))
                                .right(px(tokens::radius::MD))
                                .h(px(2.0))
                                .rounded(px(1.0))
                                .bg(gpui_color(p.accent)),
                        )
                    })
                    // 激活 tab 两侧不画分隔线，避免边缘出现一像素的凹口。
                    .when(!selected && !next_selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right_0()
                                .top(px(10.0))
                                .bottom(px(10.0))
                                .w(px(1.0))
                                .bg(gpui_color_alpha(p.foreground, 0.12)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .child(SharedString::from(tab.title.clone())),
                    )
                    .child({
                        let close_id = SharedString::from(format!("tab-close-{}", id.0));
                        div()
                            .id(close_id.clone())
                            .group(close_id.clone())
                            .aria_label(t!("chrome.close_tab"))
                            .flex_shrink_0()
                            .w(px(18.0))
                            .h(px(18.0))
                            .mr_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .hover(move |style| style.bg(close_hover))
                            .child(
                                ui::icon(
                                    Icon::Close,
                                    icon_size::XS,
                                    gpui_color_alpha(p.foreground, 0.5),
                                )
                                .group_hover(close_id, move |style| {
                                    style.text_color(gpui_color(p.foreground))
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _event, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(id, window, cx);
                                }),
                            )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, window, cx| {
                            this.activate_runtime(id, cx);
                            this.focus_active_pane(window, cx);
                            // 抑制 GPUI 对可聚焦祖先（workspace-root 的 track_focus）
                            // 的鼠标按下自动聚焦：否则它会在冒泡后期把刚交给新
                            // tab pane 的焦点抢回去，用户切完 tab 还得再点一下终端。
                            window.prevent_default();
                            cx.notify();
                        }),
                    )
                    // 中键关闭：桌面端标签页的通用约定。
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _event, window, cx| {
                            cx.stop_propagation();
                            this.close_tab(id, window, cx);
                        }),
                    )
            })
            .collect::<Vec<_>>();
        let new_tab_hover = ui::hover_wash(&p);
        let new_tab_button = div()
            .flex_shrink_0()
            .ml_1()
            .my_auto()
            .flex()
            .items_center()
            .rounded_md()
            .child(
                div()
                    .id("new-tab")
                    .group("new-tab")
                    .aria_label(t!("chrome.new_terminal_tab"))
                    .tooltip({
                        let palette = p.clone();
                        move |_window, cx| {
                            Tooltip::view(t!("chrome.new_terminal_tab"), &palette, cx)
                        }
                    })
                    .w(px(26.0))
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_l_md()
                    .cursor_pointer()
                    .hover(move |style| style.bg(new_tab_hover))
                    .child(
                        ui::icon(
                            Icon::Plus,
                            icon_size::SM,
                            gpui_color_alpha(p.foreground, 0.7),
                        )
                        .group_hover("new-tab", move |style| {
                            style.text_color(gpui_color(p.foreground))
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.request_new_terminal(Some(event.position), window, cx);
                        }),
                    ),
            )
            .child(
                div()
                    .id("new-tab-menu-toggle")
                    .aria_label(t!("ws.open_new_tab_menu"))
                    .aria_expanded(self.new_tab_menu.is_some())
                    .w(px(18.0))
                    .h(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_r_md()
                    .text_xs()
                    .cursor_pointer()
                    .hover(move |style| style.bg(new_tab_hover))
                    .child("▾")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                            this.explorer_context_menu = None;
                            this.pane_context_menu = None;
                            this.new_tab_menu = if this.new_tab_menu.is_some() {
                                None
                            } else {
                                Some(event.position)
                            };
                            cx.notify();
                        }),
                    ),
            );

        let toggle_sidebar_button = {
            let sidebar_visible = self.model.sidebar_visible;
            let button_id = "titlebar-toggle-sidebar";
            let tooltip_palette = p.clone();
            let hover_bg = ui::hover_wash(&p);
            let selected_bg = ui::selected_wash(&p);
            let muted = ui::muted(&p);
            let accent = gpui_color(p.accent);
            let foreground = gpui_color(p.foreground);
            div()
                .id(button_id)
                .group(button_id)
                .aria_label(t!("ws.toggle_sidebar"))
                .tooltip(move |_window, cx| {
                    Tooltip::view(t!("ws.toggle_sidebar_hint"), &tooltip_palette, cx)
                })
                .size(px(tokens::height::REGULAR))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(tokens::radius::MD))
                .cursor_pointer()
                .when(sidebar_visible, |this| this.bg(selected_bg))
                .when(!sidebar_visible, |this| {
                    this.hover(move |style| style.bg(hover_bg))
                })
                .child(
                    ui::icon(
                        Icon::PanelLeft,
                        icon_size::SM,
                        if sidebar_visible { accent } else { muted },
                    )
                    .group_hover(button_id, move |style| style.text_color(foreground)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _event, _window, cx| {
                        this.toggle_sidebar(cx);
                    }),
                )
        };

        // 源码/预览上下文按钮：按当前文档类型出现在标题栏右上方，而非藏在“+”下拉里。
        let preview_controls = {
            let mut buttons: Vec<AnyElement> = Vec::new();
            if markdown_source_active || markdown_preview_active {
                let (glyph, label) = if markdown_source_active {
                    (Icon::FileCode, t!("ws.preview"))
                } else {
                    (Icon::FileText, t!("ws.source"))
                };
                let hover_bg = ui::hover_wash(&p);
                let id = "preview-toggle-markdown";
                buttons.push(
                    div()
                        .id(id)
                        .group(id)
                        .aria_label(label.clone())
                        .h(px(tokens::height::REGULAR))
                        .px(px(tokens::space::SM))
                        .flex()
                        .items_center()
                        .gap(px(tokens::space::XS))
                        .rounded(px(tokens::radius::MD))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover_bg))
                        .child(ui::icon(
                            glyph,
                            icon_size::SM,
                            gpui_color_alpha(p.foreground, 0.72),
                        ))
                        .child(label.clone())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _event, window, cx| {
                                this.toggle_markdown_preview(window, cx);
                            }),
                        )
                        .into_any_element(),
                );
            }
            if let Some(url) = preview_url.clone() {
                let hover_bg = ui::hover_wash(&p);
                let id = "preview-toggle-web";
                buttons.push(
                    div()
                        .id(id)
                        .group(id)
                        .aria_label(t!("ws.web_preview"))
                        .h(px(tokens::height::REGULAR))
                        .px(px(tokens::space::SM))
                        .flex()
                        .items_center()
                        .gap(px(tokens::space::XS))
                        .rounded(px(tokens::radius::MD))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover_bg))
                        .child(ui::icon(
                            Icon::FileText,
                            icon_size::SM,
                            gpui_color_alpha(p.foreground, 0.72),
                        ))
                        .child(t!("ws.web_preview"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event, window, cx| {
                                this.create_preview_url(url.clone(), window, cx);
                            }),
                        )
                        .into_any_element(),
                );
            }
            if let Some(path) = html_file_path {
                let hover_bg = ui::hover_wash(&p);
                let id = "preview-toggle-browser";
                buttons.push(
                    div()
                        .id(id)
                        .group(id)
                        .aria_label(t!("action.open_in_browser"))
                        .h(px(tokens::height::REGULAR))
                        .px(px(tokens::space::SM))
                        .flex()
                        .items_center()
                        .gap(px(tokens::space::XS))
                        .rounded(px(tokens::radius::MD))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover_bg))
                        .child(ui::icon(
                            Icon::FileCode,
                            icon_size::SM,
                            gpui_color_alpha(p.foreground, 0.72),
                        ))
                        .child(t!("ws.open_in_browser"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_this, _event, _window, _cx| {
                                let _ = open_local_file(&path);
                            }),
                        )
                        .into_any_element(),
                );
            }
            if buttons.is_empty() {
                None
            } else {
                Some(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(buttons)
                        .into_any_element(),
                )
            }
        };

        let active_layout = {
            let runtime_ids: Vec<TabId> = self.tabs.iter().map(|tab| tab.id).collect();
            paired_tab_layout(&self.model, &runtime_ids).cloned()
        };
        let active_content = if self.active_is_sftp_browser() {
            self.sftp_browser_content(cx)
        } else {
            active_layout
                .and_then(|layout| {
                    let id = self.model.active?;
                    let tab = self.tabs.iter().find(|tab| tab.id == id)?;
                    Some(self.layout_element(&layout.root, &tab.panes, layout.focused, cx))
                })
                .unwrap_or_else(|| {
                    empty_state_message(
                        Icon::Terminal,
                        t!("empty.no_tabs"),
                        Some(t!("empty.no_tabs_detail")),
                        &p,
                    )
                    .into_any_element()
                })
        };
        let sidebar = if self.model.sidebar_visible {
            div()
                .flex()
                .flex_row()
                .w(px(self.model.sidebar_width))
                .h_full()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w(px(44.0))
                        .items_center()
                        .gap_2()
                        .pt_2()
                        .bg(gpui_color(p.panel))
                        .border_r_1()
                        .border_color(ui::border(&p))
                        .children(
                            [
                                (Icon::Terminal, SidebarPanel::Ssh, t!("activity.ssh")),
                                (Icon::Files, SidebarPanel::Explorer, t!("activity.explorer")),
                                (
                                    Icon::GitBranch,
                                    SidebarPanel::SourceControl,
                                    t!("activity.source_control"),
                                ),
                                (
                                    Icon::GitCommit,
                                    SidebarPanel::GitHistory,
                                    t!("activity.git_history"),
                                ),
                            ]
                            .into_iter()
                            .map(|(glyph, panel, label)| {
                                let selected = self.model.sidebar_panel == panel;
                                let tint = if selected {
                                    gpui_color(p.accent)
                                } else {
                                    ui::muted(&p)
                                };
                                div()
                                    .id(SharedString::from(format!("sidebar-{panel:?}")))
                                    .aria_label(label.clone())
                                    .tooltip({
                                        let palette = p.clone();
                                        let tooltip_label = label.clone();
                                        move |_window, cx| {
                                            Tooltip::view(tooltip_label.clone(), &palette, cx)
                                        }
                                    })
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .when(selected, |button| button.bg(ui::selected_wash(&p)))
                                    .when(!selected, |button| {
                                        let wash = ui::hover_wash(&p);
                                        button.hover(move |style| style.bg(wash))
                                    })
                                    .child(ui::icon(glyph, icon_size::MD, tint))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _event, _window, cx| {
                                            this.model.sidebar_panel = panel;
                                            this.model.sidebar_visible = true;
                                            cx.notify();
                                        }),
                                    )
                            }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .p_2()
                        .child(self.sidebar_content(cx)),
                )
                .child(
                    div()
                        .id("sidebar-resize-handle")
                        .w(px(5.0))
                        .h_full()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(ui::border(&p))
                        .cursor(CursorStyle::ResizeColumn)
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::start_sidebar_resize)),
                )
                .with_animation(
                    "sidebar-fade-in",
                    Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                    |style, delta| style.opacity(delta),
                )
                .into_any_element()
        } else {
            div().w(px(0.0)).into_any_element()
        };

        // 触发后台解码（键变化才真正跑）；再同步取当前图层（命中即用，否则优雅回退）。
        self.request_background_decode(cx);
        let background_layer = self.background_layer();
        let agent_indicators = self
            .notification_router
            .bell_items()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let bell_count = agent_indicators.len();
        let bell_needs_attention = agent_indicators.iter().any(|indicator| {
            matches!(
                indicator.status,
                AgentStatus::Attention | AgentStatus::Error
            )
        });
        let ai_status = agent_indicators
            .iter()
            .find(|indicator| {
                matches!(
                    indicator.status,
                    AgentStatus::Working
                        | AgentStatus::Started
                        | AgentStatus::Attention
                        | AgentStatus::Error
                )
            })
            .map(|indicator| indicator.status)
            .or_else(|| agent_indicators.first().map(|indicator| indicator.status))
            .unwrap_or(AgentStatus::Finished);
        let ai_tools_running = agent_indicators
            .iter()
            .filter(|indicator| {
                matches!(
                    indicator.status,
                    AgentStatus::Working | AgentStatus::Started
                )
            })
            .count();
        self.model.ai_tools_running = ai_tools_running;
        let git_branch_label = self
            .vcs_branch
            .as_ref()
            .and_then(|branch| {
                branch
                    .name
                    .clone()
                    .or_else(|| branch.detached.then(|| t!("ws.git_detached").to_string()))
            })
            .unwrap_or_else(|| t!("ws.git_no_branch").to_string());
        let status_left = self.status_context_label(&cwd, cx);
        let workspace_name = self
            .model
            .active_project_dir()
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| t!("ws.workspace").into());
        let bell_rows = agent_indicators
            .into_iter()
            .map(|indicator| {
                let tab_id = indicator.tab_id;
                div()
                    .id(SharedString::from(format!("agent-state-{}", indicator.id)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(ui::border(&p))
                    .child(SharedString::from(indicator.title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(gpui_color(status_color(&p, indicator.status)))
                            .child(status_label(indicator.status)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, _window, cx| {
                            workspace.open_agent_surface(tab_id, cx)
                        }),
                    )
            })
            .collect::<Vec<_>>();
        let bell_panel = self.bell_open.then(|| {
            div()
                .id("agent-bell-panel")
                .flex()
                .flex_col()
                .bg(gpui_color(p.overlay))
                .border_b_1()
                .border_color(ui::border(&p))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(t!("chrome.agent_activity")),
                )
                .children(bell_rows)
                .with_animation(
                    "bell-panel-fade-in",
                    Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                    |style, delta| style.opacity(delta),
                )
        });
        let toast_elements = self
            .toasts
            .clone()
            .into_iter()
            .map(|toast| {
                let id = toast.id;
                let target = toast.notification.target;
                let color = status_color(&p, toast.notification.status);
                div()
                    .id(SharedString::from(format!("agent-toast-{id}")))
                    .mx_3()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui_color(color))
                    .bg(gpui_color(p.elevated))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(SharedString::from(toast.notification.title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(gpui_color(color))
                                    .child(status_label(toast.notification.status)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child(SharedString::from(toast.notification.body)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |workspace, _event, _window, cx| {
                            workspace.toasts.retain(|toast| toast.id != id);
                            workspace.open_notification_target(target, cx);
                        }),
                    )
                    .with_animation(
                        SharedString::from(format!("toast-fade-{id}")),
                        Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                        |style, delta| style.opacity(delta),
                    )
            })
            .collect::<Vec<_>>();
        let ssh_context_menu = self.ssh_session_menu(cx);
        let ssh_group_menu = self.ssh_group_menu(cx);
        let explorer_context_menu = self.explorer_context_menu.clone().map(|menu| {
            anchored().position(menu.position).child(
                menu_panel(&p)
                    .id("explorer-context-menu")
                    .w(px(220.0))
                    .children(explorer_context_actions(&menu.target).iter().copied().map(
                        |action| {
                            div()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_xs()
                                .cursor_pointer()
                                .hover({
                                    let wash = ui::hover_wash(&p);
                                    move |style| style.bg(wash)
                                })
                                .child(explorer_context_action_label(action))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _event, window, cx| {
                                        cx.stop_propagation();
                                        this.handle_explorer_context_action(action, window, cx);
                                    }),
                                )
                        },
                    ))
                    .with_animation(
                        "explorer-menu-fade-in",
                        Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                        |style, delta| style.opacity(delta),
                    ),
            )
        });
        let can_split_right = self.can_split_active(SplitDirection::Right, window);
        let can_split_down = self.can_split_active(SplitDirection::Down, window);
        let has_multiple_panes = self.active_pane_count() > 1;
        let new_tab_menu = self.new_tab_menu.map(|position| {
            anchored().position(position).child(
                menu_panel(&p)
                    .id("new-tab-menu")
                    .w(px(220.0))
                    .child(new_tab_menu_item(
                        t!("ws.tab_terminal"),
                        "new-tab-terminal",
                        true,
                        NewTabAction::Terminal,
                        &p,
                        cx,
                    ))
                    .child(new_tab_menu_item(
                        t!("ws.tab_editor"),
                        "new-tab-editor",
                        true,
                        NewTabAction::Editor,
                        &p,
                        cx,
                    ))
                    .child(new_tab_menu_item(
                        t!("ws.new_tab_ssh"),
                        "new-tab-ssh",
                        true,
                        NewTabAction::Ssh,
                        &p,
                        cx,
                    ))
                    .with_animation(
                        "new-tab-menu-fade-in",
                        Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                        |style, delta| style.opacity(delta),
                    ),
            )
        });
        // 新建终端 shell 选择器（`shell_prompt` 开启时替代直接创建）：默认项 +
        // 后台探测到的 shell（含 WSL 发行版）。列表异步刷新，先渲染已有缓存。
        let shell_menu =
            self.shell_menu.map(|position| {
                anchored().position(position).child(
                    menu_panel(&p)
                        .id("shell-picker-menu")
                        .w(px(280.0))
                        .child(
                            div()
                                .id("shell-picker-title")
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(ui::muted(&p))
                                .child(t!("ws.choose_shell")),
                        )
                        .child(shell_menu_item(
                            crate::shell_select::ShellOption::Default,
                            "shell-menu-default",
                            &p,
                            cx,
                        ))
                        .children(self.discovered_shells.iter().enumerate().map(
                            |(index, shell)| {
                                shell_menu_item(
                                    crate::shell_select::ShellOption::from(shell),
                                    SharedString::from(format!("shell-menu-{index}")),
                                    &p,
                                    cx,
                                )
                            },
                        ))
                        .with_animation(
                            "shell-menu-fade-in",
                            Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                            |style, delta| style.opacity(delta),
                        ),
                )
            });
        let pane_context_menu = self.pane_context_menu.clone().map(|menu| {
            anchored().position(menu.position).child(
                menu_panel(&p)
                    .id("pane-context-menu")
                    .w(px(240.0))
                    .when_some(menu.terminal, |panel, terminal| {
                        let can_copy = terminal.read(cx).has_selection();
                        let has_failed = terminal.read(cx).has_failed_command();
                        panel
                            .child(terminal_menu_item(
                                t!("ws.copy"),
                                "terminal-copy",
                                can_copy,
                                TerminalMenuAction::Copy,
                                terminal.clone(),
                                &p,
                                cx,
                            ))
                            .child(terminal_menu_item(
                                t!("ws.paste"),
                                "terminal-paste",
                                true,
                                TerminalMenuAction::Paste,
                                terminal.clone(),
                                &p,
                                cx,
                            ))
                            .child(terminal_menu_item(
                                t!("ws.select_all"),
                                "terminal-select-all",
                                true,
                                TerminalMenuAction::SelectAll,
                                terminal.clone(),
                                &p,
                                cx,
                            ))
                            .child(terminal_menu_item(
                                t!("ws.find"),
                                "terminal-find",
                                true,
                                TerminalMenuAction::Find,
                                terminal.clone(),
                                &p,
                                cx,
                            ))
                            .child(menu_separator(&p))
                            .child(terminal_menu_item(
                                t!("ws.ask_ai_selection"),
                                "terminal-ask-ai",
                                can_copy,
                                TerminalMenuAction::AskAi,
                                terminal.clone(),
                                &p,
                                cx,
                            ))
                            .child(terminal_menu_item(
                                t!("ws.ask_ai_last_failed"),
                                "terminal-ask-ai-failed",
                                has_failed,
                                TerminalMenuAction::AskAiLastFailed,
                                terminal,
                                &p,
                                cx,
                            ))
                            .child(menu_separator(&p))
                    })
                    .child(split_menu_item(
                        t!("ws.split_right"),
                        "pane-menu-split-right",
                        can_split_right,
                        SplitMenuAction::Right,
                        &p,
                        cx,
                    ))
                    .child(split_menu_item(
                        t!("ws.split_down"),
                        "pane-menu-split-down",
                        can_split_down,
                        SplitMenuAction::Down,
                        &p,
                        cx,
                    ))
                    .child(menu_separator(&p))
                    .child(split_menu_item(
                        t!("ws.close_focused_pane"),
                        "pane-menu-close",
                        has_multiple_panes,
                        SplitMenuAction::CloseActive,
                        &p,
                        cx,
                    ))
                    .child(split_menu_item(
                        t!("ws.keep_only_focused_pane"),
                        "pane-menu-close-others",
                        has_multiple_panes,
                        SplitMenuAction::CloseOthers,
                        &p,
                        cx,
                    ))
                    .with_animation(
                        "pane-menu-fade-in",
                        Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                        |style, delta| style.opacity(delta),
                    ),
            )
        });
        let input_focus = self.focus_handle.clone();
        let input_entity = cx.entity();
        // Composer 停靠区：右侧停靠挂在内容行末尾，底部停靠排在内容行之下；
        // 分隔线由拖拽手柄承担，手柄双击恢复默认尺寸。
        let terminal_hidden = self.terminal_area_hidden();
        self.composer.update(cx, |composer, cx| {
            composer.set_fill_workspace(terminal_hidden, cx)
        });
        let composer_section_right = (!terminal_hidden
            && self.model.composer_visible
            && self.model.composer_dock == ComposerDock::Right)
            .then(|| {
                let wash = ui::hover_wash(&p);
                div()
                    .flex()
                    .flex_row()
                    .h_full()
                    .flex_shrink_0()
                    .child(
                        div()
                            .id("composer-resize-handle")
                            .w(px(COMPOSER_RESIZE_HANDLE_SIZE))
                            .h_full()
                            .flex_shrink_0()
                            .border_l_1()
                            .border_color(ui::border(&p))
                            .cursor(CursorStyle::ResizeColumn)
                            .hover(move |style| style.bg(wash))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::start_composer_resize),
                            ),
                    )
                    .child(
                        div()
                            .w(px(self.model.composer_dock_width))
                            .h_full()
                            .flex_shrink_0()
                            .child(self.composer.clone()),
                    )
            });
        let composer_section_bottom = (!terminal_hidden
            && self.model.composer_visible
            && self.model.composer_dock == ComposerDock::Bottom)
            .then(|| {
                let wash = ui::hover_wash(&p);
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(
                        div()
                            .id("composer-resize-handle")
                            .w_full()
                            .h(px(COMPOSER_RESIZE_HANDLE_SIZE))
                            .flex_shrink_0()
                            .border_t_1()
                            .border_color(ui::border(&p))
                            .cursor(CursorStyle::ResizeRow)
                            .hover(move |style| style.bg(wash))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::start_composer_resize),
                            ),
                    )
                    .child(self.composer.clone())
            });
        let root = div()
            .id("workspace-root")
            .relative()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &OpenWorkspace, window, cx| {
                this.open_workspace(window, cx);
            }))
            .on_key_down(cx.listener(Self::global_key))
            .on_mouse_move(cx.listener(Self::resize_panels))
            .on_mouse_move(cx.listener(Self::move_titlebar))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_panel_resizes))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::stop_titlebar_move))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, _window, cx| {
                    let closed_ssh = this.ssh_context_menu.take().is_some();
                    let closed_group = this.ssh_group_menu.take().is_some();
                    let closed_explorer = this.explorer_context_menu.take().is_some();
                    let closed_new_tab = this.new_tab_menu.take().is_some();
                    let closed_shell = this.shell_menu.take().is_some();
                    let closed_pane = this.pane_context_menu.take().is_some();
                    if closed_ssh
                        || closed_group
                        || closed_explorer
                        || closed_new_tab
                        || closed_shell
                        || closed_pane
                    {
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(p.background))
            .text_color(gpui_color(p.foreground))
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &input_focus,
                            ElementInputHandler::new(bounds, input_entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(
                background_layer_element(background_layer),
                |root, element| root.child(element),
            )
            .child(
                // 标题栏：系统标题栏已关闭，这一行自己承担标签页、操作区与窗口
                // 控制按钮。窗口控制按钮不自己处理点击 —— 平台层认得
                // `WindowControlArea`，于是 Snap Layouts、双击最大化、系统菜单
                // 都还是原生行为。
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(WORKSPACE_HEADER_HEIGHT))
                    .bg(gpui_color(p.chrome))
                    // macOS 的红绿灯由系统画在左上角，内容得给它让位。
                    .pl(px(app_identity::titlebar_leading_inset()))
                    .child(toggle_sidebar_button)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .min_w(px(0.0))
                            .h_full()
                            .overflow_hidden()
                            .children(tab_buttons)
                            .child(new_tab_button),
                    )
                    // 唯一的拖拽区。只覆盖标签页与操作区之间的空隙，而不是整条
                    // 标题栏：命中测试按绘制顺序返回第一个匹配，父级先于子级插入，
                    // 若把拖拽区铺满整行，里面每个控件都得 `occlude()` 才不会被
                    // 吞掉，而 `occlude()` 又会让根节点收不到"点空白处关菜单"的点击。
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(TITLE_BAR_DRAG_MIN_WIDTH))
                            .h_full()
                            .window_control_area(WindowControlArea::Drag)
                            // Windows：HTCAPTION 命中测试原生处理拖拽与双击。
                            // Linux/BSD：gpui 后端不做窗口控制命中测试，拖拽
                            // 与双击最大化由应用自己处理（对齐 Zed
                            // platform_title_bar 的 Linux 路径）。
                            .when(handles_own_window_control_clicks(), |gap| {
                                gap.on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::start_titlebar_move),
                                )
                            }),
                    )
                    .child(
                        // 标题栏操作区：打开工作区 + 通知 + 设置。
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .flex_shrink_0()
                            .gap_1()
                            .px_2()
                            .when_some(preview_controls, |area, controls| area.child(controls))
                            .child(
                                ui::icon_button(
                                    "titlebar-open-workspace",
                                    Icon::FolderOpen,
                                    t!("ws.open_workspace_hint"),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(Self::open_workspace_picker),
                                ),
                            )
                            .child({
                                // 需要注意时用状态色着色，而不是把整个按钮刷成实底 ——
                                // 一个提醒不该盖过它旁边的所有东西。
                                let tint = if bell_needs_attention {
                                    gpui_color(p.status[2])
                                } else {
                                    gpui_color_alpha(p.foreground, 0.72)
                                };
                                let wash = ui::hover_wash(&p);
                                div()
                                    .id("agent-bell")
                                    .aria_label(t!("chrome.agent_activity"))
                                    .tooltip({
                                        let palette = p.clone();
                                        move |_window, cx| {
                                            Tooltip::view(t!("chrome.agent_activity"), &palette, cx)
                                        }
                                    })
                                    .h(px(tokens::height::REGULAR))
                                    .px(px(tokens::space::SM))
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens::space::XS))
                                    .rounded(px(tokens::radius::MD))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(wash))
                                    .child(ui::icon(Icon::Bell, icon_size::SM, tint))
                                    .when(bell_count > 0, |bell| {
                                        bell.child(
                                            div()
                                                .text_size(px(tokens::font_size::MICRO))
                                                .text_color(tint)
                                                .child(SharedString::from(bell_count.to_string())),
                                        )
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::toggle_bell),
                                    )
                            })
                            .child(
                                ui::icon_button(
                                    "settings",
                                    Icon::Settings,
                                    t!("chrome.settings"),
                                    &p,
                                )
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::open_settings)),
                            )
                            .when(crate::updater::ready(cx), |bar| {
                                bar.child(
                                    ui::button(
                                        "update-ready",
                                        t!("ws.update_available"),
                                        ui::ButtonKind::Subtle,
                                        &p,
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            if let Some(handle) = this.open_settings_window(cx) {
                                                let _ = handle.update(cx, |settings, _, cx| {
                                                    settings.show_about(cx)
                                                });
                                            }
                                        }),
                                    ),
                                )
                            }),
                    )
                    .when(draws_own_window_controls(), |bar| {
                        bar.child(window_controls(&p, window.is_maximized()))
                    }),
            )
            .children(bell_panel)
            .children(toast_elements)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .w_full()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .size_full()
                            .flex()
                            .flex_col()
                            .when(
                                self.model
                                    .active_tab()
                                    .is_some_and(|tab| tab.remote.is_some()),
                                |area| {
                                    let running = self
                                        .model
                                        .active
                                        .is_some_and(|id| self.remote_runtime_connected(id, cx));
                                    area.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .py_1()
                                            .child({
                                                // 会话状态以圆点颜色表达：绿=运行中/已连接，
                                                // 灰=未连接或已正常结束，红=失败/异常。
                                                let (state, dot_color) = if running {
                                                    (
                                                        t!("ws.remote_state_running"),
                                                        ui::color(p.status[1]),
                                                    )
                                                } else {
                                                    match self.active_terminal().and_then(
                                                        |terminal| terminal.read(cx).exit_code(),
                                                    ) {
                                                        Some(0) => (
                                                            t!("ws.remote_state_finished"),
                                                            ui::alpha(p.foreground, 0.35),
                                                        ),
                                                        Some(_) => (
                                                            t!("ws.remote_state_failed"),
                                                            ui::color(p.status[3]),
                                                        ),
                                                        None => (
                                                            t!("ws.remote_state_disconnected"),
                                                            ui::alpha(p.foreground, 0.35),
                                                        ),
                                                    }
                                                };
                                                div()
                                                    .id("remote-status-dot")
                                                    .size_2()
                                                    .rounded_full()
                                                    .bg(dot_color)
                                                    .tooltip({
                                                        let palette = p.clone();
                                                        move |_window, cx| {
                                                            Tooltip::view(
                                                                state.clone(),
                                                                &palette,
                                                                cx,
                                                            )
                                                        }
                                                    })
                                            })
                                            .child(
                                                ui::button(
                                                    "remote-reconnect",
                                                    if self
                                                        .model
                                                        .active_tab()
                                                        .and_then(|tab| tab.remote.as_ref())
                                                        .is_some_and(|remote| {
                                                            remote.transfer.is_some()
                                                        })
                                                    {
                                                        t!("ws.new_transfer")
                                                    } else {
                                                        t!("ws.reconnect")
                                                    },
                                                    ButtonKind::Subtle,
                                                    &p,
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.reconnect_remote(window, cx)
                                                })),
                                            )
                                            .child(
                                                ui::button(
                                                    "remote-disconnect",
                                                    t!("ws.disconnect"),
                                                    ButtonKind::Ghost,
                                                    &p,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if let Some(id) = this.model.active {
                                                        this.close_remote_explorer(id);
                                                        cx.notify();
                                                    }
                                                    if let Some(terminal) =
                                                        this.active_terminal().cloned()
                                                    {
                                                        terminal.update(cx, |terminal, _| {
                                                            terminal.disconnect()
                                                        });
                                                    }
                                                })),
                                            )
                                            .child(
                                                ui::button(
                                                    "remote-manager",
                                                    t!("ws.connection_manager"),
                                                    ButtonKind::Ghost,
                                                    &p,
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_ssh_manager(window, cx)
                                                })),
                                            ),
                                    )
                                },
                            )
                            .when(!terminal_hidden, |area| {
                                area.child(div().flex_1().min_h_0().child(active_content))
                            })
                            .when(terminal_hidden && self.model.composer_visible, |area| {
                                area.child(self.composer.clone())
                            }),
                    )
                    .children(composer_section_right),
            )
            .children(composer_section_bottom)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(STATUS_BAR_HEIGHT))
                    .px_3()
                    .gap_3()
                    .bg(gpui_color(p.chrome))
                    .border_t_1()
                    .border_color(ui::border(&p))
                    .text_xs()
                    .text_color(ui::muted(&p))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(status_left)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .flex_shrink_0()
                            .child({
                                let wash = ui::hover_wash(&p);
                                let tint = gpui_color_alpha(p.foreground, 0.72);
                                div()
                                    .id("status-open-workspace")
                                    .aria_label(t!("ws.open_workspace"))
                                    .tooltip({
                                        let palette = p.clone();
                                        move |_window, cx| {
                                            Tooltip::view(
                                                t!("ws.open_workspace_hint"),
                                                &palette,
                                                cx,
                                            )
                                        }
                                    })
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens::space::XS))
                                    .h(px(tokens::height::REGULAR))
                                    .px(px(tokens::space::SM))
                                    .rounded(px(tokens::radius::MD))
                                    .cursor_pointer()
                                    .hover(move |style| style.bg(wash))
                                    .child(ui::icon(Icon::FolderOpen, icon_size::SM, tint))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(tint)
                                            .child(SharedString::from(workspace_name)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::open_workspace_picker),
                                    )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(ui::icon(Icon::GitBranch, icon_size::SM, ui::muted(&p)))
                                    .child(SharedString::from(git_branch_label)),
                            )
                            .child(
                                div()
                                    .text_color(gpui_color(status_color(&p, ai_status)))
                                    .child(
                                        tf!("ws.ai_status", "status" => status_label(ai_status)),
                                    ),
                            )
                            .when(ai_tools_running > 0, |row| {
                                row.child(tf!(
                                    "ws.ai_tools_running",
                                    "count" => ai_tools_running
                                ))
                            })
                            .when_some(preview_url, |row, url| {
                                let label = tf!("ws.open_web_preview", "url" => url.clone());
                                row.child(
                                    ui::button(
                                        "open-detected-preview",
                                        label,
                                        ButtonKind::Subtle,
                                        &p,
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |workspace, _, window, cx| {
                                            workspace.create_preview_url(url.clone(), window, cx)
                                        }),
                                    ),
                                )
                            })
                            .when(
                                self.model
                                    .active_tab()
                                    .is_some_and(|tab| tab.kind == TabKind::Terminal)
                                    && !self.active_is_sftp_browser(),
                                |row| {
                                    row.child(
                                        ui::icon_button(
                                            "terminal-toggle",
                                            Icon::Terminal,
                                            if terminal_hidden {
                                                t!("ws.show_terminal")
                                            } else {
                                                t!("ws.hide_terminal")
                                            },
                                            &p,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, window, cx| {
                                                this.toggle_terminal_area(window, cx);
                                            }),
                                        ),
                                    )
                                },
                            )
                            .child(
                                ui::icon_button(
                                    "composer-toggle",
                                    if self.model.composer_visible {
                                        Icon::ChevronDown
                                    } else {
                                        Icon::ChevronUp
                                    },
                                    if self.model.composer_visible {
                                        t!("ws.hide_agent_panel")
                                    } else {
                                        t!("ws.show_agent_panel")
                                    },
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_chrome_menus();
                                        this.set_composer_visible(
                                            !this.model.composer_visible,
                                            window,
                                            cx,
                                        );
                                    }),
                                ),
                            ),
                    ),
            )
            .children(ssh_context_menu)
            .children(ssh_group_menu)
            .children(explorer_context_menu)
            .children(new_tab_menu)
            .children(shell_menu)
            .children(pane_context_menu);
        self.render_window_frame(root, &p, window, cx)
    }
}

impl WorkspaceView {
    /// CSD 窗口框（对齐 Zed `workspace::client_side_decorations`，仅 Linux/BSD
    /// 且平台报告 [`Decorations::Client`] 时启用）：合成器下窗口管理器不画任何
    /// 边框，缩放手柄（环带命中 + `start_window_resize`）、resize 光标、阴影、
    /// 圆角全部由应用自绘。环带是布局内边距而非覆盖层，不会抢内容的点击——
    /// 这也是必须用环带而不是贴边覆盖层的原因：覆盖层无法在内容处理器之前
    /// 拦截按下事件。tiled/maximized 的边不留环带也不可缩放。
    ///
    /// Windows（WS_THICKFRAME/命中测试）与 macOS（系统边框）原样返回；X11 无
    /// 合成器时 gpui 自动回退 [`Decorations::Server`]，同样原样返回。
    fn render_window_frame(
        &mut self,
        content: impl IntoElement,
        p: &ResolvedPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        const BORDER: Pixels = px(1.0);
        if !handles_own_window_control_clicks() {
            return div().id("window-frame").size_full().child(content);
        }
        let Decorations::Client { tiling } = window.window_decorations() else {
            window.set_client_inset(px(0.0));
            return div().id("window-frame").size_full().child(content);
        };
        // 告诉 WM 真实内容内缩（X11 写 _GTK_FRAME_EXTENTS）：最大化时窗口可以
        // 铺到显示器边缘，内容仍落在工作区内。
        window.set_client_inset(WINDOW_RESIZE_BAND);

        div()
            .id("window-frame")
            .map(|frame| rounded_client_corners(frame, &tiling))
            .when(!tiling.top, |frame| frame.pt(WINDOW_RESIZE_BAND))
            .when(!tiling.bottom, |frame| frame.pb(WINDOW_RESIZE_BAND))
            .when(!tiling.left, |frame| frame.pl(WINDOW_RESIZE_BAND))
            .when(!tiling.right, |frame| frame.pr(WINDOW_RESIZE_BAND))
            // resize 光标登记在渲染帧上，鼠标位置变化不会自动重算样式；进入/
            // 离开/切换缩放边时刷一帧，下方 canvas 才会为新位置登记光标。
            .on_mouse_move(
                cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                    let edge = resize_edge(
                        event.position,
                        WINDOW_RESIZE_BAND,
                        window.window_bounds().get_bounds().size,
                        tiling,
                    );
                    if edge != this.resize_edge_hint {
                        this.resize_edge_hint = edge;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                if let Some(edge) = resize_edge(
                    event.position,
                    WINDOW_RESIZE_BAND,
                    window.window_bounds().get_bounds().size,
                    tiling,
                ) {
                    window.start_window_resize(edge);
                }
            })
            .size_full()
            .child(
                div()
                    .cursor(CursorStyle::Arrow)
                    .map(|inner| rounded_client_corners(inner, &tiling))
                    .border_color(ui::border(p))
                    .when(!tiling.top, |inner| inner.border_t(BORDER))
                    .when(!tiling.bottom, |inner| inner.border_b(BORDER))
                    .when(!tiling.left, |inner| inner.border_l(BORDER))
                    .when(!tiling.right, |inner| inner.border_r(BORDER))
                    .when(!tiling.is_tiled(), |inner| {
                        inner.shadow(vec![gpui::BoxShadow::new(
                            px(0.0),
                            px(0.0),
                            gpui::Hsla {
                                h: 0.0,
                                s: 0.0,
                                l: 0.0,
                                a: 0.4,
                            },
                        )
                        .blur_radius(WINDOW_RESIZE_BAND / 2.0)])
                    })
                    .size_full()
                    .child(content),
            )
            .child(
                // 全窗口命中盒：按当前鼠标位置把 resize 光标登记到渲染帧上。
                // 只有光标登记，没有鼠标监听，不会影响内容的命中测试。
                canvas(
                    |_bounds, window, _| {
                        window.insert_hitbox(
                            Bounds::new(
                                Point::new(px(0.0), px(0.0)),
                                window.window_bounds().get_bounds().size,
                            ),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_bounds, hitbox, window, _cx| {
                        let mouse = window.mouse_position();
                        let size = window.window_bounds().get_bounds().size;
                        let Some(edge) = resize_edge(mouse, WINDOW_RESIZE_BAND, size, tiling)
                        else {
                            return;
                        };
                        window.set_cursor_style(
                            match edge {
                                ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                                ResizeEdge::Left | ResizeEdge::Right => {
                                    CursorStyle::ResizeLeftRight
                                }
                                ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                    CursorStyle::ResizeUpLeftDownRight
                                }
                                ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                    CursorStyle::ResizeUpRightDownLeft
                                }
                            },
                            &hitbox,
                        );
                    },
                )
                .size_full()
                .absolute(),
            )
    }
}
