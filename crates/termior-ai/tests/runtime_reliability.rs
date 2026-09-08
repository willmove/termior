use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use termior_ai::{
    AcceptanceCheck, AcceptanceCriterion, AcceptanceReport, AcceptanceStatus, ApprovalPolicy,
    CancellationToken, ChatEvent, EvaluationSuiteReport, Message, MockProvider, RuntimeBudgets,
    RuntimeToolExecutor, ScenarioReport, Task, TaskCommand, TaskConfig, TaskRuntime, TaskState,
    ToolCall, ToolExecution, ToolExecutor, ToolRegistry, ToolState, WaitingReason,
};
use termior_security::workspace::WorkspaceAuthRegistry;

#[derive(Default)]
struct RecordingExecutor {
    calls: Arc<Mutex<Vec<String>>>,
}

struct PassingCommandExecutor;

impl RuntimeToolExecutor for PassingCommandExecutor {
    fn execute(
        &self,
        _call_id: &str,
        tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        assert_eq!(tool, "run_command");
        Ok(ToolExecution::Completed(
            "exit=0\nstdout:\nall tests passed".into(),
        ))
    }
}

struct HugeOutputExecutor;

impl RuntimeToolExecutor for HugeOutputExecutor {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        Ok(ToolExecution::Completed("x".repeat(700_000)))
    }
}

struct JournalBoundaryExecutor {
    data_root: std::path::PathBuf,
    task_id: String,
}

impl RuntimeToolExecutor for JournalBoundaryExecutor {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        let recovered = termior_store::TaskJournal::recover(
            &self.data_root.join("agent-tasks"),
            &self.task_id,
            &[],
        )
        .map_err(|error| error.to_string())?;
        let last = recovered
            .events
            .last()
            .ok_or_else(|| "journal was empty at side-effect boundary".to_owned())?;
        if last.kind != "tool_state_changed" || last.payload["to"] != "running" {
            return Err(format!(
                "missing durable running boundary: {}",
                last.payload
            ));
        }
        Ok(ToolExecution::Completed("observed durable boundary".into()))
    }
}

#[derive(Default)]
struct FailThenPassCommandExecutor(AtomicUsize);

impl RuntimeToolExecutor for FailThenPassCommandExecutor {
    fn execute(
        &self,
        _call_id: &str,
        tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        assert_eq!(tool, "run_command");
        let attempt = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolExecution::Completed(if attempt == 0 {
            "exit=1\nstderr:\ntest failed".into()
        } else {
            "exit=0\nstdout:\ntest passed".into()
        }))
    }
}

struct PendingProvider;

impl termior_ai::Provider for PendingProvider {
    fn stream_chat<'a>(
        &'a self,
        _request: &'a termior_ai::ProviderRequest,
    ) -> BoxStream<'a, ChatEvent> {
        Box::pin(futures::stream::pending())
    }
}

#[derive(Default)]
struct RequestRecordingProvider {
    messages: Mutex<Vec<Vec<Message>>>,
}

impl termior_ai::Provider for RequestRecordingProvider {
    fn stream_chat<'a>(
        &'a self,
        request: &'a termior_ai::ProviderRequest,
    ) -> BoxStream<'a, ChatEvent> {
        self.messages.lock().unwrap().push(request.messages.clone());
        Box::pin(futures::stream::iter([ChatEvent::Done(
            Message::assistant("done"),
        )]))
    }
}

struct UnconfirmedCancellationExecutor;

impl RuntimeToolExecutor for UnconfirmedCancellationExecutor {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        unreachable!()
    }

    fn cancel_active(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct ArgumentRecordingExecutor(Mutex<Vec<String>>);

impl RuntimeToolExecutor for ArgumentRecordingExecutor {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        self.0.lock().unwrap().push(normalized_arguments.into());
        Ok(ToolExecution::Completed("ok".into()))
    }
}

#[test]
fn every_builtin_tool_has_a_closed_schema() {
    let registry = ToolRegistry::default();
    let contracts = registry.contracts();
    assert!(!contracts.is_empty());
    for contract in contracts {
        assert_eq!(contract.parameters["type"], "object", "{}", contract.name);
        assert_eq!(
            contract.parameters["additionalProperties"], false,
            "{} accepts undeclared fields",
            contract.name
        );
        assert!(
            contract.parameters["properties"].is_object(),
            "{}",
            contract.name
        );
        assert!(contract.default_timeout_ms > 0, "{}", contract.name);
        assert!(contract.max_output_bytes > 0, "{}", contract.name);
    }
}

