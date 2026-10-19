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
    reconcile_persisted_backend_bindings, BackendEvent, CodexBackend, ExternalRecoveryTarget,
    ManagedAgentSession, PumpOutcome, TaskLaunch,
};
use termior_ai::automation::{next_run_after, AutomationManager, AutomationStore};
use termior_ai::context_engine::{
    ContextAssembler, ContextCategory, ContextItem, ContextPlan, ContextSource, MemoryStatus,
    MemoryStore, RuleResolver, SkillIndex, SkillMetadata,
};
use termior_ai::slash::{self, ComposerCommand, SlashAction, SlashCommand};
use termior_ai::{
    AgentDefinition, AgentDefinitionStore, ApprovalPolicy, Attachment, AttachmentSource,
    CancellationToken, ChangeSetId, ComposerDraft, EditProposalSummary, HttpProvider,
    KeyringSecretStore, Message, Mode, ProviderConfig, Role, RuntimeBudgets, SecretStore,
    SessionModel, SessionStore, Snippet, SnippetError, SnippetStore, TaskCommand, TaskConfig,
    TaskRuntime, TaskState, TaskSummary, TaskSummaryStore, TerminalContext,
    TerminalContextProvider, TodoStore, ToolExecutor, ToolRegistry, WaitingReason,
};
use termior_explorer_core::fuzzy::fuzzy_match;
use termior_platform::AgentStatus;
use termior_preview::MarkdownDocument;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{
    CheckpointManifest, CheckpointStore, DataFiles, KeyAction, ModelProviderSettings,
    ModelSettings, RecoveryCenter, RecoveryCommand, RecoveryTaskSummary, RestoreAction, Settings,
};
use termior_theme::ResolvedPalette;
use termior_ui_kit::{menu_panel, tokens::icon_size, Icon, Tooltip};

// 拆分结构：本文件保留核心类型与 `ComposerView` 字段。
// 任务 / 审批 / 外部后端状态在 `orchestrator`（不依赖 GPUI）。
// 其余行为按职责放在子模块（session、backend、messages、approval、input、centers、render）。
mod agent_menu;
mod approval;
mod backend;
mod centers;
mod helpers;
mod input;
mod messages;
mod model_menu;
mod notify;
mod orchestrator;
mod render;
mod session;
mod task_details;
mod tool_blocks;

use orchestrator::{
    AgentBackendChoice, AgentRuntime, ApprovalAction, AutoResolve, ComposerOrchestrator,
    ExternalCommand, ExternalDispatch, ExternalOutcome, ExternalRunResult, PreparedEdit,
    StopAction, TaskCommit, TaskDispatchReject, TaskRunResult, TaskStatusLine, TaskUiHint,
};

pub use agent_menu::ComposerLaunchCliAgent;
use task_details::TaskRef;

#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod permission_fixture_contract;

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

#[derive(Debug, Clone, Copy)]
enum MemoryAction {
    Accept,
    Reject,
    TogglePause,
    Delete,
}

#[derive(Debug, Clone)]
pub struct EditReviewRequested(pub EditProposalSummary);

/// Automatic worktree merge-back was not a conflict-free fast-forward or cherry-pick.
/// The workspace opens the existing Git review flow and does not change the target checkout.
#[derive(Debug, Clone)]
pub struct GitReviewRequested {
    pub repository: PathBuf,
    pub worktree_path: PathBuf,
    pub reason: String,
    pub patch: String,
    pub conflicts: Vec<String>,
}

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
    status: String,
    /// 当前 status 是否为「先到 Settings → Models 配置模型」引导文案；
    /// 是则空闲时在输入行内作为占位符复用（见 `model_setup_placeholder`）。
    /// 原先用 `status.contains("Settings → Models")` 之类的文本嗅探，i18n 化后
    /// 文案会随语言变化，改为在写入状态时显式维护该标记。
    needs_model_setup: bool,
    /// 任务、审批和外部后端会话。渲染只读这里，事件转发给它。
    orchestrator: ComposerOrchestrator,
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

impl EventEmitter<AgentStatus> for ComposerView {}
impl EventEmitter<EditReviewRequested> for ComposerView {}
impl EventEmitter<GitReviewRequested> for ComposerView {}
impl EventEmitter<ComposerDockToggle> for ComposerView {}
impl EventEmitter<ComposerCollapse> for ComposerView {}
impl EventEmitter<ComposerAppAction> for ComposerView {}
impl EventEmitter<ComposerLaunchCliAgent> for ComposerView {}
impl EventEmitter<ComposerCommandNotice> for ComposerView {}
