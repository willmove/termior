//! 行内补全逻辑（FR-EDIT-05）。
//!
//! 本模块是纯编排层：基于 [`Provider`] 抽象发起一次补全请求，把流式增量累积成
//! 最终的 ghost text。debounce 调度、ghost text 渲染、Tab/Esc 键位等 GPUI 侧的
//! 接线由应用 crate（`termior::editor_view`）持有，网络错误在此处被归一成
//! [`InlineCompletionResult::Error`]，由调用方静默降级（FR-EDIT-05：
//! 「请求失败/超时静默降级，不打扰输入」）。
//!
//! 复用 FR-PROV 的 Provider 与密钥配置：调用方把已配置好 base URL / 模型 / key 的
//! `Provider`（通常是 `HttpProvider`）注入 [`InlineCompleter`]，本模块不再触碰密钥
//! 或 SSRF（那些在 `HttpProvider` 内部完成）。

use crate::message::{ChatEvent, Message};
use crate::provider::{Provider, ProviderRequest};
use crate::tools::ToolRegistry;
use futures::StreamExt;

/// 一次行内补全请求的上下文：光标前后的文本与可选的语言/路径提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineCompletionContext {
    /// 光标之前的文本（来自 Rope 缓冲）。
    pub prefix: String,
    /// 光标之后的文本（来自 Rope 缓冲）。
    pub suffix: String,
    /// 检测到的语言 id（如 `"rust"`），作为模型提示。
    pub language: Option<String>,
    /// 相对工作区根的文件路径，作为模型提示。
    pub path: Option<String>,
}

/// 一次补全请求的结果（流累积 + 归一化之后）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineCompletionResult {
    /// 非空补全文本。
    Completed(String),
    /// Provider 没有返回可用文本。
    Empty,
    /// 请求失败（网络/超时/解析）；调用方静默降级。
    Error(String),
}

/// 注入到 system 提示词的补全行为约束（追加段，经 `system_prompt_extra` 传递）。
///
/// 引导 chat 模型产出「只含待插入文本、不含已输入内容、不含解释」的补全。
pub const INLINE_COMPLETION_SYSTEM_PROMPT: &str = "You are an inline code completion service. \
Return ONLY the text that should be inserted at the cursor. Do not repeat text the user already typed, \
do not wrap the answer in a code block, and do not add any explanation or surrounding quotes.";

/// 行内补全编排器：持有一个已配置好的 Provider 与补全模型 id。
pub struct InlineCompleter {
    provider: Box<dyn Provider>,
    model: String,
}

impl InlineCompleter {
    pub fn new(provider: Box<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    /// 构造一次补全请求的消息历史（单条 user 消息）。
    pub fn build_messages(&self, ctx: &InlineCompletionContext) -> Vec<Message> {
        vec![Message::user(completion_user_prompt(ctx))]
    }

    /// 发起一次补全请求并累积流式增量，返回归一化后的结果。
    ///
    /// 任何 `ChatEvent::Error` 都归一为 [`InlineCompletionResult::Error`]；
    /// 流自然结束后若累积文本经归一化为空则返回 [`InlineCompletionResult::Empty`]。
    pub async fn complete(&self, ctx: &InlineCompletionContext) -> InlineCompletionResult {
        let req = ProviderRequest {
            messages: self.build_messages(ctx),
            model: self.model.clone(),
            tools: ToolRegistry::default(),
            system_prompt_extra: Some(INLINE_COMPLETION_SYSTEM_PROMPT.to_owned()),
        };
        let mut stream = self.provider.stream_chat(&req);
        let mut text = String::new();
        let mut err: Option<String> = None;
        while let Some(event) = stream.next().await {
            match event {
                ChatEvent::TextDelta(delta) => text.push_str(&delta),
                ChatEvent::Error(message) => err = Some(message),
                _ => {}
            }
        }
        if let Some(message) = err {
            return InlineCompletionResult::Error(message);
        }
        let normalized = normalize_completion(ctx, &text);
        if normalized.is_empty() {
            InlineCompletionResult::Empty
        } else {
            InlineCompletionResult::Completed(normalized)
        }
    }
}

/// 构造补全请求的 user 提示词：包含语言/路径提示、前缀与后缀。
pub fn completion_user_prompt(ctx: &InlineCompletionContext) -> String {
    let mut prompt = String::new();
    if let Some(language) = ctx.language.as_deref() {
        prompt.push_str(&format!("Language: {language}\n"));
    }
    if let Some(path) = ctx.path.as_deref() {
        prompt.push_str(&format!("Path: {path}\n"));
    }
    prompt.push_str("Complete the code at the cursor (<cursor/>). ");
    prompt.push_str("Return only the insertion.\n\n");
    prompt.push_str("```\n");
    prompt.push_str(&ctx.prefix);
    prompt.push_str("<cursor/>");
    prompt.push_str(&ctx.suffix);
    prompt.push_str("\n```");
    prompt
}

/// 归一化模型返回的原始补全文本：
/// 1. 剥离单层 markdown 代码围栏（含可选语言标记）；
/// 2. 去除首尾空白；
/// 3. 去掉与「当前行已输入尾部」重复的前缀，避免把用户刚敲的字再插一遍。
pub fn normalize_completion(ctx: &InlineCompletionContext, raw: &str) -> String {
    let stripped = strip_code_fence(raw).trim().to_owned();
    if stripped.is_empty() {
        return String::new();
    }
    let current_line_tail = ctx
        .prefix
        .rsplit_once('\n')
        .map(|(_, tail)| tail)
        .unwrap_or(&ctx.prefix);
    drop_common_prefix(&stripped, current_line_tail)
}

/// 若 `raw` 被单个三反引号围栏包裹（开头围栏可带语言标记），返回围栏内部文本；
/// 否则原样返回。仅处理首尾各最多一个围栏，避免误伤多行内容。
fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.strip_prefix('\n').unwrap_or(raw);
    let Some(rest) = trimmed.strip_prefix("```") else {
        return raw;
    };
    // 跳过开围栏同行剩余的语言标记/空白，再跳过一个换行。
    let rest = rest.split_once('\n').map(|(_, after)| after).unwrap_or("");
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(inner) = rest.strip_suffix("```") else {
        return raw;
    };
    inner.strip_suffix('\n').unwrap_or(inner)
}

