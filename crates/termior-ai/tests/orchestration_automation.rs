use chrono::TimeZone;
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use termior_ai::{
    automation::*, evaluation::OrchestrationMetrics, orchestration::*, RuntimeBudgets,
};

fn budgets() -> RuntimeBudgets {
    RuntimeBudgets {
        max_model_steps: 20,
        max_wall_clock_ms: 60_000,
        max_tool_output_bytes: 1_000_000,
        max_input_tokens: Some(100_000),
        max_output_tokens: Some(20_000),
    }
}

#[test]
fn child_permissions_budgets_and_depth_can_only_narrow() {
    let parent_tools = BTreeSet::from(["read_file".into(), "write_file".into()]);
    let parent = budgets();
    let mut child = ChildTaskSpec {
        task_id: "c".into(),
        parent_task_id: "p".into(),
        agent_id: "reviewer".into(),
        goal: "research".into(),
        input_refs: vec![],
        output_schema: json!({"type":"object"}),
        tools: BTreeSet::from(["read_file".into()]),
        budgets: parent.clone(),
        environment_id: "direct".into(),
        provider_id: "p".into(),
        workspace_id: "w".into(),
        project_dir: "C:/workspace".into(),
        dependencies: vec![],
        depth: TaskDepth(1),
        writes: false,
        snapshot: SnapshotVersion(1),
    };
    assert!(child
        .validate_against_parent(&parent_tools, &parent, 2)
        .is_ok());
    child.tools.insert("run_command".into());
    assert_eq!(
        child
            .validate_against_parent(&parent_tools, &parent, 2)
            .unwrap_err(),
        OrchestrationError::PermissionExpansion
    );
    child.tools.remove("run_command");
    child.depth = TaskDepth(3);
    assert!(matches!(
        child.validate_against_parent(&parent_tools, &parent, 2),
        Err(OrchestrationError::DepthExceeded { .. })
    ));
}

#[test]
fn approved_subagent_plan_step_maps_to_a_normal_child_task_spec() {
    let mut plan = termior_ai::Plan::default();
    plan.add_step(termior_ai::PlanStep {
        id: "delegate".into(),
        title: "Review".into(),
        files: vec![],
        scope: "read-only".into(),
        kind: termior_ai::PlanStepKind::Subagent {
            agent_id: "reviewer".into(),
            task: "review the patch".into(),
        },
        status: termior_ai::PlanStepStatus::Proposed,
    });
    assert!(matches!(
        ChildTaskSpec::from_plan_step(
            &plan.steps[0],
            "child",
            ChildTaskLaunchContext {
                parent_task_id: "parent".into(),
                tools: BTreeSet::from(["read_file".into()]),
                budgets: budgets(),
                environment_id: "direct".into(),
                provider_id: "provider".into(),
                workspace_id: "workspace".into(),
                project_dir: "C:/workspace".into(),
                depth: TaskDepth(1),
                snapshot: SnapshotVersion(1),
            },
        ),
        Err(OrchestrationError::StepNotApproved(_))
    ));
    plan.confirm();
    let child = ChildTaskSpec::from_plan_step(
        &plan.steps[0],
        "child",
        ChildTaskLaunchContext {
            parent_task_id: "parent".into(),
            tools: BTreeSet::from(["read_file".into()]),
            budgets: budgets(),
            environment_id: "direct".into(),
            provider_id: "provider".into(),
            workspace_id: "workspace".into(),
            project_dir: "C:/workspace".into(),
            depth: TaskDepth(1),
            snapshot: SnapshotVersion(1),
        },
    )
    .unwrap();
    assert_eq!(child.agent_id, "reviewer");
    assert_eq!(child.goal, "review the patch");
}

#[test]
fn dag_rejects_cycles_and_blocks_dependents_after_failure() {
    let mut graph = TaskGraph::default();
    graph.add_node("research");
    graph.add_node("implement");
    graph
        .add_dependency("research", "implement", DependencyCondition::Succeeded)
        .unwrap();
    assert_eq!(
        graph
            .add_dependency("implement", "research", DependencyCondition::Succeeded)
            .unwrap_err(),
        OrchestrationError::Cycle
    );
    graph.nodes.insert("research".into(), NodeState::Failed);
    graph.refresh();
    assert_eq!(graph.nodes["implement"], NodeState::Blocked);
}

#[test]
fn parent_cancellation_preserves_unconfirmed_running_child_as_unknown() {
    let mut graph = TaskGraph::default();
    graph.add_node("root");
    graph.add_node("child");
    graph
        .add_dependency("root", "child", DependencyCondition::Terminal)
        .unwrap();
    graph.nodes.insert("child".into(), NodeState::Running);
    graph.cancel_descendants("root", &BTreeSet::new());
    assert_eq!(graph.nodes["child"], NodeState::Unknown);
}

#[test]
fn centralized_scheduler_exposes_slot_reason_and_fairly_releases() {
    let mut scheduler = Scheduler::new(SlotLimits {
        global: 2,
        per_provider: 1,
        per_workspace: 2,
        per_environment: 1,
    });
    let task = |id: &str, provider: &str, seq| ScheduledTask {
        id: id.into(),
        provider: provider.into(),
        workspace: "w".into(),
        environment: id.into(),
        priority: 1,
        sequence: seq,
    };
    scheduler.enqueue(task("a", "provider", 1));
    scheduler.enqueue(task("b", "provider", 2));
    assert_eq!(scheduler.next_ready(0).unwrap().id, "a");
    assert_eq!(
        scheduler.reason(&task("b", "provider", 2), 0),
        Some(QueueReason::ProviderSlot)
    );
    scheduler.finish("a");
    assert_eq!(scheduler.next_ready(0).unwrap().id, "b");
}

#[test]
fn scheduler_snapshot_and_provider_backoff_survive_restart() {
    let mut scheduler = Scheduler::new(SlotLimits {
        global: 1,
        per_provider: 1,
        per_workspace: 1,
        per_environment: 1,
    });
    let task = ScheduledTask {
        id: "a".into(),
        provider: "p".into(),
        workspace: "w".into(),
        environment: "e".into(),
        priority: 1,
        sequence: 1,
    };
    scheduler.enqueue(task.clone());
    scheduler.set_provider_backoff("p", 100);
    let mut restored = Scheduler::restore(scheduler.snapshot());
    assert_eq!(
        restored.reason(&task, 50),
        Some(QueueReason::ProviderBackoff)
    );
    assert!(restored.next_ready(50).is_none());
    assert_eq!(restored.next_ready(100).unwrap().id, "a");
}

