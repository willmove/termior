//! 内置任务与 Codex 后端：提交、切换、实时事件与结果回写。

use super::helpers::{new_session_id, requested_skill_names, user_home_dir};
use super::*;

impl ComposerView {
    pub(super) fn cycle_custom_agent(&mut self, cx: &mut Context<Self>) {
        if self.orchestrator.submission_blocked() {
            return;
        }
        let (choice, custom) = match (self.orchestrator.backend_choice, self.active_custom_agent) {
            (AgentBackendChoice::CodexAppServer, _) => (AgentBackendChoice::BuiltIn, None),
            (AgentBackendChoice::BuiltIn, None) if !self.custom_agents.is_empty() => {
                (AgentBackendChoice::BuiltIn, Some(0))
            }
            (AgentBackendChoice::BuiltIn, Some(index)) if index + 1 < self.custom_agents.len() => {
                (AgentBackendChoice::BuiltIn, Some(index + 1))
            }
            (AgentBackendChoice::BuiltIn, _) => (AgentBackendChoice::CodexAppServer, None),
        };
        self.select_backend(choice, custom, cx);
    }

    /// 切换到指定后端（内置可带自定义 Agent 序号）；有进行中的审批/评审时不切换。
    pub(super) fn select_backend(
        &mut self,
        choice: AgentBackendChoice,
        custom: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if !self.orchestrator.try_select_backend(choice) {
            return;
        }
        self.active_custom_agent = match choice {
            AgentBackendChoice::BuiltIn => custom.filter(|index| *index < self.custom_agents.len()),
            AgentBackendChoice::CodexAppServer => None,
        };
        if let Some(full_tools) = self.full_tools.clone() {
            match self.selected_agent_runtime(&full_tools, &self.base_system_prompt) {
                Ok((tools, prompt)) => {
                    if let Some(runtime) = self.orchestrator.runtime.as_mut() {
                        runtime.tools = tools;
                        runtime.system_prompt = prompt;
                    }
                    if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
                        self.set_status(t!("composer.status.active_agent_codex"));
                    } else {
                        self.set_status(tf!(
                            "composer.status.active_agent",
                            "agent" => self.active_agent_name()
                        ));
                    }
                }
                Err(error) => self.set_status(error),
            }
        }
        cx.notify();
    }

    pub(super) fn submit(&mut self, cx: &mut Context<Self>) {
        if let Some((command, args)) = slash::parse_invocation(&self.draft.input) {
            let args = args.to_owned();
            self.invoke_slash_command(command, &args, cx);
            return;
        }
        if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
            self.submit_external(cx);
            return;
        }
        if self.orchestrator.submission_blocked() {
            return;
        }
        let Some(runtime) = self.orchestrator.runtime.clone() else {
            self.set_model_setup_status(t!("composer.status.configure_first"));
            cx.notify();
            return;
        };
        let mut rule_targets = self
            .draft
            .attachments
            .iter()
            .filter_map(|attachment| match attachment {
                Attachment::File { path } => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if rule_targets.is_empty() {
            rule_targets.push(self.workspace_root.clone());
        }
        let mut payload = match self.draft.build_payload(&runtime.tools) {
            Ok(payload) => payload,
            Err(error) => {
                self.set_status(tf!("composer.status.attachment_rejected", "error" => error));
                cx.notify();
                return;
            }
        };
        payload.text = self.snippets.expand(&payload.text);
        if !payload.images.is_empty() {
            payload.text.push_str(&format!(
                "\n\n[{} image attachment(s) are available to multimodal providers]",
                payload.images.len()
            ));
        }
        if payload.text.trim().is_empty() {
            return;
        }
        let user_input = payload.text;
        let terminal_snapshot = self.terminal_context.snapshot();
        let redactor = termior_store::StreamingRedactor::new(Vec::<String>::new());
        let mut goal_item = ContextItem::inline(
            "turn-user-input",
            ContextCategory::Invariant,
            ContextSource::User,
            user_input.clone(),
            u16::MAX,
        );
        goal_item.required = true;
        let mut context_items = vec![
            goal_item,
            ContextItem::inline(
                "system-and-rules",
                ContextCategory::Invariant,
                ContextSource::System,
                redactor.redact(&runtime.system_prompt).text,
                u16::MAX - 1,
            ),
            ContextItem::inline(
                "active-terminal-evidence",
                ContextCategory::Evidence,
                ContextSource::Terminal(
                    terminal_snapshot
                        .command_reference
                        .as_ref()
                        .map(|record| record.terminal_id.clone())
                        .unwrap_or_else(|| "active".into()),
                ),
                redactor.redact(&terminal_snapshot.recent_output).text,
                100,
            ),
        ];
        let rules = match RuleResolver::new(&self.workspace_root).and_then(|resolver| {
            let mut unique = BTreeMap::new();
            for target in &rule_targets {
                for rule in resolver.for_path(target)? {
                    unique.insert(rule.path.clone(), rule);
                }
            }
            Ok(unique.into_values().collect::<Vec<_>>())
        }) {
            Ok(rules) => rules,
            Err(error) => {
                self.set_status(tf!("composer.status.rules_failed", "error" => error));
                cx.emit(AgentStatus::Error);
                cx.notify();
                return;
            }
        };
        for (index, rule) in rules.into_iter().enumerate() {
            let mut item = ContextItem::inline(
                format!("rule-{index}-{}", rule.digest),
                ContextCategory::Rules,
                ContextSource::Workspace(rule.path.clone()),
                redactor.redact(&rule.content).text,
                u16::MAX - 2,
            );
            item.required = true;
            context_items.push(item);
        }
        let requested_skills = requested_skill_names(&user_input, &self.skills);
        self.active_skills.clear();
        if !requested_skills.is_empty() {
            let user_skills_root = user_home_dir().map(|home| home.join(".agents").join("skills"));
            let index =
                match SkillIndex::scan_sources(&self.workspace_root, user_skills_root.as_deref()) {
                    Ok(index) => index,
                    Err(error) => {
                        self.set_status(tf!("composer.status.skill_failed", "error" => error));
                        cx.emit(AgentStatus::Error);
                        cx.notify();
                        return;
                    }
                };
            let task_tools = runtime
                .tools
                .contracts()
                .into_iter()
                .map(|contract| contract.name)
                .collect();
            for name in requested_skills {
                let activated = match index.activate(&name, &task_tools) {
                    Ok(activated) => activated,
                    Err(error) => {
                        self.set_status(tf!(
                            "composer.status.skill_named_failed",
                            "name" => name,
                            "error" => error
                        ));
                        cx.emit(AgentStatus::Error);
                        cx.notify();
                        return;
                    }
                };
                let mut item = ContextItem::inline(
                    format!("skill:{}", activated.metadata.name),
                    ContextCategory::Skill,
                    ContextSource::Skill(activated.metadata.name.clone()),
                    format!(
                        "Allowed tool ceiling for this skill: {}\n\n{}",
                        activated
                            .effective_tools
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", "),
                        redactor.redact(&activated.body).text
                    ),
                    u16::MAX - 3,
                );
                item.required = true;
                context_items.push(item);
                self.active_skills.push(name);
            }
        }
        let assembler = ContextAssembler::new(
            128_000,
            BTreeMap::from([
                (ContextCategory::Invariant, 96_000),
                (ContextCategory::Rules, 24_000),
                (ContextCategory::Skill, 24_000),
                (ContextCategory::Evidence, 16_000),
            ]),
        );
        let plan = match assembler.plan(&context_items) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_status(tf!("composer.status.context_limit", "error" => error));
                cx.emit(AgentStatus::Attention);
                cx.notify();
                return;
            }
        };
        let mut prompt = String::new();
        for item in plan.materialize_inline(&context_items) {
            if item.item_id == "turn-user-input" {
                continue;
            }
            if item.item_id == "system-and-rules" {
                prompt.push_str(&item.content);
            } else {
                prompt.push_str(&format!(
                    "\n\nContext item `{}` from {:?}:\n{}",
                    item.item_id, item.source, item.content
                ));
            }
        }
        self.last_context_plan = Some(plan);
        let history_before = self.history.clone();
        self.history.push(Message::user(user_input.clone()));
        self.draft.input.clear();
        self.draft.attachments.clear();
        self.cursor = 0;
        self.persist_session();

        let plan_request = self.mode == Mode::Plan && !self.plan_confirmed;
        let tools = if plan_request {
            prompt.push_str("\n\nPLAN MODE: First return an explicit numbered plan with file paths and approximate ranges. Do not invoke any approval-level tool until the user confirms the plan.");
            match runtime.tools.clone().subset([
                "read_file",
                "list_directory",
                "fs_search",
                "fs_grep",
                "get_terminal_context",
                "todo_read",
            ]) {
                Ok(tools) => tools,
                Err(error) => {
                    self.set_status(error.to_string());
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
            }
        } else {
            runtime.tools.clone()
        };
        let policy = self.approval_policy();
        // 终端快捷入口把任务固定到命令所在项目：执行器以该目录为根，任务的 project_dir 随之固定。
        let mut runtime = runtime;
        let project_dir = match self.orchestrator.take_project_override() {
            Some(dir) if dir != self.workspace_root => {
                let tools = self
                    .full_tools
                    .clone()
                    .unwrap_or_else(|| runtime.tools.clone());
                match self.build_executor(&dir, &tools) {
                    Ok(executor) => {
                        runtime.executor = executor;
                        dir
                    }
                    Err(error) => {
                        self.set_status(tf!(
                            "composer.status.project_pin_failed",
                            "dir" => dir.display(),
                            "error" => error
                        ));
                        cx.emit(AgentStatus::Error);
                        cx.notify();
                        return;
                    }
                }
            }
            _ => self.workspace_root.clone(),
        };
        let task = TaskRuntime::new(
            TaskConfig::new(user_input.clone(), project_dir, "native", "direct"),
            if policy == ApprovalPolicy::Yolo {
                RuntimeBudgets::autonomous()
            } else {
                RuntimeBudgets::default()
            },
            policy,
        )
        .with_tools(tools)
        .with_history(history_before)
        .with_system_prompt(prompt)
        .with_model(runtime.config.model.clone());
        let task = if let Some(data_dir) = self.data_dir.as_ref() {
            task.with_output_store(data_dir.join("agent-artifacts"), 32 * 1024)
        } else {
            task
        };
        let task = if let Some(data_dir) = self.data_dir.as_ref() {
            match task.try_with_journal(data_dir) {
                Ok(task) => task,
                Err(error) => {
                    self.set_status(tf!("composer.status.journal_failed", "error" => error));
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
            }
        } else {
            task
        };
        runtime.executor.set_task_owner(task.task().id.0.clone());
        self.last_task = Some(TaskRef::from_task(task.task()));
        self.command_watch = notify::CommandWatch::default();
        self.orchestrator
            .install_builtin_task(task, runtime, plan_request);
        self.dispatch_task_command(
            TaskCommand::Start { user_input },
            t!("composer.status.agent_working"),
            cx,
        );
        self.ensure_command_watch(cx);
    }

    /// 轮询当前任务的命令会话，在“运行中 → 终态”边沿发出结构化通知（Stage B 6.4）。
    /// 任务结束且没有运行中的会话时停止。
    pub(super) fn ensure_command_watch(&mut self, cx: &mut Context<Self>) {
        if self.command_watch_running {
            return;
        }
        self.command_watch_running = true;
        cx.spawn(async move |view, cx| loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(750))
                .await;
            let keep_going = view
                .update(cx, |view, cx| {
                    // 面板关闭时只需要状态，不取输出尾部。
                    let digests =
                        view.task_digests_with_tail(view.center_open(ComposerCenter::TaskDetails));
                    for notice in view.command_watch.observe(&digests) {
                        cx.emit(ComposerCommandNotice {
                            session_id: notice.session_id.0,
                            command: notice.command,
                            status: notice.status,
                        });
                    }
                    if view.center_open(ComposerCenter::TaskDetails) {
                        cx.notify();
                    }
                    // 运行中的 Turn 会暂时取走 active_task（busy 期间为 None），不能据此停止。
                    let keep_going = view
                        .orchestrator
                        .watch_should_continue(view.command_watch.has_running());
                    if !keep_going {
                        view.command_watch_running = false;
                    }
                    keep_going
                })
                .unwrap_or(false);
            if !keep_going {
                break;
            }
        })
        .detach();
    }

    /// 终端失败命令的快捷入口（Stage B 3.3）：附加命令及输出，固定任务到命令所在项目并提交。
    pub fn run_terminal_action(
        &mut self,
        action: TerminalAgentAction,
        label: String,
        text: String,
        command: Option<termior_ai::CommandProvenance>,
        project_dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self.orchestrator.terminal_action_busy() {
            // 不打断进行中的任务：只附加，交给用户决定何时提交。
            self.draft.attach_terminal_output(label, text, command);
            self.set_status(t!("composer.status.terminal_action_attached"));
            cx.notify();
            return;
        }
        let (command_text, cwd) = command
            .as_ref()
            .map(|command| {
                (
                    command.command.clone().unwrap_or_default(),
                    command.cwd.clone(),
                )
            })
            .unwrap_or_default();
        self.draft.attach_terminal_output(label, text, command);
        let prompt = match action {
            TerminalAgentAction::Explain => t!("composer.terminal_action.explain_prompt"),
            TerminalAgentAction::FixAndRerun => tf!(
                "composer.terminal_action.fix_prompt",
                "command" => command_text,
                "cwd" => cwd
            ),
        };
        self.draft.input = prompt.to_string();
        self.cursor = self.draft.input.len();
        if self.orchestrator.backend_choice == AgentBackendChoice::BuiltIn {
            self.orchestrator.pin_project(project_dir);
        }
        self.submit(cx);
        // submit 提前返回（如未配置模型）时不让固定目录泄漏到之后的手动提交。
        self.orchestrator.clear_project_pin();
        cx.notify();
    }

    pub(super) fn submit_external(&mut self, cx: &mut Context<Self>) {
        if self.orchestrator.submission_blocked() {
            return;
        }
        let Some(tools) = self.full_tools.as_ref() else {
            self.set_status(t!("composer.status.codex_unconfigured"));
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        let mut payload = match self.draft.build_payload(tools) {
            Ok(payload) => payload,
            Err(error) => {
                self.set_status(tf!("composer.status.attachment_rejected", "error" => error));
                cx.notify();
                return;
            }
        };
        payload.text = self.snippets.expand(&payload.text);
        if !payload.images.is_empty() {
            payload.text.push_str(&format!(
                "\n\n[{} image attachment(s) are available in Termior]",
                payload.images.len()
            ));
        }
        if payload.text.trim().is_empty() {
            return;
        }
        let user_input = payload.text;
        self.history.push(Message::user(user_input.clone()));
        self.draft.input.clear();
        self.draft.attachments.clear();
        self.cursor = 0;
        self.persist_session();
        self.dispatch_external_command(
            ExternalCommand::Start {
                task_id: new_session_id(),
                text: user_input,
            },
            t!("composer.status.codex_working"),
            cx,
        );
    }

    pub(super) fn dispatch_external_command(
        &mut self,
        command: ExternalCommand,
        status: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(ExternalDispatch {
            session,
            cancellation,
        }) = self.orchestrator.begin_external()
        else {
            return;
        };
        let project_dir = self.workspace_root.clone();
        let data_dir = self.data_dir.clone();
        self.set_status(status);
        cx.emit(AgentStatus::Working);
        cx.spawn(async move |view, cx| {
            let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded::<BackendEvent>();
            let task = cx.background_executor().spawn(async move {
                run_external_command(
                    session,
                    command,
                    project_dir,
                    data_dir,
                    cancellation,
                    event_tx,
                )
            });
            while let Some(event) = event_rx.next().await {
                let _ = view.update(cx, |view, cx| {
                    match event {
                        BackendEvent::AgentMessageDelta { text, .. } => {
                            view.append_text_delta(text)
                        }
                        BackendEvent::ItemStarted { .. } => {
                            view.set_status(t!("composer.status.codex_item"))
                        }
                        _ => {}
                    }
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.apply_external_result(result, cx);
                view.persist_session();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn apply_external_result(
        &mut self,
        result: ExternalRunResult,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.orchestrator.apply_external_result(result);
        match &outcome {
            ExternalOutcome::Error { error } => {
                self.set_status(tf!("composer.status.codex_error", "error" => error.clone()));
            }
            ExternalOutcome::Approval { call } => {
                self.set_status(tf!("composer.status.codex_approval", "call" => call.clone()));
            }
            ExternalOutcome::Completed { status, .. } => {
                self.set_status(tf!("composer.status.codex_turn", "status" => status.clone()));
            }
            ExternalOutcome::Unknown { reason } => {
                self.set_status(tf!("composer.status.codex_unknown", "reason" => reason.clone()));
            }
            ExternalOutcome::Running => self.set_status(t!("composer.status.codex_running")),
            ExternalOutcome::NoOutcome => self.set_status(t!("composer.status.codex_no_outcome")),
        }
        cx.emit(outcome.notify());
    }

    pub(super) fn dispatch_task_command(
        &mut self,
        command: TaskCommand,
        status: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let (runtime, task_runtime) =
            match self.orchestrator.begin_task_command(self.approval_policy()) {
                Ok(dispatch) => (dispatch.runtime, dispatch.task),
                Err(TaskDispatchReject::Busy) => return,
                Err(TaskDispatchReject::NoTask) => {
                    self.set_status(t!("composer.status.no_task"));
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
                Err(TaskDispatchReject::NoRuntime) => {
                    self.set_status(t!("composer.status.no_runtime"));
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
            };
        self.set_status(status);
        cx.emit(AgentStatus::Working);
        cx.spawn(async move |view, cx| {
            let (event_tx, event_rx) = futures::channel::mpsc::unbounded::<LiveTaskEvent>();
            let task = cx.background_executor().spawn(async move {
                run_task_command(runtime, task_runtime, command, event_tx).await
            });
            // 一批到达的增量合并成一次重渲染，长输出流式期间不逐字触发布局。
            let mut event_rx = event_rx.ready_chunks(64);
            while let Some(events) = event_rx.next().await {
                let _ = view.update(cx, |view, cx| {
                    for event in events {
                        match event {
                            LiveTaskEvent::TextDelta(delta) => view.append_text_delta(delta),
                            LiveTaskEvent::Message(message) => view.apply_live_message(message),
                        }
                    }
                    cx.notify();
                });
            }
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.streaming_draft = false;
                view.apply_task_result(result, cx);
                view.persist_session();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn apply_task_result(&mut self, result: TaskRunResult, cx: &mut Context<Self>) {
        self.history = self.orchestrator.begin_task_result(&result.task);
        self.persist_task_summary(&result.task);
        if let Some(dir) = self.data_dir.as_deref() {
            if let Err(error) = result.task.persist_journal(dir) {
                self.set_status(tf!("composer.status.journal_error", "error" => error));
                self.orchestrator.keep_task(result.task);
                cx.emit(AgentStatus::Error);
                return;
            }
        }

        if let Some(error) = result.error {
            self.set_status(tf!("composer.status.agent_error", "error" => error));
            self.orchestrator.fail_task();
            cx.emit(AgentStatus::Error);
            return;
        }

        let commit = self
            .orchestrator
            .commit_task_result(result.task, self.mode == Mode::Yolo);
        self.present_task_commit(commit, cx);
    }

    fn present_task_commit(&mut self, commit: TaskCommit, cx: &mut Context<Self>) {
        match commit.hint {
            TaskUiHint::OpenTaskDetails => {
                self.open_center = Some(ComposerCenter::TaskDetails);
            }
            TaskUiHint::AwaitPlanConfirmation => self.awaiting_plan_confirmation = true,
            TaskUiHint::None => {}
        }
        match &commit.status {
            TaskStatusLine::LostApproval => self.set_status(t!("composer.status.lost_approval")),
            TaskStatusLine::LostContract => self.set_status(t!("composer.status.lost_contract")),
            TaskStatusLine::InvalidSummary => {
                self.set_status(t!("composer.status.invalid_summary"));
            }
            TaskStatusLine::ApprovalRequired { summary } => self.set_status(tf!(
                "composer.status.approval_required",
                "summary" => summary.clone()
            )),
            TaskStatusLine::ReviewHunks => self.set_status(t!("composer.status.review_hunks")),
            TaskStatusLine::BudgetExhausted {
                dimension,
                used,
                limit,
            } => self.set_status(tf!(
                "composer.status.budget_exhausted",
                "dimension" => dimension.clone(),
                "used" => *used,
                "limit" => *limit
            )),
            TaskStatusLine::TerminalTakenOver { session, cursor } => self.set_status(tf!(
                "composer.status.terminal_taken_over",
                "session" => session.clone(),
                "cursor" => *cursor
            )),
            TaskStatusLine::NeedsInputReason { reason } => self.set_status(tf!(
                "composer.status.needs_input_reason",
                "reason" => reason.clone()
            )),
            TaskStatusLine::NeedsInput => self.set_status(t!("composer.status.needs_input")),
            TaskStatusLine::PlanReady => self.set_status(t!("composer.plan.ready")),
            TaskStatusLine::FinishedVerified => {
                self.set_status(t!("composer.status.finished_verified"));
            }
            TaskStatusLine::FinishedUnverified => {
                self.set_status(t!("composer.status.finished_unverified"));
            }
            TaskStatusLine::Cancelled => self.set_status(t!("composer.status.cancelled")),
            TaskStatusLine::UnknownResult => {
                self.set_status(t!("composer.status.unknown_result"));
            }
            TaskStatusLine::TaskFailed => self.set_status(t!("composer.status.task_failed")),
            TaskStatusLine::Other { state } => self.set_status(tf!(
                "composer.status.task_state",
                "state" => state.clone()
            )),
        }
        cx.emit(commit.notify);
        if let Some(summary) = commit.review.clone() {
            cx.emit(EditReviewRequested(summary));
        }
        self.orchestrator.finish_task_commit(&commit);
        if commit.clear_plan {
            self.plan_confirmed = false;
            self.awaiting_plan_confirmation = false;
        }
        if commit.auto_resolve {
            self.auto_resolve_in_yolo(cx);
        }
    }

    pub(super) fn continue_with_more_budget(&mut self, cx: &mut Context<Self>) {
        let Some(budgets) = self.orchestrator.increased_budgets() else {
            return;
        };
        self.dispatch_task_command(
            TaskCommand::IncreaseBudgets { budgets },
            t!("composer.budget.continuing"),
            cx,
        );
    }
}

/// 任务运行中推给 UI 的实时事件：模型文本增量，以及追加到历史的定稿消息（含工具调用与结果）。
enum LiveTaskEvent {
    TextDelta(String),
    Message(Message),
}

async fn run_task_command(
    runtime: AgentRuntime,
    mut task_runtime: TaskRuntime,
    command: TaskCommand,
    event_tx: futures::channel::mpsc::UnboundedSender<LiveTaskEvent>,
) -> TaskRunResult {
    let provider = match HttpProvider::new(runtime.config) {
        Ok(provider) => provider,
        Err(error) => {
            return TaskRunResult {
                task: task_runtime,
                error: Some(error.to_string()),
            }
        }
    };
    let message_tx = event_tx.clone();
    task_runtime.set_message_observer(Some(Arc::new(move |message: &Message| {
        let _ = message_tx.unbounded_send(LiveTaskEvent::Message(message.clone()));
    })));
    let mut on_event = move |event: &termior_ai::ChatEvent| {
        if let termior_ai::ChatEvent::TextDelta(delta) = event {
            let _ = event_tx.unbounded_send(LiveTaskEvent::TextDelta(delta.clone()));
        }
    };
    let error = task_runtime
        .handle(command, &provider, runtime.executor.as_ref(), &mut on_event)
        .await
        .err()
        .map(|error| error.to_string());
    // 观察者持有发送端；不清除的话 UI 侧的接收循环永远等不到结束。
    task_runtime.set_message_observer(None);
    TaskRunResult {
        task: task_runtime,
        error,
    }
}

fn run_external_command(
    existing: Option<Arc<Mutex<ManagedAgentSession>>>,
    command: ExternalCommand,
    project_dir: PathBuf,
    data_dir: Option<PathBuf>,
    cancellation: CancellationToken,
    event_tx: futures::channel::mpsc::UnboundedSender<BackendEvent>,
) -> ExternalRunResult {
    let session = match existing {
        Some(session) => session,
        None => {
            let task_id = match &command {
                ExternalCommand::Start { task_id, .. } => task_id.clone(),
                ExternalCommand::Respond { .. } => {
                    return ExternalRunResult {
                        session: None,
                        outcome: None,
                        error: Some(t!("composer.status.codex_session_missing").to_string()),
                    }
                }
            };
            let mut backend = CodexBackend::new(
                std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into()),
            );
            if let Some(dir) = &data_dir {
                backend = backend.with_stderr_log(
                    dir.join("agent-backends").join("codex-stderr.log"),
                    std::iter::empty::<String>(),
                );
            }
            let created = ManagedAgentSession::connect(
                Box::new(backend),
                TaskLaunch {
                    task_id,
                    project_dir: project_dir.display().to_string(),
                    environment_id: "codex-workspace-write".into(),
                    model: None,
                },
            );
            match created {
                Ok(session) => Arc::new(Mutex::new(session)),
                Err(error) => {
                    return ExternalRunResult {
                        session: None,
                        outcome: None,
                        error: Some(error.to_string()),
                    }
                }
            }
        }
    };

    let first = {
        let mut guard = match session.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return ExternalRunResult {
                    session: Some(session.clone()),
                    outcome: None,
                    error: Some(t!("composer.status.codex_lock_poisoned").to_string()),
                }
            }
        };
        if let Some(dir) = data_dir {
            guard.enable_recovery(ExternalRecoveryTarget {
                tasks_root: dir.join("agent-tasks"),
                program: std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into()),
                args: Vec::new(),
            });
        }
        match command {
            ExternalCommand::Start { text, .. } => {
                guard.start_turn(text).map(|()| PumpOutcome::Pending)
            }
            ExternalCommand::Respond {
                approved,
                granted_permission_ids,
            } => guard.respond(
                approved,
                granted_permission_ids,
                &mut |event| {
                    let _ = event_tx.unbounded_send(event.clone());
                },
                std::time::Duration::from_millis(250),
            ),
        }
    };
    let mut outcome = match first {
        Ok(outcome) => outcome,
        Err(error) => {
            return ExternalRunResult {
                session: Some(session.clone()),
                outcome: None,
                error: Some(error.to_string()),
            }
        }
    };

    loop {
        if !matches!(outcome, PumpOutcome::Pending) {
            return ExternalRunResult {
                session: Some(session.clone()),
                outcome: Some(outcome),
                error: None,
            };
        }
        if cancellation.is_cancelled() {
            let result = session
                .lock()
                .map_err(|_| t!("composer.status.codex_lock_poisoned").to_string())
                .and_then(|mut guard| guard.cancel().map_err(|error| error.to_string()));
            return ExternalRunResult {
                session: Some(session.clone()),
                outcome: Some(match result {
                    Ok(()) => PumpOutcome::Completed {
                        success: false,
                        status: "cancelled".into(),
                    },
                    Err(reason) => PumpOutcome::Unknown { reason },
                }),
                error: None,
            };
        }
        outcome = match session.lock() {
            Ok(mut guard) => {
                match guard.pump_for(std::time::Duration::from_millis(250), &mut |event| {
                    let _ = event_tx.unbounded_send(event.clone());
                }) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        return ExternalRunResult {
                            session: Some(session.clone()),
                            outcome: None,
                            error: Some(error.to_string()),
                        }
                    }
                }
            }
            Err(_) => {
                return ExternalRunResult {
                    session: Some(session.clone()),
                    outcome: None,
                    error: Some(t!("composer.status.codex_lock_poisoned").to_string()),
                }
            }
        };
    }
}
