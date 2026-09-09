use futures::StreamExt;
use gpui::{
    canvas, div, prelude::*, px, relative, App, Bounds, ClipboardEntry, ClipboardItem, Context,
    EventEmitter, FocusHandle, Focusable, FontWeight, InputHandler, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, Point, ScrollHandle, SharedString, UTF16Selection, WeakEntity, Window,
};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use termior_agent_host::{
    BackendEvent, CodexBackend, ManagedAgentSession, PumpOutcome, TaskLaunch,
};
use termior_ai::automation::{next_run_after, AutomationManager, AutomationStore};
use termior_ai::context_engine::{
    ContextAssembler, ContextCategory, ContextItem, ContextPlan, ContextSource, MemoryStatus,
    MemoryStore, RuleResolver, SkillIndex, SkillMetadata,
};
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, ApprovalPolicy, ApprovalRequest, Attachment,
    AttachmentSource, CancellationToken, ChangeSetId, ComposerDraft, EditProposalSummary,
    HttpProvider, KeyringSecretStore, Message, Mode, ProviderConfig, Role, RuntimeBudgets,
    SecretStore, SessionStore, SnippetStore, TaskCommand, TaskConfig, TaskRuntime, TaskState,
    TaskSummary, TaskSummaryStore, TerminalContext, TerminalContextProvider, ToolContract,
    ToolExecutor, ToolRegistry, WaitingReason,
};
use termior_explorer_core::fuzzy::fuzzy_match;
use termior_platform::AgentStatus;
use termior_preview::MarkdownDocument;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{
    CheckpointManifest, CheckpointStore, DataFiles, RecoveryCenter, RecoveryCommand,
    RecoveryTaskSummary, RestoreAction, Settings,
};
use termior_ui::{ComposerDock, MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT};
use termior_ui_kit::{menu_panel, tokens::icon_size, Icon, Tooltip};

#[derive(Clone)]
struct AgentRuntime {
    config: ProviderConfig,
    tools: ToolRegistry,
    executor: Arc<ToolExecutor>,
    system_prompt: String,
}

struct PendingApproval {
    request: ApprovalRequest,
    contract: ToolContract,
}

struct PendingEdit {
    summary: EditProposalSummary,
    decisions: HashMap<usize, bool>,
}