#[test]
fn aggregate_budget_reservation_prevents_spawn_based_bypass() {
    let mut budget = AggregateBudget {
        token_limit: Some(100),
        time_limit_ms: 1000,
        output_limit_bytes: 1000,
        reserved_tokens: 0,
        reserved_time_ms: 0,
        reserved_output_bytes: 0,
    };
    budget.reserve(Some(60), 400, 400).unwrap();
    assert_eq!(
        budget.reserve(Some(60), 100, 100).unwrap_err(),
        OrchestrationError::BudgetExpansion
    );
}

#[test]
fn write_results_require_evidence_freshness_changes_and_acceptance() {
    let mut result = ChildTaskResult {
        task_id: "c".into(),
        data: json!({}),
        evidence: vec!["event".into()],
        snapshot: SnapshotVersion(1),
        change_sets: vec![],
        acceptance_verified: false,
        usage_known: true,
        usage: TaskResourceUsage::default(),
        stale: false,
        unknown: false,
        writes: true,
    };
    assert_eq!(
        result.validate(SnapshotVersion(1)).unwrap_err(),
        OrchestrationError::UnverifiedWrite
    );
    result.change_sets.push("change".into());
    result.acceptance_verified = true;
    assert!(result.validate(SnapshotVersion(1)).is_ok());
    assert_eq!(
        result.validate(SnapshotVersion(2)).unwrap_err(),
        OrchestrationError::StaleResult
    );
}

#[test]
fn child_results_must_match_the_declared_output_schema() {
    let spec = ChildTaskSpec {
        task_id: "schema-child".into(),
        parent_task_id: "root".into(),
        agent_id: "researcher".into(),
        goal: "answer with evidence".into(),
        input_refs: vec![],
        output_schema: json!({
            "type":"object",
            "required":["answer","evidence"],
            "properties":{
                "answer":{"type":"string"},
                "evidence":{"type":"array","items":{"type":"string"}}
            },
            "additionalProperties":false
        }),
        tools: BTreeSet::new(),
        budgets: budgets(),
        environment_id: "direct".into(),
        provider_id: "provider".into(),
        workspace_id: "workspace".into(),
        project_dir: "C:/workspace".into(),
        dependencies: vec![],
        depth: TaskDepth(1),
        writes: false,
        snapshot: SnapshotVersion(1),
    };
    let mut result = ChildTaskResult {
        task_id: spec.task_id.clone(),
        data: json!({"answer":"done","evidence":["event:1"]}),
        evidence: vec!["event:1".into()],
        snapshot: SnapshotVersion(1),
        change_sets: vec![],
        acceptance_verified: true,
        usage_known: true,
        usage: TaskResourceUsage::default(),
        stale: false,
        unknown: false,
        writes: false,
    };
    assert!(result.validate_for(&spec).is_ok());
    result.data = json!({"answer":7,"evidence":[]});
    assert!(matches!(
        result.validate_for(&spec),
        Err(OrchestrationError::OutputSchemaViolation(_))
    ));
}

fn automation(policy: CatchUpPolicy) -> Automation {
    Automation {
        id: "daily".into(),
        name: "Daily".into(),
        template: TemplateVersion {
            version: 3,
            prompt: "check".into(),
            backend: "builtin".into(),
            agent_profile: "safe".into(),
            environment_id: "worktree".into(),
            max_concurrency: 1,
            budgets: budgets(),
        },
        trigger: Trigger::Interval {
            every_seconds: 10,
            anchor_unix: 0,
        },
        catch_up: policy,
        notification: NotificationPolicy::Actionable,
        enabled: true,
    }
}

#[test]
fn automation_runs_are_independent_versioned_and_deduplicated() {
    let mut engine = AutomationEngine::default();
    let auto = automation(CatchUpPolicy::RunOnce);
    let first = engine.trigger(&auto, 10, "event-1".into(), 10);
    let duplicate = engine.trigger(&auto, 10, "event-1".into(), 11);
    assert_ne!(first.task_id, duplicate.task_id);
    assert_eq!(first.template_version, 3);
    assert_eq!(duplicate.status, RunStatus::Duplicate);
    assert_eq!(engine.catch_up_times(&auto, 1, 55), vec![50]);
}

#[test]
fn retries_wait_for_user_on_unknown_or_non_confirmed_effects() {
    assert_eq!(
        classify_retry(FailureClass::Transient, true, true),
        RetryDecision::Retry
    );
    assert_eq!(
        classify_retry(FailureClass::UnknownEffect, true, false),
        RetryDecision::WaitForUser
    );
    assert!(!should_notify(
        RunStatus::Running,
        NotificationPolicy::Actionable
    ));
    assert!(should_notify(
        RunStatus::WaitingUser,
        NotificationPolicy::Actionable
    ));
}

#[test]
fn multi_task_evaluation_does_not_treat_more_children_as_quality() {
    let baseline = OrchestrationMetrics {
        task_count: 1,
        completed_count: 1,
        verified_count: 1,
        total_tokens: Some(1_000),
        total_cost_usd: Some(0.1),
        wall_clock_ms: 100,
        conflict_count: 0,
        stale_result_count: 0,
        duplicate_side_effect_count: 0,
        human_intervention_count: 0,
        unknown_recovery_count: 0,
    };
    let candidate = OrchestrationMetrics {
        task_count: 10,
        completed_count: 10,
        verified_count: 1,
        total_tokens: Some(10_000),
        total_cost_usd: Some(1.0),
        wall_clock_ms: 120,
        conflict_count: 1,
        stale_result_count: 0,
        duplicate_side_effect_count: 0,
        human_intervention_count: 0,
        unknown_recovery_count: 0,
    };
    assert!(!candidate.improves_on(&baseline));
}

