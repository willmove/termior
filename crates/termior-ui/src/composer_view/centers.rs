//! Composer 检查面板：恢复、自动化、检查点、技能、记忆、片段、TODO 与上下文。

use super::helpers::snippet_preview;
use super::*;

impl ComposerView {
    pub(super) fn apply_recovery_command(
        &mut self,
        task_id: &str,
        command: RecoveryCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.set_status(t!("composer.recovery.storage_unavailable"));
            cx.notify();
            return;
        };
        let tasks_root = data_dir.join("agent-tasks");
        match RecoveryCenter::apply(&tasks_root, task_id, command) {
            Ok(operation) => {
                self.set_status(tf!(
                    "composer.recovery.applied",
                    "attempt" => operation.attempt_id,
                    "state" => format!("{:?}", operation.state)
                ));
                self.recovery_tasks = RecoveryCenter::scan(&tasks_root).unwrap_or_default();
            }
            Err(error) => self.set_status(tf!("composer.recovery.action_failed", "error" => error)),
        }
        cx.notify();
    }

    pub(super) fn restore_checkpoint(&mut self, checkpoint_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.set_status(t!("composer.checkpoint.storage_unavailable"));
            cx.notify();
            return;
        };
        let store = CheckpointStore::new(data_dir.join("agent-checkpoints"));
        let result = (|| {
            let manifest = store.load_manifest(checkpoint_id)?;
            let plan = store.plan_restore(&self.workspace_root, &manifest)?;
            let restored = plan
                .actions
                .iter()
                .filter(|action| {
                    matches!(
                        action,
                        RestoreAction::Restore { .. } | RestoreAction::DeleteCreated { .. }
                    )
                })
                .count();
            let conflicts = plan
                .actions
                .iter()
                .filter(|action| matches!(action, RestoreAction::SkipConflict { .. }))
                .count();
            store.apply_restore(&self.workspace_root, &plan)?;
            Ok::<_, termior_store::CheckpointError>((restored, conflicts))
        })();
        match result {
            Ok((restored, conflicts)) => self.set_status(tf!(
                "composer.checkpoint.restored",
                "id" => checkpoint_id,
                "restored" => restored,
                "conflicts" => conflicts
            )),
            Err(error) => {
                self.set_status(tf!("composer.checkpoint.restore_failed", "error" => error))
            }
        }
        cx.notify();
    }

    pub(super) fn trigger_automation(&mut self, automation_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.set_status(t!("composer.automation.storage_unavailable"));
            cx.notify();
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let result = (|| {
            let mut manager = AutomationManager::load(data_dir)?;
            let run = manager.trigger_manual(automation_id, now)?;
            let task = run
                .create_task(&self.workspace_root, Some(data_dir))
                .map_err(std::io::Error::other)?;
            task.persist_journal(data_dir)
                .map_err(std::io::Error::other)?;
            self.automations = manager.store;
            Ok::<_, std::io::Error>(run)
        })();
        match result {
            Ok(run) => self.set_status(tf!(
                "composer.automation.queued",
                "id" => automation_id,
                "run" => run.run_id,
                "task" => run.task_id
            )),
            Err(error) => {
                self.set_status(tf!("composer.automation.trigger_failed", "error" => error))
            }
        }
        cx.notify();
    }

    pub(super) fn ensure_automation_host(&mut self, cx: &mut Context<Self>) {
        if self.automation_host_started || self.data_dir.is_none() {
            return;
        }
        self.automation_host_started = true;
        if let Some(data_dir) = self.data_dir.clone() {
            if let Ok(mut manager) = AutomationManager::load(&data_dir) {
                match manager.recover_interrupted(unix_now()) {
                    Ok(effects) => {
                        self.automations = manager.store;
                        for effect in effects {
                            self.present_automation_effect(&effect, cx);
                        }
                    }
                    Err(error) => log::warn!("automation recovery failed: {error}"),
                }
            }
        }
        cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            let request = view.update(cx, |view, _| view.automation_tick_request());
            let Ok(Some(request)) = request else {
                break;
            };
            let used = cx
                .background_executor()
                .spawn({
                    let data_dir = request.data_dir.clone();
                    async move {
                        termior_store::measure_agent_disk_usage(&data_dir)
                            .map(|usage| usage.total())
                            .unwrap_or(u64::MAX)
                    }
                })
                .await;
            let claimed = view.update(cx, |view, cx| view.claim_automation_runs(used, cx));
            let Ok(claimed) = claimed else {
                break;
            };
            for run in claimed {
                let request = request.clone();
                let view = view.clone();
                cx.spawn(async move |cx| {
                    let finished = cx
                        .background_executor()
                        .spawn(async move { execute_claimed_run(run, request).await })
                        .await;
                    let _ = view.update(cx, |view, cx| view.finish_automation_run(finished, cx));
                })
                .detach();
            }
        })
        .detach();
    }

    fn automation_tick_request(&self) -> Option<AutomationTickRequest> {
        let data_dir = self.data_dir.clone()?;
        let runtime = self.orchestrator.runtime.as_ref();
        Some(AutomationTickRequest {
            data_dir,
            project_dir: self.workspace_root.clone(),
            provider: runtime.map(|runtime| runtime.config.clone()),
            tools: runtime.map(|runtime| runtime.tools.clone()),
        })
    }

    fn automation_global_concurrency(&self) -> u16 {
        self.automations
            .automations
            .iter()
            .filter(|automation| automation.enabled)
            .fold(0u16, |total, automation| {
                total.saturating_add(automation.template.max_concurrency)
            })
            .max(1)
    }

    fn claim_automation_runs(
        &mut self,
        disk_used: u64,
        cx: &mut Context<Self>,
    ) -> Vec<AutomationRun> {
        self.automation_disk_used = Some(disk_used);
        let Some(data_dir) = self.data_dir.clone() else {
            return Vec::new();
        };
        let mut manager = match AutomationManager::load(&data_dir) {
            Ok(manager) => manager,
            Err(error) => {
                log::warn!("automation host load failed: {error}");
                return Vec::new();
            }
        };
        let now = unix_now();
        if let Err(error) = manager.poll_due(now) {
            log::warn!("automation schedule poll failed: {error}");
        }
        let claimed = match manager.claim_queued(
            now,
            HostLimits {
                max_concurrency: self.automation_global_concurrency(),
                disk_budget_bytes: self.automation_disk_budget,
                disk_used_bytes: disk_used,
            },
        ) {
            Ok(claimed) => claimed,
            Err(error) => {
                log::warn!("automation claim failed: {error}");
                Vec::new()
            }
        };
        self.automations = manager.store;
        cx.notify();
        claimed
    }

    fn finish_automation_run(
        &mut self,
        finished: (String, ExecutionOutcome),
        cx: &mut Context<Self>,
    ) {
        let (run_id, outcome) = finished;
        let Some(data_dir) = self.data_dir.clone() else {
            return;
        };
        let mut manager = match AutomationManager::load(&data_dir) {
            Ok(manager) => manager,
            Err(error) => {
                log::warn!("automation host load failed: {error}");
                return;
            }
        };
        match manager.complete_run(&run_id, outcome, unix_now()) {
            Ok(effect) => {
                self.automations = manager.store;
                self.present_automation_effect(&effect, cx);
            }
            Err(error) => log::warn!("automation completion failed: {error}"),
        }
        cx.notify();
    }

    /// Approval waits update the admin list and, when the policy says so, a notification.
    /// They never install the composer approval card and never approve the call.
    fn present_automation_effect(&mut self, effect: &RunEffect, cx: &mut Context<Self>) {
        let policy = self
            .automations
            .automations
            .iter()
            .find(|automation| automation.id == effect.automation_id)
            .map(|automation| automation.notification)
            .unwrap_or(NotificationPolicy::Actionable);
        if effect.approval_wait {
            let presentation = approval_presentation(policy);
            if presentation.notify && !presentation.open_modal && !presentation.auto_approve {
                self.set_status(tf!(
                    "composer.automation.approval_waiting",
                    "id" => effect.automation_id.clone(),
                    "run" => effect.run_id.clone()
                ));
                cx.emit(AgentStatus::Attention);
            }
            return;
        }
        if !effect.notify {
            return;
        }
        self.set_status(tf!(
            "composer.automation.host_status",
            "id" => effect.automation_id.clone(),
            "status" => format!("{:?}", effect.status),
            "run" => effect.run_id.clone()
        ));
        let status = match effect.status {
            RunStatus::Completed => AgentStatus::Finished,
            RunStatus::Failed => AgentStatus::Error,
            _ => AgentStatus::Attention,
        };
        cx.emit(status);
    }

    pub(super) fn apply_memory_action(
        &mut self,
        id: &str,
        action: MemoryAction,
        cx: &mut Context<Self>,
    ) {
        let result = match action {
            MemoryAction::Accept => self.memory.accept(id),
            MemoryAction::Reject => {
                self.memory.reject(id);
                Ok(())
            }
            MemoryAction::TogglePause => {
                let paused = self
                    .memory
                    .entries
                    .get(id)
                    .is_some_and(|entry| entry.enabled);
                self.memory.pause(id, paused)
            }
            MemoryAction::Delete => {
                self.memory.delete(id);
                Ok(())
            }
        };
        if let Err(error) = result {
            self.set_status(tf!("composer.memory.action_failed", "error" => error));
            cx.notify();
            return;
        }
        if let Some(data_dir) = self.data_dir.as_ref() {
            if let Err(error) = self.memory.persist(&data_dir.join("agent-memory.json")) {
                self.set_status(tf!("composer.memory.persist_failed", "error" => error));
                cx.notify();
                return;
            }
        }
        self.set_status(tf!("composer.memory.updated", "id" => id));
        cx.notify();
    }

    /// 用户在面板里勾选/删除 TODO：先落盘再更新共享状态，与 `todo_write` 的失败语义一致。
    pub(super) fn update_todos(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut TodoStore) -> bool,
    ) {
        let todos = self.todos.clone();
        let mut store = todos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = store.clone();
        if !change(&mut next) {
            return;
        }
        if let Some(dir) = &self.data_dir {
            if let Err(error) = DataFiles::new(dir.clone()).todos::<TodoStore>().save(&next) {
                self.set_status(tf!("composer.todo.save_failed", "error" => error));
                cx.notify();
                return;
            }
        }
        *store = next;
        cx.notify();
    }

    pub(super) fn delete_snippet(&mut self, handle: &str, cx: &mut Context<Self>) {
        if self.snippets.remove(handle) {
            self.set_status(tf!("composer.snippet.deleted", "handle" => handle));
            self.persist_snippets();
        }
        cx.notify();
    }

    /// 任一检查面板展开时都不能收缩成 compact，否则面板被裁掉（`/skills` 等在空会话中也要可见）。
    pub(super) fn any_center_open(&self) -> bool {
        self.open_center.is_some()
    }

    pub(super) fn center_open(&self, center: ComposerCenter) -> bool {
        self.open_center == Some(center)
    }

    /// 打开某个面板时收起其它面板；再次触发同一面板则收起。
    pub(super) fn toggle_center(&mut self, center: ComposerCenter) {
        self.open_center = (!self.center_open(center)).then_some(center);
    }

    pub(super) fn render_todo_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let todo_center = self.center_open(ComposerCenter::Todos).then(|| {
            let todos = self
                .todos
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let open = todos.open_count();
            let rows = todos.items.into_iter().enumerate().map(|(index, item)| {
                let toggle_id = item.id.clone();
                let delete_id = item.id.clone();
                let done = item.done;
                div()
                    .id(SharedString::from(format!("todo-entry-{index}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .id(SharedString::from(format!("toggle-todo-{index}")))
                            .cursor_pointer()
                            .child(SharedString::from(if done { "☑" } else { "☐" }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let id = toggle_id.clone();
                                    this.update_todos(cx, |store| store.set_done(&id, !done));
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .when(done, |text| {
                                text.line_through().text_color(crate::ui::muted(p))
                            })
                            .child(SharedString::from(item.text)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("delete-todo-{index}")))
                            .px_2()
                            .py(px(2.0))
                            .rounded_md()
                            .border_1()
                            .border_color(crate::ui::color(p.status[3]))
                            .text_xs()
                            .cursor_pointer()
                            .child(t!("composer.todo.delete"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let id = delete_id.clone();
                                    this.update_todos(cx, |store| store.remove(&id));
                                }),
                            ),
                    )
            });
            div()
                .id("todo-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tf!("composer.todo.title", "open" => open)),
                )
                .when(rows.len() == 0, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(t!("composer.todo.empty")),
                    )
                })
                .children(rows)
        });
        todo_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_snippet_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let snippet_center = self.center_open(ComposerCenter::Snippets).then(|| {
            let rows = self
                .snippets
                .snippets
                .iter()
                .enumerate()
                .map(|(index, snippet)| {
                    let handle = snippet.handle.clone();
                    div()
                        .id(SharedString::from(format!("snippet-entry-{index}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("#{}", snippet.handle))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(crate::ui::muted(p))
                                .child(SharedString::from(snippet_preview(&snippet.body))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("delete-snippet-{index}")))
                                .px_2()
                                .py(px(2.0))
                                .rounded_md()
                                .border_1()
                                .border_color(crate::ui::color(p.status[3]))
                                .text_xs()
                                .cursor_pointer()
                                .child(t!("composer.snippet.delete"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.delete_snippet(&handle, cx);
                                    }),
                                ),
                        )
                });
            div()
                .id("snippet-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.snippet.title")),
                )
                .when(self.snippets.snippets.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(t!("composer.snippet.empty")),
                    )
                })
                .children(rows)
        });
        snippet_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_context_inspector(
        &mut self,
        p: &ResolvedPalette,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let context_inspector = self
            .center_open(ComposerCenter::ContextInspector)
            .then_some(self.last_context_plan.as_ref())
            .flatten()
            .map(|plan| {
                let rows = plan.entries.iter().enumerate().map(|(index, entry)| {
                    div()
                        .id(SharedString::from(format!("context-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(SharedString::from(entry.item_id.clone()))
                                .child(SharedString::from(format!(
                                    "{:?} · {}t",
                                    entry.disposition, entry.tokens
                                ))),
                        )
                        .child(div().text_xs().text_color(crate::ui::muted(p)).child(
                            SharedString::from(format!("{:?} · {}", entry.category, entry.reason)),
                        ))
                });
                div()
                    .id("context-inspector-panel")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t!("composer.context_inspector"))
                            .child(tf!(
                                "composer.context_tokens",
                                "used" => plan.total_tokens,
                                "limit" => plan.hard_limit
                            )),
                    )
                    .children(rows)
            });
        context_inspector.map(IntoElement::into_any_element)
    }

    pub(super) fn render_recovery_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let recovery_center = self.center_open(ComposerCenter::Recovery).then(|| {
            let rows = self.recovery_tasks.iter().enumerate().map(|(index, task)| {
                let retry_task_id = task.task_id.clone();
                let abandon_task_id = task.task_id.clone();
                let can_retry = task
                    .available_actions
                    .iter()
                    .any(|action| action == "retry");
                let can_abandon = task
                    .available_actions
                    .iter()
                    .any(|action| action == "abandon");
                div()
                    .id(SharedString::from(format!("recovery-task-{index}")))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(SharedString::from(format!(
                                "{} · {}",
                                task.task_id, task.attempt_id
                            )))
                            .child(tf!(
                                "composer.recovery.attempt_state",
                                "state" => format!("{:?}", task.recovered_state),
                                "event" => task.last_trusted_event
                            )),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                        "composer.recovery.task_detail",
                        "persisted" => task.persisted_state,
                        "backend" => task
                            .backend_id
                            .clone()
                            .unwrap_or_else(|| t!("composer.recovery.unknown_backend").to_string()),
                        "actions" => task.available_actions.join(", ")
                    )))
                    .when_some(task.diagnostic.clone(), |row, diagnostic| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::color(p.status[3]))
                                .child(SharedString::from(diagnostic)),
                        )
                    })
                    .when(can_retry || can_abandon, |row| {
                        row.child(
                            div()
                                .flex()
                                .gap_1()
                                .when(can_retry, |actions| {
                                    actions.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "recovery-retry-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.recovery.prepare_retry"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_recovery_command(
                                                        &retry_task_id,
                                                        RecoveryCommand::Retry,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                })
                                .when(can_abandon, |actions| {
                                    actions.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "recovery-abandon-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::color(p.status[3]))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.recovery.abandon"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_recovery_command(
                                                        &abandon_task_id,
                                                        RecoveryCommand::Abandon,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                }),
                        )
                    })
            });
            div()
                .id("recovery-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.recovery.title")),
                )
                .when(self.recovery_tasks.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(t!("composer.recovery.empty")),
                    )
                })
                .children(rows)
        });
        recovery_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_automation_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let automation_center = self.center_open(ComposerCenter::Automations).then(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let disk_label = self.automation_disk_label();
            let store = self.automations.clone();
            let rows = store
                .automations
                .iter()
                .enumerate()
                .map(|(index, automation)| {
                    let automation_id = automation.id.clone();
                    let summary = admin_summary(&store, &automation.id, now);
                    let next = summary
                        .as_ref()
                        .and_then(|summary| summary.next_run_at)
                        .map(|timestamp| timestamp.to_string())
                        .unwrap_or_else(|| "event/manual".into());
                    let last = summary
                        .as_ref()
                        .and_then(|summary| summary.last_runs.first())
                        .map(|run| format!("{:?} ({})", run.status, run.run_id))
                        .unwrap_or_else(|| "never".into());
                    let missed = summary.as_ref().map(|summary| summary.missed).unwrap_or(0);
                    let coalesced = summary
                        .as_ref()
                        .map(|summary| summary.coalesced)
                        .unwrap_or(0);
                    let duplicate = summary
                        .as_ref()
                        .map(|summary| summary.duplicate)
                        .unwrap_or(0);
                    let last_runs = summary
                        .as_ref()
                        .map(|summary| summary.last_runs.clone())
                        .unwrap_or_default();
                    div()
                        .id(SharedString::from(format!("automation-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(SharedString::from(automation.name.clone()))
                                .child(if automation.enabled {
                                    t!("composer.automation.enabled")
                                } else {
                                    t!("composer.automation.disabled")
                                }),
                        )
                        .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                            "composer.automation.detail",
                            "version" => automation.template.version,
                            "next" => next,
                            "last" => last,
                            "trigger" => format!("{:?}", automation.trigger)
                        )))
                        .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                            "composer.automation.counts",
                            "missed" => missed,
                            "coalesced" => coalesced,
                            "duplicate" => duplicate
                        )))
                        .child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(p))
                                .child(disk_label.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(p))
                                .child(t!("composer.automation.last_title")),
                        )
                        .children(last_runs.iter().enumerate().map(|(run_index, run)| {
                            let reason = if run.schedule_reason.is_empty() {
                                "—".to_owned()
                            } else {
                                run.schedule_reason.clone()
                            };
                            div()
                                .id(SharedString::from(format!(
                                    "automation-run-{index}-{run_index}"
                                )))
                                .text_xs()
                                .text_color(crate::ui::muted(p))
                                .child(tf!(
                                    "composer.automation.run_line",
                                    "status" => format!("{:?}", run.status),
                                    "run" => run.run_id.clone(),
                                    "reason" => reason
                                ))
                        }))
                        .when(automation.enabled, |row| {
                            row.child(
                                div()
                                    .id(SharedString::from(format!("run-automation-{index}")))
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(crate::ui::border(p))
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(t!("composer.automation.queue_run"))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.trigger_automation(&automation_id, cx);
                                        }),
                                    ),
                            )
                        })
                });
            div()
                .id("automation-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.automation.title")),
                )
                .when(self.automations.automations.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(t!("composer.automation.empty")),
                    )
                })
                .children(rows)
        });
        automation_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_checkpoint_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let checkpoint_center = self.center_open(ComposerCenter::Checkpoints).then(|| {
            let rows = self
                .checkpoints
                .iter()
                .enumerate()
                .map(|(index, checkpoint)| {
                    let checkpoint_id = checkpoint.id.clone();
                    let files = checkpoint
                        .files
                        .iter()
                        .map(|file| file.relative_path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    div()
                        .id(SharedString::from(format!("checkpoint-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(SharedString::from(checkpoint.id.clone()))
                                .child(tf!(
                                    "composer.checkpoint.meta",
                                    "task" => checkpoint.task_id,
                                    "event" => checkpoint.sequence
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(p))
                                .child(tf!("composer.checkpoint.files", "files" => files)),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("restore-checkpoint-{index}")))
                                .px_2()
                                .py(px(2.0))
                                .rounded_md()
                                .border_1()
                                .border_color(crate::ui::border(p))
                                .text_xs()
                                .cursor_pointer()
                                .child(t!("composer.checkpoint.restore"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.restore_checkpoint(&checkpoint_id, cx);
                                    }),
                                ),
                        )
                });
            div()
                .id("checkpoint-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.checkpoint.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(p))
                        .child(t!("composer.checkpoint.note")),
                )
                .children(rows)
        });
        checkpoint_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_skill_center(
        &mut self,
        p: &ResolvedPalette,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let skill_center = self.center_open(ComposerCenter::Skills).then(|| {
            let rows = self.skills.iter().enumerate().map(|(index, skill)| {
                let active = self.active_skills.iter().any(|name| name == &skill.name);
                div()
                    .id(SharedString::from(format!("skill-entry-{index}")))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(SharedString::from(format!("${}", skill.name)))
                            .child(if active {
                                t!("composer.skills.active")
                            } else {
                                t!("composer.skills.available")
                            }),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                        "composer.skills.detail",
                        "description" => skill.description,
                        "source" => skill.path.display(),
                        "tools" => if skill.allowed_tools.is_empty() {
                            t!("composer.skills.task_tool_set").to_string()
                        } else {
                            skill
                                .allowed_tools
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    )))
            });
            div()
                .id("skill-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.skills.title")),
                )
                .when_some(self.skill_scan_error.clone(), |panel, error| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::color(p.status[3]))
                            .child(SharedString::from(error)),
                    )
                })
                .children(rows)
        });
        skill_center.map(IntoElement::into_any_element)
    }

    pub(super) fn render_memory_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let memory_center = self.center_open(ComposerCenter::Memory).then(|| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let candidates =
                self.memory
                    .candidates
                    .values()
                    .enumerate()
                    .map(|(index, candidate)| {
                        let accept_id = candidate.id.clone();
                        let reject_id = candidate.id.clone();
                        div()
                            .id(SharedString::from(format!("memory-candidate-{index}")))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .p_2()
                            .rounded_md()
                            .bg(crate::ui::alpha(p.status[2], 0.08))
                            .child(tf!(
                                "composer.memory.candidate",
                                "id" => candidate.id,
                                "confidence" => candidate.confidence
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .child(SharedString::from(candidate.content.clone())),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "accept-memory-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.memory.accept"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_memory_action(
                                                        &accept_id,
                                                        MemoryAction::Accept,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "reject-memory-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::color(p.status[3]))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.memory.reject"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_memory_action(
                                                        &reject_id,
                                                        MemoryAction::Reject,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    ),
                            )
                    });
            let entries = self
                .memory
                .entries
                .values()
                .enumerate()
                .map(|(index, entry)| {
                    let toggle_id = entry.id.clone();
                    let delete_id = entry.id.clone();
                    let status = self
                        .memory
                        .status(&entry.id, now_ms)
                        .unwrap_or(MemoryStatus::Paused);
                    div()
                        .id(SharedString::from(format!("memory-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(SharedString::from(format!("{} · {:?}", entry.id, status)))
                        .child(
                            div()
                                .text_xs()
                                .child(SharedString::from(entry.content.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("toggle-memory-{index}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::border(p))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(if entry.enabled {
                                            t!("composer.memory.pause")
                                        } else {
                                            t!("composer.memory.resume")
                                        })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.apply_memory_action(
                                                    &toggle_id,
                                                    MemoryAction::TogglePause,
                                                    cx,
                                                );
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("delete-memory-{index}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::color(p.status[3]))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(t!("composer.memory.delete"))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.apply_memory_action(
                                                    &delete_id,
                                                    MemoryAction::Delete,
                                                    cx,
                                                );
                                            }),
                                        ),
                                ),
                        )
                });
            div()
                .id("memory-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.memory.title")),
                )
                .children(candidates)
                .children(entries)
        });
        memory_center.map(IntoElement::into_any_element)
    }
}