struct TaskRunResult {
    task: TaskRuntime,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentBackendChoice {
    BuiltIn,
    CodexAppServer,
}

struct ExternalPendingApproval {
    backend_request_id: String,
    tool_call_id: String,
    request_kind: String,
    arguments: String,
}

enum ExternalCommand {
    Start { task_id: String, text: String },
    Respond { approved: bool },
}

#[derive(Debug, Clone, Copy)]
enum MemoryAction {
    Accept,
    Reject,
    TogglePause,
    Delete,
}

struct ExternalRunResult {
    session: Option<Arc<Mutex<ManagedAgentSession>>>,
    outcome: Option<PumpOutcome>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EditReviewRequested(pub EditProposalSummary);

/// 面板头图标按钮：请求在底部/右侧停靠间切换。
#[derive(Debug, Clone)]
pub struct ComposerDockToggle;

/// 面板头「收起」按钮请求隐藏 Composer（等价 `Ctrl+I`）。
#[derive(Debug, Clone)]
pub struct ComposerCollapse;

struct LiveTerminalContext {
    snapshot: Mutex<TerminalContext>,
}

impl TerminalContextProvider for LiveTerminalContext {
    fn snapshot(&self) -> TerminalContext {
        self.snapshot.lock().unwrap().clone()
    }
}

pub struct ComposerView {
    draft: ComposerDraft,
    cursor: usize,
    marked_text: String,
    /// IME 候选窗锚点（输入行文本的探针每帧刷新）。
    ime_anchor: crate::ime_anchor::ImeAnchor,
    focus_handle: FocusHandle,
    history: Vec<Message>,
    /// 提交模式（Auto/Plan/Yolo）；随会话持久化，见 FR-AGENT-11。
    mode: Mode,
    /// 本次应用运行内是否已确认过 Yolo 提示；确认一次后不再打扰。
    yolo_confirmed: bool,
    /// 模式下拉菜单是否展开。
    mode_menu_open: bool,
    plan_confirmed: bool,
    awaiting_plan_confirmation: bool,
    busy: bool,
    status: String,
    runtime: Option<AgentRuntime>,
    active_task: Option<TaskRuntime>,
    active_cancellation: Option<CancellationToken>,
    active_plan_request: bool,
    pending_approval: Option<PendingApproval>,
    external_pending_approval: Option<ExternalPendingApproval>,
    pending_edit: Option<PendingEdit>,
    sessions: SessionStore,
    snippets: SnippetStore,
    custom_agents: Vec<AgentDefinition>,
    active_custom_agent: Option<usize>,
    base_system_prompt: String,
    full_tools: Option<ToolRegistry>,
    data_dir: Option<PathBuf>,
    terminal_context: Arc<LiveTerminalContext>,
    workspace_root: PathBuf,
    workspace_paths: Vec<String>,
    path_suggestions: Vec<String>,
    selected_path_suggestion: usize,
    /// 停靠位置与底部停靠高度，由 WorkspaceView 在恢复/拖拽/切换后同步。
    dock: ComposerDock,
    panel_height: f32,
    /// 会话消息区的滚动容器；贴底时跟随新输出自动下滚。
    scroll_handle: ScrollHandle,
    last_context_plan: Option<ContextPlan>,
    context_inspector_open: bool,
    recovery_tasks: Vec<RecoveryTaskSummary>,
    recovery_center_open: bool,
    automations: AutomationStore,
    automation_center_open: bool,
    checkpoints: Vec<CheckpointManifest>,
    checkpoint_center_open: bool,
    skills: Vec<SkillMetadata>,
    skill_scan_error: Option<String>,
    active_skills: Vec<String>,
    skill_center_open: bool,
    memory: MemoryStore,
    memory_center_open: bool,
    backend_choice: AgentBackendChoice,
    backend_capability_summary: Option<String>,
    external_session: Option<Arc<Mutex<ManagedAgentSession>>>,
}

/// 消息区最多渲染的最近消息条数，超出部分丢弃以约束流式期间的重排成本。
const COMPOSER_RENDERED_MESSAGES: usize = 50;
/// 面板头高度。
const COMPOSER_HEADER_HEIGHT: f32 = 28.0;
/// compact（空对话、底部停靠）时的最小面板高度：面板头 + 两行输入区 + 状态行。
const COMPOSER_COMPACT_HEIGHT: f32 = 124.0;
/// 模式下拉距面板左缘的偏移。
const MODE_MENU_LEFT: f32 = 12.0;
/// 模式下拉距面板底缘的偏移：约等于工具栏行高，让菜单贴着工具栏上沿。
const MODE_MENU_BOTTOM: f32 = 68.0;
/// 状态行显示时菜单再抬高一个状态行高度。
const STATUS_ROW_HEIGHT: f32 = 24.0;

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
            busy: false,
            status: "Choose a default chat model in Settings → Models".into(),
            runtime: None,
            active_task: None,
            active_cancellation: None,
            active_plan_request: false,
            pending_approval: None,
            external_pending_approval: None,
            pending_edit: None,
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
            dock: ComposerDock::Bottom,
            panel_height: termior_ui::DEFAULT_COMPOSER_HEIGHT,
            scroll_handle: ScrollHandle::new(),
            last_context_plan: None,
            context_inspector_open: false,
            recovery_tasks: Vec::new(),
            recovery_center_open: false,
            automations: AutomationStore::default(),
            automation_center_open: false,
            checkpoints: Vec::new(),
            checkpoint_center_open: false,
            skills: Vec::new(),
            skill_scan_error: None,
            active_skills: Vec::new(),
            skill_center_open: false,
            memory: MemoryStore::default(),
            memory_center_open: false,
            backend_choice: AgentBackendChoice::BuiltIn,
            backend_capability_summary: None,
            external_session: None,
        }
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
            self.custom_agents = files
                .agents::<AgentDefinitionStore>()
                .load()
                .unwrap_or_default()
                .agents;
            self.recovery_tasks =
                RecoveryCenter::scan(&dir.join("agent-tasks")).unwrap_or_default();
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
                session.messages.clone()
            })
            .unwrap_or_default();

        // External backends use their own authentication/model source, but attachments still go
        // through Termior's workspace-confined registry.
        let full_tools = ToolRegistry::new(workspace_auth);
        self.full_tools = Some(full_tools.clone());

        let profile = settings
            .models
            .active_chat_profile
            .as_deref()
            .and_then(|id| {
                settings
                    .models
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            })
            .filter(|profile| profile.enabled)
            .cloned();
        let Some(profile) = profile else {
            self.runtime = None;
            self.status = if self.backend_choice == AgentBackendChoice::CodexAppServer {
                "Ready · Codex app-server · authentication and model managed by Codex".into()
            } else {
                "Settings → Models: pick a provider, click \"Use for chat\", and enable it".into()
            };
            cx.notify();
            return;
        };
        let key_name = format!("provider:{}", profile.id);
        let api_key = KeyringSecretStore::new().get(&key_name).ok().flatten();
        if !profile.local && api_key.is_none() {
            self.runtime = None;
            self.status = if self.backend_choice == AgentBackendChoice::CodexAppServer {
                "Ready · Codex app-server · authentication and model managed by Codex".into()
            } else {
                format!(
                    "Add an API key for {} in Settings → Models",
                    profile.display_name
                )
            };
            cx.notify();
            return;
        }
        let config = match ProviderConfig::from_settings(&profile, api_key) {
            Ok(config) => config,
            Err(error) => {
                self.runtime = None;
                self.status = error.to_string();
                cx.notify();
                return;
            }
        };
        let executor = match ToolExecutor::new(root, full_tools.clone()) {
            Ok(executor) => {
                let executor = executor.with_terminal_context(self.terminal_context.clone());
                let executor = if let Some(data_dir) = self.data_dir.as_ref() {
                    executor.with_checkpoint_store(data_dir.join("agent-checkpoints"))
                } else {
                    executor
                };
                Arc::new(executor)
            }
            Err(error) => {
                self.runtime = None;
                self.status = error.to_string();
                cx.notify();
                return;
            }
        };
        let base_system_prompt = format!(
            "You are Termior's coding agent. Work only inside the authorized workspace. Read before editing, use tools when needed, and never claim an action succeeded without its tool result.{}",
            if settings.custom_instructions.trim().is_empty() {
                String::new()
            } else {
                format!("\n\nGlobal instructions:\n{}", settings.custom_instructions)
            }
        );
        let (tools, system_prompt) =
            match self.selected_agent_runtime(&full_tools, &base_system_prompt) {
                Ok(runtime) => runtime,
                Err(error) => {
                    self.runtime = None;
                    self.status = error;
                    cx.notify();
                    return;
                }
            };
        self.full_tools = Some(full_tools);
        self.base_system_prompt = base_system_prompt;
        self.runtime = Some(AgentRuntime {
            config,
            tools,
            executor,
            system_prompt,
        });
        self.status = format!(
            "Ready · {} / {} · {}",
            profile.display_name,
            profile.model,
            self.active_agent_name()
        );
        cx.notify();
    }

    fn selected_agent_runtime(
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
            .ok_or_else(|| "Selected custom agent no longer exists".to_owned())?;
        let tools = full_tools
            .clone()
            .subset(definition.tools.clone())
            .map_err(|error| format!("Custom agent tools are invalid: {error}"))?;
        Ok((
            tools,
            format!(
                "{base_prompt}\n\nActive custom agent ({}):\n{}",
                definition.name, definition.system_prompt
            ),
        ))
    }

    fn active_agent_name(&self) -> &str {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            return "Codex app-server";
        }
        self.active_custom_agent
            .and_then(|index| self.custom_agents.get(index))
            .map(|agent| agent.name.as_str())
            .unwrap_or("Built-in Agent")
    }

    fn cycle_custom_agent(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        match (self.backend_choice, self.active_custom_agent) {
            (AgentBackendChoice::CodexAppServer, _) => {
                self.backend_choice = AgentBackendChoice::BuiltIn;
                self.active_custom_agent = None;
                self.external_session = None;
                self.backend_capability_summary = None;
            }
            (AgentBackendChoice::BuiltIn, None) if !self.custom_agents.is_empty() => {
                self.active_custom_agent = Some(0);
            }
            (AgentBackendChoice::BuiltIn, Some(index)) if index + 1 < self.custom_agents.len() => {
                self.active_custom_agent = Some(index + 1);
            }
            (AgentBackendChoice::BuiltIn, _) => {
                self.backend_choice = AgentBackendChoice::CodexAppServer;
                self.active_custom_agent = None;
            }
        }
        if let Some(full_tools) = self.full_tools.clone() {
            match self.selected_agent_runtime(&full_tools, &self.base_system_prompt) {
                Ok((tools, prompt)) => {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.tools = tools;
                        runtime.system_prompt = prompt;
                    }
                    self.status = if self.backend_choice == AgentBackendChoice::CodexAppServer {
                        "Active agent: Codex app-server · auth/model managed by Codex".into()
                    } else {
                        format!("Active agent: {}", self.active_agent_name())
                    };
                }
                Err(error) => self.status = error,
            }
        }
        cx.notify();
    }

    pub fn attach_selection(&mut self, source: AttachmentSource, label: String, text: String) {
        self.draft.attach_selection(source, label, text);
    }

    pub fn attach_file(&mut self, path: PathBuf) {
        self.draft.attach_file(path);
    }

    fn apply_recovery_command(
        &mut self,
        task_id: &str,
        command: RecoveryCommand,
        cx: &mut Context<Self>,
    ) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.status = "Recovery storage is unavailable".into();
            cx.notify();
            return;
        };
        let tasks_root = data_dir.join("agent-tasks");
        match RecoveryCenter::apply(&tasks_root, task_id, command) {
            Ok(operation) => {
                self.status = format!(
                    "Recovery {} is now {:?}",
                    operation.attempt_id, operation.state
                );
                self.recovery_tasks = RecoveryCenter::scan(&tasks_root).unwrap_or_default();
            }
            Err(error) => self.status = format!("Recovery action failed: {error}"),
        }
        cx.notify();
    }

    fn restore_checkpoint(&mut self, checkpoint_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.status = "Checkpoint storage is unavailable".into();
            cx.notify();
            return;
        };
        let store = CheckpointStore::new(data_dir.join("agent-checkpoints"));
        let result = (|| {
            let manifest = store.load_manifest(checkpoint_id)?;
            let plan = store.plan_restore(&self.workspace_root, &manifest)?;
            let restored = plan
                .actions
                .iter()
                .filter(|action| {
                    matches!(
                        action,
                        RestoreAction::Restore { .. } | RestoreAction::DeleteCreated { .. }
                    )
                })
                .count();
            let conflicts = plan
                .actions
                .iter()
                .filter(|action| matches!(action, RestoreAction::SkipConflict { .. }))
                .count();
            store.apply_restore(&self.workspace_root, &plan)?;
            Ok::<_, termior_store::CheckpointError>((restored, conflicts))
        })();
        self.status = match result {
            Ok((restored, conflicts)) => format!(
                "Checkpoint {checkpoint_id}: restored {restored} file(s), skipped {conflicts} conflict(s)"
            ),
            Err(error) => format!("Checkpoint restore failed: {error}"),
        };
        cx.notify();
    }

    fn trigger_automation(&mut self, automation_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.status = "Automation storage is unavailable".into();
            cx.notify();
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let result = (|| {
            let mut manager = AutomationManager::load(data_dir)?;
            let run = manager.trigger_manual(automation_id, now)?;
            let task = run
                .create_task(&self.workspace_root, Some(data_dir))
                .map_err(std::io::Error::other)?;
            task.persist_journal(data_dir)
                .map_err(std::io::Error::other)?;
            self.automations = manager.store;
            Ok::<_, std::io::Error>(run)
        })();
        self.status = match result {
            Ok(run) => format!(
                "Automation {} queued as {} (task {})",
                automation_id, run.run_id, run.task_id
            ),
            Err(error) => format!("Automation trigger failed: {error}"),
        };
        cx.notify();
    }

    fn apply_memory_action(&mut self, id: &str, action: MemoryAction, cx: &mut Context<Self>) {
        let result = match action {
            MemoryAction::Accept => self.memory.accept(id),
            MemoryAction::Reject => {
                self.memory.reject(id);
                Ok(())
            }
            MemoryAction::TogglePause => {
                let paused = self
                    .memory
                    .entries
                    .get(id)
                    .is_some_and(|entry| entry.enabled);
                self.memory.pause(id, paused)
            }
            MemoryAction::Delete => {
                self.memory.delete(id);
                Ok(())
            }
        };
        if let Err(error) = result {
            self.status = format!("Memory action failed: {error}");
            cx.notify();
            return;
        }
        if let Some(data_dir) = self.data_dir.as_ref() {
            if let Err(error) = self.memory.persist(&data_dir.join("agent-memory.json")) {
                self.status = format!("Memory persistence failed: {error}");
                cx.notify();
                return;
            }
        }
        self.status = format!("Memory {id} updated");
        cx.notify();
    }

    pub fn set_workspace_paths(&mut self, paths: Vec<String>) {
        self.workspace_paths = paths;
        self.update_path_suggestions();
    }

    pub fn apply_reviewed_edit(
        &mut self,
        proposal_id: &str,
        accepted: &[usize],
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self
            .pending_edit
            .as_ref()
            .map_or(true, |edit| edit.summary.id != proposal_id)
        {
            return Err("This AI edit is no longer pending".into());
        }
        self.pending_edit = None;
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: accepted.to_vec(),
            },
            "Applying reviewed hunks and continuing verification…",
            cx,
        );
        Ok("Review submitted; Agent verification is running".into())
    }

    pub fn reject_reviewed_edit(
        &mut self,
        proposal_id: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self
            .pending_edit
            .as_ref()
            .map_or(true, |edit| edit.summary.id != proposal_id)
        {
            return Err("This AI edit is no longer pending".into());
        }
        self.pending_edit = None;
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: Vec::new(),
            },
            "Returning the rejected change to the Agent…",
            cx,
        );
        Ok("Change rejected; Agent is continuing with the review result".into())
    }

    /// 单一附件入口：一个无过滤对话框，按扩展名自动判型（图片 / 文本文件）。
    fn pick_attachment(&mut self, cx: &mut Context<Self>) {
        let mut dialog = rfd::AsyncFileDialog::new().set_title("Attach to Composer");
        if self.workspace_root.is_dir() {
            dialog = dialog.set_directory(&self.workspace_root);
        }
        // Never run a blocking file dialog inside a GPUI event handler: on
        // Windows its modal message loop re-enters the foreground executor
        // while the `App` RefCell is borrowed. Await the async dialog instead.
        cx.spawn(async move |view, cx| {
            let Some(handle) = dialog.pick_file().await else {
                return;
            };
            let path = handle.path().to_path_buf();
            let _ = view.update(cx, |view, cx| {
                match image_mime(&path) {
                    Some(mime) => match std::fs::read(&path) {
                        Ok(bytes) => {
                            let name = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("image")
                                .to_owned();
                            view.draft.attach_image(name, mime, &bytes);
                            view.status = "Image attached".into();
                        }
                        Err(error) => view.status = format!("Could not attach image: {error}"),
                    },
                    None => {
                        view.draft.attach_file(path);
                        view.status = "File attached".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_path_suggestions(&mut self) {
        let Some(query) = self.active_path_query() else {
            self.path_suggestions.clear();
            self.selected_path_suggestion = 0;
            return;
        };
        self.path_suggestions =
            fuzzy_match(&query, self.workspace_paths.iter().map(String::as_str))
                .into_iter()
                .take(8)
                .map(|hit| hit.path)
                .collect();
        self.selected_path_suggestion = self
            .selected_path_suggestion
            .min(self.path_suggestions.len().saturating_sub(1));
    }

    fn active_path_query(&self) -> Option<String> {
        let prefix = self
            .draft
            .input
            .chars()
            .take(self.cursor)
            .collect::<String>();
        let token = prefix
            .rsplit(|character: char| character.is_whitespace())
            .next()?;
        token.strip_prefix('@').map(str::to_owned)
    }

    fn accept_path_suggestion(&mut self, path: String, cx: &mut Context<Self>) {
        let prefix = self
            .draft
            .input
            .chars()
            .take(self.cursor)
            .collect::<String>();
        let token_chars = prefix
            .rsplit(|character: char| character.is_whitespace())
            .next()
            .map(|token| token.chars().count())
            .unwrap_or(0);
        let start_char = self.cursor.saturating_sub(token_chars);
        let start_byte = char_to_byte(&self.draft.input, start_char);
        let end_byte = char_to_byte(&self.draft.input, self.cursor);
        self.draft.input.replace_range(start_byte..end_byte, "");
        self.cursor = start_char;
        self.draft.attach_file(self.workspace_root.join(path));
        self.path_suggestions.clear();
        self.selected_path_suggestion = 0;
        cx.notify();
    }

    /// 把剪贴板读取推迟到当前 entity 更新结束之后。Windows 上打开剪贴板会往
    /// wndproc 回派 sent message，若此时 entity 的 RefCell 仍被借着就会重入崩溃。
    fn schedule_attach_clipboard(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let Some(item) = cx.read_from_clipboard() else {
                return;
            };
            let _ = entity.update(cx, |this, cx| {
                this.apply_clipboard_item(item, cx);
            });
        });
    }

    fn apply_clipboard_item(&mut self, item: ClipboardItem, cx: &mut Context<Self>) {
        let mut attached = false;
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let format = image.format();
                    self.draft.attach_image(
                        format!("pasted-image.{}", format.extension()),
                        format.mime_type(),
                        image.bytes(),
                    );
                    attached = true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.0 {
                        if path.is_file() {
                            self.draft.attach_file(path);
                            attached = true;
                        }
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if attached {
            self.status = "Clipboard attachment added".into();
            cx.notify();
        }
    }

    pub fn update_terminal_context(
        &mut self,
        cwd: String,
        recent_output: String,
        command_reference: Option<termior_terminal::TerminalCommandRecord>,
    ) {
        let captured_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        *self.terminal_context.snapshot.lock().unwrap() = TerminalContext {
            cwd,
            recent_output,
            captured_at_unix_ms,
            command_reference,
        };
    }

    pub fn context_plan(&self) -> Option<&ContextPlan> {
        self.last_context_plan.as_ref()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
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
            self.status = "Configure a default model in Settings → Models first".into();
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
                self.status = format!("Attachment rejected: {error}");
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
                self.status = format!("Could not resolve scoped AGENTS.md rules: {error}");
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
                        self.status = format!("Could not activate Agent Skill: {error}");
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
                        self.status = format!("Could not activate Agent Skill ${name}: {error}");
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
                self.status = format!("Context limit: {error}");
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
            ]) {
                Ok(tools) => tools,
                Err(error) => {
                    self.status = error.to_string();
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
            }
        } else {
            runtime.tools.clone()
        };
        let policy = if self.mode == Mode::Yolo {
            ApprovalPolicy::Yolo
        } else {
            ApprovalPolicy::Prompt
        };
        let task = TaskRuntime::new(
            TaskConfig::new(
                user_input.clone(),
                self.workspace_root.clone(),
                "native",
                "direct",
            ),
            RuntimeBudgets::default(),
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
                    self.status = format!("Could not initialize task journal: {error}");
                    cx.emit(AgentStatus::Error);
                    cx.notify();
                    return;
                }
            }
        } else {
            task
        };
        runtime.executor.set_task_owner(task.task().id.0.clone());
        self.active_cancellation = Some(task.cancellation_token());
        self.active_task = Some(task);
        self.active_plan_request = plan_request;
        self.dispatch_task_command(TaskCommand::Start { user_input }, "Agent is working…", cx);
    }

    fn submit_external(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        let Some(tools) = self.full_tools.as_ref() else {
            self.status = "Codex backend is not configured for this workspace".into();
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        let mut payload = match self.draft.build_payload(tools) {
            Ok(payload) => payload,
            Err(error) => {
                self.status = format!("Attachment rejected: {error}");
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
            "Codex app-server is working…",
            cx,
        );
    }

    fn dispatch_external_command(
        &mut self,
        command: ExternalCommand,
        status: &str,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let session = self.external_session.clone();
        let project_dir = self.workspace_root.clone();
        let cancellation = CancellationToken::default();
        self.active_cancellation = Some(cancellation.clone());
        self.busy = true;
        self.status = status.into();
        cx.emit(AgentStatus::Working);
        cx.spawn(async move |view, cx| {
            let (event_tx, mut event_rx) = futures::channel::mpsc::unbounded::<BackendEvent>();
            let task = cx.background_executor().spawn(async move {
                run_external_command(session, command, project_dir, cancellation, event_tx)
            });
            while let Some(event) = event_rx.next().await {
                let _ = view.update(cx, |view, cx| {
                    match event {
                        BackendEvent::AgentMessageDelta { text, .. } => {
                            view.append_text_delta(text)
                        }
                        BackendEvent::ItemStarted { .. } => {
                            view.status = "Codex is running a structured item…".into()
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

    fn apply_external_result(&mut self, result: ExternalRunResult, cx: &mut Context<Self>) {
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
            self.status = format!("Codex backend error: {error}");
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
                let arguments = termior_store::StreamingRedactor::new(Vec::<String>::new())
                    .redact(&raw.to_string())
                    .text;
                self.external_pending_approval = Some(ExternalPendingApproval {
                    backend_request_id,
                    tool_call_id: tool_call_id.clone(),
                    request_kind,
                    arguments,
                });
                self.status = format!("Codex approval required: {tool_call_id}");
                cx.emit(AgentStatus::Attention);
            }
            Some(PumpOutcome::Completed { success, status }) => {
                self.external_pending_approval = None;
                self.status = format!("Codex turn {status}");
                cx.emit(if success {
                    AgentStatus::Finished
                } else {
                    AgentStatus::Error
                });
            }
            Some(PumpOutcome::Unknown { reason }) => {
                self.external_pending_approval = None;
                self.status = format!("Codex result is unknown: {reason}");
                cx.emit(AgentStatus::Attention);
            }
            Some(PumpOutcome::Pending) => {
                self.status = "Codex turn is still running".into();
                cx.emit(AgentStatus::Attention);
            }
            None => {
                self.status = "Codex backend returned no outcome".into();
                cx.emit(AgentStatus::Error);
            }
        }
    }

    fn dispatch_task_command(
        &mut self,
        command: TaskCommand,
        status: &str,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(task_runtime) = self.active_task.take() else {
            self.status = "No active Agent task".into();
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            self.active_task = Some(task_runtime);
            self.status = "No configured Agent runtime".into();
            cx.emit(AgentStatus::Error);
            cx.notify();
            return;
        };
        self.active_cancellation = Some(task_runtime.cancellation_token());
        self.busy = true;
        self.status = status.into();
        cx.emit(AgentStatus::Working);
        cx.spawn(async move |view, cx| {
            let (event_tx, mut event_rx) =
                futures::channel::mpsc::unbounded::<termior_ai::ChatEvent>();
            let task = cx.background_executor().spawn(async move {
                run_task_command(runtime, task_runtime, command, event_tx).await
            });
            while let Some(event) = event_rx.next().await {
                if let termior_ai::ChatEvent::TextDelta(delta) = event {
                    let _ = view.update(cx, |view, cx| {
                        view.append_text_delta(delta);
                        cx.notify();
                    });
                }
            }
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.apply_task_result(result, cx);
                view.persist_session();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_task_result(&mut self, result: TaskRunResult, cx: &mut Context<Self>) {
        self.busy = false;
        self.active_cancellation = None;
        self.pending_approval = None;
        self.pending_edit = None;
        self.history = result.task.history().to_vec();
        self.persist_task_summary(&result.task);
        if let Some(dir) = self.data_dir.as_deref() {
            if let Err(error) = result.task.persist_journal(dir) {
                self.status = format!("Agent journal error: {error}");
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Error);
                return;
            }
        }

        if let Some(error) = result.error {
            self.status = format!("Agent error: {error}");
            cx.emit(AgentStatus::Error);
            return;
        }

        match result.task.task().state {
            TaskState::WaitingApproval => {
                let Some(request) = result.task.pending_approval() else {
                    self.status = "Agent runtime lost its pending approval".into();
                    cx.emit(AgentStatus::Error);
                    return;
                };
                let Some(contract) = result.task.pending_contract() else {
                    self.status = "Agent runtime lost the pending tool contract".into();
                    cx.emit(AgentStatus::Error);
                    return;
                };
                self.status = format!("Approval required: {}", request.summary);
                self.pending_approval = Some(PendingApproval { request, contract });
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Attention);
            }
            TaskState::WaitingChangeReview => {
                let summary = result
                    .task
                    .pending_change_summary()
                    .and_then(|summary| serde_json::from_str::<EditProposalSummary>(summary).ok());
                let Some(summary) = summary else {
                    self.status = "Agent runtime returned an invalid change summary".into();
                    cx.emit(AgentStatus::Error);
                    return;
                };
                let review_request = EditReviewRequested(summary.clone());
                self.pending_edit = Some(PendingEdit {
                    summary,
                    decisions: HashMap::new(),
                });
                self.status = "Review every proposed hunk before verification".into();
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Attention);
                cx.emit(review_request);
            }
            TaskState::WaitingUser => {
                self.status = match result.task.task().waiting_reason.as_ref() {
                    Some(WaitingReason::BudgetExhausted {
                        dimension,
                        used,
                        limit,
                    }) => format!("Agent paused: {dimension:?} budget exhausted ({used}/{limit})"),
                    Some(reason) => format!("Agent needs input: {reason:?}"),
                    None => "Agent needs input".into(),
                };
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Attention);
            }
            TaskState::CompletedVerified => {
                if self.active_plan_request {
                    self.awaiting_plan_confirmation = true;
                    self.status =
                        "Plan ready · confirm or reject before write-capable tools are exposed"
                            .into();
                    cx.emit(AgentStatus::Attention);
                } else {
                    self.status = "Agent finished · verified".into();
                    cx.emit(AgentStatus::Finished);
                }
                self.active_task = None;
            }
            TaskState::CompletedUnverified => {
                if self.active_plan_request {
                    self.awaiting_plan_confirmation = true;
                    self.status =
                        "Plan ready · confirm or reject before write-capable tools are exposed"
                            .into();
                    cx.emit(AgentStatus::Attention);
                } else {
                    self.status = "Agent finished · unverified (no acceptance command)".into();
                    cx.emit(AgentStatus::Finished);
                }
                self.active_task = None;
            }
            TaskState::Cancelled => {
                self.status = "Agent task cancelled".into();
                self.active_task = None;
                cx.emit(AgentStatus::Finished);
            }
            TaskState::Unknown => {
                self.status = "Agent stopped with an unknown side-effect result".into();
                self.active_task = None;
                cx.emit(AgentStatus::Attention);
            }
            TaskState::Failed => {
                self.status = "Agent task failed; inspect the last tool result".into();
                self.active_task = None;
                cx.emit(AgentStatus::Error);
            }
            state => {
                self.status = format!("Agent task state: {state:?}");
                self.active_task = Some(result.task);
                cx.emit(AgentStatus::Attention);
            }
        }
        if !self.active_plan_request && self.active_task.is_none() {
            self.plan_confirmed = false;
            self.awaiting_plan_confirmation = false;
        }
    }

    fn persist_task_summary(&self, runtime: &TaskRuntime) {
        let Some(dir) = self.data_dir.as_deref() else {
            return;
        };
        let mut store = TaskSummaryStore::load(dir).unwrap_or_default();
        let sequence = runtime.events().last().map(|event| event.sequence);
        store.upsert(TaskSummary::from_task(runtime.task(), sequence));
        let _ = store.persist(dir);
    }

    fn approve_tool(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.external_pending_approval.take().is_some() {
            self.dispatch_external_command(
                ExternalCommand::Respond { approved: true },
                "Returning approval to Codex…",
                cx,
            );
            return;
        }
        let Some(pending) = self.pending_approval.take() else {
            return;
        };
        let label = format!("Running approved tool: {}", pending.request.tool_name);
        self.dispatch_task_command(
            TaskCommand::ResolveApproval {
                call_id: pending.request.call_id,
                approved: true,
            },
            &label,
            cx,
        );
    }

    fn reject_tool(&mut self, cx: &mut Context<Self>) {
        if self.external_pending_approval.take().is_some() {
            self.dispatch_external_command(
                ExternalCommand::Respond { approved: false },
                "Returning rejection to Codex…",
                cx,
            );
            return;
        }
        if let Some(pending) = self.pending_approval.take() {
            self.dispatch_task_command(
                TaskCommand::ResolveApproval {
                    call_id: pending.request.call_id,
                    approved: false,
                },
                "Returning the rejection to the Agent…",
                cx,
            );
        }
    }

    /// 追加流式文本增量：把 TextDelta 实时拼到当前 assistant 消息草稿上。
    /// 若 history 末尾不是 assistant，先 push 一条空 assistant 作为流式占位。
    /// 最终消息会在 apply_agent_result 时被 AgentOutcome.messages 整体覆盖。
    fn append_text_delta(&mut self, delta: String) {
        let needs_new = matches!(self.history.last(), Some(m) if m.role != Role::Assistant);
        if needs_new {
            self.history.push(Message::assistant(String::new()));
        }
        if let Some(last) = self.history.last_mut() {
            last.content.push_str(&delta);
        }
    }

    fn decide_hunk(&mut self, id: usize, accept: bool, cx: &mut Context<Self>) {
        if let Some(edit) = self.pending_edit.as_mut() {
            edit.decisions.insert(id, accept);
            cx.notify();
        }
    }

    fn apply_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.pending_edit.as_ref() else {
            return;
        };
        if edit.decisions.len() != edit.summary.hunk_ids.len() {
            self.status = "Accept or reject every hunk first".into();
            cx.notify();
            return;
        }
        let accepted = edit
            .summary
            .hunk_ids
            .iter()
            .filter(|id| edit.decisions.get(id) == Some(&true))
            .copied()
            .collect::<Vec<_>>();
        let proposal_id = edit.summary.id.clone();
        let _ = self.apply_reviewed_edit(&proposal_id, &accepted, cx);
    }

    fn reject_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(proposal_id) = self
            .pending_edit
            .as_ref()
            .map(|edit| edit.summary.id.clone())
        {
            let _ = self.reject_reviewed_edit(&proposal_id, cx);
        }
    }

    fn persist_session(&mut self) {
        if let Some(id) = self.sessions.active_id.clone() {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.messages = self.history.clone();
                session.mode = self.mode;
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

    /// 切换提交模式。Plan/挂起审批期间不允许切换，避免计划流状态错乱。
    /// 首次切到 Yolo 弹一次确认（本次应用运行内不再重复）。
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            self.status =
                "Codex app-server decides per-request approvals through its capability contract"
                    .into();
            cx.notify();
            return;
        }
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            return;
        }
        if mode == Mode::Yolo && !self.yolo_confirmed {
            let entity = cx.entity().downgrade();
            cx.spawn(async move |_, cx| {
                let confirmed = rfd::AsyncMessageDialog::new()
                    .set_title("Enable Yolo mode")
                    .set_description(
                        "Gated tools (file writes, commands, background shells) will run \
                         without asking for approval. Security guards (workspace bounds, \
                         secret deny-list) stay active. Enable Yolo mode?",
                    )
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

    fn apply_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.mode_menu_open = false;
        self.plan_confirmed = false;
        self.awaiting_plan_confirmation = false;
        self.status = match mode {
            Mode::Auto => "Mode: Auto · gated tools ask for approval".into(),
            Mode::Plan => "Mode: Plan · zero writes until the plan is confirmed".into(),
            Mode::Yolo => "Mode: Yolo · gated tools run without asking".into(),
        };
        self.persist_session();
        cx.notify();
    }

    fn confirm_plan(&mut self, cx: &mut Context<Self>) {
        if !self.awaiting_plan_confirmation || self.busy {
            return;
        }
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = true;
        self.draft.input = "Execute the approved plan now. Follow each listed step and use the normal approval workflow for every write or command.".into();
        self.cursor = self.draft.input.chars().count();
        self.submit(cx);
    }

    fn reject_plan(&mut self, cx: &mut Context<Self>) {
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = false;
        self.history.push(Message::assistant(
            "The proposed plan was rejected. No write-capable tool was exposed.",
        ));
        self.status = "Plan rejected".into();
        cx.emit(AgentStatus::Finished);
        self.persist_session();
        cx.notify();
    }

    fn send(&mut self, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            self.stop_active_task(cx);
        } else {
            self.submit(cx);
        }
    }

    fn stop_active_task(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = &self.active_cancellation {
            cancellation.cancel();
            self.status = "Cancelling Agent task…".into();
            cx.emit(AgentStatus::Attention);
            cx.notify();
            return;
        }
        if self.active_task.is_some() {
            self.dispatch_task_command(TaskCommand::Cancel, "Cancelling Agent task…", cx);
        }
    }

    fn continue_with_more_budget(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.active_task.as_ref() else {
            return;
        };
        if runtime.task().state != TaskState::WaitingUser {
            return;
        }
        let mut budgets = runtime.task().budgets.clone();
        budgets.max_model_steps = budgets.max_model_steps.saturating_add(25);
        budgets.max_wall_clock_ms = budgets.max_wall_clock_ms.saturating_add(15 * 60 * 1_000);
        budgets.max_tool_output_bytes = budgets
            .max_tool_output_bytes
            .saturating_add(4 * 1024 * 1024);
        self.dispatch_task_command(
            TaskCommand::IncreaseBudgets { budgets },
            "Continuing Agent task with an increased budget…",
            cx,
        );
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if primary && event.keystroke.key.eq_ignore_ascii_case("v") {
            self.schedule_attach_clipboard(cx);
            return;
        }
        if !self.path_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_path_suggestion = self.selected_path_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_path_suggestion = (self.selected_path_suggestion + 1)
                        .min(self.path_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" => {
                    if let Some(path) = self
                        .path_suggestions
                        .get(self.selected_path_suggestion)
                        .cloned()
                    {
                        self.accept_path_suggestion(path, cx);
                    }
                    return;
                }
                "escape" => {
                    self.path_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        match event.keystroke.key.as_str() {
            "escape" if self.mode_menu_open => self.mode_menu_open = false,
            "enter" | "return" if !event.keystroke.modifiers.shift => self.submit(cx),
            "backspace" if self.cursor > 0 => {
                let start = char_to_byte(&self.draft.input, self.cursor - 1);
                let end = char_to_byte(&self.draft.input, self.cursor);
                self.draft.input.replace_range(start..end, "");
                self.cursor -= 1;
            }
            "left" => self.cursor = self.cursor.saturating_sub(1),
            "right" => self.cursor = (self.cursor + 1).min(self.draft.input.chars().count()),
            _ => return,
        }
        self.update_path_suggestions();
        cx.notify();
    }

    /// 输入草稿 + 光标。光标用 ASCII `|` 字符而不是 `▏`（U+258F）：后者在
    /// Windows 上经字体回退渲染成一个很宽的空白。`|` 在任何 UI 字体里都是
    /// 窄竖线，且随文本流布局，无独立元素参与 flex 分配（曾因文本拆段 +
    /// flex 子项被压成逐字换行的竖排回归）。
    fn display_input(&self) -> String {
        let mut input = self.draft.input.clone();
        let byte = char_to_byte(&input, self.cursor);
        input.insert_str(
            byte,
            if self.marked_text.is_empty() {
                "|"
            } else {
                "|…"
            },
        );
        input
    }

    /// 底部停靠且空对话、无审批/附件时收缩到输入行高度；右侧停靠恒为整栏高度。
    pub fn is_compact(&self) -> bool {
        self.dock == ComposerDock::Bottom
            && self
                .history
                .iter()
                .all(|message| !matches!(message.role, Role::User | Role::Assistant))
            && self.pending_approval.is_none()
            && self.external_pending_approval.is_none()
            && self.pending_edit.is_none()
            && !self.awaiting_plan_confirmation
            && self.draft.attachments.is_empty()
            && self.path_suggestions.is_empty()
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

    fn model_setup_placeholder(&self) -> Option<&str> {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            return None;
        }
        if self.runtime.is_some() {
            return None;
        }
        let status = self.status.as_str();
        if status.contains("Settings → Models") || status.contains("API key") {
            Some(status)
        } else {
            None
        }
    }
}

impl EventEmitter<AgentStatus> for ComposerView {}
impl EventEmitter<EditReviewRequested> for ComposerView {}
impl EventEmitter<ComposerDockToggle> for ComposerView {}
impl EventEmitter<ComposerCollapse> for ComposerView {}

async fn run_task_command(
    runtime: AgentRuntime,
    mut task_runtime: TaskRuntime,
    command: TaskCommand,
    event_tx: futures::channel::mpsc::UnboundedSender<termior_ai::ChatEvent>,
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
    let mut on_event = move |event: &termior_ai::ChatEvent| {
        let _ = event_tx.unbounded_send(event.clone());
    };
    let error = task_runtime
        .handle(command, &provider, runtime.executor.as_ref(), &mut on_event)
        .await
        .err()
        .map(|error| error.to_string());
    TaskRunResult {
        task: task_runtime,
        error,
    }
}

fn run_external_command(
    existing: Option<Arc<Mutex<ManagedAgentSession>>>,
    command: ExternalCommand,
    project_dir: PathBuf,
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
                        error: Some("Codex approval session is missing".into()),
                    }
                }
            };
            let created = ManagedAgentSession::connect(
                Box::new(CodexBackend::new(
                    std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into()),
                )),
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
                    error: Some("Codex session lock is poisoned".into()),
                }
            }
        };
        match command {
            ExternalCommand::Start { text, .. } => {
                guard.start_turn(text).map(|()| PumpOutcome::Pending)
            }
            ExternalCommand::Respond { approved } => guard.respond(
                approved,
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
                .map_err(|_| "Codex session lock is poisoned".to_owned())
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
                    error: Some("Codex session lock is poisoned".into()),
                }
            }
        };
    }
}

