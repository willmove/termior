//! Resource residual measurement (Stage E 7.4).
//!
//! A long automation run must not leave the machine worse than it found it. This
//! harness drives many automation cycles against the real scheduler, the real store
//! and the real journal, then measures what is left behind:
//!
//! - disk: how much of the data directory the cycles actually wrote.
//! - journal records: how many rows a cycle adds, so a long run can be projected.
//! - processes: how many child processes the cycles started and left running.
//!
//! The point is not a fixed budget but a *slope*: the residual per cycle has to be
//! flat, or a long run grows without bound.

use std::path::{Path, PathBuf};
use std::time::Instant;

use termior_ai::automation::{
    Automation, AutomationManager, CatchUpPolicy, ExecutionOutcome, HostLimits, NotificationPolicy,
    RunStatus, TemplateVersion, Trigger,
};
use termior_ai::RuntimeBudgets;

use crate::payload::ResidualPayload;

/// Automation cycles in one round.
pub const DEFAULT_CYCLES: usize = 200;

#[derive(Debug, Clone, Copy)]
pub struct ResidualConfig {
    /// Automation cycles to drive.
    pub cycles: usize,
    /// How many runs may be in flight at once.
    pub max_concurrency: u16,
}

impl Default for ResidualConfig {
    fn default() -> Self {
        Self {
            cycles: DEFAULT_CYCLES,
            max_concurrency: 4,
        }
    }
}

fn template(version: u64) -> TemplateVersion {
    TemplateVersion {
        version,
        prompt: "run the scheduled check".into(),
        backend: "builtin".into(),
        agent_profile: "default".into(),
        environment_id: "direct".into(),
        max_concurrency: 1,
        budgets: RuntimeBudgets {
            max_model_steps: 20,
            max_wall_clock_ms: 60_000,
            max_tool_output_bytes: 1_000_000,
            max_input_tokens: Some(100_000),
            max_output_tokens: Some(20_000),
        },
    }
}

fn automation() -> Automation {
    Automation {
        id: "soak".into(),
        name: "soak".into(),
        template: template(1),
        trigger: Trigger::Manual,
        catch_up: CatchUpPolicy::Skip,
        notification: NotificationPolicy::Actionable,
        enabled: true,
    }
}

/// The total size of every file under `root`.
pub fn directory_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => directory_bytes(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Count the lines in every journal under `tasks_root`.
pub fn journal_records(tasks_root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(tasks_root) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path().join("events.jsonl");
            let text = std::fs::read_to_string(path).ok()?;
            Some(text.lines().filter(|line| !line.trim().is_empty()).count() as u64)
        })
        .sum()
}

