use crate::text_selection::{SegmentKey, SelectableTexts, TextSelection};
use crate::{ComposerDock, MAX_COMPOSER_HEIGHT, MIN_COMPOSER_HEIGHT};
use futures::StreamExt;
use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, App, Bounds, ClipboardEntry, ClipboardItem,
    Context, DispatchPhase, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, InputHandler,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, UTF16Selection, WeakEntity, Window,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use termior_agent_host::{
    permission_entries, BackendEvent, CodexBackend, ManagedAgentSession, PumpOutcome, TaskLaunch,
    PERMISSIONS_APPROVAL_METHOD,
};
use termior_ai::automation::{next_run_after, AutomationManager, AutomationStore};
use termior_ai::context_engine::{
    ContextAssembler, ContextCategory, ContextItem, ContextPlan, ContextSource, MemoryStatus,
    MemoryStore, RuleResolver, SkillIndex, SkillMetadata,
};
use termior_ai::slash::{self, ComposerCommand, SlashAction, SlashCommand};
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, ApprovalPolicy, ApprovalRequest, Attachment,
    AttachmentSource, CancellationToken, ChangeSetId, ComposerDraft, EditProposalSummary,
    HttpProvider, KeyringSecretStore, Message, Mode, ProviderConfig, Role, RuntimeBudgets,
    SecretStore, SessionModel, SessionStore, Snippet, SnippetError, SnippetStore, TaskCommand,
    TaskConfig, TaskRuntime, TaskState, TaskSummary, TaskSummaryStore, TerminalContext,
    TerminalContextProvider, TodoStore, ToolContract, ToolExecutor, ToolRegistry, WaitingReason,
};
use termior_explorer_core::fuzzy::fuzzy_match;
use termior_platform::AgentStatus;
use termior_preview::MarkdownDocument;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{
    CheckpointManifest, CheckpointStore, DataFiles, KeyAction, ModelProviderSettings,
    ModelSettings, RecoveryCenter, RecoveryCommand, RecoveryTaskSummary, RestoreAction, Settings,
};
use termior_ui_kit::{menu_panel, tokens::icon_size, Icon, Tooltip};

mod agent_menu;
mod model_menu;
mod notify;
mod task_details;
mod tool_blocks;

pub use agent_menu::ComposerLaunchCliAgent;
use task_details::TaskRef;

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

/// Composer 的检查面板（任务详情、上下文检查器、恢复中心等），互斥展开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposerCenter {
    TaskDetails,
    ContextInspector,
    Recovery,
    Automations,
    Checkpoints,
    Skills,
    Memory,
    Snippets,
    Todos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentBackendChoice {
    BuiltIn,
    CodexAppServer,
}

/// Codex `item/permissions/requestApproval` 在审批卡上的一项可勾选权限。
struct PermissionChoice {
    id: String,
    label: String,
    checked: bool,
}

struct ApprovalCardModel {
    headline: SharedString,
    arguments: String,
    show_arguments: bool,
    /// 权限审批才有：服务端提议的 scope，空串表示请求里没有该字段。
    scope: Option<String>,
    /// `(index, label, checked)`，index 对应 `PermissionChoice` 的位置。
    choices: Vec<(usize, String, bool)>,
}

