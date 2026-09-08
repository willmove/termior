//! Process-independent command-session domain and deterministic test service (FR-ATERM).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CommandSessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum CommandOwner {
    Task(String),
    User,
    Detached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Controller {
    User,
    Task(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandState {
    Starting,
    Running,
    WaitingInput,
    Exited,
    Terminating,
    Terminated,
    Failed,
    Unknown,
    Released,
}

impl CommandState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Exited | Self::Terminated | Self::Failed | Self::Unknown | Self::Released
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use CommandState::*;
        matches!(
            (self, next),
            (Starting, Running | Failed | Terminating | Unknown)
                | (
                    Running,
                    WaitingInput | Exited | Terminating | Failed | Unknown
                )
                | (
                    WaitingInput,
                    Running | Exited | Terminating | Failed | Unknown
                )
                | (Terminating, Terminated | Exited | Failed | Unknown)
                | (Exited | Terminated | Failed | Unknown, Released)
        ) || self == next
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandCapabilities {
    pub stdin: bool,
    pub resize: bool,
    pub kill: bool,
    pub detach: bool,
}

impl Default for CommandCapabilities {
    fn default() -> Self {
        Self {
            stdin: true,
            resize: true,
            kill: true,
            detach: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandCreate {
    pub owner: CommandOwner,
    pub project_dir: String,
    pub environment_id: String,
    pub cwd: String,
    pub shell: String,
    pub command: String,
    pub interactive: bool,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSession {
    pub id: CommandSessionId,
    pub owner: CommandOwner,
    pub project_dir: String,
    pub environment_id: String,
    pub cwd: String,
    pub shell: String,
    pub command: String,
    pub interactive: bool,
    pub state: CommandState,
    pub controller: Controller,
    pub capabilities: CommandCapabilities,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub output_start: OutputCursor,
    pub output_end: OutputCursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OutputCursor {
    pub sequence: u64,
}

impl OutputCursor {
    pub const START: Self = Self { sequence: 0 };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
    Pty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputChunk {
    pub sequence: u64,
    pub stream: OutputStream,
    pub timestamp_ms: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRead {
    pub chunks: Vec<OutputChunk>,
    pub next: OutputCursor,
    pub truncated_before: Option<OutputCursor>,
    pub state: CommandState,
    pub exit_code: Option<i32>,
}

impl OutputRead {
    pub fn bytes(&self) -> Vec<u8> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitResult {
    pub state: CommandState,
    pub cursor: OutputCursor,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TerminalServiceError {
    #[error("command session not found: {0:?}")]
    NotFound(CommandSessionId),
    #[error("command operation is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("command controller mismatch: expected {expected:?}, received {actual:?}")]
    ControllerMismatch {
        expected: Controller,
        actual: Controller,
    },
    #[error("a running command must be killed or explicitly detached before release")]
    RunningReleaseDenied,
    #[error("illegal command state transition from {from:?} to {to:?}")]
    IllegalTransition {
        from: CommandState,
        to: CommandState,
    },
    #[error("terminal service I/O failed: {0}")]
    Io(String),
}

pub trait TerminalService: Send + Sync {
    fn create(&self, request: CommandCreate) -> Result<CommandSessionId, TerminalServiceError>;
    fn session(&self, id: &CommandSessionId) -> Result<CommandSession, TerminalServiceError>;
    fn read_output(
        &self,
        id: &CommandSessionId,
        after: Option<OutputCursor>,
        max_bytes: usize,
    ) -> Result<OutputRead, TerminalServiceError>;
    fn wait(
        &self,
        id: &CommandSessionId,
        timeout: Duration,
    ) -> Result<WaitResult, TerminalServiceError>;
    fn write_stdin(
        &self,
        id: &CommandSessionId,
        controller: &Controller,
        bytes: &[u8],
    ) -> Result<(), TerminalServiceError>;
    fn resize(
        &self,
        id: &CommandSessionId,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalServiceError>;
    fn kill(&self, id: &CommandSessionId) -> Result<(), TerminalServiceError>;
    fn release(&self, id: &CommandSessionId, detach: bool) -> Result<(), TerminalServiceError>;
    fn take_over(&self, id: &CommandSessionId) -> Result<OutputCursor, TerminalServiceError>;
    fn hand_back(
        &self,
        id: &CommandSessionId,
        controller: Controller,
    ) -> Result<OutputCursor, TerminalServiceError>;
}

struct FakeEntry {
    session: CommandSession,
    chunks: VecDeque<OutputChunk>,
    retained_bytes: usize,
    inputs: Vec<Vec<u8>>,
}

pub struct FakeTerminalService {
    next_id: AtomicU64,
    output_budget: usize,
    entries: Mutex<HashMap<CommandSessionId, FakeEntry>>,
    changed: Condvar,
}

impl Default for FakeTerminalService {
    fn default() -> Self {
        Self::with_output_budget(1024 * 1024)
    }
}

impl FakeTerminalService {
    pub fn with_output_budget(output_budget: usize) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            output_budget: output_budget.max(1),
            entries: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
        }
    }

    pub fn create_with_capabilities(
        &self,
        request: CommandCreate,
        capabilities: CommandCapabilities,
    ) -> Result<CommandSessionId, TerminalServiceError> {
        let id = CommandSessionId(format!(
            "cmd-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        ));
        let controller = match &request.owner {
            CommandOwner::Task(id) => Controller::Task(id.clone()),
            CommandOwner::User | CommandOwner::Detached => Controller::User,
        };
        let session = CommandSession {
            id: id.clone(),
            owner: request.owner,
            project_dir: request.project_dir,
            environment_id: request.environment_id,
            cwd: request.cwd,
            shell: request.shell,
            command: request.command,
            interactive: request.interactive,
            state: CommandState::Running,
            controller,
            capabilities,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            exit_code: None,
            output_start: OutputCursor::START,
            output_end: OutputCursor::START,
        };
        self.entries.lock().map_err(|_| lock_error())?.insert(
            id.clone(),
            FakeEntry {
                session,
                chunks: VecDeque::new(),
                retained_bytes: 0,
                inputs: Vec::new(),
            },
        );
        Ok(id)
    }

    pub fn push_output(
        &self,
        id: &CommandSessionId,
        stream: OutputStream,
        bytes: Vec<u8>,
    ) -> Result<OutputCursor, TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        let sequence = entry.session.output_end.sequence + 1;
        entry.retained_bytes += bytes.len();
        entry.chunks.push_back(OutputChunk {
            sequence,
            stream,
            timestamp_ms: now_ms(),
            bytes,
        });
        entry.session.output_end = OutputCursor { sequence };
        while entry.retained_bytes > self.output_budget && entry.chunks.len() > 1 {
            if let Some(removed) = entry.chunks.pop_front() {
                entry.retained_bytes = entry.retained_bytes.saturating_sub(removed.bytes.len());
                entry.session.output_start = OutputCursor {
                    sequence: removed.sequence,
                };
            }
        }
        self.changed.notify_all();
        Ok(entry.session.output_end)
    }

    pub fn finish(
        &self,
        id: &CommandSessionId,
        exit_code: i32,
    ) -> Result<(), TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        transition(&mut entry.session, CommandState::Exited)?;
        entry.session.exit_code = Some(exit_code);
        entry.session.finished_at_ms = Some(now_ms());
        self.changed.notify_all();
        Ok(())
    }

    pub fn mark_unknown(&self, id: &CommandSessionId) -> Result<(), TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        if !entry.session.state.is_terminal() {
            transition(&mut entry.session, CommandState::Unknown)?;
            entry.session.finished_at_ms = Some(now_ms());
            self.changed.notify_all();
        }
        Ok(())
    }

    fn capabilities(
        &self,
        id: &CommandSessionId,
    ) -> Result<CommandCapabilities, TerminalServiceError> {
        Ok(self.session(id)?.capabilities)
    }
}

impl TerminalService for FakeTerminalService {
    fn create(&self, request: CommandCreate) -> Result<CommandSessionId, TerminalServiceError> {
        self.create_with_capabilities(request, CommandCapabilities::default())
    }

    fn session(&self, id: &CommandSessionId) -> Result<CommandSession, TerminalServiceError> {
        self.entries
            .lock()
            .map_err(|_| lock_error())?
            .get(id)
            .map(|entry| entry.session.clone())
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))
    }

    fn read_output(
        &self,
        id: &CommandSessionId,
        after: Option<OutputCursor>,
        max_bytes: usize,
    ) -> Result<OutputRead, TerminalServiceError> {
        let entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        let requested = after.unwrap_or(entry.session.output_start);
        let truncated_before =
            (requested < entry.session.output_start).then_some(entry.session.output_start);
        let effective = requested.max(entry.session.output_start);
        let mut used = 0usize;
        let mut chunks = Vec::new();
        for chunk in entry
            .chunks
            .iter()
            .filter(|chunk| chunk.sequence > effective.sequence)
        {
            if !chunks.is_empty() && used.saturating_add(chunk.bytes.len()) > max_bytes {
                break;
            }
            used = used.saturating_add(chunk.bytes.len());
            chunks.push(chunk.clone());
        }
        let next = chunks
            .last()
            .map(|chunk| OutputCursor {
                sequence: chunk.sequence,
            })
            .unwrap_or(effective);
        Ok(OutputRead {
            chunks,
            next,
            truncated_before,
            state: entry.session.state,
            exit_code: entry.session.exit_code,
        })
    }

    fn wait(
        &self,
        id: &CommandSessionId,
        timeout: Duration,
    ) -> Result<WaitResult, TerminalServiceError> {
        let entries = self.entries.lock().map_err(|_| lock_error())?;
        if !entries.contains_key(id) {
            return Err(TerminalServiceError::NotFound(id.clone()));
        }
        let (entries, wait) = self
            .changed
            .wait_timeout_while(entries, timeout, |entries| {
                entries
                    .get(id)
                    .is_some_and(|entry| !entry.session.state.is_terminal())
            })
            .map_err(|_| lock_error())?;
        let entry = entries
            .get(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        Ok(WaitResult {
            state: entry.session.state,
            cursor: entry.session.output_end,
            exit_code: entry.session.exit_code,
            timed_out: wait.timed_out() && !entry.session.state.is_terminal(),
        })
    }

    fn write_stdin(
        &self,
        id: &CommandSessionId,
        controller: &Controller,
        bytes: &[u8],
    ) -> Result<(), TerminalServiceError> {
        if !self.capabilities(id)?.stdin {
            return Err(TerminalServiceError::Unsupported("stdin"));
        }
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        if &entry.session.controller != controller {
            return Err(TerminalServiceError::ControllerMismatch {
                expected: entry.session.controller.clone(),
                actual: controller.clone(),
            });
        }
        entry.inputs.push(bytes.to_vec());
        Ok(())
    }

    fn resize(
        &self,
        id: &CommandSessionId,
        _rows: u16,
        _cols: u16,
    ) -> Result<(), TerminalServiceError> {
        if !self.capabilities(id)?.resize {
            return Err(TerminalServiceError::Unsupported("resize"));
        }
        Ok(())
    }

    fn kill(&self, id: &CommandSessionId) -> Result<(), TerminalServiceError> {
        if !self.capabilities(id)?.kill {
            return Err(TerminalServiceError::Unsupported("kill"));
        }
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        if entry.session.state.is_terminal() {
            return Ok(());
        }
        transition(&mut entry.session, CommandState::Terminating)?;
        transition(&mut entry.session, CommandState::Terminated)?;
        entry.session.finished_at_ms = Some(now_ms());
        self.changed.notify_all();
        Ok(())
    }

    fn release(&self, id: &CommandSessionId, detach: bool) -> Result<(), TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        if !entry.session.state.is_terminal() && !detach {
            return Err(TerminalServiceError::RunningReleaseDenied);
        }
        if detach && !entry.session.capabilities.detach {
            return Err(TerminalServiceError::Unsupported("detach"));
        }
        entries.remove(id);
        Ok(())
    }

    fn take_over(&self, id: &CommandSessionId) -> Result<OutputCursor, TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        entry.session.controller = Controller::User;
        Ok(entry.session.output_end)
    }

    fn hand_back(
        &self,
        id: &CommandSessionId,
        controller: Controller,
    ) -> Result<OutputCursor, TerminalServiceError> {
        let mut entries = self.entries.lock().map_err(|_| lock_error())?;
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))?;
        entry.session.controller = controller;
        Ok(entry.session.output_end)
    }
}

fn transition(
    session: &mut CommandSession,
    next: CommandState,
) -> Result<(), TerminalServiceError> {
    if !session.state.can_transition_to(next) {
        return Err(TerminalServiceError::IllegalTransition {
            from: session.state,
            to: next,
        });
    }
    session.state = next;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn lock_error() -> TerminalServiceError {
    TerminalServiceError::Io("terminal service lock poisoned".into())
}
