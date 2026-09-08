//! ACP stdio adapter over the common backend contract.

use crate::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub const ACP_PROTOCOL_VERSION: u32 = 1;

pub trait AcpClientHandler: Send + Sync {
    fn filesystem_read_enabled(&self) -> bool;
    fn filesystem_write_enabled(&self) -> bool;
    fn terminal_enabled(&self) -> bool;
    fn handle(&self, method: &str, params: &Value) -> Result<Value, BackendError>;
}

pub struct AcpBackend {
    command: TransportCommand,
    transport: Option<StdioTransport>,
    capabilities: CapabilityProfile,
    version: String,
    pending_prompts: BTreeMap<u64, (String, String)>,
    client_handler: Option<Arc<dyn AcpClientHandler>>,
}
impl AcpBackend {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: TransportCommand {
                program: program.into(),
                args,
            },
            transport: None,
            capabilities: CapabilityProfile::default(),
            version: ACP_PROTOCOL_VERSION.to_string(),
            pending_prompts: BTreeMap::new(),
            client_handler: None,
        }
    }
    fn transport(&mut self) -> Result<&mut StdioTransport, BackendError> {
        self.transport
            .as_mut()
            .ok_or_else(|| BackendError::Unavailable("ACP backend is not initialized".into()))
    }

    pub fn with_client_handler(mut self, handler: Arc<dyn AcpClientHandler>) -> Self {
        self.client_handler = Some(handler);
        self
    }
}

impl AgentBackend for AcpBackend {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError> {
        let mut transport = StdioTransport::spawn(&self.command)?;
        let filesystem_read = self
            .client_handler
            .as_ref()
            .is_some_and(|handler| handler.filesystem_read_enabled());
        let filesystem_write = self
            .client_handler
            .as_ref()
            .is_some_and(|handler| handler.filesystem_write_enabled());
        let terminal = self
            .client_handler
            .as_ref()
            .is_some_and(|handler| handler.terminal_enabled());
        let result = transport.request("initialize", json!({
            "protocolVersion":ACP_PROTOCOL_VERSION,
            "clientCapabilities":{
                "fs":{"readTextFile":filesystem_read,"writeTextFile":filesystem_write},
                "terminal":terminal
            },
            "clientInfo":{"name":"termior","title":"Termior","version":env!("CARGO_PKG_VERSION")}
        }), Duration::from_secs(30))?;
        self.version = result
            .get("protocolVersion")
            .map(ToString::to_string)
            .unwrap_or_else(|| ACP_PROTOCOL_VERSION.to_string());
        let caps = result
            .get("agentCapabilities")
            .cloned()
            .unwrap_or(Value::Null);
        self.capabilities = CapabilityProfile {
            session_resume: support_bool(&caps, "loadSession"),
            session_fork: CapabilitySupport::Unsupported,
            mid_turn_steer: CapabilitySupport::Unsupported,
            cancel: CapabilitySupport::Supported,
            approval_requests: CapabilitySupport::Supported,
            host_terminal: CapabilitySupport::Supported,
            diff_events: CapabilitySupport::Unknown,
            model_selection: CapabilitySupport::Unknown,
            sandbox_reporting: CapabilitySupport::Unknown,
        };
        self.transport = Some(transport);
        Ok(BackendInfo {
            id: "acp".into(),
            display_name: "ACP Agent".into(),
            version: self.version.clone(),
            authentication_source: "Agent managed".into(),
            model_source: "Agent managed".into(),
            capabilities: self.capabilities.clone(),
        })
    }
    fn capabilities(&self) -> CapabilityProfile {
        self.capabilities.clone()
    }
    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError> {
        let result = self.transport()?.request(
            "session/new",
            json!({"cwd":launch.project_dir,"mcpServers":[]}),
            Duration::from_secs(30),
        )?;
        let id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BackendError::Protocol("session/new response has no sessionId".into())
            })?;
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: id.into(),
            backend_turn_id: None,
        })
    }
    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError> {
        if !self.capabilities.can_resume() {
            return Err(BackendError::Unsupported("session resume"));
        }
        self.transport()?.request(
            "session/load",
            json!({
                "sessionId":binding.backend_session_id,
                "cwd":std::env::current_dir().map_err(|error| BackendError::Transport(error.to_string()))?,
                "mcpServers":[]
            }),
            Duration::from_secs(30),
        )?;
        Ok(binding)
    }
    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError> {
        let turn_id = format!("acp-prompt-{}", self.pending_prompts.len() + 1);
        let request_id = self.transport()?.begin_request(
            "session/prompt",
            json!({"sessionId":launch.backend_session_id,"prompt":[{"type":"text","text":launch.text}]}),
        )?;
        self.pending_prompts.insert(
            request_id,
            (launch.backend_session_id.clone(), turn_id.clone()),
        );
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: launch.backend_session_id,
            backend_turn_id: Some(turn_id),
        })
    }
    fn next_event(&mut self, timeout: Duration) -> Result<Option<BackendEvent>, BackendError> {
        loop {
            let Some(raw) = self.transport()?.next_event(timeout)? else {
                return Ok(None);
            };
            if raw.get("method").is_none() {
                if let Some(id) = raw.get("id").and_then(Value::as_u64) {
                    if let Some((session_id, turn_id)) = self.pending_prompts.remove(&id) {
                        if let Some(error) = raw.get("error") {
                            return Ok(Some(BackendEvent::Error {
                                message: error.to_string(),
                                raw,
                            }));
                        }
                        let result = raw.get("result").cloned().unwrap_or(Value::Null);
                        let status = result
                            .get("stopReason")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_owned();
                        return Ok(Some(BackendEvent::TurnCompleted {
                            session_id,
                            turn_id,
                            success: status == "end_turn",
                            status,
                            raw: result,
                        }));
                    }
                }
            }
            let method = raw.get("method").and_then(Value::as_str);
            let request_id = raw.get("id").cloned();
            if let (Some(method), Some(request_id), Some(handler)) =
                (method, request_id, self.client_handler.clone())
            {
                if is_client_callback(method) {
                    let params = raw.get("params").cloned().unwrap_or(Value::Null);
                    match handler.handle(method, &params) {
                        Ok(result) => self.transport()?.respond(request_id, result)?,
                        Err(error) => self.transport()?.respond_error(
                            request_id,
                            -32001,
                            &error.to_string(),
                        )?,
                    }
                    continue;
                }
            }
            return Ok(Some(map_acp_message(&raw)));
        }
    }
    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError> {
        let id = decision
            .backend_request_id
            .parse::<u64>()
            .map(Value::from)
            .unwrap_or(Value::String(decision.backend_request_id));
        self.transport()?.respond(
            id,
            json!({"outcome":{"outcome":if decision.approved {"selected"} else {"cancelled"}}}),
        )
    }
    fn cancel(&mut self, binding: &BackendBinding) -> Result<(), BackendError> {
        self.transport()?.notify(
            "session/cancel",
            json!({"sessionId":binding.backend_session_id}),
        )
    }
    fn shutdown(&mut self, timeout: Duration) -> Result<(), BackendError> {
        if let Some(mut transport) = self.transport.take() {
            transport.shutdown(timeout)?;
        }
        Ok(())
    }
}

