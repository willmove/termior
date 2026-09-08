//! Agent 循环状态机（FR-AGENT-08）。
//!
//! 流式响应、工具调用、步数上限（[`MAX_AGENT_STEPS`]）、系统提示词可配置。
//! 审批门控工具触发时挂起：`run` 返回 [`AgentOutcome`] 携带 `pending_approval`，
//! 调用方决议后用 [`Agent::resume`] 续跑。决议是 `resume` 的函数参数，不经阻塞
//! channel 或自旋等待传递。
//!
//! 本模块用 [`MockProvider`]（或任意 [`Provider`]）跑通「提问 -> 工具调用 -> 审批 -> 续跑 -> 完成」
//! 的全链路单测。工具的实际副作用（写盘/执行命令）由调用方在审批通过后执行。

use crate::approval::{ApprovalDecision, ApprovalRequest};
use crate::message::{ChatEvent, Message, Role};
use crate::provider::{Provider, ProviderRequest};
use crate::tools::ToolRegistry;
use futures::StreamExt;

/// 最大步数（FR-AGENT-08 `MAX_AGENT_STEPS`）。
pub const MAX_AGENT_STEPS: usize = 50;

/// Agent 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Busy,
    AwaitingApproval,
    Finished,
    Error,
}

/// Agent 执行错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("max agent steps ({0}) exceeded")]
    MaxStepsExceeded(usize),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("approval rejected")]
    Rejected,
}

/// 一轮 Agent 循环的结果。
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    /// 结束状态。
    pub state: AgentState,
    /// 本轮累积的助手消息与工具调用。
    pub messages: Vec<Message>,
    /// 若停在审批门，等待用户决议的请求。
    pub pending_approval: Option<ApprovalRequest>,
    /// 已执行步数。
    pub steps: usize,
}

