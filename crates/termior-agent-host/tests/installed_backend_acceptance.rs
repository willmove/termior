//! Opt-in model turns through Termior's real backend adapters, confined to a fresh fixture.
//! These tests use the CLI's existing authentication and can consume model quota.
use std::time::{Duration, Instant};

use termior_agent_host::{
    AcpBackend, AgentBackend, BackendDecision, BackendEvent, CodexBackend, CodexSandboxMode,
    TaskLaunch, TurnLaunch,
};

fn read_evidence(mut backend: Box<dyn AgentBackend>) {
    assert_eq!(
        std::env::var("TERMIOR_REAL_BACKEND_TESTS").as_deref(),
        Ok("1")
    );
    let project = tempfile::tempdir().unwrap();
    let proof = format!(
        "termior-proof-{}",
        project.path().file_name().unwrap().to_string_lossy()
    );
    std::fs::write(project.path().join("acceptance.txt"), &proof).unwrap();
    let before = std::fs::read(project.path().join("acceptance.txt")).unwrap();
    let info = backend.initialize().unwrap();
    let binding = backend
        .create_task(TaskLaunch {
            task_id: "installed-read-evidence".into(),
            project_dir: project.path().display().to_string(),
            environment_id: "direct-acceptance-fixture".into(),
            model: None,
        })
        .unwrap();
    backend.start_turn(TurnLaunch {
        task_id: binding.task_id.clone(),
        backend_session_id: binding.backend_session_id.clone(),
        text: "Read acceptance.txt in the current working directory using your file or shell tool. Reply with its complete exact content. This is a read-only acceptance test: do not write files, access the network, or inspect any files outside the current directory.".into(),
    }).unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(90);
    let mut output = String::new();
    let result = loop {
        if Instant::now() >= deadline {
            break Err("model turn timed out".to_string());
        }
        match backend.next_event(Duration::from_millis(200)) {
            Ok(Some(BackendEvent::AgentMessageDelta { text, .. })) => output.push_str(&text),
            Ok(Some(BackendEvent::ApprovalRequested {
                backend_request_id, ..
            })) => {
                backend
                    .respond(BackendDecision {
                        backend_request_id,
                        approved: false,
                        granted_permission_ids: None,
                    })
                    .unwrap();
            }
            Ok(Some(BackendEvent::TurnCompleted {
                success, status, ..
            })) => {
                break if success {
                    Ok(())
                } else {
                    Err(format!("turn did not succeed: {status}"))
                };
            }
            Ok(Some(BackendEvent::Error { message, .. })) => break Err(message),
            Ok(Some(BackendEvent::Diagnostic { method, raw })) => {
                if method == "error" {
                    let redactor = termior_store::StreamingRedactor::new(Vec::<String>::new());
                    eprintln!(
                        "backend diagnostic error: {}",
                        redactor.redact(&raw.to_string()).text
                    );
                } else {
                    eprintln!("backend diagnostic method={method}");
                }
            }
            Ok(_) => {}
            Err(error) => break Err(error.to_string()),
        }
    };
    backend.shutdown(Duration::from_secs(2)).unwrap();
    result.unwrap();
    assert!(
        output.contains(&proof),
        "{} did not return the fixture's unprompted proof; received {output:?}",
        info.id
    );
    assert_eq!(
        std::fs::read(project.path().join("acceptance.txt")).unwrap(),
        before
    );
    println!(
        "REAL_BACKEND_READ_OK backend={} version={} proof_verified=true elapsed_s={}",
        info.id,
        info.version,
        started.elapsed().as_secs()
    );
}

#[test]
#[ignore = "requires authenticated Codex and explicit TERMIOR_REAL_BACKEND_TESTS=1"]
fn installed_codex_reads_real_workspace_evidence() {
    read_evidence(Box::new(
        CodexBackend::new(std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into()))
            .with_sandbox(CodexSandboxMode::from_env()),
    ));
}

#[test]
#[ignore = "requires authenticated OpenCode and explicit TERMIOR_REAL_BACKEND_TESTS=1"]
fn installed_opencode_reads_real_workspace_evidence() {
    read_evidence(Box::new(AcpBackend::new(
        std::env::var("TERMIOR_OPENCODE_PATH").unwrap_or_else(|_| "opencode".into()),
        vec!["acp".into(), "--pure".into()],
    )));
}