#[test]
fn task_runtime_journal_persistence_is_incremental_and_snapshot_backed() {
    let data = tempfile::tempdir().unwrap();
    let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("done"))]);
    let executor = RecordingExecutor::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("persist", data.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "go".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    runtime.persist_journal(data.path()).unwrap();
    runtime.persist_journal(data.path()).unwrap();
    let recovered = termior_store::TaskJournal::recover(
        &data.path().join("agent-tasks"),
        &runtime.task().id.0,
        &[],
    )
    .unwrap();
    assert_eq!(recovered.events.len(), runtime.events().len());
    assert!(termior_store::TaskJournal::load_snapshot(
        &data.path().join("agent-tasks"),
        &runtime.task().id.0
    )
    .unwrap()
    .is_some());
}

#[test]
fn critical_running_event_is_durable_before_tool_execution() {
    let data = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "durable-read".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("durable", data.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .try_with_journal(data.path())
    .unwrap();
    let executor = JournalBoundaryExecutor {
        data_root: data.path().to_path_buf(),
        task_id: runtime.task().id.0.clone(),
    };
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "read".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
}

#[test]
fn snapshot_plus_replay_recovers_incomplete_running_state_as_unknown() {
    let data = tempfile::tempdir().unwrap();
    let runtime = TaskRuntime::new(
        TaskConfig::new("recover", data.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .try_with_journal(data.path())
    .unwrap();
    let task_id = runtime.task().id.0.clone();
    let mut journal =
        termior_store::TaskJournal::create(&data.path().join("agent-tasks"), &task_id).unwrap();
    let kind = termior_ai::TaskEventKind::StateChanged {
        from: TaskState::Idle,
        to: TaskState::Running,
    };
    journal
        .append("state_changed", serde_json::to_value(kind).unwrap(), true)
        .unwrap();
    drop(journal);
    let recovered = TaskRuntime::recover_journal(
        data.path(),
        &task_id,
        ApprovalPolicy::Prompt,
        ToolRegistry::default(),
    )
    .unwrap();
    assert_eq!(recovered.task().state, TaskState::Unknown);
    assert_eq!(recovered.events().len(), 1);
}

#[test]
fn live_runtime_compaction_injects_a_code_built_task_brief() {
    let provider = RequestRecordingProvider::default();
    let old_marker = "old-history-marker".repeat(200);
    let history = vec![
        Message::user(old_marker.clone()),
        Message::assistant(old_marker.clone()),
    ];
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("compact", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_history(history)
    .with_context_compaction(256, 2);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "current request".into(),
        },
        &provider,
        &RecordingExecutor::default(),
        &mut |_| {},
    ))
    .unwrap();
    let requests = provider.messages.lock().unwrap();
    let messages = &requests[0];
    assert!(messages
        .iter()
        .any(|message| message.content.contains("TaskBrief reconstructed")));
    assert!(!messages
        .iter()
        .any(|message| message.content.contains(&old_marker)));
    assert!(runtime.events().iter().any(|event| matches!(
        event.kind,
        termior_ai::TaskEventKind::ContextCompacted { .. }
    )));
}

#[test]
fn unshrinkable_required_input_pauses_instead_of_thrashing() {
    let provider = RequestRecordingProvider::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("too large", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_context_compaction(64, 1);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "x".repeat(2000),
        },
        &provider,
        &RecordingExecutor::default(),
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingUser);
    assert!(provider.messages.lock().unwrap().is_empty());
}