fn scope_element_id(scope: &str) -> String {
    let token = if scope.is_empty() {
        "unspecified".to_string()
    } else {
        scope
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!("permission-scope-{token}")
}

struct ExternalPendingApproval {
    backend_request_id: String,
    tool_call_id: String,
    request_kind: String,
    arguments: String,
    /// 服务端提议的 scope（`turn` / `session` / 未知值）。缺省为空串。
    /// 真正回给 Codex 的授权范围始终是当前 turn。
    scope: String,
    permissions: Vec<PermissionChoice>,
}

impl ExternalPendingApproval {
    fn from_codex(
        backend_request_id: String,
        tool_call_id: String,
        request_kind: String,
        raw: &serde_json::Value,
    ) -> Self {
        let redactor = termior_store::StreamingRedactor::new(Vec::<String>::new());
        let arguments = redactor.redact(&raw.to_string()).text;
        let scope = raw
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let permissions = if request_kind == PERMISSIONS_APPROVAL_METHOD {
            let profile = raw.get("permissions").unwrap_or(&serde_json::Value::Null);
            permission_entries(profile)
                .into_iter()
                .map(|entry| PermissionChoice {
                    label: redactor.redact(&entry.label).text,
                    id: entry.id,
                    checked: true,
                })
                .collect()
        } else {
            Vec::new()
        };
        Self {
            backend_request_id,
            tool_call_id,
            request_kind,
            arguments,
            scope,
            permissions,
        }
    }

    fn is_permissions(&self) -> bool {
        self.request_kind == PERMISSIONS_APPROVAL_METHOD
    }

    /// 权限审批：批准回传勾选子集，拒绝回传空集。其它审批不携带权限 id。
    fn granted_permission_ids(&self, approved: bool) -> Option<Vec<String>> {
        if !self.is_permissions() {
            return None;
        }
        if !approved {
            return Some(Vec::new());
        }
        Some(
            self.permissions
                .iter()
                .filter(|choice| choice.checked)
                .map(|choice| choice.id.clone())
                .collect(),
        )
    }
}

enum ExternalCommand {
    Start {
        task_id: String,
        text: String,
    },
    Respond {
        approved: bool,
        granted_permission_ids: Option<Vec<String>>,
    },
}

/// 审批按钮交给 Codex 会话的决议，供 GPUI 测试断言勾选子集。
#[cfg(test)]
struct ExternalApprovalReply {
    approved: bool,
    granted_permission_ids: Option<Vec<String>>,
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

/// `/` 命令面板选中了应用级动作（FR-AGENT-05），由 WorkspaceView 按快捷键同一路径执行。
#[derive(Debug, Clone, Copy)]
pub struct ComposerAppAction(pub KeyAction);

/// 任务拥有的命令会话结束（由 CommandState 驱动，Stage B 6.4），供工作区路由通知。
#[derive(Debug, Clone)]
pub struct ComposerCommandNotice {
    pub session_id: String,
    pub command: String,
    pub status: AgentStatus,
}

/// 终端里对失败命令的快捷入口（Stage B 3.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAgentAction {
    /// 解释失败原因，不修改文件。
    Explain,
    /// 修复原因并在同一 cwd 重跑命令验证。
    FixAndRerun,
}

struct LiveTerminalContext {
    snapshot: Mutex<TerminalContext>,
}

impl TerminalContextProvider for LiveTerminalContext {
    fn snapshot(&self) -> TerminalContext {
        // 锁中毒不 panic：into_inner 取回数据（快照克隆无一致性风险）。
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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
    /// 当前 status 是否为「先到 Settings → Models 配置模型」引导文案；
    /// 是则空闲时在输入行内作为占位符复用（见 `model_setup_placeholder`）。
    /// 原先用 `status.contains("Settings → Models")` 之类的文本嗅探，i18n 化后
    /// 文案会随语言变化，改为在写入状态时显式维护该标记。
    needs_model_setup: bool,
    runtime: Option<AgentRuntime>,
    active_task: Option<TaskRuntime>,
    active_cancellation: Option<CancellationToken>,
    active_plan_request: bool,
    pending_approval: Option<PendingApproval>,
    external_pending_approval: Option<ExternalPendingApproval>,
    #[cfg(test)]
    last_external_approval: Option<ExternalApprovalReply>,
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
    /// `/` 命令面板的候选（FR-AGENT-05），输入以 `/` 开头时实时刷新。
    slash_suggestions: Vec<&'static SlashCommand>,
    selected_slash_suggestion: usize,
    /// `#handle` 片段补全候选（FR-AGENT-04）。
    snippet_suggestions: Vec<Snippet>,
    selected_snippet_suggestion: usize,
    /// 与 ToolExecutor 共享的应用内 TODO（FR-SESS-04），Agent 与用户读写同一份。
    todos: Arc<Mutex<TodoStore>>,
    /// 停靠位置与底部停靠高度，由 WorkspaceView 在恢复/拖拽/切换后同步。
    dock: ComposerDock,
    fill_workspace: bool,
    panel_height: f32,
    /// 会话消息区的滚动容器；贴底时跟随新输出自动下滚。
    scroll_handle: ScrollHandle,
    last_context_plan: Option<ContextPlan>,
    /// 当前展开的检查面板；同一时间至多一个，避免多个面板堆叠挤占对话区。
    open_center: Option<ComposerCenter>,
    recovery_tasks: Vec<RecoveryTaskSummary>,
    automations: AutomationStore,
    checkpoints: Vec<CheckpointManifest>,
    skills: Vec<SkillMetadata>,
    skill_scan_error: Option<String>,
    active_skills: Vec<String>,
    memory: MemoryStore,
    backend_choice: AgentBackendChoice,
    backend_capability_summary: Option<String>,
    external_session: Option<Arc<Mutex<ManagedAgentSession>>>,
    /// Agent 选择器下拉（Stage B 6.1）与打开时探测的 CLI Agent 可用性。
    agent_menu_open: bool,
    cli_agent_available: Vec<bool>,
    /// 设置里的模型配置（configure 时同步），供会话级模型选择与运行时重建使用。
    model_settings: ModelSettings,
    custom_instructions: String,
    /// 当前会话选定的模型；`None` 跟随 Agent 默认模型。随会话持久化。
    model_choice: Option<SessionModel>,
    model_menu_open: bool,
    /// 打开模型菜单时探测的各 profile 凭据可用性（与 `model_settings.profiles` 对齐）。
    model_menu_ready: Vec<bool>,
    /// 会话消息的文本选取：本帧已绘制的文本段、当前选区与是否正在拖选。
    selectable_texts: SelectableTexts,
    text_selection: Option<TextSelection>,
    selecting_text: bool,
    /// 任务详情面板（Stage B 6.3）所指的任务；任务结束后仍可查看命令结果。
    last_task: Option<TaskRef>,
    /// 活动任务固定使用的运行时：执行器的根即任务的 project_dir，审批/续跑不随全局切换漂移。
    active_runtime: Option<AgentRuntime>,
    /// 下一次提交要固定到的项目目录（终端快捷入口设置，Stage B 3.3）。
    task_project_override: Option<PathBuf>,
    /// 任务命令会话的状态边沿检测（Stage B 6.4）。
    command_watch: notify::CommandWatch,
    command_watch_running: bool,
    /// 会话里已展开输出的工具调用（按 call_id）；默认折叠，只显示命令与状态。
    expanded_tools: HashSet<String>,
    /// history 末尾是否为流式文本占位：运行时实时推送的定稿消息到达时替换它。
    streaming_draft: bool,
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
/// 检查面板区最多占 Composer 高度的比例，其余留给对话。
const CENTER_DOCK_MAX_FRACTION: f32 = 0.45;

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
            status: t!("composer.status.choose_model").to_string(),
            needs_model_setup: true,
            runtime: None,
            active_task: None,
            active_cancellation: None,
            active_plan_request: false,
            pending_approval: None,
            external_pending_approval: None,
            #[cfg(test)]
            last_external_approval: None,
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
            checkpoints: Vec::new(),
            skills: Vec::new(),
            skill_scan_error: None,
            active_skills: Vec::new(),
            memory: MemoryStore::default(),
            backend_choice: AgentBackendChoice::BuiltIn,
            backend_capability_summary: None,
            external_session: None,
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
            active_runtime: None,
            task_project_override: None,
            command_watch: notify::CommandWatch::default(),
            command_watch_running: false,
            expanded_tools: HashSet::new(),
            streaming_draft: false,
        }
    }

    /// 活动任务固定的运行时；没有活动任务时为全局配置的运行时。
    fn task_runtime(&self) -> Option<&AgentRuntime> {
        self.active_runtime.as_ref().or(self.runtime.as_ref())
    }

    /// 覆盖状态行文案；非「模型未配置」引导类状态统一走这里，
    /// 确保输入行占位符判定随之失效。
    fn set_status(&mut self, status: impl Into<SharedString>) {
        self.status = status.into().to_string();
        self.needs_model_setup = false;
    }

    /// 写入「模型未配置」引导类状态：会被输入行占位符复用。
    fn set_model_setup_status(&mut self, status: impl Into<SharedString>) {
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
        self.rebuild_runtime(cx);
    }

    /// 选定模型所在 profile（已启用），模型字段替换为所选模型。
    fn choice_profile(&self, choice: &SessionModel) -> Option<ModelProviderSettings> {
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
    fn effective_profile(&self) -> Option<ModelProviderSettings> {
        self.model_choice
            .as_ref()
            .and_then(|choice| self.choice_profile(choice))
            .or_else(|| self.model_settings.active_chat().cloned())
    }

    /// 按当前模型选择重建内置 Agent 运行时；进行中的任务持有自己的 `active_runtime`，不受影响。
    fn rebuild_runtime(&mut self, cx: &mut Context<Self>) {
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
            self.runtime = None;
            if self.backend_choice == AgentBackendChoice::CodexAppServer {
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
            self.runtime = None;
            if self.backend_choice == AgentBackendChoice::CodexAppServer {
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
                self.runtime = None;
                self.set_status(error.to_string());
                cx.notify();
                return;
            }
        };
        let executor = match self.build_executor(&root, &full_tools) {
            Ok(executor) => executor,
            Err(error) => {
                self.runtime = None;
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
                    self.runtime = None;
                    self.set_status(error);
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
        self.set_status(tf!(
            "composer.status.ready",
            "provider" => profile.display_name,
            "model" => profile.model,
            "agent" => self.active_agent_name()
        ));
        cx.notify();
    }

    /// 以 `root` 为工作区根构建工具执行器（共享终端上下文、TODO 与检查点存储）。
    fn build_executor(
        &self,
        root: &Path,
        tools: &ToolRegistry,
    ) -> Result<Arc<ToolExecutor>, termior_ai::ToolError> {
        let executor = ToolExecutor::new(root, tools.clone())?
            .with_terminal_context(self.terminal_context.clone())
            .with_todo_store(self.todos.clone(), self.data_dir.clone());
        Ok(Arc::new(match self.data_dir.as_ref() {
            Some(data_dir) => executor.with_checkpoint_store(data_dir.join("agent-checkpoints")),
            None => executor,
        }))
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

    fn active_agent_name(&self) -> SharedString {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            return SharedString::from("Codex app-server");
        }
        self.active_custom_agent
            .and_then(|index| self.custom_agents.get(index))
            .map(|agent| SharedString::from(agent.name.as_str()))
            .unwrap_or_else(|| t!("composer.agent_builtin"))
    }

    fn cycle_custom_agent(&mut self, cx: &mut Context<Self>) {
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
    fn select_backend(
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

    pub fn attach_selection(&mut self, source: AttachmentSource, label: String, text: String) {
        self.draft.attach_selection(source, label, text);
    }

    pub fn attach_terminal_output(
        &mut self,
        label: String,
        text: String,
        command: Option<termior_ai::CommandProvenance>,
    ) {
        self.draft.attach_terminal_output(label, text, command);
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
            self.set_status(t!("composer.recovery.storage_unavailable"));
            cx.notify();
            return;
        };
        let tasks_root = data_dir.join("agent-tasks");
        match RecoveryCenter::apply(&tasks_root, task_id, command) {
            Ok(operation) => {
                self.set_status(tf!(
                    "composer.recovery.applied",
                    "attempt" => operation.attempt_id,
                    "state" => format!("{:?}", operation.state)
                ));
                self.recovery_tasks = RecoveryCenter::scan(&tasks_root).unwrap_or_default();
            }
            Err(error) => self.set_status(tf!("composer.recovery.action_failed", "error" => error)),
        }
        cx.notify();
    }

    fn restore_checkpoint(&mut self, checkpoint_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.set_status(t!("composer.checkpoint.storage_unavailable"));
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
        match result {
            Ok((restored, conflicts)) => self.set_status(tf!(
                "composer.checkpoint.restored",
                "id" => checkpoint_id,
                "restored" => restored,
                "conflicts" => conflicts
            )),
            Err(error) => {
                self.set_status(tf!("composer.checkpoint.restore_failed", "error" => error))
            }
        }
        cx.notify();
    }

    fn trigger_automation(&mut self, automation_id: &str, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.as_ref() else {
            self.set_status(t!("composer.automation.storage_unavailable"));
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
        match result {
            Ok(run) => self.set_status(tf!(
                "composer.automation.queued",
                "id" => automation_id,
                "run" => run.run_id,
                "task" => run.task_id
            )),
            Err(error) => {
                self.set_status(tf!("composer.automation.trigger_failed", "error" => error))
            }
        }
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
            self.set_status(tf!("composer.memory.action_failed", "error" => error));
            cx.notify();
            return;
        }
        if let Some(data_dir) = self.data_dir.as_ref() {
            if let Err(error) = self.memory.persist(&data_dir.join("agent-memory.json")) {
                self.set_status(tf!("composer.memory.persist_failed", "error" => error));
                cx.notify();
                return;
            }
        }
        self.set_status(tf!("composer.memory.updated", "id" => id));
        cx.notify();
    }

    pub fn set_workspace_paths(&mut self, paths: Vec<String>) {
        self.workspace_paths = paths;
        self.update_suggestions();
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
            .is_none_or(|edit| edit.summary.id != proposal_id)
        {
            return Err(t!("composer.edit.no_longer_pending").to_string());
        }
        self.pending_edit = None;
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: accepted.to_vec(),
            },
            t!("composer.edit.applying"),
            cx,
        );
        Ok(t!("composer.edit.applied").to_string())
    }

    pub fn reject_reviewed_edit(
        &mut self,
        proposal_id: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if self
            .pending_edit
            .as_ref()
            .is_none_or(|edit| edit.summary.id != proposal_id)
        {
            return Err(t!("composer.edit.no_longer_pending").to_string());
        }
        self.pending_edit = None;
        self.dispatch_task_command(
            TaskCommand::ReviewChange {
                change_set_id: ChangeSetId(proposal_id.to_owned()),
                accepted_hunks: Vec::new(),
            },
            t!("composer.edit.returning_rejection"),
            cx,
        );
        Ok(t!("composer.edit.rejected").to_string())
    }

    /// 单一附件入口：一个无过滤对话框，按扩展名自动判型（图片 / 文本文件）。
    fn pick_attachment(&mut self, cx: &mut Context<Self>) {
        let mut dialog =
            rfd::AsyncFileDialog::new().set_title(t!("composer.attach_dialog_title").to_string());
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
                            view.set_status(t!("composer.status.image_attached"));
                        }
                        Err(error) => view.set_status(tf!(
                            "composer.status.image_failed",
                            "error" => error
                        )),
                    },
                    None => {
                        view.draft.attach_file(path);
                        view.set_status(t!("composer.status.file_attached"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_suggestions(&mut self) {
        self.update_path_suggestions();
        self.update_slash_suggestions();
        self.update_snippet_suggestions();
    }

    fn update_snippet_suggestions(&mut self) {
        self.snippet_suggestions = termior_ai::snippet_query(&self.draft.input, self.cursor)
            .map(|query| {
                self.snippets
                    .suggest(query)
                    .into_iter()
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.selected_snippet_suggestion = self
            .selected_snippet_suggestion
            .min(self.snippet_suggestions.len().saturating_sub(1));
    }

    /// 把光标前的 `#query` 词元替换为 `#handle `；正文在提交时才展开，输入框保持可读。
    fn accept_snippet_suggestion(&mut self, handle: String, cx: &mut Context<Self>) {
        let query_chars = termior_ai::snippet_query(&self.draft.input, self.cursor)
            .map_or(0, |query| query.chars().count() + 1);
        let start_char = self.cursor.saturating_sub(query_chars);
        let start_byte = char_to_byte(&self.draft.input, start_char);
        let end_byte = char_to_byte(&self.draft.input, self.cursor);
        let replacement = format!("#{handle} ");
        self.draft
            .input
            .replace_range(start_byte..end_byte, &replacement);
        self.cursor = start_char + replacement.chars().count();
        self.snippet_suggestions.clear();
        self.selected_snippet_suggestion = 0;
        cx.notify();
    }

    fn persist_snippets(&mut self) {
        let Some(dir) = &self.data_dir else {
            return;
        };
        if let Err(error) = DataFiles::new(dir.clone())
            .snippets::<SnippetStore>()
            .save(&self.snippets)
        {
            self.set_status(tf!("composer.snippet.save_failed", "error" => error));
        }
    }

    /// 用户在面板里勾选/删除 TODO：先落盘再更新共享状态，与 `todo_write` 的失败语义一致。
    fn update_todos(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut TodoStore) -> bool,
    ) {
        let todos = self.todos.clone();
        let mut store = todos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = store.clone();
        if !change(&mut next) {
            return;
        }
        if let Some(dir) = &self.data_dir {
            if let Err(error) = DataFiles::new(dir.clone()).todos::<TodoStore>().save(&next) {
                self.set_status(tf!("composer.todo.save_failed", "error" => error));
                cx.notify();
                return;
            }
        }
        *store = next;
        cx.notify();
    }

    fn delete_snippet(&mut self, handle: &str, cx: &mut Context<Self>) {
        if self.snippets.remove(handle) {
            self.set_status(tf!("composer.snippet.deleted", "handle" => handle));
            self.persist_snippets();
        }
        cx.notify();
    }

    fn update_slash_suggestions(&mut self) {
        self.slash_suggestions = slash::slash_query(&self.draft.input, self.cursor)
            .map(slash::match_commands)
            .unwrap_or_default();
        self.selected_slash_suggestion = self
            .selected_slash_suggestion
            .min(self.slash_suggestions.len().saturating_sub(1));
    }

    /// 执行命令面板选中的命令：先清空 `/xxx` 输入，再分派动作。
    fn run_slash_command(&mut self, command: &'static SlashCommand, cx: &mut Context<Self>) {
        self.slash_suggestions.clear();
        self.selected_slash_suggestion = 0;
        if command.takes_args {
            // 需要参数：补全命令名，等用户输入参数后按 Enter 提交。
            self.draft.input = format!("/{} ", command.name);
            self.cursor = self.draft.input.chars().count();
            self.set_status(t!(command.description_key));
            cx.notify();
            return;
        }
        self.invoke_slash_command(command, "", cx);
    }

    fn invoke_slash_command(
        &mut self,
        command: &'static SlashCommand,
        args: &str,
        cx: &mut Context<Self>,
    ) {
        self.draft.input.clear();
        self.cursor = 0;
        self.slash_suggestions.clear();
        self.selected_slash_suggestion = 0;
        match command.action {
            SlashAction::App(action) => cx.emit(ComposerAppAction(action)),
            SlashAction::Composer(command) => match command {
                ComposerCommand::NewSession => self.new_session(cx),
                ComposerCommand::ModeAuto => self.set_mode(Mode::Auto, cx),
                ComposerCommand::ModePlan => self.set_mode(Mode::Plan, cx),
                ComposerCommand::ModeYolo => self.set_mode(Mode::Yolo, cx),
                ComposerCommand::CycleAgent => self.cycle_custom_agent(cx),
                ComposerCommand::Stop => self.stop_active_task(cx),
                ComposerCommand::AttachFile => self.pick_attachment(cx),
                ComposerCommand::ToggleDock => cx.emit(ComposerDockToggle),
                ComposerCommand::ContextInspector => {
                    self.toggle_center(ComposerCenter::ContextInspector)
                }
                ComposerCommand::Skills => self.toggle_center(ComposerCenter::Skills),
                ComposerCommand::Memory => self.toggle_center(ComposerCenter::Memory),
                ComposerCommand::Recovery => self.toggle_center(ComposerCenter::Recovery),
                ComposerCommand::Checkpoints => self.toggle_center(ComposerCenter::Checkpoints),
                ComposerCommand::Automations => self.toggle_center(ComposerCenter::Automations),
                ComposerCommand::SaveSnippet => match self.snippets.define(args) {
                    Ok(handle) => {
                        self.set_status(tf!("composer.snippet.saved", "handle" => handle));
                        self.persist_snippets();
                    }
                    Err(error) => {
                        // 保留输入，便于用户修正参数后重新提交。
                        self.draft.input = format!("/snippet {args}");
                        self.cursor = self.draft.input.chars().count();
                        self.set_status(match error {
                            SnippetError::MissingBody => t!("composer.snippet.usage"),
                            SnippetError::InvalidHandle(handle) => {
                                tf!("composer.snippet.invalid_handle", "handle" => handle)
                            }
                        });
                    }
                },
                ComposerCommand::Snippets => self.toggle_center(ComposerCenter::Snippets),
                ComposerCommand::Todos => self.toggle_center(ComposerCenter::Todos),
            },
        }
        cx.notify();
    }

    /// 在消息区按下左键：命中文字则开始选取（双击选词、三击选整段、Shift 扩展），
    /// 否则清除选区。不阻止冒泡，面板的聚焦与收起菜单逻辑照常执行。
    fn begin_text_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.selectable_texts.hit(event.position) else {
            self.selecting_text = false;
            if self.text_selection.take().is_some() {
                cx.notify();
            }
            return;
        };
        // 复制快捷键要落到面板上，而不是仍持有焦点的终端（那里 Ctrl+C 是中断）。
        window.focus(&self.focus_handle, cx);
        self.selecting_text = false;
        self.text_selection = match event.click_count {
            2 => self.selectable_texts.word_at(point),
            count if count >= 3 => self.selectable_texts.segment_at(point),
            _ => {
                self.selecting_text = true;
                match self.text_selection {
                    Some(selection) if event.modifiers.shift => Some(TextSelection {
                        anchor: selection.anchor,
                        head: point,
                    }),
                    _ => Some(TextSelection::caret(point)),
                }
            }
        };
        cx.notify();
    }

    /// 拖选期间跟踪整个窗口的鼠标：拖出消息区仍能延伸选区，任意位置松开结束。
    fn text_drag_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selecting = self.selecting_text;
        let view = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                if !selecting {
                    return;
                }
                let moved = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                    {
                        return;
                    }
                    let _ = moved.update(cx, |this, cx| {
                        this.extend_text_selection(event.position, cx)
                    });
                });
                let released = view.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = released.update(cx, |this, _| this.selecting_text = false);
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    fn extend_text_selection(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(selection) = self.text_selection.as_mut() else {
            return;
        };
        if let Some(head) = self.selectable_texts.nearest(position) {
            if selection.head != head {
                selection.head = head;
                cx.notify();
            }
        }
    }

    /// 复制选中的消息文本；没有选区时不做任何事。
    fn copy_text_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self
            .text_selection
            .filter(|selection| !selection.is_empty())
        else {
            return;
        };
        let text = self.selectable_texts.selected_text(selection);
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status(t!("composer.status.selection_copied"));
        cx.notify();
    }

    /// 开始一个新的命名会话；当前会话已镜像落盘，忙碌或有挂起审批时拒绝。
    fn new_session(&mut self, cx: &mut Context<Self>) {
        if self.busy
            || self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
        {
            self.set_status(t!("composer.status.session_busy"));
            return;
        }
        self.persist_session();
        self.sessions.create(new_session_id());
        self.history.clear();
        self.expanded_tools.clear();
        self.text_selection = None;
        self.active_task = None;
        self.last_context_plan = None;
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = false;
        self.external_session = None;
        self.draft = ComposerDraft::default();
        self.mode = Mode::Auto;
        if self.model_choice.take().is_some() {
            self.rebuild_runtime(cx);
        }
        self.persist_session();
        self.set_status(t!("composer.status.session_new"));
        cx.notify();
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

    /// 在光标处插入文本，光标按字符数推进到插入内容之后。键盘输入、IME
    /// 提交与剪贴板粘贴共用；同时清空预编辑串并刷新补全建议。
    fn insert_text_at_cursor(&mut self, text: &str, cx: &mut Context<Self>) {
        let byte = char_to_byte(&self.draft.input, self.cursor);
        self.draft.input.insert_str(byte, text);
        self.cursor += text.chars().count();
        self.marked_text.clear();
        self.update_suggestions();
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
                // 剪贴板是纯文本时插入输入框光标处；图片/外部路径才作为附件。
                ClipboardEntry::String(text) => self.insert_text_at_cursor(&text.text, cx),
            }
        }
        if attached {
            self.set_status(t!("composer.status.clipboard_attached"));
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
        *self
            .terminal_context
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = TerminalContext {
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
    fn ensure_command_watch(&mut self, cx: &mut Context<Self>) {
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

    fn submit_external(&mut self, cx: &mut Context<Self>) {
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

    fn dispatch_external_command(
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

    fn dispatch_task_command(
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

    fn apply_task_result(&mut self, result: TaskRunResult, cx: &mut Context<Self>) {
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

    /// Yolo 即 Agent 完全自主：挂起中的工具审批直接批准、变更审阅直接接受全部 hunk。
    /// 正常 Yolo 任务在运行时内部就已自动放行；这里兜底任务在 Auto 下挂起后才切到 Yolo 的情形。
    fn auto_resolve_in_yolo(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::Yolo || self.busy {
            return;
        }
        if self.pending_approval.is_some() {
            self.approve_tool(cx);
        } else if let Some(edit) = self.pending_edit.as_ref() {
            let id = edit.summary.id.clone();
            let hunks = edit.summary.hunk_ids.clone();
            let _ = self.apply_reviewed_edit(&id, &hunks, cx);
        }
    }

    fn approval_policy(&self) -> ApprovalPolicy {
        if self.mode == Mode::Yolo {
            ApprovalPolicy::Yolo
        } else {
            ApprovalPolicy::Prompt
        }
    }

    fn persist_task_summary(&self, runtime: &TaskRuntime) {
        let Some(dir) = self.data_dir.as_deref() else {
            return;
        };
        let mut store = TaskSummaryStore::load(dir).unwrap_or_default();
        let sequence = runtime.events().last().map(|event| event.sequence);
        store.upsert(TaskSummary::from_task(runtime.task(), sequence));
        if let Err(error) = store.persist(dir) {
            log::warn!("failed to persist task summary: {error}");
        }
    }

    fn dispatch_external_approval(
        &mut self,
        approved: bool,
        pending: ExternalPendingApproval,
        status: SharedString,
        cx: &mut Context<Self>,
    ) {
        let granted_permission_ids = pending.granted_permission_ids(approved);
        #[cfg(test)]
        {
            self.last_external_approval = Some(ExternalApprovalReply {
                approved,
                granted_permission_ids: granted_permission_ids.clone(),
            });
        }
        self.dispatch_external_command(
            ExternalCommand::Respond {
                approved,
                granted_permission_ids,
            },
            status,
            cx,
        );
    }

    fn toggle_permission(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(pending) = self.external_pending_approval.as_mut() else {
            return;
        };
        let Some(choice) = pending.permissions.get_mut(index) else {
            return;
        };
        choice.checked = !choice.checked;
        cx.notify();
    }

    fn approve_tool(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(pending) = self.external_pending_approval.take() {
            self.dispatch_external_approval(
                true,
                pending,
                t!("composer.status.returning_approval_codex"),
                cx,
            );
            return;
        }
        let Some(pending) = self.pending_approval.take() else {
            return;
        };
        let label = tf!(
            "composer.status.running_tool",
            "tool" => pending.request.tool_name
        );
        self.dispatch_task_command(
            TaskCommand::ResolveApproval {
                call_id: pending.request.call_id,
                approved: true,
            },
            label,
            cx,
        );
    }

    fn reject_tool(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.external_pending_approval.take() {
            self.dispatch_external_approval(
                false,
                pending,
                t!("composer.status.returning_rejection_codex"),
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
                t!("composer.status.returning_rejection"),
                cx,
            );
        }
    }

    /// 追加流式文本增量：把 TextDelta 实时拼到当前 assistant 消息草稿上。
    /// 若 history 末尾不是 assistant，先 push 一条空 assistant 作为流式占位。
    /// 最终消息会在 apply_agent_result 时被 AgentOutcome.messages 整体覆盖。
    fn append_text_delta(&mut self, delta: String) {
        let draft_open = self.streaming_draft
            && matches!(self.history.last(), Some(m) if m.role == Role::Assistant);
        if !draft_open {
            self.history.push(Message::assistant(String::new()));
        }
        self.streaming_draft = true;
        if let Some(last) = self.history.last_mut() {
            last.content.push_str(&delta);
        }
    }

    /// 运行时追加到历史的定稿消息（实时推送）：assistant 消息替换流式占位，工具结果直接追加；
    /// 用户/系统消息在提交时已显示或不属于会话视图，忽略。命令处理返回后
    /// apply_task_result 仍以运行时历史整体覆盖，这里只负责运行期间的即时显示。
    fn apply_live_message(&mut self, message: Message) {
        match message.role {
            Role::Assistant => {
                let draft_open = self.streaming_draft
                    && matches!(self.history.last(), Some(m) if m.role == Role::Assistant);
                match self.history.last_mut() {
                    Some(last) if draft_open => *last = message,
                    _ => self.history.push(message),
                }
            }
            Role::Tool => self.history.push(message),
            Role::User | Role::System => return,
        }
        self.streaming_draft = false;
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
            self.set_status(t!("composer.status.decide_hunks"));
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
    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            self.set_status(t!("composer.status.codex_owns_approvals"));
            cx.notify();
            return;
        }
        let pending = self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some();
        if self.busy || (pending && mode != Mode::Yolo) {
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

    fn apply_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
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
        self.set_status(t!("composer.plan.rejected"));
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
            self.set_status(t!("composer.status.cancelling"));
            cx.emit(AgentStatus::Attention);
            cx.notify();
            return;
        }
        if self.active_task.is_some() {
            self.dispatch_task_command(TaskCommand::Cancel, t!("composer.status.cancelling"), cx);
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
        if primary && event.keystroke.key.eq_ignore_ascii_case("c") {
            self.copy_text_selection(cx);
            return;
        }
        if primary && event.keystroke.key.eq_ignore_ascii_case("v") {
            self.schedule_attach_clipboard(cx);
            return;
        }
        if !self.slash_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_slash_suggestion =
                        self.selected_slash_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_slash_suggestion = (self.selected_slash_suggestion + 1)
                        .min(self.slash_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" | "tab" if !event.keystroke.modifiers.shift => {
                    if let Some(command) = self
                        .slash_suggestions
                        .get(self.selected_slash_suggestion)
                        .copied()
                    {
                        self.run_slash_command(command, cx);
                    }
                    return;
                }
                "escape" => {
                    self.slash_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        if !self.snippet_suggestions.is_empty() {
            match event.keystroke.key.as_str() {
                "up" => {
                    self.selected_snippet_suggestion =
                        self.selected_snippet_suggestion.saturating_sub(1);
                    cx.notify();
                    return;
                }
                "down" => {
                    self.selected_snippet_suggestion = (self.selected_snippet_suggestion + 1)
                        .min(self.snippet_suggestions.len().saturating_sub(1));
                    cx.notify();
                    return;
                }
                "enter" | "return" | "tab" if !event.keystroke.modifiers.shift => {
                    if let Some(snippet) = self
                        .snippet_suggestions
                        .get(self.selected_snippet_suggestion)
                    {
                        let handle = snippet.handle.clone();
                        self.accept_snippet_suggestion(handle, cx);
                    }
                    return;
                }
                "escape" => {
                    self.snippet_suggestions.clear();
                    cx.notify();
                    return;
                }
                _ => {}
            }
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
            "escape" if self.agent_menu_open => self.agent_menu_open = false,
            "escape" if self.model_menu_open => self.model_menu_open = false,
            "escape" if self.text_selection.is_some() => self.text_selection = None,
            "escape" if self.open_center.is_some() => self.open_center = None,
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
        self.update_suggestions();
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
        !self.fill_workspace
            && self.dock == ComposerDock::Bottom
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
            && self.slash_suggestions.is_empty()
            && self.snippet_suggestions.is_empty()
            && !self.any_center_open()
    }

    /// 任一检查面板展开时都不能收缩成 compact，否则面板被裁掉（`/skills` 等在空会话中也要可见）。
    fn any_center_open(&self) -> bool {
        self.open_center.is_some()
    }

    fn center_open(&self, center: ComposerCenter) -> bool {
        self.open_center == Some(center)
    }

    /// 打开某个面板时收起其它面板；再次触发同一面板则收起。
    fn toggle_center(&mut self, center: ComposerCenter) {
        self.open_center = (!self.center_open(center)).then_some(center);
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

    fn model_setup_placeholder(&self) -> Option<&str> {
        if self.backend_choice == AgentBackendChoice::CodexAppServer {
            return None;
        }
        if self.runtime.is_some() {
            return None;
        }
        self.needs_model_setup.then_some(self.status.as_str())
    }
}

impl EventEmitter<AgentStatus> for ComposerView {}
impl EventEmitter<EditReviewRequested> for ComposerView {}
impl EventEmitter<ComposerDockToggle> for ComposerView {}
impl EventEmitter<ComposerCollapse> for ComposerView {}
impl EventEmitter<ComposerAppAction> for ComposerView {}
impl EventEmitter<ComposerLaunchCliAgent> for ComposerView {}
impl EventEmitter<ComposerCommandNotice> for ComposerView {}

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
        let agent_name = self.active_agent_name();
        let agent_tooltip = self
            .backend_capability_summary
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| t!("composer.agent_tooltip"));
        let external_backend = self.backend_choice == AgentBackendChoice::CodexAppServer;
        let mode_label = if external_backend {
            t!("composer.backend_approval")
        } else {
            SharedString::from(self.mode.label())
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
            tf!(
                "composer.environment_external",
                "host" => std::env::consts::OS,
                "root" => self.workspace_root.display()
            )
        } else {
            tf!(
                "composer.environment_builtin",
                "host" => std::env::consts::OS,
                "root" => self.workspace_root.display()
            )
        };
        let context_label = self.context_plan().map(|plan| {
            tf!(
                "composer.context_summary",
                "tokens" => plan.total_tokens,
                "items" => plan.entries.len()
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
        // 工具结果按 call_id 并入发起它的 assistant 消息显示；找不到发起方的结果单独成块。
        let issued_calls: HashSet<&str> = self
            .history
            .iter()
            .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
            .collect();
        let tool_results: HashMap<&str, &termior_ai::ToolResult> = self
            .history
            .iter()
            .filter_map(|message| message.tool_result.as_ref())
            .map(|result| (result.call_id.as_str(), result))
            .collect();
        // 选区坐标以消息在会话中的序号分组：历史只追加，序号在流式更新间保持稳定。
        let conversation = self
            .history
            .iter()
            .filter(|message| match message.role {
                Role::User | Role::Assistant => true,
                Role::Tool => message
                    .tool_result
                    .as_ref()
                    .is_some_and(|result| !issued_calls.contains(result.call_id.as_str())),
                Role::System => false,
            })
            .enumerate()
            .collect::<Vec<_>>();
        let skipped = conversation
            .len()
            .saturating_sub(COMPOSER_RENDERED_MESSAGES);
        let texts = self.selectable_texts.clone();
        texts.clear();
        let selection = self.text_selection;
        let selection_background: Hsla = crate::ui::alpha(p.accent, 0.35).into();
        let tool_style = tool_blocks::ToolBlockStyle {
            palette: &p,
            texts: &texts,
            selection,
            selection_background,
        };
        let messages = conversation
            .iter()
            .enumerate()
            .skip(skipped)
            .map(|(position, &(index, message))| {
                let is_user = message.role == Role::User;
                // 同一轮里连续的 Agent 输出（文本与工具调用）只在开头标一次名字。
                let show_label = is_user
                    || position == skipped
                    || conversation[position - 1].1.role == Role::User;
                let label = if is_user {
                    t!("composer.message_you")
                } else {
                    t!("composer.message_agent")
                };
                let label_color = if is_user {
                    crate::ui::muted(&p)
                } else {
                    crate::ui::color(p.accent)
                };
                // Agent 回复按 Markdown 渲染（复用预览的 pulldown-cmark + GPUI
                // 管线，见 SDD 依赖决策）；用户消息保持原样，避免输入里的
                // *、# 等字符被误解析成格式。
                let body = if is_user {
                    // 整条消息作为一段多行文本，复制结果与原文一致；
                    // 空消息用不断行空格占位，避免塌陷成零高度。
                    let content = message.content.replace("\r\n", "\n");
                    let content = if content.is_empty() {
                        "\u{00A0}".to_owned()
                    } else {
                        content
                    };
                    Some(div().text_xs().cursor_text().children(texts.text(
                        SegmentKey {
                            group: index,
                            seq: 0,
                        },
                        content.into(),
                        None,
                        Vec::new(),
                        selection,
                        selection_background,
                    )))
                } else if message.role == Role::Assistant
                    && (message.tool_calls.is_empty() || !message.content.trim().is_empty())
                {
                    // 流式期间每个增量都会重渲染并重新解析；消息量已被
                    // COMPOSER_RENDERED_MESSAGES 截断，解析成本可接受。
                    let document = MarkdownDocument::parse(&message.content);
                    let blocks = crate::markdown_render::render_selectable_blocks(
                        &document,
                        crate::markdown_render::Density::CHAT,
                        &format!("composer-md-{index}"),
                        &p,
                        crate::markdown_render::Selectable {
                            texts: &texts,
                            group: index,
                            selection,
                            background: selection_background,
                        },
                    );
                    Some(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .cursor_text()
                            .text_size(px(crate::markdown_render::Density::CHAT.base))
                            .line_height(relative(1.5))
                            // 首个增量到达前内容为空，用占位空行维持行高。
                            .when(blocks.is_empty(), |body| {
                                body.child(SharedString::from("\u{00A0}"))
                            })
                            .children(blocks),
                    )
                } else {
                    None
                };
                let tool_blocks = message
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(ordinal, call)| {
                        let result = tool_results.get(call.id.as_str()).copied();
                        self.tool_block(index, ordinal, call, result, &tool_style, cx)
                    })
                    .chain(message.tool_result.as_ref().map(|result| {
                        let call = termior_ai::ToolCall {
                            id: result.call_id.clone(),
                            name: t!("composer.tool.result_label").to_string(),
                            arguments: String::new(),
                        };
                        self.tool_block(index, 0, &call, Some(result), &tool_style, cx)
                    }))
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .gap(px(2.0))
                    .when(show_label, |item| {
                        item.child(div().text_xs().text_color(label_color).child(label))
                    })
                    .children(body)
                    .children(tool_blocks)
            })
            .collect::<Vec<_>>();
        // 菜单与任务详情需要 &mut cx 注册监听，须在惰性的 chips 迭代器借用 cx 之前构建。
        let agent_menu = self.agent_menu_open.then(|| {
            self.agent_menu(
                &p,
                if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                },
                cx,
            )
        });
        let model_menu = self.model_menu_open.then(|| {
            self.model_menu(
                &p,
                if show_status_row {
                    MODE_MENU_BOTTOM + STATUS_ROW_HEIGHT
                } else {
                    MODE_MENU_BOTTOM
                },
                cx,
            )
        });
        let model_label = self.model_chip_label();
        let text_drag_tracker = self.text_drag_tracker(cx);
        let task_details = self.task_details_center(&p, cx);
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
                    .child(tf!(
                        "composer.attachment_chip",
                        "label" => attachment.chip_label()
                    ))
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
        let slash_suggestions = (!self.slash_suggestions.is_empty()).then(|| {
            div()
                .id("composer-slash-palette")
                .flex()
                .flex_col()
                .mx_3()
                .mt_1()
                .max_h(px(240.0))
                .overflow_y_scroll()
                .rounded_md()
                .border_1()
                .border_color(crate::ui::color(p.accent))
                .bg(crate::ui::color(p.elevated))
                .shadow_md()
                .children(self.slash_suggestions.iter().copied().enumerate().map(
                    |(index, command)| {
                        let selected = index == self.selected_slash_suggestion;
                        div()
                            .id(SharedString::from(format!("composer-slash-{index}")))
                            .flex()
                            .flex_row()
                            .gap_3()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(selected, |row| row.bg(crate::ui::selected_wash(&p)))
                            .child(
                                div()
                                    .min_w(px(120.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(format!("/{}", command.name))),
                            )
                            .child(
                                div()
                                    .text_color(crate::ui::muted(&p))
                                    .child(t!(command.description_key)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.run_slash_command(command, cx)
                                }),
                            )
                    },
                ))
        });
        let snippet_suggestions =
            (!self.snippet_suggestions.is_empty()).then(|| {
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
                    .children(self.snippet_suggestions.iter().enumerate().map(
                        |(index, snippet)| {
                            let selected = index == self.selected_snippet_suggestion;
                            let handle = snippet.handle.clone();
                            div()
                                .id(SharedString::from(format!("composer-snippet-{index}")))
                                .flex()
                                .flex_row()
                                .gap_3()
                                .px_3()
                                .py_1()
                                .text_xs()
                                .cursor_pointer()
                                .when(selected, |row| row.bg(crate::ui::selected_wash(&p)))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(format!("#{}", snippet.handle))),
                                )
                                .child(
                                    div()
                                        .text_color(crate::ui::muted(&p))
                                        .child(SharedString::from(snippet_preview(&snippet.body))),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.accept_snippet_suggestion(handle.clone(), cx)
                                    }),
                                )
                        },
                    ))
            });
        let todo_center = self.center_open(ComposerCenter::Todos).then(|| {
            let todos = self
                .todos
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let open = todos.open_count();
            let rows = todos.items.into_iter().enumerate().map(|(index, item)| {
                let toggle_id = item.id.clone();
                let delete_id = item.id.clone();
                let done = item.done;
                div()
                    .id(SharedString::from(format!("todo-entry-{index}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(crate::ui::alpha(p.foreground, 0.04))
                    .child(
                        div()
                            .id(SharedString::from(format!("toggle-todo-{index}")))
                            .cursor_pointer()
                            .child(SharedString::from(if done { "☑" } else { "☐" }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let id = toggle_id.clone();
                                    this.update_todos(cx, |store| store.set_done(&id, !done));
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .when(done, |text| {
                                text.line_through().text_color(crate::ui::muted(&p))
                            })
                            .child(SharedString::from(item.text)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("delete-todo-{index}")))
                            .px_2()
                            .py(px(2.0))
                            .rounded_md()
                            .border_1()
                            .border_color(crate::ui::color(p.status[3]))
                            .text_xs()
                            .cursor_pointer()
                            .child(t!("composer.todo.delete"))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let id = delete_id.clone();
                                    this.update_todos(cx, |store| store.remove(&id));
                                }),
                            ),
                    )
            });
            div()
                .id("todo-center-panel")
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
                        .child(tf!("composer.todo.title", "open" => open)),
                )
                .when(rows.len() == 0, |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child(t!("composer.todo.empty")),
                    )
                })
                .children(rows)
        });
        let snippet_center = self.center_open(ComposerCenter::Snippets).then(|| {
            let rows = self
                .snippets
                .snippets
                .iter()
                .enumerate()
                .map(|(index, snippet)| {
                    let handle = snippet.handle.clone();
                    div()
                        .id(SharedString::from(format!("snippet-entry-{index}")))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .bg(crate::ui::alpha(p.foreground, 0.04))
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("#{}", snippet.handle))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(crate::ui::muted(&p))
                                .child(SharedString::from(snippet_preview(&snippet.body))),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("delete-snippet-{index}")))
                                .px_2()
                                .py(px(2.0))
                                .rounded_md()
                                .border_1()
                                .border_color(crate::ui::color(p.status[3]))
                                .text_xs()
                                .cursor_pointer()
                                .child(t!("composer.snippet.delete"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.delete_snippet(&handle, cx);
                                    }),
                                ),
                        )
                });
            div()
                .id("snippet-center-panel")
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
                        .child(t!("composer.snippet.title")),
                )
                .when(self.snippets.snippets.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child(t!("composer.snippet.empty")),
                    )
                })
                .children(rows)
        });
        let approval_model = if let Some(pending) = self.pending_approval.as_ref() {
            Some(ApprovalCardModel {
                headline: tf!(
                    "composer.approval.headline",
                    "summary" => pending.request.summary,
                    "call" => pending.request.call_id,
                    "effect" => format!("{:?}", pending.contract.side_effect)
                ),
                arguments: pending.request.arguments.clone(),
                show_arguments: true,
                scope: None,
                choices: Vec::new(),
            })
        } else {
            self.external_pending_approval.as_ref().map(|pending| {
                let permissions_request = pending.is_permissions();
                ApprovalCardModel {
                    headline: tf!(
                        "composer.approval.codex_headline",
                        "kind" => pending.request_kind,
                        "request" => pending.backend_request_id,
                        "call" => pending.tool_call_id
                    ),
                    show_arguments: !permissions_request || pending.permissions.is_empty(),
                    arguments: pending.arguments.clone(),
                    scope: permissions_request.then(|| pending.scope.clone()),
                    choices: pending
                        .permissions
                        .iter()
                        .enumerate()
                        .map(|(index, choice)| (index, choice.label.clone(), choice.checked))
                        .collect(),
                }
            })
        };
        let approval = approval_model.map(|model| {
            let scope_row = model.scope.as_ref().map(|scope| {
                let shown: SharedString = if scope.is_empty() {
                    t!("composer.approval.scope_unspecified")
                } else {
                    SharedString::from(scope.clone())
                };
                let scope_id = scope_element_id(scope);
                let scope_debug = scope_id.clone();
                div()
                    .id(SharedString::from(scope_id))
                    .debug_selector(move || scope_debug.clone())
                    .text_xs()
                    .text_color(crate::ui::muted(&p))
                    .child(tf!("composer.approval.permission_scope", "scope" => shown))
            });
            let choice_rows = model.choices.into_iter().map(|(index, label, checked)| {
                let choice_id = format!("permission-choice-{index}");
                let choice_debug = choice_id.clone();
                div()
                    .id(SharedString::from(choice_id))
                    .debug_selector(move || choice_debug.clone())
                    .flex()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .bg(if checked {
                        crate::ui::alpha(p.status[1], 0.18)
                    } else {
                        crate::ui::color(p.elevated)
                    })
                    .child(SharedString::from(format!(
                        "{}  {label}",
                        if checked { "☑" } else { "☐" }
                    )))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_permission(index, cx);
                        }),
                    )
            });
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
                .child(model.headline)
                .children(scope_row)
                .children(choice_rows)
                .when(model.show_arguments, |card| {
                    card.child(div().text_xs().child(SharedString::from(model.arguments)))
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("approve-tool")
                                .debug_selector(|| "approve-tool".into())
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[1]))
                                .text_color(crate::ui::on_color(p.status[1]))
                                .cursor_pointer()
                                .child(t!("composer.approval.approve"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.approve_tool(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-tool")
                                .debug_selector(|| "reject-tool".into())
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(crate::ui::color(p.status[3]))
                                .text_color(crate::ui::on_color(p.status[3]))
                                .cursor_pointer()
                                .child(t!("composer.approval.reject"))
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
                        .child(tf!("composer.edit.accept_hunk", "id" => id))
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
                        .child(tf!("composer.edit.reject_hunk", "id" => id))
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
                .child(tf!("composer.edit.headline", "path" => edit.summary.path))
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
                                .child(t!("composer.edit.apply"))
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
                                .child(t!("composer.edit.reject_all"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_edit(cx)),
                                ),
                        ),
                )
        });
        let context_inspector = self
            .center_open(ComposerCenter::ContextInspector)
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
                            .child(t!("composer.context_inspector"))
                            .child(tf!(
                                "composer.context_tokens",
                                "used" => plan.total_tokens,
                                "limit" => plan.hard_limit
                            )),
                    )
                    .children(rows)
            });
        let recovery_center = self.center_open(ComposerCenter::Recovery).then(|| {
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
                            .child(tf!(
                                "composer.recovery.attempt_state",
                                "state" => format!("{:?}", task.recovered_state),
                                "event" => task.last_trusted_event
                            )),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(&p)).child(tf!(
                        "composer.recovery.task_detail",
                        "persisted" => task.persisted_state,
                        "backend" => task
                            .backend_id
                            .clone()
                            .unwrap_or_else(|| t!("composer.recovery.unknown_backend").to_string()),
                        "actions" => task.available_actions.join(", ")
                    )))
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
                                            .child(t!("composer.recovery.prepare_retry"))
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
                                            .child(t!("composer.recovery.abandon"))
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
                        .child(t!("composer.recovery.title")),
                )
                .when(self.recovery_tasks.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child(t!("composer.recovery.empty")),
                    )
                })
                .children(rows)
        });
        let automation_center = self.center_open(ComposerCenter::Automations).then(|| {
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
                                        t!("composer.automation.enabled")
                                    } else {
                                        t!("composer.automation.disabled")
                                    }),
                            )
                            .child(div().text_xs().text_color(crate::ui::muted(&p)).child(tf!(
                                "composer.automation.detail",
                                "version" => automation.template.version,
                                "next" => next,
                                "last" => last,
                                "trigger" => format!("{:?}", automation.trigger)
                            )))
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
                                        .child(t!("composer.automation.queue_run"))
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
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("composer.automation.title")),
                )
                .when(self.automations.automations.is_empty(), |panel| {
                    panel.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::muted(&p))
                            .child(t!("composer.automation.empty")),
                    )
                })
                .children(rows)
        });
        let checkpoint_center = self.center_open(ComposerCenter::Checkpoints).then(|| {
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
                                .child(tf!(
                                    "composer.checkpoint.meta",
                                    "task" => checkpoint.task_id,
                                    "event" => checkpoint.sequence
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(&p))
                                .child(tf!("composer.checkpoint.files", "files" => files)),
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
                                .child(t!("composer.checkpoint.restore"))
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
                        .child(t!("composer.checkpoint.title")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(&p))
                        .child(t!("composer.checkpoint.note")),
                )
                .children(rows)
        });
        let skill_center = self.center_open(ComposerCenter::Skills).then(|| {
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
                                t!("composer.skills.active")
                            } else {
                                t!("composer.skills.available")
                            }),
                    )
                    .child(div().text_xs().text_color(crate::ui::muted(&p)).child(tf!(
                        "composer.skills.detail",
                        "description" => skill.description,
                        "source" => skill.path.display(),
                        "tools" => if skill.allowed_tools.is_empty() {
                            t!("composer.skills.task_tool_set").to_string()
                        } else {
                            skill
                                .allowed_tools
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        }
                    )))
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
                        .child(t!("composer.skills.title")),
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
        let memory_center = self.center_open(ComposerCenter::Memory).then(|| {
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
                            .child(tf!(
                                "composer.memory.candidate",
                                "id" => candidate.id,
                                "confidence" => candidate.confidence
                            ))
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
                                            .child(t!("composer.memory.accept"))
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
                                            .child(t!("composer.memory.reject"))
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
                                        .child(if entry.enabled {
                                            t!("composer.memory.pause")
                                        } else {
                                            t!("composer.memory.resume")
                                        })
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
                                        .child(t!("composer.memory.delete"))
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
                        .child(t!("composer.memory.title")),
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
                    .child(t!("composer.budget.reached"))
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
                            .child(t!("composer.budget.continue"))
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
                .child(t!("composer.plan.awaiting"))
                .child(
                    div()
                        .id("confirm-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(crate::ui::color(p.status[1]))
                        .text_color(crate::ui::on_color(p.status[1]))
                        .cursor_pointer()
                        .child(t!("composer.plan.confirm"))
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
                        .child(t!("composer.plan.reject"))
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
                        // 档位名 Auto/Plan/Yolo 是专有名词保留原文；一句话说明走 i18n。
                        let description = match mode {
                            Mode::Auto => t!("composer.mode.auto_desc"),
                            Mode::Plan => t!("composer.mode.plan_desc"),
                            Mode::Yolo => t!("composer.mode.yolo_desc"),
                        };
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
                            .child(div().text_color(crate::ui::muted(&p)).child(description))
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
        // 面板互斥展开，至多一个为 Some。
        let center_panel = task_details
            .or(todo_center.map(IntoElement::into_any_element))
            .or(snippet_center.map(IntoElement::into_any_element))
            .or(memory_center.map(IntoElement::into_any_element))
            .or(skill_center.map(IntoElement::into_any_element))
            .or(checkpoint_center.map(IntoElement::into_any_element))
            .or(automation_center.map(IntoElement::into_any_element))
            .or(recovery_center.map(IntoElement::into_any_element))
            .or(context_inspector.map(IntoElement::into_any_element));
        div()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .when(
                self.fill_workspace || self.dock == ComposerDock::Right,
                |root| root.w_full().h_full(),
            )
            .when(
                !self.fill_workspace && self.dock == ComposerDock::Bottom,
                |root| {
                    root.w_full()
                        .when(compact, |root| root.min_h(px(COMPOSER_COMPACT_HEIGHT)))
                        .when(!compact, |root| root.h(px(self.panel_height)))
                },
            )
            // 分隔线由工作区在面板外沿绘制的拖拽手柄承担。
            .bg(crate::ui::color(p.elevated))
            .text_color(crate::ui::color(p.foreground))
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            // 面板空白处点击收起模式下拉；chip 与菜单项各自 stop_propagation。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.mode_menu_open || this.agent_menu_open || this.model_menu_open {
                        this.mode_menu_open = false;
                        this.agent_menu_open = false;
                        this.model_menu_open = false;
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
                                    .child(t!("composer.header")),
                            )
                            .when(self.last_task.is_some(), |header| {
                                header.child(
                                    div()
                                        .id("composer-task-details-badge")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_md()
                                        .bg(crate::ui::alpha(p.status[0], 0.12))
                                        .text_xs()
                                        .cursor_pointer()
                                        .child(t!("composer.task.badge"))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::TaskDetails);
                                                if this.center_open(ComposerCenter::TaskDetails) {
                                                    this.ensure_command_watch(cx);
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
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
                                        .child(tf!(
                                            "composer.recovery.badge",
                                            "count" => self.recovery_tasks.len()
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::Recovery);
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
                                    .child(tf!(
                                        "composer.automation.badge",
                                        "count" => self.automations.automations.len()
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_center(ComposerCenter::Automations);
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
                                        .child(tf!(
                                            "composer.checkpoint.badge",
                                            "count" => self.checkpoints.len()
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_center(ComposerCenter::Checkpoints);
                                                cx.notify();
                                            }),
                                        ),
                                )
                            })
                            .when(
                                !self.skills.is_empty() || self.skill_scan_error.is_some(),
                                |header| {
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
                                            .child(tf!(
                                                "composer.skills.badge",
                                                "count" => self.skills.len()
                                            ))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_center(ComposerCenter::Skills);
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                },
                            )
                            .when(
                                !self.memory.candidates.is_empty()
                                    || !self.memory.entries.is_empty(),
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
                                            .child(tf!(
                                                "composer.memory.badge",
                                                "count" => self.memory.candidates.len()
                                                    + self.memory.entries.len()
                                            ))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.toggle_center(ComposerCenter::Memory);
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
                                    t!(self.dock.toggle_label_key()),
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
                                    t!("composer.hide_panel"),
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
                        .debug_selector(|| "composer-messages".into())
                        .relative()
                        .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_text_selection))
                        .child(text_drag_tracker)
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
                        .children(approval)
                        .children(edit_review)
                        .children(budget_resume)
                        .children(plan_review),
                )
                // 检查面板独立于对话流：限高并自带滚动，展开后最近的回复仍然可见。
                .when_some(center_panel, |root, panel| {
                    root.child(
                        div()
                            .id("composer-center-dock")
                            .debug_selector(|| "composer-center-dock".into())
                            .flex()
                            .flex_row()
                            .flex_none()
                            .max_h(relative(CENTER_DOCK_MAX_FRACTION))
                            .mx_3()
                            .mb_1()
                            .gap_1()
                            .child(
                                div()
                                    .id("composer-center-scroll")
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_y_scroll()
                                    .child(panel),
                            )
                            .child(
                                crate::ui::icon_button(
                                    "composer-center-close",
                                    Icon::Close,
                                    t!("composer.close_center"),
                                    &p,
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.open_center = None;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    )
                })
            })
            .child(div().flex().flex_row().px_3().gap_1().children(chips))
            .children(slash_suggestions)
            .children(snippet_suggestions)
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
                            t!("composer.attach_hint"),
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
                                        t!("composer.mode.backend_hint")
                                    } else {
                                        t!("composer.mode.hint")
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
                                        this.set_status(t!(
                                            "composer.status.codex_owns_approvals_hint"
                                        ));
                                    } else {
                                        this.mode_menu_open = !this.mode_menu_open;
                                        this.model_menu_open = false;
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
                            .flex()
                            .items_center()
                            .gap(px(3.0))
                            .when(self.agent_menu_open, |chip| {
                                chip.bg(crate::ui::selected_wash(&p))
                            })
                            .child(agent_name)
                            .child(crate::ui::icon(
                                Icon::ChevronDown,
                                icon_size::XS,
                                crate::ui::muted(&p),
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_agent_menu(cx);
                                }),
                            )
                    })
                    // 会话级模型选择；外部后端自带模型来源，不显示。
                    .when(!external_backend, |toolbar| {
                        let model_palette = p.clone();
                        toolbar.child(
                            div()
                                .id("composer-model")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(crate::ui::border(&p))
                                .bg(if self.model_menu_open {
                                    crate::ui::selected_wash(&p)
                                } else {
                                    crate::ui::color(p.elevated)
                                })
                                .cursor_pointer()
                                .flex_none()
                                .max_w(px(180.0))
                                .overflow_hidden()
                                .text_xs()
                                .flex()
                                .items_center()
                                .gap(px(3.0))
                                .tooltip(move |_window, cx| {
                                    Tooltip::view(
                                        t!("composer.model_menu.hint"),
                                        &model_palette,
                                        cx,
                                    )
                                })
                                .child(model_label)
                                .child(crate::ui::icon(
                                    Icon::ChevronDown,
                                    icon_size::XS,
                                    crate::ui::muted(&p),
                                ))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_model_menu(cx);
                                    }),
                                ),
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
                                        t!("composer.context_inspector_hint"),
                                        &inspector_palette,
                                        cx,
                                    )
                                })
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_center(ComposerCenter::ContextInspector);
                                        cx.notify();
                                    }),
                                ),
                        )
                    })
                    .child(div().flex_1().min_w(px(0.0)))
                    .child(
                        crate::ui::icon_button(
                            "composer-send",
                            if self.busy { Icon::Stop } else { Icon::Send },
                            if self.busy {
                                t!("composer.stop_task")
                            } else {
                                t!("composer.send_hint")
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
            .when_some(agent_menu, |root, menu| root.child(menu))
            .when_some(model_menu, |root, menu| root.child(menu))
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
                view.insert_text_at_cursor(text, cx);
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

/// 片段正文的单行预览，用于补全弹层与片段面板。
fn snippet_preview(body: &str) -> String {
    const MAX_CHARS: usize = 60;
    let line = body.lines().next().unwrap_or_default();
    let mut preview: String = line.chars().take(MAX_CHARS).collect();
    if line.chars().count() > MAX_CHARS || body.lines().nth(1).is_some() {
        preview.push('…');
    }
    preview
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

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    /// 真实布局下拖选跨越用户消息与 Agent 回复的多个段落，复制得到原文。
    #[test]
    fn dragging_over_messages_selects_and_copies_text() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, _| {
            // 面板足够高，内容不触发贴底滚动，第一条消息完整可见。
            view.panel_height = 600.0;
            view.history.push(Message::user("请解释一下"));
            view.history.push(Message::assistant(
                "First paragraph\n\nSecond **bold** line",
            ));
        });
        draw(vcx);
        let segments = composer.update(vcx, |view, _| view.selectable_texts.segment_bounds());
        let keys = segments.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                SegmentKey { group: 0, seq: 0 },
                SegmentKey { group: 1, seq: 0 },
                SegmentKey { group: 1, seq: 1 },
            ]
        );
        let start = segments[0].1.origin + gpui::point(px(1.0), segments[0].1.size.height / 2.0);
        let last = segments[2].1;
        let end = gpui::point(last.right() + px(40.0), last.bottom() + px(4.0));

        vcx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
        draw(vcx);
        vcx.simulate_mouse_move(end, MouseButton::Left, gpui::Modifiers::none());

        vcx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::none());
        composer.update(vcx, |view, _| {
            assert!(!view.selecting_text, "mouse up ends the drag")
        });
        vcx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("selection copied");
        assert_eq!(
            copied,
            "请解释一下\n\nFirst paragraph\n\nSecond **bold** line"
        );

        // 点在消息区内、文字之外的空白处清除选区。
        let blank = gpui::point(start.x, last.bottom() + px(40.0));
        vcx.simulate_mouse_down(blank, MouseButton::Left, gpui::Modifiers::none());
        composer.update(vcx, |view, _| assert!(view.text_selection.is_none()));
    }

    /// 完整选中的 Markdown 段复制源码：标记、链接、列表标记与代码围栏保留，
    /// 段间空行分隔；部分选取的段仍按渲染文本精确截取。
    #[test]
    fn selecting_markdown_blocks_copies_markdown_source() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        let message = concat!(
            "修复 `run()` 里的 **竞态**，见 [笔记](https://example.com/a)。\n\n",
            "- 先停任务\n",
            "- 再清理缓存\n\n",
            "```rust\nfn main() {}\n```"
        );
        composer.update(vcx, |view, _| {
            view.panel_height = 600.0;
            view.history.push(Message::assistant(message));
        });
        draw(vcx);
        let segments = composer.update(vcx, |view, _| view.selectable_texts.segment_bounds());
        // 段落、两个列表项、代码块各一段。
        assert_eq!(segments.len(), 4, "paragraph + two items + code block");
        let first = segments[0].1;
        let last = segments[3].1;
        // 按下必须命中段内（hit 而非 nearest）才会开始选取；拖动与抬起可用
        // 段外坐标，nearest 把端点吸附到首/末段边界，得到全选。
        let start = first.origin + gpui::point(px(1.0), first.size.height / 2.0);
        let end = gpui::point(last.right() + px(4.0), last.bottom() + px(4.0));
        vcx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
        draw(vcx);
        vcx.simulate_mouse_move(end, MouseButton::Left, gpui::Modifiers::none());
        vcx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::none());
        vcx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("selection copied");
        assert_eq!(
            copied,
            concat!(
                "修复 `run()` 里的 **竞态**，见 [笔记](https://example.com/a)。\n\n",
                "- 先停任务\n\n",
                "- 再清理缓存\n\n",
                "```rust\nfn main() {}\n```"
            )
        );
    }

    fn segment_keys(
        composer: &gpui::Entity<ComposerView>,
        vcx: &mut gpui::VisualTestContext,
    ) -> Vec<SegmentKey> {
        composer.update(vcx, |view, _| {
            view.selectable_texts
                .segment_bounds()
                .into_iter()
                .map(|(key, _)| key)
                .collect()
        })
    }

    /// Agent 的命令在会话里显示为折叠块：默认只有命令与状态，展开后输出可选取复制。
    #[test]
    fn tool_calls_render_as_collapsible_blocks() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, _| {
            view.panel_height = 600.0;
            view.history.push(Message::user("跑一下测试"));
            view.history.push(Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: vec![termior_ai::ToolCall {
                    id: "call-1".into(),
                    name: "run_command".into(),
                    arguments: r#"{"command":"cargo test"}"#.into(),
                }],
                tool_result: None,
            });
            view.history.push(Message {
                role: Role::Tool,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_result: Some(termior_ai::ToolResult::success(
                    "call-1",
                    "session_id=s1 state=Exited exit=0 timed_out=false cursor=2\ntest result: ok",
                )),
            });
        });
        draw(vcx);
        assert_eq!(
            segment_keys(&composer, vcx),
            vec![SegmentKey { group: 0, seq: 0 }],
            "collapsed block shows no output text"
        );

        composer.update(vcx, |view, cx| view.toggle_tool_block("call-1", cx));
        draw(vcx);
        let keys = segment_keys(&composer, vcx);
        assert_eq!(keys.len(), 2, "expanded block adds the output segment");
        assert_eq!(keys[1].group, 1, "output belongs to the issuing message");
    }

    /// 运行中实时推送的定稿 assistant 消息替换流式占位，工具结果随即追加。
    #[test]
    fn live_messages_replace_the_streaming_draft() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, _| {
            view.history.push(Message::user("hi"));
            view.append_text_delta("Let me ".into());
            view.append_text_delta("check".into());
            let mut assistant = Message::assistant("Let me check");
            assistant.tool_calls.push(termior_ai::ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            });
            view.apply_live_message(Message::user("hi"));
            view.apply_live_message(assistant);
            view.apply_live_message(Message {
                role: Role::Tool,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_result: Some(termior_ai::ToolResult::success("call-1", "fn main() {}")),
            });
            view.append_text_delta("Done".into());
            let roles = view
                .history
                .iter()
                .map(|message| message.role)
                .collect::<Vec<_>>();
            assert_eq!(
                roles,
                vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
            );
            assert_eq!(view.history[1].tool_calls.len(), 1);
            assert_eq!(view.history[3].content, "Done");
        });
    }

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

    /// Ctrl+V 把剪贴板纯文本插入输入框光标处（含多行），不产生附件。
    #[test]
    fn ctrl_v_pastes_clipboard_text_at_cursor() {
        let mut cx = TestAppContext::single();
        cx.write_to_clipboard(ClipboardItem::new_string(" the bug\nsee log".to_string()));
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, _| {
            view.draft.input = "fix".into();
            view.cursor = "fix".chars().count();
        });
        press(
            &composer,
            vcx,
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
        );
        composer.update(vcx, |view, _| {
            assert_eq!(view.draft.input, "fix the bug\nsee log");
            assert_eq!(view.cursor, "fix the bug\nsee log".chars().count());
            assert!(view.draft.attachments.is_empty());
        });
    }

    fn type_text(
        composer: &gpui::Entity<ComposerView>,
        vcx: &mut gpui::VisualTestContext,
        text: &str,
    ) {
        vcx.update(|window, cx| {
            let mut handler = ComposerInputHandler {
                view: composer.downgrade(),
            };
            handler.replace_text_in_range(None, text, window, cx);
        });
    }

    #[test]
    fn slash_palette_runs_composer_commands_and_clears_input() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        type_text(&composer, vcx, "/pl");
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        composer.update(vcx, |view, cx| {
            assert_eq!(view.slash_suggestions[0].name, "plan");
            assert!(!view.is_compact(), "palette must expand the composer");
            view.run_slash_command(view.slash_suggestions[0], cx);
            assert_eq!(view.mode, Mode::Plan);
            assert!(view.draft.input.is_empty());
            assert!(view.slash_suggestions.is_empty());
        });
    }

    #[test]
    fn slash_palette_ignores_paths_inside_messages() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        type_text(&composer, vcx, "look at /usr/bin");
        composer.update(vcx, |view, _| assert!(view.slash_suggestions.is_empty()));
    }

    #[test]
    fn slash_palette_forwards_app_actions_to_the_workspace() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&composer, move |_, action: &ComposerAppAction, _| {
                sink.lock().unwrap().push(action.0);
            })
            .detach();
        });
        composer.update(vcx, |view, cx| {
            let command = slash::find_command("split-right").expect("catalogued");
            view.run_slash_command(command, cx);
        });
        assert_eq!(*received.lock().unwrap(), vec![KeyAction::SplitRight]);
    }

    fn press(composer: &gpui::Entity<ComposerView>, vcx: &mut gpui::VisualTestContext, key: &str) {
        composer.update_in(vcx, |view, window, cx| {
            let event = KeyDownEvent {
                keystroke: gpui::Keystroke::parse(key).unwrap(),
                is_held: false,
                prefer_character_input: false,
            };
            view.handle_key_down(&event, window, cx);
        });
    }

    #[test]
    fn snippet_command_saves_and_hash_completion_inserts_handle() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        type_text(&composer, vcx, "/snippet review Please review\n  carefully");
        press(&composer, vcx, "enter");
        composer.update(vcx, |view, _| {
            assert!(view.draft.input.is_empty(), "command must not be sent");
            assert!(view.history.is_empty());
            assert_eq!(view.snippets.snippets[0].handle, "review");
        });

        type_text(&composer, vcx, "do #rev");
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        composer.update(vcx, |view, _| {
            assert_eq!(view.snippet_suggestions[0].handle, "review");
            assert!(!view.is_compact());
        });
        press(&composer, vcx, "tab");
        composer.update(vcx, |view, _| {
            assert_eq!(view.draft.input, "do #review ");
            assert_eq!(view.cursor, view.draft.input.chars().count());
            assert!(view.snippet_suggestions.is_empty());
        });
    }

    #[test]
    fn invalid_snippet_command_keeps_input_for_correction() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        type_text(&composer, vcx, "/snippet onlyhandle");
        press(&composer, vcx, "enter");
        composer.update(vcx, |view, _| {
            assert_eq!(view.draft.input, "/snippet onlyhandle");
            assert!(view.snippets.snippets.is_empty());
        });
    }

    #[test]
    fn selecting_an_argument_command_only_completes_its_name() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        type_text(&composer, vcx, "/snippet");
        press(&composer, vcx, "enter");
        composer.update(vcx, |view, _| {
            assert_eq!(view.draft.input, "/snippet ");
            assert!(view.slash_suggestions.is_empty());
        });
    }

    #[test]
    fn open_panels_keep_an_empty_composer_expanded() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            assert!(view.is_compact());
            view.invoke_slash_command(slash::find_command("snippets").unwrap(), "", cx);
            assert!(view.center_open(ComposerCenter::Snippets));
            assert!(!view.is_compact());
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    #[test]
    fn opening_a_panel_closes_the_previous_one() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            view.invoke_slash_command(slash::find_command("snippets").unwrap(), "", cx);
            view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
            assert!(view.center_open(ComposerCenter::Todos));
            assert!(!view.center_open(ComposerCenter::Snippets));
            view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
            assert!(!view.any_center_open());
        });
    }

    #[test]
    fn a_tall_panel_is_capped_and_leaves_room_for_the_conversation() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        let panel_height = composer.update(vcx, |view, cx| {
            for index in 0..40 {
                view.todos
                    .lock()
                    .unwrap()
                    .add(format!("todo-{index}"), "a long backlog item");
            }
            view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
            view.panel_height
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let dock = vcx
            .debug_bounds("composer-center-dock")
            .expect("panel dock rendered");
        let messages = vcx
            .debug_bounds("composer-messages")
            .expect("conversation rendered");
        assert!(
            dock.size.height <= px(panel_height * CENTER_DOCK_MAX_FRACTION + 1.0),
            "panel dock not capped: {dock:?}"
        );
        assert!(
            dock.size.height >= px(panel_height * CENTER_DOCK_MAX_FRACTION - 1.0),
            "overflowing panel should fill its cap: {dock:?}"
        );
        assert!(
            messages.size.height > px(0.0),
            "conversation squeezed out: {messages:?}"
        );
    }

    #[test]
    fn todo_panel_shows_agent_items_and_user_edits_update_the_shared_store() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            view.todos.lock().unwrap().add("todo-1", "write tests");
            view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
            assert!(view.center_open(ComposerCenter::Todos));
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        composer.update(vcx, |view, cx| {
            view.update_todos(cx, |store| store.set_done("todo-1", true));
            assert_eq!(view.todos.lock().unwrap().open_count(), 0);
            view.update_todos(cx, |store| store.remove("todo-1"));
            assert!(view.todos.lock().unwrap().items.is_empty());
        });
    }

    #[test]
    fn new_session_command_starts_an_empty_active_session() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            view.sessions.create("old");
            view.history.push(Message::user("hello"));
            view.new_session(cx);
            assert!(view.history.is_empty());
            assert_eq!(view.sessions.sessions.len(), 2);
            assert_ne!(view.sessions.active_id.as_deref(), Some("old"));
            assert_eq!(view.sessions.get("old").unwrap().messages.len(), 1);

            view.busy = true;
            view.new_session(cx);
            assert_eq!(
                view.sessions.sessions.len(),
                2,
                "busy composer keeps its session"
            );
        });
    }

    #[test]
    fn session_model_choice_overrides_the_default_and_resets_for_new_sessions() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            let mut models = ModelSettings::default();
            for profile in &mut models.profiles {
                profile.enabled = profile.id == "anthropic";
            }
            models.active_chat_profile = Some("anthropic".into());
            view.model_settings = models;
            view.sessions.create("s1");
            assert_eq!(view.model_chip_label().as_ref(), "claude-opus-5-5");

            let choice = SessionModel {
                profile_id: "anthropic".into(),
                model: "claude-haiku-5-5".into(),
            };
            view.choose_model(Some(choice.clone()), cx);
            assert_eq!(view.model_chip_label().as_ref(), "claude-haiku-5-5");
            assert_eq!(
                view.sessions.get("s1").unwrap().model.as_ref(),
                Some(&choice),
                "the choice is remembered with the session"
            );

            // 选到未启用的服务商不生效，回落到 Agent 默认模型。
            view.choose_model(
                Some(SessionModel {
                    profile_id: "openai".into(),
                    model: "gpt-6.1-sol".into(),
                }),
                cx,
            );
            assert_eq!(view.model_chip_label().as_ref(), "claude-opus-5-5");

            view.model_choice = Some(choice);
            view.new_session(cx);
            assert!(view.model_choice.is_none());

            view.model_menu_open = true;
            view.model_menu_ready = vec![true; view.model_settings.profiles.len()];
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
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

    #[test]
    fn agent_menu_separates_backend_kinds_and_launches_cli_agents_in_a_terminal() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        let launched = Arc::new(Mutex::new(Vec::new()));
        let sink = launched.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&composer, move |_, launch: &ComposerLaunchCliAgent, _| {
                sink.lock().unwrap().push(launch.0.program);
            })
            .detach();
        });
        composer.update(vcx, |view, cx| {
            view.toggle_agent_menu(cx);
            assert!(view.agent_menu_open);
            assert_eq!(
                view.cli_agent_available.len(),
                termior_agent_host::CLI_AGENTS.len()
            );
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        composer.update(vcx, |view, cx| {
            view.choose_agent(agent_menu::AgentMenuChoice::Codex, cx);
            assert_eq!(view.backend_choice, AgentBackendChoice::CodexAppServer);
            assert!(!view.agent_menu_open);
            view.choose_agent(agent_menu::AgentMenuChoice::BuiltIn(None), cx);
            assert_eq!(view.backend_choice, AgentBackendChoice::BuiltIn);
            // 普通 CLI Agent 不改变 Composer 后端，只请求在终端里启动。
            view.choose_agent(agent_menu::AgentMenuChoice::Cli(0), cx);
            assert_eq!(view.backend_choice, AgentBackendChoice::BuiltIn);
        });
        assert_eq!(*launched.lock().unwrap(), vec!["claude"]);
    }

    #[test]
    fn terminal_action_without_a_model_keeps_the_attachment_and_drops_the_project_pin() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            view.run_terminal_action(
                TerminalAgentAction::FixAndRerun,
                "$ cargo test (exit 101)".into(),
                "error[E0425]".into(),
                Some(termior_ai::CommandProvenance {
                    id: "cmd-1".into(),
                    cwd: "/project".into(),
                    command: Some("cargo test".into()),
                    exit_code: Some(101),
                }),
                PathBuf::from("/project"),
                cx,
            );
            assert_eq!(view.draft.attachments.len(), 1);
            assert!(view.draft.input.contains("cargo test"));
            assert!(view.needs_model_setup);
            assert_eq!(view.task_project_override, None);
            assert!(view.active_task.is_none());
        });
    }

    #[test]
    fn task_details_panel_renders_for_the_last_task_and_hand_back_needs_an_agent() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        composer.update(vcx, |view, cx| {
            view.last_task = Some(TaskRef {
                task_id: "task-details-without-commands".into(),
                project_dir: PathBuf::from("/project"),
                backend_id: "native".into(),
                environment_id: "direct".into(),
            });
            view.open_center = Some(ComposerCenter::TaskDetails);
            assert!(view.any_center_open());
            assert!(view.task_digests().is_empty());
            view.hand_back_terminal(&termior_terminal::CommandSessionId("cmd-x".into()), cx);
            assert_eq!(
                view.status,
                t!("composer.task.no_agent_to_hand_back").to_string()
            );
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
    }

    fn sample_permission_profile() -> serde_json::Value {
        serde_json::json!({
            "network": {"enabled": true},
            "fileSystem": {"write": ["/proj", "/shared"]}
        })
    }

    fn show_permission_approval(
        composer: &gpui::Entity<ComposerView>,
        vcx: &mut gpui::VisualTestContext,
        scope: &str,
    ) {
        let scope = scope.to_string();
        composer.update(vcx, |view, _| {
            let raw = serde_json::json!({
                "threadId": "th-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "reason": "needs access",
                "scope": scope,
                "permissions": sample_permission_profile(),
            });
            view.panel_height = 600.0;
            view.external_pending_approval = Some(ExternalPendingApproval::from_codex(
                "7".into(),
                "item-1".into(),
                PERMISSIONS_APPROVAL_METHOD.into(),
                &raw,
            ));
        });
        draw(vcx);
    }

    fn click_id(vcx: &mut gpui::VisualTestContext, id: &'static str) {
        let bounds = vcx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("missing element {id}"));
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
    }

    fn click_permission(vcx: &mut gpui::VisualTestContext, index: usize) {
        match index {
            0 => click_id(vcx, "permission-choice-0"),
            1 => click_id(vcx, "permission-choice-1"),
            2 => click_id(vcx, "permission-choice-2"),
            other => panic!("unexpected permission choice {other}"),
        }
    }

    /// 三项权限默认全选；批准回传完整 profile，scope 固定为 turn。
    #[test]
    fn permission_card_approve_all_sends_every_checked_permission() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        show_permission_approval(&composer, vcx, "turn");
        assert!(vcx.debug_bounds("permission-scope-turn").is_some());
        assert!(vcx.debug_bounds("permission-choice-0").is_some());
        assert!(vcx.debug_bounds("permission-choice-1").is_some());
        assert!(vcx.debug_bounds("permission-choice-2").is_some());
        let expected_ids = termior_agent_host::permission_entries(&sample_permission_profile())
            .into_iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        composer.update(vcx, |view, _| {
            let pending = view.external_pending_approval.as_ref().unwrap();
            assert!(pending.permissions.iter().all(|choice| choice.checked));
            assert_eq!(
                pending
                    .permissions
                    .iter()
                    .map(|choice| choice.id.as_str())
                    .collect::<Vec<_>>(),
                expected_ids.iter().map(String::as_str).collect::<Vec<_>>()
            );
            assert_eq!(
                pending
                    .permissions
                    .iter()
                    .map(|choice| choice.label.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "fileSystem.write[0]  /proj",
                    "fileSystem.write[1]  /shared",
                    "network  {\"enabled\":true}",
                ]
            );
        });
        click_id(vcx, "approve-tool");
        composer.update(vcx, |view, _| {
            let reply = view.last_external_approval.as_ref().unwrap();
            assert!(reply.approved);
            assert_eq!(
                reply.granted_permission_ids.as_deref(),
                Some(expected_ids.as_slice())
            );
            assert_eq!(
                termior_agent_host::permissions_approval_result(
                    &sample_permission_profile(),
                    true,
                    reply.granted_permission_ids.as_deref(),
                ),
                serde_json::json!({
                    "permissions": sample_permission_profile(),
                    "scope": "turn"
                })
            );
        });
    }

    /// 取消一项后，批准只回传仍勾选的权限，服务端提议的 session 也不会被放大。
    #[test]
    fn permission_card_partial_approve_sends_only_checked_permissions() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        show_permission_approval(&composer, vcx, "session");
        assert!(vcx.debug_bounds("permission-scope-session").is_some());
        let shared_index = composer.update(vcx, |view, _| {
            view.external_pending_approval
                .as_ref()
                .unwrap()
                .permissions
                .iter()
                .position(|choice| choice.id == "fileSystem.write[1]")
                .unwrap()
        });
        click_permission(vcx, shared_index);
        composer.update(vcx, |view, _| {
            let pending = view.external_pending_approval.as_ref().unwrap();
            assert!(!pending.permissions[shared_index].checked);
            assert_eq!(
                pending
                    .permissions
                    .iter()
                    .filter(|choice| choice.checked)
                    .map(|choice| choice.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["fileSystem.write[0]", "network"]
            );
        });
        draw(vcx);
        click_id(vcx, "approve-tool");
        composer.update(vcx, |view, _| {
            let reply = view.last_external_approval.as_ref().unwrap();
            assert!(reply.approved);
            assert_eq!(
                reply.granted_permission_ids.as_deref(),
                Some(["fileSystem.write[0]".to_string(), "network".to_string(),].as_slice())
            );
            assert_eq!(
                termior_agent_host::permissions_approval_result(
                    &sample_permission_profile(),
                    true,
                    reply.granted_permission_ids.as_deref(),
                ),
                serde_json::json!({
                    "permissions": {
                        "network": {"enabled": true},
                        "fileSystem": {"write": ["/proj"]}
                    },
                    "scope": "turn"
                })
            );
        });
    }

    /// 拒绝忽略勾选，回传空权限集。
    #[test]
    fn permission_card_deny_sends_an_empty_permission_set() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        show_permission_approval(&composer, vcx, "session");
        click_id(vcx, "reject-tool");
        composer.update(vcx, |view, _| {
            let reply = view.last_external_approval.as_ref().unwrap();
            assert!(!reply.approved);
            assert_eq!(reply.granted_permission_ids.as_deref(), Some([].as_slice()));
            assert_eq!(
                termior_agent_host::permissions_approval_result(
                    &sample_permission_profile(),
                    false,
                    reply.granted_permission_ids.as_deref(),
                ),
                serde_json::json!({"permissions": {}, "scope": "turn"})
            );
        });
    }

    /// 未知 scope 仍显示在卡片上，批准后的授权范围仍是 turn。
    #[test]
    fn permission_card_shows_unknown_scope_and_grants_for_the_turn() {
        let mut cx = TestAppContext::single();
        let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
        show_permission_approval(&composer, vcx, "something-new");
        assert!(vcx.debug_bounds("permission-scope-something-new").is_some());
        composer.update(vcx, |view, _| {
            assert_eq!(
                view.external_pending_approval.as_ref().unwrap().scope,
                "something-new"
            );
        });
        click_id(vcx, "approve-tool");
        composer.update(vcx, |view, _| {
            assert!(view.external_pending_approval.is_none());
            let reply = view.last_external_approval.as_ref().unwrap();
            assert!(reply.approved);
            let result = termior_agent_host::permissions_approval_result(
                &sample_permission_profile(),
                reply.approved,
                reply.granted_permission_ids.as_deref(),
            );
            assert_eq!(
                result.get("scope").and_then(serde_json::Value::as_str),
                Some("turn")
            );
        });
    }
}

