//! Stable, serializable domain model for reliable Agent tasks (FR-ARUN).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const TASK_EVENT_SCHEMA_VERSION: u32 = 1;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn new_id(prefix: &str) -> String {
    let millis = now_ms();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{millis:x}-{sequence:x}")
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new() -> Self {
                Self(new_id($prefix))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

id_type!(TaskId, "task");
id_type!(TurnId, "turn");
id_type!(ChangeSetId, "change");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskConfig {
    pub goal: String,
    pub project_dir: PathBuf,
    pub backend_id: String,
    pub environment_id: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
}

impl TaskConfig {
    pub fn new(
        goal: impl Into<String>,
        project_dir: impl Into<PathBuf>,
        backend_id: impl Into<String>,
        environment_id: impl Into<String>,
    ) -> Self {
        Self {
            goal: goal.into(),
            project_dir: project_dir.into(),
            backend_id: backend_id.into(),
            environment_id: environment_id.into(),
            acceptance_criteria: Vec::new(),
        }
    }

    pub fn with_acceptance_criteria(
        mut self,
        criteria: impl IntoIterator<Item = AcceptanceCriterion>,
    ) -> Self {
        self.acceptance_criteria = criteria.into_iter().collect();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Idle,
    Running,
    WaitingApproval,
    WaitingPlan,
    WaitingChangeReview,
    WaitingUser,
    Cancelling,
    CompletedVerified,
    CompletedUnverified,
    Failed,
    Cancelled,
    Unknown,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CompletedVerified
                | Self::CompletedUnverified
                | Self::Failed
                | Self::Cancelled
                | Self::Unknown
        )
    }

    pub fn allows(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        match self {
            Self::Idle => matches!(next, Self::Running | Self::Cancelling | Self::Cancelled),
            Self::Running => matches!(
                next,
                Self::WaitingApproval
                    | Self::WaitingPlan
                    | Self::WaitingChangeReview
                    | Self::WaitingUser
                    | Self::Cancelling
                    | Self::CompletedVerified
                    | Self::CompletedUnverified
                    | Self::Failed
                    | Self::Unknown
            ),
            Self::WaitingApproval
            | Self::WaitingPlan
            | Self::WaitingChangeReview
            | Self::WaitingUser => matches!(
                next,
                Self::Running | Self::Cancelling | Self::Failed | Self::Cancelled | Self::Unknown
            ),
            Self::Cancelling => matches!(next, Self::Cancelled | Self::Unknown | Self::Failed),
            Self::CompletedUnverified => {
                matches!(next, Self::CompletedVerified | Self::Failed)
            }
            Self::CompletedVerified | Self::Failed | Self::Cancelled | Self::Unknown => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitingReason {
    Approval {
        call_id: String,
    },
    Plan {
        turn_id: TurnId,
    },
    ChangeReview {
        call_id: String,
        change_set_id: ChangeSetId,
        conflict: Option<String>,
    },
    User {
        message: String,
    },
    BudgetExhausted {
        dimension: BudgetDimension,
        used: u64,
        limit: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    ModelSteps,
    WallClockMs,
    InputTokens,
    OutputTokens,
    ToolOutputBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub schema_version: u32,
    pub id: TaskId,
    pub title: String,
    pub goal: String,
    pub project_dir: PathBuf,
    pub backend_id: String,
    pub environment_id: String,
    pub created_at_ms: u64,
    pub state: TaskState,
    pub waiting_reason: Option<WaitingReason>,
    pub budgets: RuntimeBudgets,
    pub usage: RuntimeUsage,
    pub acceptance_report: AcceptanceReport,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

impl Task {
    pub fn transition(&mut self, next: TaskState) -> Result<(), TaskStateTransitionError> {
        if !self.state.allows(next) {
            return Err(TaskStateTransitionError {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

/// M6 persistence boundary: a restart-safe task index without mid-event recovery claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub schema_version: u32,
    pub id: TaskId,
    pub title: String,
    pub goal: String,
    pub project_dir: PathBuf,
    pub backend_id: String,
    pub environment_id: String,
    pub state: TaskState,
    pub acceptance_report: AcceptanceReport,
    pub updated_at_ms: u64,
    pub last_event_sequence: Option<u64>,
    pub legacy_session_id: Option<String>,
}

impl TaskSummary {
    pub fn from_task(task: &Task, last_event_sequence: Option<u64>) -> Self {
        Self {
            schema_version: TASK_EVENT_SCHEMA_VERSION,
            id: task.id.clone(),
            title: task.title.clone(),
            goal: task.goal.clone(),
            project_dir: task.project_dir.clone(),
            backend_id: task.backend_id.clone(),
            environment_id: task.environment_id.clone(),
            state: task.state,
            acceptance_report: task.acceptance_report.clone(),
            updated_at_ms: now_ms(),
            last_event_sequence,
            legacy_session_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummaryStore {
    pub schema_version: u32,
    #[serde(default)]
    pub tasks: Vec<TaskSummary>,
}

impl Default for TaskSummaryStore {
    fn default() -> Self {
        Self {
            schema_version: TASK_EVENT_SCHEMA_VERSION,
            tasks: Vec::new(),
        }
    }
}

impl TaskSummaryStore {
    pub fn upsert(&mut self, summary: TaskSummary) {
        if let Some(existing) = self.tasks.iter_mut().find(|task| task.id == summary.id) {
            *existing = summary;
        } else {
            self.tasks.push(summary);
        }
    }

    pub fn persist(&self, root: &std::path::Path) -> Result<PathBuf, std::io::Error> {
        let path = root.join("agent-tasks").join("index.json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        termior_store::atomic_write(&path, &json)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(path)
    }

    pub fn load(root: &std::path::Path) -> Result<Self, std::io::Error> {
        let path = root.join("agent-tasks").join("index.json");
        let json = termior_store::atomic::read_text(&path)?;
        serde_json::from_str(&json).map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid task state transition: {from:?} -> {to:?}")]
pub struct TaskStateTransitionError {
    pub from: TaskState,
    pub to: TaskState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub user_input: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    #[serde(default)]
    pub event_sequences: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Prompt,
    Yolo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBudgets {
    pub max_model_steps: u64,
    pub max_wall_clock_ms: u64,
    pub max_tool_output_bytes: u64,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

impl Default for RuntimeBudgets {
    fn default() -> Self {
        Self {
            max_model_steps: 50,
            max_wall_clock_ms: 30 * 60 * 1_000,
            max_tool_output_bytes: 8 * 1024 * 1024,
            max_input_tokens: None,
            max_output_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeUsage {
    pub model_steps: u64,
    pub wall_clock_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub tool_output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Proposed,
    AwaitingApproval,
    Approved,
    Running,
    AwaitingChangeReview,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
    Unknown,
}

impl ToolState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Cancelled | Self::Unknown
        )
    }

    pub fn allows(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Proposed,
                Self::AwaitingApproval
                    | Self::Approved
                    | Self::Denied
                    | Self::Failed
                    | Self::Cancelled
            ) | (
                Self::AwaitingApproval,
                Self::Approved | Self::Denied | Self::Cancelled
            ) | (Self::Approved, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::AwaitingChangeReview | Self::Succeeded | Self::Failed | Self::Unknown
                )
                | (
                    Self::AwaitingChangeReview,
                    Self::Succeeded | Self::Failed | Self::Cancelled | Self::Unknown
                )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    PolicyAuto,
    User,
    Yolo,
    PolicyDeny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDecision {
    pub approved: bool,
    pub source: DecisionSource,
    pub decided_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAttempt {
    pub number: u32,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub call_id: String,
    pub tool_name: String,
    pub raw_arguments: String,
    pub normalized_arguments: Option<String>,
    pub state: ToolState,
    pub decision: Option<ToolDecision>,
    pub proposed_at_ms: u64,
    pub state_changed_at_ms: u64,
    #[serde(default)]
    pub attempts: Vec<ToolAttempt>,
    pub result: Option<crate::message::ToolResult>,
    pub change_set_id: Option<ChangeSetId>,
    #[serde(default)]
    pub change_summary: Option<String>,
    #[serde(default)]
    pub acceptance_criterion_id: Option<String>,
    #[serde(default)]
    pub prepared: bool,
}

impl ToolInvocation {
    pub fn transition(&mut self, next: ToolState) -> Result<(), StateTransitionError> {
        if !self.state.allows(next) {
            return Err(StateTransitionError {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        self.state_changed_at_ms = now_ms();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid tool state transition: {from:?} -> {to:?}")]
pub struct StateTransitionError {
    pub from: ToolState,
    pub to: ToolState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub description: String,
    pub required: bool,
    pub command: Option<String>,
}

impl AcceptanceCriterion {
    pub fn command(
        id: impl Into<String>,
        description: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            required: true,
            command: Some(command.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceCheck {
    pub criterion_id: String,
    pub status: AcceptanceStatus,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_reference: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceReport {
    pub criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    pub checks: Vec<AcceptanceCheck>,
}

impl AcceptanceReport {
    pub fn new(criteria: Vec<AcceptanceCriterion>) -> Self {
        Self {
            criteria,
            checks: Vec::new(),
        }
    }

    pub fn completion_state(&self) -> TaskState {
        if self.criteria.is_empty() {
            return TaskState::CompletedUnverified;
        }
        let required = self.criteria.iter().filter(|criterion| criterion.required);
        let mut saw_required = false;
        let mut all_passed = true;
        for criterion in required {
            saw_required = true;
            let status = self
                .checks
                .iter()
                .rev()
                .find(|check| check.criterion_id == criterion.id)
                .map(|check| check.status);
            match status {
                Some(AcceptanceStatus::Passed) => {}
                Some(AcceptanceStatus::Failed) => return TaskState::Failed,
                _ => all_passed = false,
            }
        }
        if saw_required && all_passed {
            TaskState::CompletedVerified
        } else {
            TaskState::CompletedUnverified
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub schema_version: u32,
    pub sequence: u64,
    pub task_id: TaskId,
    pub turn_id: Option<TurnId>,
    pub occurred_at_ms: u64,
    pub kind: TaskEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEventKind {
    StateChanged {
        from: TaskState,
        to: TaskState,
    },
    Waiting {
        reason: WaitingReason,
    },
    MessageAdded {
        message: crate::message::Message,
    },
    ToolQueued {
        call_id: String,
        tool_name: String,
        #[serde(default)]
        raw_arguments: String,
        #[serde(default)]
        normalized_arguments: Option<String>,
    },
    ToolStateChanged {
        call_id: String,
        from: ToolState,
        to: ToolState,
    },
    ToolDecisionRecorded {
        call_id: String,
        approved: bool,
        source: DecisionSource,
    },
    BudgetUpdated {
        usage: RuntimeUsage,
    },
    AcceptanceUpdated {
        report: AcceptanceReport,
    },
    ContextCompacted {
        covered_start: u64,
        covered_end: u64,
        version: u32,
    },
    Diagnostic {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum TaskCommand {
    Start {
        user_input: String,
    },
    ResolveApproval {
        call_id: String,
        approved: bool,
    },
    ReviewChange {
        change_set_id: ChangeSetId,
        accepted_hunks: Vec<usize>,
    },
    RecordAcceptance {
        check: AcceptanceCheck,
    },
    IncreaseBudgets {
        budgets: RuntimeBudgets,
    },
    WaitForUser {
        message: String,
    },
    ContinueAfterUser,
    Cancel,
}
