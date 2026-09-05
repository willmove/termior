use crate::ui::{self, ButtonKind};
use gpui::{div, prelude::*, px, Context, EventEmitter, MouseButton, SharedString, Window};
use std::collections::HashMap;
use termior_ai::EditProposalSummary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiDiffAction {
    Apply {
        proposal_id: String,
        accepted_hunks: Vec<usize>,
    },
    RejectAll {
        proposal_id: String,
    },
}

pub struct AiDiffView {
    summary: EditProposalSummary,
    decisions: HashMap<usize, bool>,
    result: Option<Result<String, String>>,
}

impl AiDiffView {
    pub fn new(summary: EditProposalSummary) -> Self {
        Self {
            summary,
            decisions: HashMap::new(),
            result: None,
        }
    }

    pub fn set_result(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        self.result = Some(result);
        cx.notify();
    }

    fn decide(&mut self, hunk_id: usize, accept: bool, cx: &mut Context<Self>) {
        if self.result.is_none() {
            self.decisions.insert(hunk_id, accept);
            cx.notify();
        }
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        if self.decisions.len() != self.summary.hunk_ids.len() || self.result.is_some() {
            return;
        }
        let accepted_hunks = self
            .summary
            .hunk_ids
            .iter()
            .filter(|id| self.decisions.get(id) == Some(&true))
            .copied()
            .collect();
        cx.emit(AiDiffAction::Apply {
            proposal_id: self.summary.id.clone(),
            accepted_hunks,
        });
    }

    fn reject_all(&mut self, cx: &mut Context<Self>) {
        if self.result.is_none() {
            cx.emit(AiDiffAction::RejectAll {
                proposal_id: self.summary.id.clone(),
            });
        }
    }
}

impl EventEmitter<AiDiffAction> for AiDiffView {}

impl gpui::Render for AiDiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let controls = self.summary.hunk_ids.iter().map(|id| {
            let accept_id = *id;
            let reject_id = *id;
            let decision = self.decisions.get(id).copied();
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(ui::border(&p))
                .child(SharedString::from(format!("Hunk {}", id + 1)))
                .child(
                    ui::button(
                        SharedString::from(format!("ai-diff-accept-{id}")),
                        "Accept",
                        if decision == Some(true) {
                            ButtonKind::Success
                        } else {
                            ButtonKind::Subtle
                        },
                        &p,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.decide(accept_id, true, cx)),
                    ),
                )
                .child(
                    ui::button(
                        SharedString::from(format!("ai-diff-reject-{id}")),
                        "Reject",
                        if decision == Some(false) {
                            ButtonKind::Danger
                        } else {
                            ButtonKind::Subtle
                        },
                        &p,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.decide(reject_id, false, cx)),
                    ),
                )
        });
        let patch_lines = self.summary.patch.lines().map(|line| {
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                ui::color(p.diff[0])
            } else if line.starts_with('-') && !line.starts_with("---") {
                ui::color(p.diff[1])
            } else if line.starts_with("@@") {
                ui::color(p.accent)
            } else {
                ui::alpha(p.foreground, 0.75)
            };
            div()
                .px_3()
                .text_color(color)
                .child(SharedString::from(line.to_owned()))
        });
        let complete = self.decisions.len() == self.summary.hunk_ids.len();
        let result = self.result.as_ref().map(|result| match result {
            Ok(message) => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(ui::alpha(p.status[1], 0.18))
                .text_color(ui::color(p.status[1]))
                .child(SharedString::from(message.clone())),
            Err(error) => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(ui::alpha(p.status[3], 0.18))
                .text_color(ui::color(p.status[3]))
                .child(SharedString::from(error.clone())),
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(ui::border(&p))
                    .child(
                        div().flex().flex_col().child("AI proposed edit").child(
                            div()
                                .text_xs()
                                .text_color(ui::muted(&p))
                                .child(SharedString::from(self.summary.path.clone())),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                ui::button(
                                    "ai-diff-apply",
                                    "Apply reviewed hunks",
                                    if complete {
                                        ButtonKind::Success
                                    } else {
                                        ButtonKind::Subtle
                                    },
                                    &p,
                                )
                                .when(!complete, |button| button.opacity(0.6))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.apply(cx)),
                                ),
                            )
                            .child(
                                ui::button(
                                    "ai-diff-reject-all",
                                    "Reject all",
                                    ButtonKind::Danger,
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_all(cx)),
                                ),
                            ),
                    ),
            )
            .child(div().flex().flex_wrap().gap_2().p_3().children(controls))
            .children(result)
            .child(
                div()
                    .id("ai-diff-patch")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_x_scroll()
                    .overflow_y_scroll()
                    .p_3()
                    .font_family(crate::monospace_font::default_family())
                    .text_sm()
                    .children(patch_lines),
            )
    }
}
