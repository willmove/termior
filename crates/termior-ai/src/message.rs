//! 统一消息 / 工具调用 / 流式事件模型（FR-PROV）。

use serde::{Deserialize, Serialize};

/// 对话角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 一条消息（可能含工具调用或工具结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// 助手消息发起的工具调用（仅 role=Assistant 时有意义）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// tool 消息回填的结果（仅 role=Tool）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ToolResult>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: vec![],
            tool_result: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: vec![],
            tool_result: None,
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: vec![],
            tool_result: None,
        }
    }
}

/// 一次工具调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 id（用于把 tool_result 回填到对应调用）。
    pub id: String,
    /// 工具名（对齐 FR-AGENT-09 工具清单）。
    pub name: String,
    /// 参数（JSON 文本）。
    pub arguments: String,
}

/// 工具执行结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    /// 是否执行成功。
    pub ok: bool,
    pub output: String,
    /// Handle for the complete retained body when `output` is a bounded summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<crate::context_engine::ContentRef>,
}

impl ToolResult {
    pub fn success(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            ok: true,
            output: output.into(),
            content_ref: None,
        }
    }
    pub fn failure(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            ok: false,
            output: output.into(),
            content_ref: None,
        }
    }

    pub fn with_content_ref(mut self, content_ref: crate::context_engine::ContentRef) -> Self {
        self.content_ref = Some(content_ref);
        self
    }
}

/// 流式聊天事件（Provider 统一增量事件模型）。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// 文本增量。
    TextDelta(String),
    /// 模型发起的工具调用（通常在 delta 结束后整块下发）。
    ToolCall(ToolCall),
    /// 一轮响应结束（含本次助手最终消息）。
    Done(Message),
    /// 错误。
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        assert_eq!(Message::user("hi").role, Role::User);
        assert_eq!(Message::assistant("ok").role, Role::Assistant);
        assert_eq!(Message::system("rules").role, Role::System);
    }

    #[test]
    fn tool_result_helpers() {
        let ok = ToolResult::success("c1", "done");
        assert!(ok.ok);
        let err = ToolResult::failure("c2", "boom");
        assert!(!err.ok);
    }

    #[test]
    fn message_with_tool_call_serializes() {
        let m = Message {
            role: Role::Assistant,
            content: "calling".into(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            }],
            tool_result: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("read_file"));
        // 回填
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tool_calls.len(), 1);
    }

    #[test]
    fn empty_tool_calls_skipped_in_json() {
        let m = Message::user("hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("tool_calls"));
    }
}
