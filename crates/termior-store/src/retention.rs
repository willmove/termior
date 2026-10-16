//! Cancellable, reference-aware cleanup for Agent artifacts and completed task data.
//!
//! Cleanup never rewrites a journal. Removable files are renamed aside in the same directory
//! and only then unlinked, so a crash cannot truncate a live file. Sessions that still hold
//! [`SessionLease`] are left untouched, including shared blobs and logs they may still append.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::UNIX_EPOCH;
use thiserror::Error;

const MIB: u64 = 1024 * 1024;
const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionPolicy {
    pub max_age_days: u64,
    pub max_disk_bytes: u64,
}

impl RetentionPolicy {
    pub const MIN_AGE_DAYS: u64 = 1;
    pub const MAX_AGE_DAYS: u64 = 3_650;
    pub const MIN_DISK_BYTES: u64 = 32 * MIB;
    pub const MAX_DISK_BYTES: u64 = 1024 * 1024 * MIB;

    pub fn in_settings_range(&self) -> bool {
        (Self::MIN_AGE_DAYS..=Self::MAX_AGE_DAYS).contains(&self.max_age_days)
            && (Self::MIN_DISK_BYTES..=Self::MAX_DISK_BYTES).contains(&self.max_disk_bytes)
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age_days: 30,
            max_disk_bytes: 2 * 1024 * MIB,
        }
    }
}

/// Byte totals for the Agent data a settings page can show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskUsage {
    pub journal: u64,
    pub blobs: u64,
    pub context: u64,
    pub hook_mcp: u64,
    pub crash: u64,
    pub other: u64,
}

impl DiskUsage {
    pub fn total(self) -> u64 {
        self.journal
            .saturating_add(self.blobs)
            .saturating_add(self.context)
            .saturating_add(self.hook_mcp)
            .saturating_add(self.crash)
            .saturating_add(self.other)
    }

