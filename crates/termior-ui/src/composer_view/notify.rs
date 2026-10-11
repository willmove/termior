//! 通知路由只消费结构化状态（Stage B 6.4）：任务看 [`TaskState`]，命令会话看 [`CommandState`]。
//! 这里不读取任何屏幕文本或状态栏文案；普通 CLI Agent 的通知只来自其显式 OSC 777 事件。

use std::collections::HashMap;

use termior_ai::TaskState;
use termior_platform::AgentStatus;
use termior_terminal::{CommandDigest, CommandSessionId, CommandState};

/// 任务状态对应的通知级别。`plan_request` 时完成态表示“计划待确认”，需要用户注意。
pub(super) fn status_for_task(state: TaskState, plan_request: bool) -> AgentStatus {
    match state {
        TaskState::Idle | TaskState::Running | TaskState::Cancelling => AgentStatus::Working,
        TaskState::WaitingApproval
        | TaskState::WaitingPlan
        | TaskState::WaitingChangeReview
        | TaskState::WaitingUser
        | TaskState::Unknown => AgentStatus::Attention,
        TaskState::CompletedVerified | TaskState::CompletedUnverified if plan_request => {
            AgentStatus::Attention
        }
        TaskState::CompletedVerified | TaskState::CompletedUnverified | TaskState::Cancelled => {
            AgentStatus::Finished
        }
        TaskState::Failed => AgentStatus::Error,
    }
}

/// 命令会话进入终态时的通知级别；未结束或被释放的会话不通知。
pub(super) fn status_for_command(
    state: CommandState,
    exit_code: Option<i32>,
) -> Option<AgentStatus> {
    match state {
        CommandState::Exited if exit_code == Some(0) => Some(AgentStatus::Finished),
        CommandState::Exited | CommandState::Failed => Some(AgentStatus::Error),
        CommandState::Terminated => Some(AgentStatus::Finished),
        CommandState::Unknown => Some(AgentStatus::Attention),
        CommandState::Starting
        | CommandState::Running
        | CommandState::WaitingInput
        | CommandState::Terminating
        | CommandState::Released => None,
    }
}

/// 一条命令会话结束的通知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandNotice {
    pub session_id: CommandSessionId,
    pub command: String,
    pub status: AgentStatus,
}

/// 记住每个任务命令会话上次看到的状态，只在“运行中 → 终态”的边沿产生一次通知。
#[derive(Debug, Default)]
pub(super) struct CommandWatch {
    seen: HashMap<CommandSessionId, CommandState>,
}

impl CommandWatch {
    pub fn observe(&mut self, digests: &[CommandDigest]) -> Vec<CommandNotice> {
        let mut notices = Vec::new();
        for digest in digests {
            let session = &digest.session;
            let previous = self.seen.insert(session.id.clone(), session.state);
            let was_running = previous.is_some_and(|state| !state.is_terminal());
            if !was_running {
                continue;
            }
            if let Some(status) = status_for_command(session.state, session.exit_code) {
                notices.push(CommandNotice {
                    session_id: session.id.clone(),
                    command: session.command.clone(),
                    status,
                });
            }
        }
        notices
    }

    /// 是否还有需要继续轮询的会话。
    pub fn has_running(&self) -> bool {
        self.seen.values().any(|state| !state.is_terminal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_terminal::{
        CommandCapabilities, CommandOwner, CommandSession, Controller, OutputCursor,
    };

    fn digest(id: &str, state: CommandState, exit_code: Option<i32>) -> CommandDigest {
        CommandDigest {
            session: CommandSession {
                id: CommandSessionId(id.into()),
                owner: CommandOwner::Task("task-1".into()),
                project_dir: "/p".into(),
                environment_id: "direct".into(),
                cwd: "/p".into(),
                shell: "sh".into(),
                command: format!("build {id}"),
                interactive: false,
                state,
                controller: Controller::Task("task-1".into()),
                capabilities: CommandCapabilities::default(),
                started_at_ms: 1,
                finished_at_ms: None,
                exit_code,
                output_start: OutputCursor::START,
                output_end: OutputCursor::START,
            },
            tail: String::new(),
            tail_truncated: false,
            buffer_truncated: false,
        }
    }

    #[test]
    fn task_states_map_to_actionable_levels() {
        assert_eq!(
            status_for_task(TaskState::Running, false),
            AgentStatus::Working
        );
        assert_eq!(
            status_for_task(TaskState::WaitingUser, false),
            AgentStatus::Attention
        );
        assert_eq!(
            status_for_task(TaskState::CompletedUnverified, false),
            AgentStatus::Finished
        );
        assert_eq!(
            status_for_task(TaskState::CompletedVerified, true),
            AgentStatus::Attention
        );
        assert_eq!(
            status_for_task(TaskState::Failed, false),
            AgentStatus::Error
        );
        assert_eq!(
            status_for_task(TaskState::Unknown, false),
            AgentStatus::Attention
        );
    }

    #[test]
    fn command_states_map_without_reading_output() {
        assert_eq!(
            status_for_command(CommandState::Exited, Some(0)),
            Some(AgentStatus::Finished)
        );
        assert_eq!(
            status_for_command(CommandState::Exited, Some(2)),
            Some(AgentStatus::Error)
        );
        assert_eq!(
            status_for_command(CommandState::Unknown, None),
            Some(AgentStatus::Attention)
        );
        assert_eq!(status_for_command(CommandState::Running, None), None);
        assert_eq!(status_for_command(CommandState::Released, Some(0)), None);
    }

    #[test]
    fn watch_notifies_once_on_the_running_to_terminal_edge() {
        let mut watch = CommandWatch::default();
        // 首次看到就已结束的会话不通知（不是本次观察期间结束的）。
        assert!(watch
            .observe(&[
                digest("a", CommandState::Running, None),
                digest("old", CommandState::Exited, Some(0)),
            ])
            .is_empty());
        assert!(watch.has_running());

        let notices = watch.observe(&[digest("a", CommandState::Exited, Some(1))]);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].status, AgentStatus::Error);
        assert_eq!(notices[0].command, "build a");
        assert!(!watch.has_running());

        assert!(watch
            .observe(&[digest("a", CommandState::Exited, Some(1))])
            .is_empty());
    }
}
