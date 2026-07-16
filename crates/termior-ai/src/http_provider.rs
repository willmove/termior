//! Real HTTP/SSE adapters for cloud and local model providers (FR-PROV-01/02/05).

use crate::message::{ChatEvent, Message, Role, ToolCall};
use crate::model_registry::ProviderKind;
use crate::provider::{Provider, ProviderRequest};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::net::ToSocketAddrs;
use std::time::Duration;
use termior_security::ssrf::{check_ip, SsrfGuard};
use url::Url;

/// Runtime-only provider configuration. It intentionally does not implement Serialize so an API
/// key cannot enter settings/session JSON (INV-5).
#[derive(Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub local: bool,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("local", &self.local)
            .finish()
    }
}

impl ProviderConfig {
    pub fn for_kind(kind: ProviderKind, model: impl Into<String>) -> Self {
        let (base_url, local) = match kind {
            ProviderKind::Anthropic => ("https://api.anthropic.com/v1", false),
            ProviderKind::OpenAi => ("https://api.openai.com/v1", false),
            ProviderKind::Google => ("https://generativelanguage.googleapis.com/v1beta", false),
            ProviderKind::Groq => ("https://api.groq.com/openai/v1", false),
            ProviderKind::Xai => ("https://api.x.ai/v1", false),
            ProviderKind::Cerebras => ("https://api.cerebras.ai/v1", false),
            ProviderKind::OpenRouter => ("https://openrouter.ai/api/v1", false),
            ProviderKind::DeepSeek => ("https://api.deepseek.com/v1", false),
            ProviderKind::Mistral => ("https://api.mistral.ai/v1", false),
            ProviderKind::OpenAiCompatible => ("https://api.openai.com/v1", false),
            ProviderKind::LmStudio => ("http://127.0.0.1:1234/v1", true),
            ProviderKind::Mlx => ("http://127.0.0.1:8080/v1", true),
            ProviderKind::Ollama => ("http://127.0.0.1:11434", true),
        };
        Self {
            kind,
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            local,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn from_settings(
        profile: &termior_store::ModelProviderSettings,
        api_key: Option<String>,
    ) -> Result<Self, ProviderTransportError> {
        let kind = ProviderKind::from_settings_id(&profile.provider).ok_or_else(|| {
            ProviderTransportError::Response(format!(
                "unknown provider kind in settings: {}",
                profile.provider
            ))
        })?;
        Ok(Self {
            kind,
            base_url: profile.base_url.clone(),
            model: profile.model.clone(),
            api_key,
            local: profile.local,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderTransportError {
    #[error("provider URL denied: {0}")]
    Denied(String),
    #[error("invalid provider URL: {0}")]
    InvalidUrl(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("response I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider response error: {0}")]
    Response(String),
}

pub struct HttpProvider {
    config: ProviderConfig,
    client: Client,
    guard: SsrfGuard,
}

impl std::fmt::Debug for HttpProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpProvider")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl HttpProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderTransportError> {
        let allowed = if config.local {
            vec![config.base_url.clone()]
        } else {
            Vec::new()
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .user_agent(concat!("Termior/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            config,
            client,
            guard: SsrfGuard::new(allowed),
        })
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    pub fn ping(&self) -> Result<Duration, ProviderTransportError> {
        self.check_endpoint(&self.config.base_url)?;
        let started = std::time::Instant::now();
        self.client
            .get(&self.config.base_url)
            .send()?
            .error_for_status()?;
        Ok(started.elapsed())
    }

    fn send(&self, req: &ProviderRequest) -> Result<Vec<ChatEvent>, ProviderTransportError> {
        let model = if req.model == "default" || req.model.is_empty() {
            &self.config.model
        } else {
            &req.model
        };
        match self.config.kind {
            ProviderKind::Anthropic => self.send_anthropic(req, model),
            ProviderKind::Google => self.send_google(req, model),
            ProviderKind::Ollama => self.send_ollama(req, model),
            _ => self.send_openai(req, model),
        }
    }

    fn send_openai(
        &self,
        req: &ProviderRequest,
        model: &str,
    ) -> Result<Vec<ChatEvent>, ProviderTransportError> {
        let endpoint = join_endpoint(&self.config.base_url, "chat/completions")?;
        self.check_endpoint(endpoint.as_str())?;
        let body = json!({
            "model": model,
            "stream": true,
            "messages": openai_messages(&req.messages, req.system_prompt_extra.as_deref()),
            "tools": openai_tools(req),
        });
        let response = self
            .client
            .post(endpoint)
            .headers(self.headers(false)?)
            .json(&body)
            .send()?
            .error_for_status()?;
        parse_sse(response, OpenAiAccumulator::default())
    }

    fn send_anthropic(
        &self,
        req: &ProviderRequest,
        model: &str,
    ) -> Result<Vec<ChatEvent>, ProviderTransportError> {
        let endpoint = join_endpoint(&self.config.base_url, "messages")?;
        self.check_endpoint(endpoint.as_str())?;
        let (system, messages) =
            anthropic_messages(&req.messages, req.system_prompt_extra.as_deref());
        let body = json!({
            "model": model,
            "max_tokens": 8192,
            "stream": true,
            "system": system,
            "messages": messages,
            "tools": anthropic_tools(req),
        });
        let response = self
            .client
            .post(endpoint)
            .headers(self.headers(true)?)
            .json(&body)
            .send()?
            .error_for_status()?;
        parse_sse(response, AnthropicAccumulator::default())
    }

    fn send_google(
        &self,
        req: &ProviderRequest,
        model: &str,
    ) -> Result<Vec<ChatEvent>, ProviderTransportError> {
        let endpoint = join_endpoint(
            &self.config.base_url,
            &format!("models/{model}:streamGenerateContent?alt=sse"),
        )?;
        self.check_endpoint(endpoint.as_str())?;
        let body = google_body(req);
        let response = self
            .client
            .post(endpoint)
            .headers(self.headers(false)?)
            .json(&body)
            .send()?
            .error_for_status()?;
        parse_sse(response, GoogleAccumulator::default())
    }

    fn send_ollama(
        &self,
        req: &ProviderRequest,
        model: &str,
    ) -> Result<Vec<ChatEvent>, ProviderTransportError> {
        let endpoint = join_endpoint(&self.config.base_url, "api/chat")?;
        self.check_endpoint(endpoint.as_str())?;
        let body = json!({
            "model": model,
            "stream": true,
            "messages": openai_messages(&req.messages, req.system_prompt_extra.as_deref()),
            "tools": openai_tools(req),
        });
        let response = self
            .client
            .post(endpoint)
            .headers(self.headers(false)?)
            .json(&body)
            .send()?
            .error_for_status()?;
        parse_ndjson(response, OllamaAccumulator::default())
    }

    fn headers(&self, anthropic: bool) -> Result<HeaderMap, ProviderTransportError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(key) = self.config.api_key.as_deref() {
            let value = HeaderValue::from_str(key)
                .map_err(|_| ProviderTransportError::Response("invalid API key header".into()))?;
            if anthropic {
                headers.insert("x-api-key", value);
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            } else if self.config.kind == ProviderKind::Google {
                headers.insert("x-goog-api-key", value);
            } else {
                let bearer = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                    ProviderTransportError::Response("invalid API key header".into())
                })?;
                headers.insert(AUTHORIZATION, bearer);
            }
        }
        Ok(headers)
    }

    fn check_endpoint(&self, endpoint: &str) -> Result<(), ProviderTransportError> {
        self.guard
            .check(endpoint)
            .map_err(|error| ProviderTransportError::Denied(error.to_string()))?;
        let url = Url::parse(endpoint)
            .map_err(|_| ProviderTransportError::InvalidUrl(endpoint.to_owned()))?;
        if !self.config.local {
            let host = url
                .host_str()
                .ok_or_else(|| ProviderTransportError::InvalidUrl(endpoint.to_owned()))?;
            let port = url.port_or_known_default().unwrap_or(443);
            for address in (host, port)
                .to_socket_addrs()
                .map_err(ProviderTransportError::Io)?
            {
                check_ip(&address.ip(), host)
                    .map_err(|error| ProviderTransportError::Denied(error.to_string()))?;
            }
        }
        Ok(())
    }
}

impl Provider for HttpProvider {
    fn stream_chat(&self, req: &ProviderRequest) -> Vec<ChatEvent> {
        self.send(req)
            .unwrap_or_else(|error| vec![ChatEvent::Error(error.to_string())])
    }
}

fn join_endpoint(base: &str, suffix: &str) -> Result<Url, ProviderTransportError> {
    let base = format!("{}/", base.trim_end_matches('/'));
    Url::parse(&base)
        .and_then(|url| url.join(suffix))
        .map_err(|_| ProviderTransportError::InvalidUrl(base))
}

fn openai_messages(messages: &[Message], extra: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(extra) = extra.filter(|value| !value.is_empty()) {
        out.push(json!({"role": "system", "content": extra}));
    }
    for message in messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut value = json!({"role": role, "content": message.content});
        if !message.tool_calls.is_empty() {
            value["tool_calls"] = Value::Array(
                message
                    .tool_calls
                    .iter()
                    .map(|call| json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}}))
                    .collect(),
            );
        }
        if let Some(result) = &message.tool_result {
            value["tool_call_id"] = json!(result.call_id);
            value["content"] = json!(result.output);
        }
        out.push(value);
    }
    out
}

