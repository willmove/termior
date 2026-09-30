//! Structured references from terminal output to Agent context (FR-ATERM-05).

use crate::command::{CommandSessionId, OutputCursor};
use crate::OscEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCommandRecord {
    pub id: String,
    pub terminal_id: String,
    pub command_session_id: Option<CommandSessionId>,
    pub cwd: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_start: OutputCursor,
    pub output_end: Option<OutputCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalContextReference {
    pub terminal_id: String,
    pub command_session_id: Option<CommandSessionId>,
    pub cwd: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub output_start: OutputCursor,
    pub output_end: OutputCursor,
    pub selected_text: Option<String>,
}

impl TerminalContextReference {
    pub fn selection(
        terminal_id: impl Into<String>,
        cwd: impl Into<String>,
        output_start: OutputCursor,
        output_end: OutputCursor,
        text: impl Into<String>,
    ) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            command_session_id: None,
            cwd: cwd.into(),
            command: None,
            exit_code: None,
            output_start,
            output_end,
            selected_text: Some(text.into()),
        }
    }
}

/// 保留的命令记录与输出捕获上限：记录超出后淘汰最旧的，每条输出只留尾部。
const MAX_RECORDS: usize = 256;
const MAX_CAPTURE_BYTES: usize = 32 * 1024;

pub struct OscCommandTracker {
    terminal_id: String,
    cwd: String,
    next_id: u64,
    active: Option<TerminalCommandRecord>,
    records: Vec<TerminalCommandRecord>,
    /// 命令 ID → 去除控制序列后的输出尾部（FR-ATERM-05 的“精确输出片段”）。
    outputs: std::collections::HashMap<String, Vec<u8>>,
    stripper: ControlStripper,
}

