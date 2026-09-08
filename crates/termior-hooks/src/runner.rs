//! Bounded, non-interactive lifecycle hook runner (Stage C / M8).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use thiserror::Error;
use wait_timeout::ChildExt;

pub const HOOK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookPoint {
    PreTask,
    PostTask,
    PreTool,
    PostTool,
    PostChangeApply,
    PreVerification,
    PostVerification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub schema_version: u32,
    pub point: HookPoint,
    pub task_id: String,
    pub tool: Option<String>,
    pub arguments: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum HookDecision {
    Allow,
    Deny { reason: String },
    Replace { arguments: Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePolicy {
    FailClosed,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookConfig {
    pub id: String,
    pub point: HookPoint,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub enabled: bool,
    pub fail_closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRunRecord {
    pub hook_id: String,
    pub task_id: String,
    pub point: HookPoint,
    pub decision: String,
    pub diagnostic: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookPipelineResult {
    pub arguments: Value,
    pub replaced: bool,
    pub records: Vec<HookRunRecord>,
}

pub struct HookPipeline {
    hooks: Vec<HookConfig>,
    recent: Mutex<Vec<HookRunRecord>>,
}

impl HookPipeline {
    pub fn new(hooks: Vec<HookConfig>) -> Self {
        Self {
            hooks,
            recent: Mutex::new(vec![]),
        }
    }

    pub fn recent(&self) -> Vec<HookRunRecord> {
        self.recent
            .lock()
            .map(|runs| runs.clone())
            .unwrap_or_default()
    }

    pub fn apply_pre_tool<F>(
        &self,
        event: &HookEvent,
        mut revalidate: F,
    ) -> Result<HookPipelineResult, HookError>
    where
        F: FnMut(&Value) -> Result<(), String>,
    {
        let mut arguments = event
            .arguments
            .clone()
            .unwrap_or(Value::Object(Default::default()));
        let original = arguments.clone();
        let mut records = Vec::new();
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.enabled && hook.point == HookPoint::PreTool)
        {
            let runner = HookRunner {
                failure_policy: if hook.fail_closed {
                    FailurePolicy::FailClosed
                } else {
                    FailurePolicy::Warn
                },
                ..HookRunner::default()
            };
            let mut hook_event = event.clone();
            hook_event.arguments = Some(arguments.clone());
            let run = match runner.run(&hook.program, &hook.args, &hook_event) {
                Ok(run) => run,
                Err(HookError::Warning(message)) => {
                    records.push(HookRunRecord {
                        hook_id: hook.id.clone(),
                        task_id: event.task_id.clone(),
                        point: event.point,
                        decision: "warning".into(),
                        diagnostic: redact_diagnostic(&message),
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            arguments = runner.apply_pre_tool(&arguments, run.decision.clone(), &mut revalidate)?;
            records.push(HookRunRecord {
                hook_id: hook.id.clone(),
                task_id: event.task_id.clone(),
                point: event.point,
                decision: match run.decision {
                    HookDecision::Allow => "allow",
                    HookDecision::Deny { .. } => "deny",
                    HookDecision::Replace { .. } => "replace",
                }
                .into(),
                diagnostic: redact_diagnostic(&run.diagnostics.join("\n")),
            });
        }
        if let Ok(mut recent) = self.recent.lock() {
            recent.extend(records.clone());
            if recent.len() > 100 {
                let remove = recent.len() - 100;
                recent.drain(..remove);
            }
        }
        Ok(HookPipelineResult {
            replaced: arguments != original,
            arguments,
            records,
        })
    }
}

fn redact_diagnostic(value: &str) -> String {
    termior_store::StreamingRedactor::new([]).redact(value).text
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookRun {
    pub decision: HookDecision,
    pub diagnostics: Vec<String>,
    pub output_truncated: bool,
}

pub struct HookRunner {
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub failure_policy: FailurePolicy,
}
impl Default for HookRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_output_bytes: 64 * 1024,
            failure_policy: FailurePolicy::FailClosed,
        }
    }
}

impl HookRunner {
    pub fn run(
        &self,
        program: &str,
        args: &[String],
        event: &HookEvent,
    ) -> Result<HookRun, HookError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| self.failure(error.to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| self.failure("stdout unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| self.failure("stderr unavailable".into()))?;
        let output_limit = self.max_output_bytes;
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout
                .take((output_limit + 1) as u64)
                .read_to_end(&mut bytes);
            bytes
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.take(64 * 1024).read_to_end(&mut bytes);
            bytes
        });
        if let Some(mut stdin) = child.stdin.take() {
            serde_json::to_writer(&mut stdin, event)
                .map_err(|error| self.failure(error.to_string()))?;
        }
        if child
            .wait_timeout(self.timeout)
            .map_err(|error| self.failure(error.to_string()))?
            .is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(self.failure("hook timed out".into()));
        }
        let mut stdout = stdout_reader
            .join()
            .map_err(|_| self.failure("stdout reader panicked".into()))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| self.failure("stderr reader panicked".into()))?;
        let truncated = stdout.len() > self.max_output_bytes;
        stdout.truncate(self.max_output_bytes);
        if truncated {
            return Err(self.failure("hook output exceeded limit".into()));
        }
        let decision = serde_json::from_slice(&stdout)
            .map_err(|error| self.failure(format!("invalid hook decision: {error}")))?;
        let diagnostic = termior_store::StreamingRedactor::new([])
            .redact(&String::from_utf8_lossy(&stderr))
            .text;
        Ok(HookRun {
            decision,
            diagnostics: (!diagnostic.trim().is_empty())
                .then_some(diagnostic)
                .into_iter()
                .collect(),
            output_truncated: false,
        })
    }

    pub fn apply_pre_tool<F>(
        &self,
        original: &Value,
        decision: HookDecision,
        mut revalidate: F,
    ) -> Result<Value, HookError>
    where
        F: FnMut(&Value) -> Result<(), String>,
    {
        match decision {
            HookDecision::Allow => {
                revalidate(original).map_err(HookError::Revalidation)?;
                Ok(original.clone())
            }
            HookDecision::Deny { reason } => Err(HookError::Denied(reason)),
            HookDecision::Replace { arguments } => {
                revalidate(&arguments).map_err(HookError::Revalidation)?;
                Ok(arguments)
            }
        }
    }

    fn failure(&self, message: String) -> HookError {
        match self.failure_policy {
            FailurePolicy::FailClosed => HookError::FailedClosed(message),
            FailurePolicy::Warn => HookError::Warning(message),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HookError {
    #[error("hook denied the operation: {0}")]
    Denied(String),
    #[error("hook failed closed: {0}")]
    FailedClosed(String),
    #[error("hook warning: {0}")]
    Warning(String),
    #[error("modified tool arguments failed complete revalidation: {0}")]
    Revalidation(String),
}
