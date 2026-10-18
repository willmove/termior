//! Stage D 2.3: query Codex/ACP on restore, then reduce. Never replay side effects.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use termior_agent_host::{
    checkpoint_session, reconcile_persisted_backend_bindings, restore_external_session,
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    BackendSessionReport, CapabilityProfile, CapabilitySupport, ExternalSessionCheckpoint,
    ManagedSessionState, TaskLaunch, TurnLaunch,
};
use termior_store::{RecoveredOperationState, RecoveryCenter};

struct RecordingBackend {
    queryable: bool,
    status: String,
    fail_query: bool,
    calls: Arc<Mutex<Vec<String>>>,
}

impl AgentBackend for RecordingBackend {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError> {
        Ok(BackendInfo {
            id: "codex-app-server".into(),
            display_name: "Fake".into(),
            version: "1".into(),
            authentication_source: "test".into(),
            model_source: "test".into(),
            capabilities: self.capabilities(),
        })
    }

    fn capabilities(&self) -> CapabilityProfile {
        CapabilityProfile {
            session_resume: if self.queryable {
                CapabilitySupport::Supported
            } else {
                CapabilitySupport::Unsupported
            },
            ..CapabilityProfile::default()
        }
    }

    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError> {
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: "session-1".into(),
            backend_turn_id: None,
        })
    }

    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError> {
        self.calls.lock().unwrap().push("resume_task".into());
        Ok(binding)
    }

    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError> {
        self.calls.lock().unwrap().push("start_turn".into());
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: launch.backend_session_id,
            backend_turn_id: Some("turn".into()),
        })
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<Option<BackendEvent>, BackendError> {
        Ok(None)
    }

    fn respond(&mut self, _decision: BackendDecision) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push("respond".into());
        Ok(())
    }

    fn cancel(&mut self, _binding: &BackendBinding) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push("cancel".into());
        Ok(())
    }

    fn shutdown(&mut self, _timeout: Duration) -> Result<(), BackendError> {
        Ok(())
    }

    fn query_session(
        &mut self,
        _binding: &BackendBinding,
    ) -> Result<BackendSessionReport, BackendError> {
        self.calls.lock().unwrap().push("query".into());
        if self.fail_query {
            return Err(BackendError::Unavailable("backend down".into()));
        }
        Ok(BackendSessionReport {
            status: self.status.clone(),
        })
    }
}

fn binding() -> BackendBinding {
    BackendBinding {
        task_id: "task-ext".into(),
        backend_session_id: "session-1".into(),
        backend_turn_id: None,
    }
}

#[test]
fn queryable_restore_reads_status_without_replaying_side_effects() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backend = RecordingBackend {
        queryable: true,
        status: "waiting-approval".into(),
        fail_query: false,
        calls: calls.clone(),
    };
    let report = restore_external_session(&mut backend, &binding());
    assert_eq!(report.state, RecoveredOperationState::Waiting);
    assert!(report.queried);
    assert_eq!(calls.lock().unwrap().as_slice(), ["query"]);

    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut running = RecordingBackend {
        queryable: true,
        status: "running".into(),
        fail_query: false,
        calls: calls.clone(),
    };
    let report = restore_external_session(&mut running, &binding());
    assert_eq!(report.state, RecoveredOperationState::Unknown);
    assert_eq!(calls.lock().unwrap().as_slice(), ["query"]);
}

#[test]
fn non_queryable_and_failed_queries_stay_unknown_without_side_effects() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backend = RecordingBackend {
        queryable: false,
        status: "succeeded".into(),
        fail_query: false,
        calls: calls.clone(),
    };
    let report = restore_external_session(&mut backend, &binding());
    assert_eq!(report.state, RecoveredOperationState::Unknown);
    assert!(!report.queried);
    assert!(calls.lock().unwrap().is_empty());

    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut backend = RecordingBackend {
        queryable: true,
        status: "succeeded".into(),
        fail_query: true,
        calls: calls.clone(),
    };
    let report = restore_external_session(&mut backend, &binding());
    assert_eq!(report.state, RecoveredOperationState::Unknown);
    assert!(report.queried);
    assert_eq!(calls.lock().unwrap().as_slice(), ["query"]);
}

