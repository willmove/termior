//! Fault injection for every critical journal event (Stage D task 1.6).
//!
//! Each critical event is appended once with a simulated process exit before, during, and
//! after the write. Recovery is compared with the state implied by the last durable prefix.
//! One subprocess exits for real at each phase of the running-state boundary.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use termior_ai::{
    event_requires_durable_journal, AcceptanceCheck, AcceptanceCriterion, AcceptanceReport,
    AcceptanceStatus, ApprovalPolicy, DecisionSource, RuntimeBudgets, RuntimeUsage, Task,
    TaskEventKind, TaskId, TaskRuntime, TaskState, ToolInvocation, ToolRegistry, ToolState,
    WaitingReason, TASK_EVENT_SCHEMA_VERSION,
};
use termior_store::{
    arm_journal_fault, disarm_journal_fault, JournalFault, JournalFaultPhase, TaskJournal,
    JOURNAL_FAULT_EXIT_CODE,
};

const TASK_ID: &str = "fault-task";

#[derive(Serialize)]
struct Persisted<'a> {
    task: &'a Task,
    history: Vec<termior_ai::Message>,
    invocations: &'a [ToolInvocation],
}

struct ToolView {
    call_id: String,
    state: ToolState,
    decision: Option<(bool, DecisionSource)>,
}

struct View {
    state: TaskState,
    waiting: Option<WaitingReason>,
    tools: Vec<ToolView>,
    acceptance: AcceptanceReport,
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        disarm_journal_fault();
    }
}

#[test]
fn non_critical_events_are_not_durable_boundaries() {
    let kinds = [
        TaskEventKind::MessageAdded {
            message: termior_ai::Message::user("hi"),
        },
        TaskEventKind::BudgetUpdated {
            usage: RuntimeUsage::default(),
        },
        TaskEventKind::ContextCompacted {
            covered_start: 1,
            covered_end: 2,
            version: 1,
        },
        TaskEventKind::Diagnostic {
            message: "note".into(),
        },
        TaskEventKind::ToolStateChanged {
            call_id: "call-1".into(),
            from: ToolState::Proposed,
            to: ToolState::AwaitingApproval,
        },
        TaskEventKind::ToolStateChanged {
            call_id: "call-1".into(),
            from: ToolState::AwaitingApproval,
            to: ToolState::Approved,
        },
    ];
    for kind in &kinds {
        assert!(
            !event_requires_durable_journal(kind),
            "expected a non-critical event, got {kind:?}"
        );
    }
}

#[test]
fn every_critical_journal_event_recovers_after_in_process_faults() {
    for (label, kind) in critical_events() {
        assert!(
            event_requires_durable_journal(&kind),
            "{label} must be a durable journal boundary"
        );
        for phase in [
            JournalFaultPhase::BeforeWrite,
            JournalFaultPhase::DuringWrite,
            JournalFaultPhase::AfterWrite,
        ] {
            let dir = tempfile::tempdir().unwrap();
            inject_and_assert(dir.path(), &kind, phase, false);
            let applied = (phase == JournalFaultPhase::AfterWrite).then_some(kind.clone());
            assert_recovered(dir.path(), applied.as_ref(), phase, label);
        }
    }
}

#[test]
fn subprocess_journal_exit_recovers_running_state() {
    if let Ok(phase) = std::env::var("TERMIOR_JOURNAL_FAULT_CHILD") {
        let dir = PathBuf::from(std::env::var("TERMIOR_JOURNAL_FAULT_DIR").unwrap());
        let phase = match phase.as_str() {
            "before" => JournalFaultPhase::BeforeWrite,
            "during" => JournalFaultPhase::DuringWrite,
            "after" => JournalFaultPhase::AfterWrite,
            other => panic!("unknown journal fault phase {other}"),
        };
        let kind = running_state_event();
        inject_and_assert(&dir, &kind, phase, true);
        panic!("journal fault did not exit the process");
    }

    let kind = running_state_event();
    for (label, phase) in [
        ("before", JournalFaultPhase::BeforeWrite),
        ("during", JournalFaultPhase::DuringWrite),
        ("after", JournalFaultPhase::AfterWrite),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("subprocess_journal_exit_recovers_running_state")
            .arg("--exact")
            .env("TERMIOR_JOURNAL_FAULT_CHILD", label)
            .env("TERMIOR_JOURNAL_FAULT_DIR", dir.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(JOURNAL_FAULT_EXIT_CODE),
            "phase {label} stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_recovered(
            dir.path(),
            (phase == JournalFaultPhase::AfterWrite).then_some(&kind),
            phase,
            "subprocess running",
        );
    }
}

