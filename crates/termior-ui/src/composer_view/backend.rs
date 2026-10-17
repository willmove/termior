//! 内置任务与 Codex 后端：提交、切换、实时事件与结果回写。

use super::helpers::{new_session_id, requested_skill_names, user_home_dir};
use super::*;

impl ComposerView {
    pub(super) fn cycle_custom_agent(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        let (choice, custom) = match (self.backend_choice, self.active_custom_agent) {
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
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        if self.backend_choice == AgentBackendChoice::CodexAppServer
            && choice != AgentBackendChoice::CodexAppServer
        {
            self.external_session = None;
            self.backend_capability_summary = None;
        }
        self.backend_choice = choice;
        self.active_custom_agent = match choice {
            AgentBackendChoice::BuiltIn => custom.filter(|index| *index < self.custom_agents.len()),
            AgentBackendChoice::CodexAppServer => None,
        };
        if let Some(full_tools) = self.full_tools.clone() {
            match self.selected_agent_runtime(&full_tools, &self.base_system_prompt) {
                Ok((tools, prompt)) => {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.tools = tools;
                        runtime.system_prompt = prompt;
                    }
                    if self.backend_choice == AgentBackendChoice::CodexAppServer {
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
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            self.submit_external(cx);
            return;
        }
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
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
        let project_dir = match self.task_project_override.take() {
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
        self.active_runtime = Some(runtime);
        self.active_cancellation = Some(task.cancellation_token());
        self.active_task = Some(task);
        self.active_plan_request = plan_request;
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
                    let keep_going =
                        view.busy || view.active_task.is_some() || view.command_watch.has_running();
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
        if self.busy || self.active_task.is_some() {
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
        if self.backend_choice == AgentBackendChoice::BuiltIn {
            self.task_project_override = Some(project_dir);
        }
        self.submit(cx);
        // submit 提前返回（如未配置模型）时不让固定目录泄漏到之后的手动提交。
        self.task_project_override = None;
        cx.notify();
    }

    pub(super) fn submit_external(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
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
        if self.busy {
            return;
        }
        let session = self.external_session.clone();
        let project_dir = self.workspace_root.clone();
        let data_dir = self.data_dir.clone();
        let cancellation = CancellationToken::default();
        self.active_cancellation = Some(cancellation.clone());
        self.busy = true;
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
        self.busy = false;
        self.active_cancellation = None;
        if let Some(session) = result.session.as_ref() {
            if let Ok(session) = session.lock() {
                let capabilities = &session.info().capabilities;
                self.backend_capability_summary = Some(format!(
                    "{} {} · auth={} · model={} · resume={:?} · fork={:?} · steer={:?} · cancel={:?} · approvals={:?} · terminal={:?}",
                    session.info().display_name,
                    session.info().version,
                    session.info().authentication_source,
                    session.info().model_source,
                    capabilities.session_resume,
                    capabilities.session_fork,
                    capabilities.mid_turn_steer,
                    capabilities.cancel,
                    capabilities.approval_requests,
                    capabilities.host_terminal,
                ));
            }
        }
        if let Some(session) = result.session {
            self.external_session = Some(session);
        }
        if let Some(error) = result.error {
            self.set_status(tf!("composer.status.codex_error", "error" => error));
            cx.emit(AgentStatus::Error);
            return;
        }
        match result.outcome {
            Some(PumpOutcome::WaitingApproval {
                backend_request_id,
                tool_call_id,
                request_kind,
                raw,
            }) => {
                self.external_pending_approval = Some(ExternalPendingApproval::from_codex(
                    backend_request_id,
                    tool_call_id.clone(),
                    request_kind,
                    &raw,
                ));
                self.set_status(tf!(
                    "composer.status.codex_approval",
                    "call" => tool_call_id
                ));
                cx.emit(AgentStatus::Attention);
            }
            Some(PumpOutcome::Completed { success, status }) => {
                self.external_pending_approval = None;
                self.set_status(tf!("composer.status.codex_turn", "status" => status));
                cx.emit(if success {
                    AgentStatus::Finished
                } else {
                    AgentStatus::Error
                });
            }
            Some(PumpOutcome::Unknown { reason }) => {
                self.external_pending_approval = None;
                self.set_status(tf!("composer.status.codex_unknown", "reason" => reason));
                cx.emit(AgentStatus::Attention);
            }
            Some(PumpOutcome::Pending) => {
                self.set_status(t!("composer.status.codex_running"));
                cx.emit(AgentStatus::Attention);
            }
            None => {
                self.set_status(t!("composer.status.codex_no_outcome"));
                cx.emit(AgentStatus::Error);
            }
        }
    }

    pub(super) fn dispatch_task_command(
        &mut self,
        command: TaskCommand,
        status: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(mut task_runtime) = self.active_task.take() else {
            self.set_status(t!("composer.status.no_task"));
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        let Some(runtime) = self.task_runtime().cloned() else {
            self.active_task = Some(task_runtime);
            self.set_status(t!("composer.status.no_runtime"));
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        // 续跑时按当前模式同步审批策略：任务挂起期间切到 Yolo，后续调用即不再询问。
        task_runtime.set_approval_policy(self.approval_policy());
        self.active_cancellation = Some(task_runtime.cancellation_token());
        self.busy = true;
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
        self.busy = false;
        self.active_cancellation = None;
        self.pending_approval = None;
        self.pending_edit = None;
        self.history = result.task.history().to_vec();
        self.persist_task_summary(&result.task);
        if let Some(dir) = self.data_dir.as_deref() {
            if let Err(error) = result.task.persist_journal(dir) {
                self.set_status(tf!("composer.status.journal_error", "error" => error));
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Error);
                return;
            }
        }

        if let Some(error) = result.error {
            self.set_status(tf!("composer.status.agent_error", "error" => error));
            self.active_runtime = None;
            cx.emit(AgentStatus::Error);
            return;
        }

        // 通知级别只由结构化 TaskState 决定（Stage B 6.4），不解析状态文案。
        let status = notify::status_for_task(result.task.task().state, self.active_plan_request);
        match result.task.task().state {
            TaskState::WaitingApproval => {
                let Some(request) = result.task.pending_approval() else {
                    self.set_status(t!("composer.status.lost_approval"));
                    cx.emit(AgentStatus::Error);
                    return;
                };
                let Some(contract) = result.task.pending_contract() else {
                    self.set_status(t!("composer.status.lost_contract"));
                    cx.emit(AgentStatus::Error);
                    return;
                };
                self.set_status(tf!(
                    "composer.status.approval_required",
                    "summary" => request.summary
                ));
                self.pending_approval = Some(PendingApproval { request, contract });
                self.active_task = Some(result.task);
                cx.emit(status);
            }
            TaskState::WaitingChangeReview => {
                let summary = result
                    .task
                    .pending_change_summary()
                    .and_then(|summary| serde_json::from_str::<EditProposalSummary>(summary).ok());
                let Some(summary) = summary else {
                    self.set_status(t!("composer.status.invalid_summary"));
                    cx.emit(AgentStatus::Error);
                    return;
                };
                let review_request = EditReviewRequested(summary.clone());
                self.pending_edit = Some(PendingEdit {
                    summary,
                    decisions: HashMap::new(),
                });
                self.set_status(t!("composer.status.review_hunks"));
                self.active_task = Some(result.task);
                cx.emit(status);
                // Yolo 下随后自动接受全部 hunk，不再弹审阅面板。
                if self.mode != Mode::Yolo {
                    cx.emit(review_request);
                }
            }
            TaskState::WaitingUser => {
                match result.task.task().waiting_reason.as_ref() {
                    Some(WaitingReason::BudgetExhausted {
                        dimension,
                        used,
                        limit,
                    }) => self.set_status(tf!(
                        "composer.status.budget_exhausted",
                        "dimension" => format!("{dimension:?}"),
                        "used" => used,
                        "limit" => limit
                    )),
                    Some(WaitingReason::TerminalControl { session_id, cursor }) => {
                        self.open_center = Some(ComposerCenter::TaskDetails);
                        self.set_status(tf!(
                            "composer.status.terminal_taken_over",
                            "session" => session_id,
                            "cursor" => cursor
                        ))
                    }
                    Some(reason) => self.set_status(tf!(
                        "composer.status.needs_input_reason",
                        "reason" => format!("{reason:?}")
                    )),
                    None => self.set_status(t!("composer.status.needs_input")),
                }
                self.active_task = Some(result.task);
                cx.emit(status);
            }
            TaskState::CompletedVerified => {
                if self.active_plan_request {
                    self.awaiting_plan_confirmation = true;
                    self.set_status(t!("composer.plan.ready"));
                    cx.emit(status);
                } else {
                    self.set_status(t!("composer.status.finished_verified"));
                    cx.emit(status);
                }
                self.active_task = None;
            }
            TaskState::CompletedUnverified => {
                if self.active_plan_request {
                    self.awaiting_plan_confirmation = true;
                    self.set_status(t!("composer.plan.ready"));
                    cx.emit(status);
                } else {
                    self.set_status(t!("composer.status.finished_unverified"));
                    cx.emit(status);
                }
                self.active_task = None;
            }
            TaskState::Cancelled => {
                self.set_status(t!("composer.status.cancelled"));
                self.active_task = None;
                cx.emit(status);
            }
            TaskState::Unknown => {
                self.set_status(t!("composer.status.unknown_result"));
                self.active_task = None;
                cx.emit(status);
            }
            TaskState::Failed => {
                self.set_status(t!("composer.status.task_failed"));
                self.active_task = None;
                cx.emit(status);
            }
            state => {
                self.set_status(tf!(
                    "composer.status.task_state",
                    "state" => format!("{state:?}")
                ));
                self.active_task = Some(result.task);
                cx.emit(status);
            }
        }
        if !self.active_plan_request && self.active_task.is_none() {
            self.plan_confirmed = false;
            self.awaiting_plan_confirmation = false;
        }
        if self.active_task.is_none() {
            self.active_runtime = None;
        }
        self.auto_resolve_in_yolo(cx);
    }

    pub(super) fn continue_with_more_budget(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.active_task.as_ref() else {
            return;
        };
        if runtime.task().state != TaskState::WaitingUser {
            return;
        }
        let mut budgets = runtime.task().budgets.clone();
        budgets.max_model_steps = budgets.max_model_steps.saturating_add(100);
        budgets.max_wall_clock_ms = budgets.max_wall_clock_ms.saturating_add(30 * 60 * 1_000);
        budgets.max_tool_output_bytes = budgets
            .max_tool_output_bytes
            .saturating_add(16 * 1024 * 1024);
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
