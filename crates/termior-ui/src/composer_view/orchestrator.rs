//! Composer 的任务 / 审批 / 外部后端编排状态。
//!
//! 不引用 GPUI。`ComposerView` 持有本类型，渲染时读取，把点击和提交转成这里的方法；
//! 状态转移的结果再由 View 写成文案、焦点和 `cx.emit`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use termior_agent_host::{
    permission_entries, ManagedAgentSession, PumpOutcome, PERMISSIONS_APPROVAL_METHOD,
};
use termior_ai::context_engine::RepoMap;
use termior_ai::{
    ApprovalPolicy, ApprovalRequest, CancellationToken, EditProposalSummary, Mode, ProviderConfig,
    RuntimeBudgets, TaskRuntime, TaskState, ToolContract, ToolExecutor, ToolRegistry,
    WaitingReason,
};
use termior_platform::AgentStatus;
use termior_vcs::{MergeBack, WorktreeEnvironment, WorktreeError, WorktreeManager};

use super::notify::status_for_task;

#[derive(Clone)]
pub(super) struct AgentRuntime {
    pub(super) config: ProviderConfig,
    pub(super) tools: ToolRegistry,
    pub(super) executor: Arc<ToolExecutor>,
    pub(super) system_prompt: String,
}

pub(super) struct PendingApproval {
    pub(super) request: ApprovalRequest,
    pub(super) contract: ToolContract,
}

pub(super) struct PendingEdit {
    pub(super) summary: EditProposalSummary,
    pub(super) decisions: HashMap<usize, bool>,
}

pub(super) struct TaskRunResult {
    pub(super) task: TaskRuntime,
    pub(super) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentBackendChoice {
    BuiltIn,
    CodexAppServer,
}

/// Codex `item/permissions/requestApproval` 在审批卡上的一项可勾选权限。
pub(super) struct PermissionChoice {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) checked: bool,
}

pub(super) struct ExternalPendingApproval {
    pub(super) backend_request_id: String,
    pub(super) tool_call_id: String,
    pub(super) request_kind: String,
    pub(super) arguments: String,
    /// 服务端提议的 scope（`turn` / `session` / 未知值）。缺省为空串。
    /// 真正回给 Codex 的授权范围始终是当前 turn。
    pub(super) scope: String,
    pub(super) permissions: Vec<PermissionChoice>,
}

impl ExternalPendingApproval {
    pub(super) fn from_codex(
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

    pub(super) fn is_permissions(&self) -> bool {
        self.request_kind == PERMISSIONS_APPROVAL_METHOD
    }

    /// 权限审批：批准回传勾选子集，拒绝回传空集。其它审批不携带权限 id。
    pub(super) fn granted_permission_ids(&self, approved: bool) -> Option<Vec<String>> {
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

pub(super) enum ExternalCommand {
    Start {
        task_id: String,
        text: String,
    },
    Respond {
        approved: bool,
        granted_permission_ids: Option<Vec<String>>,
    },
}

/// 审批按钮交给 Codex 会话的决议，供测试断言勾选子集。
#[cfg(test)]
pub(super) struct ExternalApprovalReply {
    pub(super) approved: bool,
    pub(super) granted_permission_ids: Option<Vec<String>>,
}

pub(super) struct ExternalRunResult {
    pub(super) session: Option<Arc<Mutex<ManagedAgentSession>>>,
    pub(super) outcome: Option<PumpOutcome>,
    pub(super) error: Option<String>,
}

/// 内置任务或外部会话可以开始一轮命令时，View 拿去丢进后台执行器。
pub(super) struct TaskDispatch {
    pub(super) runtime: AgentRuntime,
    pub(super) task: TaskRuntime,
}

#[derive(Debug)]
pub(super) enum TaskDispatchReject {
    Busy,
    NoTask,
    NoRuntime,
}

pub(super) struct ExternalDispatch {
    pub(super) session: Option<Arc<Mutex<ManagedAgentSession>>>,
    pub(super) cancellation: CancellationToken,
}

pub(super) enum ApprovalAction {
    None,
    External(ExternalCommand),
    Builtin {
        call_id: String,
        tool_name: String,
        approved: bool,
    },
}

pub(super) enum PreparedEdit {
    Missing,
    Incomplete,
    Ready {
        proposal_id: String,
        accepted: Vec<usize>,
    },
}

pub(super) enum AutoResolve {
    None,
    Tool,
    Edit { id: String, hunks: Vec<usize> },
}

pub(super) enum StopAction {
    CancelToken(CancellationToken),
    DispatchCancel,
    Idle,
}

/// `apply_external_result` 之后 View 要呈现的结果。通知级别与原先 `cx.emit` 一致。
pub(super) enum ExternalOutcome {
    Error { error: String },
    Approval { call: String },
    Completed { success: bool, status: String },
    Unknown { reason: String },
    Running,
    NoOutcome,
}

impl ExternalOutcome {
    pub(super) fn notify(&self) -> AgentStatus {
        match self {
            Self::Error { .. } | Self::NoOutcome => AgentStatus::Error,
            Self::Approval { .. } | Self::Unknown { .. } | Self::Running => AgentStatus::Attention,
            Self::Completed { success: true, .. } => AgentStatus::Finished,
            Self::Completed { success: false, .. } => AgentStatus::Error,
        }
    }
}

/// 任务结果落定后 View 要写的状态行。文案键仍由 View 选择。
pub(super) enum TaskStatusLine {
    LostApproval,
    LostContract,
    InvalidSummary,
    ApprovalRequired {
        summary: String,
    },
    ReviewHunks,
    BudgetExhausted {
        dimension: String,
        used: u64,
        limit: u64,
    },
    TerminalTakenOver {
        session: String,
        cursor: u64,
    },
    NeedsInputReason {
        reason: String,
    },
    NeedsInput,
    PlanReady,
    FinishedVerified,
    FinishedUnverified,
    Cancelled,
    UnknownResult,
    TaskFailed,
    Other {
        state: String,
    },
}

pub(super) enum TaskUiHint {
    None,
    AwaitPlanConfirmation,
    OpenTaskDetails,
}

pub(super) struct TaskCommit {
    pub(super) status: TaskStatusLine,
    pub(super) notify: AgentStatus,
    pub(super) review: Option<EditProposalSummary>,
    pub(super) hint: TaskUiHint,
    /// 任务已结束且这次不是计划请求时，清掉计划确认标记。
    pub(super) clear_plan: bool,
    /// 终态在通知发出后清掉固定运行时。早退路径为 false，运行时留着。
    pub(super) clear_runtime: bool,
    /// 完成态在 `cx.emit` 之后才丢掉任务，通知回调仍能看见它。
    pub(super) release_after_notify: bool,
    /// 早退（丢失审批 / 非法摘要）为 false，View 不再自动放行。
    pub(super) auto_resolve: bool,
}

enum TaskDisposition {
    /// 任务丢弃，活动运行时保留。对应原先 match 里的提前 return。
    Drop,
    Keep,
    /// 取消 / 失败 / 未知：通知前丢掉任务，通知后清运行时。
    ReleaseBeforeNotify,
    /// 完成态：通知回调期间任务还在，回调之后再丢掉。
    ReleaseAfterNotify,
}

struct TaskClass {
    disposition: TaskDisposition,
    approval: Option<PendingApproval>,
    edit: Option<PendingEdit>,
    commit: TaskCommit,
}

struct TaskFacts {
    state: TaskState,
    approval: Option<ApprovalRequest>,
    contract: Option<ToolContract>,
    change: Option<EditProposalSummary>,
    waiting_reason: Option<WaitingReason>,
    plan_request: bool,
    mode_is_yolo: bool,
}

pub(super) struct TaskExecutionRoot {
    pub(super) root: PathBuf,
    pub(super) environment_id: String,
    pub(super) worktree: Option<WorktreeEnvironment>,
}

#[derive(Clone, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct PendingGitReview {
    pub(super) repository: PathBuf,
    pub(super) worktree_path: PathBuf,
    pub(super) reason: String,
    pub(super) patch: String,
    pub(super) conflicts: Vec<String>,
}

pub(super) struct ComposerOrchestrator {
    pub(super) runtime: Option<AgentRuntime>,
    pub(super) active_task: Option<TaskRuntime>,
    pub(super) active_cancellation: Option<CancellationToken>,
    pub(super) active_plan_request: bool,
    pub(super) pending_approval: Option<PendingApproval>,
    pub(super) external_pending_approval: Option<ExternalPendingApproval>,
    #[cfg(test)]
    pub(super) last_external_approval: Option<ExternalApprovalReply>,
    pub(super) pending_edit: Option<PendingEdit>,
    pub(super) backend_choice: AgentBackendChoice,
    pub(super) backend_capability_summary: Option<String>,
    pub(super) external_session: Option<Arc<Mutex<ManagedAgentSession>>>,
    pub(super) active_runtime: Option<AgentRuntime>,
    pub(super) task_project_override: Option<PathBuf>,
    pub(super) bound_worktree: Option<WorktreeEnvironment>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) pending_git_review: Option<PendingGitReview>,
    repo_map: Option<RepoMap>,
    repo_map_root: Option<PathBuf>,
    pub(super) busy: bool,
}

impl ComposerOrchestrator {
    pub(super) fn new() -> Self {
        Self {
            runtime: None,
            active_task: None,
            active_cancellation: None,
            active_plan_request: false,
            pending_approval: None,
            external_pending_approval: None,
            #[cfg(test)]
            last_external_approval: None,
            pending_edit: None,
            backend_choice: AgentBackendChoice::BuiltIn,
            backend_capability_summary: None,
            external_session: None,
            active_runtime: None,
            task_project_override: None,
            bound_worktree: None,
            pending_git_review: None,
            repo_map: None,
            repo_map_root: None,
            busy: false,
        }
    }