#[test]
fn invalid_arguments_fail_before_approval_or_execution() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "write-invalid".into(),
            name: "write_file".into(),
            arguments: r#"{"content":"TOP-SECRET","unexpected":true}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("handled"))],
    ]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("validate", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );

    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
    assert_eq!(runtime.invocations()[0].state, ToolState::Failed);
    let output = &runtime.invocations()[0].result.as_ref().unwrap().output;
    assert!(output.contains("write_file"));
    assert!(output.contains("$.path"));
    assert!(!output.contains("TOP-SECRET"));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn write_tool_waits_for_hunk_review_then_reports_the_real_apply_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "old\n").unwrap();
    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([dir
        .path()
        .display()
        .to_string()]));
    let executor = ToolExecutor::new(dir.path(), registry.clone()).unwrap();
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "write-real".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"new\n"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant(
            "verified by inspection",
        ))],
    ]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("edit", dir.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_tools(registry);

    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "edit a.txt".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-real".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(runtime.task().state, TaskState::WaitingChangeReview);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    let WaitingReason::ChangeReview { change_set_id, .. } =
        runtime.task().waiting_reason.clone().unwrap()
    else {
        panic!("expected change review")
    };
    futures::executor::block_on(runtime.handle(
        TaskCommand::ReviewChange {
            change_set_id,
            accepted_hunks: vec![0],
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "new\n");
    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
    let tool_result = runtime.invocations()[0].result.as_ref().unwrap();
    assert!(tool_result.ok);
    assert!(tool_result.output.contains("applied=1"));
    assert!(tool_result.output.contains("rejected=0"));
}

#[test]
fn pre_tool_hook_replacement_is_revalidated_and_shown_in_the_new_approval() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let (program, args) = if cfg!(windows) {
        let script = dir.path().join("replace.ps1");
        std::fs::write(
            &script,
            "[Console]::In.ReadToEnd() | Out-Null\n[Console]::Out.Write('{\"decision\":\"replace\",\"arguments\":{\"path\":\"b.txt\",\"content\":\"new\"}}')",
        )
        .unwrap();
        (
            "powershell".to_owned(),
            vec![
                "-NoProfile".into(),
                "-File".into(),
                script.display().to_string(),
            ],
        )
    } else {
        let script = dir.path().join("replace.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"decision\":\"replace\",\"arguments\":{\"path\":\"b.txt\",\"content\":\"new\"}}'\n",
        )
        .unwrap();
        ("sh".to_owned(), vec![script.display().to_string()])
    };
    let pipeline = Arc::new(termior_hooks::HookPipeline::new(vec![
        termior_hooks::HookConfig {
            id: "replace-target".into(),
            point: termior_hooks::HookPoint::PreTool,
            program,
            args,
            enabled: true,
            fail_closed: true,
        },
    ]));
    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([dir
        .path()
        .display()
        .to_string()]));
    let executor = ToolExecutor::new(dir.path(), registry.clone())
        .unwrap()
        .with_hook_pipeline(pipeline.clone());
    let provider = MockProvider::single(vec![ChatEvent::Done(assistant_with_calls(vec![
        ToolCall {
            id: "hook-write".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"old"}"#.into(),
        },
    ]))]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("hook", dir.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_tools(registry);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    let approval = runtime.pending_approval().unwrap();
    assert!(approval.arguments.contains("b.txt"));
    assert!(!approval.arguments.contains("a.txt"));
    assert_eq!(pipeline.recent()[0].decision, "replace");
}

#[test]
fn provider_retries_only_errors_before_any_response_progress() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Error("temporary-1".into())],
        vec![ChatEvent::Error("temporary-2".into())],
        vec![ChatEvent::Done(Message::assistant("recovered"))],
    ]);
    let executor = RecordingExecutor::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("retry", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "hello".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
    assert_eq!(runtime.task().usage.model_steps, 1);
    assert!(
        runtime
            .events()
            .iter()
            .filter(|event| matches!(event.kind, termior_ai::TaskEventKind::Diagnostic { .. }))
            .count()
            >= 2
    );
}

