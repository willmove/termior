//! `termior-hooks` — Claude Code hooks 安装器（FR-TAGENT-03/04，NFR-10 点名纯函数）。
//!
//! 向 `~/.claude/settings.json` 写入三条 hook：
//! - `UserPromptSubmit` → working
//! - `Notification` → attention
//! - `Stop` → finished
//!
//! 每条 hook 是输出含 `terminalSequence`（OSC 777）JSON 的单行命令，以 `Termior_TERMINAL`
//! 环境变量守卫使其在其他终端中为空操作。
//!
//! 安全性（FR-TAGENT-04）：
//! - 现存 `settings.json` 非法 JSON 时**拒绝写入绝不覆盖**。
//! - 临时文件 + rename 原子写。
//! - **幂等**：重跑不重复。
//! - 卸载只删除自有标记（命令串含 `notify;Termior;`）的 hooks，清理遗留空 hook 组。
//! - 提供安装状态查询。
//!
//! 本 crate 提供纯函数式核心（[`merge_install`] / [`merge_uninstall`] / [`is_installed`]），
//! 文件 IO 由上层用 termior-store 的原子写完成。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod runner;

pub use runner::{
    FailurePolicy, HookConfig, HookDecision, HookError, HookEvent, HookPipeline,
    HookPipelineResult, HookPoint, HookRun, HookRunRecord, HookRunner, HOOK_SCHEMA_VERSION,
};

/// 我们写入的 hook 事件名（Claude Code 的 hook key）。
pub const HOOK_EVENTS: &[&str] = &["UserPromptSubmit", "Notification", "Stop"];

/// Termior 自有标记：命令串含此子串即视为我们写入的 hook。
pub const OWNERSHIP_MARKER: &str = "notify;Termior;";

/// 环境变量守卫名（其他终端中为空操作）。
pub const ENV_GUARD: &str = "TERMior_TERMINAL";

/// Agent 状态事件（与 OSC 777 载荷对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEvent {
    Working,
    Attention,
    Finished,
}

impl AgentEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentEvent::Working => "working",
            AgentEvent::Attention => "attention",
            AgentEvent::Finished => "finished",
        }
    }
}

/// hook 事件 → Termior 事件映射。
pub fn event_for(hook_event: &str) -> Option<AgentEvent> {
    match hook_event {
        "UserPromptSubmit" => Some(AgentEvent::Working),
        "Notification" => Some(AgentEvent::Attention),
        "Stop" => Some(AgentEvent::Finished),
        _ => None,
    }
}

/// 生成单条 hook 命令串（输出 OSC 777 序列）。
///
/// 命令以 `TERMior_TERMINAL` 环境变量守卫：未设置时为空操作（其他终端中不输出）。
pub fn build_command(event: AgentEvent) -> String {
    // 守卫：仅当 TERMior_TERMINAL 非空时输出 OSC 777 序列。
    // printf 的转义序列对应 ESC ] 7 7 7 ; notify ; Termior ; <event> BEL
    format!(
        "if [ -n \"${env}\" ]; then printf '\\033]777;notify;Termior;{evt}\\a'; fi",
        env = ENV_GUARD,
        evt = event.as_str()
    )
}

/// 安装结果（纯函数视图）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallResult {
    /// 写入后的完整 settings.json 文本。
    pub json: String,
    /// 本次新写入的事件数。
    pub added: usize,
    /// 已存在（幂等）跳过的事件数。
    pub already_present: usize,
}

/// 卸载结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UninstallResult {
    pub json: String,
    pub removed: usize,
}

/// 安装状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallStatus {
    pub installed_events: Vec<String>,
    pub missing_events: Vec<String>,
    pub fully_installed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum HooksError {
    #[error("existing settings.json is not valid json; refusing to overwrite: {0}")]
    InvalidExistingJson(String),
    #[error("settings root is not a json object")]
    NotAnObject,
    #[error("json serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("could not resolve the user home directory")]
    HomeUnavailable,
    #[error("hook settings I/O failed: {0}")]
    Io(String),
}