#[cfg(unix)]
#[test]
fn persisted_codex_and_acp_sessions_are_queried_before_the_recovery_center_lists_them() {
    let root = tempfile::tempdir().unwrap();
    let tasks = root.path().join("agent-tasks");
    let waiting = write_fake(root.path(), "waiting", "codex", "waiting-approval", true);
    let running = write_fake(root.path(), "running", "codex", "running", true);
    let blind = write_fake(root.path(), "blind", "acp", "succeeded", false);
    checkpoint_session(&checkpoint(
        &tasks,
        "waiting",
        "codex-app-server",
        &waiting,
        true,
    ))
    .unwrap();
    checkpoint_session(&checkpoint(
        &tasks,
        "running",
        "codex-app-server",
        &running,
        true,
    ))
    .unwrap();
    checkpoint_session(&checkpoint(&tasks, "blind", "acp", &blind, false)).unwrap();

    let reports = reconcile_persisted_backend_bindings(&tasks).unwrap();
    let state = |id: &str| {
        reports
            .iter()
            .find(|report| report.task_id == id)
            .unwrap()
            .state
    };
    assert_eq!(state("waiting"), RecoveredOperationState::Waiting);
    assert_eq!(state("running"), RecoveredOperationState::Unknown);
    assert_eq!(state("blind"), RecoveredOperationState::Unknown);

    let listed = RecoveryCenter::scan(&tasks).unwrap();
    assert_eq!(
        listed
            .iter()
            .find(|task| task.task_id == "running")
            .unwrap()
            .recovered_state,
        RecoveredOperationState::Unknown
    );
    assert_eq!(
        listed
            .iter()
            .find(|task| task.task_id == "blind")
            .unwrap()
            .recovered_state,
        RecoveredOperationState::Unknown
    );
    assert_eq!(
        listed
            .iter()
            .find(|task| task.task_id == "waiting")
            .unwrap()
            .recovered_state,
        RecoveredOperationState::Waiting
    );
    let waiting_log = std::fs::read_to_string(root.path().join("waiting.log")).unwrap();
    let running_log = std::fs::read_to_string(root.path().join("running.log")).unwrap();
    let blind_log = std::fs::read_to_string(root.path().join("blind.log")).unwrap_or_default();
    assert!(waiting_log.contains("thread/resume"), "{waiting_log}");
    assert!(running_log.contains("thread/resume"), "{running_log}");
    assert!(!waiting_log.contains("SIDE_EFFECT"), "{waiting_log}");
    assert!(!running_log.contains("SIDE_EFFECT"), "{running_log}");
    assert!(
        !blind_log.contains("session/load") && !blind_log.contains("SIDE_EFFECT"),
        "{blind_log}"
    );
}

#[cfg(unix)]
#[test]
fn acp_without_load_session_is_not_asked_to_replay_the_prompt() {
    let root = tempfile::tempdir().unwrap();
    let tasks = root.path().join("agent-tasks");
    let program = write_fake(root.path(), "acp-closed", "acp", "end_turn", false);
    checkpoint_session(&checkpoint(&tasks, "acp-closed", "acp", &program, true)).unwrap();
    let reports = reconcile_persisted_backend_bindings(&tasks).unwrap();
    assert_eq!(reports[0].state, RecoveredOperationState::Unknown);
    assert!(!reports[0].queried);
    let log = std::fs::read_to_string(root.path().join("acp-closed.log")).unwrap();
    assert!(log.contains("initialize"), "{log}");
    assert!(!log.contains("session/load"), "{log}");
    assert!(!log.contains("SIDE_EFFECT"), "{log}");
    assert_eq!(
        RecoveryCenter::scan(&tasks).unwrap()[0].recovered_state,
        RecoveredOperationState::Unknown
    );
}

#[cfg(unix)]
fn checkpoint(
    tasks: &Path,
    task_id: &str,
    backend_id: &str,
    program: &Path,
    queryable: bool,
) -> ExternalSessionCheckpoint {
    ExternalSessionCheckpoint {
        tasks_root: tasks.to_path_buf(),
        task_id: task_id.into(),
        backend_id: backend_id.into(),
        backend_session_id: format!("{task_id}-session"),
        queryable,
        program: program.display().to_string(),
        args: Vec::new(),
        project_dir: "/work".into(),
        managed_state: ManagedSessionState::Running,
    }
}

#[cfg(unix)]
fn write_fake(dir: &Path, name: &str, protocol: &str, status: &str, load_session: bool) -> PathBuf {
    let log = dir.join(format!("{name}.log"));
    let script = dir.join(name);
    let log_py = serde_json::to_string(&log.display().to_string()).unwrap();
    let protocol_py = serde_json::to_string(protocol).unwrap();
    let status_py = serde_json::to_string(status).unwrap();
    let load_py = if load_session { "True" } else { "False" };
    let script_body = format!(
        r#"#!/usr/bin/env python3
import json, sys
log_path = {log_py}
protocol = {protocol_py}
status = {status_py}
load_session = {load_py}
if "--version" in sys.argv:
    print("codex-cli 0.153.0", flush=True)
    raise SystemExit(0)
log = open(log_path, "a", encoding="utf-8")
for line in sys.stdin:
    log.write(line)
    log.flush()
    text = line.strip()
    if not text:
        continue
    try:
        msg = json.loads(text)
    except json.JSONDecodeError:
        continue
    method = msg.get("method")
    if method in ("turn/start", "session/prompt", "session/new"):
        log.write("SIDE_EFFECT " + method + "\n")
        log.flush()
    if "id" not in msg or method is None:
        continue
    if method == "initialize" and protocol == "acp":
        result = {{"protocolVersion": 1, "agentCapabilities": {{"loadSession": load_session}}, "authMethods": []}}
    elif method == "initialize":
        result = {{"userAgent": "fake"}}
    elif method == "thread/resume":
        result = {{"thread": {{"id": "th-1", "status": status}}}}
    elif method == "session/load":
        result = {{"status": status}}
    else:
        result = {{}}
    sys.stdout.write(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}) + "\n")
    sys.stdout.flush()
"#
    );
    std::fs::write(&script, script_body).unwrap();
    std::fs::write(&log, "").unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    script
}