fn openai_tools(req: &ProviderRequest) -> Vec<Value> {
    req.tools
        .descriptors()
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": {"type": "object", "additionalProperties": true}
                }
            })
        })
        .collect()
}

fn anthropic_messages(messages: &[Message], extra: Option<&str>) -> (String, Vec<Value>) {
    let mut system = extra.unwrap_or_default().to_owned();
    let mut out = Vec::new();
    for message in messages {
        if message.role == Role::System {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(&message.content);
            continue;
        }
        match message.role {
            Role::Tool => {
                if let Some(result) = &message.tool_result {
                    out.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": result.call_id, "content": result.output, "is_error": !result.ok}]}));
                }
            }
            Role::Assistant if !message.tool_calls.is_empty() => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({"type": "text", "text": message.content}));
                }
                content.extend(message.tool_calls.iter().map(|call| {
                    let input = serde_json::from_str::<Value>(&call.arguments)
                        .unwrap_or_else(|_| json!({}));
                    json!({"type": "tool_use", "id": call.id, "name": call.name, "input": input})
                }));
                out.push(json!({"role": "assistant", "content": content}));
            }
            Role::Assistant => out.push(json!({"role": "assistant", "content": message.content})),
            Role::User => out.push(json!({"role": "user", "content": message.content})),
            Role::System => {}
        }
    }
    (system, out)
}