#[cfg(test)]
mod permission_fixture_contract {
    use super::ExternalPendingApproval;
    use termior_agent_host::{
        map_codex_message, permissions_approval_result, BackendEvent, PERMISSIONS_APPROVAL_METHOD,
    };

    const REQUESTS: &str = include_str!(
        "../../termior-agent-host/tests/fixtures/codex-permissions-request-approval/requests.jsonl"
    );
    const RESPONSES: &str = include_str!(
        "../../termior-agent-host/tests/fixtures/codex-permissions-request-approval/responses.jsonl"
    );

    #[test]
    fn recorded_permission_fixtures_map_through_the_approval_card() {
        for response in jsonl(RESPONSES) {
            let id = response["id"].as_u64().unwrap();
            let decision = response["decision"].as_str().unwrap();
            let granted = strings(&response["granted"]);
            let request = jsonl(REQUESTS)
                .into_iter()
                .find(|value| value["id"].as_u64() == Some(id))
                .unwrap_or_else(|| panic!("missing request {id}"));
            let event = map_codex_message(&request);
            let BackendEvent::ApprovalRequested {
                backend_request_id,
                tool_call_id,
                request_kind,
                raw,
                ..
            } = event
            else {
                panic!("fixture {id} did not map to an approval");
            };
            assert_eq!(request_kind, PERMISSIONS_APPROVAL_METHOD);
            assert_eq!(backend_request_id, id.to_string());
            assert!(raw.get("scope").is_none());
            let mut pending = ExternalPendingApproval::from_codex(
                backend_request_id,
                tool_call_id,
                request_kind,
                &raw,
            );
            assert!(pending.scope.is_empty());
            assert!(pending.is_permissions());
            if decision == "partial" {
                for choice in &mut pending.permissions {
                    choice.checked = granted.iter().any(|entry| entry == &choice.id);
                }
            }
            let approved = decision != "deny";
            let ids = pending.granted_permission_ids(approved).unwrap();
            assert_eq!(ids, granted);
            let profile = raw
                .get("permissions")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            assert_eq!(
                permissions_approval_result(&profile, approved, Some(&ids)),
                response["message"]["result"]
            );
            assert!(
                !pending.arguments.contains("codex-user") && !pending.arguments.contains("sk-"),
                "{}",
                pending.arguments
            );
            for choice in &pending.permissions {
                assert!(!choice.label.contains("codex-user"), "{}", choice.label);
            }
        }
    }

    fn jsonl(body: &str) -> Vec<serde_json::Value> {
        body.lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn strings(value: &serde_json::Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect()
    }
}
