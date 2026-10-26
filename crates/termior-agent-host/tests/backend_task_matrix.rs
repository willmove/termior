//! The common task set every backend must be able to run (Stage C 8.2).
//!
//! One task set, three backends, one capability matrix. The built-in backend runs the set
//! against the deterministic in-process provider; Codex and ACP run it against the real
//! CLIs when they are installed and the caller opts in.
//!
//! The built-in run is not ignored: it always executes, so a capability regression is
//! caught without any external CLI. The two external runs are ignored by default because
//! they consume model quota.
//!
//! The matrix is printed by the run so the evidence is reviewable, and the recorded run
//! lives in `docs/process/agent-acceptance-2026-10-26.md`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use termior_agent_host::{
    AcpBackend, AgentBackend, BackendDecision, BackendEvent, BackendInfo, CapabilityProfile,
    CapabilitySupport, CodexBackend, CodexSandboxMode, InProcessBackend, TaskLaunch, TurnLaunch,
};

/// One task in the common set.
struct CommonTask {
    id: &'static str,
    /// What the agent is asked to do.
    prompt: &'static str,
    /// The exact string the answer must contain.
    expect: String,
}

/// The common set: read one file, then answer with a value computed from it.
///
/// The second task needs the first task's answer, so the tasks also exercise multi-turn
/// session continuity rather than a single lucky turn.
fn common_tasks(proof: &str) -> Vec<CommonTask> {
    vec![
        CommonTask {
            id: "read-file",
            prompt: "Read acceptance.txt in the current working directory using your file or shell tool. Reply with its complete exact content. This is a read-only acceptance test: do not write files, access the network, or inspect any files outside the current directory.",
            expect: proof.to_string(),
        },
        CommonTask {
            id: "follow-up",
            prompt: "Without reading any file again, reply with exactly the same content you reported for acceptance.txt, prefixed by the word ACK and a colon. Do not write files or use the network.",
            expect: format!("ACK:{proof}"),
        },
    ]
}

/// What a backend managed to do on the common task set.
#[derive(Debug, Clone)]
struct TaskOutcome {
    task_id: &'static str,
    answered: bool,
    duration_ms: u64,
    approvals_requested: usize,
    error: Option<String>,
}

/// A capability-matrix cell: what a backend claims and what it actually did.
#[derive(Debug, Clone)]
struct MatrixCell {
    backend: String,
    version: String,
    declared: CapabilityProfile,
    completed_tasks: usize,
    failed_tasks: usize,
    approvals_requested: usize,
    notes: Vec<String>,
}

fn fixture() -> (tempfile::TempDir, String) {
    let project = tempfile::tempdir().unwrap();
    let proof = format!(
        "termior-proof-{}",
        project.path().file_name().unwrap().to_string_lossy()
    );
    std::fs::write(project.path().join("acceptance.txt"), &proof).unwrap();
    (project, proof)
}

/// Run the common task set against one backend and report what happened.
fn run_task_set(
    backend: &mut dyn AgentBackend,
    project: &std::path::Path,
    proof: &str,
    task_id_prefix: &str,
    new_task_per_turn: bool,
) -> Vec<TaskOutcome> {
    let tasks = common_tasks(proof);
    let mut outcomes = Vec::new();
    let mut binding = backend
        .create_task(TaskLaunch {
            task_id: format!("{task_id_prefix}-common-set"),
            project_dir: project.display().to_string(),
            environment_id: "direct-common-set".into(),
            model: None,
        })
        .expect("the backend must accept a task");
    for task in tasks {
        let started = Instant::now();
        let mut output = String::new();
        let mut approvals = 0usize;
        let mut error = None;
        let mut answered = false;
        if let Err(start_error) = backend.start_turn(TurnLaunch {
            task_id: binding.task_id.clone(),
            backend_session_id: binding.backend_session_id.clone(),
            text: task.prompt.to_string(),
        }) {
            // A built-in task is terminal once its turn completes, so the second turn needs
            // a new task. A backend that keeps a session across turns does not.
            assert!(
                new_task_per_turn,
                "the backend must accept a second turn in the same session: {start_error}"
            );
            binding = backend
                .create_task(TaskLaunch {
                    task_id: format!("{task_id_prefix}-common-set-{}", task.id),
                    project_dir: project.display().to_string(),
                    environment_id: "direct-common-set".into(),
                    model: None,
                })
                .expect("the backend must accept a task");
            backend
                .start_turn(TurnLaunch {
                    task_id: binding.task_id.clone(),
                    backend_session_id: binding.backend_session_id.clone(),
                    text: task.prompt.to_string(),
                })
                .expect("the backend must accept a turn");
        }
        let deadline = started + Duration::from_secs(120);
        loop {
            if Instant::now() >= deadline {
                error = Some("the turn timed out".to_string());
                break;
            }
            match backend.next_event(Duration::from_millis(200)) {
                Ok(Some(BackendEvent::AgentMessageDelta { text, .. })) => output.push_str(&text),
                Ok(Some(BackendEvent::ApprovalRequested {
                    backend_request_id, ..
                })) => {
                    approvals += 1;
                    // The common set is read-only: refuse every side effect and require the
                    // backend to keep working without it.
                    backend
                        .respond(BackendDecision {
                            backend_request_id,
                            approved: false,
                            granted_permission_ids: None,
                        })
                        .expect("the backend must accept an approval decision");
                }
                Ok(Some(BackendEvent::TurnCompleted {
                    success, status, ..
                })) => {
                    if success {
                        answered = output.contains(&task.expect);
                        if !answered {
                            error = Some(format!(
                                "the turn completed without the expected answer (status {status})"
                            ));
                        }
                    } else {
                        error = Some(format!("the turn did not succeed: {status}"));
                    }
                    break;
                }
                Ok(Some(BackendEvent::Error { message, .. })) => {
                    error = Some(message);
                    break;
                }
                Ok(Some(BackendEvent::Diagnostic { method, .. })) => {
                    eprintln!("diagnostic method={method}");
                }
                Ok(_) => {}
                Err(backend_error) => {
                    error = Some(backend_error.to_string());
                    break;
                }
            }
        }
        let failed = error.is_some();
        outcomes.push(TaskOutcome {
            task_id: task.id,
            answered,
            duration_ms: started.elapsed().as_millis() as u64,
            approvals_requested: approvals,
            error,
        });
        if failed {
            // A failed task ends the run: continuing would test nothing.
            break;
        }
    }
    outcomes
}