fn anthropic_tools(req: &ProviderRequest) -> Vec<Value> {
    req.tools
        .descriptors()
        .into_iter()
        .map(|tool| json!({"name": tool.name, "description": tool.description, "input_schema": {"type": "object", "additionalProperties": true}}))
        .collect()
}

fn google_body(req: &ProviderRequest) -> Value {
    let mut contents = Vec::new();
    let mut system = req.system_prompt_extra.clone().unwrap_or_default();
    for message in &req.messages {
        match message.role {
            Role::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&message.content);
            }
            Role::User | Role::Assistant => contents.push(json!({
                "role": if message.role == Role::Assistant { "model" } else { "user" },
                "parts": [{"text": message.content}]
            })),
            Role::Tool => {}
        }
    }
    json!({
        "systemInstruction": {"parts": [{"text": system}]},
        "contents": contents,
        "tools": [{"functionDeclarations": req.tools.descriptors().into_iter().map(|tool| json!({"name": tool.name, "description": tool.description, "parameters": {"type": "object"}})).collect::<Vec<_>>()}]
    })
}

trait StreamAccumulator: Default {
    fn push_json(&mut self, value: Value, events: &mut Vec<ChatEvent>);
    fn finish(self, events: &mut Vec<ChatEvent>);
}

fn parse_sse<A: StreamAccumulator>(
    response: Response,
    accumulator: A,
) -> Result<Vec<ChatEvent>, ProviderTransportError> {
    parse_sse_lines(BufReader::new(response), accumulator)
}