#[test]
fn automation_store_round_trips_without_changing_active_template_versions() {
    let dir = tempfile::tempdir().unwrap();
    let auto = automation(CatchUpPolicy::Skip);
    let run = AutomationEngine::default().trigger(&auto, 10, "manual".into(), 10);
    let store = AutomationStore {
        schema_version: 1,
        automations: vec![auto],
        recent_runs: vec![run.clone()],
        audit: vec![AutomationAuditEvent {
            timestamp: 10,
            automation_id: "daily".into(),
            run_id: Some(run.run_id),
            kind: "trigger".into(),
            detail: "manual".into(),
            trigger: Some("manual".into()),
            template_version: Some(3),
            dedupe_key: Some("manual".into()),
            schedule_reason: Some("manual".into()),
            retry_decision: None,
            approval: None,
            merge: None,
        }],
        engine: AutomationEngine::default(),
    };
    store.persist(dir.path()).unwrap();
    let loaded = AutomationStore::load(dir.path()).unwrap();
    assert_eq!(loaded.recent_runs[0].template_version, 3);
    assert_eq!(loaded.audit[0].kind, "trigger");
    assert_eq!(
        AutomationEngine::repo_event_key("a", "repo", "commit"),
        "repo:a:repo:commit"
    );
}

#[test]
fn automation_manager_persists_template_versions_dedupe_and_actionable_status() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    let mut auto = automation(CatchUpPolicy::RunOnce);
    manager.upsert(auto.clone()).unwrap();
    assert_eq!(manager.next_run_at("daily", 10), Some(20));
    let first = manager.trigger_manual("daily", 10).unwrap();
    assert_eq!(first.template_version, 3);
    assert!(!manager
        .update_run_status(&first.run_id, RunStatus::Running, 11, "started")
        .unwrap());
    assert!(manager
        .update_run_status(&first.run_id, RunStatus::Completed, 12, "verified")
        .unwrap());

    auto.template.prompt = "updated".into();
    manager.upsert(auto).unwrap();
    assert_eq!(manager.store.automations[0].template.version, 4);
    let second = manager
        .trigger_repo_event("daily", "repo", "commit-1", 20)
        .unwrap();
    let duplicate = manager
        .trigger_repo_event("daily", "repo", "commit-1", 21)
        .unwrap();
    assert_eq!(second.template_version, 4);
    assert_eq!(duplicate.status, RunStatus::Duplicate);

    let loaded = AutomationManager::load(dir.path()).unwrap();
    assert_eq!(loaded.store.automations[0].template.version, 4);
    assert_eq!(loaded.store.recent_runs.len(), 3);
    assert!(loaded
        .store
        .audit
        .iter()
        .any(|event| event.kind == "status"));
}

#[test]
fn local_schedule_handles_dst_gaps_and_overlaps_without_duplicates() {
    let mut auto = automation(CatchUpPolicy::RunEach { maximum: 10 });
    auto.trigger = Trigger::LocalSchedule {
        hour: 2,
        minute: 30,
        timezone: "America/New_York".into(),
    };
    let utc = chrono::Utc;
    let from = utc
        .with_ymd_and_hms(2024, 3, 9, 0, 0, 0)
        .unwrap()
        .timestamp() as u64;
    let to = utc
        .with_ymd_and_hms(2024, 3, 12, 0, 0, 0)
        .unwrap()
        .timestamp() as u64;
    // March 10 02:30 does not exist because of the spring-forward gap.
    assert_eq!(
        AutomationEngine::default()
            .catch_up_times(&auto, from, to)
            .len(),
        2
    );

    auto.trigger = Trigger::LocalSchedule {
        hour: 1,
        minute: 30,
        timezone: "America/New_York".into(),
    };
    let from = utc
        .with_ymd_and_hms(2024, 11, 3, 0, 0, 0)
        .unwrap()
        .timestamp() as u64;
    let to = utc
        .with_ymd_and_hms(2024, 11, 4, 0, 0, 0)
        .unwrap()
        .timestamp() as u64;
    // The repeated local time produces one run, at the first occurrence.
    assert_eq!(
        AutomationEngine::default()
            .catch_up_times(&auto, from, to)
            .len(),
        1
    );
}

#[test]
fn repo_event_storm_is_deduplicated_and_coalesced() {
    let mut auto = automation(CatchUpPolicy::Skip);
    auto.trigger = Trigger::RepoEvent {
        event: "change".into(),
        coalesce_seconds: 30,
    };
    let mut engine = AutomationEngine::default();
    let first = engine.trigger_repo_event(&auto, "repo", "one", 100);
    let duplicate = engine.trigger_repo_event(&auto, "repo", "one", 101);
    let coalesced = engine.trigger_repo_event(&auto, "repo", "two", 102);
    assert_eq!(first.status, RunStatus::Queued);
    assert_eq!(duplicate.status, RunStatus::Duplicate);
    assert_eq!(coalesced.status, RunStatus::Coalesced);
}

/// The dedupe map is a window, not a history.
///
/// A host that runs an automation on a schedule would otherwise keep one entry per
/// occurrence forever, and the automation store — which is rewritten on every run —
/// would grow without bound.
#[test]
fn the_dedupe_map_is_bounded() {
    let auto = automation(CatchUpPolicy::Skip);
    let mut engine = AutomationEngine::default();
    for occurrence in 0..5_000u64 {
        engine.trigger(
            &auto,
            occurrence,
            format!("occurrence-{occurrence}"),
            occurrence,
        );
    }
    assert!(
        engine.dedupe_entries() <= 2_000,
        "the dedupe map grew to {} entries",
        engine.dedupe_entries()
    );
}

/// A key that falls out of the window is forgotten, so it is no longer a duplicate.
#[test]
fn a_dedupe_key_outside_the_window_is_forgotten() {
    let auto = automation(CatchUpPolicy::Skip);
    let mut engine = AutomationEngine::default();
    let first = engine.trigger(&auto, 0, "same".into(), 0);
    assert_eq!(first.status, RunStatus::Queued);
    let inside = engine.trigger(&auto, 1, "same".into(), 1);
    assert_eq!(inside.status, RunStatus::Duplicate);
    // An hour later the window has moved past the key, which was last seen at 1.
    let outside = engine.trigger(&auto, 2, "same".into(), 3_600_001);
    assert_eq!(outside.status, RunStatus::Queued);
}

