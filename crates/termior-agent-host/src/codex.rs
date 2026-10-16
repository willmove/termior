use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    CapabilityProfile, CapabilitySupport, StdioCapture, StdioTransport, TaskLaunch,
    TransportCommand, TurnLaunch,
};

pub const CODEX_COMPATIBLE_MINOR: &str = "0.153";

pub const PERMISSIONS_APPROVAL_METHOD: &str = "item/permissions/requestApproval";

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
    /// Requested permission profiles of unanswered `item/permissions/requestApproval` requests,
    /// keyed by backend request ID. A grant must echo back (a subset of) this profile.
    pending_permissions: HashMap<String, Value>,
    stderr_log: Option<PathBuf>,
    known_secrets: Vec<String>,
}

impl CodexBackend {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            protocol: CodexProtocol::stable_verified(),
            transport: None,
            initialized_version: None,
            pending_permissions: HashMap::new(),
            stderr_log: None,
            known_secrets: Vec::new(),
        }
    }

    /// Persist backend stderr after streaming redaction.
    pub fn with_stderr_log(
        mut self,
        path: impl Into<PathBuf>,
        known_secrets: impl IntoIterator<Item = String>,
    ) -> Self {
        self.stderr_log = Some(path.into());
        self.known_secrets = known_secrets.into_iter().collect();
        self
    }

    fn track_pending_permissions(&mut self, event: &BackendEvent) {
        if let BackendEvent::ApprovalRequested {
            backend_request_id,
            request_kind,
            raw,
            ..
        } = event
        {
            if request_kind == PERMISSIONS_APPROVAL_METHOD {
                self.pending_permissions.insert(
                    backend_request_id.clone(),
                    raw.get("permissions").cloned().unwrap_or(Value::Null),
                );
            }
        }
    }

    fn approval_result(&mut self, decision: &BackendDecision) -> Value {
        match self
            .pending_permissions
            .remove(&decision.backend_request_id)
        {
            Some(requested) => permissions_approval_result(
                &requested,
                decision.approved,
                decision.granted_permission_ids.as_deref(),
            ),
            None => json!({"decision": if decision.approved {"accept"} else {"decline"}}),
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
        let mut command = Command::new(&self.executable);
        command.arg("--version");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let version = command
            .output()
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
        if !self.protocol.accepts_version(&version) {
            return Err(BackendError::Unavailable(format!(
                "expected a verified codex-cli line (0.137.x or {}.x), found {version}",
                CODEX_COMPATIBLE_MINOR,
            )));
        }
        let mut transport = StdioTransport::spawn_capturing(
            &TransportCommand::codex(&self.executable),
            StdioCapture {
                stderr_log: self.stderr_log.clone(),
                known_secrets: self.known_secrets.clone(),
            },
        )?;
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
        let event = self
            .transport()
            .and_then(|transport| transport.next_event(timeout))?
            .as_ref()
            .map(map_codex_message);
        if let Some(event) = &event {
            self.track_pending_permissions(event);
        }
        Ok(event)
    }

    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError> {
        let result = self.approval_result(&decision);
        let id = decision
            .backend_request_id
            .parse::<u64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(decision.backend_request_id));
        self.transport()?.respond(id, result)
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
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | PERMISSIONS_APPROVAL_METHOD => BackendEvent::ApprovalRequested {
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
        },
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

/// One grantable line inside a Codex permission profile.
///
/// Objects without nested arrays (such as `network`) are a single entry. Each array element,
/// including a filesystem path or sandbox entry, is its own entry so the approval card can
/// grant a subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionEntry {
    pub id: String,
    pub label: String,
}

/// Flat list of permissions the approval card can toggle.
///
/// A non-object profile yields no entries, and approving it grants nothing.
pub fn permission_entries(permissions: &Value) -> Vec<PermissionEntry> {
    let Some(profile) = permissions.as_object() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for (key, value) in profile {
        collect_permission_entries(key, value, &mut entries);
    }
    // `serde_json::Map` is a `BTreeMap` unless another crate enables `preserve_order`,
    // so iteration order is not part of the protocol. Sort ids for a stable card.
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    entries
}

