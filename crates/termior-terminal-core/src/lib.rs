//! `termior-terminal-core` — 终端核心纯逻辑（FR-TERM-06 + 附录B，INV-4）。
//!
//! 包括：
//! - [`osc`]：OSC 序列解析（OSC 7 cwd / OSC 133 A·B·C·D / OSC 777 notify）。
//! - [`shell_integration`]：shell integration 注入串生成器（zsh/bash/pwsh）。
//!
//! **INV-4**：终端代理状态转换只能由显式 OSC 序列触发，绝不基于原始输出启发式推断。
//! [`osc`] 模块的状态机保证：无显式 OSC 777/133;C 序列时，不产出任何状态事件。

#![forbid(unsafe_code)]

pub mod command;
pub mod command_context;
pub mod filter;
pub mod osc;
pub mod search;
pub mod shell_integration;

pub use command::{CommandSessionId, OutputCursor};
pub use command_context::{OscCommandTracker, TerminalCommandRecord, TerminalContextReference};
pub use filter::{FilteredOutput, OscStreamFilter};
pub use search::{find_hyperlinks, SearchHit, TerminalSearch};

pub use osc::{OscEvent, OscParser, PromptMark};
pub use shell_integration::{ShellIntegrationSnippets, ShellKind};
