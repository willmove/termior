//! Persisted lifecycle-hook catalog and redacted display rows.

use crate::{HookConfig, HookRunRecord};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LIFECYCLE_HOOKS_FILE: &str = "Termior-lifecycle-hooks.json";
pub const HOOK_RUNS_FILE: &str = "Termior-hook-runs.json";
/// How many recent runs the management UI shows.
pub const RECENT_RUN_LIMIT: usize = 20;

pub fn lifecycle_hooks_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LIFECYCLE_HOOKS_FILE)
}

pub fn hook_runs_path(data_dir: &Path) -> PathBuf {
    data_dir.join(HOOK_RUNS_FILE)
}

/// Configured lifecycle hooks. Enablement lives here so a disabled hook stays disabled
/// across restarts and is skipped by every pipeline that reads the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HookCatalog {
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

impl HookCatalog {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        serde_json::from_str(&raw).map_err(|error| error.to_string())
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let raw = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        termior_store::assert_no_persistent_secret(&raw).map_err(|error| error.to_string())?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
        }
        termior_store::atomic_write(path, &raw).map_err(|error| error.to_string())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        let Some(hook) = self.hooks.iter_mut().find(|hook| hook.id == id) else {
            return false;
        };
        hook.enabled = enabled;
        true
    }
}

pub fn load_run_log(path: &Path) -> Result<Vec<HookRunRecord>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

/// One configured hook, safe to render. Free-text fields are passed through
/// [`termior_store::StreamingRedactor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookListEntry {
    pub id: String,
    pub source: String,
    pub point: String,
    pub command: String,
    pub timeout_ms: u64,
    pub enabled: bool,
}

/// One recent run, safe to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRunEntry {
    pub hook_id: String,
    pub point: String,
    pub duration_ms: u64,
    pub result: String,
    pub modified_arguments: bool,
}

pub fn hook_list_entries(hooks: &[HookConfig], secrets: &[String]) -> Vec<HookListEntry> {
    hooks
        .iter()
        .map(|hook| HookListEntry {
            id: redact(&hook.id, secrets),
            source: redact(&hook.source, secrets),
            point: hook.point.as_str().to_owned(),
            command: redact(&hook.command_line(), secrets),
            timeout_ms: u64::try_from(hook.effective_timeout().as_millis()).unwrap_or(u64::MAX),
            enabled: hook.enabled,
        })
        .collect()
}

pub fn recent_run_entries(
    runs: &[HookRunRecord],
    limit: usize,
    secrets: &[String],
) -> Vec<HookRunEntry> {
    let start = runs.len().saturating_sub(limit);
    runs[start..]
        .iter()
        .rev()
        .map(|run| {
            let result = if run.diagnostic.is_empty() {
                run.decision.clone()
            } else {
                format!("{}: {}", run.decision, run.diagnostic)
            };
            HookRunEntry {
                hook_id: redact(&run.hook_id, secrets),
                point: run.point.as_str().to_owned(),
                duration_ms: run.duration_ms,
                result: redact(&result, secrets),
                modified_arguments: run.modified_arguments,
            }
        })
        .collect()
}

fn redact(value: &str, secrets: &[String]) -> String {
    termior_store::StreamingRedactor::new(secrets.iter().cloned())
        .redact(value)
        .text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HookPoint;

    fn sample(enabled: bool) -> HookConfig {
        HookConfig {
            id: "lint".into(),
            point: HookPoint::PreTool,
            program: "echo".into(),
            args: vec!["sk-testsecretvalue123456".into()],
            enabled,
            fail_closed: true,
            source: "project".into(),
            timeout_ms: 1500,
        }
    }

    #[test]
    fn list_and_run_rows_redact_sensitive_fields() {
        let hooks = hook_list_entries(&[sample(true)], &[]);
        assert_eq!(hooks[0].source, "project");
        assert_eq!(hooks[0].point, "pre-tool");
        assert_eq!(hooks[0].timeout_ms, 1500);
        assert!(
            hooks[0].command.contains("[REDACTED]"),
            "{}",
            hooks[0].command
        );
        assert!(!hooks[0].command.contains("sk-testsecretvalue123456"));

        let runs = recent_run_entries(
            &[HookRunRecord {
                hook_id: "lint".into(),
                task_id: "task".into(),
                point: HookPoint::PreTool,
                decision: "replace".into(),
                diagnostic: "path sk-testsecretvalue123456".into(),
                duration_ms: 42,
                modified_arguments: true,
            }],
            RECENT_RUN_LIMIT,
            &[],
        );
        assert_eq!(runs[0].duration_ms, 42);
        assert!(runs[0].modified_arguments);
        assert!(runs[0].result.contains("[REDACTED]"), "{}", runs[0].result);
        assert!(!runs[0].result.contains("sk-testsecretvalue123456"));
        assert!(runs[0].result.contains("pre-tool") || runs[0].point == "pre-tool");
    }

    #[test]
    fn catalog_roundtrip_keeps_enabled_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = lifecycle_hooks_path(dir.path());
        let mut hook = sample(true);
        hook.args = vec!["lint".into()];
        let mut catalog = HookCatalog { hooks: vec![hook] };
        assert!(catalog.set_enabled("lint", false));
        catalog.save(&path).unwrap();
        let loaded = HookCatalog::load(&path).unwrap();
        assert!(!loaded.hooks[0].enabled);
        assert_eq!(loaded.hooks[0].source, "project");
        assert_eq!(loaded.hooks[0].timeout_ms, 1500);
    }

    #[test]
    fn missing_catalog_fields_use_defaults() {
        let raw = r#"{"hooks":[{"id":"a","point":"pre-tool","program":"echo","enabled":false,"fail_closed":true}]}"#;
        let catalog: HookCatalog = serde_json::from_str(raw).unwrap();
        assert_eq!(catalog.hooks[0].source, "user");
        assert_eq!(catalog.hooks[0].timeout_ms, crate::DEFAULT_HOOK_TIMEOUT_MS);
        assert!(!catalog.hooks[0].enabled);
    }
}