    fn add(&mut self, kind: UsageKind, bytes: u64) {
        let slot = match kind {
            UsageKind::Journal => &mut self.journal,
            UsageKind::Blobs => &mut self.blobs,
            UsageKind::Context => &mut self.context,
            UsageKind::HookMcp => &mut self.hook_mcp,
            UsageKind::Crash => &mut self.crash,
            UsageKind::Other => &mut self.other,
        };
        *slot = slot.saturating_add(bytes);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageKind {
    Journal,
    Blobs,
    Context,
    HookMcp,
    Crash,
    Other,
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

/// Exclusive lease for one running Agent task.
///
/// The lock is per open file description, so a cleanup pass in this process can see it.
/// Dropping the lease releases the lock. A leftover `session.lock` file without a holder
/// does not keep the task in use.
pub struct SessionLease {
    _file: File,
}

impl SessionLease {
    pub fn acquire(tasks_root: impl AsRef<Path>, task_id: &str) -> Result<Self, io::Error> {
        let task_id = sanitize_task_id(task_id)?;
        let dir = tasks_root.as_ref().join(task_id);
        std::fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("session.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                ErrorKind::ResourceBusy,
                "agent session is already running",
            )),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

#[derive(Clone, Copy)]
enum ScanScope {
    /// Every regular file under the root. Used by retention tests and custom roots.
    Tree,
    /// Only Agent data inside an application data directory.
    AppData,
}

pub struct RetentionManager {
    root: PathBuf,
    policy: RetentionPolicy,
    scope: ScanScope,
}

impl RetentionManager {
    pub fn new(root: impl Into<PathBuf>, policy: RetentionPolicy) -> Self {
        Self {
            root: root.into(),
            policy,
            scope: ScanScope::Tree,
        }
    }

    pub fn for_app_data(data_dir: impl Into<PathBuf>, policy: RetentionPolicy) -> Self {
        Self {
            root: data_dir.into(),
            policy,
            scope: ScanScope::AppData,
        }
    }

    pub fn usage(&self) -> Result<DiskUsage, RetentionError> {
        let mut usage = DiskUsage::default();
        for file in self.scan()? {
            usage.add(file.kind, file.bytes);
        }
        Ok(usage)
    }

    pub fn plan(&self, now_ms: u64) -> Result<Vec<CleanupCandidate>, RetentionError> {
        let _ = now_ms;
        Ok(self
            .scan()?
            .into_iter()
            .map(|file| CleanupCandidate {
                retrievable: file.retrievable,
                path: file.path,
                bytes: file.bytes,
                modified_ms: file.modified_ms,
            })
            .collect())
    }

    pub fn cleanup(
        &self,
        now_ms: u64,
        cancelled: &AtomicBool,
    ) -> Result<CleanupReport, RetentionError> {
        self.cleanup_observing(now_ms, cancelled, &mut |_| {})
    }

    fn cleanup_observing(
        &self,
        now_ms: u64,
        cancelled: &AtomicBool,
        after_delete: &mut dyn FnMut(&Path),
    ) -> Result<CleanupReport, RetentionError> {
        let root = match canonicalize_existing(&self.root) {
            Some(root) => root,
            None => return Ok(CleanupReport::default()),
        };
        let files = self.scan()?;
        let mut report = CleanupReport {
            before_bytes: files.iter().map(|file| file.bytes).sum(),
            ..CleanupReport::default()
        };
        if cancelled.load(Ordering::Acquire) {
            report.cancelled = true;
            report.after_bytes = report.before_bytes;
            return Ok(report);
        }
        let cutoff = now_ms.saturating_sub(self.policy.max_age_days.saturating_mul(DAY_MS));
        let mut remaining = report.before_bytes;
        let mut deletable: Vec<(u8, ScannedFile)> = files
            .into_iter()
            .filter_map(|file| {
                let rank = deletion_rank(&file)?;
                Some((rank, file))
            })
            .collect();
        deletable.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.modified_ms.cmp(&right.1.modified_ms))
                .then_with(|| left.1.path.cmp(&right.1.path))
        });
        for (_, candidate) in deletable {
            if cancelled.load(Ordering::Acquire) {
                report.cancelled = true;
                report.preserved.push(candidate.path);
                continue;
            }
            if !candidate.staging && file_in_use(&root, &candidate) {
                report.preserved.push(candidate.path);
                continue;
            }
            if !same_file_snapshot(&candidate) {
                report.preserved.push(candidate.path);
                continue;
            }
            let over_budget = remaining > self.policy.max_disk_bytes;
            let expired = candidate.modified_ms < cutoff;
            if !(candidate.staging || candidate.retrievable || candidate.log) {
                report.preserved.push(candidate.path);
                continue;
            }
            if !(candidate.staging || expired || over_budget) {
                report.preserved.push(candidate.path);
                continue;
            }
            match atomic_remove(&candidate.path) {
                Ok(()) => {
                    remaining = remaining.saturating_sub(candidate.bytes);
                    let path = candidate.path;
                    after_delete(&path);
                    report.deleted.push(path);
                }
                Err(_) => report.preserved.push(candidate.path),
            }
        }
        remove_empty_dirs(&root, &root);
        report.after_bytes = remaining;
        Ok(report)
    }
}

/// Measure Agent data under an application data directory.
pub fn measure_agent_disk_usage(data_dir: &Path) -> Result<DiskUsage, RetentionError> {
    RetentionManager::for_app_data(data_dir, RetentionPolicy::default()).usage()
}

/// Delete expired or over-budget Agent data. Journals and in-use sessions stay.
pub fn cleanup_agent_data(
    data_dir: &Path,
    policy: RetentionPolicy,
    now_ms: u64,
    cancelled: &AtomicBool,
) -> Result<CleanupReport, RetentionError> {
    RetentionManager::for_app_data(data_dir, policy).cleanup(now_ms, cancelled)
}

struct ScannedFile {
    path: PathBuf,
    bytes: u64,
    modified_ms: u64,
    kind: UsageKind,
    protected: bool,
    retrievable: bool,
    log: bool,
    staging: bool,
    task_id: Option<String>,
}

fn deletion_rank(file: &ScannedFile) -> Option<u8> {
    if file.staging {
        return Some(0);
    }
    if file.protected {
        return None;
    }
    if file.retrievable {
        return Some(1);
    }
    if file.log {
        return Some(2);
    }
    None
}

impl RetentionManager {
    fn scan(&self) -> Result<Vec<ScannedFile>, RetentionError> {
        let Some(root) = canonicalize_existing(&self.root) else {
            return Ok(Vec::new());
        };
        let mut files = Vec::new();
        match self.scope {
            ScanScope::Tree => collect_tree(&root, &root, true, &mut files)?,
            ScanScope::AppData => collect_tree(&root, &root, false, &mut files)?,
        }
        Ok(files)
    }
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    inside: bool,
    output: &mut Vec<ScannedFile>,
) -> Result<(), RetentionError> {
    let Some(entries) = read_dir_optional(dir)? else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let next_inside = inside || (dir == root && is_agent_dir(&name));
            if next_inside {
                collect_tree(root, &path, true, output)?;
            }
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let include = inside || (dir == root && is_agent_file(&name));
        if !include {
            continue;
        }
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let kind = classify(&path);
        let staging = is_staging_name(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        );
        let protected = !staging && is_protected(&path, kind);
        let retrievable = !protected && matches!(kind, UsageKind::Blobs | UsageKind::Context);
        let log = !protected
            && (matches!(kind, UsageKind::HookMcp | UsageKind::Crash)
                || path.extension().and_then(|ext| ext.to_str()) == Some("log"));
        output.push(ScannedFile {
            task_id: task_id_of(root, &path),
            path,
            bytes: metadata.len(),
            modified_ms,
            kind,
            protected,
            retrievable,
            log,
            staging,
        });
    }
    Ok(())
}

