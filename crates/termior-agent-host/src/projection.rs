use crate::BackendEvent;
use termior_ai::{
    TaskEvent, TaskEventKind, TaskId, TaskState, TurnId, WaitingReason, TASK_EVENT_SCHEMA_VERSION,
};

/// Deterministically projects transport-neutral backend events into Termior task events.
pub struct BackendTaskProjector {
    task_id: TaskId,
    state: TaskState,
    sequence: u64,
}

impl BackendTaskProjector {
    pub fn new(task_id: impl Into<TaskId>) -> Self {
        Self {
            task_id: task_id.into(),
            state: TaskState::Idle,
            sequence: 0,
        }
    }

    pub fn state(&self) -> TaskState {
        self.state
    }

    pub fn apply(&mut self, event: &BackendEvent) -> Vec<TaskEvent> {
        let turn_id = backend_turn_id(event).map(TurnId);
        match event {
            BackendEvent::TurnStarted { .. } => self.change_state(TaskState::Running, turn_id),
            BackendEvent::ApprovalRequested { tool_call_id, .. } => {
                let mut events = self.change_state(TaskState::WaitingApproval, turn_id.clone());
                events.push(self.event(
                    turn_id,
                    TaskEventKind::Waiting {
                        reason: WaitingReason::Approval {
                            call_id: tool_call_id.clone(),
                        },
                    },
                ));
                events
            }
            BackendEvent::TurnCompleted { status, .. } => {
                let state = match status.as_str() {
                    "completed" => TaskState::CompletedUnverified,
                    "interrupted" => TaskState::Cancelled,
                    "failed" => TaskState::Failed,
                    _ => TaskState::Unknown,
                };
                self.change_state(state, turn_id)
            }
            BackendEvent::Error { message, .. } => {
                let mut events = self.change_state(TaskState::Failed, turn_id.clone());
                events.push(self.event(
                    turn_id,
                    TaskEventKind::Diagnostic {
                        message: message.clone(),
                    },
                ));
                events
            }
            BackendEvent::Diagnostic { method, .. } => vec![self.event(
                turn_id,
                TaskEventKind::Diagnostic {
                    message: format!("unhandled backend event: {method}"),
                },
            )],
            BackendEvent::SessionCreated { .. }
            | BackendEvent::AgentMessageDelta { .. }
            | BackendEvent::ItemStarted { .. }
            | BackendEvent::ItemCompleted { .. } => Vec::new(),
        }
    }

    fn change_state(&mut self, next: TaskState, turn_id: Option<TurnId>) -> Vec<TaskEvent> {
        if self.state == next {
            return Vec::new();
        }
        let from = self.state;
        self.state = next;
        vec![self.event(turn_id, TaskEventKind::StateChanged { from, to: next })]
    }

    fn event(&mut self, turn_id: Option<TurnId>, kind: TaskEventKind) -> TaskEvent {
        self.sequence += 1;
        TaskEvent {
            schema_version: TASK_EVENT_SCHEMA_VERSION,
            sequence: self.sequence,
            task_id: self.task_id.clone(),
            turn_id,
            occurred_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            kind,
        }
    }
}

fn backend_turn_id(event: &BackendEvent) -> Option<String> {
    match event {
        BackendEvent::TurnStarted { turn_id, .. }
        | BackendEvent::AgentMessageDelta { turn_id, .. }
        | BackendEvent::ItemStarted { turn_id, .. }
        | BackendEvent::ItemCompleted { turn_id, .. }
        | BackendEvent::ApprovalRequested { turn_id, .. }
        | BackendEvent::TurnCompleted { turn_id, .. } => Some(turn_id.clone()),
        BackendEvent::SessionCreated { .. }
        | BackendEvent::Error { .. }
        | BackendEvent::Diagnostic { .. } => None,
    }
}
