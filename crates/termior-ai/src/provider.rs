//! Provider 抽象层（FR-PROV）。
//!
//! `Provider` trait 统一不同模型服务的流式消息、工具调用与错误语义。
//! 各家做请求/响应适配器（FR-PROV-01，P0 首发 Anthropic + OpenAI + OpenAI-compatible，
//! 真实 HTTP 留待可编译验证里程碑）。本 crate 提供 [`MockProvider`] 跑通 Agent 循环。

use crate::message::{ChatEvent, Message};
use crate::tools::ToolRegistry;

/// 一次聊天请求。
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    /// 完整消息历史（含 system / user / assistant / tool）。
    pub messages: Vec<Message>,
    /// 当前模型 id。
    pub model: String,
    /// 允许模型调用的工具注册表（描述层）。
    pub tools: ToolRegistry,
    /// 可配置的系统提示词附加段（FR-AGENT-08）。
    pub system_prompt_extra: Option<String>,
}

/// Provider 抽象（FR-PROV 技术要点）。
///
/// 真实实现返回 `impl Stream<Item = ChatEvent>`；为兼容单测与无异步运行时环境，
/// 本 trait 采用同步 `Vec<ChatEvent>` 接口（事件已在外层 tokio 线程收集完毕）。
/// 迁移到真实 SSE 时，外层把流收集成 Vec 再传入即可，trait 不变。
pub trait Provider: Send + Sync {
    /// 执行一次流式聊天，返回有序事件列表。
    fn stream_chat(&self, req: &ProviderRequest) -> Vec<ChatEvent>;
}

/// 内存 Mock Provider：按预设脚本回放事件，用于 Agent 循环单测。
pub struct MockProvider {
    /// 预设事件序列（每次 stream_chat 调用消费一组）。
    pub scripts: std::sync::Mutex<Vec<Vec<ChatEvent>>>,
}

impl MockProvider {
    pub fn new(scripts: Vec<Vec<ChatEvent>>) -> Self {
        Self {
            scripts: std::sync::Mutex::new(scripts),
        }
    }

    /// 单脚本便利构造。
    pub fn single(events: Vec<ChatEvent>) -> Self {
        Self::new(vec![events])
    }
}

impl Provider for MockProvider {
    fn stream_chat(&self, _req: &ProviderRequest) -> Vec<ChatEvent> {
        let mut guard = self.scripts.lock().unwrap();
        if guard.is_empty() {
            // 无脚本：返回空 Done
            return vec![ChatEvent::Done(Message::assistant(""))];
        }
        guard.remove(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_provider_replays_script() {
        let p = MockProvider::single(vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::Done(Message::assistant("Hello")),
        ]);
        let req = ProviderRequest {
            messages: vec![Message::user("hi")],
            model: "test".into(),
            tools: ToolRegistry::default(),
            system_prompt_extra: None,
        };
        let events = p.stream_chat(&req);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ChatEvent::TextDelta(_)));
    }

    #[test]
    fn mock_provider_consumes_scripts_in_order() {
        let p = MockProvider::new(vec![
            vec![ChatEvent::Done(Message::assistant("first"))],
            vec![ChatEvent::Done(Message::assistant("second"))],
        ]);
        let req = ProviderRequest {
            messages: vec![],
            model: "test".into(),
            tools: ToolRegistry::default(),
            system_prompt_extra: None,
        };
        let e1 = p.stream_chat(&req);
        let e2 = p.stream_chat(&req);
        assert!(matches!(e1.last(), Some(ChatEvent::Done(m)) if m.content == "first"));
        assert!(matches!(e2.last(), Some(ChatEvent::Done(m)) if m.content == "second"));
    }
}