fn classify(path: &Path) -> UsageKind {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if is_staging_name(name) {
        return UsageKind::Other;
    }
    if component_is(path, "agent-checkpoints") {
        return UsageKind::Other;
    }
    if name == crate::crash_log::CRASH_LOG_FILE
        || name == "crash.log"
        || component_is(path, "crash")
    {
        return UsageKind::Crash;
    }
    if is_hook_mcp(path, name) {
        return UsageKind::HookMcp;
    }
    if name == "events.jsonl" || path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        return UsageKind::Journal;
    }
    if component_is(path, "tool-output")
        || component_is(path, "context")
        || component_is(path, "agent-context")
    {
        return UsageKind::Context;
    }
    if component_is(path, "blobs") || component_is(path, "artifacts") {
        return UsageKind::Blobs;
    }
    UsageKind::Other
}

fn is_hook_mcp(path: &Path, name: &str) -> bool {
    if matches!(
        name,
        "hooks.json" | "mcp.log" | "hook-audit.json" | "hook-audit.log"
    ) || name.starts_with("hook-audit")
        || (name.starts_with("mcp") && name.ends_with(".log"))
    {
        return true;
    }
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("hooks" | "mcp" | "agent-hooks" | "agent-mcp" | "hook-audit")
        )
    })
}

fn is_protected(path: &Path, kind: UsageKind) -> bool {
    if kind == UsageKind::Journal
        || component_is(path, "agent-checkpoints")
        || component_is(path, "manifests")
    {
        return true;
    }
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            "events.jsonl"
                | "snapshot.json"
                | "summary.json"
                | "acceptance.json"
                | "recovery.json"
                | "index.json"
                | "session.lock"
                | "agent-memory.json"
        )
    )
}

fn is_agent_dir(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "agent-tasks"
                | "agent-artifacts"
                | "agent-checkpoints"
                | "agent-backends"
                | "agent-hooks"
                | "agent-mcp"
                | "agent-context"
                | "hooks"
                | "mcp"
                | "crash"
        )
    )
}

fn is_agent_file(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "Termior-crash.log"
                | "agent-memory.json"
                | "hooks.json"
                | "mcp.log"
                | "hook-audit.json"
                | "hook-audit.log"
        )
    )
}