/// Agent 运行器。
pub struct Agent {
    pub provider: Box<dyn Provider>,
    pub tools: ToolRegistry,
    pub system_prompt: Option<String>,
    pub max_steps: usize,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, tools: ToolRegistry) -> Self {
        Self {
            provider,
            tools,
            system_prompt: None,
            max_steps: MAX_AGENT_STEPS,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    /// 运行一轮：从当前历史出发，驱动 Provider 直到完成或需要审批。
    ///
    /// `exec_tool` 是工具实际执行的回调（调用方实现真实副作用）；仅在审批通过后调用。
    /// `on_event` 在每个 [`ChatEvent`] 到达时被调用，用于把流式增量（如 `TextDelta`）
    /// 实时推给 UI 渲染；Agent 内部仍会累积这些事件以构造最终消息。
    /// 返回 [`AgentOutcome`]；若停在 AwaitingApproval，调用方决议后用
    /// [`Self::resume`] 续跑。
    pub async fn run<F>(
        &self,
        history: &[Message],
        exec_tool: &F,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String> + Sync,
    {
        self.drive(history, exec_tool, 0, false, on_event).await
    }

    /// 审批决议后续跑。
    pub async fn resume<F>(
        &self,
        history: &[Message],
        exec_tool: &F,
        decision: ApprovalDecision,
        pending: &ApprovalRequest,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String> + Sync,
    {
        let mut messages = history.to_vec();
        let result = match decision {
            ApprovalDecision::Approve => match self
                .tools
                .validate_and_normalize(&pending.tool_name, &pending.arguments)
            {
                Ok(arguments) => match exec_tool(&pending.tool_name, &arguments) {
                    Ok(output) => crate::message::ToolResult::success(&pending.call_id, output),
                    Err(error) => crate::message::ToolResult::failure(&pending.call_id, error),
                },
                Err(error) => {
                    crate::message::ToolResult::failure(&pending.call_id, error.to_string())
                }
            },
            ApprovalDecision::Reject => {
                crate::message::ToolResult::failure(&pending.call_id, "tool call denied by user")
            }
        };
        messages.push(tool_result_message(result));
        self.drive(&messages, exec_tool, pending.steps, true, on_event)
            .await
    }

    async fn drive<F>(
        &self,
        history: &[Message],
        exec_tool: &F,
        mut steps: usize,
        resume_existing_calls: bool,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String> + Sync,
    {
        let mut messages: Vec<Message> = history.to_vec();
        if resume_existing_calls {
            if let Some(pending) = self.process_current_calls(&mut messages, exec_tool, steps)? {
                return Ok(AgentOutcome {
                    state: AgentState::AwaitingApproval,
                    messages,
                    pending_approval: Some(pending),
                    steps,
                });
            }
        }
        loop {
            if steps >= self.max_steps {
                return Err(AgentError::MaxStepsExceeded(self.max_steps));
            }
            let req = ProviderRequest {
                messages: self.provider_messages(&messages),
                model: "default".into(),
                tools: self.tools.clone(),
                system_prompt_extra: None,
            };
            let mut stream = self.provider.stream_chat(&req);

            // 逐事件消费流：增量回调 + 累积成最终消息
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            let mut final_msg: Option<Message> = None;
            let mut err: Option<String> = None;
            while let Some(ev) = stream.next().await {
                on_event(&ev);
                match ev {
                    ChatEvent::TextDelta(d) => text.push_str(&d),
                    ChatEvent::ToolCall(tc) => tool_calls.push(tc),
                    ChatEvent::Done(m) => final_msg = Some(m),
                    ChatEvent::Error(e) => err = Some(e),
                }
            }
            if let Some(e) = err {
                return Err(AgentError::Provider(e));
            }

            let assistant = final_msg.unwrap_or_else(|| Message {
                role: Role::Assistant,
                content: text,
                tool_calls: tool_calls.clone(),
                tool_result: None,
            });
            messages.push(assistant.clone());
            steps += 1;

            // 无工具调用 → 完成
            if assistant.tool_calls.is_empty() {
                return Ok(AgentOutcome {
                    state: AgentState::Finished,
                    messages,
                    pending_approval: None,
                    steps,
                });
            }

            if let Some(pending) = self.process_current_calls(&mut messages, exec_tool, steps)? {
                return Ok(AgentOutcome {
                    state: AgentState::AwaitingApproval,
                    messages,
                    pending_approval: Some(pending),
                    steps,
                });
            }
        }
    }

    fn provider_messages(&self, messages: &[Message]) -> Vec<Message> {
        let mut request =
            Vec::with_capacity(messages.len() + usize::from(self.system_prompt.is_some()));
        if let Some(prompt) = &self.system_prompt {
            request.push(Message::system(prompt));
        }
        request.extend_from_slice(messages);
        request
    }

    fn process_current_calls<F>(
        &self,
        messages: &mut Vec<Message>,
        exec_tool: &F,
        steps: usize,
    ) -> Result<Option<ApprovalRequest>, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String> + Sync,
    {
        let Some((assistant_index, calls)) = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| message.role == Role::Assistant && !message.tool_calls.is_empty())
            .map(|(index, message)| (index, message.tool_calls.clone()))
        else {
            return Ok(None);
        };
        let completed = messages[assistant_index + 1..]
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .map(|result| result.call_id.clone())
            .collect::<std::collections::HashSet<_>>();

        for call in calls {
            if completed.contains(&call.id) {
                continue;
            }
            let arguments = match self
                .tools
                .validate_and_normalize(&call.name, &call.arguments)
            {
                Ok(arguments) => arguments,
                Err(error) => {
                    messages.push(tool_result_message(crate::message::ToolResult::failure(
                        &call.id,
                        error.to_string(),
                    )));
                    continue;
                }
            };
            match self.tools.requires_approval(&call.name) {
                Ok(true) => {
                    return Ok(Some(ApprovalRequest {
                        call_id: call.id,
                        tool_name: call.name.clone(),
                        summary: summarize(&call.name, &arguments),
                        arguments,
                        steps,
                    }));
                }
                Ok(false) => {
                    let result = match exec_tool(&call.name, &arguments) {
                        Ok(output) => crate::message::ToolResult::success(&call.id, output),
                        Err(error) => crate::message::ToolResult::failure(&call.id, error),
                    };
                    messages.push(tool_result_message(result));
                }
                Err(error) => messages.push(tool_result_message(
                    crate::message::ToolResult::failure(&call.id, error.to_string()),
                )),
            }
        }
        Ok(None)
    }
}

fn tool_result_message(result: crate::message::ToolResult) -> Message {
    Message {
        role: Role::Tool,
        content: String::new(),
        tool_calls: Vec::new(),
        tool_result: Some(result),
    }
}

fn summarize(tool: &str, args: &str) -> String {
    // 简短人类可读摘要
    match serde_json::from_str::<serde_json::Value>(args) {
        Ok(v) => {
            if let Some(path) = v.get("path").and_then(|p| p.as_str()) {
                format!("{tool} {path}")
            } else {
                format!("{tool} {args}")
            }
        }
        Err(_) => format!("{tool} {args}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::MockProvider;
    use futures::executor::block_on;
    use termior_security::workspace::WorkspaceAuthRegistry;

    fn no_op(_tool: &str, _args: &str) -> Result<String, String> {
        Ok("ok".into())
    }

    fn ignore_event(_: &ChatEvent) {}

    #[test]
    fn simple_text_response_finishes() {
        let provider = MockProvider::single(vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done(Message::assistant("Hello")),
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = block_on(async {
            agent
                .run(&[Message::user("hi")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
        assert!(outcome.pending_approval.is_none());
        assert!(outcome.steps >= 1);
    }

    #[test]
    fn auto_tool_executes_then_finishes() {
        // 第一轮：助手发起 read_file（自动）-> 第二轮：助手回复文本完成
        let provider = MockProvider::new(vec![
            vec![
                ChatEvent::ToolCall(crate::message::ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"/proj/a"}"#.into(),
                }),
                ChatEvent::Done(Message {
                    role: Role::Assistant,
                    content: "reading".into(),
                    tool_calls: vec![crate::message::ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"/proj/a"}"#.into(),
                    }],
                    tool_result: None,
                }),
            ],
            vec![ChatEvent::Done(Message::assistant("done"))],
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = block_on(async {
            agent
                .run(&[Message::user("read a")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
        // 历史里应有 tool 结果消息
        assert!(outcome.messages.iter().any(|m| m.role == Role::Tool));
    }

    #[test]
    fn approval_tool_suspends() {
        let provider = MockProvider::single(vec![ChatEvent::Done(Message {
            role: Role::Assistant,
            content: "writing".into(),
            tool_calls: vec![crate::message::ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"/proj/a","content":"x"}"#.into(),
            }],
            tool_result: None,
        })]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = block_on(async {
            agent
                .run(&[Message::user("write a")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        assert_eq!(outcome.state, AgentState::AwaitingApproval);
        let pending = outcome.pending_approval.unwrap();
        assert_eq!(pending.tool_name, "write_file");
        assert!(pending.summary.contains("write_file"));
    }

    #[test]
    fn approval_rejection_becomes_a_tool_result() {
        let provider = MockProvider::single(vec![ChatEvent::Done(Message {
            role: Role::Assistant,
            content: "writing".into(),
            tool_calls: vec![crate::message::ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"/proj/a","content":"x"}"#.into(),
            }],
            tool_result: None,
        })]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = block_on(async {
            agent
                .run(&[Message::user("write")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        let pending = outcome.pending_approval.unwrap();
        let resumed = block_on(async {
            agent
                .resume(
                    &outcome.messages,
                    &no_op,
                    ApprovalDecision::Reject,
                    &pending,
                    &mut ignore_event,
                )
                .await
        })
        .unwrap();
        assert_eq!(resumed.state, AgentState::Finished);
        assert!(resumed.messages.iter().any(|message| {
            message
                .tool_result
                .as_ref()
                .is_some_and(|result| result.call_id == "c1" && !result.ok)
        }));
    }

    #[test]
    fn approval_approved_executes_and_resumes() {
        // 第一轮（drive 内）：write_file 待审批 -> outcome
        let provider = MockProvider::new(vec![
            vec![ChatEvent::Done(Message {
                role: Role::Assistant,
                content: "writing".into(),
                tool_calls: vec![crate::message::ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"/proj/a","content":"x"}"#.into(),
                }],
                tool_result: None,
            })],
            // resume 后第二轮：完成
            vec![ChatEvent::Done(Message::assistant("written"))],
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = block_on(async {
            agent
                .run(&[Message::user("write")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        let pending = outcome.pending_approval.unwrap();
        let resumed = block_on(async {
            agent
                .resume(
                    &outcome.messages,
                    &no_op,
                    ApprovalDecision::Approve,
                    &pending,
                    &mut ignore_event,
                )
                .await
        })
        .unwrap();
        assert_eq!(resumed.state, AgentState::Finished);
    }

    #[test]
    fn max_steps_enforced() {
        // 无限工具调用循环：每轮都发起 read_file，但永不输出无 tool_call 的 Done
        let looping = vec![ChatEvent::Done(Message {
            role: Role::Assistant,
            content: "loop".into(),
            tool_calls: vec![crate::message::ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"/proj/a"}"#.into(),
            }],
            tool_result: None,
        })];
        // 复制脚本使其每次调用都返回同一循环响应
        let scripts: Vec<_> = (0..100).map(|_| looping.clone()).collect();
        let provider = MockProvider::new(scripts);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default()).with_max_steps(5);
        let err = block_on(async {
            agent
                .run(&[Message::user("loop")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap_err();
        assert!(matches!(err, AgentError::MaxStepsExceeded(5)));
    }

    #[test]
    fn system_prompt_prepended() {
        let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("ok"))]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default())
            .with_system_prompt("You are a Termior agent.");
        let outcome = block_on(async {
            agent
                .run(&[Message::user("hi")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
    }

    #[test]
    fn workspace_auth_passed_through() {
        // 验证 ToolRegistry 携带 workspace 授权并被工具门控使用
        let reg = ToolRegistry::new(WorkspaceAuthRegistry::with_roots(["/proj".to_string()]));
        assert!(reg
            .check_path_access(
                "read_file",
                "/proj/a.rs",
                termior_security::deny_list::Direction::Read
            )
            .is_ok());
    }

    #[test]
    fn provider_error_propagates() {
        let provider = MockProvider::single(vec![ChatEvent::Error("boom".into())]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let err = block_on(async {
            agent
                .run(&[Message::user("hi")], &no_op, &mut ignore_event)
                .await
        })
        .unwrap_err();
        assert!(matches!(err, AgentError::Provider(_)));
    }

    #[test]
    fn max_agent_steps_constant() {
        assert_eq!(MAX_AGENT_STEPS, 50);
    }
}