#[test]
fn response_progress_disables_provider_retry() {
    let provider = MockProvider::new(vec![
        vec![
            ChatEvent::TextDelta("partial".into()),
            ChatEvent::Error("connection lost".into()),
        ],
        vec![ChatEvent::Done(Message::assistant("must not replay"))],
    ]);
    let executor = RecordingExecutor::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("no unsafe retry", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    let error = futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "hello".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap_err();
    assert!(error.to_string().contains("connection lost"));
    assert_eq!(runtime.task().state, TaskState::Failed);
}

#[test]
fn model_step_budget_is_preserved_across_approval_resume() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "write-budget".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"x"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let budgets = RuntimeBudgets {
        max_model_steps: 1,
        ..RuntimeBudgets::default()
    };
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("budget", "/project", "native", "direct"),
        budgets,
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    assert_eq!(runtime.task().usage.model_steps, 1);

    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-budget".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingUser);
    assert_eq!(runtime.task().usage.model_steps, 1);
    assert_eq!(*calls.lock().unwrap(), vec!["write_file"]);

    let mut increased = runtime.task().budgets.clone();
    increased.max_model_steps = 2;
    futures::executor::block_on(runtime.handle(
        TaskCommand::IncreaseBudgets { budgets: increased },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
    assert_eq!(runtime.task().usage.model_steps, 2);
    assert_eq!(*calls.lock().unwrap(), vec!["write_file"]);
}

#[test]
fn cancelling_while_waiting_for_approval_prevents_all_remaining_calls() {
    let provider = MockProvider::single(vec![ChatEvent::Done(assistant_with_calls(vec![
        ToolCall {
            id: "write-cancel".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"x"}"#.into(),
        },
        ToolCall {
            id: "run-cancel".into(),
            name: "run_command".into(),
            arguments: r#"{"command":"test"}"#.into(),
        },
    ]))]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("cancel", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write and test".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    futures::executor::block_on(runtime.handle(
        TaskCommand::Cancel,
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(runtime.task().state, TaskState::Cancelled);
    assert!(runtime
        .invocations()
        .iter()
        .all(|invocation| invocation.state == ToolState::Cancelled));
    assert!(calls.lock().unwrap().is_empty());
    let approval = futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-cancel".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ));
    assert!(approval.is_err());
}

#[test]
fn task_and_events_round_trip_without_losing_fixed_scope() {
    let executor = RecordingExecutor::default();
    let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("done"))]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("scope", "/project-a", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "hello".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    let task_json = serde_json::to_string(runtime.task()).unwrap();
    let restored: Task = serde_json::from_str(&task_json).unwrap();
    assert_eq!(restored.project_dir.to_string_lossy(), "/project-a");
    assert_eq!(restored.backend_id, "native");
    assert_eq!(restored.environment_id, "direct");
    let events_json = serde_json::to_string(runtime.events()).unwrap();
    let restored_events: Vec<termior_ai::TaskEvent> = serde_json::from_str(&events_json).unwrap();
    assert_eq!(restored_events, runtime.events());
    assert!(restored_events
        .iter()
        .all(|event| event.schema_version == 1));
}

#[test]
fn illegal_task_transition_returns_an_error_without_mutating_state() {
    let runtime = TaskRuntime::new(
        TaskConfig::new("state", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    let mut task = runtime.task().clone();
    let error = task.transition(TaskState::CompletedVerified).unwrap_err();
    assert!(error.to_string().contains("Idle"));
    assert_eq!(task.state, TaskState::Idle);
}

#[test]
fn acceptance_report_distinguishes_verified_unverified_and_failed() {
    let criterion = AcceptanceCriterion::command("tests", "tests pass", "cargo test");
    let mut report = AcceptanceReport::new(vec![criterion]);
    assert_eq!(report.completion_state(), TaskState::CompletedUnverified);
    report.checks.push(AcceptanceCheck {
        criterion_id: "tests".into(),
        status: AcceptanceStatus::Passed,
        command: Some("cargo test".into()),
        exit_code: Some(0),
        output_reference: Some("tool:check-1".into()),
        duration_ms: Some(25),
    });
    assert_eq!(report.completion_state(), TaskState::CompletedVerified);
    report.checks.push(AcceptanceCheck {
        criterion_id: "tests".into(),
        status: AcceptanceStatus::Failed,
        command: Some("cargo test".into()),
        exit_code: Some(1),
        output_reference: Some("tool:check-2".into()),
        duration_ms: Some(30),
    });
    assert_eq!(report.completion_state(), TaskState::Failed);
}

#[test]
fn explicit_acceptance_command_must_pass_before_verified_completion() {
    let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("implemented"))]);
    let config =
        TaskConfig::new("verify", "/project", "native", "direct").with_acceptance_criteria([
            AcceptanceCriterion::command("tests", "test suite passes", "cargo test"),
        ]);
    let mut runtime = TaskRuntime::new(config, RuntimeBudgets::default(), ApprovalPolicy::Prompt);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "implement".into(),
        },
        &provider,
        &PassingCommandExecutor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    let call_id = match runtime.task().waiting_reason.clone().unwrap() {
        WaitingReason::Approval { call_id } => call_id,
        reason => panic!("unexpected waiting reason: {reason:?}"),
    };
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id,
            approved: true,
        },
        &provider,
        &PassingCommandExecutor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::CompletedVerified);
    assert_eq!(runtime.task().acceptance_report.checks.len(), 1);
    let check = &runtime.task().acceptance_report.checks[0];
    assert_eq!(check.status, AcceptanceStatus::Passed);
    assert_eq!(check.exit_code, Some(0));
    assert!(check
        .output_reference
        .as_deref()
        .unwrap()
        .starts_with("tool:"));
}

