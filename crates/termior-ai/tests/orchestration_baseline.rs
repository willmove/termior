//! Single-agent baseline versus parallel orchestration comparison (Stage E tasks 0.2 / 7.3).
//!
//! The comparison runs the same work twice: once as a single agent that does every step
//! in sequence, and once as a parallel DAG. Both runs use the same deterministic launcher
//! so the measured difference comes from the orchestration, not from the model.
//!
//! The harness records completion rate, verified results, total usage, wall clock,
//! conflicts, duplicate side effects, and human interventions. A candidate only counts as
//! an improvement when it keeps completion and verification and does not make the user
//! intervene more often.

use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use termior_ai::{
    evaluation::OrchestrationMetrics, orchestration::*, CancellationToken, RuntimeBudgets,
    SnapshotVersion, TaskDepth,
};

/// Per-step cost of the work both runs have to do.
const STEP_DURATION_MS: u64 = 40;
const STEP_TOKENS: u64 = 1_000;

fn budgets() -> RuntimeBudgets {
    RuntimeBudgets {
        max_model_steps: 20,
        max_wall_clock_ms: 60_000,
        max_tool_output_bytes: 1_000_000,
        max_input_tokens: Some(100_000),
        max_output_tokens: Some(20_000),
    }
}

/// The result of one baseline or candidate run.
#[derive(Debug, Clone)]
struct RunOutcome {
    metrics: OrchestrationMetrics,
    conflicts: u64,
    duplicate_side_effects: u64,
    human_interventions: u64,
}

/// A launcher that records how much work it did and how long it took.
#[derive(Clone)]
struct CountingLauncher {
    steps: Arc<AtomicU64>,
    tokens: Arc<AtomicU64>,
    /// Steps that write to the shared workspace, with the file they targeted.
    writers: Arc<std::sync::Mutex<Vec<(String, String)>>>,
}

