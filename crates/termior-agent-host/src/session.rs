use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    TaskLaunch, TurnLaunch,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSessionState {
    Ready,
    Running,
    WaitingApproval,
    Completed,
    Unknown,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PumpOutcome {
    Pending,
    WaitingApproval {
        backend_request_id: String,
        tool_call_id: String,
        request_kind: String,
        raw: serde_json::Value,
    },
    Completed {
        success: bool,
        status: String,
    },
    Unknown {
        reason: String,
    },
}

/// Owns one initialized backend process and its stable task/session binding.
///
/// `pump_for` is deliberately bounded. Callers can run it repeatedly on a background executor,
/// keeping UI event loops responsive while retaining the same backend process and approval IDs.
pub struct ManagedAgentSession {
    backend: Box<dyn AgentBackend>,
    info: BackendInfo,
    binding: BackendBinding,
    state: ManagedSessionState,
    pending_request_id: Option<String>,
}

impl ManagedAgentSession {
    pub fn connect(
        mut backend: Box<dyn AgentBackend>,
        launch: TaskLaunch,
    ) -> Result<Self, BackendError> {
        let info = backend.initialize()?;
        let binding = backend.create_task(launch)?;
        Ok(Self {
            backend,
            info,
            binding,
            state: ManagedSessionState::Ready,
            pending_request_id: None,
        })
    }

    pub fn info(&self) -> &BackendInfo {
        &self.info
    }

    pub fn binding(&self) -> &BackendBinding {
        &self.binding
    }

    pub fn state(&self) -> ManagedSessionState {
        self.state
    }

    pub fn start_turn(&mut self, text: impl Into<String>) -> Result<(), BackendError> {
        if matches!(
            self.state,
            ManagedSessionState::Running
                | ManagedSessionState::WaitingApproval
                | ManagedSessionState::Unknown
                | ManagedSessionState::Shutdown
        ) {
            return Err(BackendError::Protocol(format!(
                "cannot start a turn while session is {:?}",
                self.state
            )));
        }
        self.binding = self.backend.start_turn(TurnLaunch {
            task_id: self.binding.task_id.clone(),
            backend_session_id: self.binding.backend_session_id.clone(),
            text: text.into(),
        })?;
        self.pending_request_id = None;
        self.state = ManagedSessionState::Running;
        Ok(())
    }

    pub fn pump_for(
        &mut self,
        max_wait: Duration,
        on_event: &mut dyn FnMut(&BackendEvent),
    ) -> Result<PumpOutcome, BackendError> {
        if self.state != ManagedSessionState::Running {
            return Err(BackendError::Protocol(format!(
                "cannot pump a session while it is {:?}",
                self.state
            )));
        }
        let started = Instant::now();
        loop {
            let remaining = max_wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(PumpOutcome::Pending);
            }
            let poll = remaining.min(Duration::from_millis(250));
            let event = match self.backend.next_event(poll) {
                Ok(event) => event,
                Err(error) => {
                    self.state = ManagedSessionState::Unknown;
                    return Ok(PumpOutcome::Unknown {
                        reason: error.to_string(),
                    });
                }
            };
            let Some(event) = event else {
                continue;
            };
            on_event(&event);
            match event {
                BackendEvent::ApprovalRequested {
                    backend_request_id,
                    tool_call_id,
                    request_kind,
                    raw,
                    ..
                } => {
                    self.pending_request_id = Some(backend_request_id.clone());
                    self.state = ManagedSessionState::WaitingApproval;
                    return Ok(PumpOutcome::WaitingApproval {
                        backend_request_id: backend_request_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        request_kind: request_kind.clone(),
                        raw: raw.clone(),
                    });
                }
                BackendEvent::TurnCompleted {
                    success, status, ..
                } => {
                    self.state = ManagedSessionState::Completed;
                    return Ok(PumpOutcome::Completed { success, status });
                }
                BackendEvent::Error { message, .. } => {
                    self.state = ManagedSessionState::Unknown;
                    return Ok(PumpOutcome::Unknown {
                        reason: message.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    pub fn respond(
        &mut self,
        approved: bool,
        on_event: &mut dyn FnMut(&BackendEvent),
        max_wait: Duration,
    ) -> Result<PumpOutcome, BackendError> {
        if self.state != ManagedSessionState::WaitingApproval {
            return Err(BackendError::Protocol(
                "session has no pending approval request".into(),
            ));
        }
        let backend_request_id = self.pending_request_id.take().ok_or_else(|| {
            BackendError::Protocol("pending approval request lost its backend ID".into())
        })?;
        self.backend.respond(BackendDecision {
            backend_request_id,
            approved,
        })?;
        self.state = ManagedSessionState::Running;
        self.pump_for(max_wait, on_event)
    }

    pub fn cancel(&mut self) -> Result<(), BackendError> {
        if !self.info.capabilities.can_cancel() {
            return Err(BackendError::Unsupported("turn cancel"));
        }
        if !matches!(
            self.state,
            ManagedSessionState::Running | ManagedSessionState::WaitingApproval
        ) {
            return Ok(());
        }
        if let Err(error) = self.backend.cancel(&self.binding) {
            self.state = ManagedSessionState::Unknown;
            return Err(error);
        }
        self.pending_request_id = None;
        self.state = ManagedSessionState::Ready;
        Ok(())
    }

    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), BackendError> {
        if self.state == ManagedSessionState::Shutdown {
            return Ok(());
        }
        if matches!(
            self.state,
            ManagedSessionState::Running | ManagedSessionState::WaitingApproval
        ) {
            let _ = self.cancel();
        }
        self.backend.shutdown(timeout)?;
        self.state = ManagedSessionState::Shutdown;
        Ok(())
    }
}

impl Drop for ManagedAgentSession {
    fn drop(&mut self) {
        let _ = self.backend.shutdown(Duration::from_secs(2));
        self.state = ManagedSessionState::Shutdown;
    }
}