impl Focusable for ComposerView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for ComposerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = ComposerInputHandler {
            view: cx.entity().downgrade(),
        };
        let ime_anchor = self.ime_anchor.clone();
        let agent_name = self.active_agent_name().to_owned();
        let agent_tooltip = self
            .backend_capability_summary
            .clone()
            .unwrap_or_else(|| "Agent — click to switch".into());
        let external_backend = self.backend_choice == AgentBackendChoice::CodexAppServer;
        let mode_label = if external_backend {
            "Backend approval"
        } else {
            self.mode.label()
        };
        let environment_id = self
            .active_task
            .as_ref()
            .map(|runtime| runtime.task().environment_id.clone())
            .unwrap_or_else(|| {
                if external_backend {
                    "codex-workspace-write".into()
                } else {
                    "direct-workspace".into()
                }
            });
        let environment_tooltip = if external_backend {
            format!(
                "host={} · root={} · file=workspace-write · network=backend-declared · credentials=Codex-managed · process=backend-declared · trust=reported/unknown",
                std::env::consts::OS,
                self.workspace_root.display()
            )
        } else {
            format!(
                "host={} · root={} · file=workspace-confined · network=provider/tool policy · credentials=keyring references only · process=local session tree · trust=verified where probed",
                std::env::consts::OS,
                self.workspace_root.display()
            )
        };
        let context_label = self.context_plan().map(|plan| {
            format!(
                "Context {}t · {} items",
                plan.total_tokens,
                plan.entries.len()
            )
        });
        let compact = self.is_compact();
        let placeholder = self.model_setup_placeholder();
        let show_status_row = placeholder.is_none() && !self.status.is_empty();
        let placeholder_active = self.draft.input.is_empty() && placeholder.is_some();
        // 占位态显示引导文案 + 光标；光标是文本内的 `|` 字符，随文本流布局。
        let input_display = if placeholder_active {
            format!("{}|", placeholder.unwrap_or_default())
        } else {
            self.display_input()
        };
        // 贴底时跟随新内容下滚；用户向上翻阅（离开底部）后不再强制滚动。
        // offset/max 读取的是上一帧布局的值——判定略滞后一帧，对本场景足够。
        let scroll_offset = self.scroll_handle.offset().y;
        let scroll_max = self.scroll_handle.max_offset().y;
        if scroll_max + scroll_offset <= px(8.0) {
            self.scroll_handle.scroll_to_bottom();
        }
        let messages = self
            .history
            .iter()
            .filter(|message| matches!(message.role, Role::User | Role::Assistant))
            .rev()
            .take(COMPOSER_RENDERED_MESSAGES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .enumerate()
            .map(|(index, message)| {
                let is_user = message.role == Role::User;
                let label = if is_user { "You" } else { "Termior" };
                let label_color = if is_user {
                    crate::ui::muted(&p)
                } else {
                    crate::ui::color(p.accent)
                };
                // Agent 回复按 Markdown 渲染（复用预览的 pulldown-cmark + GPUI
                // 管线，见 SDD 依赖决策）；用户消息保持原样，避免输入里的
                // *、# 等字符被误解析成格式。
                let body = if is_user {
                    div()
                        .flex()
                        .flex_col()
                        .text_xs()
                        .children(message.content.lines().map(|line| {
                            // 空行用不断行空格占位，避免塌陷成零高度。
                            let text = if line.is_empty() { "\u{00A0}" } else { line };
                            div().child(SharedString::from(text))
                        }))
                } else {
                    // 流式期间每个增量都会重渲染并重新解析；消息量已被
                    // COMPOSER_RENDERED_MESSAGES 截断，解析成本可接受。
                    let document = MarkdownDocument::parse(&message.content);
                    let blocks = crate::markdown_render::render_blocks(
                        &document,
                        crate::markdown_render::Density::CHAT,
                        &format!("composer-md-{index}"),
                        &p,
                    );
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .text_size(px(crate::markdown_render::Density::CHAT.base))
                        .line_height(relative(1.5))
                        // 首个增量到达前内容为空，用占位空行维持行高。
                        .when(blocks.is_empty(), |body| {
                            body.child(SharedString::from("\u{00A0}"))
                        })
                        .children(blocks)
                };
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .gap(px(2.0))
                    .child(div().text_xs().text_color(label_color).child(label))
                    .child(body)
            });
        let chips = self
            .draft
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                div()
                    .id(SharedString::from(format!("composer-attachment-{index}")))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui::color(p.elevated))
                    .border_1()
                    .border_color(crate::ui::border(&p))
                    .text_xs()
                    .cursor_pointer()
                    .child(SharedString::from(format!(
                        "{}  ×",
                        attachment.chip_label()
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.draft.remove_attachment(index);
                            cx.notify();
                        }),
                    )
            });
        let path_suggestions = (!self.path_suggestions.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .mx_3()
                .mt_1()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .bg(crate::ui::color(p.elevated))
                .shadow_md()
                .children(
                    self.path_suggestions
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            let selected = index == self.selected_path_suggestion;
                            let selected_path = path.clone();
                            div()
                                .id(SharedString::from(format!("composer-path-{index}")))
                                .px_3()
                                .py_1()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |row| row.bg(crate::ui::selected_wash(&p)))
                                .child(SharedString::from(format!("@{path}")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.accept_path_suggestion(selected_path.clone(), cx)
                                    }),
                                )
                        }),
                )
        });
        let approval_display = self
            .pending_approval
            .as_ref()
            .map(|pending| {
                (
                    format!(
                        "Approval: {} · call={} · effect={:?}",
                        pending.request.summary,
                        pending.request.call_id,
                        pending.contract.side_effect
                    ),
                    pending.request.arguments.clone(),
                )
            })
            .or_else(|| {
                self.external_pending_approval.as_ref().map(|pending| {
                    (
                        format!(
                            "Codex approval: {} · request={} · call={}",
                            pending.request_kind, pending.backend_request_id, pending.tool_call_id
                        ),
                        pending.arguments.clone(),
                    )
                })
            });
        let approval = approval_display.map(|(headline, arguments)| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.status[2]))
                .bg(crate::ui::alpha(p.status[2], 0.08))
                .child(SharedString::from(headline))
                .child(div().text_xs().child(SharedString::from(arguments)))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("approve-tool")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[1]))
                                .text_color(crate::ui::on_color(p.status[1]))
                                .cursor_pointer()
                                .child("Approve")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.approve_tool(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-tool")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[3]))
                                .text_color(crate::ui::on_color(p.status[3]))
                                .cursor_pointer()
                                .child("Reject")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_tool(cx)),
                                ),
                        ),
                )
        });
        let edit_review = self.pending_edit.as_ref().map(|edit| {
            let hunk_buttons = edit.summary.hunk_ids.iter().flat_map(|id| {
                let accept_id = *id;
                let reject_id = *id;
                let decision = edit.decisions.get(id).copied();
                [
                    div()
                        .id(SharedString::from(format!("accept-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(true) {
                            crate::ui::color(p.status[1])
                        } else {
                            crate::ui::color(p.elevated)
                        })
                        .text_color(if decision == Some(true) {
                            crate::ui::on_color(p.status[1])
                        } else {
                            crate::ui::color(p.foreground)
                        })
                        .cursor_pointer()
                        .child(SharedString::from(format!("Accept hunk {id}")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(accept_id, true, cx)
                            }),
                        ),
                    div()
                        .id(SharedString::from(format!("reject-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(false) {
                            crate::ui::color(p.status[3])
                        } else {
                            crate::ui::color(p.elevated)
                        })
                        .text_color(if decision == Some(false) {
                            crate::ui::on_color(p.status[3])
                        } else {
                            crate::ui::color(p.foreground)
                        })
                        .cursor_pointer()
                        .child(SharedString::from(format!("Reject hunk {id}")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(reject_id, false, cx)
                            }),
                        ),
                ]
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .child(SharedString::from(format!(
                    "AI diff · {}",
                    edit.summary.path
                )))
                .child(
                    div()
                        .max_h(px(70.0))
                        .overflow_hidden()
                        .text_xs()
                        .child(SharedString::from(edit.summary.patch.clone())),
                )
                .child(div().flex().flex_wrap().gap_1().children(hunk_buttons))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("apply-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[1]))
                                .text_color(crate::ui::on_color(p.status[1]))
                                .cursor_pointer()
                                .child("Apply decisions")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.apply_edit(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[3]))
                                .text_color(crate::ui::on_color(p.status[3]))
                                .cursor_pointer()
                                .child("Reject all")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_edit(cx)),
                                ),
                        ),
                )
        });
        let context_inspector = self
            .context_inspector_open
            .then_some(self.last_context_plan.as_ref())
            .flatten()
            .map(|plan| {
                let rows = plan.entries.iter().enumerate().map(|(index, entry)| {
                    div()
                        .id(SharedString::from(format!("context-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(SharedString::from(entry.item_id.clone()))
                                .child(SharedString::from(format!(
                                    "{:?} · {}t",
                                    entry.disposition, entry.tokens
                                ))),
                        )
                        .child(div().text_xs().text_color(crate::ui::muted(&p)).child(
                            SharedString::from(format!("{:?} · {}", entry.category, entry.reason)),
                        ))
                });
                div()
                    .id("context-inspector-panel")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(&p))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Context Inspector")
                            .child(SharedString::from(format!(
                                "{} / {} tokens",
                                plan.total_tokens, plan.hard_limit
                            ))),
                    )
                    .children(rows)
            });
        let recovery_center = self.recovery_center_open.then(|| {
            let rows = self.recovery_tasks.iter().enumerate().map(|(index, task)| {
                let retry_task_id = task.task_id.clone();
                let abandon_task_id = task.task_id.clone();
                let can_retry = task
                    .available_actions
                    .iter()
                    .any(|action| action == "retry");
                let can_abandon = task
                    .available_actions
                    .iter()
                    .any(|action| action == "abandon");
                div()
                    .id(SharedString::from(format!("recovery-task-{index}")))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(SharedString::from(format!(
                                "{} · {}",
                                task.task_id, task.attempt_id
                            )))
                            .child(SharedString::from(format!(
                                "{:?} · event {}",
                                task.recovered_state, task.last_trusted_event
                            ))),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(&p)).child(
                        SharedString::from(format!(
                            "persisted={} · backend={} · actions: {}",
                            task.persisted_state,
                            task.backend_id.as_deref().unwrap_or("unknown"),
                            task.available_actions.join(", ")
                        )),
                    ))
                    .when_some(task.diagnostic.clone(), |row, diagnostic| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::color(p.status[3]))
                                .child(SharedString::from(diagnostic)),
                        )
                    })
                    .when(can_retry || can_abandon, |row| {
                        row.child(
                            div()
                                .flex()
                                .gap_1()
                                .when(can_retry, |actions| {
                                    actions.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "recovery-retry-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(&p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child("Prepare retry")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_recovery_command(
                                                        &retry_task_id,
                                                        RecoveryCommand::Retry,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                })
                                .when(can_abandon, |actions| {
                                    actions.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "recovery-abandon-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::color(p.status[3]))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child("Abandon")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_recovery_command(
                                                        &abandon_task_id,
                                                        RecoveryCommand::Abandon,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                }),
                        )
                    })
            });
            div()
                .id("recovery-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Recovery Center"),
                )
                .when(self.recovery_tasks.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child("No unfinished tasks were found."),
                    )
                })
                .children(rows)
        });
        let automation_center = self.automation_center_open.then(|| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let rows =
                self.automations
                    .automations
                    .iter()
                    .enumerate()
                    .map(|(index, automation)| {
                        let automation_id = automation.id.clone();
                        let next = next_run_after(automation, now)
                            .map(|timestamp| timestamp.to_string())
                            .unwrap_or_else(|| "event/manual".into());
                        let last = self
                            .automations
                            .recent_runs
                            .iter()
                            .rev()
                            .find(|run| run.automation_id == automation.id)
                            .map(|run| format!("{:?} ({})", run.status, run.run_id))
                            .unwrap_or_else(|| "never".into());
                        div()
                            .id(SharedString::from(format!("automation-entry-{index}")))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .p_2()
                            .rounded_md()
                            .bg(crate::ui::alpha(p.foreground, 0.04))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(SharedString::from(automation.name.clone()))
                                    .child(if automation.enabled {
                                        "enabled"
                                    } else {
                                        "disabled"
                                    }),
                            )
                            .child(div().text_xs().text_color(crate::ui::muted(&p)).child(
                                SharedString::from(format!(
                                    "template v{} · next={} · last={} · {:?}",
                                    automation.template.version, next, last, automation.trigger
                                )),
                            ))
                            .when(automation.enabled, |row| {
                                row.child(
                                    div()
                                        .id(SharedString::from(format!("run-automation-{index}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::border(&p))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child("Queue manual run")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.trigger_automation(&automation_id, cx);
                                            }),
                                        ),
                                )
                            })
                    });
            div()
                .id("automation-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&p))
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Automations"))
                .when(self.automations.automations.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child("No local automation templates are configured."),
                    )
                })
                .children(rows)
        });
        let checkpoint_center =
            self.checkpoint_center_open.then(|| {
                let rows = self
                    .checkpoints
                    .iter()
                    .enumerate()
                    .map(|(index, checkpoint)| {
                        let checkpoint_id = checkpoint.id.clone();
                        let files = checkpoint
                            .files
                            .iter()
                            .map(|file| file.relative_path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        div()
                            .id(SharedString::from(format!("checkpoint-entry-{index}")))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .p_2()
                            .rounded_md()
                            .bg(crate::ui::alpha(p.foreground, 0.04))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(SharedString::from(checkpoint.id.clone()))
                                    .child(SharedString::from(format!(
                                        "task {} · event {}",
                                        checkpoint.task_id, checkpoint.sequence
                                    ))),
                            )
                            .child(
                                div().text_xs().text_color(crate::ui::muted(&p)).child(
                                    SharedString::from(format!("recoverable files: {files}")),
                                ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("restore-checkpoint-{index}")))
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(crate::ui::border(&p))
                                    .text_xs()
                                    .cursor_pointer()
                                    .child("Restore unchanged outputs")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.restore_checkpoint(&checkpoint_id, cx);
                                        }),
                                    ),
                            )
                    });
                div()
                    .id("checkpoint-center-panel")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::border(&p))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("File Checkpoints"),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(&p)).child(
                        "Process, network, and external-service effects are not reversible.",
                    ))
                    .children(rows)
            });
        let skill_center = self.skill_center_open.then(|| {
            let rows = self.skills.iter().enumerate().map(|(index, skill)| {
                let active = self.active_skills.iter().any(|name| name == &skill.name);
                div()
                    .id(SharedString::from(format!("skill-entry-{index}")))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(SharedString::from(format!("${}", skill.name)))
                            .child(if active {
                                "active this turn"
                            } else {
                                "available"
                            }),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(&p)).child(
                        SharedString::from(format!(
                            "{} · source={} · allowed tools: {}",
                            skill.description,
                            skill.path.display(),
                            if skill.allowed_tools.is_empty() {
                                "task tool set".into()
                            } else {
                                skill
                                    .allowed_tools
                                    .iter()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            }
                        )),
                    ))
            });
            div()
                .id("skill-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Agent Skills"),
                )
                .when_some(self.skill_scan_error.clone(), |panel, error| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::color(p.status[3]))
                            .child(SharedString::from(error)),
                    )
                })
                .children(rows)
        });
        let memory_center = self.memory_center_open.then(|| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let candidates =
                self.memory
                    .candidates
                    .values()
                    .enumerate()
                    .map(|(index, candidate)| {
                        let accept_id = candidate.id.clone();
                        let reject_id = candidate.id.clone();
                        div()
                            .id(SharedString::from(format!("memory-candidate-{index}")))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .p_2()
                            .rounded_md()
                            .bg(crate::ui::alpha(p.status[2], 0.08))
                            .child(SharedString::from(format!(
                                "Candidate {} · confidence {}",
                                candidate.id, candidate.confidence
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .child(SharedString::from(candidate.content.clone())),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "accept-memory-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(&p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child("Accept")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_memory_action(
                                                        &accept_id,
                                                        MemoryAction::Accept,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "reject-memory-{index}"
                                            )))
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::color(p.status[3]))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child("Reject")
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.apply_memory_action(
                                                        &reject_id,
                                                        MemoryAction::Reject,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                    ),
                            )
                    });
            let entries = self
                .memory
                .entries
                .values()
                .enumerate()
                .map(|(index, entry)| {
                    let toggle_id = entry.id.clone();
                    let delete_id = entry.id.clone();
                    let status = self
                        .memory
                        .status(&entry.id, now_ms)
                        .unwrap_or(MemoryStatus::Paused);
                    div()
                        .id(SharedString::from(format!("memory-entry-{index}")))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(SharedString::from(format!("{} · {:?}", entry.id, status)))
                        .child(
                            div()
                                .text_xs()
                                .child(SharedString::from(entry.content.clone())),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("toggle-memory-{index}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::border(&p))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(if entry.enabled { "Pause" } else { "Resume" })
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.apply_memory_action(
                                                    &toggle_id,
                                                    MemoryAction::TogglePause,
                                                    cx,
                                                );
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("delete-memory-{index}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::color(p.status[3]))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child("Delete")
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.apply_memory_action(
                                                    &delete_id,
                                                    MemoryAction::Delete,
                                                    cx,
                                                );
                                            }),
                                        ),
                                ),
                        )
                });
            div()
                .id("memory-center-panel")
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::border(&p))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Agent Memory"),
                )
                .children(candidates)
                .children(entries)
        });
        let budget_resume = self
            .active_task
            .as_ref()
            .is_some_and(|runtime| runtime.task().state == TaskState::WaitingUser)
            .then(|| {
                div()
                    .mx_3()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(crate::ui::color(p.status[2]))
                    .child("The Agent reached its task budget.")
                    .child(
                        div()
                            .id("continue-agent-budget")
                            .mt_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(crate::ui::color(p.accent))
                            .text_color(crate::ui::on_color(p.accent))
                            .cursor_pointer()
                            .child("Continue (+25 steps / +15 min)")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.continue_with_more_budget(cx)),
                            ),
                    )
            });
        let plan_review = self.awaiting_plan_confirmation.then(|| {
            div()
                .flex()
                .gap_2()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .child("Plan awaiting confirmation")
                .child(
                    div()
                        .id("confirm-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui::color(p.status[1]))
                        .text_color(crate::ui::on_color(p.status[1]))
                        .cursor_pointer()
                        .child("Confirm plan")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.confirm_plan(cx)),
                        ),
                )
                .child(
                    div()
                        .id("reject-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui::color(p.status[3]))
                        .text_color(crate::ui::on_color(p.status[3]))
                        .cursor_pointer()
                        .child("Reject plan")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.reject_plan(cx)),
                        ),
                )
        });
        // 模式下拉菜单：浮在工具栏上方（面板底部附近），最后挂载以便盖住下层内容。
        // 锚定面板底缘而非光标位置——菜单向下展开会被 overflow_hidden 裁掉。
        // 偏移 = 工具栏行高（+ 状态行高度），行高变化时需同步这两个值。
        let mode_menu = {
            let selected_mode = self.mode;
            menu_panel(&p)
                .absolute()
                .left(px(MODE_MENU_LEFT))
                .bottom(px(if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                }))
                .min_w(px(230.0))
                .child(div().flex().flex_col().gap(px(2.0)).children(
                    [Mode::Auto, Mode::Plan, Mode::Yolo].map(|mode| {
                        let selected = mode == selected_mode;
                        let hover = crate::ui::hover_wash(&p);
                        div()
                            .id(SharedString::from(format!(
                                "composer-mode-{}",
                                mode.label().to_lowercase()
                            )))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .when(selected, |item| item.bg(crate::ui::selected_wash(&p)))
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .child(mode.label())
                            .child(
                                div()
                                    .text_color(crate::ui::muted(&p))
                                    .child(mode.description()),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.set_mode(mode, cx);
                                }),
                            )
                    }),
                ))
        };
        div()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .when(self.dock == ComposerDock::Right, |root| {
                root.w_full().h_full()
            })
            .when(self.dock == ComposerDock::Bottom, |root| {
                root.w_full()
                    .when(compact, |root| root.min_h(px(COMPOSER_COMPACT_HEIGHT)))
                    .when(!compact, |root| root.h(px(self.panel_height)))
            })
            // 分隔线由工作区在面板外沿绘制的拖拽手柄承担。
            .bg(crate::ui::color(p.elevated))
            .text_color(crate::ui::color(p.foreground))
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            // 面板空白处点击收起模式下拉；chip 与菜单项各自 stop_propagation。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.mode_menu_open {
                        this.mode_menu_open = false;
                        cx.notify();
                    }
                }),
            )
            // 面板头：面板级动作的常驻入口（切换停靠 / 收起），取代埋在输入行里的文字按钮。
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(COMPOSER_HEADER_HEIGHT))
                    .px_2()
                    .flex_none()
                    .border_b_1()
                    .border_color(crate::ui::border(&p))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .px_1()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Agent"),
                            )
                            .when(!self.recovery_tasks.is_empty(), |header| {
                                header.child(
                                    div()
                                        .id("composer-recovery-center")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[2], 0.14))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(SharedString::from(format!(
                                            "Recovery {}",
                                            self.recovery_tasks.len()
                                        )))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.recovery_center_open =
                                                    !this.recovery_center_open;
                                                cx.notify();
                                            }),
                                    ),
                                )
                            })
                            .child(
                                div()
                                    .id("composer-automation-center")
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .bg(crate::ui::alpha(p.accent, 0.10))
                                    .text_xs()
                                    .cursor_pointer()
                                    .child(SharedString::from(format!(
                                        "Automations {}",
                                        self.automations.automations.len()
                                    )))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.automation_center_open =
                                                !this.automation_center_open;
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .when(!self.checkpoints.is_empty(), |header| {
                                header.child(
                                    div()
                                        .id("composer-checkpoint-center")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[1], 0.12))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(SharedString::from(format!(
                                            "Checkpoints {}",
                                            self.checkpoints.len()
                                        )))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.checkpoint_center_open =
                                                    !this.checkpoint_center_open;
                                                cx.notify();
                                            }),
                                    ),
                                )
                            })
                            .when(!self.skills.is_empty() || self.skill_scan_error.is_some(), |header| {
                                header.child(
                                    div()
                                        .id("composer-skill-center")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(crate::ui::border(&p))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(SharedString::from(format!(
                                            "Skills {}",
                                            self.skills.len()
                                        )))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.skill_center_open = !this.skill_center_open;
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
                            .when(
                                !self.memory.candidates.is_empty() || !self.memory.entries.is_empty(),
                                |header| {
                                    header.child(
                                        div()
                                            .id("composer-memory-center")
                                            .px_2()
                                            .py(px(2.0))
                                            .rounded_md()
                                            .border_1()
                                            .border_color(crate::ui::border(&p))
                                            .text_xs()
                                            .cursor_pointer()
                                            .child(SharedString::from(format!(
                                                "Memory {}",
                                                self.memory.candidates.len()
                                                    + self.memory.entries.len()
                                            )))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.memory_center_open =
                                                        !this.memory_center_open;
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                },
                            )
                            .child({
                                let environment_palette = p.clone();
                                div()
                                    .id("composer-environment")
                                    .px_2()
                                    .py(px(2.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(crate::ui::border(&p))
                                    .text_xs()
                                    .child(SharedString::from(environment_id))
                                    .tooltip(move |_window, cx| {
                                        Tooltip::view(
                                            environment_tooltip.clone(),
                                            &environment_palette,
                                            cx,
                                        )
                                    })
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .child(
                                crate::ui::icon_button(
                                    "composer-dock",
                                    self.dock.toggle_icon(),
                                    self.dock.toggle_label(),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ComposerDockToggle);
                                    }),
                                ),
                            )
                            .child(
                                crate::ui::icon_button(
                                    "composer-collapse",
                                    Icon::ChevronDown,
                                    "Hide agent panel (Ctrl+I)",
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(ComposerCollapse);
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |_, _, window, cx| {
                        window.handle_input(&input_focus, handler, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .when(!compact, |root| {
                // 消息与审批/评审卡片同属对话流，共享滚动区：面板调矮后
                // Approve/Reject 等操作滚动可达，而不是被固定高度裁掉。
                root.child(
                    div()
                        .id("composer-messages")
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_handle)
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .gap_2()
                        .children(messages)
                        .children(memory_center)
                        .children(skill_center)
                        .children(checkpoint_center)
                        .children(automation_center)
                        .children(recovery_center)
                        .children(context_inspector)
                        .children(approval)
                        .children(edit_review)
                        .children(budget_resume)
                        .children(plan_review),
                )
            })
            .child(div().flex().flex_row().px_3().gap_1().children(chips))
            .children(path_suggestions)
            .child(
                // 第 1 行：输入文本独占整行宽度，右侧停靠窄面板下长提示词仍完整可见。
                // 光标是文本内的 `|` 字符；文本保持单一文本子节点，不做 flex 拆段
                // （拆段曾在真实字体下被压成逐字换行的竖排）。
                termior_ui_kit::input_field(&p, self.focus_handle.is_focused(window))
                    .flex()
                    .items_center()
                    .mx_3()
                    .mt_2()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .text_sm()
                            .when(placeholder_active, |text| {
                                text.text_color(crate::ui::muted(&p))
                            })
                            .debug_selector(|| "composer-input-text".into())
                            .relative()
                            .child(SharedString::from(input_display))
                            // IME 候选窗锚点探针：记录输入行文本 bounds 与样式。
                            .child(crate::ime_anchor::anchor_probe(&ime_anchor)),
                    ),
            )
            .child(
                // 第 2 行：动作工具栏。图标/短 chip + 弹性空白，Send 恒在最右不被裁掉。
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .mx_3()
                    .mt_1()
                    .mb_2()
                    .min_w(px(0.0))
                    .child(
                        crate::ui::icon_button(
                            "composer-attach",
                            Icon::Paperclip,
                            "Attach file or image",
                            &p,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.pick_attachment(cx)),
                        ),
                    )
                    .child({
                        let mode_palette = p.clone();
                        div()
                            .id("composer-mode")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(crate::ui::border(&p))
                            .bg(if self.mode_menu_open {
                                crate::ui::selected_wash(&p)
                            } else {
                                crate::ui::color(p.elevated)
                            })
                            .text_color(if self.mode == Mode::Yolo {
                                crate::ui::color(p.accent)
                            } else {
                                crate::ui::color(p.foreground)
                            })
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .flex_none()
                            .text_xs()
                            .tooltip(move |_window, cx| {
                                Tooltip::view(
                                    if external_backend {
                                        "Approvals are requested by the external backend"
                                    } else {
                                        "Approval mode: Auto / Plan / Yolo"
                                    },
                                    &mode_palette,
                                    cx,
                                )
                            })
                            .child(mode_label)
                            .when(!external_backend, |chip| {
                                chip.child(crate::ui::icon(
                                    Icon::ChevronDown,
                                    icon_size::XS,
                                    crate::ui::muted(&p),
                                ))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    if external_backend {
                                        this.status = "Codex app-server owns approval policy; Termior still renders and records each request".into();
                                    } else {
                                        this.mode_menu_open = !this.mode_menu_open;
                                    }
                                    cx.notify();
                                }),
                            )
                    })
                    .child({
                        let agent_palette = p.clone();
                        let agent_tooltip = agent_tooltip.clone();
                        div()
                            .id("composer-agent")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(crate::ui::border(&p))
                            .bg(crate::ui::color(p.elevated))
                            .cursor_pointer()
                            .flex_none()
                            .max_w(px(160.0))
                            .overflow_hidden()
                            .text_xs()
                            .tooltip(move |_window, cx| {
                                Tooltip::view(agent_tooltip.clone(), &agent_palette, cx)
                            })
                            .child(SharedString::from(agent_name))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cycle_custom_agent(cx)),
                            )
                    })
                    .when_some(context_label, |toolbar, label| {
                        let inspector_palette = p.clone();
                        toolbar.child(
                            div()
                                .id("composer-context-inspector")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(crate::ui::border(&p))
                                .text_xs()
                                .text_color(crate::ui::muted(&p))
                                .cursor_pointer()
                                .tooltip(move |_window, cx| {
                                    Tooltip::view(
                                        "Context Inspector — sent, summarized, truncated, and excluded items",
                                        &inspector_palette,
                                        cx,
                                    )
                                })
                                .child(SharedString::from(label))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.context_inspector_open =
                                            !this.context_inspector_open;
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(div().flex_1().min_w(px(0.0)))
                    .child(
                        crate::ui::icon_button(
                            "composer-send",
                            if self.busy { Icon::Close } else { Icon::Send },
                            if self.busy {
                                "Stop Agent task"
                            } else {
                                "Send (Enter)"
                            },
                            &p,
                        )
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::send)),
                    ),
            )
            .when(show_status_row, |root| {
                root.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(crate::ui::muted(&p))
                        .child(SharedString::from(self.status.clone())),
                )
            })
            // 模式下拉最后挂载：绘制顺序在输入区之后，能盖住下层内容。
            .when(self.mode_menu_open && !external_backend, |root| {
                root.child(mode_menu)
            })
    }
}

#[derive(Clone)]
struct ComposerInputHandler {
    view: WeakEntity<ComposerView>,
}

impl InputHandler for ComposerInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let pos = view
            .draft
            .input
            .chars()
            .take(view.cursor)
            .collect::<String>()
            .encode_utf16()
            .count();
        Some(UTF16Selection {
            range: pos..pos,
            reversed: false,
        })
    }
    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.upgrade()?;
        let len = view.read(cx).marked_text.encode_utf16().count();
        (len > 0).then_some(0..len)
    }
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                let byte = char_to_byte(&view.draft.input, view.cursor);
                view.draft.input.insert_str(byte, text);
                view.cursor += text.chars().count();
                view.marked_text.clear();
                view.update_path_suggestions();
                cx.notify();
            });
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = text.into();
                cx.notify();
            });
            // 预编辑串变化时重新上报锚点，候选窗贴住输入行光标。
            window.invalidate_character_coordinates();
        }
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        // IME 候选窗锚点：输入行文本 bounds + 光标前缀宽度（探针每帧刷新）。
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let byte = char_to_byte(&view.draft.input, view.cursor);
        view.ime_anchor
            .caret_bounds(&view.draft.input, byte, window)
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }
}

