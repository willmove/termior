//! Crash-tolerant, hash-chained task journal (Stage D / M9).

use crate::atomic_write;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

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
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        serde_json::to_writer(&mut file, &event)?;
        file.write_all(b"\n")?;
        self.pending_noncritical += usize::from(!critical);
        if critical || self.pending_noncritical >= 32 {
            file.flush()?;
            file.sync_data()?;
            self.pending_noncritical = 0;
        }
        self.next_sequence += 1;
        self.previous_hash = hash;
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
        atomic_write(&self.path.parent().unwrap().join("snapshot.json"), &text)?;
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
}
