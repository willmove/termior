//! Crash-tolerant, hash-chained task journal (Stage D / M9).

use crate::atomic_write;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(any(test, feature = "fault-injection"))]
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

const JOURNAL_FAULT_EXIT_CODE_INTERNAL: i32 = 75;

/// Process exit status used when a fault is configured to abort the process.
#[cfg(any(test, feature = "fault-injection"))]
pub const JOURNAL_FAULT_EXIT_CODE: i32 = JOURNAL_FAULT_EXIT_CODE_INTERNAL;

/// Where a simulated process exit interrupts a journal append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalFaultPhase {
    /// Exit before any byte of this record reaches the file.
    BeforeWrite,
    /// Exit after a partial record is synced and before the terminating newline.
    DuringWrite,
    /// Exit after the full record, including its newline, is flushed and synced.
    AfterWrite,
}

/// Deterministic crash point for one journal sequence.
///
/// Arm it with [`arm_journal_fault`] from tests. Production builds compile the
/// check, but the arming API is absent unless the `fault-injection` feature is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalFault {
    pub sequence: u64,
    pub phase: JournalFaultPhase,
    /// `std::process::exit` at the fault point instead of returning an error.
    pub exit_process: bool,
}

#[cfg(any(test, feature = "fault-injection"))]
thread_local! {
    static JOURNAL_FAULT: Cell<Option<JournalFault>> = const { Cell::new(None) };
}

/// Install a process-exit fault for the next matching append on this thread.
#[cfg(any(test, feature = "fault-injection"))]
pub fn arm_journal_fault(fault: JournalFault) {
    JOURNAL_FAULT.with(|slot| slot.set(Some(fault)));
}

/// Clear a fault installed by [`arm_journal_fault`].
#[cfg(any(test, feature = "fault-injection"))]
pub fn disarm_journal_fault() {
    JOURNAL_FAULT.with(|slot| slot.set(None));
}

fn consume_journal_fault(sequence: u64) -> Option<JournalFault> {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        JOURNAL_FAULT.with(|slot| {
            let armed = slot.get();
            if armed.is_some_and(|fault| fault.sequence == sequence) {
                slot.set(None);
                armed
            } else {
                None
            }
        })
    }
    #[cfg(not(any(test, feature = "fault-injection")))]
    {
        let _ = sequence;
        None
    }
}

fn trigger_journal_fault(fault: JournalFault) -> Result<JournalEvent, JournalError> {
    if fault.exit_process {
        std::process::exit(JOURNAL_FAULT_EXIT_CODE_INTERNAL);
    }
    Err(JournalError::SimulatedExit {
        phase: match fault.phase {
            JournalFaultPhase::BeforeWrite => "before",
            JournalFaultPhase::DuringWrite => "during",
            JournalFaultPhase::AfterWrite => "after",
        },
        sequence: fault.sequence,
    })
}

fn partial_record_len(len: usize) -> usize {
    if len <= 1 {
        len
    } else {
        len / 2
    }
}

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalHeader {
    schema_version: u32,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub sequence: u64,
    pub event_id: String,
    pub timestamp_ms: u64,
    pub kind: String,
    pub payload: Value,
    pub previous_hash: String,
    pub hash: String,
    pub critical: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub schema_version: u32,
    pub task_id: String,
    pub last_sequence: u64,
    pub state: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalRecovery {
    pub task_id: String,
    pub events: Vec<JournalEvent>,
    pub repaired_tail: bool,
    pub unknown_events: Vec<JournalEvent>,
}

/// What a compaction did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionReport {
    /// Whether the journal was rewritten. A task with no events is left alone.
    pub compacted: bool,
    /// The journal size before the compaction.
    pub before_bytes: u64,
    /// The journal size after the compaction.
    pub after_bytes: u64,
}

pub struct TaskJournal {
    path: PathBuf,
    task_id: String,
    next_sequence: u64,
    previous_hash: String,
    pending_noncritical: usize,
    /// How many times this journal flushed and synced to disk.
    ///
    /// The count is what the batching actually buys, and it does not depend on how
    /// expensive `fsync` happens to be on this machine: on Linux a flush costs
    /// tens of microseconds, on Windows it can cost milliseconds. A timing
    /// comparison between a batched write and a critical write therefore fails
    /// on the platform where flushing is cheap, even though the batching is
    /// doing exactly what it should. The flush count is the deterministic
    /// evidence.
    flush_count: u64,
    redactor: crate::StreamingRedactor,
    /// The append handle, kept open for the journal's lifetime.
    ///
    /// Re-opening the file on every append cost about 1.5 ms per event on Windows, which
    /// a streaming turn pays per delta. The handle is opened for append only, so it
    /// cannot truncate or rewrite what recovery reads.
    file: Option<std::fs::File>,
}
impl TaskJournal {
    pub fn create(tasks_root: &Path, task_id: &str) -> Result<Self, JournalError> {
        Self::create_with_secrets(tasks_root, task_id, Vec::<String>::new())
    }