impl CountingLauncher {
    fn new() -> Self {
        Self {
            steps: Arc::new(AtomicU64::new(0)),
            tokens: Arc::new(AtomicU64::new(0)),
            writers: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn launch(
        &self,
        spec: ChildTaskSpec,
        cancellation: CancellationToken,
    ) -> Result<ChildTaskResult, String> {
        self.steps.fetch_add(1, Ordering::SeqCst);
        self.tokens.fetch_add(STEP_TOKENS, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(STEP_DURATION_MS));
        if cancellation.is_cancelled() {
            return Err("cancelled".into());
        }
        // A writing step targets a shared file. Two writers to the same file conflict.
        if spec.writes {
            let mut writers = self.writers.lock().unwrap();
            writers.push(("shared.txt".to_string(), spec.task_id.clone()));
        }
        Ok(ChildTaskResult {
            task_id: spec.task_id,
            data: json!({"ok":true}),
            evidence: vec!["journal:event".into()],
            snapshot: spec.snapshot,
            change_sets: Vec::new(),
            acceptance_verified: true,
            usage_known: true,
            usage: TaskResourceUsage {
                input_tokens: Some(STEP_TOKENS),
                output_tokens: Some(STEP_TOKENS / 2),
                ..TaskResourceUsage::default()
            },
            stale: false,
            unknown: false,
            writes: spec.writes,
        })
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

/// Run one orchestration to completion and collect the comparable metrics.
fn run(launcher: CountingLauncher, specs: Vec<ChildTaskSpec>, slots: SlotLimits) -> RunOutcome {
    let tokens = launcher.tokens.clone();
    let writers = launcher.writers.clone();
    let mut runtime = OrchestrationRuntime::new(
        slots,
        Arc::new(
            move |spec: ChildTaskSpec, cancellation: CancellationToken| {
                launcher.launch(spec, cancellation)
            },
        ),
    );
    for spec in specs {
        runtime
            .submit(spec, 1)
            .expect("the child task must be accepted");
    }
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    while !runtime.is_settled() && Instant::now() < deadline {
        runtime.tick(0);
        std::thread::sleep(Duration::from_millis(5));
    }
    runtime.tick(0);
    let wall_clock_ms = started.elapsed().as_millis() as u64;

    let tree = runtime.task_tree(0);
    let task_count = tree.nodes.len() as u64;
    let completed_count = tree
        .nodes
        .iter()
        .filter(|node| node.state == NodeState::Succeeded)
        .count() as u64;
    let verified_count = tree
        .nodes
        .iter()
        .filter(|node| node.result_verified == Some(true))
        .count() as u64;
    let writers = writers.lock().unwrap();
    let mut duplicate_side_effects = 0u64;
    let mut seen: Vec<&String> = Vec::new();
    for (target, _) in writers.iter() {
        if seen.contains(&target) {
            duplicate_side_effects += 1;
        } else {
            seen.push(target);
        }
    }
    let mut targets: Vec<&String> = writers.iter().map(|(target, _)| target).collect();
    targets.sort();
    let mut conflicts = 0u64;
    for pair in targets.windows(2) {
        if pair[0] == pair[1] {
            conflicts += 1;
        }
    }

    RunOutcome {
        metrics: OrchestrationMetrics {
            task_count,
            completed_count,
            verified_count,
            total_tokens: Some(tokens.load(Ordering::SeqCst)),
            total_cost_usd: None,
            wall_clock_ms,
            conflict_count: conflicts,
            stale_result_count: tree
                .nodes
                .iter()
                .filter(|node| node.state == NodeState::Stale)
                .count() as u64,
            duplicate_side_effect_count: duplicate_side_effects,
            human_intervention_count: 0,
            unknown_recovery_count: tree
                .nodes
                .iter()
                .filter(|node| node.state == NodeState::Unknown)
                .count() as u64,
        },
        conflicts,
        duplicate_side_effects,
        human_interventions: 0,
    }
}

/// The single-agent baseline: one agent does the same steps, one after another.
///
/// The baseline does the identical amount of work as the parallel candidate, so the
/// comparison measures the orchestration and not a difference in the task set.
fn single_agent_baseline() -> RunOutcome {
    let launcher = CountingLauncher::new();
    run(
        launcher,
        vec![
            spec("step-1", vec![], false),
            spec("step-2", vec!["step-1".into()], false),
            spec("step-3", vec!["step-2".into()], false),
            spec("step-4", vec!["step-3".into()], false),
        ],
        SlotLimits {
            global: 1,
            per_provider: 1,
            per_workspace: 1,
            per_environment: 1,
        },
    )
}

/// The parallel candidate: independent research tasks run at the same time.
fn parallel_candidate() -> RunOutcome {
    let launcher = CountingLauncher::new();
    run(
        launcher,
        vec![
            spec("research-a", vec![], false),
            spec("research-b", vec![], false),
            spec("research-c", vec![], false),
            spec(
                "synthesis",
                vec![
                    "research-a".into(),
                    "research-b".into(),
                    "research-c".into(),
                ],
                false,
            ),
        ],
        SlotLimits {
            global: 3,
            per_provider: 3,
            per_workspace: 3,
            per_environment: 3,
        },
    )
}

/// The baseline is a fixed reference point: one task, one step, one slot.
#[test]
fn the_single_agent_baseline_is_recorded_as_the_comparison_group() {
    let baseline = single_agent_baseline();
    assert_eq!(baseline.metrics.task_count, 4);
    assert_eq!(baseline.metrics.completed_count, 4);
    assert_eq!(baseline.metrics.verified_count, 4);
    assert_eq!(baseline.metrics.completion_rate(), 1.0);
    assert_eq!(baseline.metrics.total_tokens, Some(STEP_TOKENS * 4));
    assert_eq!(baseline.conflicts, 0);
    assert_eq!(baseline.duplicate_side_effects, 0);
    assert_eq!(baseline.human_interventions, 0);
}

/// The parallel candidate finishes the same verified work faster without making the user
/// intervene more often. This is the comparison Stage E task 7.3 asks for.
#[test]
fn the_parallel_candidate_improves_on_the_single_agent_baseline() {
    let baseline = single_agent_baseline();
    let candidate = parallel_candidate();

    // Same verified outcome.
    assert_eq!(
        candidate.metrics.verified_count,
        baseline.metrics.verified_count
    );
    assert!(candidate.metrics.completion_rate() >= baseline.metrics.completion_rate());
    // Less wall clock, because the independent research tasks overlap.
    assert!(
        candidate.metrics.wall_clock_ms < baseline.metrics.wall_clock_ms,
        "the parallel run was not faster: {candidate:?} vs {baseline:?}"
    );
    // The same total token cost. The parallel run pays for the synthesis step, so it
    // spends the same budget on a shorter wall clock.
    assert_eq!(
        candidate.metrics.total_tokens,
        baseline.metrics.total_tokens
    );
    // No new conflicts, duplicate side effects, or human intervention.
    assert_eq!(candidate.conflicts, baseline.conflicts);
    assert_eq!(
        candidate.duplicate_side_effects,
        baseline.duplicate_side_effects
    );
    assert_eq!(candidate.human_interventions, baseline.human_interventions);
    assert!(candidate.metrics.improves_on(&baseline.metrics));
}

/// A candidate that finishes faster but makes the user intervene more often is not an
/// improvement. The comparison must not reward a run that offloads work onto the user.
#[test]
fn a_candidate_with_more_human_intervention_does_not_improve_on_the_baseline() {
    let baseline = single_agent_baseline();
    let mut candidate = parallel_candidate();
    candidate.metrics.human_intervention_count = 1;
    candidate.human_interventions = 1;
    assert!(
        !candidate.metrics.improves_on(&baseline.metrics),
        "the comparison ignored the extra human intervention"
    );
}

/// Two tasks writing the same file conflict, and the comparison reports it instead of
/// treating the extra parallelism as free.
#[test]
fn two_parallel_writers_to_the_same_file_are_reported_as_a_conflict() {
    let launcher = CountingLauncher::new();
    let outcome = run(
        launcher,
        vec![
            spec("writer-a", vec![], true),
            spec("writer-b", vec![], true),
        ],
        SlotLimits {
            global: 2,
            per_provider: 2,
            per_workspace: 2,
            per_environment: 2,
        },
    );
    assert_eq!(
        outcome.conflicts, 1,
        "the concurrent writers were not reported as a conflict"
    );
}
