//! Crash-tolerant, hash-chained task journal (Stage D / M9).

use crate::atomic_write;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(any(test, feature = "fault-injection"))]
use std::cell::Cell;
use std::fs::OpenOptions;
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

pub struct TaskJournal {
    path: PathBuf,
    task_id: String,
    next_sequence: u64,
    previous_hash: String,
    pending_noncritical: usize,
    redactor: crate::StreamingRedactor,
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
            redactor: crate::StreamingRedactor::new(known_secrets),
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
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        if let Some(fault) = fault.filter(|fault| fault.phase == JournalFaultPhase::DuringWrite) {
            file.write_all(&encoded[..partial_record_len(encoded.len())])?;
            file.flush()?;
            file.sync_data()?;
            return trigger_journal_fault(fault);
        }
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        self.pending_noncritical += usize::from(!critical);
        if critical || self.pending_noncritical >= 32 {
            file.flush()?;
            file.sync_data()?;
            self.pending_noncritical = 0;
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