fn inject_and_assert(
    dir: &Path,
    kind: &TaskEventKind,
    phase: JournalFaultPhase,
    exit_process: bool,
) {
    let mut journal = prepare(dir);
    arm_journal_fault(JournalFault {
        sequence: 2,
        phase,
        exit_process,
    });
    let _guard = Guard;
    let payload = serde_json::to_value(kind).unwrap();
    let kind_name = payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap()
        .to_owned();
    let error = journal
        .append(&kind_name, payload, event_requires_durable_journal(kind))
        .unwrap_err();
    if !exit_process {
        let label = match phase {
            JournalFaultPhase::BeforeWrite => "before",
            JournalFaultPhase::DuringWrite => "during",
            JournalFaultPhase::AfterWrite => "after",
        };
        assert_eq!(
            error.to_string(),
            format!("simulated process exit {label} writing journal sequence 2")
        );
    }
    drop(journal);
}

fn assert_recovered(
    data_root: &Path,
    applied: Option<&TaskEventKind>,
    phase: JournalFaultPhase,
    label: &str,
) {
    let tasks = data_root.join("agent-tasks");
    let recovery = TaskJournal::recover(&tasks, TASK_ID, &[]).unwrap();
    assert_eq!(
        recovery.repaired_tail,
        phase == JournalFaultPhase::DuringWrite,
        "{label} repaired tail"
    );
    let event_count = 1 + usize::from(applied.is_some());
    assert_eq!(recovery.events.len(), event_count, "{label} event count");
    assert!(
        recovery.events.iter().all(|event| event.critical),
        "{label}"
    );
    if let Some(kind) = applied {
        let payload: TaskEventKind =
            serde_json::from_value(recovery.events[1].payload.clone()).unwrap();
        assert_eq!(&payload, kind, "{label} replayed payload");
    }

    let recovered = TaskRuntime::recover_journal(
        data_root,
        TASK_ID,
        ApprovalPolicy::Prompt,
        ToolRegistry::default(),
    )
    .unwrap_or_else(|error| panic!("{label} recover failed: {error}"));
    let expected = expected_view(applied);
    assert_eq!(recovered.task().state, expected.state, "{label} task state");
    assert_eq!(
        recovered.task().waiting_reason,
        expected.waiting,
        "{label} waiting reason"
    );
    assert_eq!(
        recovered.task().acceptance_report,
        expected.acceptance,
        "{label} acceptance"
    );
    assert_eq!(
        recovered.events().len(),
        event_count,
        "{label} runtime events"
    );
    assert_eq!(
        recovered.invocations().len(),
        expected.tools.len(),
        "{label} invocations"
    );
    for (got, expected_tool) in recovered.invocations().iter().zip(&expected.tools) {
        assert_eq!(got.call_id, expected_tool.call_id, "{label}");
        assert_eq!(got.state, expected_tool.state, "{label} tool state");
        assert_eq!(
            got.decision
                .as_ref()
                .map(|decision| (decision.approved, decision.source)),
            expected_tool.decision,
            "{label} decision"
        );
    }
}

fn prepare(data_root: &Path) -> TaskJournal {
    let mut journal = TaskJournal::create(&data_root.join("agent-tasks"), TASK_ID).unwrap();
    let first = TaskEventKind::StateChanged {
        from: TaskState::Idle,
        to: TaskState::WaitingApproval,
    };
    journal
        .append("state_changed", serde_json::to_value(&first).unwrap(), true)
        .unwrap();
    let mut task = sample_task();
    task.state = TaskState::WaitingApproval;
    let invocations = vec![sample_invocation()];
    journal
        .write_snapshot(
            serde_json::to_value(Persisted {
                task: &task,
                history: Vec::new(),
                invocations: &invocations,
            })
            .unwrap(),
        )
        .unwrap();
    journal
}

fn expected_view(applied: Option<&TaskEventKind>) -> View {
    let mut view = View {
        state: TaskState::WaitingApproval,
        waiting: None,
        tools: vec![ToolView {
            call_id: "call-1".into(),
            state: ToolState::Approved,
            decision: None,
        }],
        acceptance: AcceptanceReport::new(Vec::new()),
    };
    let Some(kind) = applied else {
        return view;
    };
    match kind {
        TaskEventKind::StateChanged { to, .. } => view.state = *to,
        TaskEventKind::Waiting { reason } => view.waiting = Some(reason.clone()),
        TaskEventKind::ToolQueued { call_id, .. } => view.tools.push(ToolView {
            call_id: call_id.clone(),
            state: ToolState::Proposed,
            decision: None,
        }),
        TaskEventKind::ToolDecisionRecorded {
            call_id,
            approved,
            source,
        } => {
            if let Some(tool) = view.tools.iter_mut().find(|tool| &tool.call_id == call_id) {
                tool.decision = Some((*approved, *source));
            }
        }
        TaskEventKind::ToolStateChanged { call_id, to, .. } => {
            if let Some(tool) = view.tools.iter_mut().find(|tool| &tool.call_id == call_id) {
                tool.state = *to;
            }
        }
        TaskEventKind::AcceptanceUpdated { report } => view.acceptance = report.clone(),
        TaskEventKind::MessageAdded { .. }
        | TaskEventKind::BudgetUpdated { .. }
        | TaskEventKind::ContextCompacted { .. }
        | TaskEventKind::Diagnostic { .. } => {}
    }
    if matches!(view.state, TaskState::Running | TaskState::Cancelling) {
        view.state = TaskState::Unknown;
        view.waiting = None;
    }
    for tool in &mut view.tools {
        if tool.state == ToolState::Running {
            tool.state = ToolState::Unknown;
        }
    }
    view
}

