//! Composer 任务树 / DAG：后端、环境、依赖、状态、预算、验证、排队原因与用量。

use gpui::{div, px, FontWeight, MouseButton, SharedString};
use termior_ai::orchestration::{
    can_delete_task_record, format_task_usage, NodeState, TaskResourceUsage, TaskTreeNode,
    TaskTreeSnapshot,
};
use termior_ai::RuntimeBudgets;
use termior_theme::ResolvedPalette;

use super::*;

impl ComposerView {
    /// Replaces the DAG shown in Composer. The desktop host publishes snapshots here.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn set_task_tree(&mut self, snapshot: TaskTreeSnapshot) {
        let selected = self.task_tree_selected.clone();
        self.task_tree_selected =
            selected.filter(|task_id| snapshot.nodes.iter().any(|node| node.task_id == *task_id));
        self.task_tree = snapshot;
    }

    pub(super) fn adjust_task_tree_concurrency(&mut self, delta: i32) {
        let next = (self.task_tree.concurrency_cap as i32)
            .saturating_add(delta)
            .clamp(1, 64) as usize;
        self.task_tree.set_concurrency_cap(next);
    }

    pub(super) fn select_task_tree_node(&mut self, task_id: &str) {
        if self
            .task_tree
            .nodes
            .iter()
            .any(|node| node.task_id == task_id)
        {
            self.task_tree_selected = Some(task_id.to_owned());
        }
    }

    pub(super) fn cancel_task_tree_child(&mut self, task_id: &str) -> bool {
        match self.task_tree.cancel_child(task_id) {
            Ok(()) => true,
            Err(_) => {
                self.set_status(t!("composer.task_tree.missing"));
                false
            }
        }
    }

    pub(super) fn cancel_task_tree_all(&mut self) {
        self.task_tree.cancel_all();
    }

    /// Returns false when the record must stay, including unknown side effects.
    pub(super) fn delete_task_tree_record(&mut self, task_id: &str) -> bool {
        match self.task_tree.delete_record(task_id) {
            Ok(()) => {
                if self.task_tree_selected.as_deref() == Some(task_id) {
                    self.task_tree_selected = None;
                }
                true
            }
            Err(_) => {
                self.set_status(t!("composer.task_tree.delete_blocked"));
                false
            }
        }
    }

    pub(super) fn task_tree_lines(&self) -> Vec<SharedString> {
        let mut lines = vec![tf!(
            "composer.task_tree.concurrency",
            "cap" => self.task_tree.concurrency_cap
        )];
        if self.task_tree.nodes.is_empty() {
            lines.push(t!("composer.task_tree.empty"));
            return lines;
        }
        lines.push(tf!(
            "composer.task_tree.aggregate",
            "usage" => usage_line(&self.task_tree.aggregate)
        ));
        for rollup in &self.task_tree.backends {
            lines.push(tf!(
                "composer.task_tree.backend",
                "provider" => rollup.provider_id.clone(),
                "usage" => usage_line(&rollup.usage)
            ));
        }
        for node in &self.task_tree.nodes {
            lines.extend(node_lines(node));
        }
        if let Some(task_id) = &self.task_tree_selected {
            if let Some(node) = self
                .task_tree
                .nodes
                .iter()
                .find(|node| &node.task_id == task_id)
            {
                lines.push(tf!("composer.task_tree.detail_title", "id" => node.task_id.clone()));
                lines.extend(node_lines(node));
            }
        }
        lines
    }

    pub(super) fn render_task_tree_center(
        &mut self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel =
            self.center_open(ComposerCenter::TaskTree).then(|| {
                let lines = self.task_tree_lines();
                let nodes = self.task_tree.nodes.clone();
                let selected = self.task_tree_selected.clone();
                let cap = self.task_tree.concurrency_cap;
                div()
                    .id("task-tree-panel")
                    .debug_selector(|| "task-tree-panel".into())
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
                            .items_center()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(t!("composer.task_tree.title")),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id("task-tree-cap-dec")
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.task_tree.concurrency_dec"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.adjust_task_tree_concurrency(-1);
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                    .child(
                                        div().text_xs().child(
                                            tf!("composer.task_tree.concurrency", "cap" => cap),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .id("task-tree-cap-inc")
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(t!("composer.task_tree.concurrency_inc"))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.adjust_task_tree_concurrency(1);
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                    .when(!nodes.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .id("task-tree-cancel-all")
                                                .px_2()
                                                .py(px(2.0))
                                                .rounded_md()
                                                .border_1()
                                                .border_color(crate::ui::border(p))
                                                .text_xs()
                                                .cursor_pointer()
                                                .child(t!("composer.task_tree.cancel_all"))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(|this, _, _, cx| {
                                                        cx.stop_propagation();
                                                        this.cancel_task_tree_all();
                                                        cx.notify();
                                                    }),
                                                ),
                                        )
                                    }),
                            ),
                    )
                    .children(lines.into_iter().enumerate().map(|(index, line)| {
                        div()
                            .id(SharedString::from(format!("task-tree-line-{index}")))
                            .text_xs()
                            .text_color(crate::ui::muted(p))
                            .child(line)
                    }))
                    .children(nodes.into_iter().enumerate().map(|(index, node)| {
                        task_actions(p, cx, index, &node, selected.as_deref())
                    }))
            });
        panel.map(IntoElement::into_any_element)
    }
}

fn usage_line(usage: &TaskResourceUsage) -> String {
    format_task_usage(usage)
}

