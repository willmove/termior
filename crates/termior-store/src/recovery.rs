//! Honest restart recovery commands for incomplete task work.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::{JournalError, TaskJournal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveredOperationState {
    Waiting,
    Succeeded,
    Failed,
    Unknown,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredOperation {
    pub id: String,
    pub attempt_id: String,
    pub kind: String,
    pub state: RecoveredOperationState,
    pub last_trusted_event: u64,
    pub queryable_backend_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryCommand {
    Inspect,
    Abandon,
    Retry,
    Resume {
        verified_state: RecoveredOperationState,
    },
}

/// A startup-safe view of a persisted task. States that could have owned an in-flight
/// model, tool, or command operation are deliberately reported as `unknown` after restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryTaskSummary {
    pub task_id: String,
    pub attempt_id: String,
    pub project_dir: Option<PathBuf>,
    pub backend_id: Option<String>,
    pub persisted_state: String,
    pub recovered_state: RecoveredOperationState,
    pub waiting_reason: Option<String>,
    pub last_trusted_event: u64,
    pub repaired_tail: bool,
    pub diagnostic: Option<String>,
    pub available_actions: Vec<String>,
}

/// Scans the task index used by the runtime without trusting directory names or snapshots.
pub struct RecoveryCenter;

impl RecoveryCenter {
    pub fn scan(tasks_root: &Path) -> Result<Vec<RecoveryTaskSummary>, JournalError> {
        let entries = match std::fs::read_dir(tasks_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut tasks = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let task_id = entry.file_name().to_string_lossy().to_string();
            let recovery = match TaskJournal::recover(tasks_root, &task_id, &[]) {
                Ok(recovery) => recovery,
                Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => {
                    tasks.push(RecoveryTaskSummary {
                        attempt_id: format!("{task_id}-attempt-1"),
                        task_id,
                        project_dir: None,
                        backend_id: None,
                        persisted_state: "unreadable".into(),
                        recovered_state: RecoveredOperationState::Unknown,
                        waiting_reason: None,
                        last_trusted_event: 0,
                        repaired_tail: false,
                        diagnostic: Some(error.to_string()),
                        available_actions: vec!["inspect".into(), "abandon".into()],
                    });
                    continue;
                }
            };
            let snapshot = match TaskJournal::load_snapshot(tasks_root, &task_id) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tasks.push(RecoveryTaskSummary {
                        attempt_id: format!("{}-attempt-1", recovery.task_id),
                        task_id: recovery.task_id,
                        project_dir: None,
                        backend_id: None,
                        persisted_state: "snapshot-unreadable".into(),
                        recovered_state: RecoveredOperationState::Unknown,
                        waiting_reason: None,
                        last_trusted_event: recovery
                            .events
                            .last()
                            .map_or(0, |event| event.sequence),
                        repaired_tail: recovery.repaired_tail,
                        diagnostic: Some(error.to_string()),
                        available_actions: vec!["inspect".into(), "abandon".into()],
                    });
                    continue;
                }
            };
            let state = snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.state.pointer("/task/state"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let recovered_state = recovered_state(&state);
            let persisted_recovery = load_recovered_operation(tasks_root, &task_id)?;
            let recovered_state = persisted_recovery
                .as_ref()
                .map_or(recovered_state, |operation| operation.state);
            if matches!(
                recovered_state,
                RecoveredOperationState::Succeeded | RecoveredOperationState::Abandoned
            ) {
                continue;
            }
            let task = snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.state.get("task"));
            let project_dir = task
                .and_then(|task| task.get("project_dir"))
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from);
            let backend_id = task
                .and_then(|task| task.get("backend_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let waiting_reason = task
                .and_then(|task| task.get("waiting_reason"))
                .filter(|value| !value.is_null())
                .map(|value| value.to_string());
            let mut available_actions = vec!["inspect".into(), "abandon".into(), "retry".into()];
            if backend_id
                .as_deref()
                .is_some_and(|backend| backend.contains("codex") || backend.contains("acp"))
            {
                available_actions.push("resume-after-query".into());
            }
            tasks.push(RecoveryTaskSummary {
                attempt_id: persisted_recovery
                    .as_ref()
                    .map(|operation| operation.attempt_id.clone())
                    .unwrap_or_else(|| format!("{}-attempt-1", recovery.task_id)),
                task_id: recovery.task_id,
                project_dir,
                backend_id,
                persisted_state: state,
                recovered_state,
                waiting_reason,
                last_trusted_event: recovery.events.last().map_or(0, |event| event.sequence),
                repaired_tail: recovery.repaired_tail,
                diagnostic: None,
                available_actions,
            });
        }
        tasks.sort_by(|left, right| {
            right
                .last_trusted_event
                .cmp(&left.last_trusted_event)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
        Ok(tasks)
    }

    pub fn apply(
        tasks_root: &Path,
        task_id: &str,
        command: RecoveryCommand,
    ) -> Result<RecoveredOperation, JournalError> {
        let recovery = TaskJournal::recover(tasks_root, task_id, &[])?;
        let mut operation = load_recovered_operation(tasks_root, task_id)?.unwrap_or_else(|| {
            RecoveredOperation::from_incomplete(
                task_id,
                "task",
                recovery.events.last().map_or(0, |event| event.sequence),
                None,
            )
        });
        operation.apply(command);
        let path = tasks_root.join(task_id).join("recovery.json");
        crate::atomic_write(
            &path,
            &serde_json::to_string_pretty(&operation).map_err(JournalError::Json)?,
        )
        .map_err(JournalError::Atomic)?;
        Ok(operation)
    }
}

fn load_recovered_operation(
    tasks_root: &Path,
    task_id: &str,
) -> Result<Option<RecoveredOperation>, JournalError> {
    match std::fs::read(tasks_root.join(task_id).join("recovery.json")) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn recovered_state(state: &str) -> RecoveredOperationState {
    match state {
        "waiting-approval" | "waiting-plan" | "waiting-change-review" | "waiting-user" => {
            RecoveredOperationState::Waiting
        }
        "failed" => RecoveredOperationState::Failed,
        "cancelled" => RecoveredOperationState::Abandoned,
        "completed-verified" | "completed-unverified" => RecoveredOperationState::Succeeded,
        // idle may not have done work, while running/cancelling may have unobserved side effects.
        "idle" => RecoveredOperationState::Waiting,
        _ => RecoveredOperationState::Unknown,
    }
}

impl RecoveredOperation {
    pub fn from_incomplete(
        id: impl Into<String>,
        kind: impl Into<String>,
        last_trusted_event: u64,
        backend_session: Option<String>,
    ) -> Self {
        let id = id.into();
        Self {
            attempt_id: format!("{id}-attempt-1"),
            id,
            kind: kind.into(),
            state: RecoveredOperationState::Unknown,
            last_trusted_event,
            queryable_backend_session: backend_session,
        }
    }
    pub fn apply(&mut self, command: RecoveryCommand) -> Option<String> {
        match command {
            RecoveryCommand::Inspect => None,
            RecoveryCommand::Abandon => {
                self.state = RecoveredOperationState::Abandoned;
                None
            }
            RecoveryCommand::Retry => {
                let attempt = self
                    .attempt_id
                    .rsplit_once("-attempt-")
                    .and_then(|(_, value)| value.parse::<u64>().ok())
                    .unwrap_or(1)
                    + 1;
                self.attempt_id = format!("{}-attempt-{attempt}", self.id);
                self.state = RecoveredOperationState::Waiting;
                Some(self.attempt_id.clone())
            }
            RecoveryCommand::Resume { verified_state } => {
                if self.queryable_backend_session.is_some() {
                    self.state = verified_state;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovery_center_lists_actionable_tasks_and_marks_in_flight_unknown() {
        let root = std::env::temp_dir().join(format!(
            "termior-recovery-center-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut running = TaskJournal::create(&root, "running-task").unwrap();
        running
            .append("tool-started", json!({"id":"call-1"}), true)
            .unwrap();
        running
            .write_snapshot(json!({
                "task": {
                    "state": "running",
                    "project_dir": "C:/work",
                    "backend_id": "codex-app-server",
                    "waiting_reason": null
                }
            }))
            .unwrap();
        let completed = TaskJournal::create(&root, "complete-task").unwrap();
        completed
            .write_snapshot(json!({"task":{"state":"completed-verified"}}))
            .unwrap();
        std::fs::create_dir_all(root.join("corrupt-task")).unwrap();
        std::fs::write(root.join("corrupt-task/events.jsonl"), b"{broken}\n").unwrap();

        let tasks = RecoveryCenter::scan(&root).unwrap();
        assert_eq!(tasks.len(), 2);
        let running = tasks
            .iter()
            .find(|task| task.task_id == "running-task")
            .unwrap();
        assert_eq!(running.persisted_state, "running");
        assert_eq!(running.recovered_state, RecoveredOperationState::Unknown);
        assert!(running
            .available_actions
            .contains(&"resume-after-query".into()));
        assert_eq!(running.last_trusted_event, 1);
        let retry = RecoveryCenter::apply(&root, "running-task", RecoveryCommand::Retry).unwrap();
        assert_eq!(retry.attempt_id, "running-task-attempt-2");
        let rescanned = RecoveryCenter::scan(&root).unwrap();
        assert_eq!(
            rescanned
                .iter()
                .find(|task| task.task_id == "running-task")
                .unwrap()
                .attempt_id,
            "running-task-attempt-2"
        );
        RecoveryCenter::apply(&root, "running-task", RecoveryCommand::Abandon).unwrap();
        assert!(!RecoveryCenter::scan(&root)
            .unwrap()
            .iter()
            .any(|task| task.task_id == "running-task"));
        assert!(tasks
            .iter()
            .find(|task| task.task_id == "corrupt-task")
            .unwrap()
            .diagnostic
            .is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
}
