//! Bounded, non-interactive lifecycle hook runner (Stage C / M8).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;
use wait_timeout::ChildExt;

pub const HOOK_SCHEMA_VERSION: u32 = 1;
/// Default per-hook timeout. A stored `timeout_ms` of 0 means this default.
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 5_000;

fn default_timeout_ms() -> u64 {
    DEFAULT_HOOK_TIMEOUT_MS
}

fn default_enabled() -> bool {
    true
}

fn default_source() -> String {
    "user".into()
}

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

impl HookPoint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreTask => "pre-task",
            Self::PostTask => "post-task",
            Self::PreTool => "pre-tool",
            Self::PostTool => "post-tool",
            Self::PostChangeApply => "post-change-apply",
            Self::PreVerification => "pre-verification",
            Self::PostVerification => "post-verification",
        }
    }
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
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_enabled")]
    pub fail_closed: bool,
    /// Where the hook was declared (`user`, a project path, or another origin).
    #[serde(default = "default_source")]
    pub source: String,
    /// Per-hook timeout in milliseconds. `0` uses [`DEFAULT_HOOK_TIMEOUT_MS`].
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl HookConfig {
    pub fn command_line(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }

    pub fn effective_timeout(&self) -> Duration {
        let ms = if self.timeout_ms == 0 {
            DEFAULT_HOOK_TIMEOUT_MS
        } else {
            self.timeout_ms
        };
        Duration::from_millis(ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRunRecord {
    pub hook_id: String,
    pub task_id: String,
    pub point: HookPoint,
    pub decision: String,
    pub diagnostic: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub modified_arguments: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookPipelineResult {
    pub arguments: Value,
    pub replaced: bool,
    pub records: Vec<HookRunRecord>,
}

pub struct HookPipeline {
    hooks: Mutex<Vec<HookConfig>>,
    recent: Mutex<Vec<HookRunRecord>>,
    known_secrets: Vec<String>,
    catalog_path: Option<PathBuf>,
    runs_path: Option<PathBuf>,
}

impl HookPipeline {
    pub fn new(hooks: Vec<HookConfig>) -> Self {
        Self {
            hooks: Mutex::new(hooks),
            recent: Mutex::new(vec![]),
            known_secrets: Vec::new(),
            catalog_path: None,
            runs_path: None,
        }
    }

    /// Reload enablement from this catalog before each run, and persist toggles here.
    pub fn with_catalog_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.catalog_path = Some(path.into());
        self
    }

    /// Append redacted run records to this file after each invocation.
    pub fn with_runs_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.runs_path = Some(path.into());
        self
    }

    pub fn configs(&self) -> Vec<HookConfig> {
        self.hooks
            .lock()
            .map(|hooks| hooks.clone())
            .unwrap_or_default()
    }

    /// Enable or disable one hook. When a catalog path is set, the change is written
    /// before the method returns so the next run — including another pipeline — skips it.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> bool {
        let Some(hooks) = self.hooks.lock().ok().and_then(|mut hooks| {
            let hook = hooks.iter_mut().find(|hook| hook.id == id)?;
            hook.enabled = enabled;
            Some(hooks.clone())
        }) else {
            return false;
        };
        let Some(path) = &self.catalog_path else {
            return true;
        };
        if (crate::catalog::HookCatalog { hooks }).save(path).is_err() {
            if let Ok(mut hooks) = self.hooks.lock() {
                if let Some(hook) = hooks.iter_mut().find(|hook| hook.id == id) {
                    hook.enabled = !enabled;
                }
            }
            return false;
        }
        true
    }

    fn sync_from_catalog(&self) {
        let Some(path) = &self.catalog_path else {
            return;
        };
        if !path.exists() {
            return;
        }
        let Ok(catalog) = crate::catalog::HookCatalog::load(path) else {
            return;
        };
        if let Ok(mut hooks) = self.hooks.lock() {
            *hooks = catalog.hooks;
        }
    }

    pub fn with_known_secrets(mut self, known_secrets: impl IntoIterator<Item = String>) -> Self {
        self.known_secrets = known_secrets.into_iter().collect();
        self
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
        self.sync_from_catalog();
        let hooks = self.configs();
        let mut arguments = event
            .arguments
            .clone()
            .unwrap_or(Value::Object(Default::default()));
        let original = arguments.clone();
        let mut records = Vec::new();
        for hook in hooks
            .into_iter()
            .filter(|hook| hook.enabled && hook.point == HookPoint::PreTool)
        {
            let started = Instant::now();
            let runner = HookRunner {
                timeout: hook.effective_timeout(),
                failure_policy: if hook.fail_closed {
                    FailurePolicy::FailClosed
                } else {
                    FailurePolicy::Warn
                },
                known_secrets: self.known_secrets.clone(),
                ..HookRunner::default()
            };
            let mut hook_event = event.clone();
            hook_event.arguments = Some(arguments.clone());
            let run = match runner.run(&hook.program, &hook.args, &hook_event) {
                Ok(run) => run,
                Err(HookError::Warning(message)) => {
                    records.push(self.record(&hook, event, "warning", &message, started, false));
                    continue;
                }
                Err(error) => {
                    records.push(self.record(
                        &hook,
                        event,
                        "failed",
                        &error.to_string(),
                        started,
                        false,
                    ));
                    self.remember(&records);
                    return Err(error);
                }
            };
            let before = arguments.clone();
            match runner.apply_pre_tool(&before, run.decision.clone(), &mut revalidate) {
                Ok(next) => {
                    let modified = next != before;
                    arguments = next;
                    let decision = match &run.decision {
                        HookDecision::Allow => "allow",
                        HookDecision::Deny { .. } => "deny",
                        HookDecision::Replace { .. } => "replace",
                    };
                    records.push(self.record(
                        &hook,
                        event,
                        decision,
                        &run.diagnostics.join("\n"),
                        started,
                        modified,
                    ));
                }
                Err(error) => {
                    let (decision, detail) = match &error {
                        HookError::Denied(reason) => ("deny", reason.clone()),
                        HookError::Revalidation(reason) => ("revalidation", reason.clone()),
                        other => ("failed", other.to_string()),
                    };
                    let diagnostic = if run.diagnostics.is_empty() {
                        detail
                    } else {
                        format!("{detail}\n{}", run.diagnostics.join("\n"))
                    };
                    records.push(self.record(&hook, event, decision, &diagnostic, started, false));
                    self.remember(&records);
                    return Err(error);
                }
            }
        }
        self.remember(&records);
        Ok(HookPipelineResult {
            replaced: arguments != original,
            arguments,
            records,
        })
    }

    fn record(
        &self,
        hook: &HookConfig,
        event: &HookEvent,
        decision: &str,
        diagnostic: &str,
        started: Instant,
        modified_arguments: bool,
    ) -> HookRunRecord {
        HookRunRecord {
            hook_id: hook.id.clone(),
            task_id: event.task_id.clone(),
            point: hook.point,
            decision: decision.to_owned(),
            diagnostic: redact_diagnostic(diagnostic, &self.known_secrets),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            modified_arguments,
        }
    }

    fn remember(&self, records: &[HookRunRecord]) {
        if records.is_empty() {
            return;
        }
        if let Ok(mut recent) = self.recent.lock() {
            recent.extend(records.iter().cloned());
            if recent.len() > 100 {
                let remove = recent.len() - 100;
                recent.drain(..remove);
            }
        }
        if let Some(path) = &self.runs_path {
            let _ = self.persist_audit(path);
        }
    }

    /// Persist hook audit records. The serialized log is redacted again at the disk boundary.
    pub fn persist_audit(&self, path: &std::path::Path) -> Result<(), HookError> {
        let json = serde_json::to_string_pretty(&self.recent())
            .map_err(|error| HookError::FailedClosed(error.to_string()))?;
        termior_store::StreamingRedactor::new(self.known_secrets.clone())
            .write_atomic(path, &json)
            .map_err(|error| HookError::FailedClosed(error.to_string()))?;
        Ok(())
    }
}

fn redact_diagnostic(value: &str, known_secrets: &[String]) -> String {
    termior_store::StreamingRedactor::new(known_secrets.iter().cloned())
        .redact(value)
        .text
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
    pub known_secrets: Vec<String>,
}
impl Default for HookRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_output_bytes: 64 * 1024,
            failure_policy: FailurePolicy::FailClosed,
            known_secrets: Vec::new(),
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
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command
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
        let diagnostic = termior_store::StreamingRedactor::new(self.known_secrets.clone())
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
