//! Provider 抽象层（FR-PROV）。
//!
//! `Provider` trait 统一不同模型服务的流式消息、工具调用与错误语义。
//! [`crate::http_provider::HttpProvider`] 负责各家请求/响应适配；本模块同时提供
//! [`MockProvider`]，用于不访问网络地验证 Agent 循环。

use crate::message::{ChatEvent, Message};
use crate::tools::ToolRegistry;
use futures::stream::BoxStream;

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
/// `stream_chat` 返回 [`ChatEvent`] 增量流：HTTP/SSE 由 Provider 在有界 worker
/// 池里读取，每解析出一个事件（`TextDelta`/`ToolCall`/`Done`/`Error`）即经
/// channel 投递给消费方。调用方逐事件 `await`；流自然结束时最后一个事件为
/// [`ChatEvent::Done`] 或 [`ChatEvent::Error`]。
pub trait Provider: Send + Sync {
    /// 执行一次流式聊天，返回有序事件增量流。
    fn stream_chat<'a>(&'a self, req: &'a ProviderRequest) -> BoxStream<'a, ChatEvent>;
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
    fn stream_chat<'a>(&'a self, _req: &'a ProviderRequest) -> BoxStream<'a, ChatEvent> {
        let events = {
            let mut guard = self.scripts.lock().unwrap();
            if guard.is_empty() {
                vec![ChatEvent::Done(Message::assistant(""))]
            } else {
                guard.remove(0)
            }
        };
        Box::pin(futures::stream::iter(events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use futures::StreamExt;

    async fn collect_stream(s: BoxStream<'_, ChatEvent>) -> Vec<ChatEvent> {
        s.collect().await
    }

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
        let events = block_on(collect_stream(p.stream_chat(&req)));
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
        let e1 = block_on(collect_stream(p.stream_chat(&req)));
        let e2 = block_on(collect_stream(p.stream_chat(&req)));
        assert!(matches!(e1.last(), Some(ChatEvent::Done(m)) if m.content == "first"));
        assert!(matches!(e2.last(), Some(ChatEvent::Done(m)) if m.content == "second"));
    }
}