fn parse_sse_lines<R: BufRead, A: StreamAccumulator>(
    reader: R,
    mut accumulator: A,
) -> Result<Vec<ChatEvent>, ProviderTransportError> {
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            accumulator.push_json(value, &mut events);
        }
    }
    accumulator.finish(&mut events);
    Ok(events)
}

fn parse_ndjson<A: StreamAccumulator>(
    response: Response,
    accumulator: A,
) -> Result<Vec<ChatEvent>, ProviderTransportError> {
    parse_ndjson_lines(BufReader::new(response), accumulator)
}

fn parse_ndjson_lines<R: BufRead, A: StreamAccumulator>(
    reader: R,
    mut accumulator: A,
) -> Result<Vec<ChatEvent>, ProviderTransportError> {
    let mut events = Vec::new();
    for line in reader.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(&line?) {
            accumulator.push_json(value, &mut events);
        }
    }
    accumulator.finish(&mut events);
    Ok(events)
}

#[derive(Default)]
struct OpenAiAccumulator {
    text: String,
    tools: Vec<ToolCall>,
}

impl StreamAccumulator for OpenAiAccumulator {
    fn push_json(&mut self, value: Value, events: &mut Vec<ChatEvent>) {
        let Some(delta) = value.pointer("/choices/0/delta") else {
            return;
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            self.text.push_str(text);
            events.push(ChatEvent::TextDelta(text.to_owned()));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while self.tools.len() <= index {
                    self.tools.push(ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                }
                let target = &mut self.tools[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    target.id.push_str(id);
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    target.name.push_str(name);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    target.arguments.push_str(args);
                }
            }
        }
    }

    fn finish(self, events: &mut Vec<ChatEvent>) {
        for call in &self.tools {
            events.push(ChatEvent::ToolCall(call.clone()));
        }
        events.push(ChatEvent::Done(Message {
            role: Role::Assistant,
            content: self.text,
            tool_calls: self.tools,
            tool_result: None,
        }));
    }
}

#[derive(Default)]
struct AnthropicAccumulator {
    text: String,
    tools: Vec<ToolCall>,
    active_tool: Option<usize>,
}

