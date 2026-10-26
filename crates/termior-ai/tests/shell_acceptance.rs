//! Real subprocess acceptance of shell quoting and task cancellation.
#[test]
fn cancellation_interrupts_a_running_real_command_before_its_wait_timeout() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use termior_ai::{CancellationToken, RuntimeToolExecutor};
    let project = tempfile::tempdir().unwrap();
    let mut registry = termior_ai::ToolRegistry::default();
    registry
        .workspace_auth
        .authorize(&project.path().to_string_lossy());
    let executor = Arc::new(termior_ai::ToolExecutor::new(project.path(), registry).unwrap());
    executor.set_task_owner("prompt-cancel-task");
    std::fs::write(project.path().join("long-command.ps1"), "Set-Content ready.txt ready\nStart-Sleep -Seconds 60\nSet-Content forbidden.txt forbidden\n").unwrap();
    let command = if cfg!(windows) {
        "pwsh -NoProfile -File long-command.ps1"
    } else {
        "echo ready > ready.txt; sleep 60; echo forbidden > forbidden.txt"
    };
    let args = serde_json::json!({"command":command}).to_string();
    let token = CancellationToken::default();
    let worker_executor = executor.clone();
    let worker_token = token.clone();
    let worker = std::thread::spawn(move || {
        worker_executor.execute("long-command", "run_command", &args, &worker_token)
    });
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while !project.path().join("ready.txt").exists() && Instant::now() < ready_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let ready = project.path().join("ready.txt").exists();
    token.cancel();
    let stop_deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() && Instant::now() < stop_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped_promptly = worker.is_finished();
    // Always clean up the fixture before asserting, including the red run.
    executor.cancel_active();
    worker.join().unwrap().unwrap();
    assert!(
        ready,
        "the real command never reached the cancellation boundary"
    );
    assert!(
        stopped_promptly,
        "cancellation remained blocked on the 30-second tool wait"
    );
    assert!(!project.path().join("forbidden.txt").exists());
}

#[test]
fn cancelling_one_task_keeps_another_projects_background_process_alive() {
    use termior_ai::RuntimeToolExecutor;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let executor = |root: &std::path::Path, owner: &str| {
        let mut registry = termior_ai::ToolRegistry::default();
        registry.workspace_auth.authorize(&root.to_string_lossy());
        let executor = termior_ai::ToolExecutor::new(root, registry).unwrap();
        executor.set_task_owner(owner);
        executor
    };
    let first = executor(first.path(), "cancel-project-first");
    let second = executor(second.path(), "cancel-project-second");
    let command = if cfg!(windows) {
        "pwsh -NoProfile -Command Start-Sleep -Seconds 60"
    } else {
        "sleep 60"
    };
    let args = serde_json::json!({"command": command}).to_string();
    first.execute_approved("shell_bg_spawn", &args).unwrap();
    let second_session = second.execute_approved("shell_bg_spawn", &args).unwrap();
    let id = second_session
        .split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches("session_id=");
    let first_confirmed = first.cancel_active();
    let state = second
        .execute_auto(
            "command_status",
            &serde_json::json!({"session_id":id}).to_string(),
        )
        .unwrap();
    second.cancel_active();
    assert!(
        first_confirmed,
        "the cancelled task's own processes must stop"
    );
    assert!(
        state.to_ascii_lowercase().contains("running"),
        "cancelling the first project terminated the other project's command: {state}"
    );
}

#[cfg(windows)]
#[test]
fn agent_command_executes_quoted_powershell_in_a_project_with_spaces() {
    let project = tempfile::Builder::new()
        .prefix("termior project ")
        .tempdir()
        .unwrap();
    let mut registry = termior_ai::ToolRegistry::default();
    registry
        .workspace_auth
        .authorize(&project.path().to_string_lossy());
    let executor = termior_ai::ToolExecutor::new(project.path(), registry).unwrap();
    let result = termior_ai::RuntimeToolExecutor::execute(
        &executor,
        "quoted-command",
        "run_command",
        r#"{"command":"pwsh -NoProfile -Command \"Set-Content -LiteralPath 'quoted output.txt' -Value 'executed value'; Write-Output 'execution-proof'\""}"#,
        &termior_ai::CancellationToken::default(),
    ).unwrap();
    assert_eq!(
        std::fs::read_to_string(project.path().join("quoted output.txt"))
            .unwrap()
            .trim(),
        "executed value",
        "a zero exit code must not replace evidence that the script executed"
    );
    let termior_ai::ToolExecution::Completed(output) = result else {
        panic!("command did not complete")
    };
    assert!(output.contains("execution-proof"), "{output}");
}
