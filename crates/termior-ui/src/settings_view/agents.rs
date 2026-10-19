//! Agent 页：定义、工具权限、数据占用与清理。

use super::*;

impl SettingsView {
    pub(super) fn agent(&self) -> Option<&AgentDefinition> {
        self.agents.agents.get(self.agent_index)
    }

    pub(super) fn agent_mut(&mut self) -> Option<&mut AgentDefinition> {
        self.agents.agents.get_mut(self.agent_index)
    }

    pub(super) fn new_agent(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        self.confirm_agent_removal = false;
        self.select_menu = None;
        let next = (1..)
            .find(|index| {
                !self
                    .agents
                    .agents
                    .iter()
                    .any(|agent| agent.id == format!("custom-agent-{index}"))
            })
            .unwrap_or(1);
        self.agents.agents.push(AgentDefinition {
            id: format!("custom-agent-{next}"),
            name: format!("Custom Agent {next}"),
            system_prompt: "You are a focused coding assistant.".into(),
            tools: Vec::new(),
            icon: "agent".into(),
            color: "#4f8fef".into(),
        });
        self.agent_index = self.agents.agents.len() - 1;
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn select_agent(&mut self, index: usize, cx: &mut Context<Self>) {
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        if index < self.agents.agents.len() {
            self.agent_index = index;
        }
        self.confirm_agent_removal = false;
        self.select_menu = None;
        cx.notify();
    }

    pub(super) fn toggle_agent_tool(&mut self, name: &str, cx: &mut Context<Self>) {
        self.commit_edit();
        if let Some(agent) = self.agent_mut() {
            if agent.tools.iter().any(|tool| tool == name) {
                agent.tools.retain(|tool| tool != name);
            } else {
                agent.tools.push(name.to_owned());
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn set_agent_tools(&mut self, read_only: bool, cx: &mut Context<Self>) {
        self.commit_edit();
        if let Some(agent) = self.agent_mut() {
            agent.tools = termior_security::gating::ALL_TOOLS
                .iter()
                .filter(|tool| {
                    read_only && tool.level() == termior_security::gating::ToolLevel::Auto
                })
                .map(|tool| tool.name().to_owned())
                .collect();
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn remove_agent(&mut self, cx: &mut Context<Self>) {
        self.commit_edit();
        self.confirm_agent_removal = false;
        if self.agent_index < self.agents.agents.len() {
            self.agents.agents.remove(self.agent_index);
            self.agent_index = self
                .agent_index
                .min(self.agents.agents.len().saturating_sub(1));
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn install_hooks(&mut self, cx: &mut Context<Self>) {
        self.status = match termior_hooks::install() {
            Ok(result) => {
                tf!("settings.status.hooks_installed", "count" => result.added).to_string()
            }
            Err(error) => tf!("settings.status.hooks_install_failed", "error" => error).to_string(),
        };
        cx.notify();
    }

    pub(super) fn uninstall_hooks(&mut self, cx: &mut Context<Self>) {
        self.status = match termior_hooks::uninstall() {
            Ok(result) => {
                tf!("settings.status.hooks_removed", "count" => result.removed).to_string()
            }
            Err(error) => {
                tf!("settings.status.hooks_uninstall_failed", "error" => error).to_string()
            }
        };
        cx.notify();
    }

    pub(super) fn agent_tools(&self, cx: &mut Context<Self>) -> AnyElement {
        use termior_security::gating::{ToolLevel, ALL_TOOLS};
        let Some(agent) = self.agent() else {
            return div().into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(tn!(agent.tools.len(), "settings.agents.tools_selected"))
            .child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(&self.palette))
                    .child(t!("settings.agents.tools_hint")),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Self::button(
                            t!("settings.agents.read_only_preset"),
                            "agent-read-only",
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.set_agent_tools(true, cx)),
                        ),
                    )
                    .child(
                        Self::button(
                            t!("settings.agents.clear_all"),
                            "agent-no-tools",
                            &self.palette,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.set_agent_tools(false, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .id("agent-tools-list")
                    .max_h(px(280.0))
                    .overflow_y_scroll()
                    .children(
                        [ToolLevel::Auto, ToolLevel::Approval]
                            .into_iter()
                            .map(|level| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_sm().child(if level == ToolLevel::Auto {
                                        t!("settings.agents.tools_group_auto")
                                    } else {
                                        t!("settings.agents.tools_group_approval")
                                    }))
                                    .children(
                                        ALL_TOOLS
                                            .iter()
                                            .filter(move |tool| tool.level() == level)
                                            .map(|tool| {
                                                let name = tool.name();
                                                Self::select_option(
                                                    tool_label(*tool),
                                                    SharedString::from(format!(
                                                        "agent-tool-{name}"
                                                    )),
                                                    agent.tools.iter().any(|t| t == name),
                                                    &self.palette,
                                                )
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.toggle_agent_tool(name, cx)
                                                    }),
                                                )
                                            }),
                                    )
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn refresh_agent_usage(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.data_dir.clone() else {
            self.agent_usage = None;
            self.agent_usage_error = Some(t!("settings.agents.data_usage_unavailable").to_string());
            return;
        };
        self.agent_usage_error = None;
        let task = cx.spawn(async move |view, cx| {
            let usage = cx
                .background_executor()
                .spawn(async move { termior_store::measure_agent_disk_usage(&root) })
                .await;
            let _ = view.update(cx, |view, cx| {
                match usage {
                    Ok(usage) => {
                        view.agent_usage = Some(usage);
                        view.agent_usage_error = None;
                    }
                    Err(error) => {
                        view.agent_usage = None;
                        view.agent_usage_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn start_agent_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup_running {
            return;
        }
        self.commit_edit();
        self.schedule_save_if_dirty(cx);
        if let Err(error) = self.settings.validate() {
            self.status = error.to_string();
            cx.notify();
            return;
        }
        let Some(root) = self.data_dir.clone() else {
            self.status = t!("settings.agents.data_usage_unavailable").to_string();
            cx.notify();
            return;
        };
        let policy = self.settings.agent_retention.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cleanup_cancel = Some(cancel.clone());
        self.cleanup_running = true;
        self.status = t!("settings.agents.data_cleanup_running").to_string();
        cx.notify();
        let task = cx.spawn(async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
                        .unwrap_or(0);
                    let report = termior_store::cleanup_agent_data(&root, policy, now_ms, &cancel)
                        .map_err(|error| error.to_string())?;
                    let usage = termior_store::measure_agent_disk_usage(&root)
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>((report, usage))
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.cleanup_running = false;
                view.cleanup_cancel = None;
                match outcome {
                    Ok((report, usage)) => {
                        view.agent_usage = Some(usage);
                        view.agent_usage_error = None;
                        let freed = report.before_bytes.saturating_sub(report.after_bytes);
                        let size = crate::updater::format_bytes(freed);
                        view.status = if report.cancelled {
                            tf!("settings.agents.data_cleanup_cancelled", "size" => size)
                                .to_string()
                        } else if freed == 0 {
                            t!("settings.agents.data_cleanup_none").to_string()
                        } else {
                            tf!("settings.agents.data_cleanup_done", "size" => size).to_string()
                        };
                    }
                    Err(error) => {
                        view.status = tf!("settings.agents.data_cleanup_failed", "error" => error)
                            .to_string();
                    }
                }
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn cancel_agent_cleanup(&mut self, cx: &mut Context<Self>) {
        if let Some(flag) = &self.cleanup_cancel {
            flag.store(true, Ordering::Release);
        }
        cx.notify();
    }

    pub(super) fn usage_line(&self, id: &'static str, text: SharedString) -> AnyElement {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .text_sm()
            .child(text)
            .into_any_element()
    }

    pub(super) fn agent_data_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let usage: Vec<AnyElement> = if let Some(error) = &self.agent_usage_error {
            vec![self.usage_line("agent-data-usage-error", error.clone().into())]
        } else if let Some(usage) = self.agent_usage {
            vec![
                self.usage_line(
                    "agent-data-usage-total",
                    tf!("settings.agents.data_usage_total", "size" => crate::updater::format_bytes(usage.total())),
                ),
                self.usage_line(
                    "agent-data-usage-journal",
                    tf!("settings.agents.data_usage_journal", "size" => crate::updater::format_bytes(usage.journal)),
                ),
                self.usage_line(
                    "agent-data-usage-blobs",
                    tf!("settings.agents.data_usage_blobs", "size" => crate::updater::format_bytes(usage.blobs)),
                ),
                self.usage_line(
                    "agent-data-usage-context",
                    tf!("settings.agents.data_usage_context", "size" => crate::updater::format_bytes(usage.context)),
                ),
                self.usage_line(
                    "agent-data-usage-hook-mcp",
                    tf!("settings.agents.data_usage_hook_mcp", "size" => crate::updater::format_bytes(usage.hook_mcp)),
                ),
                self.usage_line(
                    "agent-data-usage-crash",
                    tf!("settings.agents.data_usage_crash", "size" => crate::updater::format_bytes(usage.crash)),
                ),
                self.usage_line(
                    "agent-data-usage-other",
                    tf!("settings.agents.data_usage_other", "size" => crate::updater::format_bytes(usage.other)),
                ),
            ]
        } else {
            vec![self.usage_line(
                "agent-data-usage-loading",
                t!("settings.agents.data_usage_loading"),
            )]
        };
        let action = if self.cleanup_running {
            Self::button(
                t!("settings.agents.data_cancel_cleanup"),
                "agent-data-cancel-cleanup",
                &self.palette,
            )
            .debug_selector(|| "agent-data-cancel-cleanup".into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_agent_cleanup(cx)),
            )
            .into_any_element()
        } else {
            Self::button(
                t!("settings.agents.data_cleanup"),
                "agent-data-cleanup",
                &self.palette,
            )
            .debug_selector(|| "agent-data-cleanup".into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.start_agent_cleanup(cx)),
            )
            .into_any_element()
        };
        self.section(
            t!("settings.agents.data_title"),
            t!("settings.agents.data_description"),
            usage
                .into_iter()
                .chain([
                    self.edit_row(
                        t!("settings.agents.data_retention_days_label"),
                        t!("settings.agents.data_retention_days_description"),
                        EditField::RetentionDays,
                        cx,
                    ),
                    self.edit_row(
                        t!("settings.agents.data_retention_capacity_label"),
                        t!("settings.agents.data_retention_capacity_description"),
                        EditField::RetentionCapacity,
                        cx,
                    ),
                    action,
                ])
                .collect::<Vec<_>>(),
        )
    }

    fn capability_matrix_section(&self) -> AnyElement {
        let rows = termior_agent_host::capability_matrix()
            .into_iter()
            .map(|row| {
                let label = match row.backend {
                    "direct" => t!("settings.agents.matrix_direct"),
                    "worktree" => t!("settings.agents.matrix_worktree"),
                    "sandboxed" => t!("settings.agents.matrix_sandboxed"),
                    "external" => t!("settings.agents.matrix_external"),
                    _ => SharedString::from(row.backend.to_owned()),
                };
                let detail = tf!(
                    "composer.environment.capabilities",
                    "host" => row.host,
                    "root" => row.root,
                    "file" => row.file,
                    "network" => row.network,
                    "credentials" => row.credentials,
                    "process" => row.process
                );
                div()
                    .id(SharedString::from(format!(
                        "settings-capability-{}",
                        row.backend
                    )))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(div().text_sm().child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&self.palette))
                            .child(detail),
                    )
                    .into_any_element()
            });
        self.section(
            t!("settings.agents.matrix_title"),
            t!("settings.agents.matrix_description"),
            std::iter::once(
                div()
                    .id("settings-capability-matrix")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows)
                    .into_any_element(),
            ),
        )
    }

    pub(super) fn agents_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let hook_status = termior_hooks::status()
            .map(|status| {
                if status.fully_installed {
                    t!("settings.agents.hooks_installed")
                } else {
                    t!("settings.agents.hooks_not_installed")
                }
            })
            .unwrap_or_else(|_| t!("settings.agents.hooks_unavailable"));
        let agent_controls = if let Some(agent) = self.agent() {
            let menu_open = self.select_menu == Some(SelectMenu::AgentProfile);
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    Self::select_button(
                        t!("settings.agents.agent_label"),
                        agent.name.clone(),
                        "agent-select",
                        menu_open,
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_select_menu(SelectMenu::AgentProfile, cx);
                        }),
                    ),
                )
                .when(menu_open, |container| {
                    container.child(div().flex().flex_col().children(
                        self.agents.agents.iter().enumerate().map(|(index, agent)| {
                            Self::select_option(
                                agent.name.clone(),
                                SharedString::from(format!("agent-option-{}", agent.id)),
                                index == self.agent_index,
                                &self.palette,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.select_agent(index, cx)),
                            )
                        }),
                    ))
                })
                .child(self.edit_row(
                    t!("settings.agents.name_label"),
                    t!("settings.agents.name_description"),
                    EditField::AgentName,
                    cx,
                ))
                .child(self.edit_row(
                    t!("settings.agents.instructions_label"),
                    t!("settings.agents.instructions_description"),
                    EditField::AgentPrompt,
                    cx,
                ))
                .child(self.agent_tools(cx))
                .child(
                    Self::button(
                        if self.agent_appearance_open {
                            t!("settings.agents.appearance_open")
                        } else {
                            t!("settings.agents.appearance_closed")
                        },
                        "agent-appearance",
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.agent_appearance_open = !this.agent_appearance_open;
                            cx.notify();
                        }),
                    ),
                )
                .when(self.agent_appearance_open, |container| {
                    container.children([
                        self.edit_row(
                            t!("settings.agents.icon_label"),
                            t!("settings.agents.icon_description"),
                            EditField::AgentIcon,
                            cx,
                        ),
                        self.edit_row(
                            t!("settings.agents.color_label"),
                            t!("settings.agents.color_description"),
                            EditField::AgentColor,
                            cx,
                        ),
                    ])
                })
                .child(
                    Self::button(
                        t!("settings.agents.delete_agent_ellipsis"),
                        "remove-agent",
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.confirm_agent_removal = true;
                            cx.notify();
                        }),
                    ),
                )
                .when(self.confirm_agent_removal, |container| {
                    container.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(tf!(
                                "settings.agents.delete_confirm",
                                "name" => agent.name
                            ))
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Self::button(
                                            t!("settings.agents.delete_agent"),
                                            "confirm-remove-agent",
                                            &self.palette,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| this.remove_agent(cx)),
                                        ),
                                    )
                                    .child(
                                        Self::button(
                                            t!("settings.agents.cancel"),
                                            "cancel-remove-agent",
                                            &self.palette,
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.confirm_agent_removal = false;
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            ),
                    )
                })
                .into_any_element()
        } else {
            div()
                .text_sm()
                .child(t!("settings.agents.empty_hint"))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(self.capability_matrix_section())
            .child(self.agent_data_section(cx))
            .child(
                self.section(
                    t!("settings.agents.title"),
                    t!("settings.agents.description"),
                    [
                        Self::button(t!("settings.agents.new_agent"), "new-agent", &self.palette)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.new_agent(cx)),
                            )
                            .into_any_element(),
                        agent_controls,
                    ],
                ),
            )
            .child(
                self.section(
                    t!("settings.agents.hooks_title"),
                    t!("settings.agents.hooks_description"),
                    [
                        div()
                            .text_sm()
                            .child(tf!("settings.agents.hooks_status", "value" => hook_status))
                            .into_any_element(),
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Self::button(
                                    t!("settings.agents.install_hooks"),
                                    "install-hooks",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.install_hooks(cx)),
                                ),
                            )
                            .child(
                                Self::button(
                                    t!("settings.agents.uninstall_hooks"),
                                    "uninstall-hooks",
                                    &self.palette,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.uninstall_hooks(cx)),
                                ),
                            )
                            .into_any_element(),
                    ],
                ),
            )
            .child(
                self.section(
                    t!("settings.agents.notifications_title"),
                    t!("settings.agents.notifications_description"),
                    [Self::button(
                        tf!(
                            "settings.agents.notifications",
                            "state" => on_off(self.settings.agent_notifications)
                        ),
                        "agent-notifications",
                        &self.palette,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.settings.agent_notifications = !this.settings.agent_notifications;
                            this.schedule_save(cx);
                            cx.notify();
                        }),
                    )
                    .into_any_element()],
                ),
            )
            .into_any_element()
    }
}