/// The automation store is bounded: the finished run rows fall out of the recent
/// window instead of accumulating.
#[test]
fn the_automation_store_keeps_only_the_recent_finished_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    manager.upsert(automation(CatchUpPolicy::Skip)).unwrap();
    for cycle in 0..600u64 {
        let run = manager.trigger_manual("daily", cycle).unwrap();
        manager
            .complete_run(
                &run.run_id,
                ExecutionOutcome::Completed {
                    detail: "soak".into(),
                },
                cycle,
            )
            .unwrap();
    }
    assert!(
        manager.store.recent_runs.len() <= 500,
        "the run rows grew to {}",
        manager.store.recent_runs.len()
    );
    // The store that is written to disk stays bounded too.
    let bytes = std::fs::metadata(dir.path().join("automations.json"))
        .unwrap()
        .len();
    assert!(
        bytes < 2_000_000,
        "the automation store grew to {bytes} bytes"
    );
}

/// The audit trail of a dropped run goes with it.
#[test]
fn the_audit_trail_of_a_dropped_run_goes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    manager.upsert(automation(CatchUpPolicy::Skip)).unwrap();
    let mut dropped = Vec::new();
    for cycle in 0..600u64 {
        let run = manager.trigger_manual("daily", cycle).unwrap();
        manager
            .complete_run(
                &run.run_id,
                ExecutionOutcome::Completed {
                    detail: "soak".into(),
                },
                cycle,
            )
            .unwrap();
        dropped.push(run.run_id);
    }
    let kept: std::collections::BTreeSet<String> = manager
        .store
        .recent_runs
        .iter()
        .map(|run| run.run_id.clone())
        .collect();
    let audited: std::collections::BTreeSet<String> = manager
        .store
        .audit
        .iter()
        .filter_map(|event| event.run_id.clone())
        .collect();
    for run_id in &dropped {
        if !kept.contains(run_id) {
            assert!(
                !audited.contains(run_id),
                "the audit trail of the dropped run {run_id} was kept"
            );
        }
    }
}

#[test]
fn automation_run_keeps_an_immutable_template_and_creates_its_own_journaled_task() {
    let dir = tempfile::tempdir().unwrap();
    let mut auto = automation(CatchUpPolicy::Skip);
    let run = AutomationEngine::default().trigger(&auto, 10, "manual".into(), 10);
    auto.template.backend = "changed-after-trigger".into();
    auto.template.version = 4;
    let task = run.create_task(dir.path(), Some(dir.path())).unwrap();
    assert_eq!(task.task().id.0, run.task_id);
    assert_eq!(task.task().backend_id, "builtin");
    assert_eq!(run.template.version, 3);
    assert!(dir
        .path()
        .join("agent-tasks")
        .join(&run.task_id)
        .join("snapshot.json")
        .exists());
}

#[test]
fn scheduler_snapshot_persists_and_running_work_recovers_as_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scheduler.json");
    let mut scheduler = Scheduler::new(SlotLimits {
        global: 1,
        per_provider: 1,
        per_workspace: 1,
        per_environment: 1,
    });
    scheduler.enqueue(ScheduledTask {
        id: "running".into(),
        provider: "p".into(),
        workspace: "w".into(),
        environment: "e".into(),
        priority: 1,
        sequence: 1,
    });
    scheduler.next_ready(0).unwrap();
    scheduler.snapshot().persist(&path).unwrap();
    let snapshot = SchedulerSnapshot::load(&path).unwrap();
    let (mut recovered, interrupted) = Scheduler::restore_after_restart(snapshot);
    assert_eq!(interrupted, vec!["running"]);
    assert!(recovered.next_ready(0).is_none());
}

#[test]
fn integration_queue_is_serial_and_requires_validation() {
    let mut queue = IntegrationQueue::default();
    queue.enqueue(IntegrationRequest {
        task_id: "one".into(),
        environment_path: "one".into(),
        base_revision: "base".into(),
        target_branch: "main".into(),
        validation_passed: false,
    });
    queue.enqueue(IntegrationRequest {
        task_id: "two".into(),
        environment_path: "two".into(),
        base_revision: "base".into(),
        target_branch: "main".into(),
        validation_passed: true,
    });
    assert_eq!(queue.start_next().unwrap().task_id, "one");
    assert_eq!(
        queue.finish_active(true).unwrap_err(),
        OrchestrationError::UnverifiedWrite
    );
    assert_eq!(queue.finish_active(false).unwrap().task_id, "one");
    assert_eq!(queue.start_next().unwrap().task_id, "two");
}

#[test]
fn orchestration_runtime_executes_ready_children_in_parallel_and_then_dependencies() {
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let launcher = {
        let active = active.clone();
        let maximum = maximum.clone();
        let completed = completed.clone();
        Arc::new(
            move |spec: ChildTaskSpec, cancellation: termior_ai::CancellationToken| {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(40));
                active.fetch_sub(1, Ordering::SeqCst);
                if cancellation.is_cancelled() {
                    return Err("cancelled".into());
                }
                completed.lock().unwrap().push(spec.task_id.clone());
                Ok(ChildTaskResult {
                    task_id: spec.task_id,
                    data: json!({"ok":true}),
                    evidence: vec!["journal:event".into()],
                    snapshot: spec.snapshot,
                    change_sets: vec![],
                    acceptance_verified: true,
                    usage_known: true,
                    usage: TaskResourceUsage::default(),
                    stale: false,
                    unknown: false,
                    writes: false,
                })
            },
        )
    };
    let mut runtime = OrchestrationRuntime::new(
        SlotLimits {
            global: 2,
            per_provider: 2,
            per_workspace: 2,
            per_environment: 1,
        },
        launcher,
    );
    let spec = |id: &str, environment: &str, dependencies: Vec<String>| ChildTaskSpec {
        task_id: id.into(),
        parent_task_id: "root".into(),
        agent_id: "worker".into(),
        goal: id.into(),
        input_refs: vec![],
        output_schema: json!({"type":"object"}),
        tools: BTreeSet::new(),
        budgets: budgets(),
        environment_id: environment.into(),
        provider_id: "provider".into(),
        workspace_id: "workspace".into(),
        project_dir: "C:/workspace".into(),
        dependencies,
        depth: TaskDepth(1),
        writes: false,
        snapshot: SnapshotVersion(1),
    };
    runtime.submit(spec("a", "env-a", vec![]), 1).unwrap();
    runtime.submit(spec("b", "env-b", vec![]), 1).unwrap();
    runtime
        .submit(spec("after", "env-c", vec!["a".into(), "b".into()]), 1)
        .unwrap();
    let initial_tree = runtime.task_tree(7);
    let dependent = initial_tree
        .nodes
        .iter()
        .find(|node| node.task_id == "after")
        .unwrap();
    assert_eq!(dependent.state, NodeState::Queued);
    assert_eq!(dependent.queue_reason.as_deref(), Some("waiting for a, b"));
    assert_eq!(dependent.environment_id, "env-c");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !runtime.is_settled() && std::time::Instant::now() < deadline {
        runtime.tick(0);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    runtime.tick(0);
    assert!(runtime.is_settled());
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
    assert_eq!(runtime.graph().nodes["after"], NodeState::Succeeded);
    let final_tree = runtime.task_tree(8);
    assert!(final_tree
        .nodes
        .iter()
        .all(|node| node.result_verified == Some(true)));
    let completed = completed.lock().unwrap();
    let after_index = completed.iter().position(|id| id == "after").unwrap();
    assert!(completed[..after_index].contains(&"a".to_string()));
    assert!(completed[..after_index].contains(&"b".to_string()));
}