fn new_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("session-{millis}")
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

/// 附件单入口的判型映射：认得的扩展名返回 MIME（图片附件），`None` 走文本文件附件。
/// svg 归图片。判型与 MIME 用同一张表，避免两份清单漂移。
fn image_mime(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    })
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn requested_skill_names(input: &str, skills: &[SkillMetadata]) -> Vec<String> {
    skills
        .iter()
        .filter(|skill| {
            input.split_whitespace().any(|word| {
                word.starts_with('$')
                    && word.trim_matches(|ch: char| {
                        !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_'
                    }) == skill.name
            })
        })
        .map(|skill| skill.name.clone())
        .collect()
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::TestAppContext;

    /// 用真实 ComposerView 渲染，防止输入行结构改动把文本压成逐字换行（竖排）。
    /// CJK 输入 + 光标在开头是用户踩过的状态；断言输入文本保持单行高度。
    #[test]
    fn composer_input_text_stays_on_one_line() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        let input = "你现在是什么模型为什么这样显示输入内容测试";
        composer.update(vcx, |view, _| {
            view.draft.input = input.into();
            view.cursor = 0;
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let text = vcx
            .debug_bounds("composer-input-text")
            .expect("input text rendered");
        assert!(
            text.size.height < px(30.0),
            "composer input wrapped to multiple lines: {text:?}"
        );
        assert!(
            text.size.width > px(150.0),
            "composer input collapsed: {text:?}"
        );
    }

    /// IME 候选窗锚点必须落在输入行文本上，且随光标前缀（如 CJK）右移。
    #[test]
    fn ime_anchor_tracks_composer_input_cursor() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, _| {
            view.draft.input = "你好 world".into();
            view.cursor = "你好".chars().count();
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let text = vcx
            .debug_bounds("composer-input-text")
            .expect("input text rendered");
        vcx.update(|window, cx| {
            let mut handler = ComposerInputHandler {
                view: composer.downgrade(),
            };
            let bounds = handler
                .bounds_for_range(0..0, window, cx)
                .expect("bounds_for_range must report caret bounds after paint");
            assert_eq!(bounds.origin.y, text.origin.y);
            assert!(
                bounds.origin.x > text.origin.x,
                "CJK prefix must shift anchor right: {bounds:?} vs {text:?}"
            );
            assert!(bounds.size.height > px(0.0));
        });
    }

    #[test]
    fn skills_activate_only_through_an_explicit_dollar_mention() {
        let skills = vec![SkillMetadata {
            name: "review-code".into(),
            description: "Review code".into(),
            path: PathBuf::from(".agents/skills/review-code/SKILL.md"),
            allowed_tools: Default::default(),
            body_loaded: false,
        }];

        assert!(requested_skill_names("please review-code", &skills).is_empty());
        assert_eq!(
            requested_skill_names("please use $review-code, now", &skills),
            vec!["review-code"]
        );
    }
}
