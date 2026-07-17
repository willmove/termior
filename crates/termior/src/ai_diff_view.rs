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
                .border_color(gpui::rgba(0x344052ff))
                .child(SharedString::from(format!("Hunk {}", id + 1)))
                .child(
                    div()
                        .id(SharedString::from(format!("ai-diff-accept-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(if decision == Some(true) {
                            gpui::rgba(0x3a9b62ff)
                        } else {
                            gpui::rgba(0x293241ff)
                        })
                        .child("Accept")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| this.decide(accept_id, true, cx)),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("ai-diff-reject-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(if decision == Some(false) {
                            gpui::rgba(0xa64a4aff)
                        } else {
                            gpui::rgba(0x293241ff)
                        })
                        .child("Reject")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| this.decide(reject_id, false, cx)),
                        ),
                )
        });
        let patch_lines = self.summary.patch.lines().map(|line| {
            let color = if line.starts_with('+') && !line.starts_with("+++") {
                gpui::rgba(0x8bd49cff)
            } else if line.starts_with('-') && !line.starts_with("---") {
                gpui::rgba(0xe06c75ff)
            } else if line.starts_with("@@") {
                gpui::rgba(0x75a7ffff)
            } else {
                gpui::rgba(0xc6d0e0ff)
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
                .bg(gpui::rgba(0x244c36ff))
                .child(SharedString::from(message.clone())),
            Err(error) => div()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(gpui::rgba(0x5b2929ff))
                .child(SharedString::from(error.clone())),
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::rgba(0x151a22ff))
            .text_color(gpui::rgba(0xd7deebff))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(gpui::rgba(0x344052ff))
                    .child(
                        div().flex().flex_col().child("AI proposed edit").child(
                            div()
                                .text_xs()
                                .text_color(gpui::rgba(0x9aa6b7ff))
                                .child(SharedString::from(self.summary.path.clone())),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("ai-diff-apply")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(if complete {
                                        gpui::rgba(0x3a9b62ff)
                                    } else {
                                        gpui::rgba(0x59606bff)
                                    })
                                    .child("Apply reviewed hunks")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.apply(cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .id("ai-diff-reject-all")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(gpui::rgba(0xa64a4aff))
                                    .child("Reject all")
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
                    .font_family("monospace")
                    .text_sm()
                    .children(patch_lines),
            )
    }
}