    /// Pin the next builtin task to a prepared worktree. Tool, terminal, rule, and repo-map
    /// roots come from `environment.path`, not the main checkout.
    ///
    /// Called when a worktree environment is prepared for a task. No in-app control creates
    /// that environment yet, so the entry point is part of the host contract rather than a button.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn bind_worktree(&mut self, environment: WorktreeEnvironment) {
        if self.repo_map_root.as_ref() != Some(&environment.path) {
            self.repo_map = None;
            self.repo_map_root = None;
        }
        self.bound_worktree = Some(environment);
    }

    /// Root for the task executor, rules, and repo map.
    ///
    /// A bound worktree wins over a one-shot project pin and over the main workspace.
    /// The project pin is consumed so it cannot leak onto a later task.
    pub(super) fn take_execution_root(&mut self, workspace_root: &Path) -> TaskExecutionRoot {
        if let Some(worktree) = self.bound_worktree.clone() {
            self.task_project_override = None;
            return TaskExecutionRoot {
                environment_id: worktree.id.clone(),
                root: worktree.path.clone(),
                worktree: Some(worktree),
            };
        }
        if let Some(root) = self.take_project_override() {
            return TaskExecutionRoot {
                root,
                environment_id: "direct".into(),
                worktree: None,
            };
        }
        TaskExecutionRoot {
            root: workspace_root.to_path_buf(),
            environment_id: "direct".into(),
            worktree: None,
        }
    }

    pub(super) fn displayed_root(&self, workspace_root: &Path) -> PathBuf {
        if let Some(worktree) = &self.bound_worktree {
            return worktree.path.clone();
        }
        if let Some(task) = &self.active_task {
            return task.task().project_dir.clone();
        }
        if let Some(root) = &self.task_project_override {
            return root.clone();
        }
        workspace_root.to_path_buf()
    }