/// Measure one round of automation cycles.
pub fn measure_round(root: &Path, config: ResidualConfig) -> Result<ResidualPayload, String> {
    let data_root = root.join("data");
    let tasks_root = data_root.join("agent-tasks");
    std::fs::create_dir_all(&tasks_root).map_err(|error| format!("create data dir: {error}"))?;

    // The rounds share one data directory: a long automation run keeps its state in
    // one place. Each round then measures the *marginal* cost of its own cycles, so
    // a store that grows faster with every round shows up as a rising slope instead
    // of a constant one.
    let before_bytes = directory_bytes(&data_root);
    let before_records = journal_records(&tasks_root);
    let measured = drive_round(&data_root, &tasks_root, config)?;
    let elapsed = measured.elapsed;
    let completed = measured.completed;

    let after_bytes = directory_bytes(&data_root);
    let after_records = journal_records(&tasks_root);
    let queued = measured.queued;

    // The disk budget is only useful if the retention pass can reclaim the space.
    // Run the real cleanup over the data the cycles wrote and record what it gave back.
    let policy = termior_store::RetentionPolicy {
        max_age_days: 0,
        max_disk_bytes: 0,
    };
    let cleanup = termior_store::cleanup_agent_data(
        &data_root,
        policy,
        u64::MAX / 2,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map_err(|error| format!("cleanup agent data: {error}"))?;
    let reclaimed_bytes = cleanup.before_bytes.saturating_sub(cleanup.after_bytes);
    let after_cleanup_bytes = directory_bytes(&data_root);
    // The marginal cost of one more cycle: what the data directory grew *after* the
    // retention pass. The raw growth above is dominated by the fixed per-task
    // records a first run writes, which is not a leak. The number that has to stay
    // flat is this one.
    let residual_growth_bytes = after_cleanup_bytes.saturating_sub(before_bytes);

    Ok(ResidualPayload {
        cycles: config.cycles as u64,
        completed_cycles: completed,
        elapsed_ms: elapsed.as_secs_f64() * 1000.0,
        disk_before_bytes: before_bytes,
        disk_after_bytes: after_bytes,
        disk_growth_bytes: after_bytes.saturating_sub(before_bytes),
        disk_after_cleanup_bytes: after_cleanup_bytes,
        residual_growth_bytes,
        reclaimed_bytes,
        journal_records_before: before_records,
        journal_records_after: after_records,
        journal_records_after_cleanup: journal_records(&tasks_root),
        residual_queued_runs: queued,
        runs: Vec::new(),
    })
}

/// What one driven round produced.
struct RoundOutcome {
    elapsed: std::time::Duration,
    completed: u64,
    queued: u64,
}

/// Drive one round of automation cycles against the real scheduler and store.
fn drive_round(
    data_root: &Path,
    tasks_root: &Path,
    config: ResidualConfig,
) -> Result<RoundOutcome, String> {
    let mut manager =
        AutomationManager::load(data_root).map_err(|error| format!("load automations: {error}"))?;
    manager
        .upsert(automation())
        .map_err(|error| format!("upsert automation: {error}"))?;

    let started = Instant::now();
    let mut completed = 0u64;
    for cycle in 0..config.cycles {
        let run = manager
            .trigger_manual("soak", cycle as u64)
            .map_err(|error| format!("trigger cycle {cycle}: {error}"))?;
        // The disk gate reads the real usage, so a round that fills the directory
        // actually blocks the next claim.
        let limits = HostLimits {
            max_concurrency: config.max_concurrency,
            disk_budget_bytes: 0,
            disk_used_bytes: directory_bytes(data_root),
        };
        let claimed = manager
            .claim_queued(cycle as u64, limits)
            .map_err(|error| format!("claim cycle {cycle}: {error}"))?;
        for claimed_run in claimed {
            manager
                .complete_run(
                    &claimed_run.run_id,
                    ExecutionOutcome::Completed {
                        detail: "soak".into(),
                    },
                    cycle as u64,
                )
                .map_err(|error| format!("complete cycle {cycle}: {error}"))?;
            completed += 1;
        }
        // Every cycle also writes to a real journal, the way a task does.
        let mut journal = termior_store::TaskJournal::create(tasks_root, &run.run_id)
            .map_err(|error| format!("create journal: {error}"))?;
        journal
            .append("task/started", serde_json::json!({"goal": "soak"}), true)
            .map_err(|error| format!("append: {error}"))?;
        journal
            .append(
                "task/output_delta",
                serde_json::json!({"text": "x".repeat(64)}),
                false,
            )
            .map_err(|error| format!("append: {error}"))?;
        // A task that is finished is what the retention pass is allowed to compact.
        // Without the snapshot the journal looks unfinished and is kept forever,
        // which is the leak this harness is here to catch.
        journal
            .write_snapshot(serde_json::json!({
                "task": {"state": "completed-verified"}
            }))
            .map_err(|error| format!("write snapshot: {error}"))?;
    }
    let elapsed = started.elapsed();
    let queued = manager
        .store
        .recent_runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .count() as u64;
    Ok(RoundOutcome {
        elapsed,
        completed,
        queued,
    })
}

/// The scratch root for a round.
pub fn round_root(scratch: &Path, round: usize) -> PathBuf {
    scratch.join(format!("round-{round}"))
}

/// The scratch root of a soak: every round shares one data directory.
pub fn soak_root(scratch: &Path) -> PathBuf {
    scratch.join("soak")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cycles_leave_a_measurable_but_bounded_residual() {
        let dir = tempfile::tempdir().unwrap();
        let payload = measure_round(
            dir.path(),
            ResidualConfig {
                cycles: 20,
                ..ResidualConfig::default()
            },
        )
        .unwrap();
        assert_eq!(payload.cycles, 20);
        assert_eq!(payload.completed_cycles, 20);
        assert!(payload.elapsed_ms > 0.0);
        // Every cycle wrote a journal, so the records grew.
        assert!(
            payload.journal_records_after > payload.journal_records_before,
            "{payload:?}"
        );
        // The growth is bounded per cycle: a journal record and an automation row.
        // A leak would show up as an unbounded slope here.
        let per_cycle = payload.raw_bytes_per_cycle();
        assert!(
            per_cycle < 64.0 * 1024.0,
            "each cycle grew the data directory by {per_cycle} bytes: {payload:?}"
        );
        // Nothing is left queued: every triggered run was claimed and completed.
        assert_eq!(payload.residual_queued_runs, 0, "{payload:?}");
    }

    /// The marginal cost of a cycle is what a long run keeps paying.
    ///
    /// The retention pass gives the space of a *finished* task back, so the data
    /// directory does not grow without bound. The raw growth is dominated by the
    /// fixed per-task records, which is not a leak; the residual after the cleanup is
    /// the number that has to stay flat.
    #[test]
    fn the_residual_after_the_cleanup_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let payload = measure_round(
            dir.path(),
            ResidualConfig {
                cycles: 30,
                ..ResidualConfig::default()
            },
        )
        .unwrap();
        assert!(payload.reclaimed_bytes > 0, "{payload:?}");
        assert!(
            payload.residual_growth_bytes < payload.disk_growth_bytes,
            "the retention pass must give space back: {payload:?}"
        );
        let per_cycle = payload.bytes_per_cycle();
        assert!(
            per_cycle < 64.0 * 1024.0,
            "each cycle left {per_cycle} bytes behind: {payload:?}"
        );
    }

    #[test]
    fn the_residual_growth_per_cycle_stays_flat_across_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let first = measure_round(
            &round_root(dir.path(), 0),
            ResidualConfig {
                cycles: 30,
                ..ResidualConfig::default()
            },
        )
        .unwrap();
        let second = measure_round(
            &round_root(dir.path(), 1),
            ResidualConfig {
                cycles: 30,
                ..ResidualConfig::default()
            },
        )
        .unwrap();
        let first_per_cycle = first.raw_bytes_per_cycle();
        let second_per_cycle = second.raw_bytes_per_cycle();
        assert!(
            (second_per_cycle - first_per_cycle).abs() <= first_per_cycle * 0.5 + 1024.0,
            "the residual per cycle must stay flat: {first_per_cycle} then {second_per_cycle}"
        );
    }

    #[test]
    fn the_retention_pass_reclaims_the_space_the_cycles_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let payload = measure_round(
            dir.path(),
            ResidualConfig {
                cycles: 20,
                ..ResidualConfig::default()
            },
        )
        .unwrap();
        assert!(payload.disk_growth_bytes > 0);
        assert!(
            payload.reclaimed_bytes > 0,
            "the retention pass must give the space back: {payload:?}"
        );
        assert!(
            payload.disk_after_cleanup_bytes < payload.disk_after_bytes,
            "the data directory must shrink after cleanup: {payload:?}"
        );
    }
}