struct ScriptedExecutor {
    outcome: ExecutionOutcome,
    executed: Vec<String>,
}

impl RunExecutor for ScriptedExecutor {
    fn execute(&mut self, run: &AutomationRun) -> ExecutionOutcome {
        self.executed.push(run.run_id.clone());
        self.outcome.clone()
    }
}

fn host_limits(concurrency: u16, budget: u64, used: u64) -> HostLimits {
    HostLimits {
        max_concurrency: concurrency,
        disk_budget_bytes: budget,
        disk_used_bytes: used,
    }
}

#[test]
fn host_tick_claims_and_executes_under_concurrency_and_disk_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    let mut auto = automation(CatchUpPolicy::Skip);
    auto.trigger = Trigger::Manual;
    auto.template.max_concurrency = 4;
    manager.upsert(auto).unwrap();
    for offset in 0..3 {
        manager.trigger_manual("daily", 100 + offset).unwrap();
    }
    let created = manager
        .store
        .audit
        .iter()
        .find(|event| event.kind == "manual")
        .unwrap();
    assert_eq!(created.trigger.as_deref(), Some("manual"));
    assert_eq!(created.template_version, Some(3));
    assert!(created.dedupe_key.is_some());
    assert_eq!(created.schedule_reason.as_deref(), Some("manual"));

    let mut executor = ScriptedExecutor {
        outcome: ExecutionOutcome::Completed {
            detail: "done".into(),
        },
        executed: Vec::new(),
    };
    let effects = manager
        .tick(200, host_limits(1, 1_000, 10), &mut executor)
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(executor.executed.len(), 1);
    assert!(effects.iter().all(|effect| !effect.auto_approved));
    assert_eq!(
        manager
            .store
            .recent_runs
            .iter()
            .filter(|run| run.status == RunStatus::Queued)
            .count(),
        2
    );

    let mut blocked = ScriptedExecutor {
        outcome: ExecutionOutcome::Completed {
            detail: "should-not-run".into(),
        },
        executed: Vec::new(),
    };
    let blocked_effects = manager
        .tick(201, host_limits(4, 100, 100), &mut blocked)
        .unwrap();
    assert!(blocked_effects.is_empty());
    assert!(blocked.executed.is_empty());
    assert!(manager.store.audit.iter().any(|event| {
        event.kind == "deferred" && event.schedule_reason.as_deref() == Some("disk-budget")
    }));

    let mut tight = automation(CatchUpPolicy::Skip);
    tight.id = "tight".into();
    tight.trigger = Trigger::Manual;
    tight.template.max_concurrency = 1;
    manager.upsert(tight).unwrap();
    for offset in 0..3 {
        manager.trigger_manual("tight", 300 + offset).unwrap();
    }
    let mut limited = ScriptedExecutor {
        outcome: ExecutionOutcome::Merged {
            detail: "fast-forward".into(),
        },
        executed: Vec::new(),
    };
    let merged = manager
        .tick(400, host_limits(8, 1_000, 10), &mut limited)
        .unwrap();
    assert!(merged.iter().all(|effect| !effect.auto_approved));
    assert_eq!(
        manager
            .store
            .recent_runs
            .iter()
            .filter(|run| run.automation_id == "tight" && run.status == RunStatus::Completed)
            .count(),
        1
    );
    assert_eq!(
        manager
            .store
            .recent_runs
            .iter()
            .filter(|run| run.automation_id == "tight" && run.status == RunStatus::Queued)
            .count(),
        2
    );
    assert!(manager
        .store
        .audit
        .iter()
        .any(|event| { event.kind == "merge" && event.merge.as_deref() == Some("fast-forward") }));
    assert_eq!(
        manager
            .store
            .recent_runs
            .iter()
            .filter(|run| run.automation_id == "daily" && run.status == RunStatus::Queued)
            .count(),
        0,
        "queued runs execute once the disk budget allows them"
    );
}

#[test]
fn catch_up_admin_summary_counts_missed_coalesced_and_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    let mut auto = automation(CatchUpPolicy::RunOnce);
    manager.upsert(auto.clone()).unwrap();
    let executable = manager.catch_up("daily", 0, 50).unwrap();
    assert_eq!(executable.len(), 1);
    let summary = admin_summary(&manager.store, "daily", 50).unwrap();
    assert_eq!(summary.coalesced, 4);
    assert_eq!(summary.missed, 0);
    assert!(summary.next_run_at.is_some());
    assert_eq!(summary.last_runs.len(), 5);

    auto.id = "skip".into();
    auto.catch_up = CatchUpPolicy::Skip;
    manager.upsert(auto).unwrap();
    assert!(manager.catch_up("skip", 0, 50).unwrap().is_empty());
    let skipped = admin_summary(&manager.store, "skip", 50).unwrap();
    assert_eq!(skipped.missed, 5);
    assert_eq!(skipped.next_run_at, Some(60));

    let mut event = automation(CatchUpPolicy::Skip);
    event.id = "repo".into();
    event.trigger = Trigger::RepoEvent {
        event: "change".into(),
        coalesce_seconds: 30,
    };
    manager.upsert(event).unwrap();
    manager
        .trigger_repo_event("repo", "repo", "one", 10)
        .unwrap();
    manager
        .trigger_repo_event("repo", "repo", "one", 11)
        .unwrap();
    let repo = admin_summary(&manager.store, "repo", 11).unwrap();
    assert_eq!(repo.duplicate, 1);
    assert!(repo.next_run_at.is_none());
}