fn node_lines(node: &TaskTreeNode) -> Vec<SharedString> {
    let mut lines = vec![tf!(
        "composer.task_tree.node",
        "id" => node.task_id.clone(),
        "state" => state_label(node.state).to_string(),
        "backend" => format!("{}/{}", node.agent_id, node.provider_id),
        "environment" => node.environment_id.clone()
    )];
    if node.dependencies.is_empty() {
        lines.push(t!("composer.task_tree.deps_none"));
    } else {
        lines.push(tf!(
            "composer.task_tree.deps",
            "deps" => node.dependencies.join(", ")
        ));
    }
    lines.push(budget_line(&node.budgets));
    lines.push(tf!(
        "composer.task_tree.verification",
        "status" => verification_label(node.result_verified).to_string()
    ));
    if node.blockers.is_empty() {
        lines.push(t!("composer.task_tree.blockers_none"));
    } else {
        lines.push(tf!(
            "composer.task_tree.blockers",
            "blockers" => node.blockers.join(", ")
        ));
    }
    match &node.queue_reason {
        Some(reason) => lines.push(tf!("composer.task_tree.reason", "reason" => reason.clone())),
        None => lines.push(t!("composer.task_tree.reason_none")),
    }
    match node.ready_condition.as_deref() {
        Some("ready to run") | None => lines.push(t!("composer.task_tree.ready_now")),
        Some(condition) => {
            lines.push(tf!("composer.task_tree.ready", "condition" => condition.to_owned()))
        }
    }
    lines.push(tf!(
        "composer.task_tree.usage",
        "usage" => usage_line(&node.usage)
    ));
    if node.unknown_side_effects {
        lines.push(t!("composer.task_tree.unknown_effects"));
    }
    lines
}

fn budget_line(budgets: &RuntimeBudgets) -> SharedString {
    tf!(
        "composer.task_tree.budget",
        "steps" => budgets.max_model_steps,
        "time" => budgets.max_wall_clock_ms,
        "output" => budgets.max_tool_output_bytes,
        "input" => budget_token(budgets.max_input_tokens),
        "output_tokens" => budget_token(budgets.max_output_tokens)
    )
}

fn budget_token(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => t!("composer.task_tree.unlimited").to_string(),
    }
}

fn state_label(state: NodeState) -> SharedString {
    match state {
        NodeState::Queued => t!("composer.task_tree.state.queued"),
        NodeState::Ready => t!("composer.task_tree.state.ready"),
        NodeState::Running => t!("composer.task_tree.state.running"),
        NodeState::Succeeded => t!("composer.task_tree.state.succeeded"),
        NodeState::Failed => t!("composer.task_tree.state.failed"),
        NodeState::Cancelled => t!("composer.task_tree.state.cancelled"),
        NodeState::Unknown => t!("composer.task_tree.state.unknown"),
        NodeState::Blocked => t!("composer.task_tree.state.blocked"),
        NodeState::Stale => t!("composer.task_tree.state.stale"),
    }
}

fn verification_label(verified: Option<bool>) -> SharedString {
    match verified {
        Some(true) => t!("composer.task_tree.verified"),
        Some(false) => t!("composer.task_tree.unverified"),
        None => t!("composer.task_tree.pending"),
    }
}

fn task_actions(
    p: &ResolvedPalette,
    cx: &mut Context<ComposerView>,
    index: usize,
    node: &TaskTreeNode,
    selected: Option<&str>,
) -> gpui::AnyElement {
    let view_id = node.task_id.clone();
    let cancel_id = node.task_id.clone();
    let delete_id = node.task_id.clone();
    let cancellable = matches!(
        node.state,
        NodeState::Queued
            | NodeState::Ready
            | NodeState::Running
            | NodeState::Blocked
            | NodeState::Stale
    );
    let deletable = can_delete_task_record(node);
    let viewing = selected == Some(node.task_id.as_str());
    div()
        .id(SharedString::from(format!("task-tree-actions-{index}")))
        .flex()
        .gap_1()
        .child(
            div()
                .id(SharedString::from(format!("task-tree-view-{index}")))
                .debug_selector({
                    let selector = format!("task-tree-view-{index}");
                    move || selector.clone()
                })
                .px_2()
                .py(px(2.0))
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .text_xs()
                .cursor_pointer()
                .child(t!("composer.task_tree.view"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_task_tree_node(&view_id);
                        cx.notify();
                    }),
                ),
        )
        .when(cancellable, |row| {
            row.child(
                div()
                    .id(SharedString::from(format!("task-tree-cancel-{index}")))
                    .px_2()
                    .py(px(2.0))
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .text_xs()
                    .cursor_pointer()
                    .child(t!("composer.task_tree.cancel"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.cancel_task_tree_child(&cancel_id);
                            cx.notify();
                        }),
                    ),
            )
        })
        .when(deletable, |row| {
            row.child(
                div()
                    .id(SharedString::from(format!("task-tree-delete-{index}")))
                    .debug_selector({
                        let selector = format!("task-tree-delete-{index}");
                        move || selector.clone()
                    })
                    .px_2()
                    .py(px(2.0))
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(p))
                    .text_xs()
                    .cursor_pointer()
                    .child(t!("composer.task_tree.delete"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.delete_task_tree_record(&delete_id);
                            cx.notify();
                        }),
                    ),
            )
        })
        .when(viewing, |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(crate::ui::muted(p))
                    .child(t!("composer.task_tree.viewing")),
            )
        })
        .into_any_element()
}