#[test]
fn multiple_sequential_writes_share_one_reviewable_change_set() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "a\n").unwrap();
    std::fs::write(&b, "b\n").unwrap();
    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([dir
        .path()
        .display()
        .to_string()]));
    let executor = ToolExecutor::new(dir.path(), registry.clone()).unwrap();
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![
            ToolCall {
                id: "write-a".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"a.txt","content":"A\n"}"#.into(),
            },
            ToolCall {
                id: "write-b".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"b.txt","content":"B\n"}"#.into(),
            },
        ]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("two files", dir.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_tools(registry);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "edit both".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-a".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    assert_eq!(runtime.pending_approval().unwrap().call_id, "write-b");
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-b".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingChangeReview);
    let summary: termior_ai::EditProposalSummary =
        serde_json::from_str(runtime.pending_change_summary().unwrap()).unwrap();
    assert_eq!(summary.path, "2 files");
    assert_eq!(summary.hunk_ids, vec![0, 1]);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b\n");
    let WaitingReason::ChangeReview { change_set_id, .. } =
        runtime.task().waiting_reason.clone().unwrap()
    else {
        panic!()
    };
    futures::executor::block_on(runtime.handle(
        TaskCommand::ReviewChange {
            change_set_id,
            accepted_hunks: vec![0],
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(std::fs::read_to_string(a).unwrap(), "A\n");
    assert_eq!(std::fs::read_to_string(b).unwrap(), "b\n");
    assert!(runtime
        .invocations()
        .iter()
        .all(|invocation| invocation.state == ToolState::Succeeded));
}

#[test]
fn per_tool_output_limit_truncates_before_returning_content_to_the_model() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "search-huge".into(),
            name: "fs_search".into(),
            arguments: r#"{"query":"x"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("limit", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "search".into(),
        },
        &provider,
        &HugeOutputExecutor,
        &mut |_| {},
    ))
    .unwrap();
    let output = &runtime.invocations()[0].result.as_ref().unwrap().output;
    let limit = ToolRegistry::default()
        .contract("fs_search")
        .unwrap()
        .max_output_bytes as usize;
    assert!(output.len() <= limit);
    assert!(output.contains("tool output truncated"));
}

#[test]
fn large_tool_results_become_bounded_summaries_with_retrievable_references() {
    let dir = tempfile::tempdir().unwrap();
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "large-read".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("large output", dir.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_output_store(dir.path().join("artifacts"), 1024);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "read".into(),
        },
        &provider,
        &HugeOutputExecutor,
        &mut |_| {},
    ))
    .unwrap();
    let result = runtime
        .history()
        .iter()
        .find_map(|message| message.tool_result.as_ref())
        .unwrap();
    assert!(result.output.len() < 5000);
    assert!(result.output.contains("externalized"));
    let reference = result.content_ref.as_ref().unwrap();
    let store = termior_ai::context_engine::ContextContentStore::new(
        dir.path().join("artifacts"),
        Vec::<String>::new(),
    );
    assert!(store.resolve(reference).unwrap().len() >= 500_000);
}

#[test]
fn offline_evaluation_report_contains_required_reliability_metrics() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "read-eval".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let executor = RecordingExecutor::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("evaluation", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "read".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    let mut suite = EvaluationSuiteReport::new("m6-reliable-runtime");
    suite.push(ScenarioReport::capture("read-only", &runtime));
    let json = suite.to_json().unwrap();
    for field in [
        "completion_result",
        "verified",
        "tool_call_count",
        "duplicate_side_effect_count",
        "human_decision_count",
        "safety_refusal_count",
    ] {
        assert!(json.contains(field), "missing {field}: {json}");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = suite.persist(dir.path()).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), json);
}