struct RefuseTools;

impl termior_ai::RuntimeToolExecutor for RefuseTools {
    fn execute(
        &self,
        _call_id: &str,
        _tool: &str,
        _arguments: &str,
        _cancellation: &termior_ai::CancellationToken,
    ) -> Result<termior_ai::ToolExecution, String> {
        Err("automation executed a tool before approval".into())
    }
}

#[test]
fn automation_approval_waits_are_notified_and_never_auto_approved() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = AutomationManager::load(dir.path()).unwrap();
    let mut auto = automation(CatchUpPolicy::Skip);
    auto.trigger = Trigger::Manual;
    auto.notification = NotificationPolicy::Actionable;
    manager.upsert(auto.clone()).unwrap();
    manager.trigger_manual("daily", 10).unwrap();
    let claimed = manager.claim_queued(11, host_limits(2, 0, 0)).unwrap();
    assert_eq!(claimed.len(), 1);

    let provider =
        termior_ai::MockProvider::single(vec![termior_ai::ChatEvent::Done(termior_ai::Message {
            role: termior_ai::Role::Assistant,
            content: String::new(),
            tool_calls: vec![termior_ai::ToolCall {
                id: "call-1".into(),
                name: "write_file".into(),
                arguments: r#"{"path":"notes.txt","content":"hi"}"#.into(),
            }],
            tool_result: None,
        })]);
    let outcome = futures::executor::block_on(drive_automation_run(
        &claimed[0],
        dir.path(),
        Some(dir.path()),
        &provider,
        &RefuseTools,
    ));
    assert!(
        matches!(outcome, ExecutionOutcome::NeedsApproval { .. }),
        "{outcome:?}"
    );
    let effect = manager
        .complete_run(&claimed[0].run_id, outcome, 12)
        .unwrap();
    let presentation = approval_presentation(NotificationPolicy::Actionable);
    assert!(!presentation.open_modal);
    assert!(!presentation.auto_approve);
    assert!(presentation.notify);
    assert_eq!(effect.status, RunStatus::WaitingUser);
    assert!(effect.notify);
    assert!(!effect.auto_approved);
    let approval = manager
        .store
        .audit
        .iter()
        .find(|event| event.kind == "approval")
        .unwrap();
    assert_eq!(approval.approval.as_deref(), Some("waiting"));
    assert_ne!(approval.approval.as_deref(), Some("approved"));
    assert_eq!(approval.template_version, Some(claimed[0].template_version));
    assert_eq!(
        approval.dedupe_key.as_deref(),
        Some(claimed[0].dedupe_key.as_str())
    );

    let recovered = termior_ai::TaskRuntime::recover_journal(
        dir.path(),
        &claimed[0].task_id,
        termior_ai::ApprovalPolicy::Prompt,
        termior_ai::ToolRegistry::default(),
    )
    .unwrap();
    assert_eq!(
        recovered.task().state,
        termior_ai::TaskState::WaitingApproval
    );
    assert!(recovered.invocations().iter().all(|invocation| {
        invocation
            .decision
            .as_ref()
            .is_none_or(|decision| !decision.approved)
    }));
    assert!(recovered
        .invocations()
        .iter()
        .all(|invocation| invocation.state != termior_ai::ToolState::Approved));

    auto.notification = NotificationPolicy::Muted;
    manager.upsert(auto).unwrap();
    manager.trigger_manual("daily", 20).unwrap();
    let muted_run = manager.claim_queued(21, host_limits(2, 0, 0)).unwrap();
    assert_eq!(muted_run.len(), 1);
    let muted = manager
        .complete_run(
            &muted_run[0].run_id,
            ExecutionOutcome::NeedsApproval {
                detail: "needs a person".into(),
            },
            22,
        )
        .unwrap();
    let muted_presentation = approval_presentation(NotificationPolicy::Muted);
    assert!(!muted_presentation.open_modal);
    assert!(!muted_presentation.auto_approve);
    assert!(!muted_presentation.notify);
    assert!(!muted.notify);
    assert!(!muted.auto_approved);
    assert_eq!(muted.status, RunStatus::WaitingUser);

    manager.trigger_manual("daily", 30).unwrap();
    let retry_run = manager.claim_queued(31, host_limits(1, 0, 0)).unwrap();
    let retried = manager
        .complete_run(
            &retry_run[0].run_id,
            ExecutionOutcome::Failed {
                class: FailureClass::Transient,
                idempotent: true,
                result_known: true,
                detail: "timeout".into(),
            },
            32,
        )
        .unwrap();
    assert_eq!(retried.status, RunStatus::Failed);
    assert!(manager.store.audit.iter().any(|event| {
        event.kind == "retry" && event.retry_decision.as_deref() == Some("retry")
    }));
    assert!(manager
        .store
        .recent_runs
        .iter()
        .any(|run| run.schedule_reason == "retry" && run.status == RunStatus::Queued));
    assert!(manager
        .store
        .audit
        .iter()
        .all(|event| { event.approval.as_deref() != Some("approved") }));
}

fn child_spec(id: &str, dependencies: Vec<String>) -> ChildTaskSpec {
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
        writes: false,
        snapshot: SnapshotVersion(1),
    }
}

fn unknown_result(spec: &ChildTaskSpec) -> ChildTaskResult {
    ChildTaskResult {
        task_id: spec.task_id.clone(),
        data: json!({"ok": false}),
        evidence: vec!["journal:event".into()],
        snapshot: spec.snapshot,
        change_sets: vec![],
        acceptance_verified: false,
        usage_known: false,
        usage: TaskResourceUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            cost_usd: Some(0.0),
            wall_clock_ms: Some(0),
            output_bytes: Some(0),
        },
        stale: false,
        unknown: true,
        writes: false,
    }
}

