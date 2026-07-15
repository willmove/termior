//! 工具两级门控模型（FR-SEC-01 / FR-AGENT-09）。
//!
//! 工具分两级：
//! - [`ToolLevel::Auto`]：只读工具，自动执行（`read_file`、`list_directory`、`fs_search`、`fs_grep`）。
//! - [`ToolLevel::Approval`]：审批门控（`write_file`、`create_directory`、`rename`、`delete`、
//!   `run_command`、`shell_session_run`、`shell_bg_spawn`）。

/// 工具标识符（对齐 FR-AGENT-09 的工具名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolId {
    // 只读 / 自动
    ReadFile,
    ListDirectory,
    FsSearch,
    FsGrep,
    // 写 / 审批
    WriteFile,
    CreateDirectory,
    Rename,
    Delete,
    RunCommand,
    ShellSessionRun,
    ShellBgSpawn,
    /// 实时上下文桥（FR-AGENT-07）——只读快照，自动执行。
    GetTerminalContext,
}

impl ToolId {
    pub fn name(&self) -> &'static str {
        match self {
            ToolId::ReadFile => "read_file",
            ToolId::ListDirectory => "list_directory",
            ToolId::FsSearch => "fs_search",
            ToolId::FsGrep => "fs_grep",
            ToolId::WriteFile => "write_file",
            ToolId::CreateDirectory => "create_directory",
            ToolId::Rename => "rename",
            ToolId::Delete => "delete",
            ToolId::RunCommand => "run_command",
            ToolId::ShellSessionRun => "shell_session_run",
            ToolId::ShellBgSpawn => "shell_bg_spawn",
            ToolId::GetTerminalContext => "get_terminal_context",
        }
    }

    /// FR-SEC-01 / FR-AGENT-09：每个工具的固定门控级别。
    pub fn level(&self) -> ToolLevel {
        match self {
            ToolId::ReadFile
            | ToolId::ListDirectory
            | ToolId::FsSearch
            | ToolId::FsGrep
            | ToolId::GetTerminalContext => ToolLevel::Auto,
            ToolId::WriteFile
            | ToolId::CreateDirectory
            | ToolId::Rename
            | ToolId::Delete
            | ToolId::RunCommand
            | ToolId::ShellSessionRun
            | ToolId::ShellBgSpawn => ToolLevel::Approval,
        }
    }

    pub fn from_name(s: &str) -> Option<ToolId> {
        ALL_TOOLS.iter().find(|t| t.name() == s).copied()
    }
}

/// 工具门控级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolLevel {
    /// 自动执行（只读）。
    Auto,
    /// 审批门控（写/执行）。
    Approval,
}

/// 所有内置工具的全集（对齐 FR-AGENT-09 工具清单）。
pub const ALL_TOOLS: &[ToolId] = &[
    ToolId::ReadFile,
    ToolId::ListDirectory,
    ToolId::FsSearch,
    ToolId::FsGrep,
    ToolId::WriteFile,
    ToolId::CreateDirectory,
    ToolId::Rename,
    ToolId::Delete,
    ToolId::RunCommand,
    ToolId::ShellSessionRun,
    ToolId::ShellBgSpawn,
    ToolId::GetTerminalContext,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tools_are_auto() {
        for t in [ToolId::ReadFile, ToolId::ListDirectory, ToolId::FsSearch, ToolId::FsGrep, ToolId::GetTerminalContext] {
            assert_eq!(t.level(), ToolLevel::Auto, "{} should be Auto", t.name());
        }
    }

    #[test]
    fn write_tools_require_approval() {
        for t in [
            ToolId::WriteFile,
            ToolId::CreateDirectory,
            ToolId::Rename,
            ToolId::Delete,
            ToolId::RunCommand,
            ToolId::ShellSessionRun,
            ToolId::ShellBgSpawn,
        ] {
            assert_eq!(t.level(), ToolLevel::Approval, "{} should be Approval", t.name());
        }
    }

    #[test]
    fn from_name_roundtrip() {
        for t in [
            ToolId::ReadFile,
            ToolId::WriteFile,
            ToolId::RunCommand,
            ToolId::GetTerminalContext,
        ] {
            assert_eq!(ToolId::from_name(t.name()), Some(t));
        }
        assert_eq!(ToolId::from_name("nope"), None);
    }
}
