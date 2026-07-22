//! PTY/VTE 解析的共享 harness（bench 与 `nfr-pty` 二进制复用，避免重复）。
//!
//! 复刻应用真实消费路径（`crates/termior/src/terminal_view.rs` 的 `TermSize`
//! 与 `VteProcessor::advance`）：把「cat 大文件期间渲染能否跟上」拆成可纯函数
//! 量化的部分——VTE 解析 + 网格写入速率，排除平台 GPU/显示噪声（spec NFR-02）。

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};

/// 80x24 终端尺寸适配（与应用默认 `PtySessionConfig` 一致）。
pub struct BenchSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for BenchSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// 默认 80x24 尺寸。
pub fn default_size() -> BenchSize {
    BenchSize {
        columns: 80,
        screen_lines: 24,
    }
}

/// 构造一个 80x24 `Term`（复用：bench 与 `nfr-pty` 共享）。
pub fn new_term() -> Term<VoidListener> {
    Term::new(TermConfig::default(), &default_size(), VoidListener)
}

/// 生成模拟 `cat` 大文件输出：纯文本行 + 适度 ANSI 着色 + 光标移动，
/// 覆盖终端最常见的解析路径（SGR、CR/LF），而非刻意极端序列。
pub fn synthetic_pty_output(total_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total_bytes);
    let line = b"\x1b[32m pub fn generated_line() -> usize { 123456789 } \x1b[0m // payload\r\n";
    while out.len() + line.len() <= total_bytes {
        out.extend_from_slice(line);
    }
    out
}