impl OscCommandTracker {
    pub fn new(terminal_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            cwd: cwd.into(),
            next_id: 1,
            active: None,
            records: Vec::new(),
            outputs: std::collections::HashMap::new(),
            stripper: ControlStripper::default(),
        }
    }

    /// 喂入 VTE 可见字节；只有处于命令执行期（OSC 133 C..D 之间）的输出会被捕获。
    /// 调用方须按 [`crate::FilteredOutput::event_offsets`] 与 [`Self::observe`] 交错调用。
    pub fn observe_output(&mut self, bytes: &[u8]) {
        let Some(active) = self.active.as_ref() else {
            // 仍需推进剥离器状态，避免提示符里半截转义序列污染下一条命令；
            // 不产出文本，终端热路径上不做分配。
            self.stripper.feed(bytes, None);
            return;
        };
        let capture = self.outputs.entry(active.id.clone()).or_default();
        self.stripper.feed(bytes, Some(capture));
        if capture.len() > MAX_CAPTURE_BYTES {
            let excess = capture.len() - MAX_CAPTURE_BYTES;
            capture.drain(..excess);
        }
    }

    /// 某条命令捕获到的输出（已去除 ANSI/OSC 控制序列，仅保留尾部）。
    pub fn output_of(&self, id: &str) -> Option<String> {
        let bytes = self.outputs.get(id)?;
        // 尾部截断可能切在 UTF-8 字符中间：跳过开头的续字节。
        let start = bytes
            .iter()
            .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
            .unwrap_or(bytes.len());
        Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
    }

    /// 为终端选区找出其所属命令：从最新往旧，在输出捕获中查找选中文本
    /// （空白归一后比较）。找不到时返回 `None`，由调用方把命令字段保留为 unknown。
    pub fn find_by_output(&self, selection: &str) -> Option<&TerminalCommandRecord> {
        let needle = normalize_whitespace(selection);
        if needle.is_empty() {
            return None;
        }
        self.active
            .iter()
            .chain(self.records.iter().rev())
            .find(|record| {
                self.output_of(&record.id)
                    .is_some_and(|output| normalize_whitespace(&output).contains(&needle))
            })
    }

    /// 最近一条以非零退出码结束的命令。
    pub fn last_failed(&self) -> Option<&TerminalCommandRecord> {
        self.records
            .iter()
            .rev()
            .find(|record| record.exit_code.is_some_and(|code| code != 0))
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    fn push_record(&mut self, record: TerminalCommandRecord) {
        self.records.push(record);
        if self.records.len() > MAX_RECORDS {
            let evicted = self.records.remove(0);
            self.outputs.remove(&evicted.id);
        }
    }

    pub fn observe(&mut self, event: &OscEvent, cursor: OutputCursor) {
        match event {
            OscEvent::Cwd { path, .. } => self.cwd = path.clone(),
            OscEvent::CommandStart { cmd } => {
                if let Some(mut previous) = self.active.take() {
                    previous.output_end = Some(cursor);
                    self.push_record(previous);
                }
                let id = format!("{}-command-{}", self.terminal_id, self.next_id);
                self.next_id += 1;
                self.active = Some(TerminalCommandRecord {
                    id,
                    terminal_id: self.terminal_id.clone(),
                    command_session_id: None,
                    cwd: self.cwd.clone(),
                    command: (!cmd.trim().is_empty()).then(|| cmd.clone()),
                    exit_code: None,
                    output_start: cursor,
                    output_end: None,
                });
            }
            OscEvent::CommandExit { code } => {
                if let Some(mut active) = self.active.take() {
                    active.exit_code = *code;
                    active.output_end = Some(cursor);
                    self.push_record(active);
                }
            }
            OscEvent::Prompt(_) | OscEvent::AgentEvent(_) => {}
        }
    }

    pub fn records(&self) -> &[TerminalCommandRecord] {
        &self.records
    }

    pub fn active(&self) -> Option<&TerminalCommandRecord> {
        self.active.as_ref()
    }
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 流式去除终端控制序列，得到用户在屏幕上看到的文本近似：
/// CSI/OSC/DCS 等转义序列整体丢弃，`\r\n` 视为换行，单独的 `\r` 视为回到行首
/// （进度条只保留最后一次重绘），其余 C0 控制字符除 `\n`/`\t` 外丢弃。
#[derive(Debug, Default)]
struct ControlStripper {
    state: StripState,
    pending_cr: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum StripState {
    #[default]
    Ground,
    Escape,
    Csi,
    /// OSC / DCS / APC 等字符串序列，以 BEL 或 ST 结束。
    String,
    StringEscape,
}

impl ControlStripper {
    fn feed(&mut self, bytes: &[u8], mut out: Option<&mut Vec<u8>>) {
        for &byte in bytes {
            match self.state {
                StripState::Ground => match byte {
                    0x1b => self.state = StripState::Escape,
                    b'\r' => self.pending_cr = true,
                    b'\n' => {
                        self.pending_cr = false;
                        if let Some(out) = out.as_deref_mut() {
                            out.push(b'\n');
                        }
                    }
                    0x00..=0x08 | 0x0a..=0x1f | 0x7f => {}
                    _ => {
                        if let Some(out) = out.as_deref_mut() {
                            self.push_text(byte, out);
                        } else {
                            self.pending_cr = false;
                        }
                    }
                },
                StripState::Escape => {
                    self.state = match byte {
                        b'[' => StripState::Csi,
                        b']' | b'P' | b'_' | b'^' | b'X' => StripState::String,
                        _ => StripState::Ground,
                    }
                }
                StripState::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.state = StripState::Ground;
                    }
                }
                StripState::String => match byte {
                    0x07 => self.state = StripState::Ground,
                    0x1b => self.state = StripState::StringEscape,
                    _ => {}
                },
                StripState::StringEscape => {
                    self.state = if byte == b'\\' {
                        StripState::Ground
                    } else {
                        StripState::String
                    };
                }
            }
        }
    }

    fn push_text(&mut self, byte: u8, out: &mut Vec<u8>) {
        if self.pending_cr {
            // 单独的回车：丢弃当前行已写内容，模拟覆盖重绘。
            let line_start = out
                .iter()
                .rposition(|existing| *existing == b'\n')
                .map_or(0, |index| index + 1);
            out.truncate(line_start);
            self.pending_cr = false;
        }
        out.push(byte);
    }
}
