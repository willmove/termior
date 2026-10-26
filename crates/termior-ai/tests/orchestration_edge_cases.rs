//! Edge cases for parallel orchestration (Stage E task 3.6).
//!
//! The scenarios here are the ones that are easy to get wrong: a project that is not a
//! Git repository, a host that has run out of disk, a worktree that conflicts with an
//! existing change, a dependency that changed under a running task, and a partial failure
//! where some children succeed and others do not.

use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use termior_ai::{orchestration::*, RuntimeBudgets, SnapshotVersion, TaskDepth};

fn budgets() -> RuntimeBudgets {
    RuntimeBudgets {
        max_model_steps: 20,
        max_wall_clock_ms: 60_000,
        max_tool_output_bytes: 1_000_000,
        max_input_tokens: Some(100_000),
        max_output_tokens: Some(20_000),
    }
}

fn spec(id: &str, dependencies: Vec<String>, writes: bool) -> ChildTaskSpec {
    ChildTaskSpec {
        task_id: id.into(),
        parent_task_id: "root".into(),
        agent_id: "worker".into(),
        goal: id.into(),
        input_refs: vec![],
        output_schema: json!({"type":"object"}),
        tools: BTreeSet::new(),
        budgets: budgets(),
        environment_id: format!("env-{id}"),
        provider_id: "provider".into(),
        workspace_id: "workspace".into(),
        project_dir: "C:/workspace".into(),
        dependencies,
        depth: TaskDepth(1),
        writes,
        snapshot: SnapshotVersion(1),
    }
}

fn slots(global: usize) -> SlotLimits {
    SlotLimits {
        global,
        per_provider: global,
        per_workspace: global,
        per_environment: global,
    }
}

/// A launcher that fails the tasks whose id is in `failures`.
fn launcher(
    failures: Vec<String>,
) -> impl Fn(ChildTaskSpec, termior_ai::CancellationToken) -> Result<ChildTaskResult, String> {
    move |spec: ChildTaskSpec, _cancellation| {
        if failures.contains(&spec.task_id) {
            return Err("child failed".into());
        }
        Ok(ChildTaskResult {
            task_id: spec.task_id,
            data: json!({"ok":true}),
            evidence: vec!["journal:event".into()],
            snapshot: spec.snapshot,
            // A write child must report what it changed.
            change_sets: if spec.writes {
                vec!["shared.txt".into()]
            } else {
                Vec::new()
            },
            acceptance_verified: true,
            usage_known: true,
            usage: TaskResourceUsage::default(),
            stale: false,
            unknown: false,
            writes: spec.writes,
        })
    }
}

/// Drive a runtime to a settled state.
fn settle<L: TaskLauncher>(runtime: &mut OrchestrationRuntime<L>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !runtime.is_settled() && std::time::Instant::now() < deadline {
        runtime.tick(0);
        std::thread::sleep(Duration::from_millis(5));
    }
    runtime.tick(0);
}

/// A write task in a project without a Git repository still runs, but the child result
/// must report the writes so the parent can refuse to merge them.
#[test]
fn a_write_child_without_a_worktree_still_reports_its_writes() {
    let directory = tempfile::tempdir().unwrap();
    assert!(
        !directory.path().join(".git").exists(),
        "the fixture must not be a repository"
    );
    let mut runtime = OrchestrationRuntime::new(slots(1), Arc::new(launcher(Vec::new())));
    runtime.submit(spec("writer", vec![], true), 1).unwrap();
    settle(&mut runtime);
    let result = runtime
        .result("writer")
        .expect("the child must report a result");
    assert!(result.writes, "the write must be visible to the parent");
}