fn critical_events() -> Vec<(&'static str, TaskEventKind)> {
    let mut events = vec![
        ("state-running", running_state_event()),
        (
            "state-cancelling",
            TaskEventKind::StateChanged {
                from: TaskState::Running,
                to: TaskState::Cancelling,
            },
        ),
        (
            "state-failed",
            TaskEventKind::StateChanged {
                from: TaskState::Running,
                to: TaskState::Failed,
            },
        ),
        (
            "state-completed",
            TaskEventKind::StateChanged {
                from: TaskState::Running,
                to: TaskState::CompletedVerified,
            },
        ),
        (
            "state-waiting-approval",
            TaskEventKind::StateChanged {
                from: TaskState::Running,
                to: TaskState::WaitingApproval,
            },
        ),
        (
            "waiting-approval",
            TaskEventKind::Waiting {
                reason: WaitingReason::Approval {
                    call_id: "call-1".into(),
                },
            },
        ),
        (
            "tool-queued",
            TaskEventKind::ToolQueued {
                call_id: "call-2".into(),
                tool_name: "read_file".into(),
                raw_arguments: "{}".into(),
                normalized_arguments: Some("{}".into()),
            },
        ),
        (
            "tool-decision",
            TaskEventKind::ToolDecisionRecorded {
                call_id: "call-1".into(),
                approved: true,
                source: DecisionSource::User,
            },
        ),
        (
            "acceptance",
            TaskEventKind::AcceptanceUpdated {
                report: passed_report(),
            },
        ),
    ];
    for (label, state) in [
        ("tool-running", ToolState::Running),
        ("tool-succeeded", ToolState::Succeeded),
        ("tool-failed", ToolState::Failed),
        ("tool-denied", ToolState::Denied),
        ("tool-cancelled", ToolState::Cancelled),
        ("tool-unknown", ToolState::Unknown),
        ("tool-review", ToolState::AwaitingChangeReview),
    ] {
        events.push((
            label,
            TaskEventKind::ToolStateChanged {
                call_id: "call-1".into(),
                from: ToolState::Approved,
                to: state,
            },
        ));
    }
    events
}

fn running_state_event() -> TaskEventKind {
    TaskEventKind::StateChanged {
        from: TaskState::WaitingApproval,
        to: TaskState::Running,
    }
}

fn passed_report() -> AcceptanceReport {
    let mut report = AcceptanceReport::new(vec![AcceptanceCriterion::command(
        "build",
        "builds",
        "cargo test",
    )]);
    report.checks.push(AcceptanceCheck {
        criterion_id: "build".into(),
        status: AcceptanceStatus::Passed,
        command: Some("cargo test".into()),
        exit_code: Some(0),
        output_reference: None,
        duration_ms: Some(10),
    });
    report
}

fn sample_task() -> Task {
    Task {
        schema_version: TASK_EVENT_SCHEMA_VERSION,
        id: TaskId::from(TASK_ID),
        title: "fault".into(),
        goal: "fault".into(),
        project_dir: PathBuf::from("."),
        backend_id: "native".into(),
        environment_id: "direct".into(),
        created_at_ms: 1,
        state: TaskState::Idle,
        waiting_reason: None,
        budgets: RuntimeBudgets::default(),
        usage: RuntimeUsage::default(),
        acceptance_report: AcceptanceReport::new(Vec::new()),
        turns: Vec::new(),
    }
}

fn sample_invocation() -> ToolInvocation {
    ToolInvocation {
        call_id: "call-1".into(),
        tool_name: "read_file".into(),
        raw_arguments: "{}".into(),
        normalized_arguments: None,
        state: ToolState::Approved,
        decision: None,
        proposed_at_ms: 1,
        state_changed_at_ms: 1,
        attempts: Vec::new(),
        result: None,
        change_set_id: None,
        change_summary: None,
        acceptance_criterion_id: None,
        prepared: false,
    }
}