#[derive(Clone)]
struct AutomationTickRequest {
    data_dir: PathBuf,
    project_dir: PathBuf,
    provider: Option<ProviderConfig>,
    tools: Option<ToolRegistry>,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl ComposerView {
    fn automation_disk_label(&self) -> SharedString {
        match self.automation_disk_used {
            Some(used) if self.automation_disk_budget > 0 && used < u64::MAX => tf!(
                "composer.automation.disk",
                "used" => used / (1024 * 1024),
                "budget" => self.automation_disk_budget / (1024 * 1024)
            ),
            _ => t!("composer.automation.disk_unavailable"),
        }
    }
}

async fn execute_claimed_run(
    run: AutomationRun,
    request: AutomationTickRequest,
) -> (String, ExecutionOutcome) {
    let unavailable = |detail: String| {
        (
            run.run_id.clone(),
            ExecutionOutcome::Failed {
                class: FailureClass::Permanent,
                idempotent: true,
                result_known: true,
                detail,
            },
        )
    };
    let Some(config) = request.provider else {
        return unavailable("model provider is not configured".into());
    };
    let Some(tools) = request.tools else {
        return unavailable("automation tools are not configured".into());
    };
    let provider = match HttpProvider::new(config) {
        Ok(provider) => provider,
        Err(error) => return unavailable(error.to_string()),
    };
    let executor = match ToolExecutor::new(&request.project_dir, tools) {
        Ok(executor) => executor,
        Err(error) => return unavailable(error.to_string()),
    };
    let outcome = drive_automation_run(
        &run,
        &request.project_dir,
        Some(&request.data_dir),
        &provider,
        &executor,
    )
    .await;
    (run.run_id, outcome)
}