fn component_is(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

fn task_id_of(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<&std::ffi::OsStr> = relative.components().map(|part| part.as_os_str()).collect();
    if parts.len() < 2 {
        return None;
    }
    if let Some(index) = parts
        .iter()
        .position(|part| *part == "tool-output" || *part == "context")
    {
        if index > 0 {
            return Some(parts[index - 1].to_string_lossy().into_owned());
        }
    }
    match parts[0].to_str() {
        Some("agent-tasks" | "agent-context") => Some(parts[1].to_string_lossy().into_owned()),
        Some("agent-artifacts") if parts[1] != "blobs" && parts.len() > 2 => {
            Some(parts[1].to_string_lossy().into_owned())
        }
        Some("agent-artifacts") => None,
        _ => {
            let candidate = root.join(parts[0]);
            if candidate.join("events.jsonl").is_file() || candidate.join("session.lock").is_file()
            {
                Some(parts[0].to_string_lossy().into_owned())
            } else {
                None
            }
        }
    }
}

fn file_in_use(root: &Path, file: &ScannedFile) -> bool {
    if let Some(task_id) = file.task_id.as_deref() {
        return task_in_use(root, task_id);
    }
    any_session_in_use(root)
}

fn task_in_use(root: &Path, task_id: &str) -> bool {
    lock_held(&root.join("agent-tasks").join(task_id).join("session.lock"))
        || lock_held(&root.join(task_id).join("session.lock"))
}

fn any_session_in_use(root: &Path) -> bool {
    [root.to_path_buf(), root.join("agent-tasks")]
        .into_iter()
        .any(|base| {
            let Ok(entries) = std::fs::read_dir(base) else {
                return false;
            };
            entries
                .flatten()
                .any(|entry| entry.path().is_dir() && lock_held(&entry.path().join("session.lock")))
        })
}

fn lock_held(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return true;
    };
    match file.try_lock() {
        Ok(()) => false,
        Err(_) => true,
    }
}

fn same_file_snapshot(file: &ScannedFile) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(&file.path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    metadata.len() == file.bytes && modified_ms == file.modified_ms
}

fn atomic_remove(path: &Path) -> Result<(), io::Error> {
    let staging = staging_path(path);
    std::fs::rename(path, &staging)?;
    if let Some(parent) = staging.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    // The live name is already gone. If unlinking the staging file fails, the next cleanup sweeps it.
    let _ = std::fs::remove_file(&staging);
    Ok(())
}

fn staging_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".{name}.termior-deleting-{}-{sequence}",
        std::process::id()
    ))
}

fn is_staging_name(name: &str) -> bool {
    name.contains(".termior-deleting-")
}

fn remove_empty_dirs(root: &Path, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            remove_empty_dirs(root, &path);
        }
    }
    if dir != root {
        let _ = std::fs::remove_dir(dir);
    }
}

fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn read_dir_optional(path: &Path) -> Result<Option<std::fs::ReadDir>, RetentionError> {
    match std::fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn sanitize_task_id(task_id: &str) -> Result<&str, io::Error> {
    if task_id.is_empty()
        || task_id == "."
        || task_id == ".."
        || task_id.contains(['/', '\\', '\0'])
    {
        return Err(io::Error::new(ErrorKind::InvalidInput, "invalid task id"));
    }
    Ok(task_id)
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("retention I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TaskJournal;
    use serde_json::json;
    use std::io::Write;
    use std::time::{Duration, SystemTime};

    fn policy_tight() -> RetentionPolicy {
        RetentionPolicy {
            max_age_days: u64::MAX,
            max_disk_bytes: 32,
        }
    }

    fn backdate(path: &Path) {
        let file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(5))
            .unwrap();
    }

    #[test]
    fn usage_splits_journal_blobs_context_logs_crash_and_other() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("agent-tasks/task/tool-output")).unwrap();
        std::fs::write(root.join("agent-tasks/task/events.jsonl"), b"journal").unwrap();
        std::fs::write(root.join("agent-tasks/task/snapshot.json"), b"snap").unwrap();
        std::fs::write(root.join("agent-tasks/index.json"), b"idx").unwrap();
        std::fs::create_dir_all(root.join("agent-artifacts/task/tool-output")).unwrap();
        std::fs::write(
            root.join("agent-artifacts/task/tool-output/body.txt"),
            b"context",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("agent-artifacts/blobs")).unwrap();
        std::fs::write(root.join("agent-artifacts/blobs/chunk.txt"), b"blob").unwrap();
        std::fs::create_dir_all(root.join("agent-checkpoints/blobs")).unwrap();
        std::fs::write(root.join("agent-checkpoints/blobs/hash"), b"checkpoint").unwrap();
        std::fs::create_dir_all(root.join("agent-backends")).unwrap();
        std::fs::write(root.join("agent-backends/codex-stderr.log"), b"stderr").unwrap();
        std::fs::write(root.join("hooks.json"), b"hook").unwrap();
        std::fs::write(root.join("mcp.log"), b"mcp").unwrap();
        std::fs::create_dir_all(root.join("agent-hooks")).unwrap();
        std::fs::write(root.join("agent-hooks/audit.log"), b"audit").unwrap();
        std::fs::write(root.join("Termior-crash.log"), b"crash").unwrap();
        std::fs::write(root.join("Termior-settings.json"), b"do-not-count").unwrap();
        std::fs::write(root.join("agent-memory.json"), b"memory").unwrap();

        let usage = measure_agent_disk_usage(root).unwrap();
        assert_eq!(usage.journal, b"journal".len() as u64);
        assert_eq!(usage.blobs, b"blob".len() as u64);
        assert_eq!(usage.context, b"context".len() as u64);
        assert_eq!(
            usage.hook_mcp,
            (b"hook".len() + b"mcp".len() + b"audit".len()) as u64
        );
        assert_eq!(usage.crash, b"crash".len() as u64);
        assert_eq!(
            usage.other,
            (b"snap".len() + b"idx".len() + b"checkpoint".len() + b"stderr".len() + b"memory".len())
                as u64
        );
        assert_eq!(
            usage.total(),
            usage.journal
                + usage.blobs
                + usage.context
                + usage.hook_mcp
                + usage.crash
                + usage.other
        );
        assert!(!std::fs::read(root.join("Termior-settings.json"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn cleanup_keeps_journal_bytes_and_replay_while_removing_old_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = TaskJournal::create(dir.path(), "task-1").unwrap();
        journal
            .append("output", json!({"blob": "kept-in-journal"}), true)
            .unwrap();
        let journal_path = dir.path().join("task-1/events.jsonl");
        let before = std::fs::read(&journal_path).unwrap();
        let blob = dir.path().join("task-1/tool-output/large.txt");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, vec![b'x'; 8192]).unwrap();
        std::fs::write(dir.path().join("task-1/snapshot.json"), b"snapshot").unwrap();
        std::fs::write(dir.path().join("task-1/summary.json"), b"summary").unwrap();
        std::fs::write(dir.path().join("task-1/acceptance.json"), b"accept").unwrap();

        let report = RetentionManager::new(dir.path(), policy_tight())
            .cleanup(u64::MAX, &AtomicBool::new(false))
            .unwrap();
        assert!(report.deleted.contains(&blob));
        assert!(!blob.exists());
        assert_eq!(std::fs::read(&journal_path).unwrap(), before);
        assert_eq!(
            std::fs::read(dir.path().join("task-1/snapshot.json")).unwrap(),
            b"snapshot"
        );
        assert_eq!(
            std::fs::read(dir.path().join("task-1/summary.json")).unwrap(),
            b"summary"
        );
        assert_eq!(
            std::fs::read(dir.path().join("task-1/acceptance.json")).unwrap(),
            b"accept"
        );
        let recovered = TaskJournal::recover(dir.path(), "task-1", &["output"]).unwrap();
        assert_eq!(recovered.events.len(), 1);
        assert!(report.after_bytes < report.before_bytes);
        assert!(!report.cancelled);
    }

    #[test]
    fn in_use_session_keeps_its_output_and_shared_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lease = SessionLease::acquire(root, "live").unwrap();
        assert!(SessionLease::acquire(root, "live").is_err());
        std::fs::write(root.join("live/events.jsonl"), b"live-journal").unwrap();
        std::fs::create_dir_all(root.join("live/tool-output")).unwrap();
        std::fs::write(root.join("live/tool-output/a.txt"), vec![b'a'; 5000]).unwrap();
        std::fs::create_dir_all(root.join("old/tool-output")).unwrap();
        std::fs::write(root.join("old/events.jsonl"), b"old-journal").unwrap();
        std::fs::write(root.join("old/tool-output/b.txt"), vec![b'b'; 5000]).unwrap();
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::write(root.join("blobs/shared.txt"), vec![b's'; 5000]).unwrap();

        RetentionManager::new(root, policy_tight())
            .cleanup(u64::MAX, &AtomicBool::new(false))
            .unwrap();
        assert!(root.join("live/tool-output/a.txt").exists());
        assert!(!root.join("old/tool-output/b.txt").exists());
        assert!(root.join("blobs/shared.txt").exists());
        assert_eq!(
            std::fs::read(root.join("live/events.jsonl")).unwrap(),
            b"live-journal"
        );
        assert_eq!(
            std::fs::read(root.join("old/events.jsonl")).unwrap(),
            b"old-journal"
        );
        drop(lease);
    }

    #[test]
    fn dropped_lease_allows_cleanup_of_that_session() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lease = SessionLease::acquire(root, "live").unwrap();
        std::fs::write(root.join("live/events.jsonl"), b"journal").unwrap();
        std::fs::create_dir_all(root.join("live/tool-output")).unwrap();
        let blob = root.join("live/tool-output/a.txt");
        std::fs::write(&blob, vec![b'a'; 5000]).unwrap();
        drop(lease);
        RetentionManager::new(root, policy_tight())
            .cleanup(u64::MAX, &AtomicBool::new(false))
            .unwrap();
        assert!(!blob.exists());
        assert_eq!(
            std::fs::read(root.join("live/events.jsonl")).unwrap(),
            b"journal"
        );
    }

    #[test]
    fn cancel_before_and_during_cleanup_stops_without_touching_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let journal = root.join("task/events.jsonl");
        std::fs::create_dir_all(root.join("task/tool-output")).unwrap();
        std::fs::write(&journal, b"journal").unwrap();
        let first = root.join("task/tool-output/a.txt");
        let second = root.join("task/tool-output/b.txt");
        std::fs::write(&first, vec![b'a'; 4000]).unwrap();
        std::fs::write(&second, vec![b'b'; 4000]).unwrap();
        let manager = RetentionManager::new(root, policy_tight());
        let flag = AtomicBool::new(true);
        let cancelled = manager.cleanup(u64::MAX, &flag).unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.deleted.is_empty());
        assert!(first.exists() && second.exists());

        let flag = AtomicBool::new(false);
        let partial = manager
            .cleanup_observing(u64::MAX, &flag, &mut |_| {
                flag.store(true, Ordering::Release)
            })
            .unwrap();
        assert!(partial.cancelled);
        assert_eq!(partial.deleted.len(), 1);
        assert!(first.exists() ^ second.exists());
        assert_eq!(std::fs::read(&journal).unwrap(), b"journal");
    }

    #[test]
    fn changed_or_symlinked_files_are_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("task/tool-output")).unwrap();
        std::fs::write(root.join("task/events.jsonl"), b"journal").unwrap();
        let stable = root.join("task/tool-output/a.txt");
        let growing = root.join("task/tool-output/b.txt");
        std::fs::write(&stable, vec![b'a'; 4000]).unwrap();
        std::fs::write(&growing, vec![b'b'; 4000]).unwrap();
        let stamp = UNIX_EPOCH + Duration::from_secs(50);
        OpenOptions::new()
            .write(true)
            .open(&stable)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        OpenOptions::new()
            .write(true)
            .open(&growing)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let outside = root.join("outside.txt");
        std::fs::write(&outside, b"secret-target").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, root.join("task/tool-output/link.txt")).unwrap();
        }
        let manager = RetentionManager::new(root, policy_tight());
        manager
            .cleanup_observing(u64::MAX, &AtomicBool::new(false), &mut |deleted| {
                if deleted.ends_with("a.txt") {
                    let mut file = OpenOptions::new().append(true).open(&growing).unwrap();
                    file.write_all(b"more").unwrap();
                }
            })
            .unwrap();
        assert!(!stable.exists());
        assert!(growing.exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"secret-target");
        assert_eq!(
            std::fs::read(root.join("task/events.jsonl")).unwrap(),
            b"journal"
        );
    }

    #[test]
    fn staging_leftovers_are_swept_and_app_data_keeps_settings_and_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Termior-settings.json"), b"settings").unwrap();
        std::fs::create_dir_all(root.join("agent-tasks/old/tool-output")).unwrap();
        let journal = root.join("agent-tasks/old/events.jsonl");
        std::fs::write(&journal, b"journal").unwrap();
        let blob = root.join("agent-tasks/old/tool-output/old.txt");
        std::fs::write(&blob, vec![b'x'; 3000]).unwrap();
        backdate(&blob);
        let staging = root.join("agent-tasks/old/tool-output/.old.txt.termior-deleting-1-1");
        std::fs::write(&staging, b"orphan").unwrap();
        std::fs::create_dir_all(root.join("agent-checkpoints/blobs")).unwrap();
        let checkpoint = root.join("agent-checkpoints/blobs/hash");
        std::fs::write(&checkpoint, vec![b'k'; 3000]).unwrap();
        backdate(&checkpoint);
        std::fs::write(root.join("Termior-crash.log"), vec![b'c'; 2000]).unwrap();
        backdate(&root.join("Termior-crash.log"));
        std::fs::write(root.join("hooks.json"), b"hook-log").unwrap();
        backdate(&root.join("hooks.json"));
        std::fs::create_dir_all(root.join("agent-backends")).unwrap();
        let stderr = root.join("agent-backends/codex-stderr.log");
        std::fs::write(&stderr, vec![b's'; 1500]).unwrap();
        backdate(&stderr);

        let report = cleanup_agent_data(
            root,
            RetentionPolicy {
                max_age_days: 1,
                max_disk_bytes: RetentionPolicy::MAX_DISK_BYTES,
            },
            30 * DAY_MS,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!staging.exists());
        assert!(!blob.exists());
        assert!(!root.join("Termior-crash.log").exists());
        assert!(!root.join("hooks.json").exists());
        assert!(!stderr.exists());
        assert_eq!(std::fs::read(&journal).unwrap(), b"journal");
        assert_eq!(
            std::fs::read(root.join("Termior-settings.json")).unwrap(),
            b"settings"
        );
        assert!(checkpoint.exists());
        assert!(report.deleted.iter().any(|path| path == &blob));
        assert!(!report.deleted.iter().any(|path| path == &journal));
    }

    #[test]
    fn fresh_output_under_budget_is_kept_and_missing_roots_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("task/tool-output/fresh.txt");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"fresh").unwrap();
        std::fs::write(dir.path().join("task/events.jsonl"), b"journal").unwrap();
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let report = RetentionManager::new(
            dir.path(),
            RetentionPolicy {
                max_age_days: 30,
                max_disk_bytes: 10 * MIB,
            },
        )
        .cleanup(now_ms, &AtomicBool::new(false))
        .unwrap();
        assert!(blob.exists());
        assert!(report.deleted.is_empty());

        let missing = dir.path().join("missing");
        assert!(measure_agent_disk_usage(&missing).unwrap().total() == 0);
        assert!(cleanup_agent_data(
            &missing,
            RetentionPolicy::default(),
            0,
            &AtomicBool::new(false)
        )
        .unwrap()
        .deleted
        .is_empty());
        assert!(SessionLease::acquire(dir.path(), "../escape").is_err());
        assert!(SessionLease::acquire(dir.path(), "").is_err());
    }

    #[test]
    fn age_cutoff_removes_only_expired_retrievable_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("task/tool-output")).unwrap();
        std::fs::write(root.join("task/events.jsonl"), b"journal").unwrap();
        let old = root.join("task/tool-output/old.txt");
        let fresh = root.join("task/tool-output/fresh.txt");
        std::fs::write(&old, b"old-output").unwrap();
        std::fs::write(&fresh, b"fresh-output").unwrap();
        backdate(&old);
        let report = RetentionManager::new(
            root,
            RetentionPolicy {
                max_age_days: 1,
                max_disk_bytes: RetentionPolicy::MAX_DISK_BYTES,
            },
        )
        .cleanup(30 * DAY_MS, &AtomicBool::new(false))
        .unwrap();
        assert!(report.deleted.contains(&old));
        assert!(fresh.exists());
        assert_eq!(
            std::fs::read(root.join("task/events.jsonl")).unwrap(),
            b"journal"
        );
    }
}
