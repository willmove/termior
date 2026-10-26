//! Agent IO measurement (task journal write path). `nfr-agent-io` binary and the unit
//! tests share this module.
//!
//! One round measures:
//! 1. A burst of non-critical journal events (the shape of a streaming agent turn: text
//!    deltas and tool output). The journal batches these and flushes every 32 events, so
//!    this measures the write path a busy task actually hits.
//! 2. A critical event (a task state transition). Critical events flush immediately, so
//!    this measures the latency the recovery guarantee depends on.
//! 3. A journal replay, which is what a restart pays.
//!
//! The journal is the durability boundary of a task. If it slows down, a busy turn
//! stalls; if the replay slows down, a restart stalls. Both are measured here rather
//! than left as "should be fine".

use std::path::{Path, PathBuf};
use std::time::Instant;

use termior_store::TaskJournal;

use crate::payload::AgentIoPayload;

/// Non-critical events in one burst. A streaming turn produces deltas at this rate.
pub const DEFAULT_BURST_EVENTS: usize = 512;

/// The payload a delta event carries. Sized like a real tool-output chunk.
fn delta_payload(index: usize) -> serde_json::Value {
    serde_json::json!({
        "text": format!("delta-{index} {}", "x".repeat(64)),
        "item_id": format!("item-{index}"),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct AgentIoConfig {
    /// Non-critical events written in one burst.
    pub burst_events: usize,
    /// Critical events written after the burst.
    pub critical_events: usize,
}

impl Default for AgentIoConfig {
    fn default() -> Self {
        Self {
            burst_events: DEFAULT_BURST_EVENTS,
            critical_events: 8,
        }
    }
}

/// Measure one round of the journal write path.
pub fn measure_round(root: &Path, config: AgentIoConfig) -> Result<AgentIoPayload, String> {
    std::fs::create_dir_all(root).map_err(|error| format!("create {}: {error}", root.display()))?;
    let task_id = "agent-io";
    let mut journal =
        TaskJournal::create(root, task_id).map_err(|error| format!("create journal: {error}"))?;

    // 1. A burst of non-critical deltas.
    let burst_started = Instant::now();
    let mut burst_samples = Vec::with_capacity(config.burst_events);
    for index in 0..config.burst_events {
        let started = Instant::now();
        journal
            .append("task/output_delta", delta_payload(index), false)
            .map_err(|error| format!("append delta {index}: {error}"))?;
        burst_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let burst_elapsed = burst_started.elapsed();

    // 2. Critical events, which flush on every append.
    let critical_started = Instant::now();
    let mut critical_samples = Vec::with_capacity(config.critical_events);
    for index in 0..config.critical_events {
        let started = Instant::now();
        journal
            .append(
                "task/state_changed",
                serde_json::json!({"state": "Running", "index": index}),
                true,
            )
            .map_err(|error| format!("append critical {index}: {error}"))?;
        critical_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let critical_elapsed = critical_started.elapsed();

    // 3. Replay: what a restart pays to rebuild the task.
    let replay_started = Instant::now();
    let recovery = TaskJournal::recover(root, task_id, &[])
        .map_err(|error| format!("recover journal: {error}"))?;
    let replay_elapsed = replay_started.elapsed();

    let expected_events = config.burst_events + config.critical_events;
    if recovery.events.len() != expected_events {
        return Err(format!(
            "the journal replayed {} events, expected {expected_events}",
            recovery.events.len()
        ));
    }

    Ok(AgentIoPayload {
        burst_events: config.burst_events as u64,
        critical_events: config.critical_events as u64,
        burst_total_ms: burst_elapsed.as_secs_f64() * 1000.0,
        burst_p99_ms: percentile(&burst_samples, 0.99),
        burst_max_ms: burst_samples.iter().copied().fold(None, |worst, sample| {
            Some(worst.map_or(sample, |worst: f64| worst.max(sample)))
        }),
        per_event_us: burst_elapsed.as_secs_f64() * 1_000_000.0 / config.burst_events.max(1) as f64,
        critical_total_ms: critical_elapsed.as_secs_f64() * 1000.0,
        critical_p99_ms: percentile(&critical_samples, 0.99),
        replay_ms: replay_elapsed.as_secs_f64() * 1000.0,
        journal_bytes: std::fs::metadata(root.join(task_id).join("events.jsonl"))
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        runs: Vec::new(),
    })
}

/// The `quantile` fraction of the samples, or `None` when there are none.
fn percentile(samples: &[f64], quantile: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * quantile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    Some(sorted[index])
}

/// The scratch root for a round.
pub fn round_root(scratch: &Path, round: usize) -> PathBuf {
    scratch.join(format!("round-{round}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the per-append cost actually goes. The journal opens the file on every
    /// append; this measures that against a kept handle so the fix is evidence-based.
    #[test]
    fn opening_the_journal_per_append_dominates_the_write_cost() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![b'x'; 200];

        let per_append = dir.path().join("per-append.jsonl");
        std::fs::File::create(&per_append).unwrap();
        let started = Instant::now();
        for _ in 0..256 {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&per_append)
                .unwrap();
            file.write_all(&payload).unwrap();
            file.flush().unwrap();
        }
        let per_append_ms = started.elapsed().as_secs_f64() * 1000.0 / 256.0;

        let kept = dir.path().join("kept.jsonl");
        std::fs::File::create(&kept).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&kept)
            .unwrap();
        let started = Instant::now();
        for _ in 0..256 {
            file.write_all(&payload).unwrap();
            file.flush().unwrap();
        }
        let kept_ms = started.elapsed().as_secs_f64() * 1000.0 / 256.0;

        assert!(
            per_append_ms > kept_ms,
            "re-opening the file should cost more than keeping it open: \
             per-append {per_append_ms:.3} ms vs kept {kept_ms:.3} ms"
        );
    }

    #[test]
    fn a_burst_of_deltas_is_written_and_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let payload = measure_round(dir.path(), AgentIoConfig::default()).unwrap();
        assert_eq!(payload.burst_events, DEFAULT_BURST_EVENTS as u64);
        assert_eq!(payload.critical_events, 8);
        assert!(payload.burst_total_ms > 0.0);
        assert!(payload.critical_total_ms > 0.0);
        assert!(payload.replay_ms > 0.0);
        assert!(payload.journal_bytes > 0);
        // The journal batches non-critical events, so a burst must be cheaper per event
        // than a critical flush. The critical path pays an fsync per event.
        assert!(
            payload.critical_total_ms / payload.critical_events as f64
                > payload.per_event_us / 1000.0,
            "a critical flush should cost more than a batched delta: {payload:?}"
        );
    }
}
