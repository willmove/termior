//! Command/event driven reliable single-agent runtime (FR-ARUN / FR-ACHG).

use crate::message::{ChatEvent, Message, Role, ToolResult};
use crate::approval::ApprovalRequest;
use crate::provider::{Provider, ProviderRequest};
use crate::task::{
    now_ms, AcceptanceCheck, AcceptanceReport, AcceptanceStatus, ApprovalPolicy, BudgetDimension,
    ChangeSetId, DecisionSource, RuntimeBudgets, RuntimeUsage, Task, TaskCommand, TaskConfig,
    TaskEvent, TaskEventKind, TaskId, TaskState, ToolAttempt, ToolDecision, ToolInvocation,
    ToolState, Turn, TurnId, WaitingReason, TASK_EVENT_SCHEMA_VERSION,
};
use crate::tools::ToolRegistry;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
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
    fn execute(
        &self,
        call_id: &str,
        tool: &str,
        normalized_arguments: &str,
        cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String>;

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
}

pub struct TaskRuntime {
    task: Task,
    approval_policy: ApprovalPolicy,
    tools: ToolRegistry,
    history: Vec<Message>,
    invocations: Vec<ToolInvocation>,
    events: Vec<TaskEvent>,
    cancellation: CancellationToken,
    started_at_ms: Option<u64>,
    active_turn_id: Option<TurnId>,
    verification_started: bool,
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
            history: Vec::new(),
            invocations: Vec::new(),
            events: Vec::new(),
            cancellation: CancellationToken::default(),
            started_at_ms: None,
            active_turn_id: None,
            verification_started: false,
        }
    }

    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
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

    pub fn pending_approval(&self) -> Option<ApprovalRequest> {
        let invocation = self
            .invocations
            .iter()
            .find(|invocation| invocation.state == ToolState::AwaitingApproval)?;
        let arguments = invocation
            .normalized_arguments
            .clone()
            .unwrap_or_else(|| invocation.raw_arguments.clone());
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
                self.review_change(
                    change_set_id,
                    &accepted_hunks,
                    provider,
                    executor,
                    on_event,
                )
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

            let assistant = self.request_assistant(provider, on_event).await?;
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
                messages: self.history.clone(),
                model: "default".into(),
                tools: self.tools.clone(),
                system_prompt_extra: None,
            };
            let mut stream = provider.stream_chat(&request);
            let mut text = String::new();
            let mut streamed_calls = Vec::new();
            let mut final_message = None;
            let mut error = None;
            let mut saw_progress = false;
            while let Some(event) = stream.next().await {
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
                    std::thread::sleep(std::time::Duration::from_millis(
                        10 * u64::from(attempt),
                    ));
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
                self.tools.validate_and_normalize(&call.name, &call.arguments)
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
            });
            self.emit(TaskEventKind::ToolQueued {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
            });
            if let Err(error) = normalized {
                let index = self.invocations.len() - 1;
                let _ = self.transition_tool(index, ToolState::Failed);
                let result = ToolResult::failure(call.id.clone(), error.to_string());
                self.invocations[index].result = Some(result.clone());
                self.add_tool_result(result);
            }
        }
    }

    /// Returns true when processing must pause for external input.
    fn process_invocation(
        &mut self,
        index: usize,
        executor: &dyn RuntimeToolExecutor,
    ) -> Result<bool, RuntimeError> {
        let state = self.invocations[index].state;
        if state == ToolState::Proposed {
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
                    self.transition_tool(index, ToolState::Approved)?;
                }
                Ok(false) => {
                    self.invocations[index].decision = Some(ToolDecision {
                        approved: true,
                        source: DecisionSource::PolicyAuto,
                        decided_at_ms: now_ms(),
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
            self.transition_tool(index, ToolState::Running)?;
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
                    let result = ToolResult::success(
                        self.invocations[index].call_id.clone(),
                        output,
                    );
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
                    let result = ToolResult::failure(
                        self.invocations[index].call_id.clone(),
                        error,
                    );
                    self.invocations[index].result = Some(result.clone());
                    self.add_tool_result(result);
                }
            }
        }
        Ok(false)
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
        match executor
            .review_change(&change_set_id, accepted_hunks, &self.cancellation)
            .map_err(RuntimeError::Invariant)?
        {
            ChangeReviewExecution::Applied(output) => {
                self.record_output_bytes(output.len() as u64);
                self.transition_tool(index, ToolState::Succeeded)?;
                let result = ToolResult::success(
                    self.invocations[index].call_id.clone(),
                    output,
                );
                self.invocations[index].result = Some(result.clone());
                self.add_tool_result(result);
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
                self.transition_tool(index, ToolState::Unknown)?;
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

    fn add_tool_result(&mut self, result: ToolResult) {
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
        fixed.or_else(|| {
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
        self.task.usage.tool_output_bytes =
            self.task.usage.tool_output_bytes.saturating_add(bytes);
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
        let mut boundary = limit;
        while !output.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        output.truncate(boundary);
        output.push_str(&format!("\n[tool output truncated: {original} bytes total]"));
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
                        .is_none_or(|check| check.status != AcceptanceStatus::Passed)
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
        let duration_ms = self.invocations[index]
            .attempts
            .last()
            .and_then(|attempt| {
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
