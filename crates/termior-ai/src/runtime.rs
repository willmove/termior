//! Command/event driven reliable single-agent runtime (FR-ARUN / FR-ACHG).

use crate::approval::ApprovalRequest;
use crate::message::{ChatEvent, Message, Role, ToolResult};
use crate::provider::{Provider, ProviderRequest};
use crate::task::{
    now_ms, AcceptanceCheck, AcceptanceReport, AcceptanceStatus, ApprovalPolicy, BudgetDimension,
    ChangeSetId, DecisionSource, RuntimeBudgets, RuntimeUsage, Task, TaskCommand, TaskConfig,
    TaskEvent, TaskEventKind, TaskId, TaskState, ToolAttempt, ToolDecision, ToolInvocation,
    ToolState, Turn, TurnId, WaitingReason, TASK_EVENT_SCHEMA_VERSION,
};
use crate::tools::ToolRegistry;
use futures::StreamExt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedRuntimeState {
    task: Task,
    history: Vec<Message>,
    invocations: Vec<ToolInvocation>,
}

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    waker: futures::task::AtomicWaker,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.waker.wake();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn cancelled(&self) -> CancellationFuture {
        CancellationFuture(self.clone())
    }
}

pub struct CancellationFuture(CancellationToken);

impl Future for CancellationFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0.is_cancelled() {
            return Poll::Ready(());
        }
        self.0 .0.waker.register(context.waker());
        if self.0.is_cancelled() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolExecution {
    Completed(String),
    ChangeProposed {
        change_set_id: ChangeSetId,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeReviewExecution {
    Applied(String),
    Conflict(String),
    Unknown(String),
}

pub trait RuntimeToolExecutor: Send + Sync {
    fn prepare(
        &self,
        _call_id: &str,
        _tool: &str,
        _normalized_arguments: &str,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn execute(
        &self,
        call_id: &str,
        tool: &str,
        normalized_arguments: &str,
        cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String>;

    fn execute_change_batch(
        &self,
        calls: &[(String, String)],
        cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        if calls.len() != 1 {
            return Err("executor does not support multi-file change sets".into());
        }
        self.execute(&calls[0].0, "write_file", &calls[0].1, cancellation)
    }

    fn review_change(
        &self,
        _change_set_id: &ChangeSetId,
        _accepted_hunks: &[usize],
        _cancellation: &CancellationToken,
    ) -> Result<ChangeReviewExecution, String> {
        Err("change review is not supported by this executor".into())
    }

    fn cancel_active(&self) -> bool {
        true
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("task command is invalid while task is {state:?}: {command}")]
    InvalidCommand { state: TaskState, command: String },
    #[error("tool call is not pending approval: {0}")]
    ApprovalNotPending(String),
    #[error("change set is not pending review: {0}")]
    ChangeReviewNotPending(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("runtime invariant failed: {0}")]
    Invariant(String),
    #[error("task cancelled")]
    Cancelled,
}

pub struct TaskRuntime {
    task: Task,
    approval_policy: ApprovalPolicy,
    tools: ToolRegistry,
    system_prompt: Option<String>,
    model: String,
    history: Vec<Message>,
    invocations: Vec<ToolInvocation>,
    events: Vec<TaskEvent>,
    cancellation: CancellationToken,
    started_at_ms: Option<u64>,
    active_turn_id: Option<TurnId>,
    verification_started: bool,
    output_store_root: Option<PathBuf>,
    output_inline_limit: usize,
    context_soft_limit_tokens: u64,
    consecutive_compactions: u32,
    max_consecutive_compactions: u32,
    journal_data_root: Option<PathBuf>,
    persistence_error: Option<String>,
}

impl TaskRuntime {
    pub fn new(
        config: TaskConfig,
        budgets: RuntimeBudgets,
        approval_policy: ApprovalPolicy,
    ) -> Self {
        let acceptance_report = AcceptanceReport::new(config.acceptance_criteria.clone());
        Self {
            task: Task {
                schema_version: TASK_EVENT_SCHEMA_VERSION,
                id: TaskId::new(),
                title: config.goal.clone(),
                goal: config.goal,
                project_dir: config.project_dir,
                backend_id: config.backend_id,
                environment_id: config.environment_id,
                created_at_ms: now_ms(),
                state: TaskState::Idle,
                waiting_reason: None,
                budgets,
                usage: RuntimeUsage::default(),
                acceptance_report,
                turns: Vec::new(),
            },
            approval_policy,
            tools: ToolRegistry::default(),
            system_prompt: None,
            model: "default".into(),
            history: Vec::new(),
            invocations: Vec::new(),
            events: Vec::new(),
            cancellation: CancellationToken::default(),
            started_at_ms: None,
            active_turn_id: None,
            verification_started: false,
            output_store_root: None,
            output_inline_limit: 32 * 1024,
            context_soft_limit_tokens: 96_000,
            consecutive_compactions: 0,
            max_consecutive_compactions: 2,
            journal_data_root: None,
            persistence_error: None,
        }
    }

    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Result<Self, String> {
        if self.task.state != TaskState::Idle
            || !self.events.is_empty()
            || !self.task.turns.is_empty()
        {
            return Err("task identity can only be fixed before execution".into());
        }
        self.task.id = TaskId(task_id.into());
        Ok(self)
    }

    pub fn with_output_store(mut self, root: impl Into<PathBuf>, inline_limit: usize) -> Self {
        self.output_store_root = Some(root.into());
        self.output_inline_limit = inline_limit.max(256);
        self
    }

    pub fn with_context_compaction(mut self, soft_limit_tokens: u64, max_consecutive: u32) -> Self {
        self.context_soft_limit_tokens = soft_limit_tokens.max(64);
        self.max_consecutive_compactions = max_consecutive.max(1);
        self
    }

    pub fn try_with_journal(mut self, data_root: impl Into<PathBuf>) -> Result<Self, String> {
        self.journal_data_root = Some(data_root.into());
        self.persist_journal(
            self.journal_data_root
                .as_deref()
                .expect("journal root was assigned"),
        )?;
        Ok(self)
    }

    pub fn recover_journal(
        data_root: &Path,
        task_id: &str,
        approval_policy: ApprovalPolicy,
        tools: ToolRegistry,
    ) -> Result<Self, String> {
        let tasks_root = data_root.join("agent-tasks");
        let recovery = termior_store::TaskJournal::recover(&tasks_root, task_id, &[])
            .map_err(|error| error.to_string())?;
        let snapshot = termior_store::TaskJournal::load_snapshot(&tasks_root, task_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "task snapshot is missing".to_owned())?;
        let (mut task, mut history, mut invocations) =
            match serde_json::from_value::<PersistedRuntimeState>(snapshot.state.clone()) {
                Ok(state) => (state.task, state.history, state.invocations),
                Err(_) => (
                    serde_json::from_value::<Task>(snapshot.state)
                        .map_err(|error| format!("task snapshot is invalid: {error}"))?,
                    vec![],
                    vec![],
                ),
            };
        let mut events = Vec::with_capacity(recovery.events.len());
        for record in recovery.events {
            let kind: TaskEventKind = serde_json::from_value(record.payload)
                .map_err(|error| format!("task event {} is invalid: {error}", record.sequence))?;
            let event = TaskEvent {
                schema_version: TASK_EVENT_SCHEMA_VERSION,
                sequence: record.sequence,
                task_id: TaskId(task_id.into()),
                turn_id: None,
                occurred_at_ms: record.timestamp_ms,
                kind,
            };
            if event.sequence > snapshot.last_sequence {
                reduce_recovered_event(&mut task, &mut history, &mut invocations, &event);
            }
            events.push(event);
        }
        let had_incomplete_task = matches!(task.state, TaskState::Running | TaskState::Cancelling);
        if had_incomplete_task {
            task.state = TaskState::Unknown;
            task.waiting_reason = None;
        }
        for invocation in &mut invocations {
            if invocation.state == ToolState::Running {
                invocation.state = ToolState::Unknown;
                invocation.state_changed_at_ms = now_ms();
            }
        }
        let active_turn_id = task
            .turns
            .last()
            .filter(|turn| turn.finished_at_ms.is_none())
            .map(|turn| turn.id.clone());
        Ok(Self {
            task,
            approval_policy,
            tools,
            system_prompt: None,
            model: "default".into(),
            history,
            invocations,
            events,
            cancellation: CancellationToken::default(),
            started_at_ms: None,
            active_turn_id,
            verification_started: false,
            output_store_root: None,
            output_inline_limit: 32 * 1024,
            context_soft_limit_tokens: 96_000,
            consecutive_compactions: 0,
            max_consecutive_compactions: 2,
            journal_data_root: Some(data_root.to_path_buf()),
            persistence_error: None,
        })
    }

    pub fn task_brief(&self) -> crate::context_engine::TaskBrief {
        use crate::context_engine::{PendingInvariant, TaskBrief};
        let pending = |invocation: &ToolInvocation| PendingInvariant {
            id: invocation.call_id.clone(),
            detail: format!(
                "{} {}",
                invocation.tool_name,
                invocation
                    .normalized_arguments
                    .as_deref()
                    .unwrap_or(&invocation.raw_arguments)
            ),
        };
        TaskBrief {
            schema_version: 1,
            goal: self.task.goal.clone(),
            acceptance: self
                .task
                .acceptance_report
                .criteria
                .iter()
                .map(|criterion| criterion.description.clone())
                .collect(),
            user_constraints: vec![],
            decisions: self
                .invocations
                .iter()
                .filter_map(|invocation| {
                    invocation.decision.as_ref().map(|decision| {
                        format!(
                            "{} approved={} source={:?}",
                            invocation.call_id, decision.approved, decision.source
                        )
                    })
                })
                .collect(),
            completed_actions: self
                .invocations
                .iter()
                .filter(|invocation| invocation.state == ToolState::Succeeded)
                .map(|invocation| format!("{}: {}", invocation.call_id, invocation.tool_name))
                .collect(),
            pending_approvals: self
                .invocations
                .iter()
                .filter(|invocation| invocation.state == ToolState::AwaitingApproval)
                .map(pending)
                .collect(),
            pending_changes: self
                .invocations
                .iter()
                .filter(|invocation| invocation.state == ToolState::AwaitingChangeReview)
                .map(pending)
                .collect(),
            unknown_side_effects: self
                .invocations
                .iter()
                .filter(|invocation| invocation.state == ToolState::Unknown)
                .map(pending)
                .collect(),
            failure_evidence: self
                .invocations
                .iter()
                .filter(|invocation| invocation.state == ToolState::Failed)
                .filter_map(|invocation| invocation.result.as_ref())
                .map(|result| truncate_utf8(&result.output, 1024).to_owned())
                .collect(),
            next_steps: vec![],
            covered_event_range: (1, self.events.len() as u64),
            summary_model: None,
        }
    }

    pub fn task(&self) -> &Task {
        &self.task
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    pub fn invocations(&self) -> &[ToolInvocation] {
        &self.invocations
    }

    pub fn events(&self) -> &[TaskEvent] {
        &self.events
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Append event deltas to the hash-chained journal and atomically snapshot the reduced task.
    /// Calling this after every handled command is idempotent by event sequence.
    pub fn persist_journal(&self, data_root: &Path) -> Result<(), String> {
        let tasks_root = data_root.join("agent-tasks");
        let mut journal = termior_store::TaskJournal::create(&tasks_root, &self.task.id.0)
            .map_err(|error| error.to_string())?;
        let persisted = journal.last_sequence();
        for event in self
            .events
            .iter()
            .filter(|event| event.sequence > persisted)
        {
            let payload = serde_json::to_value(&event.kind).map_err(|error| error.to_string())?;
            let kind = payload
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            journal
                .append(&kind, payload, task_event_is_critical(&event.kind))
                .map_err(|error| error.to_string())?;
        }
        journal
            .write_snapshot(
                serde_json::to_value(PersistedRuntimeState {
                    task: self.task.clone(),
                    history: self.history.clone(),
                    invocations: self.invocations.clone(),
                })
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn pending_approval(&self) -> Option<ApprovalRequest> {
        let invocation = self
            .invocations
            .iter()
            .find(|invocation| invocation.state == ToolState::AwaitingApproval)?;
        let exact_arguments = invocation
            .normalized_arguments
            .clone()
            .unwrap_or_else(|| invocation.raw_arguments.clone());
        let arguments = self.tools.redact_arguments_for_display(&exact_arguments);
        Some(ApprovalRequest {
            call_id: invocation.call_id.clone(),
            tool_name: invocation.tool_name.clone(),
            summary: summarize_call(&invocation.tool_name, &arguments),
            arguments,
            steps: self.task.usage.model_steps as usize,
        })
    }

    pub fn pending_contract(&self) -> Option<crate::tools::ToolContract> {
        let invocation = self
            .invocations
            .iter()
            .find(|invocation| invocation.state == ToolState::AwaitingApproval)?;
        self.tools.contract(&invocation.tool_name).ok()
    }

    pub fn pending_change_summary(&self) -> Option<&str> {
        self.invocations
            .iter()
            .find(|invocation| invocation.state == ToolState::AwaitingChangeReview)
            .and_then(|invocation| invocation.change_summary.as_deref())
    }

    pub async fn handle(
        &mut self,
        command: TaskCommand,
        provider: &dyn Provider,
        executor: &dyn RuntimeToolExecutor,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<(), RuntimeError> {
        match command {
            TaskCommand::Start { user_input } => {
                if self.task.state != TaskState::Idle && self.task.state.is_terminal() {
                    return Err(self.invalid_command("start"));
                }
                if self.task.state != TaskState::Idle {
                    return Err(self.invalid_command("start"));
                }
                self.started_at_ms = Some(now_ms());
                let turn = Turn {
                    id: TurnId::new(),
                    user_input: user_input.clone(),
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                    event_sequences: Vec::new(),
                };
                self.active_turn_id = Some(turn.id.clone());
                self.task.turns.push(turn);
                self.add_message(Message::user(user_input));
                self.set_state(TaskState::Running, None);
                self.drive(provider, executor, on_event).await
            }
            TaskCommand::ResolveApproval { call_id, approved } => {
                if self.task.state != TaskState::WaitingApproval {
                    return Err(self.invalid_command("resolve_approval"));
                }
                let index = self
                    .invocations
                    .iter()
                    .position(|invocation| {
                        invocation.call_id == call_id
                            && invocation.state == ToolState::AwaitingApproval
                    })
                    .ok_or_else(|| RuntimeError::ApprovalNotPending(call_id.clone()))?;
                let source = DecisionSource::User;
                self.invocations[index].decision = Some(ToolDecision {
                    approved,
                    source,
                    decided_at_ms: now_ms(),
                });
                self.emit(TaskEventKind::ToolDecisionRecorded {
                    call_id: call_id.clone(),
                    approved,
                    source,
                });
                if approved {
                    self.transition_tool(index, ToolState::Approved)?;
                } else {
                    self.transition_tool(index, ToolState::Denied)?;
                    let result = ToolResult::failure(&call_id, "tool call denied by user");
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
                self.set_state(TaskState::Running, None);
                self.drive(provider, executor, on_event).await
            }
            TaskCommand::ReviewChange {
                change_set_id,
                accepted_hunks,
            } => {
                self.review_change(change_set_id, &accepted_hunks, provider, executor, on_event)
                    .await
            }
            TaskCommand::RecordAcceptance { check } => {
                self.task.acceptance_report.checks.push(check);
                self.emit(TaskEventKind::AcceptanceUpdated {
                    report: self.task.acceptance_report.clone(),
                });
                Ok(())
            }
            TaskCommand::IncreaseBudgets { budgets } => {
                self.task.budgets = budgets;
                if self.task.state == TaskState::WaitingUser {
                    self.set_state(TaskState::Running, None);
                    self.drive(provider, executor, on_event).await?;
                }
                Ok(())
            }
            TaskCommand::WaitForUser { message } => {
                if self.task.state != TaskState::Running {
                    return Err(self.invalid_command("wait_for_user"));
                }
                self.set_state(
                    TaskState::WaitingUser,
                    Some(WaitingReason::User { message }),
                );
                Ok(())
            }
            TaskCommand::ContinueAfterUser => {
                if self.task.state != TaskState::WaitingUser
                    || !matches!(self.task.waiting_reason, Some(WaitingReason::User { .. }))
                {
                    return Err(self.invalid_command("continue_after_user"));
                }
                self.set_state(TaskState::Running, None);
                self.drive(provider, executor, on_event).await
            }
            TaskCommand::Cancel => {
                self.cancel(executor);
                Ok(())
            }
        }
    }

    async fn drive(
        &mut self,
        provider: &dyn Provider,
        executor: &dyn RuntimeToolExecutor,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<(), RuntimeError> {
        loop {
            self.ensure_persistence()?;
            if self.cancellation.is_cancelled() {
                self.cancel(executor);
                return Ok(());
            }
            self.update_elapsed();
            if let Some(reason) = self.exhausted_execution_budget() {
                self.set_state(TaskState::WaitingUser, Some(reason));
                return Ok(());
            }

            if let Some(index) = self.next_nonterminal_invocation() {
                if self.process_invocation(index, executor)? {
                    return Ok(());
                }
                continue;
            }

            if self.verification_started {
                match self.task.acceptance_report.completion_state() {
                    TaskState::CompletedVerified => {
                        self.finish_turn();
                        self.set_state(TaskState::CompletedVerified, None);
                        return Ok(());
                    }
                    TaskState::Failed => {
                        // Feed the failed command result back to the model before trying again.
                        self.verification_started = false;
                    }
                    TaskState::CompletedUnverified => {
                        if self.schedule_acceptance_check() {
                            continue;
                        }
                        self.finish_turn();
                        self.set_state(TaskState::CompletedUnverified, None);
                        return Ok(());
                    }
                    _ => {}
                }
            }

            if let Some(reason) = self.exhausted_model_budget() {
                self.set_state(TaskState::WaitingUser, Some(reason));
                return Ok(());
            }

            if let Err(message) = self.compact_history_if_needed() {
                self.set_state(
                    TaskState::WaitingUser,
                    Some(WaitingReason::User { message }),
                );
                return Ok(());
            }
            let assistant = match self.request_assistant(provider, on_event).await {
                Ok(assistant) => assistant,
                Err(RuntimeError::Cancelled) => {
                    self.cancel(executor);
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let has_calls = !assistant.tool_calls.is_empty();
            self.add_message(assistant.clone());
            if !has_calls {
                if self.schedule_acceptance_check() {
                    self.verification_started = true;
                    continue;
                } else {
                    let completion = self.task.acceptance_report.completion_state();
                    self.finish_turn();
                    self.set_state(completion, None);
                    return Ok(());
                }
            }
            self.enqueue_calls(&assistant.tool_calls);
        }
    }

    async fn request_assistant(
        &mut self,
        provider: &dyn Provider,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<Message, RuntimeError> {
        for attempt in 1..=3u32 {
            let request = ProviderRequest {
                messages: self.provider_messages(),
                model: self.model.clone(),
                tools: self.tools.clone(),
                system_prompt_extra: None,
            };
            let mut stream = provider.stream_chat(&request);
            let mut text = String::new();
            let mut streamed_calls = Vec::new();
            let mut final_message = None;
            let mut error = None;
            let mut saw_progress = false;
            loop {
                let next = stream.next();
                let cancelled = self.cancellation.cancelled();
                futures::pin_mut!(next, cancelled);
                let event = match futures::future::select(next, cancelled).await {
                    futures::future::Either::Left((event, _)) => event,
                    futures::future::Either::Right(((), _)) => return Err(RuntimeError::Cancelled),
                };
                let Some(event) = event else {
                    break;
                };
                on_event(&event);
                match event {
                    ChatEvent::TextDelta(delta) => {
                        saw_progress = true;
                        text.push_str(&delta);
                    }
                    ChatEvent::ToolCall(call) => {
                        saw_progress = true;
                        streamed_calls.push(call);
                    }
                    ChatEvent::Done(message) => {
                        saw_progress = true;
                        final_message = Some(message);
                    }
                    ChatEvent::Error(message) => error = Some(message),
                }
            }
            if let Some(error) = error {
                if !saw_progress && attempt < 3 {
                    self.emit(TaskEventKind::Diagnostic {
                        message: format!(
                            "provider attempt {attempt} failed before response progress; retrying"
                        ),
                    });
                    std::thread::sleep(std::time::Duration::from_millis(10 * u64::from(attempt)));
                    continue;
                }
                if saw_progress {
                    self.task.usage.model_steps += 1;
                    self.emit(TaskEventKind::BudgetUpdated {
                        usage: self.task.usage.clone(),
                    });
                }
                self.set_state(TaskState::Failed, None);
                return Err(RuntimeError::Provider(error));
            }
            self.task.usage.model_steps += 1;
            self.emit(TaskEventKind::BudgetUpdated {
                usage: self.task.usage.clone(),
            });
            return Ok(final_message.unwrap_or(Message {
                role: Role::Assistant,
                content: text,
                tool_calls: streamed_calls,
                tool_result: None,
            }));
        }
        unreachable!("provider retry loop always returns")
    }

    fn enqueue_calls(&mut self, calls: &[crate::message::ToolCall]) {
        for call in calls {
            let now = now_ms();
            let duplicate = self
                .invocations
                .iter()
                .any(|invocation| invocation.call_id == call.id);
            let normalized = if duplicate {
                Err(crate::tools::ToolError::InvalidArguments(format!(
                    "{} at $.id: duplicate call id",
                    call.name
                )))
            } else {
                self.tools
                    .validate_and_normalize(&call.name, &call.arguments)
            };
            self.invocations.push(ToolInvocation {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                raw_arguments: call.arguments.clone(),
                normalized_arguments: normalized.as_ref().ok().cloned(),
                state: ToolState::Proposed,
                decision: None,
                proposed_at_ms: now,
                state_changed_at_ms: now,
                attempts: Vec::new(),
                result: None,
                change_set_id: None,
                change_summary: None,
                acceptance_criterion_id: None,
                prepared: false,
            });
            self.emit(TaskEventKind::ToolQueued {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                raw_arguments: call.arguments.clone(),
                normalized_arguments: normalized.as_ref().ok().cloned(),
            });
            if let Err(error) = normalized {
                let index = self.invocations.len() - 1;
                let policy_denied = matches!(
                    error,
                    crate::tools::ToolError::Unknown(_) | crate::tools::ToolError::NotAllowed(_)
                );
                let terminal = if policy_denied {
                    self.invocations[index].decision = Some(ToolDecision {
                        approved: false,
                        source: DecisionSource::PolicyDeny,
                        decided_at_ms: now_ms(),
                    });
                    self.emit(TaskEventKind::ToolDecisionRecorded {
                        call_id: self.invocations[index].call_id.clone(),
                        approved: false,
                        source: DecisionSource::PolicyDeny,
                    });
                    ToolState::Denied
                } else {
                    ToolState::Failed
                };
                let _ = self.transition_tool(index, terminal);
                let result = ToolResult::failure(call.id.clone(), error.to_string());
                self.invocations[index].result = Some(result.clone());
                self.add_tool_result(result);
            }
        }
    }

    fn provider_messages(&self) -> Vec<Message> {
        let mut messages =
            Vec::with_capacity(self.history.len() + usize::from(self.system_prompt.is_some()));
        if let Some(prompt) = &self.system_prompt {
            messages.push(Message::system(prompt));
        }
        messages.extend(self.history.clone());
        messages
    }

    fn compact_history_if_needed(&mut self) -> Result<(), String> {
        let tokens = estimate_message_tokens(self.system_prompt.as_deref(), &self.history);
        if tokens <= self.context_soft_limit_tokens {
            self.consecutive_compactions = 0;
            return Ok(());
        }
        let current_user = self
            .history
            .iter()
            .rposition(|message| message.role == Role::User)
            .unwrap_or(0);
        let required_tokens = estimate_message_tokens(None, &self.history[current_user..]);
        if required_tokens > self.context_soft_limit_tokens
            && self.history.len().saturating_sub(current_user) <= 2
        {
            return Err(format!(
                "Current required input needs about {required_tokens} tokens, above the {} token context threshold",
                self.context_soft_limit_tokens
            ));
        }
        if self.consecutive_compactions >= self.max_consecutive_compactions {
            return Err(format!(
                "Context compaction repeated {} times without freeing enough space",
                self.consecutive_compactions
            ));
        }
        let keep_from = current_user.max(self.history.len().saturating_sub(8));
        let removed = self.history[..keep_from].to_vec();
        if removed.is_empty() {
            return Err(format!(
                "Active turn context needs about {tokens} tokens and cannot be compacted safely"
            ));
        }
        let narrative = removed
            .iter()
            .rev()
            .filter(|message| !message.content.trim().is_empty())
            .take(4)
            .map(|message| truncate_utf8(&message.content, 1024))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        let brief = self.task_brief();
        let compacted = crate::context_engine::Compactor::new(self.max_consecutive_compactions)
            .compact(&brief, &narrative, self.consecutive_compactions)
            .map_err(|error| error.to_string())?;
        let summary = serde_json::to_string(&compacted)
            .map_err(|error| format!("could not serialize task brief: {error}"))?;
        let mut history = vec![Message::system(format!(
            "TaskBrief reconstructed by Termior; preserve every pending invariant:\n{summary}"
        ))];
        history.extend_from_slice(&self.history[keep_from..]);
        self.history = history;
        self.consecutive_compactions = compacted.version;
        self.emit(TaskEventKind::ContextCompacted {
            covered_start: brief.covered_event_range.0,
            covered_end: brief.covered_event_range.1,
            version: compacted.version,
        });
        Ok(())
    }

    /// Returns true when processing must pause for external input.
    fn process_invocation(
        &mut self,
        index: usize,
        executor: &dyn RuntimeToolExecutor,
    ) -> Result<bool, RuntimeError> {
        let state = self.invocations[index].state;
        if state == ToolState::Proposed {
            if !self.prepare_invocation(index, executor)? {
                return Ok(false);
            }
            let tool_name = self.invocations[index].tool_name.clone();
            match self.tools.requires_approval(&tool_name) {
                Ok(true) if self.approval_policy == ApprovalPolicy::Prompt => {
                    self.transition_tool(index, ToolState::AwaitingApproval)?;
                    let call_id = self.invocations[index].call_id.clone();
                    self.set_state(
                        TaskState::WaitingApproval,
                        Some(WaitingReason::Approval { call_id }),
                    );
                    return Ok(true);
                }
                Ok(true) => {
                    self.invocations[index].decision = Some(ToolDecision {
                        approved: true,
                        source: DecisionSource::Yolo,
                        decided_at_ms: now_ms(),
                    });
                    self.emit(TaskEventKind::ToolDecisionRecorded {
                        call_id: self.invocations[index].call_id.clone(),
                        approved: true,
                        source: DecisionSource::Yolo,
                    });
                    self.transition_tool(index, ToolState::Approved)?;
                }
                Ok(false) => {
                    self.invocations[index].decision = Some(ToolDecision {
                        approved: true,
                        source: DecisionSource::PolicyAuto,
                        decided_at_ms: now_ms(),
                    });
                    self.emit(TaskEventKind::ToolDecisionRecorded {
                        call_id: self.invocations[index].call_id.clone(),
                        approved: true,
                        source: DecisionSource::PolicyAuto,
                    });
                    self.transition_tool(index, ToolState::Approved)?;
                }
                Err(error) => {
                    self.transition_tool(index, ToolState::Failed)?;
                    let result = ToolResult::failure(
                        self.invocations[index].call_id.clone(),
                        error.to_string(),
                    );
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                    return Ok(false);
                }
            }
        }
        if self.invocations[index].state == ToolState::Approved {
            if self.invocations[index].tool_name == "write_file" {
                return self.process_change_batch(index, executor);
            }
            self.transition_tool(index, ToolState::Running)?;
            self.ensure_persistence()?;
            let attempt = ToolAttempt {
                number: self.invocations[index].attempts.len() as u32 + 1,
                started_at_ms: now_ms(),
                finished_at_ms: None,
                error: None,
            };
            self.invocations[index].attempts.push(attempt);
            let tool = self.invocations[index].tool_name.clone();
            let arguments = self.invocations[index]
                .normalized_arguments
                .clone()
                .unwrap_or_else(|| self.invocations[index].raw_arguments.clone());
            let call_id = self.invocations[index].call_id.clone();
            let result = executor.execute(&call_id, &tool, &arguments, &self.cancellation);
            if let Some(attempt) = self.invocations[index].attempts.last_mut() {
                attempt.finished_at_ms = Some(now_ms());
                if let Err(error) = &result {
                    attempt.error = Some(error.clone());
                }
            }
            match result {
                Ok(ToolExecution::Completed(output)) => {
                    let output = self.limit_tool_output(index, output);
                    self.record_output_bytes(output.len() as u64);
                    self.transition_tool(index, ToolState::Succeeded)?;
                    self.record_acceptance_output(index, &output, AcceptanceStatus::Passed);
                    let result =
                        ToolResult::success(self.invocations[index].call_id.clone(), output);
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
                Ok(ToolExecution::ChangeProposed {
                    change_set_id,
                    summary,
                }) => {
                    self.invocations[index].change_set_id = Some(change_set_id.clone());
                    self.invocations[index].change_summary = Some(summary);
                    self.transition_tool(index, ToolState::AwaitingChangeReview)?;
                    let call_id = self.invocations[index].call_id.clone();
                    self.set_state(
                        TaskState::WaitingChangeReview,
                        Some(WaitingReason::ChangeReview {
                            call_id,
                            change_set_id,
                            conflict: None,
                        }),
                    );
                    return Ok(true);
                }
                Err(error) => {
                    self.transition_tool(index, ToolState::Failed)?;
                    self.record_acceptance_output(index, &error, AcceptanceStatus::Failed);
                    let result =
                        ToolResult::failure(self.invocations[index].call_id.clone(), error);
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
            }
        }
        Ok(false)
    }

    fn process_change_batch(
        &mut self,
        first: usize,
        executor: &dyn RuntimeToolExecutor,
    ) -> Result<bool, RuntimeError> {
        let mut indices = Vec::new();
        for index in first..self.invocations.len() {
            if self.invocations[index].tool_name != "write_file" {
                break;
            }
            if self.invocations[index].state != ToolState::Failed {
                indices.push(index);
            }
        }

        for &index in &indices {
            if self.invocations[index].state == ToolState::Proposed
                && !self.prepare_invocation(index, executor)?
            {
                continue;
            }
            match self.invocations[index].state {
                ToolState::Proposed if self.approval_policy == ApprovalPolicy::Prompt => {
                    self.transition_tool(index, ToolState::AwaitingApproval)?;
                    let call_id = self.invocations[index].call_id.clone();
                    self.set_state(
                        TaskState::WaitingApproval,
                        Some(WaitingReason::Approval { call_id }),
                    );
                    return Ok(true);
                }
                ToolState::Proposed => {
                    self.invocations[index].decision = Some(ToolDecision {
                        approved: true,
                        source: DecisionSource::Yolo,
                        decided_at_ms: now_ms(),
                    });
                    self.emit(TaskEventKind::ToolDecisionRecorded {
                        call_id: self.invocations[index].call_id.clone(),
                        approved: true,
                        source: DecisionSource::Yolo,
                    });
                    self.transition_tool(index, ToolState::Approved)?;
                }
                ToolState::AwaitingApproval => {
                    let call_id = self.invocations[index].call_id.clone();
                    self.set_state(
                        TaskState::WaitingApproval,
                        Some(WaitingReason::Approval { call_id }),
                    );
                    return Ok(true);
                }
                _ => {}
            }
        }

        let approved = indices
            .into_iter()
            .filter(|index| self.invocations[*index].state == ToolState::Approved)
            .collect::<Vec<_>>();
        if approved.is_empty() {
            return Ok(false);
        }
        let mut calls = Vec::with_capacity(approved.len());
        for &index in &approved {
            self.transition_tool(index, ToolState::Running)?;
            let attempt_number = self.invocations[index].attempts.len() as u32 + 1;
            self.invocations[index].attempts.push(ToolAttempt {
                number: attempt_number,
                started_at_ms: now_ms(),
                finished_at_ms: None,
                error: None,
            });
            calls.push((
                self.invocations[index].call_id.clone(),
                self.invocations[index]
                    .normalized_arguments
                    .clone()
                    .unwrap_or_else(|| self.invocations[index].raw_arguments.clone()),
            ));
        }
        self.ensure_persistence()?;
        let result = executor.execute_change_batch(&calls, &self.cancellation);
        let finished = now_ms();
        for &index in &approved {
            if let Some(attempt) = self.invocations[index].attempts.last_mut() {
                attempt.finished_at_ms = Some(finished);
                if let Err(error) = &result {
                    attempt.error = Some(error.clone());
                }
            }
        }
        match result {
            Ok(ToolExecution::ChangeProposed {
                change_set_id,
                summary,
            }) => {
                for &index in &approved {
                    self.invocations[index].change_set_id = Some(change_set_id.clone());
                    self.invocations[index].change_summary = Some(summary.clone());
                    self.transition_tool(index, ToolState::AwaitingChangeReview)?;
                }
                let call_id = self.invocations[approved[0]].call_id.clone();
                self.set_state(
                    TaskState::WaitingChangeReview,
                    Some(WaitingReason::ChangeReview {
                        call_id,
                        change_set_id,
                        conflict: None,
                    }),
                );
                Ok(true)
            }
            Ok(ToolExecution::Completed(output)) => {
                for &index in &approved {
                    self.transition_tool(index, ToolState::Succeeded)?;
                    let result = ToolResult::success(
                        self.invocations[index].call_id.clone(),
                        output.clone(),
                    );
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
                Ok(false)
            }
            Err(error) => {
                for &index in &approved {
                    self.transition_tool(index, ToolState::Failed)?;
                    let result =
                        ToolResult::failure(self.invocations[index].call_id.clone(), error.clone());
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
                Ok(false)
            }
        }
    }

    async fn review_change(
        &mut self,
        change_set_id: ChangeSetId,
        accepted_hunks: &[usize],
        provider: &dyn Provider,
        executor: &dyn RuntimeToolExecutor,
        on_event: &mut (dyn FnMut(&ChatEvent) + Send),
    ) -> Result<(), RuntimeError> {
        if self.task.state != TaskState::WaitingChangeReview {
            return Err(self.invalid_command("review_change"));
        }
        let index = self
            .invocations
            .iter()
            .position(|invocation| {
                invocation.state == ToolState::AwaitingChangeReview
                    && invocation.change_set_id.as_ref() == Some(&change_set_id)
            })
            .ok_or_else(|| RuntimeError::ChangeReviewNotPending(change_set_id.to_string()))?;
        self.ensure_persistence()?;
        match executor
            .review_change(&change_set_id, accepted_hunks, &self.cancellation)
            .map_err(RuntimeError::Invariant)?
        {
            ChangeReviewExecution::Applied(output) => {
                self.record_output_bytes(output.len() as u64);
                let indices = self
                    .invocations
                    .iter()
                    .enumerate()
                    .filter(|(_, invocation)| {
                        invocation.state == ToolState::AwaitingChangeReview
                            && invocation.change_set_id.as_ref() == Some(&change_set_id)
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                for index in indices {
                    self.transition_tool(index, ToolState::Succeeded)?;
                    let result = ToolResult::success(
                        self.invocations[index].call_id.clone(),
                        output.clone(),
                    );
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
                self.set_state(TaskState::Running, None);
                self.drive(provider, executor, on_event).await
            }
            ChangeReviewExecution::Conflict(conflict) => {
                let call_id = self.invocations[index].call_id.clone();
                self.set_state(
                    TaskState::WaitingChangeReview,
                    Some(WaitingReason::ChangeReview {
                        call_id,
                        change_set_id,
                        conflict: Some(conflict),
                    }),
                );
                Ok(())
            }
            ChangeReviewExecution::Unknown(message) => {
                let indices = self
                    .invocations
                    .iter()
                    .enumerate()
                    .filter(|(_, invocation)| {
                        invocation.state == ToolState::AwaitingChangeReview
                            && invocation.change_set_id.as_ref() == Some(&change_set_id)
                    })
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                for index in indices {
                    self.transition_tool(index, ToolState::Unknown)?;
                }
                self.set_state(TaskState::Unknown, None);
                self.emit(TaskEventKind::Diagnostic { message });
                Ok(())
            }
        }
    }

    fn cancel(&mut self, executor: &dyn RuntimeToolExecutor) {
        if self.task.state.is_terminal() {
            return;
        }
        self.set_state(TaskState::Cancelling, None);
        self.cancellation.cancel();
        for index in 0..self.invocations.len() {
            let state = self.invocations[index].state;
            if matches!(
                state,
                ToolState::Proposed
                    | ToolState::AwaitingApproval
                    | ToolState::Approved
                    | ToolState::AwaitingChangeReview
            ) {
                let _ = self.transition_tool(index, ToolState::Cancelled);
            }
        }
        if executor.cancel_active() {
            self.set_state(TaskState::Cancelled, None);
        } else {
            for index in 0..self.invocations.len() {
                if self.invocations[index].state == ToolState::Running {
                    let _ = self.transition_tool(index, ToolState::Unknown);
                }
            }
            self.set_state(TaskState::Unknown, None);
        }
        self.finish_turn();
    }

    fn next_nonterminal_invocation(&self) -> Option<usize> {
        self.invocations
            .iter()
            .position(|invocation| !invocation.state.is_terminal())
    }

    fn prepare_invocation(
        &mut self,
        index: usize,
        executor: &dyn RuntimeToolExecutor,
    ) -> Result<bool, RuntimeError> {
        if self.invocations[index].prepared {
            return Ok(true);
        }
        let arguments = self.invocations[index]
            .normalized_arguments
            .clone()
            .unwrap_or_else(|| self.invocations[index].raw_arguments.clone());
        match executor.prepare(
            &self.invocations[index].call_id,
            &self.invocations[index].tool_name,
            &arguments,
        ) {
            Ok(replacement) => {
                if let Some(replacement) = replacement {
                    let normalized = self
                        .tools
                        .validate_and_normalize(&self.invocations[index].tool_name, &replacement)
                        .map_err(|error| RuntimeError::Invariant(error.to_string()))?;
                    self.invocations[index].normalized_arguments = Some(normalized);
                    self.invocations[index].decision = None;
                    let call_id = self.invocations[index].call_id.clone();
                    self.emit(TaskEventKind::Diagnostic {
                        message: format!(
                            "pre-tool hook changed arguments for {call_id}; approval was recalculated"
                        ),
                    });
                }
                self.invocations[index].prepared = true;
                Ok(true)
            }
            Err(error) => {
                self.invocations[index].prepared = true;
                self.transition_tool(index, ToolState::Failed)?;
                let result = ToolResult::failure(
                    self.invocations[index].call_id.clone(),
                    format!("pre-tool policy rejected call: {error}"),
                );
                self.invocations[index].result = Some(result.clone());
                self.add_tool_result(result);
                Ok(false)
            }
        }
    }

    fn transition_tool(&mut self, index: usize, next: ToolState) -> Result<(), RuntimeError> {
        let from = self.invocations[index].state;
        self.invocations[index]
            .transition(next)
            .map_err(|error| RuntimeError::Invariant(error.to_string()))?;
        let call_id = self.invocations[index].call_id.clone();
        self.emit(TaskEventKind::ToolStateChanged {
            call_id,
            from,
            to: next,
        });
        Ok(())
    }

    fn add_message(&mut self, message: Message) {
        self.history.push(message.clone());
        self.emit(TaskEventKind::MessageAdded { message });
    }

    fn add_tool_result(&mut self, mut result: ToolResult) {
        if result.output.len() > self.output_inline_limit {
            if let Some(root) = self.output_store_root.as_ref() {
                let store =
                    crate::context_engine::ContextContentStore::new(root, Vec::<String>::new());
                if let Ok(reference) =
                    store.put_tool_output(&self.task.id.0, &result.call_id, &result.output)
                {
                    let original = result.output.len();
                    let preview = truncate_utf8(&result.output, self.output_inline_limit.min(4096));
                    result.output = format!(
                        "[tool output externalized: {original} bytes; complete body is available through content_ref]\n{preview}"
                    );
                    result.content_ref = Some(reference);
                }
            }
        }
        self.add_message(Message {
            role: Role::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_result: Some(result),
        });
    }

    fn set_state(&mut self, next: TaskState, waiting_reason: Option<WaitingReason>) {
        let from = self.task.state;
        self.task.state = next;
        self.task.waiting_reason = waiting_reason.clone();
        if from != next {
            self.emit(TaskEventKind::StateChanged { from, to: next });
        }
        if let Some(reason) = waiting_reason {
            self.emit(TaskEventKind::Waiting { reason });
        }
    }

    fn emit(&mut self, kind: TaskEventKind) {
        let sequence = self.events.len() as u64 + 1;
        let event = TaskEvent {
            schema_version: TASK_EVENT_SCHEMA_VERSION,
            sequence,
            task_id: self.task.id.clone(),
            turn_id: self.active_turn_id.clone(),
            occurred_at_ms: now_ms(),
            kind,
        };
        if let Some(turn) = self.task.turns.last_mut() {
            turn.event_sequences.push(sequence);
        }
        self.events.push(event);
        self.persist_latest_event();
    }

    fn persist_latest_event(&mut self) {
        let Some(data_root) = self.journal_data_root.clone() else {
            return;
        };
        let Some(event) = self.events.last().cloned() else {
            return;
        };
        let result = (|| -> Result<(), String> {
            let tasks_root = data_root.join("agent-tasks");
            let mut journal = termior_store::TaskJournal::create(&tasks_root, &self.task.id.0)
                .map_err(|error| error.to_string())?;
            if journal.last_sequence() >= event.sequence {
                return Ok(());
            }
            if journal.last_sequence() + 1 != event.sequence {
                return Err(format!(
                    "journal sequence gap: persisted {}, next runtime event {}",
                    journal.last_sequence(),
                    event.sequence
                ));
            }
            let payload = serde_json::to_value(&event.kind).map_err(|error| error.to_string())?;
            let kind = payload
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            journal
                .append(&kind, payload, task_event_is_critical(&event.kind))
                .map_err(|error| error.to_string())?;
            if task_event_is_critical(&event.kind) {
                journal
                    .write_snapshot(
                        serde_json::to_value(PersistedRuntimeState {
                            task: self.task.clone(),
                            history: self.history.clone(),
                            invocations: self.invocations.clone(),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.persistence_error = Some(error);
        }
    }

    fn ensure_persistence(&self) -> Result<(), RuntimeError> {
        if let Some(error) = self.persistence_error.as_ref() {
            Err(RuntimeError::Invariant(format!(
                "task persistence failed before side effect: {error}"
            )))
        } else {
            Ok(())
        }
    }

    fn finish_turn(&mut self) {
        if let Some(turn) = self.task.turns.last_mut() {
            turn.finished_at_ms = Some(now_ms());
        }
    }

    fn update_elapsed(&mut self) {
        if let Some(started_at) = self.started_at_ms {
            self.task.usage.wall_clock_ms = now_ms().saturating_sub(started_at);
        }
    }

    fn exhausted_execution_budget(&self) -> Option<WaitingReason> {
        let pairs = [
            (
                BudgetDimension::WallClockMs,
                self.task.usage.wall_clock_ms,
                self.task.budgets.max_wall_clock_ms,
            ),
            (
                BudgetDimension::ToolOutputBytes,
                self.task.usage.tool_output_bytes,
                self.task.budgets.max_tool_output_bytes,
            ),
        ];
        let fixed = pairs
            .into_iter()
            .find(|(_, used, limit)| *used >= *limit)
            .map(|(dimension, used, limit)| WaitingReason::BudgetExhausted {
                dimension,
                used,
                limit,
            });
        fixed
            .or_else(|| {
                optional_budget_reason(
                    BudgetDimension::InputTokens,
                    self.task.usage.input_tokens,
                    self.task.budgets.max_input_tokens,
                )
            })
            .or_else(|| {
                optional_budget_reason(
                    BudgetDimension::OutputTokens,
                    self.task.usage.output_tokens,
                    self.task.budgets.max_output_tokens,
                )
            })
    }

    fn exhausted_model_budget(&self) -> Option<WaitingReason> {
        (self.task.usage.model_steps >= self.task.budgets.max_model_steps).then_some(
            WaitingReason::BudgetExhausted {
                dimension: BudgetDimension::ModelSteps,
                used: self.task.usage.model_steps,
                limit: self.task.budgets.max_model_steps,
            },
        )
    }

    fn record_output_bytes(&mut self, bytes: u64) {
        self.task.usage.tool_output_bytes = self.task.usage.tool_output_bytes.saturating_add(bytes);
        self.emit(TaskEventKind::BudgetUpdated {
            usage: self.task.usage.clone(),
        });
    }

    fn limit_tool_output(&self, index: usize, mut output: String) -> String {
        let limit = self
            .tools
            .contract(&self.invocations[index].tool_name)
            .map(|contract| contract.max_output_bytes as usize)
            .unwrap_or(512 * 1024);
        if output.len() <= limit {
            return output;
        }
        let original = output.len();
        let notice = format!("\n[tool output truncated: {original} bytes total]");
        let mut boundary = limit.saturating_sub(notice.len());
        while !output.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        output.truncate(boundary);
        output.push_str(&notice);
        output
    }

    fn schedule_acceptance_check(&mut self) -> bool {
        let criterion = self
            .task
            .acceptance_report
            .criteria
            .iter()
            .find(|criterion| {
                criterion.command.is_some()
                    && self
                        .task
                        .acceptance_report
                        .checks
                        .iter()
                        .rev()
                        .find(|check| check.criterion_id == criterion.id)
                        .map_or(true, |check| check.status != AcceptanceStatus::Passed)
            })
            .cloned();
        let Some(criterion) = criterion else {
            return false;
        };
        let attempt = self
            .task
            .acceptance_report
            .checks
            .iter()
            .filter(|check| check.criterion_id == criterion.id)
            .count()
            + 1;
        let call_id = format!("acceptance-{}-{attempt}", criterion.id);
        let call = crate::message::ToolCall {
            id: call_id.clone(),
            name: "run_command".into(),
            arguments: serde_json::json!({
                "command": criterion.command.as_deref().unwrap_or_default()
            })
            .to_string(),
        };
        self.enqueue_calls(&[call]);
        if let Some(invocation) = self
            .invocations
            .iter_mut()
            .find(|invocation| invocation.call_id == call_id)
        {
            invocation.acceptance_criterion_id = Some(criterion.id);
        }
        true
    }

    fn record_acceptance_output(
        &mut self,
        index: usize,
        output: &str,
        fallback_status: AcceptanceStatus,
    ) {
        let Some(criterion_id) = self.invocations[index].acceptance_criterion_id.clone() else {
            return;
        };
        let exit_code = parse_exit_code(output);
        let status = match exit_code {
            Some(0) => AcceptanceStatus::Passed,
            Some(_) => AcceptanceStatus::Failed,
            None => fallback_status,
        };
        let command = self
            .task
            .acceptance_report
            .criteria
            .iter()
            .find(|criterion| criterion.id == criterion_id)
            .and_then(|criterion| criterion.command.clone());
        let duration_ms = self.invocations[index].attempts.last().and_then(|attempt| {
            attempt
                .finished_at_ms
                .map(|finished| finished.saturating_sub(attempt.started_at_ms))
        });
        self.task.acceptance_report.checks.push(AcceptanceCheck {
            criterion_id,
            status,
            command,
            exit_code,
            output_reference: Some(format!("tool:{}", self.invocations[index].call_id)),
            duration_ms,
        });
        self.emit(TaskEventKind::AcceptanceUpdated {
            report: self.task.acceptance_report.clone(),
        });
    }

    fn invalid_command(&self, command: &str) -> RuntimeError {
        RuntimeError::InvalidCommand {
            state: self.task.state,
            command: command.into(),
        }
    }
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    let mut boundary = value.len().min(limit);
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    &value[..boundary]
}

fn estimate_message_tokens(system: Option<&str>, messages: &[Message]) -> u64 {
    let characters = system.map_or(0, |value| value.chars().count())
        + messages
            .iter()
            .map(|message| {
                message.content.chars().count()
                    + message
                        .tool_calls
                        .iter()
                        .map(|call| call.arguments.chars().count() + call.name.chars().count())
                        .sum::<usize>()
                    + message
                        .tool_result
                        .as_ref()
                        .map_or(0, |result| result.output.chars().count())
            })
            .sum::<usize>();
    (characters as u64).div_ceil(4)
}

fn reduce_recovered_event(
    task: &mut Task,
    history: &mut Vec<Message>,
    invocations: &mut Vec<ToolInvocation>,
    event: &TaskEvent,
) {
    match &event.kind {
        TaskEventKind::StateChanged { to, .. } => task.state = *to,
        TaskEventKind::Waiting { reason } => task.waiting_reason = Some(reason.clone()),
        TaskEventKind::MessageAdded { message } => history.push(message.clone()),
        TaskEventKind::ToolQueued {
            call_id,
            tool_name,
            raw_arguments,
            normalized_arguments,
        } => invocations.push(ToolInvocation {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            raw_arguments: raw_arguments.clone(),
            normalized_arguments: normalized_arguments.clone(),
            state: ToolState::Proposed,
            decision: None,
            proposed_at_ms: event.occurred_at_ms,
            state_changed_at_ms: event.occurred_at_ms,
            attempts: vec![],
            result: None,
            change_set_id: None,
            change_summary: None,
            acceptance_criterion_id: None,
            prepared: false,
        }),
        TaskEventKind::ToolStateChanged { call_id, to, .. } => {
            if let Some(invocation) = invocations
                .iter_mut()
                .find(|invocation| &invocation.call_id == call_id)
            {
                invocation.state = *to;
                invocation.state_changed_at_ms = event.occurred_at_ms;
            }
        }
        TaskEventKind::ToolDecisionRecorded {
            call_id,
            approved,
            source,
        } => {
            if let Some(invocation) = invocations
                .iter_mut()
                .find(|invocation| &invocation.call_id == call_id)
            {
                invocation.decision = Some(ToolDecision {
                    approved: *approved,
                    source: *source,
                    decided_at_ms: event.occurred_at_ms,
                });
            }
        }
        TaskEventKind::BudgetUpdated { usage } => task.usage = usage.clone(),
        TaskEventKind::AcceptanceUpdated { report } => task.acceptance_report = report.clone(),
        TaskEventKind::ContextCompacted { .. } | TaskEventKind::Diagnostic { .. } => {}
    }
}

fn optional_budget_reason(
    dimension: BudgetDimension,
    used: Option<u64>,
    limit: Option<u64>,
) -> Option<WaitingReason> {
    match (used, limit) {
        (Some(used), Some(limit)) if used >= limit => Some(WaitingReason::BudgetExhausted {
            dimension,
            used,
            limit,
        }),
        _ => None,
    }
}

fn task_event_is_critical(kind: &TaskEventKind) -> bool {
    matches!(
        kind,
        TaskEventKind::StateChanged { .. }
            | TaskEventKind::Waiting { .. }
            | TaskEventKind::ToolQueued { .. }
            | TaskEventKind::ToolDecisionRecorded { .. }
            | TaskEventKind::AcceptanceUpdated { .. }
            | TaskEventKind::ToolStateChanged {
                to: ToolState::Running
                    | ToolState::Succeeded
                    | ToolState::Failed
                    | ToolState::Denied
                    | ToolState::Cancelled
                    | ToolState::Unknown
                    | ToolState::AwaitingChangeReview,
                ..
            }
    )
}

fn parse_exit_code(output: &str) -> Option<i32> {
    let value = output.strip_prefix("exit=")?.lines().next()?;
    value.trim().parse().ok()
}

fn summarize_call(tool: &str, arguments: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(arguments) {
        Ok(value) => value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| tool.to_owned(), |path| format!("{tool} {path}")),
        Err(_) => tool.to_owned(),
    }
}