#[test]
fn cancel_guards_keep_unknown_side_effect_records() {
    let release = Arc::new(std::sync::Barrier::new(2));
    let launcher = {
        let release = release.clone();
        Arc::new(
            move |spec: ChildTaskSpec, cancellation: termior_ai::CancellationToken| {
                if spec.task_id == "running" {
                    release.wait();
                    while !cancellation.is_cancelled() {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    return Ok(unknown_result(&spec));
                }
                Ok(ChildTaskResult {
                    task_id: spec.task_id.clone(),
                    data: json!({"ok": true}),
                    evidence: vec!["journal:event".into()],
                    snapshot: spec.snapshot,
                    change_sets: vec![],
                    acceptance_verified: true,
                    usage_known: true,
                    usage: TaskResourceUsage {
                        input_tokens: Some(4),
                        output_tokens: Some(2),
                        cost_usd: Some(0.25),
                        wall_clock_ms: Some(15),
                        output_bytes: Some(32),
                    },
                    stale: false,
                    unknown: false,
                    writes: false,
                })
            },
        )
    };
    let mut runtime = OrchestrationRuntime::new(
        SlotLimits {
            global: 1,
            per_provider: 2,
            per_workspace: 2,
            per_environment: 2,
        },
        launcher,
    );
    runtime.submit(child_spec("running", vec![]), 1).unwrap();
    runtime
        .submit(child_spec("queued", vec!["running".into()]), 1)
        .unwrap();
    runtime.tick(0);
    release.wait();
    let before = runtime.task_tree(1);
    assert_eq!(before.nodes.len(), 2);
    let queued = before
        .nodes
        .iter()
        .find(|node| node.task_id == "queued")
        .unwrap();
    assert_eq!(queued.state, NodeState::Queued);
    assert_eq!(queued.queue_reason.as_deref(), Some("waiting for running"));
    assert_eq!(
        queued.ready_condition.as_deref(),
        Some("dependencies succeed: running")
    );
    assert_eq!(queued.blockers, vec!["running".to_string()]);
    assert_eq!(before.concurrency_cap, 1);

    runtime.cancel_child("running").unwrap();
    let cancelled = runtime.task_tree(2);
    assert_eq!(cancelled.nodes.len(), 2, "cancel does not drop records");
    let running = cancelled
        .nodes
        .iter()
        .find(|node| node.task_id == "running")
        .unwrap();
    assert_eq!(running.state, NodeState::Unknown);
    assert!(running.unknown_side_effects);
    assert!(!can_delete_task_record(running));
    assert_eq!(
        runtime.delete_record("running").unwrap_err(),
        OrchestrationError::UnknownRecordRetained
    );
    assert!(runtime
        .task_tree(3)
        .nodes
        .iter()
        .any(|node| node.task_id == "running"));

    runtime.cancel_all();
    let aggregate = runtime.task_tree(4);
    assert_eq!(aggregate.nodes.len(), 2);
    let queued = aggregate
        .nodes
        .iter()
        .find(|node| node.task_id == "queued")
        .unwrap();
    assert_eq!(queued.state, NodeState::Cancelled);
    assert!(!queued.unknown_side_effects);
    runtime.delete_record("queued").unwrap();
    let remaining = runtime.task_tree(5);
    assert!(remaining.nodes.iter().any(|node| node.task_id == "running"));
    assert!(!remaining.nodes.iter().any(|node| node.task_id == "queued"));
    assert_eq!(
        runtime.delete_record("running").unwrap_err(),
        OrchestrationError::UnknownRecordRetained
    );

    runtime.set_global_concurrency(3);
    assert_eq!(runtime.global_concurrency(), 3);
    assert_eq!(runtime.task_tree(6).concurrency_cap, 3);
}

#[test]
fn snapshot_cancel_and_concurrency_cap_keep_unknown_nodes() {
    let mut snapshot = TaskTreeSnapshot {
        generated_at_ms: 1,
        concurrency_cap: 2,
        nodes: vec![
            tree_node("root-child", NodeState::Running, vec![], false),
            tree_node("later", NodeState::Queued, vec!["root-child".into()], false),
        ],
        aggregate: TaskResourceUsage::default(),
        backends: vec![],
    };
    snapshot.nodes[1].queue_reason = Some("waiting for root-child".into());
    snapshot.nodes[1].ready_condition = Some("dependencies succeed: root-child".into());
    snapshot.nodes[1].blockers = vec!["root-child".into()];
    snapshot.cancel_child("root-child").unwrap();
    assert_eq!(snapshot.nodes.len(), 2);
    assert_eq!(snapshot.nodes[0].state, NodeState::Unknown);
    assert!(snapshot.nodes[0].unknown_side_effects);
    assert_eq!(snapshot.nodes[1].state, NodeState::Cancelled);
    assert_eq!(
        snapshot.delete_record("root-child").unwrap_err(),
        OrchestrationError::UnknownRecordRetained
    );
    snapshot.cancel_all();
    assert_eq!(snapshot.nodes.len(), 2);
    snapshot.set_concurrency_cap(1);
    assert_eq!(snapshot.concurrency_cap, 1);
    snapshot.delete_record("later").unwrap();
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].task_id, "root-child");
}

