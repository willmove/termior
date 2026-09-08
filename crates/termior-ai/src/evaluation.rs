//! Offline, deterministic reliability evaluation reports (FR-AORCH-07).

use crate::{DecisionSource, TaskRuntime, TaskState, ToolState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const EVALUATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    pub completion_result: TaskState,
    pub verified: bool,
    pub tool_call_count: u64,
    pub duplicate_side_effect_count: u64,
    pub human_decision_count: u64,
    pub safety_refusal_count: u64,
    pub model_steps: u64,
    pub tool_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioReport {
    pub name: String,
    pub task_id: String,
    pub metrics: EvaluationMetrics,
}

impl ScenarioReport {
    pub fn capture(name: impl Into<String>, runtime: &TaskRuntime) -> Self {
        let duplicate_side_effect_count = runtime
            .invocations()
            .iter()
            .filter(|invocation| {
                invocation.attempts.len() > 1
                    && ![
                        "read_file",
                        "list_directory",
                        "fs_search",
                        "fs_grep",
                        "get_terminal_context",
                    ]
                    .contains(&invocation.tool_name.as_str())
            })
            .count() as u64;
        let human_decision_count = runtime
            .invocations()
            .iter()
            .filter(|invocation| {
                invocation
                    .decision
                    .as_ref()
                    .is_some_and(|decision| decision.source == DecisionSource::User)
            })
            .count() as u64;
        let safety_refusal_count = runtime
            .invocations()
            .iter()
            .filter(|invocation| {
                invocation.state == ToolState::Denied
                    || invocation.result.as_ref().is_some_and(|result| {
                        !result.ok
                            && [
                                "denied",
                                "not authorized",
                                "not enabled",
                                "invalid tool arguments",
                            ]
                            .iter()
                            .any(|needle| result.output.contains(needle))
                    })
            })
            .count() as u64;
        Self {
            name: name.into(),
            task_id: runtime.task().id.to_string(),
            metrics: EvaluationMetrics {
                completion_result: runtime.task().state,
                verified: runtime.task().state == TaskState::CompletedVerified,
                tool_call_count: runtime.invocations().len() as u64,
                duplicate_side_effect_count,
                human_decision_count,
                safety_refusal_count,
                model_steps: runtime.task().usage.model_steps,
                tool_output_bytes: runtime.task().usage.tool_output_bytes,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationSuiteReport {
    pub schema_version: u32,
    pub suite: String,
    pub scenarios: Vec<ScenarioReport>,
}

impl EvaluationSuiteReport {
    pub fn new(suite: impl Into<String>) -> Self {
        Self {
            schema_version: EVALUATION_SCHEMA_VERSION,
            suite: suite.into(),
            scenarios: Vec::new(),
        }
    }

    pub fn push(&mut self, report: ScenarioReport) {
        self.scenarios.push(report);
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn persist(&self, directory: &Path) -> Result<PathBuf, std::io::Error> {
        let path = directory.join("m6-agent-evaluation.json");
        let json = self
            .to_json()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        termior_store::atomic_write(&path, &json)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(path)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationMetrics {
    pub task_count: u64,
    pub completed_count: u64,
    pub verified_count: u64,
    pub total_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub wall_clock_ms: u64,
    pub conflict_count: u64,
    pub stale_result_count: u64,
    pub duplicate_side_effect_count: u64,
    pub human_intervention_count: u64,
    pub unknown_recovery_count: u64,
}

impl OrchestrationMetrics {
    pub fn completion_rate(&self) -> f64 {
        if self.task_count == 0 {
            0.0
        } else {
            self.completed_count as f64 / self.task_count as f64
        }
    }

    /// Returns true only when the candidate preserves completion and verification while reducing
    /// wall time without increasing conflicts or duplicate effects.
    pub fn improves_on(&self, baseline: &Self) -> bool {
        self.completion_rate() >= baseline.completion_rate()
            && self.verified_count >= baseline.verified_count
            && self.wall_clock_ms < baseline.wall_clock_ms
            && self.conflict_count <= baseline.conflict_count
            && self.duplicate_side_effect_count <= baseline.duplicate_side_effect_count
    }
}
