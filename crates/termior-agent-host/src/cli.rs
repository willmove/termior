//! 普通 CLI/TUI Agent：只在 PTY 中运行，不冒充结构化后端（FR-AEXT，Stage B D7）。
//!
//! Termior 对这类 Agent 只提供可兑现的终端增强：PTY、附件、diff 审阅，以及 Agent 自己通过
//! hook/OSC 777 显式上报的通知。它们没有 Task/Turn/Event 协议，因此 [`CapabilityProfile`]
//! 的所有结构化能力都报告为 unsupported，UI 不得从屏幕文本推断工具调用或完成状态。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{CapabilityProfile, CapabilitySupport};

/// Agent 选择器中的三类后端（Stage B 6.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentBackendKind {
    /// Termior 内置运行时（in-process adapter）。
    BuiltIn,
    /// 经结构化协议（Codex app-server、ACP）连接、通过能力握手的外部 Agent。
    StructuredExternal,
    /// 只在 PTY 中运行的普通 CLI/TUI Agent。
    PlainCli,
}

/// 一个可在终端里启动的普通 CLI Agent。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliAgentProfile {
    pub id: &'static str,
    pub display_name: &'static str,
    /// 在 PATH 中查找并在新终端里执行的程序名。
    pub program: &'static str,
}

/// 内置识别的普通 CLI Agent。只列出程序名，不内置安装或认证逻辑。
pub const CLI_AGENTS: &[CliAgentProfile] = &[
    CliAgentProfile {
        id: "claude-code",
        display_name: "Claude Code",
        program: "claude",
    },
    CliAgentProfile {
        id: "codex-cli",
        display_name: "Codex CLI",
        program: "codex",
    },
    CliAgentProfile {
        id: "gemini-cli",
        display_name: "Gemini CLI",
        program: "gemini",
    },
    CliAgentProfile {
        id: "opencode",
        display_name: "OpenCode",
        program: "opencode",
    },
];

/// 普通 CLI Agent 在 Termior 中实际获得的终端增强。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliAgentFeatures {
    pub pty: bool,
    pub attachments: bool,
    pub diff_review: bool,
    /// 仅当 Agent 通过已安装的 hook 发出 OSC 777 事件时才有通知。
    pub hook_notifications: bool,
}

impl CliAgentProfile {
    pub fn kind(&self) -> AgentBackendKind {
        AgentBackendKind::PlainCli
    }

    /// 结构化能力全部 unsupported：没有协议握手就没有可验证的能力。
    pub fn capabilities(&self) -> CapabilityProfile {
        let unsupported = CapabilitySupport::Unsupported;
        CapabilityProfile {
            session_resume: unsupported,
            session_fork: unsupported,
            mid_turn_steer: unsupported,
            cancel: unsupported,
            approval_requests: unsupported,
            host_terminal: unsupported,
            diff_events: unsupported,
            model_selection: unsupported,
            sandbox_reporting: unsupported,
        }
    }

    pub fn features(&self) -> CliAgentFeatures {
        CliAgentFeatures {
            pty: true,
            attachments: true,
            diff_review: true,
            hook_notifications: true,
        }
    }

    /// 在当前进程的 PATH 中查找程序。
    pub fn locate(&self) -> Option<PathBuf> {
        find_program(
            self.program,
            std::env::var_os("PATH"),
            std::env::var_os("PATHEXT"),
        )
    }

    /// 在新终端里启动该 Agent 时写入 shell 的命令行（含回车）。
    pub fn launch_input(&self) -> String {
        format!("{}\r", self.program)
    }
}

/// 在 `path` 列出的目录里查找可执行文件；Windows 下按 `pathext` 补扩展名。
pub fn find_program(
    program: &str,
    path: Option<OsString>,
    pathext: Option<OsString>,
) -> Option<PathBuf> {
    let path = path?;
    let extensions: Vec<String> = if cfg!(windows) {
        pathext
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    std::env::split_paths(&path).find_map(|dir| {
        std::iter::once(dir.join(program))
            .chain(
                extensions
                    .iter()
                    .map(|ext| dir.join(format!("{program}{ext}"))),
            )
            .find(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_agents_never_claim_structured_capabilities() {
        for agent in CLI_AGENTS {
            let capabilities = agent.capabilities();
            assert_eq!(agent.kind(), AgentBackendKind::PlainCli);
            assert!(!capabilities.can_resume());
            assert!(!capabilities.can_fork());
            assert!(!capabilities.can_cancel());
            assert!(!capabilities.approval_requests.is_supported());
            assert!(!capabilities.diff_events.is_supported());
            assert!(agent.features().pty);
            assert!(agent.launch_input().ends_with('\r'));
        }
    }

    #[test]
    fn find_program_searches_path_entries_in_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "fake-agent.exe"
        } else {
            "fake-agent"
        };
        let target = second.path().join(name);
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        let found = find_program("fake-agent", Some(path.clone()), Some(".EXE;.CMD".into()))
            .expect("program found in the second PATH entry");
        // Windows 文件系统大小写不敏感，返回的扩展名大小写取自 PATHEXT。
        assert_eq!(found.parent(), Some(second.path()));
        assert!(found
            .file_name()
            .is_some_and(|file| file.eq_ignore_ascii_case(name)));
        assert_eq!(
            find_program("missing-agent", Some(path), Some(".EXE".into())),
            None
        );
        assert_eq!(find_program("fake-agent", None, None), None);
    }
}