#[test]
fn failed_acceptance_evidence_returns_to_the_agent_before_a_successful_retry() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(Message::assistant("first implementation"))],
        vec![ChatEvent::Done(Message::assistant(
            "fixed after test failure",
        ))],
    ]);
    let executor = FailThenPassCommandExecutor::default();
    let config =
        TaskConfig::new("repair", "/project", "native", "direct").with_acceptance_criteria([
            AcceptanceCriterion::command("tests", "tests pass", "cargo test"),
        ]);
    let mut runtime = TaskRuntime::new(config, RuntimeBudgets::default(), ApprovalPolicy::Prompt);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "implement".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    let first_call = runtime.pending_approval().unwrap().call_id;
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: first_call,
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    assert!(runtime.history().iter().any(|message| {
        message
            .tool_result
            .as_ref()
            .is_some_and(|result| !result.ok || result.output.contains("exit=1"))
    }));
    let second_call = runtime.pending_approval().unwrap().call_id;
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: second_call,
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::CompletedVerified);
    assert_eq!(
        runtime
            .task()
            .acceptance_report
            .checks
            .iter()
            .map(|check| check.status)
            .collect::<Vec<_>>(),
        vec![AcceptanceStatus::Failed, AcceptanceStatus::Passed]
    );
}

#[test]
fn change_review_conflict_keeps_user_disk_content_and_stays_pending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    std::fs::write(&path, "old\n").unwrap();
    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([dir
        .path()
        .display()
        .to_string()]));
    let executor = ToolExecutor::new(dir.path(), registry.clone()).unwrap();
    let provider = MockProvider::single(vec![ChatEvent::Done(assistant_with_calls(vec![
        ToolCall {
            id: "conflict-write".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"agent\n"}"#.into(),
        },
    ]))]);
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("conflict", dir.path(), "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    )
    .with_tools(registry);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "edit".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "conflict-write".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    std::fs::write(&path, "user\n").unwrap();
    let WaitingReason::ChangeReview { change_set_id, .. } =
        runtime.task().waiting_reason.clone().unwrap()
    else {
        panic!()
    };
    futures::executor::block_on(runtime.handle(
        TaskCommand::ReviewChange {
            change_set_id,
            accepted_hunks: vec![0],
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingChangeReview);
    assert!(matches!(
        runtime.task().waiting_reason,
        Some(WaitingReason::ChangeReview {
            conflict: Some(_),
            ..
        })
    ));
    assert_eq!(
        runtime.invocations()[0].state,
        ToolState::AwaitingChangeReview
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "user\n");
}

#[test]
fn cancellation_with_unconfirmed_external_state_finishes_unknown() {
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("cancel pending provider", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    let token = runtime.cancellation_token();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(10));
        token.cancel();
    });
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "wait".into(),
        },
        &PendingProvider,
        &UnconfirmedCancellationExecutor,
        &mut |_| {},
    ))
    .unwrap();
    canceller.join().unwrap();
    assert_eq!(runtime.task().state, TaskState::Unknown);
}

#[test]
fn randomized_tool_state_transitions_never_mutate_on_invalid_edges() {
    let mut seed = 0x5eed_u64;
    let states = [
        ToolState::Proposed,
        ToolState::AwaitingApproval,
        ToolState::Approved,
        ToolState::Running,
        ToolState::AwaitingChangeReview,
        ToolState::Succeeded,
        ToolState::Failed,
        ToolState::Denied,
        ToolState::Cancelled,
        ToolState::Unknown,
    ];
    for _ in 0..2_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let from = states[(seed as usize) % states.len()];
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let to = states[(seed as usize) % states.len()];
        let mut invocation = termior_ai::ToolInvocation {
            call_id: "random".into(),
            tool_name: "read_file".into(),
            raw_arguments: "{}".into(),
            normalized_arguments: Some("{}".into()),
            state: from,
            decision: None,
            proposed_at_ms: 0,
            state_changed_at_ms: 0,
            attempts: vec![],
            result: None,
            change_set_id: None,
            change_summary: None,
            acceptance_criterion_id: None,
            prepared: false,
        };
        let result = invocation.transition(to);
        if from.allows(to) {
            assert!(result.is_ok());
            assert_eq!(invocation.state, to);
        } else {
            assert!(result.is_err());
            assert_eq!(invocation.state, from);
        }
    }
}

