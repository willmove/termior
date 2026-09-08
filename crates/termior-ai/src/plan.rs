//! Plan mode, custom agents and restricted sub-agent execution (FR-PLAN).

use crate::{
    ApprovalPolicy, CancellationToken, ChatEvent, Message, Provider, RuntimeBudgets,
    RuntimeToolExecutor, TaskCommand, TaskConfig, TaskRuntime, TaskState, ToolExecution,
    ToolRegistry,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Proposed,
    Approved,
    Rejected,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanStepKind {
    Work,
    Subagent { agent_id: String, task: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub title: String,
    pub files: Vec<String>,
    pub scope: String,
    pub kind: PlanStepKind,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub steps: Vec<PlanStep>,
    pub confirmed: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    #[error("plan is not confirmed")]
    NotConfirmed,
    #[error("step not found: {0}")]
    StepNotFound(String),
    #[error("step is not approved: {0}")]
    StepNotApproved(String),
    #[error("agent definition is invalid: {0}")]
    InvalidAgent(String),
    #[error("subagent failed: {0}")]
    Subagent(String),
}

impl Plan {
    pub fn add_step(&mut self, step: PlanStep) {
        self.confirmed = false;
        self.steps.push(step);
    }

    pub fn confirm(&mut self) {
        self.confirmed = true;
        for step in &mut self.steps {
            if step.status == PlanStepStatus::Proposed {
                step.status = PlanStepStatus::Approved;
            }
        }
    }

    pub fn reject_step(&mut self, id: &str) -> Result<(), PlanError> {
        let step = self
            .steps
            .iter_mut()
            .find(|step| step.id == id)
            .ok_or_else(|| PlanError::StepNotFound(id.to_owned()))?;
        step.status = PlanStepStatus::Rejected;
        Ok(())
    }

    pub fn begin_step(&mut self, id: &str) -> Result<&PlanStep, PlanError> {
        if !self.confirmed {
            return Err(PlanError::NotConfirmed);
        }
        let step = self
            .steps
            .iter_mut()
            .find(|step| step.id == id)
            .ok_or_else(|| PlanError::StepNotFound(id.to_owned()))?;
        if step.status != PlanStepStatus::Approved {
            return Err(PlanError::StepNotApproved(id.to_owned()));
        }
        step.status = PlanStepStatus::Running;
        Ok(step)
    }

    pub fn finish_step(&mut self, id: &str, success: bool) -> Result<(), PlanError> {
        let step = self
            .steps
            .iter_mut()
            .find(|step| step.id == id)
            .ok_or_else(|| PlanError::StepNotFound(id.to_owned()))?;
        step.status = if success {
            PlanStepStatus::Completed
        } else {
            PlanStepStatus::Failed
        };
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub icon: String,
    pub color: String,
}

impl AgentDefinition {
    pub fn validate(&self, registry: &ToolRegistry) -> Result<(), PlanError> {
        if self.id.trim().is_empty()
            || self.name.trim().is_empty()
            || self.system_prompt.trim().is_empty()
        {
            return Err(PlanError::InvalidAgent(
                "id, name and prompt are required".into(),
            ));
        }
        for tool in &self.tools {
            registry
                .requires_approval(tool)
                .or_else(|error| match error {
                    crate::ToolError::NotAllowed(_) => Ok(false),
                    other => Err(other),
                })
                .map_err(|error| PlanError::InvalidAgent(error.to_string()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinitionStore {
    #[serde(default)]
    pub agents: Vec<AgentDefinition>,
}

impl AgentDefinitionStore {
    pub fn upsert(
        &mut self,
        definition: AgentDefinition,
        tools: &ToolRegistry,
    ) -> Result<(), PlanError> {
        definition.validate(tools)?;
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|agent| agent.id == definition.id)
        {
            *existing = definition;
        } else {
            self.agents.push(definition);
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.agents.len();
        self.agents.retain(|agent| agent.id != id);
        self.agents.len() != before
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubagentResult {
    pub task_id: String,
    pub agent_id: String,
    pub answer: String,
    pub messages: Vec<Message>,
    pub state: TaskState,
}

pub async fn run_subagent<P, F>(
    definition: &AgentDefinition,
    provider: P,
    tools: ToolRegistry,
    task: &str,
    exec_tool: &F,
) -> Result<SubagentResult, PlanError>
where
    P: Provider + 'static,
    F: Fn(&str, &str) -> Result<String, String> + Sync,
{
    let workspace = std::env::current_dir().unwrap_or_default();
    let spec = crate::ChildTaskSpec {
        task_id: format!("child-{}", definition.id),
        parent_task_id: "legacy-parent".into(),
        agent_id: definition.id.clone(),
        goal: task.into(),
        input_refs: Vec::new(),
        output_schema: serde_json::json!({"type":"string"}),
        tools: definition.tools.iter().cloned().collect(),
        budgets: RuntimeBudgets::default(),
        environment_id: "direct".into(),
        provider_id: "native-child".into(),
        workspace_id: workspace.display().to_string(),
        project_dir: workspace,
        dependencies: Vec::new(),
        depth: crate::TaskDepth(1),
        writes: definition.tools.iter().any(|tool| {
            tools
                .contract(tool)
                .is_ok_and(|contract| contract.side_effect != crate::SideEffectClass::Read)
        }),
        snapshot: crate::SnapshotVersion(0),
    };
    run_child_task(definition, provider, tools, &spec, exec_tool).await
}

pub async fn run_child_task<P, F>(
    definition: &AgentDefinition,
    provider: P,
    tools: ToolRegistry,
    spec: &crate::ChildTaskSpec,
    exec_tool: &F,
) -> Result<SubagentResult, PlanError>
where
    P: Provider + 'static,
    F: Fn(&str, &str) -> Result<String, String> + Sync,
{
    definition.validate(&tools)?;
    if spec.agent_id != definition.id {
        return Err(PlanError::InvalidAgent(format!(
            "child spec selects {}, definition is {}",
            spec.agent_id, definition.id
        )));
    }
    let definition_tools = definition.tools.iter().cloned().collect::<BTreeSet<_>>();
    if !spec.tools.is_subset(&definition_tools) {
        return Err(PlanError::InvalidAgent(
            "child spec expands the selected agent tool set".into(),
        ));
    }
    let restricted = tools
        .subset(spec.tools.iter().cloned())
        .map_err(|error| PlanError::InvalidAgent(error.to_string()))?;
    struct ClosureExecutor<'a, F>(&'a F);
    impl<F> RuntimeToolExecutor for ClosureExecutor<'_, F>
    where
        F: Fn(&str, &str) -> Result<String, String> + Sync,
    {
        fn execute(
            &self,
            _call_id: &str,
            tool: &str,
            arguments: &str,
            _cancellation: &CancellationToken,
        ) -> Result<ToolExecution, String> {
            (self.0)(tool, arguments).map(ToolExecution::Completed)
        }
    }
    let mut runtime = TaskRuntime::new(
        TaskConfig::new(
            spec.goal.clone(),
            &spec.project_dir,
            spec.provider_id.clone(),
            spec.environment_id.clone(),
        ),
        spec.budgets.clone(),
        ApprovalPolicy::Prompt,
    )
    .with_tools(restricted)
    .with_system_prompt(&definition.system_prompt)
    .with_task_id(spec.task_id.clone())
    .map_err(PlanError::Subagent)?;
    runtime
        .handle(
            TaskCommand::Start {
                user_input: spec.goal.clone(),
            },
            &provider,
            &ClosureExecutor(exec_tool),
            &mut |_: &ChatEvent| {},
        )
        .await
        .map_err(|error| PlanError::Subagent(error.to_string()))?;
    let answer = runtime
        .history()
        .iter()
        .rev()
        .find(|message| message.role == crate::Role::Assistant)
        .map(|message| message.content.clone())
        .unwrap_or_default();
    Ok(SubagentResult {
        task_id: runtime.task().id.0.clone(),
        agent_id: definition.id.clone(),
        answer,
        messages: runtime.history().to_vec(),
        state: runtime.task().state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatEvent, MockProvider};

    fn definition() -> AgentDefinition {
        AgentDefinition {
            id: "reviewer".into(),
            name: "Reviewer".into(),
            system_prompt: "Review only".into(),
            tools: vec!["read_file".into()],
            icon: "R".into(),
            color: "#00aaff".into(),
        }
    }

    #[test]
    fn plan_blocks_writes_and_subagents_until_confirmed() {
        let mut plan = Plan::default();
        plan.add_step(PlanStep {
            id: "1".into(),
            title: "edit".into(),
            files: vec!["src/lib.rs".into()],
            scope: "one function".into(),
            kind: PlanStepKind::Work,
            status: PlanStepStatus::Proposed,
        });
        assert_eq!(plan.begin_step("1"), Err(PlanError::NotConfirmed));
        plan.confirm();
        assert!(plan.begin_step("1").is_ok());
        plan.finish_step("1", true).unwrap();
        assert_eq!(plan.steps[0].status, PlanStepStatus::Completed);
    }

    #[test]
    fn individual_step_can_be_rejected() {
        let mut plan = Plan::default();
        plan.add_step(PlanStep {
            id: "spawn".into(),
            title: "delegate".into(),
            files: vec![],
            scope: "tests".into(),
            kind: PlanStepKind::Subagent {
                agent_id: "reviewer".into(),
                task: "review".into(),
            },
            status: PlanStepStatus::Proposed,
        });
        plan.reject_step("spawn").unwrap();
        plan.confirm();
        assert!(matches!(
            plan.begin_step("spawn"),
            Err(PlanError::StepNotApproved(_))
        ));
    }

    #[test]
    fn custom_agent_persists_and_subagent_is_restricted() {
        let tools = ToolRegistry::default();
        let mut store = AgentDefinitionStore::default();
        store.upsert(definition(), &tools).unwrap();
        let json = serde_json::to_string(&store).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentDefinitionStore>(&json).unwrap(),
            store
        );
        let provider = MockProvider::single(vec![ChatEvent::Done(Message::assistant("reviewed"))]);
        let result = futures::executor::block_on(run_subagent(
            &definition(),
            provider,
            tools,
            "review it",
            &|_, _| Ok("ok".into()),
        ))
        .unwrap();
        assert_eq!(result.answer, "reviewed");
    }
}
