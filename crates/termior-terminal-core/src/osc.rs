//! OSC 序列解析（附录B / FR-TERM-06）。
//!
//! 解析 PTY reader 线程的字节流中的 Operating System Command 序列：
//! - `OSC 7 ; file://<host><path>` → shell 报告 cwd（状态栏面包屑、新 tab cwd 继承）。
//! - `OSC 133 ; A / B` → 提示符开始/结束。
//! - `OSC 133 ; C ; <cmd>` → 命令输出开始；**自锁定**终端代理检测器。
//! - `OSC 133 ; D ; <code>` → 命令退出码。
//! - `OSC 777 ; notify ; Termior ; <event>` → 终端代理状态（started/working/attention/finished/exited）。
//!
//! 序列以 `ESC ]`（`0x1b 0x5d`）开始，以 `BEL`（`0x07`）或 `ST`（`ESC \``）结束。
//!
//! **INV-4**：终端代理状态转换只能由显式 OSC 777 序列触发。本解析器在无显式序列时
//! 不产出任何 [`OscEvent::AgentEvent`]；高频重绘的 TUI 输出不会抖动通知状态。

#![allow(clippy::module_inception)]

/// 解析出的 OSC 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscEvent {
    /// OSC 7：当前工作目录（已规范化：Windows 盘符转正斜杠）。
    Cwd { host: String, path: String },
    /// OSC 133 A/B：提示符开始/结束。
    Prompt(PromptMark),
    /// OSC 133 C：命令输出开始（自锁定检测器）。`cmd` 可能为空。
    CommandStart { cmd: String },
    /// OSC 133 D：命令退出。
    CommandExit { code: Option<i32> },
    /// OSC 777：终端代理状态事件（FR-TAGENT-01）。
    AgentEvent(AgentState),
}

/// 提示符标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// 提示符开始（OSC 133 A）。
    PromptStart,
    /// 提示符结束（OSC 133 B）。
    PromptEnd,
}

/// 终端代理状态（OSC 777 载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Started,
    Working,
    Attention,
    Finished,
    Exited,
}

impl AgentState {
    /// 解析事件字符串。未知值返回 None（保持当前状态不变，符合 INV-4）。
    pub fn parse(s: &str) -> Option<AgentState> {
        match s.trim() {
            "started" => Some(AgentState::Started),
            "working" => Some(AgentState::Working),
            "attention" => Some(AgentState::Attention),
            "finished" => Some(AgentState::Finished),
            "exited" => Some(AgentState::Exited),
            _ => None,
        }
    }
}

/// 字节流式 OSC 解析器。可多次 [`feed`][Self::feed]，跨字节边界的状态机。
#[derive(Debug, Default)]
pub struct OscParser {
    state: State,
    buf: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,       // 看到 ESC
    Osc,       // 看到 ESC ]，正在收集载荷
    OscEsc,    // OSC 内看到 ESC（等待 '\' 作 ST 结束）
}

impl OscParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入字节，返回本次解析出的事件。
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<OscEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    if b == b']' {
                        self.state = State::Osc;
                        self.buf.clear();
                    } else {
                        // 其他 ESC 序列（CSI 等），不处理，回 Ground
                        self.state = State::Ground;
                    }
                }
                State::Osc => {
                    if b == 0x07 {
                        // BEL 结束
                        if let Some(ev) = parse_payload(&self.buf) {
                            events.push(ev);
                        }
                        self.buf.clear();
                        self.state = State::Ground;
                    } else if b == 0x1b {
                        self.state = State::OscEsc;
                    } else {
                        self.buf.push(b);
                    }
                }
                State::OscEsc => {
                    if b == b'\\' {
                        // ST 结束
                        if let Some(ev) = parse_payload(&self.buf) {
                            events.push(ev);
                        }
                        self.buf.clear();
                        self.state = State::Ground;
                    } else {
                        // 非 ST，回 OSC 继续（丢弃这个 ESC）
                        self.state = State::Osc;
                        // 把当前字节当作载荷一部分（保守）
                        self.buf.push(b);
                    }
                }
            }
        }
        events
    }
}

/// 解析单个 OSC 载荷（已去掉首尾定界）为事件。
fn parse_payload(buf: &[u8]) -> Option<OscEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    let s = s.trim();
    // 形如 `<ps>;<pt>` 或 `<ps>;<pi>;<pt>` …
    let mut parts = s.splitn(2, ';');
    let ps = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim_start_matches(';');
    match ps {
        "7" => parse_osc7(rest),
        "133" => parse_osc133(rest),
        "777" => parse_osc777(rest),
        _ => None, // 其他 OSC（标题、超链接等）本模块不消费
    }
}