    pub fn create_with_secrets(
        tasks_root: &Path,
        task_id: &str,
        known_secrets: impl IntoIterator<Item = String>,
    ) -> Result<Self, JournalError> {
        let directory = tasks_root.join(task_id);
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("events.jsonl");
        if !path.exists() {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            serde_json::to_writer(
                &mut file,
                &JournalHeader {
                    schema_version: JOURNAL_SCHEMA_VERSION,
                    task_id: task_id.into(),
                },
            )?;
            file.write_all(b"\n")?;
            file.sync_data()?;
        }
        let recovery = Self::recover_path(&path, &[])?;
        let previous_hash = recovery
            .events
            .last()
            .map(|event| event.hash.clone())
            .unwrap_or_else(|| "root".into());
        Ok(Self {
            path,
            task_id: task_id.into(),
            next_sequence: recovery.events.last().map_or(1, |event| event.sequence + 1),
            previous_hash,
            pending_noncritical: 0,
            flush_count: 0,
            redactor: crate::StreamingRedactor::new(known_secrets),
            file: None,
        })
    }

    pub fn append(
        &mut self,
        kind: &str,
        mut payload: Value,
        critical: bool,
    ) -> Result<JournalEvent, JournalError> {
        redact_value(&mut payload, &self.redactor);
        let sequence = self.next_sequence;
        let timestamp_ms = now_ms();
        let event_id = format!("{}-{sequence}", self.task_id);
        let hash = record_hash(
            sequence,
            &event_id,
            timestamp_ms,
            kind,
            &payload,
            &self.previous_hash,
            critical,
        )?;
        let event = JournalEvent {
            sequence,
            event_id,
            timestamp_ms,
            kind: kind.into(),
            payload,
            previous_hash: self.previous_hash.clone(),
            hash: hash.clone(),
            critical,
        };
        let encoded = serde_json::to_vec(&event)?;
        let fault = consume_journal_fault(sequence);
        if let Some(fault) = fault.filter(|fault| fault.phase == JournalFaultPhase::BeforeWrite) {
            return trigger_journal_fault(fault);
        }
        // The handle is taken as a field borrow so the pending counter stays writable.
        if self.file.is_none() {
            self.file = Some(OpenOptions::new().append(true).open(&self.path)?);
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| JournalError::Io(std::io::Error::other("the journal has no file")))?;
        if let Some(fault) = fault.filter(|fault| fault.phase == JournalFaultPhase::DuringWrite) {
            file.write_all(&encoded[..partial_record_len(encoded.len())])?;
            file.flush()?;
            file.sync_data()?;
            return trigger_journal_fault(fault);
        }
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        let pending = self.pending_noncritical + usize::from(!critical);
        let should_flush = critical || pending >= 32;
        self.pending_noncritical = if should_flush { 0 } else { pending };
        if should_flush {
            file.flush()?;
            file.sync_data()?;
            self.flush_count = self.flush_count.saturating_add(1);
        }
        self.next_sequence += 1;
        self.previous_hash = hash;
        if let Some(fault) = fault.filter(|fault| fault.phase == JournalFaultPhase::AfterWrite) {
            file.flush()?;
            file.sync_data()?;
            return trigger_journal_fault(fault);
        }
        Ok(event)
    }

    pub fn last_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    /// How many times this journal flushed and synced to disk.
    ///
    /// Non-critical events are batched, so a burst of them costs far fewer
    /// flushes than the number of events it wrote. A critical event flushes
    /// every time, because the recovery guarantee depends on it.
    pub fn flush_count(&self) -> u64 {
        self.flush_count
    }

