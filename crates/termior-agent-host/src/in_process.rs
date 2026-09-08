use crate::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    CapabilityProfile, CapabilitySupport, TaskLaunch, TurnLaunch,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use termior_ai::{
    ApprovalPolicy, ChatEvent, Provider, RuntimeBudgets, RuntimeToolExecutor, TaskCommand,
    TaskConfig, TaskRuntime, TaskState,
};

/// Adapts the built-in reliable runtime to the same host contract used by external backends.
pub struct InProcessBackend {
    provider: Arc<dyn Provider>,
    executor: Arc<dyn RuntimeToolExecutor>,
    runtimes: HashMap<String, TaskRuntime>,
    events: VecDeque<BackendEvent>,
}

impl InProcessBackend {
    pub fn new(provider: Arc<dyn Provider>, executor: Arc<dyn RuntimeToolExecutor>) -> Self {
        Self {
            provider,
            executor,
            runtimes: HashMap::new(),
            events: VecDeque::new(),
        }
    }

    fn drive(&mut self, task_id: &str, command: TaskCommand) -> Result<(), BackendError> {
        let runtime = self
            .runtimes
            .get_mut(task_id)
            .ok_or_else(|| BackendError::Protocol(format!("task is not bound: {task_id}")))?;
        let session_id = format!("builtin:{task_id}");
        let turn_id = runtime
            .task()
            .turns
            .last()
            .map(|turn| turn.id.0.clone())
            .unwrap_or_else(|| "pending".into());
        let events = &mut self.events;
        futures::executor::block_on(runtime.handle(
            command,
            self.provider.as_ref(),
            self.executor.as_ref(),
            &mut |event| match event {
                ChatEvent::TextDelta(text) => events.push_back(BackendEvent::AgentMessageDelta {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    item_id: "assistant-message".into(),
                    text: text.clone(),
                }),
                ChatEvent::Error(message) => events.push_back(BackendEvent::Error {
                    message: message.clone(),
                    raw: serde_json::Value::Null,
                }),
                ChatEvent::ToolCall(_) | ChatEvent::Done(_) => {}
            },
        ))
        .map_err(|error| BackendError::Protocol(error.to_string()))?;
        let state = runtime.task().state;
        if state.is_terminal() {
            events.push_back(BackendEvent::TurnCompleted {
                session_id,
                turn_id: runtime
                    .task()
                    .turns
                    .last()
                    .map(|turn| turn.id.0.clone())
                    .unwrap_or(turn_id),
                success: matches!(
                    state,
                    TaskState::CompletedVerified | TaskState::CompletedUnverified
                ),
                status: format!("{state:?}"),
                raw: serde_json::to_value(runtime.task())
                    .map_err(|error| BackendError::Protocol(error.to_string()))?,
            });
        }
        Ok(())
    }
}

impl AgentBackend for InProcessBackend {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError> {
        Ok(BackendInfo {
            id: "termior-built-in".into(),
            display_name: "Termior built-in Agent".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authentication_source: "Termior provider settings".into(),
            model_source: "Termior model registry".into(),
            capabilities: self.capabilities(),
        })
    }

    fn capabilities(&self) -> CapabilityProfile {
        CapabilityProfile {
            session_resume: CapabilitySupport::Supported,
            session_fork: CapabilitySupport::Unsupported,
            mid_turn_steer: CapabilitySupport::Unsupported,
            cancel: CapabilitySupport::Supported,
            approval_requests: CapabilitySupport::Supported,
            host_terminal: CapabilitySupport::Supported,
            diff_events: CapabilitySupport::Supported,
            model_selection: CapabilitySupport::Supported,
            sandbox_reporting: CapabilitySupport::Supported,
        }
    }

    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError> {
        let task_id = launch.task_id;
        let session_id = format!("builtin:{task_id}");
        let runtime = TaskRuntime::new(
            TaskConfig {
                goal: task_id.clone(),
                project_dir: launch.project_dir.into(),
                backend_id: "termior-built-in".into(),
                environment_id: launch.environment_id,
                acceptance_criteria: Vec::new(),
            },
            RuntimeBudgets::default(),
            ApprovalPolicy::Prompt,
        )
        .with_model(launch.model.unwrap_or_else(|| "default".into()));
        self.runtimes.insert(task_id.clone(), runtime);
        self.events.push_back(BackendEvent::SessionCreated {
            session_id: session_id.clone(),
            raw: serde_json::json!({"taskId":task_id}),
        });
        Ok(BackendBinding {
            task_id,
            backend_session_id: session_id,
            backend_turn_id: None,
        })
    }

    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError> {
        if self.runtimes.contains_key(&binding.task_id) {
            Ok(binding)
        } else {
            Err(BackendError::Protocol(format!(
                "task is not bound: {}",
                binding.task_id
            )))
        }
    }

    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError> {
        self.drive(
            &launch.task_id,
            TaskCommand::Start {
                user_input: launch.text,
            },
        )?;
        let turn_id = self
            .runtimes
            .get(&launch.task_id)
            .and_then(|runtime| runtime.task().turns.last())
            .map(|turn| turn.id.0.clone());
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: launch.backend_session_id,
            backend_turn_id: turn_id,
        })
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<Option<BackendEvent>, BackendError> {
        Ok(self.events.pop_front())
    }

    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError> {
        let task_id = self
            .runtimes
            .iter()
            .find(|(_, runtime)| {
                runtime
                    .pending_approval()
                    .is_some_and(|request| request.call_id == decision.backend_request_id)
            })
            .map(|(task_id, _)| task_id.clone())
            .ok_or_else(|| {
                BackendError::Protocol("approval request is no longer pending".into())
            })?;
        self.drive(
            &task_id,
            TaskCommand::ResolveApproval {
                call_id: decision.backend_request_id,
                approved: decision.approved,
            },
        )
    }

    fn cancel(&mut self, binding: &BackendBinding) -> Result<(), BackendError> {
        self.drive(&binding.task_id, TaskCommand::Cancel)
    }

    fn shutdown(&mut self, _timeout: Duration) -> Result<(), BackendError> {
        let active = self
            .runtimes
            .iter()
            .filter(|(_, runtime)| !runtime.task().state.is_terminal())
            .map(|(task_id, _)| task_id.clone())
            .collect::<Vec<_>>();
        for task_id in active {
            self.drive(&task_id, TaskCommand::Cancel)?;
        }
        Ok(())
    }
}