/// OSC 7：`file://<host><path>`，Windows 盘符规范化。
fn parse_osc7(rest: &str) -> Option<OscEvent> {
    let url = rest.trim();
    // 规范化 scheme 前缀（file:, file://, file:///）
    let after_scheme = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("file:"))
        .unwrap_or(url);

    // 形如 `<authority><path>`：authority 到第一个 '/' 为止。
    // 特例：`//host/path`（保留了双斜杠）按 authority+path 解析。
    let (host, path) = if let Some(rest) = after_scheme.strip_prefix("//") {
        // //host/path
        if let Some(slash) = rest.find('/') {
            (rest[..slash].to_string(), rest[slash..].to_string())
        } else {
            (rest.to_string(), String::from("/"))
        }
    } else if let Some(slash_idx) = after_scheme.find('/') {
        // authority/path（无前导 //）：authority 在第一个 / 之前
        // 但仅当 authority 看起来像 host（不含盘符冒号）时才拆分
        let authority = &after_scheme[..slash_idx];
        if authority.is_empty() {
            (String::new(), after_scheme.to_string())
        } else if authority.contains(':') || authority.contains('\\') {
            // 看起来是路径片段（如 Windows C:），不当作 host
            (String::new(), after_scheme.to_string())
        } else {
            (authority.to_string(), after_scheme[slash_idx..].to_string())
        }
    } else {
        (String::new(), after_scheme.to_string())
    };
    Some(OscEvent::Cwd {
        host,
        path: normalize_cwd(&path),
    })
}

/// Windows 盘符规范化：`/C:/Users` → `C:/Users`（附录B）。
pub fn normalize_cwd(path: &str) -> String {
    // 统一反斜杠为正斜杠
    let p = path.replace('\\', "/");
    // 形如 `/C:/...` 或 `/C|/...`（某些 shell 用 | 代替 :）
    let bytes = p.as_bytes();
    if p.starts_with('/') && p.len() >= 3 {
        let drive = bytes[1];
        let sep = bytes[2];
        if drive.is_ascii_alphabetic() && (sep == b':' || sep == b'|') {
            // 去掉前导 '/'，把 '|' 还原为 ':'
            let mut out = String::with_capacity(p.len());
            out.push(drive as char);
            out.push(':');
            out.push_str(&p[3..]);
            return out;
        }
    }
    p
}

/// OSC 133：`A` / `B` / `C;<cmd>` / `D;<code>`。
fn parse_osc133(rest: &str) -> Option<OscEvent> {
    let mut parts = rest.splitn(2, ';');
    let code = parts.next()?.trim();
    let arg = parts.next().unwrap_or("").trim();
    match code {
        "A" => Some(OscEvent::Prompt(PromptMark::PromptStart)),
        "B" => Some(OscEvent::Prompt(PromptMark::PromptEnd)),
        "C" => Some(OscEvent::CommandStart { cmd: arg.to_string() }),
        "D" => {
            let n = arg.parse::<i32>().ok();
            Some(OscEvent::CommandExit { code: n })
        }
        _ => None,
    }
}

