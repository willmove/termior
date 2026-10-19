//! 任务详情面板（Stage B 6.3）：绑定项目、执行环境、命令会话、controller、增量输出尾部、
//! 退出码与截断提示，以及 Take over / Hand back 快捷操作（Stage B 3.4）。

use super::*;
use termior_terminal::{
    shared_terminal_service, CommandDigest, CommandSessionId, CommandState, Controller,
    TerminalService,
};
use termior_theme::ResolvedPalette;

/// 每条命令会话展示的输出尾部字节数与行数。
const TAIL_BYTES: usize = 4 * 1024;
const TAIL_LINES: usize = 12;

/// 任务详情所指的任务：活动任务结束后仍保留，便于查看其命令结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskRef {
    pub task_id: String,
    pub project_dir: PathBuf,
    pub backend_id: String,
    pub environment_id: String,
}

impl TaskRef {
    pub fn from_task(task: &termior_ai::Task) -> Self {
        Self {
            task_id: task.id.0.clone(),
            project_dir: task.project_dir.clone(),
            backend_id: task.backend_id.clone(),
            environment_id: task.environment_id.clone(),
        }
    }
}

impl ComposerView {
    /// Pinned environment when a task has one; otherwise a display-only profile for the header.
    pub(super) fn displayed_environment(&self) -> PinnedEnvironment {
        if let Some(pin) = &self.orchestrator.pinned_environment {
            return pin.clone();
        }
        let root = self.orchestrator.displayed_root(&self.workspace_root);
        let profile = if self.orchestrator.bound_worktree.is_some() {
            EnvironmentProfile::Worktree
        } else if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
            EnvironmentProfile::external("codex")
        } else {
            EnvironmentProfile::Direct
        };
        PinnedEnvironment::display_only(profile, root)
    }

    pub(super) fn environment_capability_line(&self) -> SharedString {
        let pin = self.displayed_environment();
        tf!(
            "composer.environment.capabilities",
            "host" => pin.host.clone(),
            "root" => pin.root.display().to_string(),
            "file" => pin.file.clone(),
            "network" => pin.network.clone(),
            "credentials" => pin.credentials.clone(),
            "process" => pin.process.clone()
        )
    }

    pub(super) fn capability_row(&self, id: &'static str, p: &ResolvedPalette) -> AnyElement {
        div()
            .id(id)
            .text_xs()
            .text_color(crate::ui::muted(p))
            .child(self.environment_capability_line())
            .into_any_element()
    }

    pub(super) fn task_digests(&self) -> Vec<CommandDigest> {
        self.task_digests_with_tail(true)
    }

    /// `with_tail = false` 时只取会话状态（输出尾部至多一个 chunk），供后台轮询使用。
    pub(super) fn task_digests_with_tail(&self, with_tail: bool) -> Vec<CommandDigest> {
        let tail_bytes = if with_tail { TAIL_BYTES } else { 0 };
        self.last_task
            .as_ref()
            .map(|task| shared_terminal_service().task_digests(&task.task_id, tail_bytes))
            .unwrap_or_default()
    }

    /// 当前活动任务正因用户接管该会话而等待。
    fn waiting_for_hand_back(&self, id: &CommandSessionId) -> bool {
        self.orchestrator.active_task.as_ref().is_some_and(|task| {
            matches!(
                &task.task().waiting_reason,
                Some(WaitingReason::TerminalControl { session_id, .. }) if *session_id == id.0
            )
        })
    }

    /// 用户从任务详情或终端横幅接管 Agent 持有的会话；运行中的任务在下一步进入 waiting-user。
    pub fn take_over_terminal(&mut self, id: &CommandSessionId, cx: &mut Context<Self>) {
        match shared_terminal_service().take_over(id) {
            Ok(cursor) => self.set_status(tf!(
                "composer.task.took_over",
                "session" => id.0.clone(),
                "cursor" => cursor.sequence
            )),
            Err(error) => self.set_status(tf!(
                "composer.task.control_failed",
                "error" => error
            )),
        }
        cx.notify();
    }

    /// 用户把接管的会话交回 Agent（Hand back）：恢复写权限，并以当前游标继续等待中的任务。
    pub fn hand_back_terminal(&mut self, id: &CommandSessionId, cx: &mut Context<Self>) {
        let Some(executor) = self.task_runtime().map(|runtime| runtime.executor.clone()) else {
            self.set_status(t!("composer.task.no_agent_to_hand_back"));
            cx.notify();
            return;
        };
        let cursor = match executor.hand_back_to_task(id) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.set_status(tf!("composer.task.control_failed", "error" => error));
                cx.notify();
                return;
            }
        };
        if self.waiting_for_hand_back(id) && !self.orchestrator.busy {
            self.dispatch_task_command(
                TaskCommand::HandBack {
                    session_id: id.0.clone(),
                    cursor: cursor.sequence,
                },
                t!("composer.task.handed_back"),
                cx,
            );
        } else {
            self.set_status(tf!(
                "composer.task.handed_back_idle",
                "session" => id.0.clone()
            ));
            cx.notify();
        }
    }

    pub(super) fn task_details_center(
        &self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.center_open(ComposerCenter::TaskDetails) {
            return None;
        }
        let Some(task) = self.last_task.as_ref() else {
            return Some(
                div()
                    .p_2()
                    .text_xs()
                    .text_color(crate::ui::muted(p))
                    .child(t!("composer.task.none"))
                    .into_any_element(),
            );
        };
        let state = self
            .orchestrator
            .active_task
            .as_ref()
            .filter(|runtime| runtime.task().id.0 == task.task_id)
            .map(|runtime| format!("{:?}", runtime.task().state))
            .unwrap_or_else(|| t!("composer.task.state_finished").to_string());
        let digests = self.task_digests();
        let header = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t!("composer.task.title"))
                    .child(SharedString::from(state)),
            )
            .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                "composer.task.binding",
                "task" => task.task_id.clone(),
                "project" => task.project_dir.display(),
                "backend" => task.backend_id.clone(),
                "environment" => task.environment_id.clone()
            )))
            .child(self.capability_row("composer-task-capabilities", p));
        let empty = digests.is_empty().then(|| {
            div()
                .text_xs()
                .text_color(crate::ui::muted(p))
                .child(t!("composer.task.no_commands"))
        });
        let rows = digests
            .into_iter()
            .enumerate()
            .map(|(index, digest)| self.command_row(index, digest, p, cx));
        Some(
            div()
                .id("composer-task-details")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(p))
                .child(header)
                .children(empty)
                .children(rows)
                .into_any_element(),
        )
    }

    fn command_row(
        &self,
        index: usize,
        digest: CommandDigest,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = &digest.session;
        let state_color = match session.state {
            CommandState::Exited if session.exit_code == Some(0) => p.status[1],
            CommandState::Exited | CommandState::Failed => p.status[3],
            CommandState::Unknown => p.status[2],
            _ => p.status[0],
        };
        let controller = match &session.controller {
            Controller::User => t!("composer.task.controller_user"),
            Controller::Task(_) => t!("composer.task.controller_agent"),
        };
        let exit = session
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| {
                if session.state.is_terminal() {
                    t!("composer.task.exit_unknown").to_string()
                } else {
                    t!("composer.task.exit_running").to_string()
                }
            });
        let command = if session.command.is_empty() {
            t!("composer.task.interactive_shell").to_string()
        } else {
            session.command.clone()
        };
        let tail = last_lines(&digest.tail, TAIL_LINES);
        let truncation = if digest.buffer_truncated {
            Some(tf!(
                "composer.task.buffer_truncated",
                "cursor" => session.output_start.sequence
            ))
        } else if digest.tail_truncated {
            Some(t!("composer.task.tail_truncated"))
        } else {
            None
        };
        let running_interactive = session.interactive && !session.state.is_terminal();
        let can_take_over =
            running_interactive && matches!(session.controller, Controller::Task(_));
        let can_hand_back = running_interactive
            && session.controller == Controller::User
            && self.waiting_for_hand_back(&session.id);
        let take_id = session.id.clone();
        let back_id = session.id.clone();
        div()
            .id(SharedString::from(format!("composer-task-command-{index}")))
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
                    .gap_2()
                    .child(
                        div()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .font_family(crate::monospace_font::default_family())
                            .text_xs()
                            .child(SharedString::from(format!("$ {command}"))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(crate::ui::color(state_color))
                            .child(tf!(
                                "composer.task.command_state",
                                "state" => format!("{:?}", session.state),
                                "exit" => exit
                            )),
                    ),
            )
            .child(div().text_xs().text_color(crate::ui::muted(p)).child(tf!(
                "composer.task.command_detail",
                "session" => session.id.0.clone(),
                "controller" => controller,
                "cwd" => session.cwd.clone(),
                "start" => session.output_start.sequence,
                "end" => session.output_end.sequence
            )))
            .when_some(truncation, |row, note| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::color(p.status[2]))
                        .child(note),
                )
            })
            .when(!tail.is_empty(), |row| {
                row.child(
                    div()
                        .p_1()
                        .rounded_sm()
                        .bg(crate::ui::alpha(p.foreground, 0.05))
                        .font_family(crate::monospace_font::default_family())
                        .text_xs()
                        .child(SharedString::from(tail)),
                )
            })
            .when(can_take_over || can_hand_back, |row| {
                row.child(
                    div()
                        .flex()
                        .gap_1()
                        .when(can_take_over, |actions| {
                            actions.child(
                                crate::ui::button(
                                    SharedString::from(format!("composer-task-take-over-{index}")),
                                    t!("composer.task.take_over"),
                                    crate::ui::ButtonKind::Subtle,
                                    p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.take_over_terminal(&take_id, cx);
                                    }),
                                ),
                            )
                        })
                        .when(can_hand_back, |actions| {
                            actions.child(
                                crate::ui::button(
                                    SharedString::from(format!("composer-task-hand-back-{index}")),
                                    t!("composer.task.hand_back"),
                                    crate::ui::ButtonKind::Primary,
                                    p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.hand_back_terminal(&back_id, cx);
                                    }),
                                ),
                            )
                        }),
                )
            })
    }
}

/// 取文本最后 `max` 行（去掉末尾空行）。
fn last_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::last_lines;

    #[test]
    fn last_lines_keeps_the_newest_lines() {
        assert_eq!(last_lines("a\nb\nc\n\n", 2), "b\nc");
        assert_eq!(last_lines("", 3), "");
        assert_eq!(last_lines("only", 3), "only");
    }
}