#[test]
fn unknown_cost_renders_as_unknown_and_does_not_sum_as_zero() {
    let known = TaskResourceUsage {
        input_tokens: Some(10),
        output_tokens: Some(4),
        cost_usd: Some(1.5),
        wall_clock_ms: Some(20),
        output_bytes: Some(8),
    };
    let unknown_price = TaskResourceUsage {
        input_tokens: Some(3),
        output_tokens: Some(1),
        cost_usd: None,
        wall_clock_ms: Some(5),
        output_bytes: Some(2),
    };
    let missing = TaskResourceUsage::default();
    assert_eq!(format_cost_usd(None), "unknown");
    assert_eq!(format_cost_usd(Some(0.0)), "0");
    assert_eq!(format_cost_usd(Some(1.5)), "1.5");
    assert_eq!(format_optional_quantity(None), "unknown");
    assert_eq!(format_optional_quantity(Some(0)), "0");
    let unknown_line = format_task_usage(&unknown_price);
    assert_eq!(
        unknown_line,
        "tokens 3/1 · cost unknown · time 5 · output 2"
    );
    assert!(!unknown_line.contains("cost 0"));
    let missing_line = format_task_usage(&missing);
    assert_eq!(
        missing_line,
        "tokens unknown/unknown · cost unknown · time unknown · output unknown"
    );
    assert!(!missing_line.contains("cost 0"));
    assert!(!missing_line.contains("time 0"));

    let mixed = aggregate_usage([&known, &unknown_price]);
    assert_eq!(mixed.cost_usd, None);
    assert_eq!(mixed.input_tokens, Some(13));
    assert_eq!(format_cost_usd(mixed.cost_usd), "unknown");
    let with_gap = aggregate_usage([&known, &missing]);
    assert_eq!(with_gap.cost_usd, None);
    assert_eq!(with_gap.wall_clock_ms, None);
    assert_eq!(with_gap.output_bytes, None);
    assert!(!format_task_usage(&with_gap).contains("cost 0"));

    let known_zero = aggregate_usage([&TaskResourceUsage {
        input_tokens: Some(0),
        output_tokens: Some(0),
        cost_usd: Some(0.0),
        wall_clock_ms: Some(0),
        output_bytes: Some(0),
    }]);
    assert_eq!(known_zero.cost_usd, Some(0.0));
    assert_eq!(format_cost_usd(known_zero.cost_usd), "0");

    let reported = known.clone();
    let mut runtime = OrchestrationRuntime::new(
        SlotLimits {
            global: 2,
            per_provider: 2,
            per_workspace: 2,
            per_environment: 2,
        },
        Arc::new(
            move |spec: ChildTaskSpec, _: termior_ai::CancellationToken| {
                let unpriced = spec.task_id == "unpriced";
                Ok(ChildTaskResult {
                    unknown: false,
                    usage_known: !unpriced,
                    usage: if unpriced {
                        TaskResourceUsage {
                            input_tokens: Some(9),
                            output_tokens: Some(1),
                            cost_usd: Some(0.0),
                            wall_clock_ms: Some(4),
                            output_bytes: Some(1),
                        }
                    } else {
                        reported.clone()
                    },
                    task_id: spec.task_id,
                    data: json!({"ok": true}),
                    evidence: vec!["journal:event".into()],
                    snapshot: spec.snapshot,
                    change_sets: vec![],
                    acceptance_verified: true,
                    stale: false,
                    writes: false,
                })
            },
        ),
    );
    runtime.submit(child_spec("known", vec![]), 1).unwrap();
    runtime.submit(child_spec("unpriced", vec![]), 1).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !runtime.is_settled() && std::time::Instant::now() < deadline {
        runtime.tick(0);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    runtime.tick(0);
    let tree = runtime.task_tree(9);
    let known_node = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "known")
        .unwrap();
    let unpriced_node = tree
        .nodes
        .iter()
        .find(|node| node.task_id == "unpriced")
        .unwrap();
    assert_eq!(
        format_task_usage(&known_node.usage),
        format_task_usage(&known)
    );
    assert_eq!(unpriced_node.usage.cost_usd, None);
    assert_eq!(unpriced_node.usage.input_tokens, None);
    assert_eq!(
        format_task_usage(&unpriced_node.usage),
        "tokens unknown/unknown · cost unknown · time 4 · output 1"
    );
    assert_eq!(tree.aggregate.cost_usd, None);
    assert_eq!(format_cost_usd(tree.aggregate.cost_usd), "unknown");
    assert_eq!(tree.backends[0].usage.cost_usd, None);
    assert!(!format_task_usage(&tree.aggregate).contains("cost 0"));
}

#[test]
fn queued_ready_condition_follows_the_user_concurrency_cap() {
    let mut runtime = OrchestrationRuntime::new(
        SlotLimits {
            global: 1,
            per_provider: 2,
            per_workspace: 2,
            per_environment: 2,
        },
        Arc::new(
            |spec: ChildTaskSpec, cancellation: termior_ai::CancellationToken| {
                while !cancellation.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Ok(unknown_result(&spec))
            },
        ),
    );
    runtime.submit(child_spec("first", vec![]), 1).unwrap();
    runtime.submit(child_spec("second", vec![]), 1).unwrap();
    runtime.tick(0);
    let waiting = runtime
        .task_tree(1)
        .nodes
        .into_iter()
        .find(|node| node.task_id == "second")
        .unwrap();
    assert_eq!(waiting.state, NodeState::Ready);
    assert_eq!(waiting.queue_reason.as_deref(), Some("global-slot"));
    assert_eq!(
        waiting.ready_condition.as_deref(),
        Some("a global concurrency slot is free (cap 1)")
    );
    runtime.set_global_concurrency(2);
    let released = runtime
        .task_tree(2)
        .nodes
        .into_iter()
        .find(|node| node.task_id == "second")
        .unwrap();
    assert_eq!(released.queue_reason, None);
    assert_eq!(released.ready_condition.as_deref(), Some("ready to run"));
    let mut view = runtime.task_tree(3);
    view.nodes
        .iter_mut()
        .find(|node| node.task_id == "second")
        .unwrap()
        .queue_reason = Some("global-slot".into());
    view.set_concurrency_cap(4);
    assert_eq!(view.concurrency_cap, 4);
    assert_eq!(
        view.nodes
            .iter()
            .find(|node| node.task_id == "second")
            .unwrap()
            .ready_condition
            .as_deref(),
        Some("a global concurrency slot is free (cap 4)")
    );
    runtime.cancel_all();
}

fn tree_node(
    id: &str,
    state: NodeState,
    dependencies: Vec<String>,
    unknown_side_effects: bool,
) -> TaskTreeNode {
    TaskTreeNode {
        task_id: id.into(),
        parent_task_id: "root".into(),
        agent_id: "worker".into(),
        provider_id: "provider".into(),
        environment_id: format!("env-{id}"),
        project_dir: "C:/workspace".into(),
        dependencies,
        state,
        budgets: budgets(),
        queue_reason: None,
        blockers: vec![],
        ready_condition: None,
        result_verified: None,
        unknown_side_effects,
        usage: TaskResourceUsage::default(),
    }
}