fn collect_permission_entries(path: &str, value: &Value, entries: &mut Vec<PermissionEntry>) {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let id = format!("{path}[{index}]");
                entries.push(PermissionEntry {
                    label: permission_label(&id, item),
                    id,
                });
            }
        }
        Value::Object(fields) if fields.values().any(contains_permission_array) => {
            for (key, child) in fields {
                collect_permission_entries(&format!("{path}.{key}"), child, entries);
            }
        }
        other => entries.push(PermissionEntry {
            id: path.to_string(),
            label: permission_label(path, other),
        }),
    }
}

fn contains_permission_array(value: &Value) -> bool {
    match value {
        Value::Array(_) => true,
        Value::Object(fields) => fields.values().any(contains_permission_array),
        _ => false,
    }
}

fn permission_label(path: &str, value: &Value) -> String {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    };
    format!("{path}  {rendered}")
}

fn retain_permission(path: &str, value: &Value, selected: &HashSet<&str>) -> Option<Value> {
    match value {
        Value::Array(items) => {
            let kept = items
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    let id = format!("{path}[{index}]");
                    selected.contains(id.as_str())
                })
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            if kept.is_empty() {
                None
            } else {
                Some(Value::Array(kept))
            }
        }
        Value::Object(fields) if fields.values().any(contains_permission_array) => {
            let mut kept = serde_json::Map::new();
            for (key, child) in fields {
                if let Some(value) = retain_permission(&format!("{path}.{key}"), child, selected) {
                    kept.insert(key.clone(), value);
                }
            }
            if kept.is_empty() {
                None
            } else {
                Some(Value::Object(kept))
            }
        }
        other => selected.contains(path).then(|| other.clone()),
    }
}

fn granted_permission_profile(requested: &Value, granted_ids: Option<&[String]>) -> Value {
    let Some(profile) = requested.as_object() else {
        return json!({});
    };
    let Some(granted_ids) = granted_ids else {
        return requested.clone();
    };
    let selected = granted_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut kept = serde_json::Map::new();
    for (key, child) in profile {
        if let Some(value) = retain_permission(key, child, &selected) {
            kept.insert(key.clone(), value);
        }
    }
    Value::Object(kept)
}

