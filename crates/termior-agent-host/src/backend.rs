use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

impl CapabilitySupport {
    pub fn is_supported(self) -> bool {
        self == Self::Supported
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilityProfile {
    pub session_resume: CapabilitySupport,
    pub session_fork: CapabilitySupport,
    pub mid_turn_steer: CapabilitySupport,
    pub cancel: CapabilitySupport,
    pub approval_requests: CapabilitySupport,
    pub host_terminal: CapabilitySupport,
    pub diff_events: CapabilitySupport,
    pub model_selection: CapabilitySupport,
    pub sandbox_reporting: CapabilitySupport,
}

impl CapabilityProfile {
    pub fn can_resume(&self) -> bool {
        self.session_resume.is_supported()
    }

    pub fn can_fork(&self) -> bool {
        self.session_fork.is_supported()
    }

    pub fn can_cancel(&self) -> bool {
        self.cancel.is_supported()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: String,
    pub display_name: String,
    pub version: String,
    pub authentication_source: String,
    pub model_source: String,
    pub capabilities: CapabilityProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLaunch {
    pub task_id: String,
    pub project_dir: String,
    pub environment_id: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLaunch {
    pub task_id: String,
    pub backend_session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendBinding {
    pub task_id: String,
    pub backend_session_id: String,
    pub backend_turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendEvent {
    SessionCreated {
        session_id: String,
        raw: Value,
    },
    TurnStarted {
        session_id: String,
        turn_id: String,
        raw: Value,
    },
    AgentMessageDelta {
        session_id: String,
        turn_id: String,
        item_id: String,
        text: String,
    },
    ItemStarted {
        session_id: String,
        turn_id: String,
        item_id: String,
        raw: Value,
    },
    ItemCompleted {
        session_id: String,
        turn_id: String,
        item_id: String,
        raw: Value,
    },
    ApprovalRequested {
        backend_request_id: String,
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        request_kind: String,
        raw: Value,
    },
    TurnCompleted {
        session_id: String,
        turn_id: String,
        success: bool,
        status: String,
        raw: Value,
    },
    Error {
        message: String,
        raw: Value,
    },
    Diagnostic {
        method: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendDecision {
    pub backend_request_id: String,
    pub approved: bool,
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("backend is unavailable: {0}")]
    Unavailable(String),
    #[error("backend protocol error: {0}")]
    Protocol(String),
    #[error("backend transport error: {0}")]
    Transport(String),
    #[error("backend request timed out: {0}")]
    Timeout(String),
    #[error("backend capability is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("backend exited during an active turn: {0}")]
    Exited(String),
}

pub trait AgentBackend: Send {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError>;
    fn capabilities(&self) -> CapabilityProfile;
    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError>;
    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError>;
    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError>;
    fn next_event(&mut self, timeout: Duration) -> Result<Option<BackendEvent>, BackendError>;
    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError>;
    fn cancel(&mut self, binding: &BackendBinding) -> Result<(), BackendError>;
    fn shutdown(&mut self, timeout: Duration) -> Result<(), BackendError>;
}