/// 去掉 `completion` 与 `already_typed` 的最长公共字符前缀，避免重复插入已输入文本。
fn drop_common_prefix(completion: &str, already_typed: &str) -> String {
    let mut completion_chars = completion.chars();
    let mut typed_chars = already_typed.chars();
    let mut consumed = 0usize;
    loop {
        match (completion_chars.next(), typed_chars.next()) {
            (Some(a), Some(b)) if a == b => consumed += a.len_utf8(),
            _ => break,
        }
    }
    completion[consumed..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Role};
    use crate::provider::{MockProvider, ProviderRequest};
    use futures::executor::block_on;
    use futures::stream::BoxStream;

    /// 一个始终返回 Error 事件的 Provider，用于验证错误归一。
    struct ErrorProvider;

    impl Provider for ErrorProvider {
        fn stream_chat<'a>(&'a self, _req: &'a ProviderRequest) -> BoxStream<'a, ChatEvent> {
            Box::pin(futures::stream::iter(vec![ChatEvent::Error("boom".into())]))
        }
    }

    fn ctx(prefix: &str, suffix: &str) -> InlineCompletionContext {
        InlineCompletionContext {
            prefix: prefix.into(),
            suffix: suffix.into(),
            language: Some("rust".into()),
            path: Some("src/main.rs".into()),
        }
    }

    #[test]
    fn build_messages_is_single_user_message_with_prefix_and_suffix() {
        let completer = InlineCompleter::new(Box::new(MockProvider::single(vec![])), "model");
        let messages = completer.build_messages(&ctx("fn main() {", "}"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        let prompt = &messages[0].content;
        assert!(prompt.contains("fn main() {"));
        assert!(prompt.contains("<cursor/>"));
        assert!(prompt.contains('}'));
        assert!(prompt.contains("Language: rust"));
        assert!(prompt.contains("Path: src/main.rs"));
    }

    #[test]
    fn complete_accumulates_deltas_into_completed() {
        let provider = MockProvider::single(vec![
            ChatEvent::TextDelta("Hello".into()),
            ChatEvent::TextDelta(" world".into()),
            ChatEvent::Done(Message::assistant("Hello world")),
        ]);
        // 无当前行尾重复：prefix 末行为空。
        let c = InlineCompletionContext {
            prefix: "abc\n".into(),
            suffix: String::new(),
            language: None,
            path: None,
        };
        let completer = InlineCompleter::new(Box::new(provider), "m");
        match block_on(completer.complete(&c)) {
            InlineCompletionResult::Completed(text) => assert_eq!(text, "Hello world"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn complete_returns_empty_when_no_text() {
        let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant(""))]);
        let completer = InlineCompleter::new(Box::new(provider), "m");
        match block_on(completer.complete(&ctx("x", ""))) {
            InlineCompletionResult::Empty => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn complete_returns_error_on_error_event() {
        let completer = InlineCompleter::new(Box::new(ErrorProvider), "m");
        match block_on(completer.complete(&ctx("x", ""))) {
            InlineCompletionResult::Error(message) => assert_eq!(message, "boom"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn normalize_strips_code_fence_with_language_tag() {
        let raw = "```rust\nlet x = 1;\n```";
        let c = ctx("", "");
        assert_eq!(normalize_completion(&c, raw), "let x = 1;");
    }

    #[test]
    fn normalize_strips_plain_fence() {
        let raw = "```\nfoo()\n```";
        let c = ctx("", "");
        assert_eq!(normalize_completion(&c, raw), "foo()");
    }

    #[test]
    fn normalize_drops_current_line_echo_prefix() {
        // 用户已输入 `fn he`（当前行尾 = "fn he"），模型回显整行再补全。
        let raw = "fn hello() {}";
        let c = InlineCompletionContext {
            prefix: "use x;\nfn he".into(),
            suffix: String::new(),
            language: None,
            path: None,
        };
        assert_eq!(normalize_completion(&c, raw), "llo() {}");
    }

    #[test]
    fn normalize_keeps_content_when_no_overlap() {
        let raw = "world";
        let c = InlineCompletionContext {
            prefix: "hello ".into(),
            suffix: String::new(),
            language: None,
            path: None,
        };
        assert_eq!(normalize_completion(&c, raw), "world");
    }

    #[test]
    fn normalize_empty_input_yields_empty() {
        let c = ctx("abc", "");
        assert_eq!(normalize_completion(&c, "   "), "");
        assert_eq!(normalize_completion(&c, "```\n```"), "");
    }

    #[test]
    fn drop_common_prefix_handles_unicode() {
        // 中文字符前缀重复应按字符（而非字节）对齐剥离。
        let got = drop_common_prefix("你好世界", "你好");
        assert_eq!(got, "世界");
    }
}