    /// Symbols from the worktree checkout only. Paths outside that root are dropped.
    pub(super) fn worktree_repo_excerpt(
        &mut self,
        root: &Path,
        query: &str,
    ) -> Result<String, String> {
        let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
        if self.repo_map_root.as_ref() != Some(&root) {
            let mut map = RepoMap::default();
            map.index_workspace(&root)
                .map_err(|error| error.to_string())?;
            self.repo_map = Some(map);
            self.repo_map_root = Some(root.clone());
        }
        let Some(map) = self.repo_map.as_ref() else {
            return Ok(String::new());
        };
        let mut lines = Vec::new();
        for entry in map.relevant(query, 1_200) {
            if entry.path != root && !entry.path.starts_with(&root) {
                continue;
            }
            let relative = entry.path.strip_prefix(&root).unwrap_or(&entry.path);
            lines.push(format!(
                "{} {}",
                relative.display(),
                entry.symbols.join(",")
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Fast-forward or cherry-pick when that cannot touch user edits. Otherwise record a Git review.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn integrate_worktree(
        &mut self,
        target_ref: &str,
        validation_passed: bool,
    ) -> Result<MergeBack, WorktreeError> {
        let Some(environment) = self.bound_worktree.clone() else {
            return Err(WorktreeError::Git("no worktree is bound".into()));
        };
        let outcome = WorktreeManager::merge_back(&environment, target_ref, validation_passed)?;
        self.pending_git_review = match &outcome {
            MergeBack::Review { reason, preview } => Some(PendingGitReview {
                repository: environment.repository.clone(),
                worktree_path: environment.path.clone(),
                reason: reason.to_string(),
                patch: preview.patch.clone(),
                conflicts: preview.conflicts.clone(),
            }),
            MergeBack::Applied { .. } => None,
        };
        Ok(outcome)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn take_git_review(&mut self) -> Option<PendingGitReview> {
        self.pending_git_review.take()
    }

    /// 活动任务固定的运行时；没有活动任务时为全局配置的运行时。
    pub(super) fn task_runtime(&self) -> Option<&AgentRuntime> {
        self.active_runtime.as_ref().or(self.runtime.as_ref())
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending_approval.is_some()
            || self.external_pending_approval.is_some()
            || self.pending_edit.is_some()
    }

    /// 忙碌，或有未决的工具审批 / 外部审批 / 编辑审阅。
    pub(super) fn submission_blocked(&self) -> bool {
        self.busy || self.has_pending()
    }

    pub(super) fn blocks_mode_switch(&self, next: Mode) -> bool {
        self.busy || (self.has_pending() && next != Mode::Yolo)
    }

    pub(super) fn terminal_action_busy(&self) -> bool {
        self.busy || self.active_task.is_some()
    }

    pub(super) fn watch_should_continue(&self, command_running: bool) -> bool {
        self.busy || self.active_task.is_some() || command_running
    }

    /// 切换后端。进行中的回合或未决卡片时拒绝，离开 Codex 时丢掉外部会话。
    pub(super) fn try_select_backend(&mut self, choice: AgentBackendChoice) -> bool {
        if self.submission_blocked() {
            return false;
        }
        if self.backend_choice == AgentBackendChoice::CodexAppServer
            && choice != AgentBackendChoice::CodexAppServer
        {
            self.external_session = None;
            self.backend_capability_summary = None;
        }
        self.backend_choice = choice;
        true
    }

    pub(super) fn take_project_override(&mut self) -> Option<PathBuf> {
        self.task_project_override.take()
    }

    pub(super) fn pin_project(&mut self, dir: PathBuf) {
        self.task_project_override = Some(dir);
    }

    pub(super) fn clear_project_pin(&mut self) {
        self.task_project_override = None;
    }

    pub(super) fn install_builtin_task(
        &mut self,
        task: TaskRuntime,
        runtime: AgentRuntime,
        plan_request: bool,
    ) {
        self.active_runtime = Some(runtime);
        self.active_cancellation = Some(task.cancellation_token());
        self.active_plan_request = plan_request;
        self.active_task = Some(task);
    }

    pub(super) fn clear_for_new_session(&mut self) {
        self.active_task = None;
        self.external_session = None;
    }

    pub(super) fn stop_action(&self) -> StopAction {
        if let Some(token) = &self.active_cancellation {
            return StopAction::CancelToken(token.clone());
        }
        if self.active_task.is_some() {
            return StopAction::DispatchCancel;
        }
        StopAction::Idle
    }

    pub(super) fn increased_budgets(&self) -> Option<RuntimeBudgets> {
        let task = self.active_task.as_ref()?;
        increase_budgets(task.task().state, &task.task().budgets)
    }

    /// 已经在跑则返回 `None`（调用方保持原样，不发通知）。
    pub(super) fn begin_external(&mut self) -> Option<ExternalDispatch> {
        if self.busy {
            return None;
        }
        let session = self.external_session.clone();
        let cancellation = CancellationToken::default();
        self.active_cancellation = Some(cancellation.clone());
        self.busy = true;
        Some(ExternalDispatch {
            session,
            cancellation,
        })
    }

    pub(super) fn apply_external_result(&mut self, result: ExternalRunResult) -> ExternalOutcome {
        self.busy = false;
        self.active_cancellation = None;
        if let Some(session) = result.session.as_ref() {
            if let Ok(session) = session.lock() {
                self.backend_capability_summary = Some(capability_summary(&session));
            }
        }
        if let Some(session) = result.session {
            self.external_session = Some(session);
        }
        if let Some(error) = result.error {
            return ExternalOutcome::Error { error };
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
                ExternalOutcome::Approval { call: tool_call_id }
            }
            Some(PumpOutcome::Completed { success, status }) => {
                self.external_pending_approval = None;
                ExternalOutcome::Completed { success, status }
            }
            Some(PumpOutcome::Unknown { reason }) => {
                self.external_pending_approval = None;
                ExternalOutcome::Unknown { reason }
            }
            Some(PumpOutcome::Pending) => ExternalOutcome::Running,
            None => ExternalOutcome::NoOutcome,
        }
    }

    pub(super) fn begin_task_command(
        &mut self,
        policy: ApprovalPolicy,
    ) -> Result<TaskDispatch, TaskDispatchReject> {
        if self.busy {
            return Err(TaskDispatchReject::Busy);
        }
        let Some(mut task) = self.active_task.take() else {
            return Err(TaskDispatchReject::NoTask);
        };
        let Some(runtime) = self.task_runtime().cloned() else {
            self.active_task = Some(task);
            return Err(TaskDispatchReject::NoRuntime);
        };
        // 续跑时按当前模式同步审批策略：任务挂起期间切到 Yolo，后续调用即不再询问。
        task.set_approval_policy(policy);
        self.active_cancellation = Some(task.cancellation_token());
        self.busy = true;
        Ok(TaskDispatch { runtime, task })
    }

    /// 清掉忙碌和未决卡片，并返回要镜像进会话的历史。日志失败时任务仍可 `keep_task`。
    pub(super) fn begin_task_result(&mut self, task: &TaskRuntime) -> Vec<termior_ai::Message> {
        self.busy = false;
        self.active_cancellation = None;
        self.pending_approval = None;
        self.pending_edit = None;
        task.history().to_vec()
    }

    pub(super) fn keep_task(&mut self, task: TaskRuntime) {
        self.active_task = Some(task);
    }

    pub(super) fn fail_task(&mut self) {
        self.active_runtime = None;
    }

    pub(super) fn commit_task_result(
        &mut self,
        task: TaskRuntime,
        mode_is_yolo: bool,
    ) -> TaskCommit {
        let class = classify_task(&task, self.active_plan_request, mode_is_yolo);
        self.apply_classified(task, class)
    }

    fn apply_classified(&mut self, task: TaskRuntime, class: TaskClass) -> TaskCommit {
        match class.disposition {
            TaskDisposition::Drop => {}
            TaskDisposition::Keep | TaskDisposition::ReleaseAfterNotify => {
                self.pending_approval = class.approval;
                self.pending_edit = class.edit;
                self.active_task = Some(task);
            }
            TaskDisposition::ReleaseBeforeNotify => {
                self.active_task = None;
            }
        }
        class.commit
    }

    /// 通知已经发出。完成态在这里丢掉任务；终态在这里清掉固定运行时。
    pub(super) fn finish_task_commit(&mut self, commit: &TaskCommit) {
        if commit.release_after_notify {
            self.active_task = None;
        }
        if commit.clear_runtime {
            self.active_runtime = None;
        }
    }

    pub(super) fn toggle_permission(&mut self, index: usize) -> bool {
        let Some(pending) = self.external_pending_approval.as_mut() else {
            return false;
        };
        let Some(choice) = pending.permissions.get_mut(index) else {
            return false;
        };
        choice.checked = !choice.checked;
        true
    }

    /// `block_if_busy` 为真时（批准按钮）忙碌直接留下卡片。
    /// 拒绝按钮不看忙碌：先取走卡片，再由派发在忙碌时丢弃这次命令。
    pub(super) fn take_approval(&mut self, approved: bool, block_if_busy: bool) -> ApprovalAction {
        if block_if_busy && self.busy {
            return ApprovalAction::None;
        }
        if let Some(pending) = self.external_pending_approval.take() {
            let granted_permission_ids = pending.granted_permission_ids(approved);
            #[cfg(test)]
            {
                self.last_external_approval = Some(ExternalApprovalReply {
                    approved,
                    granted_permission_ids: granted_permission_ids.clone(),
                });
            }
            return ApprovalAction::External(ExternalCommand::Respond {
                approved,
                granted_permission_ids,
            });
        }
        if let Some(pending) = self.pending_approval.take() {
            return ApprovalAction::Builtin {
                call_id: pending.request.call_id,
                tool_name: pending.request.tool_name,
                approved,
            };
        }
        ApprovalAction::None
    }

    pub(super) fn decide_hunk(&mut self, id: usize, accept: bool) -> bool {
        let Some(edit) = self.pending_edit.as_mut() else {
            return false;
        };
        edit.decisions.insert(id, accept);
        true
    }

    pub(super) fn prepared_edit(&self) -> PreparedEdit {
        let Some(edit) = self.pending_edit.as_ref() else {
            return PreparedEdit::Missing;
        };
        if edit.decisions.len() != edit.summary.hunk_ids.len() {
            return PreparedEdit::Incomplete;
        }
        let accepted = edit
            .summary
            .hunk_ids
            .iter()
            .filter(|id| edit.decisions.get(id) == Some(&true))
            .copied()
            .collect::<Vec<_>>();
        PreparedEdit::Ready {
            proposal_id: edit.summary.id.clone(),
            accepted,
        }
    }

    pub(super) fn pending_edit_id(&self) -> Option<String> {
        self.pending_edit
            .as_ref()
            .map(|edit| edit.summary.id.clone())
    }

    /// 提案 id 不匹配时不动卡片。匹配则取走，调用方再派发 `ReviewChange`。
    pub(super) fn take_matching_edit(&mut self, proposal_id: &str) -> bool {
        if self
            .pending_edit
            .as_ref()
            .is_none_or(|edit| edit.summary.id != proposal_id)
        {
            return false;
        }
        self.pending_edit = None;
        true
    }

    pub(super) fn auto_resolve(&self) -> AutoResolve {
        if self.busy {
            return AutoResolve::None;
        }
        if self.pending_approval.is_some() {
            return AutoResolve::Tool;
        }
        if let Some(edit) = self.pending_edit.as_ref() {
            return AutoResolve::Edit {
                id: edit.summary.id.clone(),
                hunks: edit.summary.hunk_ids.clone(),
            };
        }
        AutoResolve::None
    }
}

fn increase_budgets(state: TaskState, budgets: &RuntimeBudgets) -> Option<RuntimeBudgets> {
    if state != TaskState::WaitingUser {
        return None;
    }
    let mut budgets = budgets.clone();
    budgets.max_model_steps = budgets.max_model_steps.saturating_add(100);
    budgets.max_wall_clock_ms = budgets.max_wall_clock_ms.saturating_add(30 * 60 * 1_000);
    budgets.max_tool_output_bytes = budgets
        .max_tool_output_bytes
        .saturating_add(16 * 1024 * 1024);
    Some(budgets)
}

fn capability_summary(session: &ManagedAgentSession) -> String {
    let capabilities = &session.info().capabilities;
    format!(
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
    )
}

fn classify_task(task: &TaskRuntime, plan_request: bool, mode_is_yolo: bool) -> TaskClass {
    let change = task
        .pending_change_summary()
        .and_then(|summary| serde_json::from_str::<EditProposalSummary>(summary).ok());
    classify_facts(TaskFacts {
        state: task.task().state,
        approval: task.pending_approval(),
        contract: task.pending_contract(),
        change,
        waiting_reason: task.task().waiting_reason.clone(),
        plan_request,
        mode_is_yolo,
    })
}

fn classify_facts(facts: TaskFacts) -> TaskClass {
    let notify = status_for_task(facts.state, facts.plan_request);
    let early = |status| TaskClass {
        disposition: TaskDisposition::Drop,
        approval: None,
        edit: None,
        commit: TaskCommit {
            status,
            notify: AgentStatus::Error,
            review: None,
            hint: TaskUiHint::None,
            clear_plan: false,
            clear_runtime: false,
            release_after_notify: false,
            auto_resolve: false,
        },
    };
    let keep = |status, hint, approval, edit, review| TaskClass {
        disposition: TaskDisposition::Keep,
        approval,
        edit,
        commit: TaskCommit {
            status,
            notify,
            review,
            hint,
            clear_plan: false,
            clear_runtime: false,
            release_after_notify: false,
            auto_resolve: true,
        },
    };
    let release = |status, hint, after_notify| TaskClass {
        disposition: if after_notify {
            TaskDisposition::ReleaseAfterNotify
        } else {
            TaskDisposition::ReleaseBeforeNotify
        },
        approval: None,
        edit: None,
        commit: TaskCommit {
            status,
            notify,
            review: None,
            hint,
            clear_plan: !facts.plan_request,
            clear_runtime: true,
            release_after_notify: after_notify,
            auto_resolve: true,
        },
    };

    match facts.state {
        TaskState::WaitingApproval => {
            let Some(request) = facts.approval else {
                return early(TaskStatusLine::LostApproval);
            };
            let Some(contract) = facts.contract else {
                return early(TaskStatusLine::LostContract);
            };
            let summary = request.summary.clone();
            keep(
                TaskStatusLine::ApprovalRequired { summary },
                TaskUiHint::None,
                Some(PendingApproval { request, contract }),
                None,
                None,
            )
        }
        TaskState::WaitingChangeReview => {
            let Some(summary) = facts.change else {
                return early(TaskStatusLine::InvalidSummary);
            };
            let review = (!facts.mode_is_yolo).then(|| summary.clone());
            keep(
                TaskStatusLine::ReviewHunks,
                TaskUiHint::None,
                None,
                Some(PendingEdit {
                    summary,
                    decisions: HashMap::new(),
                }),
                review,
            )
        }
        TaskState::WaitingUser => match facts.waiting_reason {
            Some(WaitingReason::BudgetExhausted {
                dimension,
                used,
                limit,
            }) => keep(
                TaskStatusLine::BudgetExhausted {
                    dimension: format!("{dimension:?}"),
                    used,
                    limit,
                },
                TaskUiHint::None,
                None,
                None,
                None,
            ),
            Some(WaitingReason::TerminalControl { session_id, cursor }) => keep(
                TaskStatusLine::TerminalTakenOver {
                    session: session_id,
                    cursor,
                },
                TaskUiHint::OpenTaskDetails,
                None,
                None,
                None,
            ),
            Some(reason) => keep(
                TaskStatusLine::NeedsInputReason {
                    reason: format!("{reason:?}"),
                },
                TaskUiHint::None,
                None,
                None,
                None,
            ),
            None => keep(
                TaskStatusLine::NeedsInput,
                TaskUiHint::None,
                None,
                None,
                None,
            ),
        },
        TaskState::CompletedVerified => {
            if facts.plan_request {
                release(
                    TaskStatusLine::PlanReady,
                    TaskUiHint::AwaitPlanConfirmation,
                    true,
                )
            } else {
                release(TaskStatusLine::FinishedVerified, TaskUiHint::None, true)
            }
        }
        TaskState::CompletedUnverified => {
            if facts.plan_request {
                release(
                    TaskStatusLine::PlanReady,
                    TaskUiHint::AwaitPlanConfirmation,
                    true,
                )
            } else {
                release(TaskStatusLine::FinishedUnverified, TaskUiHint::None, true)
            }
        }
        TaskState::Cancelled => release(TaskStatusLine::Cancelled, TaskUiHint::None, false),
        TaskState::Unknown => release(TaskStatusLine::UnknownResult, TaskUiHint::None, false),
        TaskState::Failed => release(TaskStatusLine::TaskFailed, TaskUiHint::None, false),
        state => keep(
            TaskStatusLine::Other {
                state: format!("{state:?}"),
            },
            TaskUiHint::None,
            None,
            None,
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_ai::{
        ApprovalClass, BudgetDimension, Idempotency, Message, ProviderKind, SideEffectClass,
        TaskConfig, ToolLevelSerde,
    };

    fn idle_task(goal: &str) -> TaskRuntime {
        TaskRuntime::new(
            TaskConfig::new(goal, "/tmp/termior-orch", "native", "direct"),
            RuntimeBudgets::default(),
            ApprovalPolicy::Prompt,
        )
        .with_history(vec![Message::user(goal)])
    }

    fn approval(call_id: &str) -> ApprovalRequest {
        ApprovalRequest {
            call_id: call_id.into(),
            tool_name: "write_file".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
            summary: "write a.rs".into(),
            steps: 2,
        }
    }

    fn contract(name: &str) -> ToolContract {
        ToolContract {
            name: name.into(),
            level: ToolLevelSerde::Approval,
            description: String::new(),
            parameters: serde_json::json!({}),
            side_effect: SideEffectClass::LocalWrite,
            approval: ApprovalClass::User,
            default_timeout_ms: 1,
            max_output_bytes: 1,
            idempotency: Idempotency::Unknown,
            parallel_safe: false,
        }
    }

    fn proposal() -> EditProposalSummary {
        EditProposalSummary {
            id: "edit-1".into(),
            path: "a.rs".into(),
            baseline_digest: "abc".into(),
            hunk_ids: vec![0, 1],
            patch: String::new(),
        }
    }

    fn permission_raw(scope: &str) -> serde_json::Value {
        serde_json::json!({
            "threadId": "th-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "reason": "needs access",
            "scope": scope,
            "permissions": {
                "network": {"enabled": true},
                "fileSystem": {"write": ["/proj", "/shared"]}
            }
        })
    }

    fn permission_pending(scope: &str) -> ExternalPendingApproval {
        ExternalPendingApproval::from_codex(
            "7".into(),
            "item-1".into(),
            PERMISSIONS_APPROVAL_METHOD.into(),
            &permission_raw(scope),
        )
    }

    fn facts(state: TaskState) -> TaskFacts {
        TaskFacts {
            state,
            approval: None,
            contract: None,
            change: None,
            waiting_reason: None,
            plan_request: false,
            mode_is_yolo: false,
        }
    }

    #[test]
    fn backend_switch_is_refused_while_busy_and_clears_codex_on_the_way_out() {
        let mut orch = ComposerOrchestrator::new();
        orch.backend_choice = AgentBackendChoice::CodexAppServer;
        orch.backend_capability_summary = Some("codex".into());
        orch.busy = true;
        assert!(!orch.try_select_backend(AgentBackendChoice::BuiltIn));
        assert_eq!(orch.backend_choice, AgentBackendChoice::CodexAppServer);
        assert_eq!(orch.backend_capability_summary.as_deref(), Some("codex"));

        orch.busy = false;
        orch.pending_edit = Some(PendingEdit {
            summary: proposal(),
            decisions: HashMap::new(),
        });
        assert!(orch.submission_blocked());
        assert!(!orch.try_select_backend(AgentBackendChoice::BuiltIn));

        orch.pending_edit = None;
        assert!(orch.try_select_backend(AgentBackendChoice::BuiltIn));
        assert_eq!(orch.backend_choice, AgentBackendChoice::BuiltIn);
        assert!(orch.backend_capability_summary.is_none());
        assert!(orch.external_session.is_none());
    }

    #[test]
    fn permission_approval_grants_only_the_checked_subset() {
        let mut pending = permission_pending("session");
        assert_eq!(pending.scope, "session");
        assert!(pending.permissions.iter().all(|choice| choice.checked));
        assert_eq!(
            pending
                .permissions
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fileSystem.write[0]", "fileSystem.write[1]", "network"]
        );

        let mut orch = ComposerOrchestrator::new();
        orch.external_pending_approval = Some(pending);
        assert!(orch.toggle_permission(1));
        assert!(!orch.toggle_permission(9));
        pending = orch.external_pending_approval.take().unwrap();
        assert!(!pending.permissions[1].checked);
        assert_eq!(
            pending.granted_permission_ids(true).as_deref(),
            Some(["fileSystem.write[0]".to_string(), "network".to_string()].as_slice())
        );
        assert_eq!(
            pending.granted_permission_ids(false).as_deref(),
            Some([].as_slice())
        );

        orch.external_pending_approval = Some(pending);
        match orch.take_approval(true, false) {
            ApprovalAction::External(ExternalCommand::Respond {
                approved,
                granted_permission_ids,
            }) => {
                assert!(approved);
                assert_eq!(
                    granted_permission_ids.as_deref(),
                    Some(["fileSystem.write[0]".to_string(), "network".to_string()].as_slice())
                );
            }
            _ => panic!("expected an external approval response"),
        }
        assert!(orch.external_pending_approval.is_none());
        let reply = orch.last_external_approval.as_ref().unwrap();
        assert!(reply.approved);
        assert_eq!(reply.granted_permission_ids.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn approve_while_busy_keeps_the_card_and_reject_takes_it() {
        let mut orch = ComposerOrchestrator::new();
        orch.busy = true;
        orch.external_pending_approval = Some(permission_pending("turn"));
        assert!(matches!(
            orch.take_approval(true, true),
            ApprovalAction::None
        ));
        assert!(orch.external_pending_approval.is_some());

        match orch.take_approval(false, false) {
            ApprovalAction::External(ExternalCommand::Respond {
                approved,
                granted_permission_ids,
            }) => {
                assert!(!approved);
                assert_eq!(granted_permission_ids.as_deref(), Some([].as_slice()));
            }
            _ => panic!("reject takes the card even while busy"),
        }
        assert!(orch.external_pending_approval.is_none());
    }

    #[test]
    fn non_permission_approval_does_not_carry_permission_ids() {
        let pending = ExternalPendingApproval::from_codex(
            "3".into(),
            "call-9".into(),
            "item/tool/call".into(),
            &serde_json::json!({"command": "ls"}),
        );
        assert!(!pending.is_permissions());
        assert!(pending.granted_permission_ids(true).is_none());
        assert_eq!(pending.scope, "");

        let mut orch = ComposerOrchestrator::new();
        orch.pending_approval = Some(PendingApproval {
            request: approval("call-9"),
            contract: contract("write_file"),
        });
        match orch.take_approval(true, false) {
            ApprovalAction::Builtin {
                call_id,
                tool_name,
                approved,
            } => {
                assert_eq!(call_id, "call-9");
                assert_eq!(tool_name, "write_file");
                assert!(approved);
            }
            _ => panic!("expected a built-in approval"),
        }
        assert!(orch.pending_approval.is_none());
    }

    #[test]
    fn edit_review_requires_every_hunk_and_a_matching_proposal() {
        let mut orch = ComposerOrchestrator::new();
        orch.pending_edit = Some(PendingEdit {
            summary: proposal(),
            decisions: HashMap::new(),
        });
        assert!(matches!(orch.prepared_edit(), PreparedEdit::Incomplete));
        assert!(!orch.take_matching_edit("other"));
        assert!(orch.pending_edit.is_some());

        assert!(orch.decide_hunk(0, true));
        assert!(orch.decide_hunk(1, false));
        match orch.prepared_edit() {
            PreparedEdit::Ready {
                proposal_id,
                accepted,
            } => {
                assert_eq!(proposal_id, "edit-1");
                assert_eq!(accepted, vec![0]);
            }
            _ => panic!("both hunks are decided"),
        }
        assert!(orch.take_matching_edit("edit-1"));
        assert!(matches!(orch.prepared_edit(), PreparedEdit::Missing));
        assert!(matches!(orch.auto_resolve(), AutoResolve::None));
    }

    #[test]
    fn task_result_paths_keep_release_or_drop_without_a_window() {
        let mut orch = ComposerOrchestrator::new();
        orch.busy = true;
        orch.active_plan_request = false;
        let task = idle_task("hello");
        let history = orch.begin_task_result(&task);
        assert_eq!(history.len(), 1);
        assert!(!orch.busy);
        let commit = orch.commit_task_result(task, false);
        assert!(orch.active_task.is_some());
        assert!(commit.auto_resolve);
        assert!(!commit.clear_plan);
        assert!(matches!(commit.status, TaskStatusLine::Other { .. }));
        assert_eq!(commit.notify, AgentStatus::Working);

        orch.fail_task();
        assert!(orch.active_runtime.is_none());
        let task = orch.active_task.take().unwrap();
        orch.keep_task(task);
        assert!(orch.active_task.is_some());

        orch.active_runtime = Some(sample_runtime());
        let finished = classify_facts(TaskFacts {
            plan_request: true,
            ..facts(TaskState::CompletedVerified)
        });
        assert!(!finished.commit.clear_plan);
        assert!(matches!(
            finished.commit.hint,
            TaskUiHint::AwaitPlanConfirmation
        ));
        assert_eq!(finished.commit.notify, AgentStatus::Attention);
        let task = orch.active_task.take().unwrap();
        let commit = orch.apply_classified(task, finished);
        assert!(
            orch.active_task.is_some(),
            "a verified plan stays visible until the status event is delivered"
        );
        assert!(orch.active_runtime.is_some());
        orch.finish_task_commit(&commit);
        assert!(orch.active_task.is_none());
        assert!(orch.active_runtime.is_none());
        assert!(!commit.clear_plan);
        assert!(commit.release_after_notify);
        assert!(matches!(commit.hint, TaskUiHint::AwaitPlanConfirmation));

        let verified = classify_facts(facts(TaskState::CompletedVerified));
        assert!(verified.commit.clear_plan);
        assert!(matches!(
            verified.commit.status,
            TaskStatusLine::FinishedVerified
        ));
        assert_eq!(verified.commit.notify, AgentStatus::Finished);

        let lost = classify_facts(facts(TaskState::WaitingApproval));
        assert!(!lost.commit.auto_resolve);
        assert!(matches!(lost.commit.status, TaskStatusLine::LostApproval));
        assert_eq!(lost.commit.notify, AgentStatus::Error);

        let mut waiting = facts(TaskState::WaitingApproval);
        waiting.approval = Some(approval("c1"));
        waiting.contract = Some(contract("write_file"));
        let pending = classify_facts(waiting);
        assert!(pending.commit.auto_resolve);
        assert!(matches!(
            pending.commit.status,
            TaskStatusLine::ApprovalRequired { .. }
        ));
        match &pending.approval {
            Some(pending) => assert_eq!(pending.request.call_id, "c1"),
            None => panic!("approval card is stored"),
        }

        let mut review = facts(TaskState::WaitingChangeReview);
        review.change = Some(proposal());
        review.mode_is_yolo = true;
        let yolo = classify_facts(review);
        assert!(yolo.commit.review.is_none());
        assert!(yolo.edit.is_some());

        let mut review = facts(TaskState::WaitingChangeReview);
        review.change = Some(proposal());
        let prompted = classify_facts(review);
        assert!(prompted.commit.review.is_some());

        let invalid = classify_facts(facts(TaskState::WaitingChangeReview));
        assert!(matches!(
            invalid.commit.status,
            TaskStatusLine::InvalidSummary
        ));
        assert!(!invalid.commit.auto_resolve);

        let mut user = facts(TaskState::WaitingUser);
        user.waiting_reason = Some(WaitingReason::TerminalControl {
            session_id: "cmd-1".into(),
            cursor: 4,
        });
        let takeover = classify_facts(user);
        assert!(matches!(takeover.commit.hint, TaskUiHint::OpenTaskDetails));
        assert!(matches!(
            takeover.commit.status,
            TaskStatusLine::TerminalTakenOver { cursor: 4, .. }
        ));

        let mut user = facts(TaskState::WaitingUser);
        user.waiting_reason = Some(WaitingReason::BudgetExhausted {
            dimension: BudgetDimension::ModelSteps,
            used: 200,
            limit: 200,
        });
        let budget = classify_facts(user);
        match budget.commit.status {
            TaskStatusLine::BudgetExhausted { used, limit, .. } => {
                assert_eq!((used, limit), (200, 200));
            }
            _ => panic!("budget exhaustion keeps the task"),
        }

        assert!(matches!(
            classify_facts(facts(TaskState::Failed)).commit.status,
            TaskStatusLine::TaskFailed
        ));
        assert_eq!(
            classify_facts(facts(TaskState::Cancelled)).commit.notify,
            AgentStatus::Finished
        );
        let cancelled_plan = classify_facts(TaskFacts {
            plan_request: true,
            ..facts(TaskState::Cancelled)
        });
        assert!(!cancelled_plan.commit.clear_plan);
    }

    #[test]
    fn dispatch_gates_and_budget_extension_do_not_need_gpui() {
        let mut orch = ComposerOrchestrator::new();
        assert!(matches!(orch.stop_action(), StopAction::Idle));
        assert!(matches!(
            orch.begin_task_command(ApprovalPolicy::Prompt),
            Err(TaskDispatchReject::NoTask)
        ));
        assert!(orch.begin_external().is_some());
        assert!(orch.busy);
        assert!(orch.begin_external().is_none());
        assert!(matches!(
            orch.begin_task_command(ApprovalPolicy::Yolo),
            Err(TaskDispatchReject::Busy)
        ));
        orch.busy = false;
        orch.active_cancellation = None;

        let task = idle_task("again");
        orch.install_builtin_task(task, sample_runtime(), true);
        assert!(orch.active_plan_request);
        assert!(orch.terminal_action_busy());
        assert!(matches!(orch.stop_action(), StopAction::CancelToken(_)));
        let dispatch = orch
            .begin_task_command(ApprovalPolicy::Yolo)
            .expect("runtime is installed");
        assert!(orch.busy);
        assert!(orch.active_task.is_none());
        assert_eq!(
            dispatch.task.approval_policy(),
            ApprovalPolicy::Yolo,
            "resume adopts the policy captured at dispatch"
        );

        orch.busy = false;
        orch.install_builtin_task(idle_task("third"), sample_runtime(), false);
        orch.runtime = None;
        orch.active_runtime = None;
        assert!(matches!(
            orch.begin_task_command(ApprovalPolicy::Prompt),
            Err(TaskDispatchReject::NoRuntime)
        ));
        assert!(orch.active_task.is_some(), "the task is put back");

        assert!(increase_budgets(TaskState::Idle, &RuntimeBudgets::default()).is_none());
        let extended =
            increase_budgets(TaskState::WaitingUser, &RuntimeBudgets::default()).unwrap();
        let base = RuntimeBudgets::default();
        assert_eq!(extended.max_model_steps, base.max_model_steps + 100);
        assert_eq!(
            extended.max_wall_clock_ms,
            base.max_wall_clock_ms + 30 * 60 * 1_000
        );
        assert_eq!(
            extended.max_tool_output_bytes,
            base.max_tool_output_bytes + 16 * 1024 * 1024
        );

        orch.busy = true;
        assert!(orch.blocks_mode_switch(Mode::Plan));
        orch.busy = false;
        orch.pending_approval = Some(PendingApproval {
            request: approval("c2"),
            contract: contract("write_file"),
        });
        assert!(orch.blocks_mode_switch(Mode::Auto));
        assert!(!orch.blocks_mode_switch(Mode::Yolo));
        assert!(matches!(orch.auto_resolve(), AutoResolve::Tool));

        orch.clear_for_new_session();
        assert!(orch.active_task.is_none());
        assert!(
            orch.pending_approval.is_some(),
            "a new session is refused while a card is up, so clear does not drop it"
        );
    }

    #[test]
    fn external_result_stores_approvals_and_clears_them_on_completion() {
        let mut orch = ComposerOrchestrator::new();
        orch.busy = true;
        orch.external_pending_approval = Some(permission_pending("turn"));
        let outcome = orch.apply_external_result(ExternalRunResult {
            session: None,
            outcome: None,
            error: Some("connect failed".into()),
        });
        assert!(matches!(outcome, ExternalOutcome::Error { .. }));
        assert_eq!(outcome.notify(), AgentStatus::Error);
        assert!(!orch.busy);
        assert!(
            orch.external_pending_approval.is_some(),
            "an error leaves the previous card in place"
        );

        let outcome = orch.apply_external_result(ExternalRunResult {
            session: None,
            outcome: Some(PumpOutcome::WaitingApproval {
                backend_request_id: "8".into(),
                tool_call_id: "item-2".into(),
                request_kind: PERMISSIONS_APPROVAL_METHOD.into(),
                raw: permission_raw("something-new"),
            }),
            error: None,
        });
        match &outcome {
            ExternalOutcome::Approval { call } => assert_eq!(call, "item-2"),
            _ => panic!("waiting approval"),
        }
        assert_eq!(outcome.notify(), AgentStatus::Attention);
        assert_eq!(
            orch.external_pending_approval.as_ref().unwrap().scope,
            "something-new"
        );

        let outcome = orch.apply_external_result(ExternalRunResult {
            session: None,
            outcome: Some(PumpOutcome::Pending),
            error: None,
        });
        assert!(matches!(outcome, ExternalOutcome::Running));
        assert!(orch.external_pending_approval.is_some());

        let outcome = orch.apply_external_result(ExternalRunResult {
            session: None,
            outcome: Some(PumpOutcome::Completed {
                success: true,
                status: "ok".into(),
            }),
            error: None,
        });
        assert!(matches!(
            outcome,
            ExternalOutcome::Completed { success: true, .. }
        ));
        assert_eq!(outcome.notify(), AgentStatus::Finished);
        assert!(orch.external_pending_approval.is_none());

        let outcome = orch.apply_external_result(ExternalRunResult {
            session: None,
            outcome: Some(PumpOutcome::Unknown {
                reason: "lost".into(),
            }),
            error: None,
        });
        assert!(matches!(outcome, ExternalOutcome::Unknown { .. }));
        assert_eq!(outcome.notify(), AgentStatus::Attention);
    }

    #[test]
    fn bound_worktree_replaces_the_main_workspace_and_a_project_pin() {
        let mut orch = ComposerOrchestrator::new();
        orch.pin_project(PathBuf::from("/main/workspace"));
        orch.bind_worktree(WorktreeEnvironment {
            id: "worktree-task-17".into(),
            repository: PathBuf::from("/main/workspace"),
            base_commit: "abc".into(),
            branch: "task-branch".into(),
            path: PathBuf::from("/tmp/checkout"),
            owner_task: "task-17".into(),
            orphaned: false,
        });
        let execution = orch.take_execution_root(Path::new("/main/workspace"));
        assert_eq!(execution.root, PathBuf::from("/tmp/checkout"));
        assert_eq!(execution.environment_id, "worktree-task-17");
        assert!(execution.worktree.is_some());
        assert!(orch.task_project_override.is_none());
        assert_eq!(
            orch.displayed_root(Path::new("/main/workspace")),
            PathBuf::from("/tmp/checkout")
        );
    }

    #[test]
    fn conflicting_merge_back_requests_git_review_and_keeps_user_edits() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("main");
        std::fs::create_dir(&repository).unwrap();
        git(&repository, &["init"]);
        git(&repository, &["config", "core.autocrlf", "false"]);
        git(&repository, &["config", "user.email", "test@example.com"]);
        git(&repository, &["config", "user.name", "Termior Test"]);
        std::fs::write(repository.join("a.txt"), "base\n").unwrap();
        git(&repository, &["add", "."]);
        git(&repository, &["commit", "-m", "base"]);
        let destination = root.path().join("checkout");
        let environment =
            WorktreeManager::create(&repository, &destination, "HEAD", "task-branch", "task-17")
                .unwrap();
        std::fs::write(repository.join("a.txt"), "user-commit\n").unwrap();
        git(&repository, &["add", "a.txt"]);
        git(&repository, &["commit", "-m", "user"]);
        std::fs::write(environment.path.join("a.txt"), "task-commit\n").unwrap();
        git(&environment.path, &["add", "a.txt"]);
        git(&environment.path, &["commit", "-m", "task"]);
        std::fs::write(repository.join("notes.txt"), "user scratch\n").unwrap();
        let worktree_path = environment.path.clone();

        let mut orch = ComposerOrchestrator::new();
        orch.bind_worktree(environment);
        let outcome = orch.integrate_worktree("HEAD", true).unwrap();
        assert!(matches!(
            outcome,
            MergeBack::Review {
                reason: termior_vcs::MergeBackReason::DirtyTarget,
                ..
            }
        ));
        let review = orch.take_git_review().expect("git review");
        assert_eq!(review.reason, "dirty-target");
        assert_eq!(
            review.repository,
            std::fs::canonicalize(&repository).unwrap()
        );
        assert_eq!(review.worktree_path, worktree_path);
        assert!(review.patch.contains("a.txt"), "{}", review.patch);
        assert!(review.conflicts.is_empty());
        assert_eq!(
            std::fs::read_to_string(repository.join("a.txt")).unwrap(),
            "user-commit\n"
        );
        assert_eq!(
            std::fs::read_to_string(repository.join("notes.txt")).unwrap(),
            "user scratch\n"
        );
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn sample_runtime() -> AgentRuntime {
        let root = std::env::temp_dir();
        let mut tools = ToolRegistry::default();
        let root_label = root.to_string_lossy().into_owned();
        tools.workspace_auth.authorize(&root_label);
        let executor = Arc::new(
            ToolExecutor::new(&root, tools.clone()).expect("temp dir is an authorized workspace"),
        );
        AgentRuntime {
            config: ProviderConfig::for_kind(ProviderKind::Ollama, "test"),
            tools,
            executor,
            system_prompt: "test".into(),
        }
    }
}
