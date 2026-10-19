//! 会话构造、模型运行时重建、提交模式与面板布局。

use super::helpers::{new_session_id, user_home_dir};
use super::*;

impl ComposerView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            draft: ComposerDraft::default(),
            cursor: 0,
            marked_text: String::new(),
            ime_anchor: crate::ime_anchor::ImeAnchor::new(),
            focus_handle: cx.focus_handle(),
            history: Vec::new(),
            mode: Mode::Auto,
            yolo_confirmed: false,
            mode_menu_open: false,
            plan_confirmed: false,
            awaiting_plan_confirmation: false,
            status: t!("composer.status.choose_model").to_string(),
            needs_model_setup: true,
            orchestrator: ComposerOrchestrator::new(),
            sessions: SessionStore::default(),
            snippets: SnippetStore::default(),
            custom_agents: Vec::new(),
            active_custom_agent: None,
            base_system_prompt: String::new(),
            full_tools: None,
            data_dir: None,
            terminal_context: Arc::new(LiveTerminalContext {
                snapshot: Mutex::new(TerminalContext {
                    cwd: String::new(),
                    recent_output: String::new(),
                    captured_at_unix_ms: 0,
                    command_reference: None,
                }),
            }),
            workspace_root: PathBuf::new(),
            workspace_paths: Vec::new(),
            path_suggestions: Vec::new(),
            selected_path_suggestion: 0,
            slash_suggestions: Vec::new(),
            selected_slash_suggestion: 0,
            snippet_suggestions: Vec::new(),
            selected_snippet_suggestion: 0,
            todos: Arc::new(Mutex::new(TodoStore::default())),
            dock: ComposerDock::Bottom,
            fill_workspace: false,
            panel_height: crate::DEFAULT_COMPOSER_HEIGHT,
            scroll_handle: ScrollHandle::new(),
            last_context_plan: None,
            open_center: None,
            recovery_tasks: Vec::new(),
            automations: AutomationStore::default(),
            task_tree: TaskTreeSnapshot::default(),
            task_tree_selected: None,
            checkpoints: Vec::new(),
            skills: Vec::new(),
            skill_scan_error: None,
            active_skills: Vec::new(),
            memory: MemoryStore::default(),
            agent_menu_open: false,
            cli_agent_available: Vec::new(),
            model_settings: ModelSettings::default(),
            custom_instructions: String::new(),
            model_choice: None,
            model_menu_open: false,
            model_menu_ready: Vec::new(),
            selectable_texts: SelectableTexts::default(),
            text_selection: None,
            selecting_text: false,
            last_task: None,
            command_watch: notify::CommandWatch::default(),
            command_watch_running: false,
            expanded_tools: HashSet::new(),
            streaming_draft: false,
            automation_host_started: false,
            automation_disk_budget: 0,
            automation_disk_used: None,
        }
    }

    /// 活动任务固定的运行时；没有活动任务时为全局配置的运行时。
    pub(super) fn task_runtime(&self) -> Option<&AgentRuntime> {
        self.orchestrator.task_runtime()
    }

    /// 覆盖状态行文案；非「模型未配置」引导类状态统一走这里，
    /// 确保输入行占位符判定随之失效。
    pub(super) fn set_status(&mut self, status: impl Into<SharedString>) {
        self.status = status.into().to_string();
        self.needs_model_setup = false;
    }

    /// 写入「模型未配置」引导类状态：会被输入行占位符复用。
    pub(super) fn set_model_setup_status(&mut self, status: impl Into<SharedString>) {
        self.status = status.into().to_string();
        self.needs_model_setup = true;
    }

    pub fn configure(
        &mut self,
        settings: &Settings,
        root: &Path,
        workspace_auth: WorkspaceAuthRegistry,
        data_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.workspace_root = root.to_path_buf();
        self.data_dir = data_dir;
        if let Some(dir) = &self.data_dir {
            let files = DataFiles::new(dir.clone());
            self.sessions = files.sessions::<SessionStore>().load().unwrap_or_default();
            self.snippets = files.snippets::<SnippetStore>().load().unwrap_or_default();
            self.todos = Arc::new(Mutex::new(
                files.todos::<TodoStore>().load().unwrap_or_default(),
            ));
            self.custom_agents = files
                .agents::<AgentDefinitionStore>()
                .load()
                .unwrap_or_default()
                .agents;
            let tasks_root = dir.join("agent-tasks");
            // Query persisted Codex/ACP sessions before listing them. Failures stay unknown.
            let _ = reconcile_persisted_backend_bindings(&tasks_root);
            self.recovery_tasks = RecoveryCenter::scan(&tasks_root).unwrap_or_default();
            self.automations = AutomationStore::load(dir).unwrap_or_default();
            self.checkpoints = CheckpointStore::new(dir.join("agent-checkpoints"))
                .list_manifests()
                .unwrap_or_default();
            self.memory = MemoryStore::load(&dir.join("agent-memory.json")).unwrap_or_default();
            if self
                .active_custom_agent
                .is_some_and(|index| index >= self.custom_agents.len())
            {
                self.active_custom_agent = None;
            }
        }
        let user_skills_root = user_home_dir().map(|home| home.join(".agents").join("skills"));
        match SkillIndex::scan_sources(root, user_skills_root.as_deref()) {
            Ok(index) => {
                self.skills = index.metadata();
                self.skill_scan_error = None;
            }
            Err(error) => {
                self.skills.clear();
                self.skill_scan_error = Some(error.to_string());
            }
        }
        if self.sessions.sessions.is_empty() {
            self.sessions.create(new_session_id());
        }
        self.history = self
            .sessions
            .active_id
            .as_deref()
            .and_then(|id| self.sessions.get(id))
            .map(|session| {
                // 模式随会话记住（FR-AGENT-11）。
                self.mode = session.mode;
                self.model_choice = session.model.clone();
                session.messages.clone()
            })
            .unwrap_or_default();

        // External backends use their own authentication/model source, but attachments still go
        // through Termior's workspace-confined registry.
        let full_tools = ToolRegistry::new(workspace_auth);
        self.full_tools = Some(full_tools);
        self.model_settings = settings.models.clone();
        self.custom_instructions = settings.custom_instructions.clone();
        self.automation_disk_budget = settings.agent_retention.max_disk_bytes;
        self.rebuild_runtime(cx);
        self.ensure_automation_host(cx);
    }

    /// 选定模型所在 profile（已启用），模型字段替换为所选模型。
    pub(super) fn choice_profile(&self, choice: &SessionModel) -> Option<ModelProviderSettings> {
        self.model_settings
            .profiles
            .iter()
            .find(|profile| profile.id == choice.profile_id && profile.enabled)
            .map(|profile| ModelProviderSettings {
                model: choice.model.clone(),
                ..profile.clone()
            })
    }

    /// 本会话实际使用的模型：会话选择优先，否则为设置里的 Agent 默认模型。
    pub(super) fn effective_profile(&self) -> Option<ModelProviderSettings> {
        self.model_choice
            .as_ref()
            .and_then(|choice| self.choice_profile(choice))
            .or_else(|| self.model_settings.active_chat().cloned())
    }

    /// 按当前模型选择重建内置 Agent 运行时；进行中的任务持有自己的 `active_runtime`，不受影响。
    pub(super) fn rebuild_runtime(&mut self, cx: &mut Context<Self>) {
        let Some(full_tools) = self.full_tools.clone() else {
            return;
        };
        let root = self.workspace_root.clone();
        // 所选 profile 被删除或禁用时回落默认，避免会话卡在不可用的模型上。
        if self
            .model_choice
            .as_ref()
            .is_some_and(|choice| self.choice_profile(choice).is_none())
        {
            self.model_choice = None;
        }
        let profile = self.effective_profile();
        let Some(profile) = profile else {
            self.orchestrator.runtime = None;
            if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
                self.set_status(t!("composer.status.codex_ready"));
            } else {
                self.set_model_setup_status(t!("composer.status.pick_provider"));
            }
            cx.notify();
            return;
        };
        let key_name = format!("provider:{}", profile.id);
        let api_key = KeyringSecretStore::new().get(&key_name).ok().flatten();
        if !profile.local && api_key.is_none() {
            self.orchestrator.runtime = None;
            if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
                self.set_status(t!("composer.status.codex_ready"));
            } else {
                self.set_model_setup_status(tf!(
                    "composer.status.add_api_key",
                    "provider" => profile.display_name
                ));
            }
            cx.notify();
            return;
        }
        let config = match ProviderConfig::from_settings(&profile, api_key) {
            Ok(config) => config,
            Err(error) => {
                self.orchestrator.runtime = None;
                self.set_status(error.to_string());
                cx.notify();
                return;
            }
        };
        let executor = match self.build_executor(&root, &full_tools) {
            Ok(executor) => executor,
            Err(error) => {
                self.orchestrator.runtime = None;
                self.set_status(error.to_string());
                cx.notify();
                return;
            }
        };
        let base_system_prompt = format!(
            "You are Termior's coding agent. Work only inside the authorized workspace. Read before editing, use tools when needed, and never claim an action succeeded without its tool result.{}",
            if self.custom_instructions.trim().is_empty() {
                String::new()
            } else {
                format!("\n\nGlobal instructions:\n{}", self.custom_instructions)
            }
        );
        let (tools, system_prompt) =
            match self.selected_agent_runtime(&full_tools, &base_system_prompt) {
                Ok(runtime) => runtime,
                Err(error) => {
                    self.orchestrator.runtime = None;
                    self.set_status(error);
                    cx.notify();
                    return;
                }
            };
        self.full_tools = Some(full_tools);
        self.base_system_prompt = base_system_prompt;
        self.orchestrator.runtime = Some(AgentRuntime {
            config,
            tools,
            executor,
            system_prompt,
        });
        self.set_status(tf!(
            "composer.status.ready",
            "provider" => profile.display_name,
            "model" => profile.model,
            "agent" => self.active_agent_name()
        ));
        cx.notify();
    }

    /// 以 `root` 为工作区根构建工具执行器（共享终端上下文、TODO 与检查点存储）。
    pub(super) fn build_executor(
        &self,
        root: &Path,
        tools: &ToolRegistry,
    ) -> Result<Arc<ToolExecutor>, termior_ai::ToolError> {
        self.build_executor_for(root, tools, "direct")
    }

    /// 按任务环境构建执行器。Worktree 任务的 `environment_id` 来自 worktree，终端 cwd 与文件根都是 `root`。
    pub(super) fn build_executor_for(
        &self,
        root: &Path,
        tools: &ToolRegistry,
        environment_id: &str,
    ) -> Result<Arc<ToolExecutor>, termior_ai::ToolError> {
        let executor = ToolExecutor::new_in_environment(root, tools.clone(), environment_id)?
            .with_terminal_context(self.terminal_context.clone())
            .with_todo_store(self.todos.clone(), self.data_dir.clone());
        Ok(Arc::new(match self.data_dir.as_ref() {
            Some(data_dir) => {
                let catalog_path = termior_hooks::lifecycle_hooks_path(data_dir);
                let catalog = termior_hooks::HookCatalog::load(&catalog_path).unwrap_or_default();
                let pipeline = termior_hooks::HookPipeline::new(catalog.hooks)
                    .with_catalog_path(catalog_path)
                    .with_runs_path(termior_hooks::hook_runs_path(data_dir));
                executor
                    .with_hook_pipeline(std::sync::Arc::new(pipeline))
                    .with_checkpoint_store(data_dir.join("agent-checkpoints"))
            }
            None => executor,
        }))
    }

    /// Authorize `root` on a private registry copy, then build an executor confined to it.
    pub(super) fn executor_for_root(
        &self,
        root: &Path,
        tools: &ToolRegistry,
        environment_id: &str,
    ) -> Result<Arc<ToolExecutor>, termior_ai::ToolError> {
        let mut tools = tools.clone();
        tools
            .workspace_auth
            .authorize(&root.to_string_lossy().replace('\\', "/"));
        self.build_executor_for(root, &tools, environment_id)
    }

    /// Registry the repo map checks `root` against. A task root other than the workspace root
    /// is authorized on a private copy, the same rule `executor_for_root` applies to tools.
    /// `None` until the composer is configured.
    pub(super) fn repo_map_authorization(&self, root: &Path) -> Option<WorkspaceAuthRegistry> {
        let mut authorization = self.full_tools.as_ref()?.workspace_auth.clone();
        if root != self.workspace_root {
            authorization.authorize(&root.to_string_lossy().replace('\\', "/"));
        }
        Some(authorization)
    }

    /// Start indexing the root the next builtin task will use, so the first submit finds a
    /// warm map. Spawning is cheap; the sweep runs on the repo-map thread.
    pub(super) fn warm_repo_map(&mut self) {
        if self.orchestrator.backend_choice != AgentBackendChoice::BuiltIn
            || self.draft.input.trim().is_empty()
        {
            return;
        }
        let root = self.orchestrator.displayed_root(&self.workspace_root);
        if let Some(authorization) = self.repo_map_authorization(&root) {
            let _ = self.orchestrator.ensure_repo_map(&root, &authorization);
        }
    }

    pub(super) fn selected_agent_runtime(
        &self,
        full_tools: &ToolRegistry,
        base_prompt: &str,
    ) -> Result<(ToolRegistry, String), String> {
        let Some(index) = self.active_custom_agent else {
            return Ok((full_tools.clone(), base_prompt.to_owned()));
        };
        let definition = self
            .custom_agents
            .get(index)
            .ok_or_else(|| t!("composer.status.agent_missing").to_string())?;
        let tools = full_tools
            .clone()
            .subset(definition.tools.clone())
            .map_err(|error| {
                tf!("composer.status.agent_tools_invalid", "error" => error).to_string()
            })?;
        Ok((
            tools,
            format!(
                "{base_prompt}\n\nActive custom agent ({}):\n{}",
                definition.name, definition.system_prompt
            ),
        ))
    }

    pub(super) fn active_agent_name(&self) -> SharedString {
        if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
            return SharedString::from("Codex app-server");
        }
        self.active_custom_agent
            .and_then(|index| self.custom_agents.get(index))
            .map(|agent| SharedString::from(agent.name.as_str()))
            .unwrap_or_else(|| t!("composer.agent_builtin"))
    }

    /// 开始一个新的命名会话；当前会话已镜像落盘，忙碌或有挂起审批时拒绝。
    pub(super) fn new_session(&mut self, cx: &mut Context<Self>) {
        if self.orchestrator.submission_blocked() {
            self.set_status(t!("composer.status.session_busy"));
            return;
        }
        self.persist_session();
        self.sessions.create(new_session_id());
        self.history.clear();
        self.expanded_tools.clear();
        self.text_selection = None;
        self.orchestrator.clear_for_new_session();
        self.last_context_plan = None;
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = false;
        self.draft = ComposerDraft::default();
        self.mode = Mode::Auto;
        if self.model_choice.take().is_some() {
            self.rebuild_runtime(cx);
        }
        self.persist_session();
        self.set_status(t!("composer.status.session_new"));
        cx.notify();
    }

    pub(super) fn persist_session(&mut self) {
        if let Some(id) = self.sessions.active_id.clone() {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.messages = self.history.clone();
                session.mode = self.mode;
                session.model = self.model_choice.clone();
                if session.title == "New session" {
                    if let Some(first) = self
                        .history
                        .iter()
                        .find(|message| message.role == Role::User)
                    {
                        session.push_user(first.content.clone());
                        session.messages = self.history.clone();
                    }
                }
            }
        }
        if let Some(dir) = &self.data_dir {
            let _ = DataFiles::new(dir.clone())
                .sessions::<SessionStore>()
                .save(&self.sessions);
        }
    }

    /// 切换提交模式。运行中不允许切换；挂起审批期间只允许切到 Yolo（随即自动放行），
    /// 避免计划流状态错乱。首次切到 Yolo 弹一次确认（本次应用运行内不再重复）。
    pub(super) fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
            self.set_status(t!("composer.status.codex_owns_approvals"));
            cx.notify();
            return;
        }
        if self.orchestrator.blocks_mode_switch(mode) {
            return;
        }
        if mode == Mode::Yolo && !self.yolo_confirmed {
            let entity = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                let confirmed = rfd::AsyncMessageDialog::new()
                    .set_title(t!("composer.mode.yolo_title").to_string())
                    .set_description(t!("composer.mode.yolo_description").to_string())
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    .await;
                if confirmed == rfd::MessageDialogResult::Yes {
                    let _ = entity.update(cx, |view, cx| {
                        view.yolo_confirmed = true;
                        view.apply_mode(Mode::Yolo, cx);
                    });
                }
            })
            .detach();
            return;
        }
        self.apply_mode(mode, cx);
    }

    pub(super) fn apply_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.mode_menu_open = false;
        self.plan_confirmed = false;
        self.awaiting_plan_confirmation = false;
        match mode {
            Mode::Auto => self.set_status(t!("composer.mode.status_auto")),
            Mode::Plan => self.set_status(t!("composer.mode.status_plan")),
            Mode::Yolo => self.set_status(t!("composer.mode.status_yolo")),
        }
        self.persist_session();
        self.auto_resolve_in_yolo(cx);
        cx.notify();
    }

    /// 底部停靠且空对话、无审批/附件时收缩到输入行高度；右侧停靠恒为整栏高度。
    pub fn is_compact(&self) -> bool {
        !self.fill_workspace
            && self.dock == ComposerDock::Bottom
            && self
                .history
                .iter()
                .all(|message| !matches!(message.role, Role::User | Role::Assistant))
            && !self.orchestrator.has_pending()
            && !self.awaiting_plan_confirmation
            && self.draft.attachments.is_empty()
            && self.path_suggestions.is_empty()
            && self.slash_suggestions.is_empty()
            && self.snippet_suggestions.is_empty()
            && !self.any_center_open()
    }

    /// 工作区在恢复、拖拽或停靠切换后同步布局参数。
    pub fn set_layout(&mut self, dock: ComposerDock, panel_height: f32, cx: &mut Context<Self>) {
        let panel_height = panel_height.clamp(MIN_COMPOSER_HEIGHT, MAX_COMPOSER_HEIGHT);
        if self.dock != dock || self.panel_height != panel_height {
            self.dock = dock;
            self.panel_height = panel_height;
            cx.notify();
        }
    }

    pub fn set_fill_workspace(&mut self, fill: bool, cx: &mut Context<Self>) {
        if self.fill_workspace != fill {
            self.fill_workspace = fill;
            cx.notify();
        }
    }

    pub(super) fn model_setup_placeholder(&self) -> Option<&str> {
        if self.orchestrator.backend_choice == AgentBackendChoice::CodexAppServer {
            return None;
        }
        if self.orchestrator.runtime.is_some() {
            return None;
        }
        self.needs_model_setup.then_some(self.status.as_str())
    }
}
