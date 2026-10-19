//! Bounded child-task DAG and centralized scheduler (Stage E / M10).

use crate::RuntimeBudgets;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotVersion(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDepth(pub u8);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildTaskSpec {
    pub task_id: String,
    pub parent_task_id: String,
    #[serde(default)]
    pub agent_id: String,
    pub goal: String,
    pub input_refs: Vec<String>,
    pub output_schema: Value,
    pub tools: BTreeSet<String>,
    pub budgets: RuntimeBudgets,
    pub environment_id: String,
    pub provider_id: String,
    pub workspace_id: String,
    pub project_dir: PathBuf,
    pub dependencies: Vec<String>,
    pub depth: TaskDepth,
    pub writes: bool,
    pub snapshot: SnapshotVersion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildTaskLaunchContext {
    pub parent_task_id: String,
    pub tools: BTreeSet<String>,
    pub budgets: RuntimeBudgets,
    pub environment_id: String,
    pub provider_id: String,
    pub workspace_id: String,
    pub project_dir: PathBuf,
    pub depth: TaskDepth,
    pub snapshot: SnapshotVersion,
}

impl ChildTaskSpec {
    pub fn from_plan_step(
        step: &crate::PlanStep,
        task_id: impl Into<String>,
        context: ChildTaskLaunchContext,
    ) -> Result<Self, OrchestrationError> {
        let crate::PlanStepKind::Subagent { agent_id, task } = &step.kind else {
            return Err(OrchestrationError::NotSubagentStep(step.id.clone()));
        };
        if step.status != crate::PlanStepStatus::Approved {
            return Err(OrchestrationError::StepNotApproved(step.id.clone()));
        }
        Ok(Self {
            task_id: task_id.into(),
            parent_task_id: context.parent_task_id,
            agent_id: agent_id.clone(),
            goal: task.clone(),
            input_refs: Vec::new(),
            output_schema: serde_json::json!({
                "type":"object",
                "required":["answer","evidence"],
                "properties":{
                    "answer":{"type":"string"},
                    "evidence":{"type":"array","items":{"type":"string"}}
                },
                "additionalProperties":false
            }),
            tools: context.tools,
            budgets: context.budgets,
            environment_id: context.environment_id,
            provider_id: context.provider_id,
            workspace_id: context.workspace_id,
            project_dir: context.project_dir,
            dependencies: Vec::new(),
            depth: context.depth,
            writes: false,
            snapshot: context.snapshot,
        })
    }
}

impl ChildTaskSpec {
    pub fn validate_against_parent(
        &self,
        parent_tools: &BTreeSet<String>,
        parent_budget: &RuntimeBudgets,
        max_depth: u8,
    ) -> Result<(), OrchestrationError> {
        if self.depth.0 > max_depth {
            return Err(OrchestrationError::DepthExceeded {
                depth: self.depth.0,
                max: max_depth,
            });
        }
        if !self.tools.is_subset(parent_tools) {
            return Err(OrchestrationError::PermissionExpansion);
        }
        if self.budgets.max_model_steps > parent_budget.max_model_steps
            || self.budgets.max_wall_clock_ms > parent_budget.max_wall_clock_ms
            || self.budgets.max_tool_output_bytes > parent_budget.max_tool_output_bytes
            || option_exceeds(
                self.budgets.max_input_tokens,
                parent_budget.max_input_tokens,
            )
            || option_exceeds(
                self.budgets.max_output_tokens,
                parent_budget.max_output_tokens,
            )
        {
            return Err(OrchestrationError::BudgetExpansion);
        }
        Ok(())
    }
}

fn option_exceeds(child: Option<u64>, parent: Option<u64>) -> bool {
    match (child, parent) {
        (Some(child), Some(parent)) => child > parent,
        (None, Some(_)) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeState {
    Queued,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    Blocked,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyCondition {
    Succeeded,
    Terminal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRelation {
    pub prerequisite: String,
    pub dependent: String,
    pub condition: DependencyCondition,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
    pub nodes: BTreeMap<String, NodeState>,
    pub relations: Vec<TaskRelation>,
}
impl TaskGraph {
    pub fn add_node(&mut self, id: impl Into<String>) {
        self.nodes.entry(id.into()).or_insert(NodeState::Queued);
    }
    pub fn add_dependency(
        &mut self,
        prerequisite: &str,
        dependent: &str,
        condition: DependencyCondition,
    ) -> Result<(), OrchestrationError> {
        if prerequisite == dependent || self.reachable(dependent, prerequisite) {
            return Err(OrchestrationError::Cycle);
        }
        if !self.nodes.contains_key(prerequisite) || !self.nodes.contains_key(dependent) {
            return Err(OrchestrationError::MissingNode);
        }
        self.relations.push(TaskRelation {
            prerequisite: prerequisite.into(),
            dependent: dependent.into(),
            condition,
        });
        Ok(())
    }
    pub fn refresh(&mut self) {
        for id in self.nodes.keys().cloned().collect::<Vec<_>>() {
            if self.nodes[&id] != NodeState::Queued && self.nodes[&id] != NodeState::Blocked {
                continue;
            }
            let deps = self
                .relations
                .iter()
                .filter(|relation| relation.dependent == id)
                .collect::<Vec<_>>();
            let failed = deps.iter().any(|dep| {
                dep.condition == DependencyCondition::Succeeded
                    && matches!(
                        self.nodes[&dep.prerequisite],
                        NodeState::Failed | NodeState::Cancelled | NodeState::Unknown
                    )
            });
            let ready = deps.iter().all(|dep| match dep.condition {
                DependencyCondition::Succeeded => {
                    self.nodes[&dep.prerequisite] == NodeState::Succeeded
                }
                DependencyCondition::Terminal => matches!(
                    self.nodes[&dep.prerequisite],
                    NodeState::Succeeded
                        | NodeState::Failed
                        | NodeState::Cancelled
                        | NodeState::Unknown
                ),
            });
            self.nodes.insert(
                id,
                if failed {
                    NodeState::Blocked
                } else if ready {
                    NodeState::Ready
                } else {
                    NodeState::Queued
                },
            );
        }
    }

    pub fn cancel_descendants(&mut self, root: &str, confirmed_stopped: &BTreeSet<String>) {
        let mut stack = vec![root.to_owned()];
        let mut descendants = BTreeSet::new();
        while let Some(parent) = stack.pop() {
            for child in self
                .relations
                .iter()
                .filter(|edge| edge.prerequisite == parent)
                .map(|edge| edge.dependent.clone())
            {
                if descendants.insert(child.clone()) {
                    stack.push(child);
                }
            }
        }
        for id in descendants {
            let next = match self.nodes.get(&id).copied() {
                Some(NodeState::Running) if !confirmed_stopped.contains(&id) => NodeState::Unknown,
                Some(
                    NodeState::Succeeded
                    | NodeState::Failed
                    | NodeState::Cancelled
                    | NodeState::Unknown,
                ) => continue,
                _ => NodeState::Cancelled,
            };
            self.nodes.insert(id, next);
        }
    }
    fn reachable(&self, from: &str, target: &str) -> bool {
        let mut stack = vec![from];
        let mut seen = BTreeSet::new();
        while let Some(node) = stack.pop() {
            if node == target {
                return true;
            }
            if seen.insert(node) {
                stack.extend(
                    self.relations
                        .iter()
                        .filter(|edge| edge.prerequisite == node)
                        .map(|edge| edge.dependent.as_str()),
                );
            }
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotLimits {
    pub global: usize,
    pub per_provider: usize,
    pub per_workspace: usize,
    pub per_environment: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueReason {
    ProviderSlot,
    WorkspaceSlot,
    EnvironmentSlot,
    GlobalSlot,
    ProviderBackoff,
}

impl QueueReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::ProviderSlot => "provider-slot",
            Self::WorkspaceSlot => "workspace-slot",
            Self::EnvironmentSlot => "environment-slot",
            Self::GlobalSlot => "global-slot",
            Self::ProviderBackoff => "provider-backoff",
        }
    }

    /// Condition that must become true before a ready task can leave the queue.
    pub fn ready_condition(self, task: &ScheduledTask, global_cap: usize) -> String {
        match self {
            Self::GlobalSlot => {
                format!("a global concurrency slot is free (cap {global_cap})")
            }
            Self::ProviderSlot => format!("a provider slot for {} is free", task.provider),
            Self::WorkspaceSlot => format!("a workspace slot for {} is free", task.workspace),
            Self::EnvironmentSlot => {
                format!("an environment slot for {} is free", task.environment)
            }
            Self::ProviderBackoff => format!("provider {} backoff ends", task.provider),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub provider: String,
    pub workspace: String,
    pub environment: String,
    pub priority: u16,
    pub sequence: u64,
}
pub struct Scheduler {
    limits: SlotLimits,
    ready: VecDeque<ScheduledTask>,
    running: BTreeMap<String, ScheduledTask>,
    provider_backoff_until: BTreeMap<String, u64>,
}

pub trait TaskLauncher: Send + Sync + 'static {
    fn launch(
        &self,
        spec: ChildTaskSpec,
        cancellation: crate::CancellationToken,
    ) -> Result<ChildTaskResult, String>;
}

impl<F> TaskLauncher for F
where
    F: Fn(ChildTaskSpec, crate::CancellationToken) -> Result<ChildTaskResult, String>
        + Send
        + Sync
        + 'static,
{
    fn launch(
        &self,
        spec: ChildTaskSpec,
        cancellation: crate::CancellationToken,
    ) -> Result<ChildTaskResult, String> {
        self(spec, cancellation)
    }
}

/// Executes ready DAG nodes on bounded worker threads while the scheduler owns every slot.
///
/// Each worker receives its own cancellation token and immutable child spec. Results are accepted
/// only after evidence/snapshot/write validation; invalid or unknown results never unblock a
/// `Succeeded` dependency.
pub struct OrchestrationRuntime<L: TaskLauncher> {
    launcher: Arc<L>,
    scheduler: Scheduler,
    graph: TaskGraph,
    specs: BTreeMap<String, ChildTaskSpec>,
    results: BTreeMap<String, ChildTaskResult>,
    cancellations: BTreeMap<String, crate::CancellationToken>,
    completion_tx: mpsc::Sender<(String, Result<ChildTaskResult, String>)>,
    completion_rx: mpsc::Receiver<(String, Result<ChildTaskResult, String>)>,
    next_sequence: u64,
}

/// Actual consumption reported by a task. Every field stays `None` until that
/// dimension is known, so an unreported price cannot be shown as zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskResourceUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub wall_clock_ms: Option<u64>,
    pub output_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendUsageRollup {
    pub provider_id: String,
    pub usage: TaskResourceUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskTreeNode {
    pub task_id: String,
    pub parent_task_id: String,
    pub agent_id: String,
    pub provider_id: String,
    pub environment_id: String,
    pub project_dir: PathBuf,
    pub dependencies: Vec<String>,
    pub state: NodeState,
    pub budgets: RuntimeBudgets,
    pub queue_reason: Option<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub ready_condition: Option<String>,
    pub result_verified: Option<bool>,
    pub unknown_side_effects: bool,
    #[serde(default)]
    pub usage: TaskResourceUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskTreeSnapshot {
    pub generated_at_ms: u64,
    #[serde(default = "default_concurrency_cap")]
    pub concurrency_cap: usize,
    pub nodes: Vec<TaskTreeNode>,
    #[serde(default)]
    pub aggregate: TaskResourceUsage,
    #[serde(default)]
    pub backends: Vec<BackendUsageRollup>,
}

fn default_concurrency_cap() -> usize {
    2
}

impl Default for TaskTreeSnapshot {
    fn default() -> Self {
        Self {
            generated_at_ms: 0,
            concurrency_cap: default_concurrency_cap(),
            nodes: Vec::new(),
            aggregate: TaskResourceUsage::default(),
            backends: Vec::new(),
        }
    }
}

impl TaskTreeSnapshot {
    pub fn set_concurrency_cap(&mut self, cap: usize) {
        let cap = cap.clamp(1, 64);
        self.concurrency_cap = cap;
        for node in &mut self.nodes {
            if node.queue_reason.as_deref() == Some(QueueReason::GlobalSlot.code()) {
                node.ready_condition =
                    Some(format!("a global concurrency slot is free (cap {cap})"));
            }
        }
    }

    pub fn cancel_child(&mut self, task_id: &str) -> Result<(), OrchestrationError> {
        if !self.nodes.iter().any(|node| node.task_id == task_id) {
            return Err(OrchestrationError::MissingNode);
        }
        let affected = descendant_task_ids(&self.nodes, task_id);
        self.cancel_ids(&affected);
        Ok(())
    }

    pub fn cancel_all(&mut self) {
        let ids = self
            .nodes
            .iter()
            .map(|node| node.task_id.clone())
            .collect::<BTreeSet<_>>();
        self.cancel_ids(&ids);
    }

    pub fn delete_record(&mut self, task_id: &str) -> Result<(), OrchestrationError> {
        let Some(node) = self.nodes.iter().find(|node| node.task_id == task_id) else {
            return Err(OrchestrationError::MissingNode);
        };
        if !can_delete_task_record(node) {
            return Err(OrchestrationError::UnknownRecordRetained);
        }
        self.nodes.retain(|node| node.task_id != task_id);
        for node in &mut self.nodes {
            node.dependencies.retain(|dependency| dependency != task_id);
            node.blockers.retain(|blocker| blocker != task_id);
        }
        self.recompute_rollups();
        Ok(())
    }

    fn cancel_ids(&mut self, ids: &BTreeSet<String>) {
        for node in &mut self.nodes {
            if ids.contains(&node.task_id) {
                apply_cancel(node);
            }
        }
    }

    fn recompute_rollups(&mut self) {
        self.aggregate = aggregate_usage(self.nodes.iter().map(|node| &node.usage));
        self.backends = backend_rollups(&self.nodes);
    }
}

/// Records with unknown side effects stay visible. In-flight nodes must be cancelled first.
pub fn can_delete_task_record(node: &TaskTreeNode) -> bool {
    if node.unknown_side_effects || node.state == NodeState::Unknown {
        return false;
    }
    matches!(
        node.state,
        NodeState::Succeeded
            | NodeState::Failed
            | NodeState::Cancelled
            | NodeState::Blocked
            | NodeState::Stale
    )
}

pub fn format_cost_usd(cost: Option<f64>) -> String {
    let Some(value) = cost else {
        return "unknown".to_owned();
    };
    if !value.is_finite() {
        return "unknown".to_owned();
    }
    let rendered = format!("{value:.6}");
    if rendered.trim_start_matches('-') == "0.000000" {
        return "0".to_owned();
    }
    let mut text = rendered;
    while text.contains('.') && (text.ends_with('0') || text.ends_with('.')) {
        text.pop();
    }
    text
}

pub fn format_optional_quantity(value: Option<u64>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "unknown".to_owned(),
    }
}

/// Display line for token, cost, time, and output. Unknown cost is the word `unknown`.
pub fn format_task_usage(usage: &TaskResourceUsage) -> String {
    format!(
        "tokens {}/{} · cost {} · time {} · output {}",
        format_optional_quantity(usage.input_tokens),
        format_optional_quantity(usage.output_tokens),
        format_cost_usd(usage.cost_usd),
        format_optional_quantity(usage.wall_clock_ms),
        format_optional_quantity(usage.output_bytes),
    )
}

pub fn aggregate_usage<'a>(
    usages: impl IntoIterator<Item = &'a TaskResourceUsage>,
) -> TaskResourceUsage {
    let usages = usages.into_iter().collect::<Vec<_>>();
    TaskResourceUsage {
        input_tokens: sum_optional_u64(usages.iter().map(|usage| usage.input_tokens)),
        output_tokens: sum_optional_u64(usages.iter().map(|usage| usage.output_tokens)),
        cost_usd: sum_optional_cost(usages.iter().map(|usage| usage.cost_usd)),
        wall_clock_ms: sum_optional_u64(usages.iter().map(|usage| usage.wall_clock_ms)),
        output_bytes: sum_optional_u64(usages.iter().map(|usage| usage.output_bytes)),
    }
}

pub fn backend_rollups(nodes: &[TaskTreeNode]) -> Vec<BackendUsageRollup> {
    let providers = nodes
        .iter()
        .map(|node| node.provider_id.clone())
        .collect::<BTreeSet<_>>();
    providers
        .into_iter()
        .map(|provider_id| {
            let usage = aggregate_usage(
                nodes
                    .iter()
                    .filter(|node| node.provider_id == provider_id)
                    .map(|node| &node.usage),
            );
            BackendUsageRollup { provider_id, usage }
        })
        .collect()
}

fn sum_optional_u64(values: impl Iterator<Item = Option<u64>>) -> Option<u64> {
    let mut total = 0u64;
    let mut seen = false;
    for value in values {
        total = total.saturating_add(value?);
        seen = true;
    }
    seen.then_some(total)
}

fn sum_optional_cost(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = 0.0;
    let mut seen = false;
    for value in values {
        let value = value.filter(|value| value.is_finite())?;
        total += value;
        seen = true;
    }
    seen.then_some(total)
}

fn apply_cancel(node: &mut TaskTreeNode) {
    match node.state {
        NodeState::Running => {
            node.state = NodeState::Unknown;
            node.unknown_side_effects = true;
            node.queue_reason = None;
            node.ready_condition = None;
        }
        NodeState::Queued | NodeState::Ready | NodeState::Blocked | NodeState::Stale => {
            node.state = NodeState::Cancelled;
            node.queue_reason = None;
            node.ready_condition = None;
            node.blockers.clear();
        }
        NodeState::Succeeded | NodeState::Failed | NodeState::Cancelled | NodeState::Unknown => {}
    }
}

fn descendant_task_ids(nodes: &[TaskTreeNode], root: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::from([root.to_owned()]);
    let mut grew = true;
    while grew {
        grew = false;
        for node in nodes {
            if found.contains(&node.task_id) {
                continue;
            }
            if node
                .dependencies
                .iter()
                .any(|dependency| found.contains(dependency))
                && found.insert(node.task_id.clone())
            {
                grew = true;
            }
        }
    }
    found
}

impl<L: TaskLauncher> OrchestrationRuntime<L> {
    pub fn new(limits: SlotLimits, launcher: Arc<L>) -> Self {
        let (completion_tx, completion_rx) = mpsc::channel();
        Self {
            launcher,
            scheduler: Scheduler::new(limits),
            graph: TaskGraph::default(),
            specs: BTreeMap::new(),
            results: BTreeMap::new(),
            cancellations: BTreeMap::new(),
            completion_tx,
            completion_rx,
            next_sequence: 0,
        }
    }

    pub fn graph(&self) -> &TaskGraph {
        &self.graph
    }

    pub fn result(&self, task_id: &str) -> Option<&ChildTaskResult> {
        self.results.get(task_id)
    }

    pub fn submit(&mut self, spec: ChildTaskSpec, priority: u16) -> Result<(), OrchestrationError> {
        if self.specs.contains_key(&spec.task_id) {
            return Err(OrchestrationError::DuplicateTask(spec.task_id));
        }
        for dependency in &spec.dependencies {
            if !self.graph.nodes.contains_key(dependency) {
                return Err(OrchestrationError::MissingNode);
            }
        }
        self.graph.add_node(spec.task_id.clone());
        for dependency in &spec.dependencies {
            self.graph
                .add_dependency(dependency, &spec.task_id, DependencyCondition::Succeeded)?;
        }
        self.next_sequence += 1;
        let state = if spec.dependencies.is_empty() {
            NodeState::Ready
        } else {
            NodeState::Queued
        };
        self.graph.nodes.insert(spec.task_id.clone(), state);
        if state == NodeState::Ready {
            self.scheduler
                .enqueue(scheduled(&spec, priority, self.next_sequence));
        }
        self.specs.insert(spec.task_id.clone(), spec);
        Ok(())
    }

    /// Reaps completed workers, refreshes dependencies, and fills every currently available slot.
    pub fn tick(&mut self, now_ms: u64) {
        while let Ok((task_id, result)) = self.completion_rx.try_recv() {
            self.scheduler.finish(&task_id);
            self.cancellations.remove(&task_id);
            let state = match result {
                Ok(result) => match result.validate_for(&self.specs[&task_id]) {
                    Ok(()) => {
                        self.results.insert(task_id.clone(), result);
                        NodeState::Succeeded
                    }
                    Err(OrchestrationError::StaleResult) => NodeState::Stale,
                    Err(OrchestrationError::UnknownResult) => NodeState::Unknown,
                    Err(_) => NodeState::Failed,
                },
                Err(_) => NodeState::Failed,
            };
            self.graph.nodes.insert(task_id, state);
        }

        let previously_ready = self
            .graph
            .nodes
            .iter()
            .filter_map(|(id, state)| (*state == NodeState::Ready).then_some(id.clone()))
            .collect::<BTreeSet<_>>();
        self.graph.refresh();
        for (id, state) in &self.graph.nodes {
            if *state == NodeState::Ready && !previously_ready.contains(id) {
                self.next_sequence += 1;
                let spec = &self.specs[id];
                self.scheduler
                    .enqueue(scheduled(spec, 0, self.next_sequence));
            }
        }

        while let Some(task) = self.scheduler.next_ready(now_ms) {
            let Some(spec) = self.specs.get(&task.id).cloned() else {
                self.scheduler.finish(&task.id);
                continue;
            };
            self.graph.nodes.insert(task.id.clone(), NodeState::Running);
            let cancellation = crate::CancellationToken::default();
            self.cancellations
                .insert(task.id.clone(), cancellation.clone());
            let launcher = self.launcher.clone();
            let completion = self.completion_tx.clone();
            let task_id = task.id.clone();
            let spawn_result = std::thread::Builder::new()
                .name(format!("termior-child-{task_id}"))
                .spawn(move || {
                    let worker_task_id = spec.task_id.clone();
                    // worker panic 不得静默挂起任务：映射为显式失败结果。
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        launcher.launch(spec, cancellation)
                    }))
                    .unwrap_or_else(|_| Err("child task worker panicked".to_owned()));
                    let _ = completion.send((worker_task_id, result));
                });
            if let Err(error) = spawn_result {
                // 线程 spawn 失败（句柄/内存耗尽）：显式失败而非永久 Running。
                let _ = self
                    .completion_tx
                    .send((task_id, Err(format!("worker spawn failed: {error}"))));
            }
        }
    }

    pub fn set_global_concurrency(&mut self, cap: usize) {
        self.scheduler.set_global_concurrency(cap);
    }

    pub fn global_concurrency(&self) -> usize {
        self.scheduler.global_concurrency()
    }

    /// Cancels one child and every task that depends on it. Records stay in the tree.
    pub fn cancel_child(&mut self, task_id: &str) -> Result<(), OrchestrationError> {
        if !self.specs.contains_key(task_id) {
            return Err(OrchestrationError::MissingNode);
        }
        self.cancel_tree(task_id);
        Ok(())
    }

    pub fn cancel_all(&mut self) {
        let ids = self.specs.keys().cloned().collect::<Vec<_>>();
        for id in ids {
            self.cancel_tree(&id);
        }
    }

    /// Drops a settled record. Unknown side effects are retained for inspection.
    pub fn delete_record(&mut self, task_id: &str) -> Result<(), OrchestrationError> {
        let snapshot = self.task_tree(0);
        let Some(node) = snapshot.nodes.iter().find(|node| node.task_id == task_id) else {
            return Err(OrchestrationError::MissingNode);
        };
        if !can_delete_task_record(node) {
            return Err(OrchestrationError::UnknownRecordRetained);
        }
        self.specs.remove(task_id);
        self.results.remove(task_id);
        self.graph.nodes.remove(task_id);
        self.graph
            .relations
            .retain(|relation| relation.prerequisite != task_id && relation.dependent != task_id);
        self.scheduler.remove(task_id);
        self.cancellations.remove(task_id);
        Ok(())
    }

    pub fn cancel_tree(&mut self, root: &str) {
        let affected = descendants_including(&self.graph, root);
        for task_id in &affected {
            self.scheduler.remove(task_id);
            if let Some(token) = self.cancellations.get(task_id) {
                token.cancel();
            }
        }
        self.graph.cancel_descendants(root, &BTreeSet::new());
        if let Some(state) = self.graph.nodes.get_mut(root) {
            *state = retained_cancel_state(
                *state,
                self.cancellations.contains_key(root)
                    || self.results.get(root).is_some_and(|result| result.unknown),
            );
        }
    }

    pub fn is_settled(&self) -> bool {
        self.graph.nodes.values().all(|state| {
            matches!(
                state,
                NodeState::Succeeded
                    | NodeState::Failed
                    | NodeState::Cancelled
                    | NodeState::Unknown
                    | NodeState::Blocked
                    | NodeState::Stale
            )
        })
    }

    pub fn scheduler_snapshot(&self) -> SchedulerSnapshot {
        self.scheduler.snapshot()
    }

    /// Produces the complete user-facing DAG state without dropping unknown records.
    pub fn task_tree(&self, now_ms: u64) -> TaskTreeSnapshot {
        let concurrency_cap = self.scheduler.global_concurrency();
        let nodes = self
            .specs
            .values()
            .map(|spec| {
                let state = self
                    .graph
                    .nodes
                    .get(&spec.task_id)
                    .copied()
                    .unwrap_or(NodeState::Unknown);
                let blockers = open_blockers(spec, &self.graph);
                let (queue_reason, ready_condition) = match state {
                    NodeState::Queued if !blockers.is_empty() => (
                        Some(format!("waiting for {}", blockers.join(", "))),
                        Some(format!("dependencies succeed: {}", blockers.join(", "))),
                    ),
                    NodeState::Ready => {
                        let scheduled = scheduled(spec, 0, 0);
                        match self.scheduler.reason(&scheduled, now_ms) {
                            Some(reason) => (
                                Some(reason.code().to_owned()),
                                Some(reason.ready_condition(&scheduled, concurrency_cap)),
                            ),
                            None => (None, Some("ready to run".to_owned())),
                        }
                    }
                    NodeState::Blocked => (
                        Some("dependency-blocked".to_owned()),
                        Some(format!(
                            "a blocker is resolved: {}",
                            if blockers.is_empty() {
                                "dependency failed or is unknown".to_owned()
                            } else {
                                blockers.join(", ")
                            }
                        )),
                    ),
                    NodeState::Stale => (
                        Some("snapshot-stale".to_owned()),
                        Some("the project snapshot is refreshed".to_owned()),
                    ),
                    _ => (None, None),
                };
                let result = self.results.get(&spec.task_id);
                TaskTreeNode {
                    task_id: spec.task_id.clone(),
                    parent_task_id: spec.parent_task_id.clone(),
                    agent_id: spec.agent_id.clone(),
                    provider_id: spec.provider_id.clone(),
                    environment_id: spec.environment_id.clone(),
                    project_dir: spec.project_dir.clone(),
                    dependencies: spec.dependencies.clone(),
                    state,
                    budgets: spec.budgets.clone(),
                    queue_reason,
                    blockers,
                    ready_condition,
                    result_verified: result.map(|result| {
                        result.validate_for(spec).is_ok() && result.acceptance_verified
                    }),
                    unknown_side_effects: state == NodeState::Unknown
                        || result.is_some_and(|result| result.unknown),
                    usage: usage_from_result(result),
                }
            })
            .collect::<Vec<_>>();
        let aggregate = aggregate_usage(nodes.iter().map(|node| &node.usage));
        let backends = backend_rollups(&nodes);
        TaskTreeSnapshot {
            generated_at_ms: now_ms,
            concurrency_cap,
            nodes,
            aggregate,
            backends,
        }
    }
}

fn scheduled(spec: &ChildTaskSpec, priority: u16, sequence: u64) -> ScheduledTask {
    ScheduledTask {
        id: spec.task_id.clone(),
        provider: spec.provider_id.clone(),
        workspace: spec.workspace_id.clone(),
        environment: spec.environment_id.clone(),
        priority,
        sequence,
    }
}

fn descendants_including(graph: &TaskGraph, root: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::from([root.to_owned()]);
    let mut stack = vec![root.to_owned()];
    while let Some(parent) = stack.pop() {
        for child in graph
            .relations
            .iter()
            .filter(|relation| relation.prerequisite == parent)
            .map(|relation| relation.dependent.clone())
        {
            if found.insert(child.clone()) {
                stack.push(child);
            }
        }
    }
    found
}
impl Scheduler {
    pub fn new(limits: SlotLimits) -> Self {
        Self {
            limits,
            ready: VecDeque::new(),
            running: BTreeMap::new(),
            provider_backoff_until: BTreeMap::new(),
        }
    }
    pub fn enqueue(&mut self, task: ScheduledTask) {
        self.ready.push_back(task);
        let slice = self.ready.make_contiguous();
        slice.sort_by_key(|task| (std::cmp::Reverse(task.priority), task.sequence));
    }
    pub fn next_ready(&mut self, now_ms: u64) -> Option<ScheduledTask> {
        let index = self
            .ready
            .iter()
            .position(|task| self.reason(task, now_ms).is_none())?;
        let task = self.ready.remove(index)?;
        self.running.insert(task.id.clone(), task.clone());
        Some(task)
    }
    pub fn reason(&self, task: &ScheduledTask, now_ms: u64) -> Option<QueueReason> {
        if self
            .provider_backoff_until
            .get(&task.provider)
            .is_some_and(|until| *until > now_ms)
        {
            return Some(QueueReason::ProviderBackoff);
        }
        if self.running.len() >= self.limits.global {
            return Some(QueueReason::GlobalSlot);
        }
        if self
            .running
            .values()
            .filter(|running| running.provider == task.provider)
            .count()
            >= self.limits.per_provider
        {
            return Some(QueueReason::ProviderSlot);
        }
        if self
            .running
            .values()
            .filter(|running| running.workspace == task.workspace)
            .count()
            >= self.limits.per_workspace
        {
            return Some(QueueReason::WorkspaceSlot);
        }
        if self
            .running
            .values()
            .filter(|running| running.environment == task.environment)
            .count()
            >= self.limits.per_environment
        {
            return Some(QueueReason::EnvironmentSlot);
        }
        None
    }
    pub fn finish(&mut self, id: &str) {
        self.running.remove(id);
    }

    pub fn global_concurrency(&self) -> usize {
        self.limits.global
    }

    pub fn set_global_concurrency(&mut self, cap: usize) {
        self.limits.global = cap.clamp(1, 64);
    }

    pub fn remove(&mut self, id: &str) {
        self.ready.retain(|task| task.id != id);
        self.running.remove(id);
    }

    pub fn set_provider_backoff(&mut self, provider: impl Into<String>, until_ms: u64) {
        self.provider_backoff_until
            .insert(provider.into(), until_ms);
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        SchedulerSnapshot {
            limits: self.limits.clone(),
            ready: self.ready.iter().cloned().collect(),
            running: self.running.values().cloned().collect(),
            provider_backoff_until: self.provider_backoff_until.clone(),
        }
    }

    pub fn restore(snapshot: SchedulerSnapshot) -> Self {
        Self {
            limits: snapshot.limits,
            ready: snapshot.ready.into(),
            running: snapshot
                .running
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
            provider_backoff_until: snapshot.provider_backoff_until,
        }
    }

    pub fn restore_after_restart(snapshot: SchedulerSnapshot) -> (Self, Vec<String>) {
        let interrupted = snapshot
            .running
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        (
            Self {
                limits: snapshot.limits,
                ready: snapshot.ready.into(),
                running: BTreeMap::new(),
                provider_backoff_until: snapshot.provider_backoff_until,
            },
            interrupted,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerSnapshot {
    pub limits: SlotLimits,
    pub ready: Vec<ScheduledTask>,
    pub running: Vec<ScheduledTask>,
    pub provider_backoff_until: BTreeMap<String, u64>,
}

impl SchedulerSnapshot {
    pub fn persist(&self, path: &Path) -> Result<(), OrchestrationError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|error| OrchestrationError::Persistence(error.to_string()))?;
        termior_store::atomic_write(path, &json)
            .map_err(|error| OrchestrationError::Persistence(error.to_string()))
    }

    pub fn load(path: &Path) -> Result<Self, OrchestrationError> {
        let json = std::fs::read_to_string(path)
            .map_err(|error| OrchestrationError::Persistence(error.to_string()))?;
        serde_json::from_str(&json)
            .map_err(|error| OrchestrationError::Persistence(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationRequest {
    pub task_id: String,
    pub environment_path: PathBuf,
    pub base_revision: String,
    pub target_branch: String,
    pub validation_passed: bool,
}

#[derive(Debug, Default)]
pub struct IntegrationQueue {
    waiting: VecDeque<IntegrationRequest>,
    active: Option<IntegrationRequest>,
}

impl IntegrationQueue {
    pub fn enqueue(&mut self, request: IntegrationRequest) {
        self.waiting.push_back(request);
    }

    pub fn start_next(&mut self) -> Option<&IntegrationRequest> {
        if self.active.is_none() {
            self.active = self.waiting.pop_front();
        }
        self.active.as_ref()
    }

    pub fn finish_active(
        &mut self,
        accepted: bool,
    ) -> Result<IntegrationRequest, OrchestrationError> {
        let request = self
            .active
            .take()
            .ok_or(OrchestrationError::NoActiveIntegration)?;
        if accepted && !request.validation_passed {
            self.active = Some(request);
            return Err(OrchestrationError::UnverifiedWrite);
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateBudget {
    pub token_limit: Option<u64>,
    pub time_limit_ms: u64,
    pub output_limit_bytes: u64,
    pub reserved_tokens: u64,
    pub reserved_time_ms: u64,
    pub reserved_output_bytes: u64,
}

impl AggregateBudget {
    pub fn reserve(
        &mut self,
        tokens: Option<u64>,
        time_ms: u64,
        output_bytes: u64,
    ) -> Result<(), OrchestrationError> {
        let requested_tokens = tokens.unwrap_or(0);
        if self
            .token_limit
            .is_some_and(|limit| self.reserved_tokens + requested_tokens > limit)
            || self.reserved_time_ms + time_ms > self.time_limit_ms
            || self.reserved_output_bytes + output_bytes > self.output_limit_bytes
        {
            return Err(OrchestrationError::BudgetExpansion);
        }
        self.reserved_tokens += requested_tokens;
        self.reserved_time_ms += time_ms;
        self.reserved_output_bytes += output_bytes;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildTaskResult {
    pub task_id: String,
    pub data: Value,
    pub evidence: Vec<String>,
    pub snapshot: SnapshotVersion,
    pub change_sets: Vec<String>,
    pub acceptance_verified: bool,
    pub usage_known: bool,
    #[serde(default)]
    pub usage: TaskResourceUsage,
    pub stale: bool,
    pub unknown: bool,
    pub writes: bool,
}
impl ChildTaskResult {
    pub fn validate(&self, expected_snapshot: SnapshotVersion) -> Result<(), OrchestrationError> {
        if self.evidence.is_empty() {
            return Err(OrchestrationError::MissingEvidence);
        }
        if self.snapshot != expected_snapshot || self.stale {
            return Err(OrchestrationError::StaleResult);
        }
        if self.unknown {
            return Err(OrchestrationError::UnknownResult);
        }
        if self.writes && (self.change_sets.is_empty() || !self.acceptance_verified) {
            return Err(OrchestrationError::UnverifiedWrite);
        }
        Ok(())
    }

    pub fn validate_for(&self, spec: &ChildTaskSpec) -> Result<(), OrchestrationError> {
        self.validate(spec.snapshot)?;
        validate_schema_value(&spec.output_schema, &self.data, "$")
    }
}

fn validate_schema_value(
    schema: &Value,
    data: &Value,
    path: &str,
) -> Result<(), OrchestrationError> {
    let expected_type = schema.get("type").and_then(Value::as_str);
    let type_matches = match expected_type {
        None => true,
        Some("object") => data.is_object(),
        Some("array") => data.is_array(),
        Some("string") => data.is_string(),
        Some("number") => data.is_number(),
        Some("integer") => data.as_i64().is_some() || data.as_u64().is_some(),
        Some("boolean") => data.is_boolean(),
        Some("null") => data.is_null(),
        Some(other) => {
            return Err(OrchestrationError::OutputSchemaViolation(format!(
                "{path}: unsupported schema type {other}"
            )))
        }
    };
    if !type_matches {
        return Err(OrchestrationError::OutputSchemaViolation(format!(
            "{path}: expected {}",
            expected_type.unwrap_or("value")
        )));
    }
    if let Some(object) = data.as_object() {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(OrchestrationError::OutputSchemaViolation(format!(
                        "{path}: missing required property {name}"
                    )));
                }
            }
        }
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            if let Some(name) = object.keys().find(|name| !properties.contains_key(*name)) {
                return Err(OrchestrationError::OutputSchemaViolation(format!(
                    "{path}: unexpected property {name}"
                )));
            }
        }
        for (name, property_schema) in properties {
            if let Some(value) = object.get(&name) {
                validate_schema_value(&property_schema, value, &format!("{path}.{name}"))?;
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), data.as_array()) {
        for (index, value) in array.iter().enumerate() {
            validate_schema_value(items, value, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct WriteLeaseTable {
    roots: BTreeMap<String, String>,
}
impl WriteLeaseTable {
    pub fn acquire(&mut self, root: &str, task: &str) -> Result<(), OrchestrationError> {
        if self.roots.contains_key(root) {
            return Err(OrchestrationError::WriteLeaseBusy);
        }
        self.roots.insert(root.into(), task.into());
        Ok(())
    }
    pub fn release(&mut self, root: &str, task: &str) {
        if self.roots.get(root).is_some_and(|owner| owner == task) {
            self.roots.remove(root);
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OrchestrationError {
    #[error("child depth {depth} exceeds maximum {max}")]
    DepthExceeded { depth: u8, max: u8 },
    #[error("child permissions exceed parent permissions")]
    PermissionExpansion,
    #[error("child budget exceeds parent budget")]
    BudgetExpansion,
    #[error("task dependency would create a cycle")]
    Cycle,
    #[error("task graph node is missing")]
    MissingNode,
    #[error("child result has no evidence")]
    MissingEvidence,
    #[error("child result is stale")]
    StaleResult,
    #[error("child result contains unknown effects")]
    UnknownResult,
    #[error("child output does not match its schema: {0}")]
    OutputSchemaViolation(String),
    #[error("write result lacks a change set or verified acceptance")]
    UnverifiedWrite,
    #[error("direct-root write lease is already held")]
    WriteLeaseBusy,
    #[error("orchestration persistence failed: {0}")]
    Persistence(String),
    #[error("there is no active worktree integration")]
    NoActiveIntegration,
    #[error("child task already exists: {0}")]
    DuplicateTask(String),
    #[error("plan step is not a subagent step: {0}")]
    NotSubagentStep(String),
    #[error("plan step was not approved for spawning: {0}")]
    StepNotApproved(String),
    #[error("a record with unknown side effects cannot be deleted")]
    UnknownRecordRetained,
}

fn retained_cancel_state(current: NodeState, unconfirmed: bool) -> NodeState {
    if current == NodeState::Unknown || unconfirmed {
        return NodeState::Unknown;
    }
    if matches!(
        current,
        NodeState::Succeeded | NodeState::Failed | NodeState::Cancelled
    ) {
        return current;
    }
    NodeState::Cancelled
}

fn open_blockers(spec: &ChildTaskSpec, graph: &TaskGraph) -> Vec<String> {
    spec.dependencies
        .iter()
        .filter(|dependency| graph.nodes.get(*dependency) != Some(&NodeState::Succeeded))
        .cloned()
        .collect()
}

fn usage_from_result(result: Option<&ChildTaskResult>) -> TaskResourceUsage {
    let Some(result) = result else {
        return TaskResourceUsage::default();
    };
    let mut usage = result.usage.clone();
    if !result.usage_known {
        usage.input_tokens = None;
        usage.output_tokens = None;
        usage.cost_usd = None;
    }
    if !usage.cost_usd.is_some_and(|cost| cost.is_finite()) {
        usage.cost_usd = None;
    }
    usage
}
