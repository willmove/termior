//! 实时上下文桥契约（FR-AGENT-07）。
//!
//! `get_terminal_context` 工具惰性抓取活动终端「此刻」状态：当前 cwd（OSC 7）+ 活动
//! PTY 缓冲末 ~300 行。执行时快照，不预缓存。
//!
//! 本模块定义 trait 与数据结构；真实抓取在 PTY/终端模块实现后注入。

use serde::{Deserialize, Serialize};

/// 终端上下文快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalContext {
    /// 当前工作目录（OSC 7，已规范化）。
    pub cwd: String,
    /// PTY 缓冲末尾约 300 行文本。
    pub recent_output: String,
    /// 是否为快照时刻（非缓存）。
    pub captured_at_unix_ms: u64,
    /// Most recent OSC 133 command record. Missing shell integration remains `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_reference: Option<termior_terminal::TerminalCommandRecord>,
}

/// 上下文提供者：执行时抓取活动终端状态。
pub trait TerminalContextProvider: Send + Sync {
    /// 惰性抓取：调用时才读取活动终端 cwd + 缓冲末 ~300 行。
    fn snapshot(&self) -> TerminalContext;
}

/// 默认抓取行数上限（FR-AGENT-07）。
pub const RECENT_LINES_CAP: usize = 300;

/// 截取缓冲末尾至多 N 行（纯函数，供实现复用）。
pub fn tail_lines(buf: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = buf.lines().collect();
    let start = if lines.len() > max_lines {
        lines.len() - max_lines
    } else {
        0
    };
    lines[start..].join("\n")
}

/// 测试用的固定上下文提供者。
pub struct StaticContextProvider {
    pub ctx: TerminalContext,
}

impl TerminalContextProvider for StaticContextProvider {
    fn snapshot(&self) -> TerminalContext {
        self.ctx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_caps_to_max_lines() {
        let buf = (0..1000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&buf, 300);
        assert_eq!(tail.lines().count(), 300);
        assert!(tail.contains("line 999"));
    }

    #[test]
    fn tail_shorter_buf_unchanged() {
        let buf = "a\nb\nc";
        assert_eq!(tail_lines(buf, 300), "a\nb\nc");
    }

    #[test]
    fn recent_lines_cap_is_300() {
        assert_eq!(RECENT_LINES_CAP, 300);
    }

    #[test]
    fn static_provider_returns_snapshot() {
        let p = StaticContextProvider {
            ctx: TerminalContext {
                cwd: "/proj".into(),
                recent_output: "$ ".into(),
                captured_at_unix_ms: 123,
                command_reference: None,
            },
        };
        let s = p.snapshot();
        assert_eq!(s.cwd, "/proj");
    }
}
