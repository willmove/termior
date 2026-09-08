//! Cancellable, reference-aware cleanup for Agent artifacts and completed task data.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub max_age_days: u64,
    pub max_disk_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_days: 30,
            max_disk_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified_ms: u64,
    pub retrievable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub deleted: Vec<PathBuf>,
    pub preserved: Vec<PathBuf>,
    pub cancelled: bool,
}

pub struct RetentionManager {
    root: PathBuf,
    policy: RetentionPolicy,
}

impl RetentionManager {
    pub fn new(root: impl Into<PathBuf>, policy: RetentionPolicy) -> Self {
        Self {
            root: root.into(),
            policy,
        }
    }

    pub fn plan(&self, now_ms: u64) -> Result<Vec<CleanupCandidate>, RetentionError> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let cutoff = now_ms.saturating_sub(self.policy.max_age_days.saturating_mul(86_400_000));
        let mut candidates = Vec::new();
        collect_files(&self.root, &mut candidates)?;
        candidates.sort_by_key(|candidate| {
            (
                !candidate.retrievable,
                candidate.modified_ms >= cutoff,
                candidate.modified_ms,
                candidate.path.clone(),
            )
        });
        Ok(candidates)
    }

    pub fn cleanup(
        &self,
        now_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<CleanupReport, RetentionError> {
        let candidates = self.plan(now_ms)?;
        let mut report = CleanupReport {
            before_bytes: candidates.iter().map(|item| item.bytes).sum(),
            ..CleanupReport::default()
        };
        let cutoff = now_ms.saturating_sub(self.policy.max_age_days.saturating_mul(86_400_000));
        let mut remaining = report.before_bytes;
        for candidate in candidates {
            if cancelled.load(Ordering::Acquire) {
                report.cancelled = true;
                break;
            }
            let over_budget = remaining > self.policy.max_disk_bytes;
            let expired = candidate.modified_ms < cutoff;
            // Summary, acceptance, snapshots, and journals keep references and recovery evidence.
            let protected = is_protected(&candidate.path);
            if candidate.retrievable && !protected && (expired || over_budget) {
                std::fs::remove_file(&candidate.path)?;
                remaining = remaining.saturating_sub(candidate.bytes);
                report.deleted.push(candidate.path);
            } else {
                report.preserved.push(candidate.path);
            }
        }
        report.after_bytes = remaining;
        Ok(report)
    }
}

fn collect_files(root: &Path, output: &mut Vec<CleanupCandidate>) -> Result<(), RetentionError> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_files(&path, output)?;
        } else if metadata.is_file() {
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0);
            output.push(CleanupCandidate {
                retrievable: is_retrievable(&path),
                path,
                bytes: metadata.len(),
                modified_ms,
            });
        }
    }
    Ok(())
}

fn is_retrievable(path: &Path) -> bool {
    path.components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        part == "blobs" || part == "tool-output" || part == "artifacts"
    })
}

fn is_protected(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("events.jsonl" | "snapshot.json" | "summary.json" | "acceptance.json")
    )
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("retention I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
