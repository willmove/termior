//! 工具注册表 + 两级门控（FR-AGENT-09 / FR-SEC-01）。
//!
//! 工具分两级（对齐 [`termior_security::gating`]）：
//! - 自动执行（只读）：`read_file`、`list_directory`、`fs_search`、`fs_grep`、`get_terminal_context`
//! - 审批门控（写/执行）：`write_file`、`create_directory`、`rename`、`delete`、
//!   `run_command`、`shell_session_run`、`shell_bg_spawn`
//!
//! 本模块在工具执行前做安全门控（deny-list / workspace 授权），命中则拒绝。
//! `@path` 引用读取内容前也经此过滤（FR-AGENT-03）。

use serde::{Deserialize, Serialize};
use termior_security::deny_list::{canonicalize_logical, check_path, DenyReason, Direction};
use termior_security::workspace::WorkspaceAuthRegistry;

/// 工具执行错误。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("path denied by deny-list: {reason}")]
    DenyList { reason: String },
    #[error("workspace not authorized for path: {0}")]
    NotAuthorized(String),
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("approval required for tool: {0}")]
    ApprovalRequired(String),
}

/// 一条工具的描述（供模型与 UI 展示）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub level: ToolLevelSerde,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLevelSerde {
    Auto,
    Approval,
}

/// 工具注册表：声明全部内置工具 + 关联安全上下文（deny-list + workspace 授权）。
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    pub workspace_auth: WorkspaceAuthRegistry,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            workspace_auth: WorkspaceAuthRegistry::new(),
        }
    }
}

impl ToolRegistry {
    pub fn new(workspace_auth: WorkspaceAuthRegistry) -> Self {
        Self { workspace_auth }
    }

    /// 列出全部工具描述（FR-AGENT-09）。
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        use termior_security::gating::{ToolLevel, ALL_TOOLS};
        ALL_TOOLS
            .iter()
            .map(|t| ToolDescriptor {
                name: t.name().to_string(),
                level: match t.level() {
                    ToolLevel::Auto => ToolLevelSerde::Auto,
                    ToolLevel::Approval => ToolLevelSerde::Approval,
                },
                description: description_for(*t).to_string(),
            })
            .collect()
    }

    /// 判定工具是否需要审批（FR-SEC-01）。
    pub fn requires_approval(&self, tool_name: &str) -> Result<bool, ToolError> {
        let id = termior_security::gating::ToolId::from_name(tool_name)
            .ok_or_else(|| ToolError::Unknown(tool_name.to_string()))?;
        Ok(matches!(
            id.level(),
            termior_security::gating::ToolLevel::Approval
        ))
    }

    /// 对一条带 `path` 参数的工具调用做安全门控（deny-list + workspace 授权）。
    ///
    /// 用于 read_file / write_file / list_directory / fs_search / fs_grep / rename / delete。
    /// `direction` 表示读或写（deny-list 双向禁止）。
    pub fn check_path_access(
        &self,
        tool_name: &str,
        path: &str,
        direction: Direction,
    ) -> Result<(), ToolError> {
        // 工具必须存在
        let _ = termior_security::gating::ToolId::from_name(tool_name)
            .ok_or_else(|| ToolError::Unknown(tool_name.to_string()))?;

        // INV-3：canonicalize 之后强制
        let canon = canonicalize_logical(path);

        // deny-list
        if let Some(reason) = check_path(&canon, direction) {
            return Err(ToolError::DenyList {
                reason: reason_message(&reason),
            });
        }
        // workspace 授权（FR-SEC-04）
        if !self.workspace_auth.is_authorized(&canon) {
            return Err(ToolError::NotAuthorized(canon));
        }
        Ok(())
    }
}

fn reason_message(r: &DenyReason) -> String {
    r.to_string()
}

fn description_for(t: termior_security::gating::ToolId) -> &'static str {
    use termior_security::gating::ToolId;
    match t {
        ToolId::ReadFile => "Read a file's contents (read-only, auto-executed).",
        ToolId::ListDirectory => "List directory entries (read-only, auto-executed).",
        ToolId::FsSearch => "Fuzzy file search (read-only, auto-executed).",
        ToolId::FsGrep => "Content search with grep (read-only, auto-executed).",
        ToolId::GetTerminalContext => {
            "Snapshot the active terminal's cwd + recent output (read-only, auto-executed)."
        }
        ToolId::WriteFile => "Write a file (approval-gated; rendered as hunk diff).",
        ToolId::CreateDirectory => "Create a directory (approval-gated).",
        ToolId::Rename => "Rename/move a path (approval-gated).",
        ToolId::Delete => "Delete a path (approval-gated).",
        ToolId::RunCommand => "Run a one-shot subshell command (approval-gated).",
        ToolId::ShellSessionRun => "Run a command in the persistent agent shell (approval-gated).",
        ToolId::ShellBgSpawn => "Spawn a long-running background process (approval-gated).",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_security::workspace::WorkspaceAuthRegistry;

    fn reg_with(root: &str) -> ToolRegistry {
        ToolRegistry::new(WorkspaceAuthRegistry::with_roots([root.to_string()]))
    }

    #[test]
    fn descriptors_cover_all_tools() {
        let r = ToolRegistry::default();
        let descriptors = r.descriptors();
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        for expected in [
            "read_file",
            "write_file",
            "run_command",
            "get_terminal_context",
            "fs_grep",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn read_file_in_workspace_allowed() {
        let r = reg_with("/proj");
        r.check_path_access("read_file", "/proj/src/main.rs", Direction::Read)
            .unwrap();
    }

    #[test]
    fn denylist_blocks_dotenv_even_in_workspace() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/proj/.env", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn denylist_blocks_dotenv_on_write_too() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("write_file", "/proj/.env", Direction::Write)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn traversal_to_dotenv_blocked() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/proj/src/../../.env", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn path_outside_workspace_blocked() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/etc/passwd", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::NotAuthorized(_)));
    }

    #[test]
    fn write_tools_require_approval() {
        let r = ToolRegistry::default();
        assert!(r.requires_approval("write_file").unwrap());
        assert!(r.requires_approval("run_command").unwrap());
        assert!(!r.requires_approval("read_file").unwrap());
        assert!(!r.requires_approval("get_terminal_context").unwrap());
    }

    #[test]
    fn unknown_tool_errors() {
        let r = ToolRegistry::default();
        assert!(matches!(
            r.requires_approval("nope"),
            Err(ToolError::Unknown(_))
        ));
        assert!(matches!(
            r.check_path_access("nope", "/proj/x", Direction::Read),
            Err(ToolError::Unknown(_))
        ));
    }

    #[test]
    fn ssh_key_blocked_even_in_workspace() {
        let r = reg_with("/home/u");
        let err = r
            .check_path_access("read_file", "/home/u/.ssh/id_rsa", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }
}
