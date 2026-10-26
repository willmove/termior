//! Read-only restore for Codex and ACP sessions.
//!
//! Querying a backend loads its reported status. It does not start a turn, answer an approval,
//! or replay a tool call.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    AcpBackend, AgentBackend, BackendBinding, BackendEvent, BackendSessionReport, CodexBackend,
    ManagedSessionState,
};
use termior_store::{
    reduce_queried_backend_status, JournalError, RecoveredOperationState, RecoveryCenter,
    RecoveryCommand, TaskJournal,
};

/// Where a live external session should checkpoint itself for the next process start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRecoveryTarget {
    pub tasks_root: PathBuf,
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedBackendBinding {
    task_id: String,
    backend_id: String,
    backend_session_id: String,
    queryable: bool,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    project_dir: String,
}

/// Facts captured when an external session crosses a restart boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSessionCheckpoint {
    pub tasks_root: PathBuf,
    pub task_id: String,
    pub backend_id: String,
    pub backend_session_id: String,
    pub queryable: bool,
    pub program: String,
    pub args: Vec<String>,
    pub project_dir: String,
    pub managed_state: ManagedSessionState,
}

/// Outcome of one read-only restore. `queried` is false when the session was not asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRestoreReport {
    pub task_id: String,
    pub state: RecoveredOperationState,
    pub queried: bool,
    pub queryable: bool,
    pub reported_status: Option<String>,
}

pub fn checkpoint_session(checkpoint: &ExternalSessionCheckpoint) -> Result<(), JournalError> {
    let Some(state) = snapshot_state(checkpoint.managed_state) else {
        return Ok(());
    };
    let mut journal = TaskJournal::create(&checkpoint.tasks_root, &checkpoint.task_id)?;
    if journal.last_sequence() == 0 {
        journal.append(
            "diagnostic",
            json!({"message": "external session checkpoint"}),
            true,
        )?;
    }
    journal.write_snapshot(json!({
        "task": {
            "state": state,
            "backend_id": checkpoint.backend_id,
            "backend_session_id": checkpoint.backend_session_id,
            "session_queryable": checkpoint.queryable,
            "project_dir": checkpoint.project_dir,
        }
    }))?;
    let binding = PersistedBackendBinding {
        task_id: checkpoint.task_id.clone(),
        backend_id: checkpoint.backend_id.clone(),
        backend_session_id: checkpoint.backend_session_id.clone(),
        queryable: checkpoint.queryable,
        program: checkpoint.program.clone(),
        args: checkpoint.args.clone(),
        project_dir: checkpoint.project_dir.clone(),
    };
    let path = checkpoint
        .tasks_root
        .join(&checkpoint.task_id)
        .join("backend-binding.json");
    termior_store::atomic_write(&path, &serde_json::to_string_pretty(&binding)?)?;
    Ok(())
}

/// Fetch each persisted Codex/ACP binding and reduce it before Recovery Center is shown.
///
/// Failures and non-queryable sessions become `unknown`. This function never starts a turn.
pub fn reconcile_persisted_backend_bindings(
    tasks_root: &Path,
) -> Result<Vec<ExternalRestoreReport>, JournalError> {
    let entries = match std::fs::read_dir(tasks_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut reports = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("backend-binding.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let binding: PersistedBackendBinding = match serde_json::from_slice(&bytes) {
            Ok(binding) => binding,
            Err(_) => continue,
        };
        if settled_recovery(tasks_root, &binding.task_id)? {
            continue;
        }
        let _ = TaskJournal::create(tasks_root, &binding.task_id);
        let report = if binding.queryable && !binding.backend_session_id.is_empty() {
            query_binding(&binding)
        } else {
            ExternalRestoreReport {
                task_id: binding.task_id.clone(),
                state: RecoveredOperationState::Unknown,
                queried: false,
                queryable: false,
                reported_status: None,
            }
        };
        RecoveryCenter::apply(
            tasks_root,
            &binding.task_id,
            RecoveryCommand::ApplyBackendQuery {
                session_id: binding
                    .queryable
                    .then(|| binding.backend_session_id.clone()),
                queryable: report.queryable,
                reported_status: report.reported_status.clone(),
            },
        )?;
        reports.push(report);
    }
    Ok(reports)
}