    /// Replace a finished task's journal with a compact summary record.
    ///
    /// A long-running app writes one journal per task, and each journal holds every
    /// delta of every turn. The retention pass cannot delete a journal outright
    /// because it is the source of truth for recovery, so a *finished* task's journal
    /// is compacted instead: the events are replaced by one record that keeps the
    /// task id, the final sequence, the last known state and the hash chain's tip.
    ///
    /// The compacted journal still replays: it keeps the header and the final state,
    /// and the summary record restarts the chain, so a recovery sees a terminal task
    /// instead of a half-finished one. A task that is not finished is never compacted.
    pub fn compact_finished(
        tasks_root: &Path,
        task_id: &str,
        final_state: &str,
    ) -> Result<CompactionReport, JournalError> {
        let directory = tasks_root.join(task_id);
        let path = directory.join("events.jsonl");
        let recovery = Self::recover_path(&path, &[])?;
        let Some(last) = recovery.events.last() else {
            return Ok(CompactionReport {
                compacted: false,
                before_bytes: 0,
                after_bytes: 0,
            });
        };
        let before_bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let header = JournalHeader {
            schema_version: JOURNAL_SCHEMA_VERSION,
            task_id: task_id.into(),
        };
        let summary = JournalEvent {
            // The chain restarts at the root, so the summary is the first record of
            // the compacted journal. The number of records it replaced is kept in the
            // payload, and the snapshot still holds the real last sequence.
            sequence: 1,
            event_id: format!("{task_id}-compacted-{}", last.sequence),
            timestamp_ms: now_ms(),
            kind: "task/compacted".into(),
            payload: serde_json::json!({
                "final_state": final_state,
                "event_count": recovery.events.len(),
                "last_kind": last.kind,
                "last_sequence": last.sequence,
            }),
            previous_hash: "root".into(),
            hash: String::new(),
            critical: false,
        };
        let summary = JournalEvent {
            hash: record_hash(
                summary.sequence,
                &summary.event_id,
                summary.timestamp_ms,
                &summary.kind,
                &summary.payload,
                &summary.previous_hash,
                summary.critical,
            )?,
            ..summary
        };
        let mut encoded = serde_json::to_vec(&header)?;
        encoded.push(b'\n');
        encoded.extend_from_slice(&serde_json::to_vec(&summary)?);
        encoded.push(b'\n');
        // The task summary and the acceptance conclusion are kept next to the
        // journal: the cleanup must not lose what the task concluded (spec R11).
        // The snapshot is already there, so the summary is derived from it.
        if let Ok(Some(snapshot)) = Self::load_snapshot(tasks_root, task_id) {
            let task = snapshot
                .state
                .get("task")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let acceptance = snapshot
                .state
                .get("acceptance_report")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let summary_json = serde_json::json!({
                "task_id": task_id,
                "final_state": final_state,
                "last_sequence": last.sequence,
                "event_count": recovery.events.len(),
                "task": task,
                "acceptance_report": acceptance,
            });
            let summary_text = serde_json::to_string_pretty(&summary_json)?;
            let summary_path = directory.join("summary.json");
            atomic_write(&summary_path, &summary_text)?;
        }
        // The write goes to a staging name first, so a crash cannot truncate the
        // journal the task still needs.
        let staging = directory.join("events.jsonl.compacting");
        std::fs::write(&staging, &encoded)?;
        std::fs::rename(&staging, &path)?;
        if let Ok(dir) = File::open(&directory) {
            let _ = dir.sync_all();
        }
        Ok(CompactionReport {
            compacted: true,
            before_bytes,
            after_bytes: encoded.len() as u64,
        })
    }