/// Resolve Claude Code's settings through the platform directory API (never by reading HOME).
pub fn claude_settings_path() -> Result<std::path::PathBuf, HooksError> {
    dirs::home_dir()
        .map(|home| home.join(".claude").join("settings.json"))
        .ok_or(HooksError::HomeUnavailable)
}

pub fn install() -> Result<InstallResult, HooksError> {
    install_at(&claude_settings_path()?)
}

pub fn uninstall() -> Result<UninstallResult, HooksError> {
    uninstall_at(&claude_settings_path()?)
}

pub fn status() -> Result<InstallStatus, HooksError> {
    status_at(&claude_settings_path()?)
}

pub fn install_at(path: &std::path::Path) -> Result<InstallResult, HooksError> {
    let existing = read_existing(path)?;
    let result = merge_install(&existing)?;
    termior_store::atomic_write(path, &result.json)
        .map_err(|error| HooksError::Io(error.to_string()))?;
    Ok(result)
}

pub fn uninstall_at(path: &std::path::Path) -> Result<UninstallResult, HooksError> {
    let existing = read_existing(path)?;
    let result = merge_uninstall(&existing)?;
    termior_store::atomic_write(path, &result.json)
        .map_err(|error| HooksError::Io(error.to_string()))?;
    Ok(result)
}

pub fn status_at(path: &std::path::Path) -> Result<InstallStatus, HooksError> {
    is_installed(&read_existing(path)?)
}

fn read_existing(path: &std::path::Path) -> Result<String, HooksError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(HooksError::Io(error.to_string())),
    }
}

/// 把三条 Termior hook 合并进现有 settings 文本。
///
/// - 现有非法 JSON → 返回 [`HooksError::InvalidExistingJson`]，绝不覆盖。
/// - 幂等：已存在的事件跳过。
pub fn merge_install(existing: &str) -> Result<InstallResult, HooksError> {
    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing)
            .map_err(|e| HooksError::InvalidExistingJson(e.to_string()))?
    };
    if !root.is_object() {
        return Err(HooksError::NotAnObject);
    }

    let hooks = root.get_mut("hooks").and_then(|h| h.as_object_mut());
    // 确保 hooks 是对象
    if hooks.is_none() {
        root["hooks"] = Value::Object(Map::new());
    }
    let hooks = root.get_mut("hooks").unwrap().as_object_mut().unwrap();

    let mut added = 0;
    let mut already = 0;
    for ev in HOOK_EVENTS {
        let termior_event = event_for(ev).unwrap();
        let cmd = build_command(termior_event);
        // Claude Code hooks 形如 { "hooks": { "Stop": [ { "hooks": [ { "type":"command","command":"..." } ] } ] } }
        let group_list = hooks
            .entry(ev.to_string())
            .or_insert_with(|| Value::Array(vec![]));
        if !group_list.is_array() {
            *group_list = Value::Array(vec![]);
        }
        let arr = group_list.as_array_mut().unwrap();

        // 检查是否已存在我们的命令（幂等）
        let already_present = arr.iter().any(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|inner| {
                    inner.iter().any(|c| {
                        c.get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.contains(OWNERSHIP_MARKER))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });

        if already_present {
            already += 1;
            continue;
        }

        arr.push(serde_json::json!({
            "hooks": [
                { "type": "command", "command": cmd }
            ]
        }));
        added += 1;
    }

    let json = serde_json::to_string_pretty(&root)?;
    Ok(InstallResult {
        json,
        added,
        already_present: already,
    })
}