impl StreamAccumulator for AnthropicAccumulator {
    fn push_json(&mut self, value: Value, events: &mut Vec<ChatEvent>) {
        match value.get("type").and_then(Value::as_str) {
            Some("content_block_start")
                if value.pointer("/content_block/type").and_then(Value::as_str)
                    == Some("tool_use") =>
            {
                self.tools.push(ToolCall {
                    id: value
                        .pointer("/content_block/id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: value
                        .pointer("/content_block/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: String::new(),
                });
                self.active_tool = Some(self.tools.len() - 1);
            }
            Some("content_block_delta") => {
                if let Some(text) = value.pointer("/delta/text").and_then(Value::as_str) {
                    self.text.push_str(text);
                    events.push(ChatEvent::TextDelta(text.to_owned()));
                }
                if let (Some(index), Some(partial)) = (
                    self.active_tool,
                    value.pointer("/delta/partial_json").and_then(Value::as_str),
                ) {
                    self.tools[index].arguments.push_str(partial);
                }
            }
            Some("content_block_stop") => self.active_tool = None,
            Some("error") => events.push(ChatEvent::Error(value.to_string())),
            _ => {}
        }
    }

    fn finish(self, events: &mut Vec<ChatEvent>) {
        for call in &self.tools {
            events.push(ChatEvent::ToolCall(call.clone()));
        }
        events.push(ChatEvent::Done(Message {
            role: Role::Assistant,
            content: self.text,
            tool_calls: self.tools,
            tool_result: None,
        }));
    }
}

#[derive(Default)]
struct GoogleAccumulator {
    text: String,
    tools: Vec<ToolCall>,
}

impl StreamAccumulator for GoogleAccumulator {
    fn push_json(&mut self, value: Value, events: &mut Vec<ChatEvent>) {
        let Some(parts) = value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
        else {
            return;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                self.text.push_str(text);
                events.push(ChatEvent::TextDelta(text.to_owned()));
            }
            if let Some(call) = part.get("functionCall") {
                self.tools.push(ToolCall {
                    id: format!("gemini-{}", self.tools.len()),
                    name: call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string(),
                });
            }
        }
    }

    fn finish(self, events: &mut Vec<ChatEvent>) {
        for call in &self.tools {
            events.push(ChatEvent::ToolCall(call.clone()));
        }
        events.push(ChatEvent::Done(Message {
            role: Role::Assistant,
            content: self.text,
            tool_calls: self.tools,
            tool_result: None,
        }));
    }
}

#[derive(Default)]
struct OllamaAccumulator {
    text: String,
    tools: Vec<ToolCall>,
}

impl StreamAccumulator for OllamaAccumulator {
    fn push_json(&mut self, value: Value, events: &mut Vec<ChatEvent>) {
        if let Some(text) = value.pointer("/message/content").and_then(Value::as_str) {
            self.text.push_str(text);
            events.push(ChatEvent::TextDelta(text.to_owned()));
        }
        if let Some(calls) = value
            .pointer("/message/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                self.tools.push(ToolCall {
                    id: format!("ollama-{}", self.tools.len()),
                    name: call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    arguments: call
                        .pointer("/function/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string(),
                });
            }
        }
    }

    fn finish(self, events: &mut Vec<ChatEvent>) {
        for call in &self.tools {
            events.push(ChatEvent::ToolCall(call.clone()));
        }
        events.push(ChatEvent::Done(Message {
            role: Role::Assistant,
            content: self.text,
            tool_calls: self.tools,
            tool_result: None,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn config_debug_redacts_key() {
        let config =
            ProviderConfig::for_kind(ProviderKind::OpenAi, "gpt").with_api_key("sk-secret-value");
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk-secret-value"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn openai_sse_accumulates_text_and_fragmented_tool_call() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n"
        );
        let events = parse_sse_lines(Cursor::new(input), OpenAiAccumulator::default()).unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, ChatEvent::TextDelta(text) if text == "Hi ")));
        let ChatEvent::Done(message) = events.last().unwrap() else {
            panic!()
        };
        assert_eq!(message.tool_calls[0].name, "read_file");
        assert_eq!(message.tool_calls[0].arguments, "{\"path\":\"a\"}");
    }

    #[test]
    fn anthropic_sse_accumulates_partial_json() {
        let input = concat!(
            "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read_file\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            "data: {\"type\":\"content_block_stop\"}\n"
        );
        let events = parse_sse_lines(Cursor::new(input), AnthropicAccumulator::default()).unwrap();
        let ChatEvent::Done(message) = events.last().unwrap() else {
            panic!()
        };
        assert_eq!(message.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn custom_local_base_is_whitelisted_but_other_loopback_is_not() {
        let provider = HttpProvider::new(
            ProviderConfig::for_kind(ProviderKind::OpenAiCompatible, "local")
                .with_base_url("http://127.0.0.1:9999/v1"),
        )
        .unwrap();
        // Config was not marked local, so the guard denies it.
        assert!(provider
            .check_endpoint("http://127.0.0.1:9999/v1/chat/completions")
            .is_err());
        let mut config = ProviderConfig::for_kind(ProviderKind::LmStudio, "local");
        config.base_url = "http://127.0.0.1:9999/v1".into();
        let local = HttpProvider::new(config).unwrap();
        assert!(local
            .check_endpoint("http://127.0.0.1:9999/v1/chat/completions")
            .is_ok());
    }
}
