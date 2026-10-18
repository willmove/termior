//! Minimal MCP 2025-06-18 domain layer. Transports remain isolated per server.
//!
//! Server prose is untrusted: it is length- and character-limited for local display and is never
//! copied into the model-facing tool description. One server disconnecting marks only that
//! server's tools unavailable.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{BackendError, StdioCapture, StdioTransport, TransportCommand};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const TOOLS_LIST_CHANGED: &str = "notifications/tools/list_changed";
/// Local cap. A server declaration cannot raise it.
pub const MCP_MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
pub const MCP_DESCRIPTION_MAX_CHARS: usize = 256;
const MCP_TOOL_NAME_MAX_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    Stdio {
        program: String,
        args: Vec<String>,
    },
    StreamableHttp {
        url: String,
        oauth_key_ref: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolDeclaration {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualifiedMcpTool {
    pub qualified_id: String,
    pub server_id: String,
    pub name: String,
    pub untrusted_description: String,
    pub input_schema: Value,
    pub requires_approval: bool,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
}

impl QualifiedMcpTool {
    pub fn from_declaration(
        server_id: &str,
        declaration: McpToolDeclaration,
        locally_auto_approved: &BTreeSet<String>,
    ) -> Self {
        let qualified_id = format!("mcp::{server_id}::{}", declaration.name);
        Self {
            requires_approval: !locally_auto_approved.contains(&qualified_id),
            qualified_id,
            server_id: server_id.into(),
            name: declaration.name,
            untrusted_description: sanitize_untrusted_description(&declaration.description),
            input_schema: close_object_schema(declaration.input_schema),
            timeout_ms: 30_000,
            max_output_bytes: MCP_MAX_OUTPUT_BYTES,
        }
    }

    /// Model-facing text. Server-authored description bytes are not included.
    pub fn trusted_prompt_text(&self) -> String {
        format!(
            "MCP tool `{}` from server `{}`. Server-supplied descriptions are untrusted and are excluded from this prompt.",
            self.name, self.server_id
        )
    }

    pub fn to_tool_contract(&self) -> termior_ai::ToolContract {
        termior_ai::ToolContract {
            name: self.qualified_id.clone(),
            level: if self.requires_approval {
                termior_ai::ToolLevelSerde::Approval
            } else {
                termior_ai::ToolLevelSerde::Auto
            },
            description: self.trusted_prompt_text(),
            parameters: self.input_schema.clone(),
            side_effect: termior_ai::SideEffectClass::External,
            approval: if self.requires_approval {
                termior_ai::ApprovalClass::User
            } else {
                termior_ai::ApprovalClass::Automatic
            },
            default_timeout_ms: self.timeout_ms,
            max_output_bytes: self.max_output_bytes,
            idempotency: termior_ai::Idempotency::Unknown,
            parallel_safe: false,
        }
    }
}

/// Tools accepted from one server after duplicate and name checks.
#[derive(Debug, Clone, PartialEq)]
pub struct McpIngest {
    pub tools: Vec<QualifiedMcpTool>,
    pub duplicate_names: Vec<String>,
    pub rejected_names: Vec<String>,
}

pub fn ingest_tool_declarations(
    server_id: &str,
    declarations: Vec<McpToolDeclaration>,
    locally_auto_approved: &BTreeSet<String>,
) -> McpIngest {
    let mut tools = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicate_names = Vec::new();
    let mut rejected_names = Vec::new();
    for declaration in declarations {
        if !valid_tool_name(&declaration.name) || !usable_object_schema(&declaration.input_schema) {
            rejected_names.push(declaration.name);
            continue;
        }
        if !seen.insert(declaration.name.clone()) {
            duplicate_names.push(declaration.name);
            continue;
        }
        tools.push(QualifiedMcpTool::from_declaration(
            server_id,
            declaration,
            locally_auto_approved,
        ));
    }
    McpIngest {
        tools,
        duplicate_names,
        rejected_names,
    }
}

#[derive(Debug, Default)]
pub struct McpCatalog {
    by_server: BTreeMap<String, BTreeMap<String, QualifiedMcpTool>>,
    failures: BTreeMap<String, String>,
    unavailable: BTreeSet<String>,
    duplicate_names: BTreeMap<String, Vec<String>>,
    rejected_names: BTreeMap<String, Vec<String>>,
    resources: BTreeMap<String, Vec<Value>>,
    rejected_resources: BTreeMap<String, Vec<String>>,
    diagnostic_log: Option<PathBuf>,
    known_secrets: Vec<String>,
}
impl McpCatalog {
    pub fn with_diagnostic_log(
        mut self,
        path: impl Into<PathBuf>,
        known_secrets: impl IntoIterator<Item = String>,
    ) -> Self {
        self.diagnostic_log = Some(path.into());
        self.known_secrets = known_secrets.into_iter().collect();
        self
    }

    pub fn set_known_secrets(&mut self, known_secrets: impl IntoIterator<Item = String>) {
        self.known_secrets = known_secrets.into_iter().collect();
    }

    pub fn set_diagnostic_log(&mut self, path: impl Into<PathBuf>) {
        self.diagnostic_log = Some(path.into());
    }

    pub fn replace_tools(&mut self, server_id: &str, tools: Vec<QualifiedMcpTool>) {
        self.failures.remove(server_id);
        self.unavailable.remove(server_id);
        self.duplicate_names.remove(server_id);
        self.rejected_names.remove(server_id);
        let mut map = BTreeMap::new();
        let mut duplicates = Vec::new();
        for tool in tools {
            if map.contains_key(&tool.name) {
                duplicates.push(tool.name);
                continue;
            }
            map.insert(tool.name.clone(), tool);
        }
        if !duplicates.is_empty() {
            self.duplicate_names.insert(server_id.into(), duplicates);
        }
        self.by_server.insert(server_id.into(), map);
    }

    pub fn note_ingest(&mut self, server_id: &str, ingest: &McpIngest) {
        if ingest.duplicate_names.is_empty() {
            self.duplicate_names.remove(server_id);
        } else {
            self.duplicate_names
                .insert(server_id.into(), ingest.duplicate_names.clone());
        }
        if ingest.rejected_names.is_empty() {
            self.rejected_names.remove(server_id);
        } else {
            self.rejected_names
                .insert(server_id.into(), ingest.rejected_names.clone());
        }
    }

    pub fn set_resources(&mut self, server_id: &str, kept: Vec<Value>, rejected: Vec<String>) {
        self.resources.insert(server_id.into(), kept);
        if rejected.is_empty() {
            self.rejected_resources.remove(server_id);
        } else {
            self.rejected_resources.insert(server_id.into(), rejected);
        }
    }

    pub fn mark_disconnected(&mut self, server_id: &str, error: impl Into<String>) {
        let error = error.into();
        let redacted = redact_text(&error, &self.known_secrets);
        if let Some(path) = &self.diagnostic_log {
            let line = format!("{server_id}\t{error}");
            let _ = termior_store::StreamingRedactor::new(self.known_secrets.clone())
                .append_record(path, &line);
        }
        self.unavailable.insert(server_id.into());
        self.failures.insert(server_id.into(), redacted);
    }

    pub fn tool(&self, qualified_id: &str) -> Option<&QualifiedMcpTool> {
        self.by_server
            .values()
            .flat_map(|tools| tools.values())
            .find(|tool| tool.qualified_id == qualified_id)
    }

    pub fn tool_mut(&mut self, qualified_id: &str) -> Option<&mut QualifiedMcpTool> {
        for tools in self.by_server.values_mut() {
            for tool in tools.values_mut() {
                if tool.qualified_id == qualified_id {
                    return Some(tool);
                }
            }
        }
        None
    }

    pub fn failure(&self, server_id: &str) -> Option<&str> {
        self.failures.get(server_id).map(String::as_str)
    }

    pub fn is_server_available(&self, server_id: &str) -> bool {
        self.by_server.contains_key(server_id) && !self.unavailable.contains(server_id)
    }

    pub fn tool_available(&self, qualified_id: &str) -> bool {
        self.tool(qualified_id)
            .is_some_and(|tool| self.is_server_available(&tool.server_id))
    }

    pub fn available_tools(&self) -> Vec<&QualifiedMcpTool> {
        self.by_server
            .iter()
            .filter(|(server_id, _)| !self.unavailable.contains(*server_id))
            .flat_map(|(_, tools)| tools.values())
            .collect()
    }

    pub fn qualified_ids_for_name(&self, name: &str) -> Vec<&str> {
        self.by_server
            .values()
            .filter_map(|tools| tools.get(name).map(|tool| tool.qualified_id.as_str()))
            .collect()
    }

    pub fn duplicate_names(&self, server_id: &str) -> &[String] {
        self.duplicate_names
            .get(server_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn rejected_names(&self, server_id: &str) -> &[String] {
        self.rejected_names
            .get(server_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn resources(&self, server_id: &str) -> &[Value] {
        self.resources
            .get(server_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn rejected_resources(&self, server_id: &str) -> &[String] {
        self.rejected_resources
            .get(server_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Text that may be placed in a system prompt. Unavailable tools and server prose are omitted.
    pub fn trusted_system_prompt(&self) -> String {
        self.available_tools()
            .into_iter()
            .map(QualifiedMcpTool::trusted_prompt_text)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthPkceRequest {
    pub authorization_endpoint: String,
    pub resource: String,
    pub code_challenge: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct McpDiscovery {
    pub tools: Vec<McpToolDeclaration>,
    #[serde(default)]
    pub rejected_tool_names: Vec<String>,
    pub resources: Vec<Value>,
    pub prompts: Vec<Value>,
}

pub struct McpStdioClient {
    server_id: String,
    transport: StdioTransport,
    known_secrets: Vec<String>,
    workspace_root: Option<PathBuf>,
}

#[derive(Clone)]
pub struct McpStdioToolHandler {
    client: Arc<Mutex<McpStdioClient>>,
    tool: QualifiedMcpTool,
}

impl McpStdioToolHandler {
    pub fn new(client: Arc<Mutex<McpStdioClient>>, tool: QualifiedMcpTool) -> Self {
        Self { client, tool }
    }
}

impl termior_ai::ExternalToolHandler for McpStdioToolHandler {
    fn call(&self, arguments: Value) -> Result<String, String> {
        let result = self
            .client
            .lock()
            .map_err(|_| "MCP client lock is poisoned".to_owned())?
            .call_tool(&self.tool, arguments)
            .map_err(|error| error.to_string())?;
        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}

impl McpStdioClient {
    pub fn connect(
        server_id: impl Into<String>,
        command: TransportCommand,
    ) -> Result<Self, BackendError> {
        Self::connect_capturing(server_id, command, StdioCapture::default())
    }

    pub fn connect_capturing(
        server_id: impl Into<String>,
        command: TransportCommand,
        capture: StdioCapture,
    ) -> Result<Self, BackendError> {
        let known_secrets = capture.known_secrets.clone();
        let mut transport = StdioTransport::spawn_capturing(&command, capture)?;
        let result = transport.request(
            "initialize",
            serde_json::json!({
                "protocolVersion":MCP_PROTOCOL_VERSION,
                "capabilities":{},
                "clientInfo":{"name":"termior","version":env!("CARGO_PKG_VERSION")}
            }),
            Duration::from_secs(30),
        )?;
        if result.get("protocolVersion").and_then(Value::as_str) != Some(MCP_PROTOCOL_VERSION) {
            return Err(BackendError::Protocol(
                "MCP server selected an unsupported protocol version".into(),
            ));
        }
        transport.notify("notifications/initialized", serde_json::json!({}))?;
        Ok(Self {
            server_id: server_id.into(),
            transport,
            known_secrets,
            workspace_root: None,
        })
    }

    pub fn set_workspace_root(&mut self, root: impl Into<PathBuf>) {
        self.workspace_root = Some(root.into());
    }

    pub fn add_known_secrets(&mut self, secrets: impl IntoIterator<Item = String>) {
        self.known_secrets.extend(secrets);
    }

    pub fn discover(&mut self) -> Result<McpDiscovery, BackendError> {
        let parsed = self.list_tools()?;
        let resources = self.transport.request(
            "resources/list",
            serde_json::json!({}),
            Duration::from_secs(30),
        )?;
        let prompts = self.transport.request(
            "prompts/list",
            serde_json::json!({}),
            Duration::from_secs(30),
        )?;
        Ok(McpDiscovery {
            tools: parsed.declarations,
            rejected_tool_names: parsed.rejected_names,
            resources: resources
                .get("resources")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            prompts: prompts
                .get("prompts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub fn list_tools(&mut self) -> Result<ParsedTools, BackendError> {
        let tools =
            self.transport
                .request("tools/list", serde_json::json!({}), Duration::from_secs(30))?;
        parse_tool_list(&tools)
    }

    pub fn qualified_tools(
        &mut self,
        locally_auto_approved: &BTreeSet<String>,
    ) -> Result<Vec<QualifiedMcpTool>, BackendError> {
        let server_id = self.server_id.clone();
        let tools = self.discover()?.tools;
        Ok(ingest_tool_declarations(&server_id, tools, locally_auto_approved).tools)
    }

    /// Read queued notifications. `idle` is how long to wait for the first one.
    pub fn drain_notifications(&mut self, idle: Duration) -> Result<Vec<Value>, BackendError> {
        let mut notes = Vec::new();
        let mut wait = idle;
        loop {
            match self.transport.next_event(wait) {
                Ok(Some(value)) => {
                    if value.get("id").is_none() && value.get("method").is_some() {
                        notes.push(value);
                    }
                    wait = Duration::ZERO;
                }
                Ok(None) => return Ok(notes),
                Err(error) => return Err(redact_backend_error(error, &self.known_secrets)),
            }
        }
    }

    /// When `notifications/tools/list_changed` is pending, rediscover that server's tools.
    pub fn refresh_after_list_changed(
        &mut self,
        locally_auto_approved: &BTreeSet<String>,
        idle: Duration,
    ) -> Result<Option<McpIngest>, BackendError> {
        let notes = self.drain_notifications(idle)?;
        let changed = notes
            .iter()
            .any(|note| note.get("method").and_then(Value::as_str) == Some(TOOLS_LIST_CHANGED));
        if !changed {
            return Ok(None);
        }
        let parsed = self.list_tools()?;
        let mut ingest =
            ingest_tool_declarations(&self.server_id, parsed.declarations, locally_auto_approved);
        ingest.rejected_names.extend(parsed.rejected_names);
        Ok(Some(ingest))
    }

    pub fn call_tool(
        &mut self,
        tool: &QualifiedMcpTool,
        arguments: Value,
    ) -> Result<Value, BackendError> {
        if tool.server_id != self.server_id {
            return Err(BackendError::Protocol(
                "MCP tool belongs to a different server connection".into(),
            ));
        }
        let result = self
            .transport
            .request(
                "tools/call",
                serde_json::json!({"name":tool.name,"arguments":arguments}),
                Duration::from_millis(tool.timeout_ms),
            )
            .map_err(|error| redact_backend_error(error, &self.known_secrets))?;
        bounded_tool_result(
            result,
            tool.max_output_bytes,
            self.workspace_root.as_deref(),
            &self.known_secrets,
        )
    }

    pub fn shutdown(mut self) -> Result<(), BackendError> {
        self.transport.shutdown(Duration::from_secs(2))
    }
}

pub struct McpHttpClient {
    endpoint: String,
    bearer: Option<String>,
    session_id: Option<String>,
    client: reqwest::blocking::Client,
    next_id: u64,
    known_secrets: Vec<String>,
    workspace_root: Option<PathBuf>,
}

impl McpHttpClient {
    pub fn new(endpoint: impl Into<String>, bearer: Option<String>) -> Result<Self, BackendError> {
        let endpoint = endpoint.into();
        if !endpoint.starts_with("https://")
            && !endpoint.starts_with("http://localhost")
            && !endpoint.starts_with("http://127.0.0.1")
        {
            return Err(BackendError::Transport(
                "remote MCP endpoint must use HTTPS".into(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        let mut known_secrets = Vec::new();
        if let Some(token) = bearer.clone().filter(|token| !token.is_empty()) {
            known_secrets.push(token);
        }
        Ok(Self {
            endpoint,
            bearer,
            session_id: None,
            client,
            next_id: 1,
            known_secrets,
            workspace_root: None,
        })
    }

    pub fn set_workspace_root(&mut self, root: impl Into<PathBuf>) {
        self.workspace_root = Some(root.into());
    }

    pub fn add_known_secrets(&mut self, secrets: impl IntoIterator<Item = String>) {
        self.known_secrets.extend(secrets);
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, BackendError> {
        let id = self.next_id;
        self.next_id += 1;
        self.request(id, method, params)
    }

    pub fn set_bearer(&mut self, bearer: Option<String>) {
        if let Some(token) = bearer.clone().filter(|token| !token.is_empty()) {
            self.known_secrets.push(token);
        }
        self.bearer = bearer;
    }

    pub fn initialize(&mut self) -> Result<(), BackendError> {
        let result = self.rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion":MCP_PROTOCOL_VERSION,
                "capabilities":{},
                "clientInfo":{"name":"termior","version":env!("CARGO_PKG_VERSION")}
            }),
        )?;
        if result.get("protocolVersion").and_then(Value::as_str) != Some(MCP_PROTOCOL_VERSION) {
            return Err(BackendError::Protocol(
                "MCP server selected an unsupported protocol version".into(),
            ));
        }
        self.notify("notifications/initialized", serde_json::json!({}))?;
        Ok(())
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), BackendError> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc":"2.0","method":method,"params":params}));
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        if let Some(session) = &self.session_id {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request
            .send()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(BackendError::Transport(format!(
                "MCP HTTP status {}",
                response.status()
            )))
        }
    }

    pub fn discover(&mut self) -> Result<McpDiscovery, BackendError> {
        let tools = self.rpc("tools/list", serde_json::json!({}))?;
        let resources = self.rpc("resources/list", serde_json::json!({}))?;
        let prompts = self.rpc("prompts/list", serde_json::json!({}))?;
        let parsed = parse_tool_list(&tools)?;
        Ok(McpDiscovery {
            tools: parsed.declarations,
            rejected_tool_names: parsed.rejected_names,
            resources: resources
                .get("resources")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            prompts: prompts
                .get("prompts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub fn call_tool(
        &mut self,
        tool: &QualifiedMcpTool,
        arguments: Value,
    ) -> Result<Value, BackendError> {
        let result = self
            .rpc(
                "tools/call",
                serde_json::json!({"name":tool.name,"arguments":arguments}),
            )
            .map_err(|error| redact_backend_error(error, &self.known_secrets))?;
        bounded_tool_result(
            result,
            tool.max_output_bytes,
            self.workspace_root.as_deref(),
            &self.known_secrets,
        )
    }

    pub fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, BackendError> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        if let Some(session) = &self.session_id {
            request = request.header("Mcp-Session-Id", session);
        }
        let response = request
            .send()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            self.session_id = Some(session.into());
        }
        if !response.status().is_success() {
            return Err(BackendError::Transport(format!(
                "MCP HTTP status {}",
                response.status()
            )));
        }
        let text = response
            .text()
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        let payload = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap_or(text.trim());
        let value: Value = serde_json::from_str(payload)
            .map_err(|error| BackendError::Protocol(error.to_string()))?;
        if let Some(error) = value.get("error") {
            return Err(BackendError::Protocol(error.to_string()));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| BackendError::Protocol("MCP response has no result".into()))
    }
}

#[derive(Clone)]
pub struct McpHttpToolHandler {
    client: Arc<Mutex<McpHttpClient>>,
    tool: QualifiedMcpTool,
}

impl McpHttpToolHandler {
    pub fn new(client: Arc<Mutex<McpHttpClient>>, tool: QualifiedMcpTool) -> Self {
        Self { client, tool }
    }
}

impl termior_ai::ExternalToolHandler for McpHttpToolHandler {
    fn call(&self, arguments: Value) -> Result<String, String> {
        let result = self
            .client
            .lock()
            .map_err(|_| "MCP HTTP client lock is poisoned".to_owned())?
            .call_tool(&self.tool, arguments)
            .map_err(|error| error.to_string())?;
        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}
impl OAuthPkceRequest {
    pub fn validate_callback(&self, state: &str, resource: &str) -> bool {
        !self.state.is_empty() && self.state == state && self.resource == resource
    }
}

fn close_object_schema(mut schema: Value) -> Value {
    strip_server_prose(&mut schema);
    if let Some(object) = schema.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some("object") {
            object.insert("additionalProperties".into(), Value::Bool(false));
        }
    }
    schema
}

/// Server prose inside a JSON Schema must not reach the model as instructions.
fn strip_server_prose(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["description", "title", "$comment", "examples"] {
                map.remove(key);
            }
            for child in map.values_mut() {
                strip_server_prose(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_server_prose(item);
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTools {
    pub declarations: Vec<McpToolDeclaration>,
    pub rejected_names: Vec<String>,
}

fn parse_tool_list(tools: &Value) -> Result<ParsedTools, BackendError> {
    let Some(value) = tools.get("tools") else {
        return Ok(ParsedTools {
            declarations: Vec::new(),
            rejected_names: Vec::new(),
        });
    };
    let array = value.as_array().ok_or_else(|| {
        BackendError::Protocol("MCP tools/list returned a non-array tools field".into())
    })?;
    let mut declarations = Vec::new();
    let mut rejected_names = Vec::new();
    for item in array {
        match serde_json::from_value::<McpToolDeclaration>(item.clone()) {
            Ok(declaration) => declarations.push(declaration),
            Err(_) => rejected_names.push(
                item.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<invalid>")
                    .to_owned(),
            ),
        }
    }
    Ok(ParsedTools {
        declarations,
        rejected_names,
    })
}

fn usable_object_schema(schema: &Value) -> bool {
    schema.as_object().is_some() && schema.get("type").and_then(Value::as_str) == Some("object")
}

fn valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut len = 1usize;
    for ch in chars {
        len += 1;
        if len > MCP_TOOL_NAME_MAX_CHARS
            || !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            return false;
        }
    }
    true
}

fn valid_server_id(server_id: &str) -> bool {
    let mut chars = server_id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut len = 1usize;
    for ch in chars {
        len += 1;
        if len > MCP_TOOL_NAME_MAX_CHARS || !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        {
            return false;
        }
    }
    true
}

/// Keep printable letters, numbers, ASCII punctuation, and spaces. Drop controls and bidi marks.
pub fn sanitize_untrusted_description(raw: &str) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for ch in raw.chars() {
        if count >= MCP_DESCRIPTION_MAX_CHARS {
            break;
        }
        if allowed_description_char(ch) {
            out.push(ch);
            count += 1;
        }
    }
    out
}

fn allowed_description_char(ch: char) -> bool {
    if ch == ' ' {
        return true;
    }
    if ch.is_control() || ch.is_whitespace() || is_invisible_or_bidi(ch) {
        return false;
    }
    ch.is_alphanumeric() || ch.is_ascii_punctuation()
}

fn is_invisible_or_bidi(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
    )
}

pub fn confine_workspace_path(workspace: &Path, candidate: &str) -> Result<PathBuf, String> {
    let decoded = decode_mcp_path(candidate)?;
    if decoded.is_empty() || decoded.contains('\0') {
        return Err("MCP path is empty or contains a null byte".into());
    }
    let workspace = normalize_lexical(workspace);
    let candidate_path = Path::new(&decoded);
    let joined = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        workspace.join(candidate_path)
    };
    let normal = normalize_lexical(&joined);
    if !normal.starts_with(&workspace) {
        return Err("MCP path escapes the workspace".into());
    }
    if let Ok(canonical_workspace) = workspace.canonicalize() {
        if let Ok(canonical) = normal.canonicalize() {
            if !canonical.starts_with(&canonical_workspace) {
                return Err("MCP path escapes the workspace".into());
            }
            return Ok(canonical);
        }
    }
    Ok(normal)
}

fn decode_mcp_path(candidate: &str) -> Result<String, String> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return Err("MCP path is empty".into());
    }
    let raw = if let Some(after) = trimmed.strip_prefix("file://") {
        decode_file_uri(after)?
    } else if trimmed.contains("://") {
        return Err("MCP path URI is not a local file".into());
    } else {
        trimmed.to_owned()
    };
    Ok(percent_decode(&raw).replace('\\', "/"))
}

fn decode_file_uri(after_scheme: &str) -> Result<String, String> {
    let (host, path) = if let Some(rest) = after_scheme.strip_prefix('/') {
        ("", format!("/{rest}"))
    } else if let Some((host, rest)) = after_scheme.split_once('/') {
        (host, format!("/{rest}"))
    } else {
        return Err("MCP file URI is missing a path".into());
    };
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return Err("MCP file URI host is not local".into());
    }
    let mut path = path;
    let bytes = path.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes[3] == b'/' || bytes[3] == b'\\')
    {
        path.remove(0);
    }
    Ok(path)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if let Some(value) = hex_byte(&bytes[index + 1..]) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_byte(bytes: &[u8]) -> Option<u8> {
    if bytes.len() < 2 {
        return None;
    }
    let hi = hex_val(bytes[0])?;
    let lo = hex_val(bytes[1])?;
    Some((hi << 4) | lo)
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn classify_resources(resources: &[Value], workspace: Option<&Path>) -> (Vec<Value>, Vec<String>) {
    let mut kept = Vec::new();
    let mut rejected = Vec::new();
    for resource in resources {
        let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
        if uri.is_empty() {
            rejected.push("MCP resource is missing a uri".into());
            continue;
        }
        if !is_local_resource(uri) {
            kept.push(resource.clone());
            continue;
        }
        match workspace.and_then(|root| confine_workspace_path(root, uri).ok()) {
            Some(_) => kept.push(resource.clone()),
            None => rejected.push("MCP path escapes the workspace".into()),
        }
    }
    (kept, rejected)
}

fn is_local_resource(uri: &str) -> bool {
    let trimmed = uri.trim();
    trimmed.starts_with("file:")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains("..")
        || !trimmed.contains("://")
}

fn bounded_tool_result(
    result: Value,
    max_output_bytes: u64,
    workspace: Option<&Path>,
    secrets: &[String],
) -> Result<Value, BackendError> {
    let size = serde_json::to_vec(&result)
        .map_err(|error| BackendError::Protocol(error.to_string()))?
        .len() as u64;
    if size > max_output_bytes {
        return Err(BackendError::Protocol(format!(
            "MCP tool output exceeded {max_output_bytes} bytes"
        )));
    }
    Ok(sanitize_tool_output(result, workspace, secrets))
}

fn sanitize_tool_output(value: Value, workspace: Option<&Path>, secrets: &[String]) -> Value {
    match value {
        Value::String(text) => Value::String(scrub_text(&text, workspace, secrets)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| sanitize_tool_output(item, workspace, secrets))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                if matches!(key.as_str(), "uri" | "path" | "file") {
                    if let Some(text) = child.as_str() {
                        out.insert(
                            key,
                            Value::String(scrub_path_field(text, workspace, secrets)),
                        );
                        continue;
                    }
                }
                out.insert(key, sanitize_tool_output(child, workspace, secrets));
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn scrub_text(text: &str, workspace: Option<&Path>, secrets: &[String]) -> String {
    replace_file_uris(&redact_text(text, secrets), workspace)
}

fn scrub_path_field(text: &str, workspace: Option<&Path>, secrets: &[String]) -> String {
    let redacted = redact_text(text, secrets);
    if !looks_like_local_path(&redacted) {
        return redacted;
    }
    match workspace {
        Some(root) => match confine_workspace_path(root, &redacted) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(_) => "[MCP path rejected]".into(),
        },
        None => "[MCP path rejected]".into(),
    }
}

fn looks_like_local_path(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("file:")
        || trimmed.contains("..")
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || (trimmed.len() >= 3
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':')
}

fn replace_file_uris(text: &str, workspace: Option<&Path>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(index) = rest.find("file:") {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let uri = &tail[..end];
        let allowed = workspace
            .and_then(|root| confine_workspace_path(root, uri).ok())
            .is_some();
        if allowed {
            out.push_str(uri);
        } else {
            out.push_str("[MCP path rejected]");
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn redact_text(text: &str, secrets: &[String]) -> String {
    termior_store::StreamingRedactor::new(secrets.iter().cloned())
        .redact(text)
        .text
}

fn redact_backend_error(error: BackendError, secrets: &[String]) -> BackendError {
    match error {
        BackendError::Unavailable(message) => {
            BackendError::Unavailable(redact_text(&message, secrets))
        }
        BackendError::Protocol(message) => BackendError::Protocol(redact_text(&message, secrets)),
        BackendError::Transport(message) => BackendError::Transport(redact_text(&message, secrets)),
        BackendError::Timeout(message) => BackendError::Timeout(redact_text(&message, secrets)),
        BackendError::Unsupported(message) => BackendError::Unsupported(message),
        BackendError::Exited(message) => BackendError::Exited(redact_text(&message, secrets)),
    }
}

fn link_lost(error: &BackendError) -> bool {
    matches!(error, BackendError::Exited(_) | BackendError::Transport(_))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerLinkState {
    Connected,
    Disconnected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLink {
    pub server_id: String,
    pub state: ServerLinkState,
    pub detail: String,
}

impl ServerLink {
    fn connected(server_id: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            state: ServerLinkState::Connected,
            detail: String::new(),
        }
    }

    fn disconnected(server_id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            state: ServerLinkState::Disconnected,
            detail: detail.into(),
        }
    }

    fn failed(server_id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            state: ServerLinkState::Failed,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct McpStdioSpec {
    pub server_id: String,
    pub command: TransportCommand,
    pub locally_auto_approved: BTreeSet<String>,
}

struct StdioSlot {
    command: TransportCommand,
    locally_auto_approved: BTreeSet<String>,
    client: Option<McpStdioClient>,
}

/// Several stdio MCP servers. A failure on one slot does not drop the others.
#[derive(Default)]
pub struct McpServerPool {
    catalog: McpCatalog,
    servers: BTreeMap<String, StdioSlot>,
    workspace_root: Option<PathBuf>,
    known_secrets: Vec<String>,
}

impl McpServerPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    pub fn with_known_secrets(mut self, secrets: impl IntoIterator<Item = String>) -> Self {
        self.known_secrets = secrets.into_iter().collect();
        self.catalog.set_known_secrets(self.known_secrets.clone());
        self
    }

    pub fn with_diagnostic_log(
        mut self,
        path: impl Into<PathBuf>,
        secrets: impl IntoIterator<Item = String>,
    ) -> Self {
        let secrets: Vec<String> = secrets.into_iter().collect();
        self.known_secrets = secrets.clone();
        self.catalog.set_known_secrets(secrets);
        self.catalog.set_diagnostic_log(path);
        self
    }

    pub fn catalog(&self) -> &McpCatalog {
        &self.catalog
    }

    pub fn catalog_mut(&mut self) -> &mut McpCatalog {
        &mut self.catalog
    }

    pub fn connect_all(&mut self, specs: Vec<McpStdioSpec>) -> Vec<ServerLink> {
        specs
            .into_iter()
            .map(|spec| self.connect_spec(spec))
            .collect()
    }

    pub fn connect_spec(&mut self, spec: McpStdioSpec) -> ServerLink {
        let server_id = spec.server_id.clone();
        if !valid_server_id(&server_id) {
            let detail = format!("MCP server id `{server_id}` is not a safe identifier");
            self.catalog.mark_disconnected(&server_id, detail.clone());
            return ServerLink::failed(server_id, detail);
        }
        self.servers.insert(
            server_id.clone(),
            StdioSlot {
                command: spec.command.clone(),
                locally_auto_approved: spec.locally_auto_approved.clone(),
                client: None,
            },
        );
        match self.spawn_and_discover(&spec) {
            Ok((client, ingest, resources)) => {
                self.catalog.replace_tools(&server_id, ingest.tools.clone());
                self.catalog.note_ingest(&server_id, &ingest);
                let (kept, rejected) =
                    classify_resources(&resources, self.workspace_root.as_deref());
                self.catalog.set_resources(&server_id, kept, rejected);
                if let Some(slot) = self.servers.get_mut(&server_id) {
                    slot.client = Some(client);
                }
                ServerLink::connected(server_id)
            }
            Err(error) => {
                let detail = error.to_string();
                self.catalog.mark_disconnected(&server_id, detail.clone());
                ServerLink::failed(
                    server_id,
                    self.catalog.failure(&spec.server_id).unwrap_or(&detail),
                )
            }
        }
    }

    pub fn poll(&mut self, idle: Duration) -> Vec<ServerLink> {
        let ids: Vec<String> = self.servers.keys().cloned().collect();
        ids.into_iter().map(|id| self.poll_one(&id, idle)).collect()
    }

    pub fn reconnect(&mut self, server_id: &str) -> ServerLink {
        let Some(slot) = self.servers.get(server_id) else {
            return ServerLink::failed(server_id, "MCP server is not configured");
        };
        let spec = McpStdioSpec {
            server_id: server_id.to_owned(),
            command: slot.command.clone(),
            locally_auto_approved: slot.locally_auto_approved.clone(),
        };
        if let Some(slot) = self.servers.get_mut(server_id) {
            slot.client = None;
        }
        self.connect_spec(spec)
    }

    pub fn call_tool(
        &mut self,
        qualified_id: &str,
        arguments: Value,
    ) -> Result<Value, BackendError> {
        let Some(tool) = self.catalog.tool(qualified_id).cloned() else {
            return Err(BackendError::Protocol(format!(
                "MCP tool `{qualified_id}` is not registered"
            )));
        };
        if !self.catalog.tool_available(qualified_id) {
            return Err(BackendError::Unavailable(format!(
                "MCP server `{}` is unavailable; tool `{qualified_id}` was not called",
                tool.server_id
            )));
        }
        let result = {
            let Some(client) = self
                .servers
                .get_mut(&tool.server_id)
                .and_then(|slot| slot.client.as_mut())
            else {
                return Err(BackendError::Unavailable(format!(
                    "MCP server `{}` is unavailable; tool `{qualified_id}` was not called",
                    tool.server_id
                )));
            };
            client.call_tool(&tool, arguments)
        };
        if let Err(error) = &result {
            if link_lost(error) {
                if let Some(slot) = self.servers.get_mut(&tool.server_id) {
                    slot.client = None;
                }
                self.catalog
                    .mark_disconnected(&tool.server_id, error.to_string());
            }
        }
        result
    }

    pub fn sync_contracts(
        &self,
        registry: &mut termior_ai::ToolRegistry,
    ) -> Result<(), termior_ai::ToolError> {
        let prefixes: Vec<String> = self
            .servers
            .keys()
            .map(|id| format!("mcp::{id}::"))
            .collect();
        registry.retain_external(|name| !prefixes.iter().any(|prefix| name.starts_with(prefix)));
        for tool in self.catalog.available_tools() {
            registry.upsert_external(tool.to_tool_contract())?;
        }
        Ok(())
    }

    fn poll_one(&mut self, server_id: &str, idle: Duration) -> ServerLink {
        let Some(auto) = self
            .servers
            .get(server_id)
            .map(|slot| slot.locally_auto_approved.clone())
        else {
            return ServerLink::failed(server_id, "MCP server is not configured");
        };
        if self
            .servers
            .get(server_id)
            .is_some_and(|slot| slot.client.is_none())
        {
            let detail = self
                .catalog
                .failure(server_id)
                .unwrap_or("MCP server is unavailable")
                .to_owned();
            return ServerLink::disconnected(server_id, detail);
        }
        let refreshed = self
            .servers
            .get_mut(server_id)
            .and_then(|slot| slot.client.as_mut())
            .map(|client| client.refresh_after_list_changed(&auto, idle));
        match refreshed {
            Some(Ok(Some(ingest))) => {
                self.catalog.replace_tools(server_id, ingest.tools.clone());
                self.catalog.note_ingest(server_id, &ingest);
                ServerLink::connected(server_id)
            }
            Some(Ok(None)) => ServerLink::connected(server_id),
            Some(Err(error)) => {
                if let Some(slot) = self.servers.get_mut(server_id) {
                    slot.client = None;
                }
                self.catalog.mark_disconnected(server_id, error.to_string());
                let detail = self
                    .catalog
                    .failure(server_id)
                    .unwrap_or("MCP server disconnected")
                    .to_owned();
                ServerLink::disconnected(server_id, detail)
            }
            None => ServerLink::disconnected(server_id, "MCP server is unavailable"),
        }
    }

    fn spawn_and_discover(
        &self,
        spec: &McpStdioSpec,
    ) -> Result<(McpStdioClient, McpIngest, Vec<Value>), BackendError> {
        let mut client = McpStdioClient::connect_capturing(
            spec.server_id.clone(),
            spec.command.clone(),
            StdioCapture {
                stderr_log: None,
                known_secrets: self.known_secrets.clone(),
            },
        )?;
        client.add_known_secrets(self.known_secrets.clone());
        if let Some(root) = &self.workspace_root {
            client.set_workspace_root(root.clone());
        }
        let discovery = client.discover()?;
        let mut ingest = ingest_tool_declarations(
            &spec.server_id,
            discovery.tools,
            &spec.locally_auto_approved,
        );
        ingest.rejected_names.extend(discovery.rejected_tool_names);
        Ok((client, ingest, discovery.resources))
    }
}
