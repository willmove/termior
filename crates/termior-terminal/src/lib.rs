//! `termior-terminal` — PTY 会话 + alacritty 终端网格封装（FR-TERM）。
//!
//! 把 `portable-pty`（PTY spawn/reader/writer）与 `alacritty_terminal`（VTE 解析 +
//! 网格模型）桥接，并复用 `termior-terminal-core::OscParser` 做 OSC 7/133/777 旁路嗅探。
//!
//! 本 crate 不含 GPUI 渲染，保持可独立测试；渲染层在 `termior` 消费。
//! 对齐 spec §5.2 的 `Termior-terminal` crate 与 §6.2 FR-TERM。

pub mod bridge;
pub mod pty;

pub use bridge::{PtyData, TerminalBridge, TerminalEventProxy, WriterHandle};
pub use pty::{PtySession, PtySessionConfig, SpawnError};
pub use termior_terminal_core::shell_integration::ShellKind;