/// 卸载：只删除自有标记的 hooks，清理空 hook 组。
pub fn merge_uninstall(existing: &str) -> Result<UninstallResult, HooksError> {
    let mut root: Value = if existing.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(existing)
            .map_err(|e| HooksError::InvalidExistingJson(e.to_string()))?
    };
    if !root.is_object() {
        return Err(HooksError::NotAnObject);
    }

    let mut removed = 0;
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        let event_keys: Vec<String> = hooks.keys().cloned().collect();
        for ev in event_keys {
            if let Some(Value::Array(arr)) = hooks.get_mut(&ev) {
                // 从每个 group 的 inner hooks 数组中移除自有命令
                for group in arr.iter_mut() {
                    if let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                        let before = inner.len();
                        inner.retain(|c| {
                            c.get("command")
                                .and_then(|c| c.as_str())
                                .map(|s| !s.contains(OWNERSHIP_MARKER))
                                .unwrap_or(true) // 保留非 command 或非自有的项
                        });
                        removed += before - inner.len();
                    }
                }
                // 清理空的 group（inner hooks 为空）
                arr.retain(|g| {
                    g.get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(true)
                });
                // 清理空的事件组
                if arr.is_empty() {
                    hooks.remove(&ev);
                }
            }
        }
        // 清理空的 hooks 对象
        if hooks.is_empty() {
            if let Some(obj) = root.as_object_mut() {
                obj.remove("hooks");
            }
        }
    }

    let json = serde_json::to_string_pretty(&root)?;
    Ok(UninstallResult { json, removed })
}

