use serde_json::{json, Value};
use std::process::Command;
use std::time::Duration;

use crate::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    CapabilityProfile, CapabilitySupport, StdioTransport, TaskLaunch, TransportCommand, TurnLaunch,
};

pub const CODEX_COMPATIBLE_MINOR: &str = "0.153";

#[derive(Debug, Clone)]
pub struct CodexProtocol {
    compatible_minors: &'static [&'static str],
    request_timeout: Duration,
}

impl CodexProtocol {
    pub fn stable_0_137() -> Self {
        Self {
            compatible_minors: &["0.137"],
            request_timeout: Duration::from_secs(30),
        }
    }

    /// Stable surface verified from generated schemas for both supported local distribution lines.
    pub fn stable_verified() -> Self {
        Self {
            compatible_minors: &["0.137", "0.153"],
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn capabilities(&self) -> CapabilityProfile {
        CapabilityProfile {
            session_resume: CapabilitySupport::Supported,
            session_fork: CapabilitySupport::Unsupported,
            mid_turn_steer: CapabilitySupport::Unsupported,
            cancel: CapabilitySupport::Supported,
            approval_requests: CapabilitySupport::Supported,
            host_terminal: CapabilitySupport::Unknown,
            diff_events: CapabilitySupport::Supported,
            model_selection: CapabilitySupport::Supported,
            sandbox_reporting: CapabilitySupport::Supported,
        }
    }

    pub fn initialize_request(&self, id: u64) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "clientInfo": {"name":"termior", "title":"Termior", "version":env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi":false}
            }
        })
    }

    pub fn accepts_version(&self, output: &str) -> bool {
        output
            .split_whitespace()
            .find(|part| part.chars().next().is_some_and(|ch| ch.is_ascii_digit()))
            .is_some_and(|version| {
                self.compatible_minors
                    .iter()
                    .any(|minor| version == *minor || version.starts_with(&format!("{minor}.")))
            })
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

pub struct CodexBackend {
    executable: String,
    protocol: CodexProtocol,
    transport: Option<StdioTransport>,
    initialized_version: Option<String>,
}

impl CodexBackend {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            protocol: CodexProtocol::stable_verified(),
            transport: None,
            initialized_version: None,
        }
    }

    fn transport(&mut self) -> Result<&mut StdioTransport, BackendError> {
        self.transport
            .as_mut()
            .ok_or_else(|| BackendError::Unavailable("Codex backend is not initialized".into()))
    }
}

impl AgentBackend for CodexBackend {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError> {
        let version = Command::new(&self.executable)
            .arg("--version")
            .output()
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
        if !self.protocol.accepts_version(&version) {
            return Err(BackendError::Unavailable(format!(
                "expected a verified codex-cli line (0.137.x or {}.x), found {version}",
                CODEX_COMPATIBLE_MINOR,
            )));
        }
        let mut transport = StdioTransport::spawn(&TransportCommand::codex(&self.executable))?;
        transport.request(
            "initialize",
            self.protocol.initialize_request(0)["params"].clone(),
            self.protocol.request_timeout(),
        )?;
        transport.notify("initialized", json!({}))?;
        self.transport = Some(transport);
        self.initialized_version = Some(version.clone());
        Ok(BackendInfo {
            id: "codex-app-server".into(),
            display_name: "Codex app-server".into(),
            version,
            authentication_source: "Codex managed".into(),
            model_source: "Codex managed".into(),
            capabilities: self.protocol.capabilities(),
        })
    }