fn is_client_callback(method: &str) -> bool {
    method.starts_with("fs/") || method.starts_with("terminal/")
}

pub fn map_acp_message(value: &Value) -> BackendEvent {
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("jsonrpc/response");
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    match method {
        "session/update" => {
            let update = params.get("update").cloned().unwrap_or(Value::Null);
            let kind = update
                .get("sessionUpdate")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            match kind {
                "agent_message_chunk" => BackendEvent::AgentMessageDelta {
                    session_id,
                    turn_id: "acp-prompt".into(),
                    item_id: "agent-message".into(),
                    text: update
                        .pointer("/content/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                },
                "tool_call" => BackendEvent::ItemStarted {
                    session_id,
                    turn_id: "acp-prompt".into(),
                    item_id: update
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .into(),
                    raw: update,
                },
                "tool_call_update" => BackendEvent::ItemCompleted {
                    session_id,
                    turn_id: "acp-prompt".into(),
                    item_id: update
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .into(),
                    raw: update,
                },
                _ => BackendEvent::Diagnostic {
                    method: format!("{method}/{kind}"),
                    raw: params,
                },
            }
        }
        "session/request_permission" => BackendEvent::ApprovalRequested {
            backend_request_id: value
                .get("id")
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown".into())
                .trim_matches('"')
                .into(),
            session_id,
            turn_id: "acp-prompt".into(),
            tool_call_id: params
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .into(),
            request_kind: method.into(),
            raw: params,
        },
        "session/prompt/completed" => BackendEvent::TurnCompleted {
            session_id,
            turn_id: "acp-prompt".into(),
            success: params.get("stopReason").and_then(Value::as_str) == Some("end_turn"),
            status: params
                .get("stopReason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .into(),
            raw: params,
        },
        _ => BackendEvent::Diagnostic {
            method: method.into(),
            raw: value.clone(),
        },
    }
}
fn support_bool(value: &Value, name: &str) -> CapabilitySupport {
    if value.get(name).and_then(Value::as_bool) == Some(true) {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unsupported
    }
}