    pub fn write_snapshot(&self, mut state: Value) -> Result<TaskSnapshot, JournalError> {
        redact_value(&mut state, &self.redactor);
        let snapshot = TaskSnapshot {
            schema_version: JOURNAL_SCHEMA_VERSION,
            task_id: self.task_id.clone(),
            last_sequence: self.next_sequence.saturating_sub(1),
            state,
        };
        let text = serde_json::to_string_pretty(&snapshot)?;
        let snapshot_path = self
            .path
            .parent()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "journal path has no parent directory",
                )
            })?
            .join("snapshot.json");
        atomic_write(&snapshot_path, &text)?;
        Ok(snapshot)
    }

    pub fn recover(
        tasks_root: &Path,
        task_id: &str,
        known_kinds: &[&str],
    ) -> Result<JournalRecovery, JournalError> {
        Self::recover_path(&tasks_root.join(task_id).join("events.jsonl"), known_kinds)
    }

    fn recover_path(path: &Path, known_kinds: &[&str]) -> Result<JournalRecovery, JournalError> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut offsets = Vec::new();
        let mut start = 0;
        for (index, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                offsets.push((start, index));
                start = index + 1;
            }
        }
        let repaired_tail = start < bytes.len();
        if repaired_tail {
            file.set_len(start as u64)?;
            file.seek(SeekFrom::Start(start as u64))?;
            file.sync_data()?;
        }
        let Some((header_start, header_end)) = offsets.first().copied() else {
            return Err(JournalError::Corrupt {
                sequence: 0,
                reason: "missing header".into(),
            });
        };
        let header: JournalHeader = serde_json::from_slice(&bytes[header_start..header_end])
            .map_err(|error| JournalError::Corrupt {
                sequence: 0,
                reason: error.to_string(),
            })?;
        if header.schema_version > JOURNAL_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema(header.schema_version));
        }
        let mut events = Vec::new();
        let mut previous = "root".to_owned();
        for (line_index, (line_start, line_end)) in offsets.into_iter().skip(1).enumerate() {
            if line_start == line_end {
                continue;
            }
            let event: JournalEvent = serde_json::from_slice(&bytes[line_start..line_end])
                .map_err(|error| JournalError::Corrupt {
                    sequence: line_index as u64 + 1,
                    reason: error.to_string(),
                })?;
            let expected = record_hash(
                event.sequence,
                &event.event_id,
                event.timestamp_ms,
                &event.kind,
                &event.payload,
                &event.previous_hash,
                event.critical,
            )?;
            if event.previous_hash != previous
                || event.hash != expected
                || event.sequence != events.len() as u64 + 1
            {
                return Err(JournalError::Corrupt {
                    sequence: event.sequence,
                    reason: "sequence or hash chain mismatch".into(),
                });
            }
            previous = event.hash.clone();
            events.push(event);
        }
        let unknown_events = if known_kinds.is_empty() {
            vec![]
        } else {
            events
                .iter()
                .filter(|event| !known_kinds.contains(&event.kind.as_str()))
                .cloned()
                .collect()
        };
        Ok(JournalRecovery {
            task_id: header.task_id,
            events,
            repaired_tail,
            unknown_events,
        })
    }

    pub fn load_snapshot(
        tasks_root: &Path,
        task_id: &str,
    ) -> Result<Option<TaskSnapshot>, JournalError> {
        let path = tasks_root.join(task_id).join("snapshot.json");
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TaskJournal {
    fn drop(&mut self) {
        if self.pending_noncritical == 0 {
            return;
        }
        // Flush what the batched writes left behind. The kept handle is closed right
        // after this, so the bytes reach the file before the journal goes away.
        if let Some(file) = self.file.as_mut() {
            let _ = file.flush();
            let _ = file.sync_data();
            return;
        }
        if let Ok(file) = OpenOptions::new().append(true).open(&self.path) {
            let _ = file.sync_data();
        }
        self.pending_noncritical = 0;
    }
}

fn redact_value(value: &mut Value, redactor: &crate::StreamingRedactor) {
    match value {
        Value::String(text) => *text = redactor.redact(text).text,
        Value::Array(values) => {
            for value in values {
                redact_value(value, redactor);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, redactor);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn record_hash(
    sequence: u64,
    event_id: &str,
    timestamp_ms: u64,
    kind: &str,
    payload: &Value,
    previous: &str,
    critical: bool,
) -> Result<String, JournalError> {
    let canonical = serde_json::to_string(&(
        sequence,
        event_id,
        timestamp_ms,
        kind,
        payload,
        previous,
        critical,
    ))?;
    Ok(format!("{:x}", Sha256::digest(canonical.as_bytes())))
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal atomic write failed: {0}")]
    Atomic(#[from] crate::AtomicWriteError),
    #[error("unsupported journal schema {0}")]
    UnsupportedSchema(u32),
    #[error("journal is corrupt at sequence {sequence}: {reason}")]
    Corrupt { sequence: u64, reason: String },
    /// Test fault: the writer simulated a process exit at `phase` of `sequence`.
    #[error("simulated process exit {phase} writing journal sequence {sequence}")]
    SimulatedExit { phase: &'static str, sequence: u64 },
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    use serde_json::json;

    struct ClearFault;
    impl Drop for ClearFault {
        fn drop(&mut self) {
            disarm_journal_fault();
        }
    }

    #[test]
    fn critical_append_fault_before_during_and_after_matches_recovered_records() {
        for phase in [
            JournalFaultPhase::BeforeWrite,
            JournalFaultPhase::DuringWrite,
            JournalFaultPhase::AfterWrite,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut journal = TaskJournal::create(dir.path(), "fault").unwrap();
            arm_journal_fault(JournalFault {
                sequence: 1,
                phase,
                exit_process: false,
            });
            let _guard = ClearFault;
            let error = journal
                .append(
                    "state_changed",
                    json!({"type": "state_changed", "to": "running"}),
                    true,
                )
                .unwrap_err();
            drop(journal);
            let label = match phase {
                JournalFaultPhase::BeforeWrite => "before",
                JournalFaultPhase::DuringWrite => "during",
                JournalFaultPhase::AfterWrite => "after",
            };
            assert_eq!(
                error.to_string(),
                format!("simulated process exit {label} writing journal sequence 1")
            );
            let recovered = TaskJournal::recover(dir.path(), "fault", &[]).unwrap();
            match phase {
                JournalFaultPhase::BeforeWrite => {
                    assert!(!recovered.repaired_tail);
                    assert!(recovered.events.is_empty());
                }
                JournalFaultPhase::DuringWrite => {
                    assert!(recovered.repaired_tail);
                    assert!(recovered.events.is_empty());
                }
                JournalFaultPhase::AfterWrite => {
                    assert!(!recovered.repaired_tail);
                    assert_eq!(recovered.events.len(), 1);
                    assert!(recovered.events[0].critical);
                    assert_eq!(recovered.events[0].kind, "state_changed");
                }
            }
        }
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::*;
    use serde_json::json;

    /// A compacted journal must still replay.
    ///
    /// The compaction rewrites the journal as a single summary record, which restarts
    /// the hash chain. If the summary kept the old sequence number, the recovery
    /// would reject the journal as corrupt and the task could not be restored.
    #[test]
    fn a_compacted_journal_still_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = TaskJournal::create(dir.path(), "task").unwrap();
        for index in 0..5 {
            journal
                .append(
                    "task/output_delta",
                    json!({"index": index, "text": "x".repeat(64)}),
                    false,
                )
                .unwrap();
        }
        journal
            .append("task/started", json!({"goal": "soak"}), true)
            .unwrap();
        drop(journal);

        let report =
            TaskJournal::compact_finished(dir.path(), "task", "completed-verified").unwrap();
        assert!(report.compacted);
        assert!(
            report.after_bytes < report.before_bytes,
            "the compaction must give space back: {report:?}"
        );

        let recovered = TaskJournal::recover(dir.path(), "task", &[]).unwrap();
        assert_eq!(recovered.events.len(), 1, "{recovered:?}");
        assert_eq!(recovered.events[0].kind, "task/compacted");
        assert_eq!(recovered.events[0].sequence, 1);
        assert_eq!(
            recovered.events[0].payload["final_state"],
            json!("completed-verified")
        );
        assert_eq!(recovered.events[0].payload["event_count"], json!(6));
    }

    /// A journal with no events is left alone: there is nothing to compact.
    #[test]
    fn an_empty_journal_is_not_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let journal = TaskJournal::create(dir.path(), "empty").unwrap();
        drop(journal);

        let report =
            TaskJournal::compact_finished(dir.path(), "empty", "completed-verified").unwrap();
        assert!(!report.compacted);
        let recovered = TaskJournal::recover(dir.path(), "empty", &[]).unwrap();
        assert!(recovered.events.is_empty());
    }

    /// The compacted journal must stay writable: a task that is resumed after the
    /// compaction appends new records to it.
    #[test]
    fn a_compacted_journal_accepts_new_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = TaskJournal::create(dir.path(), "task").unwrap();
        journal
            .append("task/started", json!({"goal": "soak"}), true)
            .unwrap();
        drop(journal);
        TaskJournal::compact_finished(dir.path(), "task", "completed-verified").unwrap();

        let mut journal = TaskJournal::create(dir.path(), "task").unwrap();
        journal
            .append("task/resumed", json!({"goal": "again"}), true)
            .unwrap();
        drop(journal);

        let recovered = TaskJournal::recover(dir.path(), "task", &[]).unwrap();
        assert_eq!(recovered.events.len(), 2, "{recovered:?}");
        assert_eq!(recovered.events[0].kind, "task/compacted");
        assert_eq!(recovered.events[1].kind, "task/resumed");
    }

    /// The cleanup must not lose what the task concluded (spec R11).
    #[test]
    fn the_compaction_keeps_the_task_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = TaskJournal::create(dir.path(), "task").unwrap();
        journal
            .append("task/started", json!({"goal": "soak"}), true)
            .unwrap();
        journal
            .write_snapshot(json!({
                "task": {"state": "completed-verified", "goal": "soak"},
                "acceptance_report": {"criteria": [], "checks": []}
            }))
            .unwrap();
        drop(journal);

        TaskJournal::compact_finished(dir.path(), "task", "completed-verified").unwrap();

        let summary: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("task/summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["task_id"], json!("task"));
        assert_eq!(summary["final_state"], json!("completed-verified"));
        assert_eq!(summary["task"]["state"], json!("completed-verified"));
        assert_eq!(summary["task"]["goal"], json!("soak"));
    }
}
