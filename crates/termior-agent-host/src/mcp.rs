//! Minimal MCP 2025-06-18 domain layer. Transports remain isolated per server.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{BackendError, StdioTransport, TransportCommand};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

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
            untrusted_description: declaration.description,
            input_schema: close_object_schema(declaration.input_schema),
            timeout_ms: 30_000,
            max_output_bytes: 1024 * 1024,
        }
    }

    pub fn to_tool_contract(&self) -> termior_ai::ToolContract {
        termior_ai::ToolContract {
            name: self.qualified_id.clone(),
            level: if self.requires_approval {
                termior_ai::ToolLevelSerde::Approval
            } else {
                termior_ai::ToolLevelSerde::Auto
            },
            description: format!(
                "MCP server `{}` tool `{}`. Server description is untrusted metadata: {}",
                self.server_id, self.name, self.untrusted_description
            ),
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

#[derive(Debug, Default)]
pub struct McpCatalog {
    by_server: BTreeMap<String, BTreeMap<String, QualifiedMcpTool>>,
    failures: BTreeMap<String, String>,
}
impl McpCatalog {
    pub fn replace_tools(&mut self, server_id: &str, tools: Vec<QualifiedMcpTool>) {
        self.failures.remove(server_id);
        self.by_server.insert(
            server_id.into(),
            tools
                .into_iter()
                .map(|tool| (tool.name.clone(), tool))
                .collect(),
        );
    }
    pub fn mark_disconnected(&mut self, server_id: &str, error: impl Into<String>) {
        self.failures.insert(server_id.into(), error.into());
    }
    pub fn tool(&self, qualified_id: &str) -> Option<&QualifiedMcpTool> {
        self.by_server
            .values()
            .flat_map(|tools| tools.values())
            .find(|tool| tool.qualified_id == qualified_id)
    }
    pub fn failure(&self, server_id: &str) -> Option<&str> {
        self.failures.get(server_id).map(String::as_str)
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
    pub resources: Vec<Value>,
    pub prompts: Vec<Value>,
}

pub struct McpStdioClient {
    server_id: String,
    transport: StdioTransport,
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
        let mut transport = StdioTransport::spawn(&command)?;
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
        })
    }

    pub fn discover(&mut self) -> Result<McpDiscovery, BackendError> {
        let tools =
            self.transport
                .request("tools/list", serde_json::json!({}), Duration::from_secs(30))?;
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
            tools: serde_json::from_value(
                tools
                    .get("tools")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            )
            .map_err(|error| BackendError::Protocol(error.to_string()))?,
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

    pub fn qualified_tools(
        &mut self,
        locally_auto_approved: &BTreeSet<String>,
    ) -> Result<Vec<QualifiedMcpTool>, BackendError> {
        Ok(self
            .discover()?
            .tools
            .into_iter()
            .map(|tool| {
                QualifiedMcpTool::from_declaration(&self.server_id, tool, locally_auto_approved)
            })
            .collect())
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
        let result = self.transport.request(
            "tools/call",
            serde_json::json!({"name":tool.name,"arguments":arguments}),
            Duration::from_millis(tool.timeout_ms),
        )?;
        let size = serde_json::to_vec(&result)
            .map_err(|error| BackendError::Protocol(error.to_string()))?
            .len() as u64;
        if size > tool.max_output_bytes {
            return Err(BackendError::Protocol(format!(
                "MCP tool output exceeded {} bytes",
                tool.max_output_bytes
            )));
        }
        Ok(result)
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
        Ok(Self {
            endpoint,
            bearer,
            session_id: None,
            client,
            next_id: 1,
        })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, BackendError> {
        let id = self.next_id;
        self.next_id += 1;
        self.request(id, method, params)
    }

    pub fn set_bearer(&mut self, bearer: Option<String>) {
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
        Ok(McpDiscovery {
            tools: serde_json::from_value(
                tools
                    .get("tools")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            )
            .map_err(|error| BackendError::Protocol(error.to_string()))?,
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
        let result = self.rpc(
            "tools/call",
            serde_json::json!({"name":tool.name,"arguments":arguments}),
        )?;
        let size = serde_json::to_vec(&result)
            .map_err(|error| BackendError::Protocol(error.to_string()))?
            .len() as u64;
        if size > tool.max_output_bytes {
            return Err(BackendError::Protocol(format!(
                "MCP tool output exceeded {} bytes",
                tool.max_output_bytes
            )));
        }
        Ok(result)
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
    if let Some(object) = schema.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some("object") {
            object.insert("additionalProperties".into(), Value::Bool(false));
        }
    }
    schema
}