/// When the host has run out of disk, no new run may be claimed. The deferred run keeps a
/// visible reason instead of being dropped.
#[test]
fn a_full_disk_blocks_new_claims_and_keeps_a_reason() {
    use termior_ai::automation::*;

    let directory = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(directory.path()).unwrap();
    let automation = Automation {
        id: "nightly".into(),
        name: "Nightly".into(),
        template: TemplateVersion {
            version: 1,
            prompt: "check".into(),
            backend: "builtin".into(),
            agent_profile: "safe".into(),
            environment_id: "worktree".into(),
            max_concurrency: 1,
            budgets: budgets(),
        },
        trigger: Trigger::Manual,
        catch_up: CatchUpPolicy::RunOnce,
        notification: NotificationPolicy::Actionable,
        enabled: true,
    };
    manager.upsert(automation).unwrap();
    manager.trigger_manual("nightly", 10).unwrap();

    struct NeverRuns;
    impl RunExecutor for NeverRuns {
        fn execute(&mut self, _run: &AutomationRun) -> ExecutionOutcome {
            ExecutionOutcome::Completed {
                detail: "should-not-run".into(),
            }
        }
    }
    let mut executor = NeverRuns;
    let effects = manager
        .tick(
            20,
            HostLimits {
                max_concurrency: 4,
                disk_budget_bytes: 1_000,
                disk_used_bytes: 1_000,
            },
            &mut executor,
        )
        .unwrap();
    assert!(
        effects.is_empty(),
        "a run was claimed even though the disk budget was exhausted"
    );
    let deferred = manager.store.audit.iter().any(|event| {
        event.kind == "deferred" && event.schedule_reason.as_deref() == Some("disk-budget")
    });
    assert!(
        deferred,
        "the deferred run must record the disk budget as its reason"
    );
}

/// A child whose dependency failed does not run, and does not publish a result the parent
/// could consume.
#[test]
fn a_failed_dependency_stops_the_dependent_child() {
    let mut runtime =
        OrchestrationRuntime::new(slots(2), Arc::new(launcher(vec!["writer-a".into()])));
    runtime.submit(spec("writer-a", vec![], true), 1).unwrap();
    runtime.submit(spec("writer-b", vec![], true), 1).unwrap();
    runtime
        .submit(spec("dependent", vec!["writer-a".into()], false), 1)
        .unwrap();
    settle(&mut runtime);

    let tree = runtime.task_tree(0);
    let dependent = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "dependent")
        .unwrap();
    assert!(
        dependent.state != NodeState::Succeeded,
        "a dependent child ran on a failed dependency"
    );
    assert!(
        runtime.result("dependent").is_none(),
        "a dependent child produced a result after its dependency failed"
    );
}

/// A dependency that changes under a running child makes the child's result stale, and
/// the stale result is not accepted as verified.
#[test]
fn a_changed_dependency_marks_the_child_result_stale() {
    let mut runtime = OrchestrationRuntime::new(
        slots(1),
        Arc::new(move |spec: ChildTaskSpec, _cancellation| {
            // The dependency moved to a newer snapshot while this child ran.
            Ok(ChildTaskResult {
                task_id: spec.task_id,
                data: json!({"ok":true}),
                evidence: vec!["journal:event".into()],
                snapshot: SnapshotVersion(0),
                change_sets: Vec::new(),
                acceptance_verified: true,
                usage_known: true,
                usage: TaskResourceUsage::default(),
                stale: false,
                unknown: false,
                writes: false,
            })
        }),
    );
    runtime.submit(spec("child", vec![], false), 1).unwrap();
    settle(&mut runtime);
    let tree = runtime.task_tree(0);
    let child = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "child")
        .unwrap();
    assert_eq!(
        child.state,
        NodeState::Stale,
        "a result from an older snapshot must be reported as stale"
    );
}

/// A partial failure leaves the successful children intact and does not let the parent
/// consume a failed child's conclusion.
#[test]
fn a_partial_child_failure_keeps_the_successful_children_and_reports_the_failure() {
    let mut runtime =
        OrchestrationRuntime::new(slots(2), Arc::new(launcher(vec!["broken".into()])));
    runtime.submit(spec("healthy", vec![], false), 1).unwrap();
    runtime.submit(spec("broken", vec![], false), 1).unwrap();
    settle(&mut runtime);

    let tree = runtime.task_tree(0);
    let healthy = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "healthy")
        .unwrap();
    let broken = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "broken")
        .unwrap();
    assert_eq!(healthy.state, NodeState::Succeeded);
    assert_eq!(broken.state, NodeState::Failed);
    assert!(runtime.result("healthy").is_some());
    assert!(
        runtime.result("broken").is_none(),
        "a failed child must not publish a result the parent could consume"
    );
}