fn summarize(
    info: &BackendInfo,
    declared: CapabilityProfile,
    outcomes: &[TaskOutcome],
    mut notes: Vec<String>,
) -> MatrixCell {
    let completed_tasks = outcomes.iter().filter(|outcome| outcome.answered).count();
    let failed_tasks = outcomes.len() - completed_tasks;
    let approvals_requested = outcomes
        .iter()
        .map(|outcome| outcome.approvals_requested)
        .sum();
    for outcome in outcomes {
        notes.push(format!(
            "{}: completed={} elapsed_ms={}",
            outcome.task_id, outcome.answered, outcome.duration_ms
        ));
        if let Some(error) = &outcome.error {
            notes.push(format!("{}: {error}", outcome.task_id));
        }
    }
    MatrixCell {
        backend: info.id.clone(),
        version: info.version.clone(),
        declared,
        completed_tasks,
        failed_tasks,
        approvals_requested,
        notes,
    }
}

/// A built-in backend provider that answers the common task set deterministically.
///
/// The built-in runtime is exercised through the same `AgentBackend` contract as the
/// external backends, with a scripted provider instead of a model. It proves the wiring,
/// not the model quality.
struct ScriptedProvider {
    proof: String,
}

impl termior_ai::Provider for ScriptedProvider {
    fn stream_chat<'a>(
        &'a self,
        req: &'a termior_ai::ProviderRequest,
    ) -> futures::stream::BoxStream<'a, termior_ai::ChatEvent> {
        use termior_ai::ChatEvent;
        // Answer from the request, not from a call counter: the runtime may call the
        // provider more than once per turn, and a scripted provider must not depend on
        // how many times it was asked.
        let last_user_input = req
            .messages
            .iter()
            .rev()
            .find(|message| message.role == termior_ai::Role::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let answer = if last_user_input.starts_with("Without reading any file again") {
            format!("ACK:{}", self.proof)
        } else {
            self.proof.clone()
        };
        Box::pin(futures::stream::iter(vec![
            ChatEvent::TextDelta(answer.clone()),
            ChatEvent::Done(termior_ai::Message::assistant(answer)),
        ]))
    }
}

struct NoTools;

impl termior_ai::RuntimeToolExecutor for NoTools {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        _normalized_arguments: &str,
        _cancellation: &termior_ai::CancellationToken,
    ) -> Result<termior_ai::ToolExecution, String> {
        Err("the common task set does not use the built-in tools".into())
    }
}

#[test]
fn the_built_in_backend_runs_the_common_task_set() {
    let (project, proof) = fixture();
    let provider = ScriptedProvider {
        proof: proof.clone(),
    };
    let mut backend = InProcessBackend::new(Arc::new(provider), Arc::new(NoTools));
    let info = backend.initialize().unwrap();
    let declared = backend.capabilities();
    let outcomes = run_task_set(&mut backend, project.path(), &proof, "builtin", true);
    backend.shutdown(Duration::from_secs(2)).unwrap();

    let cell = summarize(&info, declared, &outcomes, Vec::new());
    assert_eq!(
        cell.completed_tasks, 2,
        "the built-in backend must run every common task: {cell:?}"
    );
    assert_eq!(cell.failed_tasks, 0);
    assert_eq!(cell.approvals_requested, 0);
    // The built-in backend declares resume, cancel, approvals, terminal, diff, model and
    // sandbox reporting as supported, and only fork and mid-turn steer as unsupported.
    assert!(cell.declared.can_resume());
    assert!(cell.declared.can_cancel());
    assert_eq!(cell.declared.session_fork, CapabilitySupport::Unsupported);
    assert_eq!(cell.declared.mid_turn_steer, CapabilitySupport::Unsupported);
}