/// 查询安装状态。
pub fn is_installed(existing: &str) -> Result<InstallStatus, HooksError> {
    let root: Value = if existing.trim().is_empty() {
        return Ok(InstallStatus {
            installed_events: vec![],
            missing_events: HOOK_EVENTS.iter().map(|s| s.to_string()).collect(),
            fully_installed: false,
        });
    } else {
        serde_json::from_str(existing)
            .map_err(|e| HooksError::InvalidExistingJson(e.to_string()))?
    };

    let mut installed = Vec::new();
    let mut missing = Vec::new();
    for ev in HOOK_EVENTS {
        let present = root
            .get("hooks")
            .and_then(|h| h.get(ev))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|g| {
                    g.get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|inner| {
                            inner.iter().any(|c| {
                                c.get("command")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.contains(OWNERSHIP_MARKER))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if present {
            installed.push(ev.to_string());
        } else {
            missing.push(ev.to_string());
        }
    }
    let fully = missing.is_empty();
    Ok(InstallStatus {
        installed_events: installed,
        missing_events: missing,
        fully_installed: fully,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_contains_marker_and_guard() {
        let cmd = build_command(AgentEvent::Working);
        assert!(cmd.contains(OWNERSHIP_MARKER), "{cmd}");
        assert!(cmd.contains(ENV_GUARD), "{cmd}");
        assert!(cmd.contains("printf"), "{cmd}");
    }

    #[test]
    fn install_into_empty_settings() {
        let res = merge_install("").unwrap();
        assert_eq!(res.added, 3);
        assert_eq!(res.already_present, 0);
        assert!(res.json.contains("UserPromptSubmit"));
        assert!(res.json.contains("Notification"));
        assert!(res.json.contains("Stop"));
        assert!(res.json.contains(OWNERSHIP_MARKER));
    }

    #[test]
    fn install_preserves_existing_settings() {
        let existing = r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#;
        let res = merge_install(existing).unwrap();
        let root: Value = serde_json::from_str(&res.json).unwrap();
        // 用户原有 theme 保留
        assert_eq!(root["theme"], "dark");
        // Stop 组里既有用户原有命令，也有 Termior 命令
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        let commands: Vec<&str> = stop
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .filter_map(|c| c["command"].as_str())
            .collect();
        assert!(commands.iter().any(|c| c.contains("echo bye")));
        assert!(commands.iter().any(|c| c.contains(OWNERSHIP_MARKER)));
    }

    #[test]
    fn install_is_idempotent() {
        let first = merge_install("").unwrap();
        let second = merge_install(&first.json).unwrap();
        assert_eq!(second.added, 0);
        assert_eq!(second.already_present, 3);
        // JSON 文本长度不变（无重复）
        assert_eq!(first.json.len(), second.json.len());
    }

    // —— FR-TAGENT-04：非法 JSON 拒写，绝不覆盖 ——
    #[test]
    fn install_refuses_invalid_existing_json() {
        let existing = "{not valid json";
        let err = merge_install(existing).unwrap_err();
        assert!(matches!(err, HooksError::InvalidExistingJson(_)));
    }

    #[test]
    fn uninstall_refuses_invalid_existing_json() {
        let err = merge_uninstall("{bad").unwrap_err();
        assert!(matches!(err, HooksError::InvalidExistingJson(_)));
    }

    #[test]
    fn uninstall_only_removes_owned_hooks() {
        // 混合：用户自己的 hook + Termior 的 hook（三条 Termior hook）
        let installed = merge_install(
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo user"}]}]}}"#,
        )
        .unwrap();
        let uninstalled = merge_uninstall(&installed.json).unwrap();
        // 三条 Termior hook 被移除（UserPromptSubmit/Notification 各 1，Stop 中 1）
        assert_eq!(uninstalled.removed, 3);
        let root: Value = serde_json::from_str(&uninstalled.json).unwrap();
        // Stop 仍保留用户的 echo user（非自有）
        let stop_cmds: Vec<&str> = root["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            .filter_map(|c| c["command"].as_str())
            .collect();
        assert!(stop_cmds.iter().any(|c| c.contains("echo user")));
        assert!(stop_cmds.iter().all(|c| !c.contains(OWNERSHIP_MARKER)));
    }

    #[test]
    fn uninstall_cleans_empty_groups() {
        // 只有 Termior 的 hooks，卸载后整个 hooks 对象应清空
        let installed = merge_install("").unwrap();
        let uninstalled = merge_uninstall(&installed.json).unwrap();
        assert_eq!(uninstalled.removed, 3);
        let root: Value = serde_json::from_str(&uninstalled.json).unwrap();
        // hooks 键应被移除（空对象清理）
        assert!(
            root.get("hooks").is_none()
                || root["hooks"]
                    .as_object()
                    .map(|o| o.is_empty())
                    .unwrap_or(true)
        );
    }

    #[test]
    fn status_reports_install_state() {
        let empty_status = is_installed("").unwrap();
        assert!(!empty_status.fully_installed);
        assert_eq!(empty_status.missing_events.len(), 3);

        let installed = merge_install("").unwrap();
        let status = is_installed(&installed.json).unwrap();
        assert!(status.fully_installed);
        assert_eq!(status.installed_events.len(), 3);
        assert!(status.missing_events.is_empty());
    }

    #[test]
    fn status_after_partial_install() {
        // 手动只装 Stop
        let json = serde_json::json!({
            "hooks": {
                "Stop": [{ "hooks": [{ "type":"command", "command": build_command(AgentEvent::Finished) }] }]
            }
        })
        .to_string();
        let status = is_installed(&json).unwrap();
        assert!(!status.fully_installed);
        assert!(status.installed_events.contains(&"Stop".to_string()));
        assert!(status
            .missing_events
            .contains(&"UserPromptSubmit".to_string()));
    }

    #[test]
    fn reinstall_after_uninstall_works() {
        let installed = merge_install("").unwrap();
        let uninstalled = merge_uninstall(&installed.json).unwrap();
        let reinstalled = merge_install(&uninstalled.json).unwrap();
        assert_eq!(reinstalled.added, 3);
    }

    #[test]
    fn command_uses_osc777_and_events() {
        assert!(build_command(AgentEvent::Working).contains("777;notify;Termior;working"));
        assert!(build_command(AgentEvent::Attention).contains("777;notify;Termior;attention"));
        assert!(build_command(AgentEvent::Finished).contains("777;notify;Termior;finished"));
    }
}