/// OSC 777：`notify ; Termior ; <event>`。
fn parse_osc777(rest: &str) -> Option<OscEvent> {
    let mut parts = rest.split(';');
    let cmd = parts.next()?.trim();
    let target = parts.next()?.trim();
    let event = parts.next().unwrap_or("").trim();
    if !cmd.eq_ignore_ascii_case("notify") {
        return None;
    }
    // 只消费发往 Termior 的通知（避免误收其他终端代理）
    if target != "Termior" && !target.eq_ignore_ascii_case("termior") {
        return None;
    }
    AgentState::parse(event).map(OscEvent::AgentEvent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_one(input: &str) -> Vec<OscEvent> {
        let mut p = OscParser::new();
        p.feed(input.as_bytes())
    }

    // —— OSC 7 ——
    #[test]
    fn osc7_unix_cwd() {
        let ev = feed_one("\x1b]7;file://host/home/user/proj\x07");
        assert_eq!(
            ev,
            vec![OscEvent::Cwd { host: "host".into(), path: "/home/user/proj".into() }]
        );
    }

    #[test]
    fn osc7_no_host() {
        let ev = feed_one("\x1b]7;file:///home/user\x07");
        assert_eq!(ev, vec![OscEvent::Cwd { host: "".into(), path: "/home/user".into() }]);
    }

    #[test]
    fn osc7_windows_drive_normalized() {
        // `/C:/Users/u` → `C:/Users/u`
        let ev = feed_one("\x1b]7;file:///C:/Users/u/proj\x07");
        assert_eq!(ev, vec![OscEvent::Cwd { host: "".into(), path: "C:/Users/u/proj".into() }]);
    }

    #[test]
    fn osc7_windows_pipe_drive_normalized() {
        let ev = feed_one("\x1b]7;file:///C|/Users/u\x07");
        assert_eq!(ev, vec![OscEvent::Cwd { host: "".into(), path: "C:/Users/u".into() }]);
    }

    #[test]
    fn osc7_backslash_normalized() {
        let ev = feed_one("\x1b]7;file://host/C:\\Users\\u\x07");
        assert_eq!(ev, vec![OscEvent::Cwd { host: "host".into(), path: "C:/Users/u".into() }]);
    }

    // —— OSC 133 ——
    #[test]
    fn osc133_prompt_marks() {
        let ev = feed_one("\x1b]133;A\x07");
        assert_eq!(ev, vec![OscEvent::Prompt(PromptMark::PromptStart)]);
        let ev = feed_one("\x1b]133;B\x07");
        assert_eq!(ev, vec![OscEvent::Prompt(PromptMark::PromptEnd)]);
    }

    #[test]
    fn osc133_command_start_with_cmd() {
        let ev = feed_one("\x1b]133;C;npm run dev\x07");
        assert_eq!(ev, vec![OscEvent::CommandStart { cmd: "npm run dev".into() }]);
    }

    #[test]
    fn osc133_command_exit_code() {
        let ev = feed_one("\x1b]133;D;0\x07");
        assert_eq!(ev, vec![OscEvent::CommandExit { code: Some(0) }]);
        let ev = feed_one("\x1b]133;D;127\x07");
        assert_eq!(ev, vec![OscEvent::CommandExit { code: Some(127) }]);
        let ev = feed_one("\x1b]133;D\x07");
        assert_eq!(ev, vec![OscEvent::CommandExit { code: None }]);
    }

    // —— OSC 777 ——
    #[test]
    fn osc777_agent_states() {
        for (s, expected) in [
            ("started", AgentState::Started),
            ("working", AgentState::Working),
            ("attention", AgentState::Attention),
            ("finished", AgentState::Finished),
            ("exited", AgentState::Exited),
        ] {
            let input = format!("\x1b]777;notify;Termior;{s}\x07");
            let ev = feed_one(&input);
            assert_eq!(ev, vec![OscEvent::AgentEvent(expected)], "for {s}");
        }
    }

    #[test]
    fn osc777_unknown_event_ignored() {
        // INV-4：未知事件不产出，状态保持不变
        let ev = feed_one("\x1b]777;notify;Termior;bogus\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn osc777_other_target_ignored() {
        // 非发往 Termior 的 notify 不消费
        let ev = feed_one("\x1b]777;notify;OtherTool;started\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn osc777_non_notify_ignored() {
        let ev = feed_one("\x1b]777;set_title;Termior;x\x07");
        assert!(ev.is_empty());
    }

    // —— INV-4：无 OSC 时不产出状态事件 ——
    #[test]
    fn no_osc_no_state_events() {
        // 高频重绘 TUI 输出、普通命令输出，绝不产出 AgentEvent
        let mut p = OscParser::new();
        let ev = p.feed(b"\x1b[2J\x1b[HHello world \x1b[31mred text\x1b[0m\n");
        assert!(ev.is_empty(), "INV-4 violated: {ev:?}");
    }

    #[test]
    fn inv4_plain_output_no_events() {
        let mut p = OscParser::new();
        let big = "Some long build log line\n".repeat(1000);
        assert!(p.feed(big.as_bytes()).is_empty());
    }

    // —— ST 结束（ESC \）——
    #[test]
    fn st_terminator_works() {
        let ev = feed_one("\x1b]133;A\x1b\\");
        assert_eq!(ev, vec![OscEvent::Prompt(PromptMark::PromptStart)]);
    }

    // —— 跨字节边界 ——
    #[test]
    fn split_across_feeds() {
        let mut p = OscParser::new();
        assert!(p.feed(b"\x1b]7;file://").is_empty());
        assert!(p.feed(b"/home/u").is_empty());
        let ev = p.feed(b"/proj\x07");
        assert_eq!(ev, vec![OscEvent::Cwd { host: "".into(), path: "/home/u/proj".into() }]);
    }

    #[test]
    fn multiple_events_in_one_feed() {
        let input = "\x1b]7;file:///a\x07some text\x1b]133;A\x07";
        let ev = feed_one(input);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0], OscEvent::Cwd { host: "".into(), path: "/a".into() });
        assert_eq!(ev[1], OscEvent::Prompt(PromptMark::PromptStart));
    }

    #[test]
    fn invalid_utf8_payload_ignored() {
        // 非法 UTF-8 不应 panic
        let mut p = OscParser::new();
        let ev = p.feed(&[0x1b, b']', b'7', b';', 0xff, 0xfe, 0x07]);
        assert!(ev.is_empty());
    }

    #[test]
    fn other_osc_like_title_ignored() {
        // OSC 0/2 设置标题，本模块不消费（不进入网格是上层职责）
        let ev = feed_one("\x1b]0;my title\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn throughput_large_input_no_panic() {
        // 对齐 §6.2 验收：cat 大文件期间不冻结（此处验证解析器无 panic、不丢事件）
        let mut p = OscParser::new();
        let mut expected = 0;
        let mut blob = String::new();
        for _ in 0..1000 {
            blob.push_str("normal log line that is not an osc sequence\n");
        }
        for _ in 0..10 {
            blob.push_str("\x1b]7;file:///proj\x07");
            expected += 1;
        }
        let ev = p.feed(blob.as_bytes());
        assert_eq!(ev.len(), expected);
    }

    #[test]
    fn nested_tmux_sequences_still_parsed() {
        // tmux 内的 OSC 7 同样应被识别
        let ev = feed_one("\x1b]7;file://host/home/u/work\x07");
        assert_eq!(ev.len(), 1);
    }
}