fn tool_label(tool: termior_security::gating::ToolId) -> SharedString {
    use termior_security::gating::ToolId::*;
    match tool {
        ReadFile => t!("settings.tools.read_files"),
        ListDirectory => t!("settings.tools.list_folders"),
        FsSearch => t!("settings.tools.find_files"),
        FsGrep => t!("settings.tools.search_file_contents"),
        GetTerminalContext => t!("settings.tools.read_terminal_context"),
        WriteFile => t!("settings.tools.write_files"),
        CreateDirectory => t!("settings.tools.create_folders"),
        Rename => t!("settings.tools.rename_files"),
        Delete => t!("settings.tools.delete_files"),
        RunCommand => t!("settings.tools.run_command"),
        ShellSessionRun => t!("settings.tools.persistent_shell"),
        ShellBgSpawn => t!("settings.tools.start_background_commands"),
        CommandStatus => t!("settings.tools.check_command_status"),
        CommandReadOutput => t!("settings.tools.read_command_output"),
        CommandWait => t!("settings.tools.wait_for_commands"),
        CommandKill => t!("settings.tools.stop_commands"),
        CommandClaim => t!("settings.tools.take_control"),
        CommandWriteInput => t!("settings.tools.send_input"),
        RunSubagent => t!("settings.tools.delegate_subagents"),
        TodoRead => t!("settings.tools.read_todos"),
        TodoWrite => t!("settings.tools.write_todos"),
    }
}
