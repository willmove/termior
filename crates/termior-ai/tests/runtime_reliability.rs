use std::sync::{Arc, Mutex};

use termior_ai::{
    AcceptanceCheck, AcceptanceCriterion, AcceptanceReport, AcceptanceStatus, ApprovalPolicy,
    CancellationToken, ChatEvent, Message, MockProvider, RuntimeBudgets, RuntimeToolExecutor, Task,
    TaskCommand, TaskConfig, TaskRuntime, TaskState, ToolCall, ToolExecution, ToolExecutor,
    ToolRegistry, ToolState, WaitingReason,
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
        assert!(contract.parameters["properties"].is_object(), "{}", contract.name);
        assert!(contract.default_timeout_ms > 0, "{}", contract.name);
        assert!(contract.max_output_bytes > 0, "{}", contract.name);
    }
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
    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([
        dir.path().display().to_string(),
    ]));
    let executor = ToolExecutor::new(dir.path(), registry.clone()).unwrap();
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(assistant_with_calls(vec![ToolCall {
            id: "write-real".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"a.txt","content":"new\n"}"#.into(),
        }]))],
        vec![ChatEvent::Done(Message::assistant("verified by inspection"))],
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
    assert!(runtime
        .events()
        .iter()
        .filter(|event| matches!(event.kind, termior_ai::TaskEventKind::Diagnostic { .. }))
        .count()
        >= 2);
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
    let mut budgets = RuntimeBudgets::default();
    budgets.max_model_steps = 1;
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
    let restored_events: Vec<termior_ai::TaskEvent> =
        serde_json::from_str(&events_json).unwrap();
    assert_eq!(restored_events, runtime.events());
    assert!(restored_events.iter().all(|event| event.schema_version == 1));
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
    let config = TaskConfig::new("verify", "/project", "native", "direct")
        .with_acceptance_criteria([AcceptanceCriterion::command(
            "tests",
            "test suite passes",
            "cargo test",
        )]);
    let mut runtime = TaskRuntime::new(
        config,
        RuntimeBudgets::default(),
        ApprovalPolicy::Prompt,
    );
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
    assert!(check.output_reference.as_deref().unwrap().starts_with("tool:"));
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