/// Response to `item/permissions/requestApproval`: the granted profile plus its scope.
///
/// `granted_ids` is the checkbox subset. `None` grants the whole requested profile.
/// Denial, a non-object profile, or an empty subset grants `{}`. The scope is always
/// `"turn"`, including when the server proposed `"session"` or a value this client
/// does not recognize.
pub fn permissions_approval_result(
    requested: &Value,
    approved: bool,
    granted_ids: Option<&[String]>,
) -> Value {
    let permissions = if approved {
        granted_permission_profile(requested, granted_ids)
    } else {
        json!({})
    };
    json!({"permissions": permissions, "scope": "turn"})
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

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions_request(scope: &str) -> Value {
        permissions_request_with(scope, json!({"network":{"enabled":true}}))
    }

    fn rich_permissions() -> Value {
        json!({
            "network": {"enabled": true},
            "fileSystem": {"write": ["/proj", "/shared"]}
        })
    }

    fn decision(approved: bool) -> BackendDecision {
        BackendDecision {
            backend_request_id: "7".into(),
            approved,
            granted_permission_ids: None,
        }
    }

    fn decision_with(approved: bool, ids: &[&str]) -> BackendDecision {
        BackendDecision {
            backend_request_id: "7".into(),
            approved,
            granted_permission_ids: Some(ids.iter().map(|id| (*id).to_string()).collect()),
        }
    }

    fn permissions_request_with(scope: &str, permissions: Value) -> Value {
        json!({
            "jsonrpc":"2.0",
            "id":7,
            "method":PERMISSIONS_APPROVAL_METHOD,
            "params":{
                "threadId":"th-1", "turnId":"turn-1", "itemId":"item-1",
                "reason":"needs network",
                "scope":scope,
                "permissions": permissions
            }
        })
    }

    #[test]
    fn permissions_request_maps_to_approval_with_original_request_id() {
        let event = map_codex_message(&permissions_request("turn"));
        assert!(matches!(
            &event,
            BackendEvent::ApprovalRequested { backend_request_id, tool_call_id, request_kind, raw, .. }
                if backend_request_id == "7"
                    && tool_call_id == "item-1"
                    && request_kind == PERMISSIONS_APPROVAL_METHOD
                    && raw.pointer("/permissions/network/enabled") == Some(&json!(true))
        ));
    }

    #[test]
    fn approving_permissions_grants_requested_profile_for_the_turn_only() {
        for scope in ["turn", "session", "something-new"] {
            let mut backend = CodexBackend::new("codex");
            backend.track_pending_permissions(&map_codex_message(&permissions_request(scope)));
            assert_eq!(
                backend.approval_result(&decision(true)),
                json!({"permissions":{"network":{"enabled":true}}, "scope":"turn"}),
                "server scope {scope:?} must not widen the grant"
            );
        }
    }

    #[test]
    fn checking_every_permission_grants_the_full_profile_for_the_turn() {
        let requested = rich_permissions();
        let ids = permission_entries(&requested)
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "fileSystem.write[0]".to_string(),
                "fileSystem.write[1]".to_string(),
                "network".to_string(),
            ]
        );
        let mut backend = CodexBackend::new("codex");
        backend.track_pending_permissions(&map_codex_message(&permissions_request_with(
            "session",
            requested.clone(),
        )));
        assert_eq!(
            backend.approval_result(&decision_with(
                true,
                &["network", "fileSystem.write[0]", "fileSystem.write[1]"]
            )),
            json!({"permissions": requested, "scope": "turn"})
        );
    }

    #[test]
    fn partial_approval_grants_only_the_checked_permissions() {
        let requested = rich_permissions();
        let mut backend = CodexBackend::new("codex");
        backend.track_pending_permissions(&map_codex_message(&permissions_request_with(
            "session", requested,
        )));
        assert_eq!(
            backend.approval_result(&decision_with(
                true,
                &["not-requested", "fileSystem.write[0]"]
            )),
            json!({
                "permissions": {"fileSystem": {"write": ["/proj"]}},
                "scope": "turn"
            })
        );
        assert!(backend.pending_permissions.is_empty());
    }

    #[test]
    fn denying_permissions_replies_with_an_empty_grant() {
        let mut backend = CodexBackend::new("codex");
        backend.track_pending_permissions(&map_codex_message(&permissions_request("session")));
        assert_eq!(
            backend.approval_result(&decision_with(false, &["network"])),
            json!({"permissions":{}, "scope":"turn"})
        );
        assert!(backend.pending_permissions.is_empty());
    }

    #[test]
    fn unknown_permission_scope_is_kept_for_display_and_granted_only_for_the_turn() {
        let event = map_codex_message(&permissions_request("something-new"));
        match &event {
            BackendEvent::ApprovalRequested { raw, .. } => {
                assert_eq!(
                    raw.get("scope").and_then(Value::as_str),
                    Some("something-new")
                );
            }
            other => panic!("expected an approval request, got {other:?}"),
        }
        let mut backend = CodexBackend::new("codex");
        backend.track_pending_permissions(&event);
        assert_eq!(
            backend.approval_result(&decision(true)),
            json!({"permissions":{"network":{"enabled":true}}, "scope":"turn"})
        );
    }

    #[test]
    fn malformed_permission_profile_is_never_granted() {
        assert_eq!(
            permissions_approval_result(&Value::Null, true, None),
            json!({"permissions":{}, "scope":"turn"})
        );
    }

    #[test]
    fn command_approvals_keep_the_decision_reply() {
        let mut backend = CodexBackend::new("codex");
        assert_eq!(
            backend.approval_result(&decision(true)),
            json!({"decision":"accept"})
        );
        assert_eq!(
            backend.approval_result(&decision(false)),
            json!({"decision":"decline"})
        );
    }
}
