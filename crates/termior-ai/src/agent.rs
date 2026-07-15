//! Agent 循环状态机（FR-AGENT-08）。
//!
//! 流式响应、工具调用、步数上限（[`MAX_AGENT_STEPS`]）、系统提示词可配置。
//! 审批门控工具触发时挂起，等待 [`ApprovalGate`] 决议后续跑。
//!
//! 本模块用 [`MockProvider`]（或任意 [`Provider`]）跑通「提问 → 工具调用 → 审批 → 续跑 → 完成」
//! 的全链路单测。工具的实际副作用（写盘/执行命令）由调用方在审批通过后执行。

use crate::approval::{ApprovalDecision, ApprovalRequest};
use crate::message::{ChatEvent, Message, Role};
use crate::provider::{Provider, ProviderRequest};
use crate::tools::ToolRegistry;

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
    /// 返回 [`AgentOutcome`]；若停在 AwaitingApproval，调用方决议后用
    /// [`Self::resume`] 续跑。
    pub fn run<F>(&self, history: &[Message], exec_tool: &F) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String>,
    {
        self.drive(history, exec_tool, None)
    }

    /// 审批决议后续跑。
    pub fn resume<F>(
        &self,
        history: &[Message],
        exec_tool: &F,
        decision: ApprovalDecision,
        pending: &ApprovalRequest,
    ) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String>,
    {
        match decision {
            ApprovalDecision::Approve => {
                let mut messages = history.to_vec();
                // 执行被批准的工具，回填 tool_result
                match exec_tool(&pending.tool_name, &pending.arguments) {
                    Ok(out) => messages.push(Message {
                        role: Role::Tool,
                        content: String::new(),
                        tool_calls: vec![],
                        tool_result: Some(crate::message::ToolResult::success(&pending.call_id, out)),
                    }),
                    Err(e) => messages.push(Message {
                        role: Role::Tool,
                        content: String::new(),
                        tool_calls: vec![],
                        tool_result: Some(crate::message::ToolResult::failure(&pending.call_id, e)),
                    }),
                }
                self.drive(&messages, exec_tool, None)
            }
            ApprovalDecision::Reject => Err(AgentError::Rejected),
        }
    }

    fn drive<F>(
        &self,
        history: &[Message],
        exec_tool: &F,
        _resume_from: Option<&ApprovalRequest>,
    ) -> Result<AgentOutcome, AgentError>
    where
        F: Fn(&str, &str) -> Result<String, String>,
    {
        let mut messages: Vec<Message> = history.to_vec();
        // 注入 system prompt（每轮前置）
        let mut with_system: Vec<Message> = Vec::new();
        if let Some(sp) = &self.system_prompt {
            with_system.push(Message::system(sp));
        }
        with_system.extend(messages.clone());

        let mut steps = 0usize;
        loop {
            if steps >= self.max_steps {
                return Err(AgentError::MaxStepsExceeded(self.max_steps));
            }
            let req = ProviderRequest {
                messages: with_system.clone(),
                model: "default".into(),
                tools: self.tools.clone(),
                system_prompt_extra: None,
            };
            let events = self.provider.stream_chat(&req);

            // 收集本轮助手消息与工具调用
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            let mut final_msg: Option<Message> = None;
            let mut err: Option<String> = None;
            for ev in events {
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

            // 处理每个工具调用
            for tc in &assistant.tool_calls {
                let needs_approval = self.tools.requires_approval(&tc.name).unwrap_or(false);
                if needs_approval {
                    // 挂起：构造审批请求（assistant 消息已 push 进 history）
                    let req = ApprovalRequest {
                        call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                        summary: summarize(&tc.name, &tc.arguments),
                    };
                    return Ok(AgentOutcome {
                        state: AgentState::AwaitingApproval,
                        messages,
                        pending_approval: Some(req),
                        steps,
                    });
                }
                // 自动工具：直接执行
                let result = match exec_tool(&tc.name, &tc.arguments) {
                    Ok(out) => crate::message::ToolResult::success(&tc.id, out),
                    Err(e) => crate::message::ToolResult::failure(&tc.id, e),
                };
                messages.push(Message {
                    role: Role::Tool,
                    content: String::new(),
                    tool_calls: vec![],
                    tool_result: Some(result),
                });
            }

            // 重建下一轮请求的 messages
            with_system.clear();
            if let Some(sp) = &self.system_prompt {
                with_system.push(Message::system(sp));
            }
            with_system.extend(messages.clone());
        }
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
    use termior_security::workspace::WorkspaceAuthRegistry;

    fn no_op(_tool: &str, _args: &str) -> Result<String, String> {
        Ok("ok".into())
    }

    #[test]
    fn simple_text_response_finishes() {
        let provider = MockProvider::single(vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done(Message::assistant("Hello")),
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = agent.run(&[Message::user("hi")], &no_op).unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
        assert!(outcome.pending_approval.is_none());
        assert!(outcome.steps >= 1);
    }

    #[test]
    fn auto_tool_executes_then_finishes() {
        // 第一轮：助手发起 read_file（自动）→ 第二轮：助手回复文本完成
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
        let outcome = agent.run(&[Message::user("read a")], &no_op).unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
        // 历史里应有 tool 结果消息
        assert!(outcome.messages.iter().any(|m| m.role == Role::Tool));
    }

    #[test]
    fn approval_tool_suspends() {
        let provider = MockProvider::single(vec![
            ChatEvent::Done(Message {
                role: Role::Assistant,
                content: "writing".into(),
                tool_calls: vec![crate::message::ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"/proj/a","content":"x"}"#.into(),
                }],
                tool_result: None,
            }),
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = agent.run(&[Message::user("write a")], &no_op).unwrap();
        assert_eq!(outcome.state, AgentState::AwaitingApproval);
        let pending = outcome.pending_approval.unwrap();
        assert_eq!(pending.tool_name, "write_file");
        assert!(pending.summary.contains("write_file"));
    }

    #[test]
    fn approval_rejected_errors() {
        let provider = MockProvider::single(vec![
            ChatEvent::Done(Message {
                role: Role::Assistant,
                content: "writing".into(),
                tool_calls: vec![crate::message::ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"/proj/a"}"#.into(),
                }],
                tool_result: None,
            }),
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = agent.run(&[Message::user("write")], &no_op).unwrap();
        let pending = outcome.pending_approval.unwrap();
        let err = agent
            .resume(&outcome.messages, &no_op, ApprovalDecision::Reject, &pending)
            .unwrap_err();
        assert!(matches!(err, AgentError::Rejected));
    }

    #[test]
    fn approval_approved_executes_and_resumes() {
        // 第一轮（drive 内）：write_file 待审批 → outcome
        let provider = MockProvider::new(vec![
            vec![ChatEvent::Done(Message {
                role: Role::Assistant,
                content: "writing".into(),
                tool_calls: vec![crate::message::ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"/proj/a"}"#.into(),
                }],
                tool_result: None,
            })],
            // resume 后第二轮：完成
            vec![ChatEvent::Done(Message::assistant("written"))],
        ]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let outcome = agent.run(&[Message::user("write")], &no_op).unwrap();
        let pending = outcome.pending_approval.unwrap();
        let resumed = agent
            .resume(&outcome.messages, &no_op, ApprovalDecision::Approve, &pending)
            .unwrap();
        assert_eq!(resumed.state, AgentState::Finished);
    }

    #[test]
    fn max_steps_enforced() {
        // 无限工具调用循环：每轮都发起 read_file，但永不输出无 tool_call 的 Done
        let looping = vec![
            ChatEvent::Done(Message {
                role: Role::Assistant,
                content: "loop".into(),
                tool_calls: vec![crate::message::ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"/proj/a"}"#.into(),
                }],
                tool_result: None,
            }),
        ];
        // 复制脚本使其每次调用都返回同一循环响应
        let scripts: Vec<_> = (0..100).map(|_| looping.clone()).collect();
        let provider = MockProvider::new(scripts);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default()).with_max_steps(5);
        let err = agent.run(&[Message::user("loop")], &no_op).unwrap_err();
        assert!(matches!(err, AgentError::MaxStepsExceeded(5)));
    }

    #[test]
    fn system_prompt_prepended() {
        let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("ok"))]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default())
            .with_system_prompt("You are a Termior agent.");
        let outcome = agent.run(&[Message::user("hi")], &no_op).unwrap();
        assert_eq!(outcome.state, AgentState::Finished);
    }

    #[test]
    fn workspace_auth_passed_through() {
        // 验证 ToolRegistry 携带 workspace 授权并被工具门控使用
        let reg = ToolRegistry::new(WorkspaceAuthRegistry::with_roots(["/proj".to_string()]));
        assert!(reg
            .check_path_access("read_file", "/proj/a.rs", termior_security::deny_list::Direction::Read)
            .is_ok());
    }

    #[test]
    fn provider_error_propagates() {
        let provider = MockProvider::single(vec![ChatEvent::Error("boom".into())]);
        let agent = Agent::new(Box::new(provider), ToolRegistry::default());
        let err = agent.run(&[Message::user("hi")], &no_op).unwrap_err();
        assert!(matches!(err, AgentError::Provider(_)));
    }

    #[test]
    fn max_agent_steps_constant() {
        assert_eq!(MAX_AGENT_STEPS, 50);
    }
}