/// Query one already-initialized backend. Does not call `start_turn` or `respond`.
pub fn restore_external_session(
    backend: &mut dyn AgentBackend,
    binding: &BackendBinding,
) -> ExternalRestoreReport {
    let queryable = backend.capabilities().can_resume() && !binding.backend_session_id.is_empty();
    if !queryable {
        return ExternalRestoreReport {
            task_id: binding.task_id.clone(),
            state: RecoveredOperationState::Unknown,
            queried: false,
            queryable: false,
            reported_status: None,
        };
    }
    match backend.query_session(binding) {
        Ok(BackendSessionReport { status }) => ExternalRestoreReport {
            task_id: binding.task_id.clone(),
            state: reduce_queried_backend_status(true, Some(status.as_str())),
            queried: true,
            queryable: true,
            reported_status: Some(status),
        },
        Err(_) => ExternalRestoreReport {
            task_id: binding.task_id.clone(),
            state: RecoveredOperationState::Unknown,
            queried: true,
            queryable: true,
            reported_status: None,
        },
    }
}

pub fn reported_status_from_value(value: &Value) -> Option<String> {
    const PATHS: &[&str] = &[
        "/status",
        "/thread/status",
        "/turn/status",
        "/stopReason",
        "/thread/turns/0/status",
    ];
    PATHS.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .filter(|status| !status.is_empty())
            .map(str::to_owned)
    })
}

pub fn fold_observed_status(current: Option<String>, event: &BackendEvent) -> Option<String> {
    match event {
        BackendEvent::ApprovalRequested { .. } => Some("waiting-approval".into()),
        BackendEvent::TurnCompleted { status, .. } => Some(status.clone()),
        BackendEvent::Error { .. } => Some("failed".into()),
        BackendEvent::ItemStarted { .. } => {
            if current.as_deref().is_some_and(settled_status) {
                current
            } else {
                Some("running".into())
            }
        }
        _ => current,
    }
}

fn settled_status(status: &str) -> bool {
    !matches!(
        reduce_queried_backend_status(true, Some(status)),
        RecoveredOperationState::Unknown
    )
}

fn snapshot_state(state: ManagedSessionState) -> Option<&'static str> {
    match state {
        ManagedSessionState::Running => Some("running"),
        ManagedSessionState::WaitingApproval => Some("waiting-approval"),
        ManagedSessionState::Unknown => Some("unknown"),
        ManagedSessionState::Completed => Some("completed-unverified"),
        ManagedSessionState::Ready | ManagedSessionState::Shutdown => None,
    }
}

fn settled_recovery(tasks_root: &Path, task_id: &str) -> Result<bool, JournalError> {
    let path = tasks_root.join(task_id).join("recovery.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let value: Value = serde_json::from_slice(&bytes)?;
    let state = value
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(matches!(state, "succeeded" | "failed" | "abandoned"))
}

fn query_binding(binding: &PersistedBackendBinding) -> ExternalRestoreReport {
    let backend_binding = BackendBinding {
        task_id: binding.task_id.clone(),
        backend_session_id: binding.backend_session_id.clone(),
        backend_turn_id: None,
    };
    let mut backend = open_backend(binding);
    let report = match backend.initialize() {
        Ok(_) => restore_external_session(backend.as_mut(), &backend_binding),
        Err(_) => ExternalRestoreReport {
            task_id: binding.task_id.clone(),
            state: RecoveredOperationState::Unknown,
            queried: false,
            queryable: false,
            reported_status: None,
        },
    };
    let _ = backend.shutdown(Duration::from_secs(2));
    report
}

fn open_backend(binding: &PersistedBackendBinding) -> Box<dyn AgentBackend> {
    let backend_id = binding.backend_id.to_ascii_lowercase();
    if backend_id.contains("acp") {
        Box::new(AcpBackend::new(
            binding.program.clone(),
            binding.args.clone(),
        ))
    } else {
        Box::new(
            CodexBackend::new(binding.program.clone())
                .with_sandbox(crate::CodexSandboxMode::from_env()),
        )
    }
}

/// Drain notifications that arrived with a status response without answering server requests.
pub fn observe_queued_status(
    mut status: Option<String>,
    queued: impl IntoIterator<Item = Value>,
    map_message: impl Fn(&Value) -> BackendEvent,
) -> Option<String> {
    for raw in queued {
        if raw.get("method").is_some() && raw.get("id").is_some() {
            let event = map_message(&raw);
            if matches!(event, BackendEvent::ApprovalRequested { .. }) {
                status = Some("waiting-approval".into());
            }
            continue;
        }
        status = fold_observed_status(status, &map_message(&raw));
    }
    status
}