#[test]
fn rejected_call_returns_a_tool_result_and_preserves_later_calls() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![
            ToolCall {
                id: "write-denied".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"a.txt","content":"x"}"#.into(),
            },
            ToolCall {
                id: "run-after-denial".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"test"}"#.into(),
            },
        ]))],
        vec![ChatEvent::Done(Message::assistant("handled"))],
    ]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("denial", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write and run".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-denied".into(),
            approved: false,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.invocations()[0].state, ToolState::Denied);
    assert_eq!(
        runtime.pending_approval().unwrap().call_id,
        "run-after-denial"
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "run-after-denial".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), vec!["run_command"]);
    assert!(runtime.history().iter().any(|message| {
        message
            .tool_result
            .as_ref()
            .is_some_and(|result| result.call_id == "write-denied" && !result.ok)
    }));
}

#[test]
fn policy_rejects_tools_outside_the_registered_subset_before_execution() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "disallowed-write".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"x"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("blocked"))],
    ]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let tools = ToolRegistry::default().subset(["read_file"]).unwrap();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("policy", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Yolo,
    )
    .with_tools(tools);
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.invocations()[0].state, ToolState::Denied);
    assert_eq!(
        runtime.invocations()[0].decision.as_ref().unwrap().source,
        termior_ai::DecisionSource::PolicyDeny
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn approval_display_redacts_secrets_but_execution_keeps_exact_arguments() {
    let secret = "sk-1234567890abcdef";
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "redacted-write".into(),
            name: "write_file".into(),
            arguments: format!(r#"{{"path":"a.txt","content":"{secret}"}}"#),
        }]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let executor = ArgumentRecordingExecutor::default();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("redaction", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "write".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    let approval = runtime.pending_approval().unwrap();
    assert!(!approval.arguments.contains(secret));
    assert!(approval.arguments.contains("<redacted>"));
    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: approval.call_id,
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert!(executor.0.lock().unwrap()[0].contains(secret));
}

impl RuntimeToolExecutor for RecordingExecutor {
    fn execute(
        &self,
        _call_id: &str,
        tool: &str,
        _normalized_arguments: &str,
        _cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        self.calls.lock().unwrap().push(tool.to_owned());
        Ok(ToolExecution::Completed(format!("{tool} ok")))
    }
}

fn assistant_with_calls(calls: Vec<ToolCall>) -> Message {
    Message {
        role: termior_ai::Role::Assistant,
        content: String::new(),
        tool_calls: calls,
        tool_result: None,
    }
}

#[test]
fn approval_resume_preserves_the_full_queue_and_executes_each_call_once() {
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![
            ToolCall {
                id: "read-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            },
            ToolCall {
                id: "write-1".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"a.txt","content":"updated"}"#.into(),
            },
            ToolCall {
                id: "command-1".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"test"}"#.into(),
            },
        ]))],
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);
    let executor = RecordingExecutor::default();
    let calls = executor.calls.clone();
    let mut runtime = TaskRuntime::new(
        TaskConfig::new("reliable queue", "/project", "native", "direct"),
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );

    futures::executor::block_on(runtime.handle(
        TaskCommand::Start {
            user_input: "update and test".into(),
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    assert_eq!(runtime.invocations().len(), 3);
    assert_eq!(runtime.invocations()[0].state, ToolState::Succeeded);
    assert_eq!(runtime.invocations()[1].state, ToolState::AwaitingApproval);
    assert_eq!(runtime.invocations()[2].state, ToolState::Proposed);
    assert_eq!(*calls.lock().unwrap(), vec!["read_file"]);

    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "write-1".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();
    assert_eq!(runtime.task().state, TaskState::WaitingApproval);
    assert_eq!(runtime.invocations()[1].state, ToolState::Succeeded);
    assert_eq!(runtime.invocations()[2].state, ToolState::AwaitingApproval);

    futures::executor::block_on(runtime.handle(
        TaskCommand::ResolveApproval {
            call_id: "command-1".into(),
            approved: true,
        },
        &provider,
        &executor,
        &mut |_| {},
    ))
    .unwrap();

    assert_eq!(runtime.task().state, TaskState::CompletedUnverified);
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["read_file", "write_file", "run_command"]
    );
}
