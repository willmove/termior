use std::time::Duration;

use termior_agent_host::{AcpBackend, AgentBackend, TaskLaunch};

/// Real ACP initialization and session creation, without starting a model turn.
/// On Windows, TERMIOR_OPENCODE_PATH should point to the installed native executable.
#[test]
#[ignore = "requires a locally installed OpenCode ACP backend"]
fn installed_opencode_initializes_and_creates_an_acp_session() {
    let executable = std::env::var("TERMIOR_OPENCODE_PATH").unwrap_or_else(|_| "opencode".into());
    let mut backend = AcpBackend::new(executable, vec!["acp".into(), "--pure".into()]);
    let info = backend.initialize().unwrap();
    assert_eq!(info.id, "acp");
    assert_eq!(info.version, "1");
    let project = tempfile::tempdir().unwrap();
    let binding = backend
        .create_task(TaskLaunch {
            task_id: "opencode-smoke".into(),
            project_dir: project.path().display().to_string(),
            environment_id: "direct".into(),
            model: None,
        })
        .unwrap();
    assert_eq!(binding.task_id, "opencode-smoke");
    assert!(!binding.backend_session_id.is_empty());
    backend.shutdown(Duration::from_secs(2)).unwrap();
}