    fn capabilities(&self) -> CapabilityProfile {
        self.protocol.capabilities()
    }

    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError> {
        let result = self.transport()?.request(
            "thread/start",
            json!({
                "cwd": launch.project_dir,
                "model": launch.model,
                "approvalPolicy": "on-request",
                "sandbox": "workspace-write"
            }),
            Duration::from_secs(30),
        )?;
        let session_id = result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BackendError::Protocol("thread/start response has no thread.id".into())
            })?;
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: session_id.into(),
            backend_turn_id: None,
        })
    }

    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError> {
        if !self.capabilities().can_resume() {
            return Err(BackendError::Unsupported("session resume"));
        }
        self.transport()?.request(
            "thread/resume",
            json!({"threadId":binding.backend_session_id}),
            Duration::from_secs(30),
        )?;
        Ok(binding)
    }

    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError> {
        let result = self.transport()?.request(
            "turn/start",
            json!({
                "threadId": launch.backend_session_id,
                "input":[{"type":"text", "text":launch.text, "text_elements":[]}]
            }),
            Duration::from_secs(30),
        )?;
        let turn_id = result
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Protocol("turn/start response has no turn.id".into()))?;
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: launch.backend_session_id,
            backend_turn_id: Some(turn_id.into()),
        })
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<BackendEvent>, BackendError> {
        self.transport()
            .and_then(|transport| transport.next_event(timeout))
            .map(|event| event.as_ref().map(map_codex_message))
    }

    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError> {
        let id = decision
            .backend_request_id
            .parse::<u64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(decision.backend_request_id));
        self.transport()?.respond(
            id,
            json!({"decision": if decision.approved {"accept"} else {"decline"}}),
        )
    }

    fn cancel(&mut self, binding: &BackendBinding) -> Result<(), BackendError> {
        let turn_id = binding
            .backend_turn_id
            .as_ref()
            .ok_or_else(|| BackendError::Protocol("active binding has no turn ID".into()))?;
        self.transport()?.request(
            "turn/interrupt",
            json!({"threadId":binding.backend_session_id, "turnId":turn_id}),
            Duration::from_secs(10),
        )?;
        Ok(())
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<(), BackendError> {
        if let Some(mut transport) = self.transport.take() {
            transport.shutdown(timeout)?;
        }
        Ok(())
    }
}

pub fn map_codex_message(value: &Value) -> BackendEvent {
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("jsonrpc/response");
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    let session_id = string_at(&params, &["threadId", "thread", "id"]);
    let turn_id = string_at(&params, &["turnId", "turn", "id"]);
    let item_id = string_at(&params, &["itemId", "item", "id"]);
    match method {
        "thread/started" => BackendEvent::SessionCreated {
            session_id,
            raw: params,
        },
        "turn/started" => BackendEvent::TurnStarted {
            session_id,
            turn_id,
            raw: params,
        },
        "item/agentMessage/delta" => BackendEvent::AgentMessageDelta {
            session_id,
            turn_id,
            item_id,
            text: params
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
        },
        "item/started" => BackendEvent::ItemStarted {
            session_id,
            turn_id,
            item_id,
            raw: params,
        },
        "item/completed" => BackendEvent::ItemCompleted {
            session_id,
            turn_id,
            item_id,
            raw: params,
        },
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            BackendEvent::ApprovalRequested {
                backend_request_id: value
                    .get("id")
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".into())
                    .trim_matches('"')
                    .into(),
                session_id,
                turn_id,
                tool_call_id: item_id,
                request_kind: method.into(),
                raw: params,
            }
        }
        "turn/completed" => {
            let status = params
                .pointer("/turn/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            BackendEvent::TurnCompleted {
                session_id,
                turn_id,
                success: status == "completed",
                status,
                raw: params,
            }
        }
        "error" => BackendEvent::Error {
            message: params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex backend error")
                .into(),
            raw: params,
        },
        _ => BackendEvent::Diagnostic {
            method: method.into(),
            raw: value.clone(),
        },
    }
}

fn string_at(value: &Value, path: &[&str]) -> String {
    match path {
        [direct, object, nested] => value
            .get(*direct)
            .and_then(Value::as_str)
            .or_else(|| value.get(*object)?.get(*nested)?.as_str())
            .unwrap_or("unknown")
            .into(),
        _ => "unknown".into(),
    }
}