/// The capability matrix is only meaningful if every backend reports the same shape.
///
/// The built-in backend always runs. Codex and ACP join when they are installed and the
/// caller opts in, so the matrix is produced by a real run rather than a hand-written table.
#[test]
#[ignore = "requires TERMIOR_REAL_BACKEND_TESTS=1 and locally installed Codex and OpenCode"]
fn the_three_backends_share_one_capability_matrix() {
    assert_eq!(
        std::env::var("TERMIOR_REAL_BACKEND_TESTS").as_deref(),
        Ok("1"),
        "set TERMIOR_REAL_BACKEND_TESTS=1 to run the external backends"
    );
    let (project, proof) = fixture();
    let mut cells: Vec<MatrixCell> = Vec::new();

    // Built-in: deterministic provider, same contract.
    let provider = ScriptedProvider {
        proof: proof.clone(),
    };
    let mut builtin = InProcessBackend::new(Arc::new(provider), Arc::new(NoTools));
    let info = builtin.initialize().unwrap();
    let declared = builtin.capabilities();
    let outcomes = run_task_set(&mut builtin, project.path(), &proof, "builtin", true);
    builtin.shutdown(Duration::from_secs(2)).unwrap();
    cells.push(summarize(&info, declared, &outcomes, Vec::new()));

    // Codex: real CLI.
    let codex = std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into());
    let mut codex_backend = CodexBackend::new(codex).with_sandbox(CodexSandboxMode::from_env());
    match codex_backend.initialize() {
        Ok(info) => {
            let declared = codex_backend.capabilities();
            let outcomes = run_task_set(&mut codex_backend, project.path(), &proof, "codex", false);
            codex_backend.shutdown(Duration::from_secs(2)).unwrap();
            cells.push(summarize(&info, declared, &outcomes, Vec::new()));
        }
        Err(error) => cells.push(MatrixCell {
            backend: "codex-app-server".into(),
            version: "unavailable".into(),
            declared: CapabilityProfile::default(),
            completed_tasks: 0,
            failed_tasks: 1,
            approvals_requested: 0,
            notes: vec![format!("initialize failed: {error}")],
        }),
    }

    // ACP: real CLI.
    let opencode = std::env::var("TERMIOR_OPENCODE_PATH").unwrap_or_else(|_| "opencode".into());
    let mut acp_backend = AcpBackend::new(opencode, vec!["acp".into(), "--pure".into()]);
    match acp_backend.initialize() {
        Ok(info) => {
            let declared = acp_backend.capabilities();
            let outcomes = run_task_set(&mut acp_backend, project.path(), &proof, "acp", false);
            acp_backend.shutdown(Duration::from_secs(2)).unwrap();
            cells.push(summarize(&info, declared, &outcomes, Vec::new()));
        }
        Err(error) => cells.push(MatrixCell {
            backend: "acp".into(),
            version: "unavailable".into(),
            declared: CapabilityProfile::default(),
            completed_tasks: 0,
            failed_tasks: 1,
            approvals_requested: 0,
            notes: vec![format!("initialize failed: {error}")],
        }),
    }

    // The read-only set must not have produced any approval request.
    for cell in &cells {
        assert_eq!(
            cell.approvals_requested, 0,
            "a read-only task must not need approval: {cell:?}"
        );
    }
    let builtin = cells
        .iter()
        .find(|cell| cell.backend == "termior-built-in")
        .unwrap();
    assert_eq!(builtin.completed_tasks, 2, "{builtin:?}");

    // Print the matrix so the run leaves a reviewable record.
    for cell in &cells {
        println!(
            "MATRIX backend={} version={} completed={} failed={} approvals={} resume={:?} fork={:?} steer={:?} cancel={:?} approvals_declared={:?} terminal={:?} diff={:?} model={:?} sandbox={:?}",
            cell.backend,
            cell.version,
            cell.completed_tasks,
            cell.failed_tasks,
            cell.approvals_requested,
            cell.declared.session_resume,
            cell.declared.session_fork,
            cell.declared.mid_turn_steer,
            cell.declared.cancel,
            cell.declared.approval_requests,
            cell.declared.host_terminal,
            cell.declared.diff_events,
            cell.declared.model_selection,
            cell.declared.sandbox_reporting,
        );
        for note in &cell.notes {
            println!("MATRIX_NOTE backend={} {note}", cell.backend);
        }
    }
}
