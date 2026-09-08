//! Structured references from terminal output to Agent context (FR-ATERM-05).

use crate::command::{CommandSessionId, OutputCursor};
use crate::OscEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCommandRecord {
    pub id: String,
    pub terminal_id: String,
    pub command_session_id: Option<CommandSessionId>,
    pub cwd: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_start: OutputCursor,
    pub output_end: Option<OutputCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalContextReference {
    pub terminal_id: String,
    pub command_session_id: Option<CommandSessionId>,
    pub cwd: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_start: OutputCursor,
    pub output_end: OutputCursor,
    pub selected_text: Option<String>,
}

impl TerminalContextReference {
    pub fn selection(
        terminal_id: impl Into<String>,
        cwd: impl Into<String>,
        output_start: OutputCursor,
        output_end: OutputCursor,
        text: impl Into<String>,
    ) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            command_session_id: None,
            cwd: cwd.into(),
            command: None,
            exit_code: None,
            output_start,
            output_end,
            selected_text: Some(text.into()),
        }
    }
}

pub struct OscCommandTracker {
    terminal_id: String,
    cwd: String,
    next_id: u64,
    active: Option<TerminalCommandRecord>,
    records: Vec<TerminalCommandRecord>,
}

impl OscCommandTracker {
    pub fn new(terminal_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            cwd: cwd.into(),
            next_id: 1,
            active: None,
            records: Vec::new(),
        }
    }

    pub fn observe(&mut self, event: &OscEvent, cursor: OutputCursor) {
        match event {
            OscEvent::Cwd { path, .. } => self.cwd = path.clone(),
            OscEvent::CommandStart { cmd } => {
                if let Some(mut previous) = self.active.take() {
                    previous.output_end = Some(cursor);
                    self.records.push(previous);
                }
                let id = format!("{}-command-{}", self.terminal_id, self.next_id);
                self.next_id += 1;
                self.active = Some(TerminalCommandRecord {
                    id,
                    terminal_id: self.terminal_id.clone(),
                    command_session_id: None,
                    cwd: self.cwd.clone(),
                    command: (!cmd.trim().is_empty()).then(|| cmd.clone()),
                    exit_code: None,
                    output_start: cursor,
                    output_end: None,
                });
            }
            OscEvent::CommandExit { code } => {
                if let Some(mut active) = self.active.take() {
                    active.exit_code = *code;
                    active.output_end = Some(cursor);
                    self.records.push(active);
                }
            }
            OscEvent::Prompt(_) | OscEvent::AgentEvent(_) => {}
        }
    }

    pub fn records(&self) -> &[TerminalCommandRecord] {
        &self.records
    }

    pub fn active(&self) -> Option<&TerminalCommandRecord> {
        self.active.as_ref()
    }
}
