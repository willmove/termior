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
