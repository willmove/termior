//! 工具审批、Codex 权限勾选、编辑审阅与计划确认。

use super::*;

impl ComposerView {
    pub fn apply_reviewed_edit(
        &mut self,
        proposal_id: &str,
        accepted: &[usize],
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if !self.orchestrator.take_matching_edit(proposal_id) {
            return Err(t!("composer.edit.no_longer_pending").to_string());
        }
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: accepted.to_vec(),
            },
            t!("composer.edit.applying"),
            cx,
        );
        Ok(t!("composer.edit.applied").to_string())
    }

    pub fn reject_reviewed_edit(
        &mut self,
        proposal_id: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if !self.orchestrator.take_matching_edit(proposal_id) {
            return Err(t!("composer.edit.no_longer_pending").to_string());
        }
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: Vec::new(),
            },
            t!("composer.edit.returning_rejection"),
            cx,
        );
        Ok(t!("composer.edit.rejected").to_string())
    }

    /// Yolo 即 Agent 完全自主：挂起中的工具审批直接批准、变更审阅直接接受全部 hunk。
    /// 正常 Yolo 任务在运行时内部就已自动放行；这里兜底任务在 Auto 下挂起后才切到 Yolo 的情形。
    pub(super) fn auto_resolve_in_yolo(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Yolo {
            return;
        }
        match self.orchestrator.auto_resolve() {
            AutoResolve::None => {}
            AutoResolve::Tool => self.approve_tool(cx),
            AutoResolve::Edit { id, hunks } => {
                let _ = self.apply_reviewed_edit(&id, &hunks, cx);
            }
        }
    }

    pub(super) fn approval_policy(&self) -> ApprovalPolicy {
        if self.mode == Mode::Yolo {
            ApprovalPolicy::Yolo
        } else {
            ApprovalPolicy::Prompt
        }
    }

    pub(super) fn persist_task_summary(&self, runtime: &TaskRuntime) {
        let Some(dir) = self.data_dir.as_deref() else {
            return;
        };
        let mut store = TaskSummaryStore::load(dir).unwrap_or_default();
        let sequence = runtime.events().last().map(|event| event.sequence);
        store.upsert(TaskSummary::from_task(runtime.task(), sequence));
        if let Err(error) = store.persist(dir) {
            log::warn!("failed to persist task summary: {error}");
        }
    }

    pub(super) fn toggle_permission(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.orchestrator.toggle_permission(index) {
            cx.notify();
        }
    }

    pub(super) fn approve_tool(&mut self, cx: &mut Context<Self>) {
        match self.orchestrator.take_approval(true, true) {
            ApprovalAction::None => {}
            ApprovalAction::External(command) => self.dispatch_external_command(
                command,
                t!("composer.status.returning_approval_codex"),
                cx,
            ),
            ApprovalAction::Builtin {
                call_id,
                tool_name,
                approved,
            } => {
                let label = tf!("composer.status.running_tool", "tool" => tool_name);
                self.dispatch_task_command(
                    TaskCommand::ResolveApproval { call_id, approved },
                    label,
                    cx,
                );
            }
        }
    }

    pub(super) fn reject_tool(&mut self, cx: &mut Context<Self>) {
        match self.orchestrator.take_approval(false, false) {
            ApprovalAction::None => {}
            ApprovalAction::External(command) => self.dispatch_external_command(
                command,
                t!("composer.status.returning_rejection_codex"),
                cx,
            ),
            ApprovalAction::Builtin {
                call_id, approved, ..
            } => {
                self.dispatch_task_command(
                    TaskCommand::ResolveApproval { call_id, approved },
                    t!("composer.status.returning_rejection"),
                    cx,
                );
            }
        }
    }

    pub(super) fn decide_hunk(&mut self, id: usize, accept: bool, cx: &mut Context<Self>) {
        if self.orchestrator.decide_hunk(id, accept) {
            cx.notify();
        }
    }

    pub(super) fn apply_edit(&mut self, cx: &mut Context<Self>) {
        match self.orchestrator.prepared_edit() {
            PreparedEdit::Missing => {}
            PreparedEdit::Incomplete => {
                self.set_status(t!("composer.status.decide_hunks"));
                cx.notify();
            }
            PreparedEdit::Ready {
                proposal_id,
                accepted,
            } => {
                let _ = self.apply_reviewed_edit(&proposal_id, &accepted, cx);
            }
        }
    }

    pub(super) fn reject_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(proposal_id) = self.orchestrator.pending_edit_id() {
            let _ = self.reject_reviewed_edit(&proposal_id, cx);
        }
    }

    pub(super) fn confirm_plan(&mut self, cx: &mut Context<Self>) {
        if !self.awaiting_plan_confirmation || self.orchestrator.busy {
            return;
        }
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = true;
        self.draft.input = "Execute the approved plan now. Follow each listed step and use the normal approval workflow for every write or command.".into();
        self.cursor = self.draft.input.chars().count();
        self.submit(cx);
    }

    pub(super) fn reject_plan(&mut self, cx: &mut Context<Self>) {
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = false;
        self.history.push(Message::assistant(
            "The proposed plan was rejected. No write-capable tool was exposed.",
        ));
        self.set_status(t!("composer.plan.rejected"));
        cx.emit(AgentStatus::Finished);
        self.persist_session();
        cx.notify();
    }

    pub(super) fn render_approval_card(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let approval_model = if let Some(pending) = self.orchestrator.pending_approval.as_ref() {
            Some(ApprovalCardModel {
                headline: tf!(
                    "composer.approval.headline",
                    "summary" => pending.request.summary,
                    "call" => pending.request.call_id,
                    "effect" => format!("{:?}", pending.contract.side_effect)
                ),
                arguments: pending.request.arguments.clone(),
                show_arguments: true,
                scope: None,
                choices: Vec::new(),
            })
        } else {
            self.orchestrator
                .external_pending_approval
                .as_ref()
                .map(|pending| {
                    let permissions_request = pending.is_permissions();
                    ApprovalCardModel {
                        headline: tf!(
                            "composer.approval.codex_headline",
                            "kind" => pending.request_kind,
                            "request" => pending.backend_request_id,
                            "call" => pending.tool_call_id
                        ),
                        show_arguments: !permissions_request || pending.permissions.is_empty(),
                        arguments: pending.arguments.clone(),
                        scope: permissions_request.then(|| pending.scope.clone()),
                        choices: pending
                            .permissions
                            .iter()
                            .enumerate()
                            .map(|(index, choice)| (index, choice.label.clone(), choice.checked))
                            .collect(),
                    }
                })
        };
        let approval = approval_model.map(|model| {
            let scope_row = model.scope.as_ref().map(|scope| {
                let shown: SharedString = if scope.is_empty() {
                    t!("composer.approval.scope_unspecified")
                } else {
                    SharedString::from(scope.clone())
                };
                let scope_id = scope_element_id(scope);
                let scope_debug = scope_id.clone();
                div()
                    .id(SharedString::from(scope_id))
                    .debug_selector(move || scope_debug.clone())
                    .text_xs()
                    .text_color(crate::ui::muted(p))
                    .child(tf!("composer.approval.permission_scope", "scope" => shown))
            });
            let choice_rows = model.choices.into_iter().map(|(index, label, checked)| {
                let choice_id = format!("permission-choice-{index}");
                let choice_debug = choice_id.clone();
                div()
                    .id(SharedString::from(choice_id))
                    .debug_selector(move || choice_debug.clone())
                    .flex()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(if checked {
                        crate::ui::alpha(p.status[1], 0.18)
                    } else {
                        crate::ui::color(p.elevated)
                    })
                    .child(SharedString::from(format!(
                        "{}  {label}",
                        if checked { "☑" } else { "☐" }
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_permission(index, cx);
                        }),
                    )
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.status[2]))
                .bg(crate::ui::alpha(p.status[2], 0.08))
                .child(model.headline)
                .child(self.capability_row("composer-approval-capabilities", p))
                .children(scope_row)
                .children(choice_rows)
                .when(model.show_arguments, |card| {
                    card.child(div().text_xs().child(SharedString::from(model.arguments)))
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("approve-tool")
                                .debug_selector(|| "approve-tool".into())
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[1]))
                                .text_color(crate::ui::on_color(p.status[1]))
                                .cursor_pointer()
                                .child(t!("composer.approval.approve"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.approve_tool(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-tool")
                                .debug_selector(|| "reject-tool".into())
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[3]))
                                .text_color(crate::ui::on_color(p.status[3]))
                                .cursor_pointer()
                                .child(t!("composer.approval.reject"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_tool(cx)),
                                ),
                        ),
                )
        });
        approval.map(IntoElement::into_any_element)
    }

    pub(super) fn render_edit_review(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let edit_review = self.orchestrator.pending_edit.as_ref().map(|edit| {
            let hunk_buttons = edit.summary.hunk_ids.iter().flat_map(|id| {
                let accept_id = *id;
                let reject_id = *id;
                let decision = edit.decisions.get(id).copied();
                [
                    div()
                        .id(SharedString::from(format!("accept-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(true) {
                            crate::ui::color(p.status[1])
                        } else {
                            crate::ui::color(p.elevated)
                        })
                        .text_color(if decision == Some(true) {
                            crate::ui::on_color(p.status[1])
                        } else {
                            crate::ui::color(p.foreground)
                        })
                        .cursor_pointer()
                        .child(tf!("composer.edit.accept_hunk", "id" => id))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(accept_id, true, cx)
                            }),
                        ),
                    div()
                        .id(SharedString::from(format!("reject-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(false) {
                            crate::ui::color(p.status[3])
                        } else {
                            crate::ui::color(p.elevated)
                        })
                        .text_color(if decision == Some(false) {
                            crate::ui::on_color(p.status[3])
                        } else {
                            crate::ui::color(p.foreground)
                        })
                        .cursor_pointer()
                        .child(tf!("composer.edit.reject_hunk", "id" => id))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(reject_id, false, cx)
                            }),
                        ),
                ]
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .child(tf!("composer.edit.headline", "path" => edit.summary.path))
                .child(
                    div()
                        .max_h(px(70.0))
                        .overflow_hidden()
                        .text_xs()
                        .child(SharedString::from(edit.summary.patch.clone())),
                )
                .child(div().flex().flex_wrap().gap_1().children(hunk_buttons))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("apply-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[1]))
                                .text_color(crate::ui::on_color(p.status[1]))
                                .cursor_pointer()
                                .child(t!("composer.edit.apply"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.apply_edit(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[3]))
                                .text_color(crate::ui::on_color(p.status[3]))
                                .cursor_pointer()
                                .child(t!("composer.edit.reject_all"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_edit(cx)),
                                ),
                        ),
                )
        });
        edit_review.map(IntoElement::into_any_element)
    }

    pub(super) fn render_budget_resume(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let budget_resume = self
            .orchestrator
            .active_task
            .as_ref()
            .is_some_and(|runtime| runtime.task().state == TaskState::WaitingUser)
            .then(|| {
                div()
                    .mx_3()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::color(p.status[2]))
                    .child(t!("composer.budget.reached"))
                    .child(
                        div()
                            .id("continue-agent-budget")
                            .mt_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(crate::ui::color(p.accent))
                            .text_color(crate::ui::on_color(p.accent))
                            .cursor_pointer()
                            .child(t!("composer.budget.continue"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.continue_with_more_budget(cx)),
                            ),
                    )
            });
        budget_resume.map(IntoElement::into_any_element)
    }

    pub(super) fn render_plan_review(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let plan_review = self.awaiting_plan_confirmation.then(|| {
            div()
                .flex()
                .gap_2()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .child(t!("composer.plan.awaiting"))
                .child(
                    div()
                        .id("confirm-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui::color(p.status[1]))
                        .text_color(crate::ui::on_color(p.status[1]))
                        .cursor_pointer()
                        .child(t!("composer.plan.confirm"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.confirm_plan(cx)),
                        ),
                )
                .child(
                    div()
                        .id("reject-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui::color(p.status[3]))
                        .text_color(crate::ui::on_color(p.status[3]))
                        .cursor_pointer()
                        .child(t!("composer.plan.reject"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.reject_plan(cx)),
                        ),
                )
        });
        plan_review.map(IntoElement::into_any_element)
    }
}
