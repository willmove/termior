//! 会话中的工具调用块：命令/目标与执行状态一行常显，点击展开参数与输出。
//!
//! 数据全部来自任务历史（assistant 消息的 `tool_calls` + 对应 `Role::Tool` 结果），
//! 运行中由 [`ComposerView::apply_live_message`] 实时追加，因此无需另开窗口即可看到 Agent
//! 执行了什么命令、输出了什么。

use super::*;
use serde_json::Value;
use termior_ai::{ToolCall, ToolResult};
use termior_terminal_core::command_context::strip_control_sequences;
use termior_theme::ResolvedPalette;

/// 展开后最多显示的输出尾部（行数与字节数），超长日志只截尾，避免拖慢重排。
const OUTPUT_TAIL_LINES: usize = 200;
const OUTPUT_TAIL_BYTES: usize = 32 * 1024;
/// 折叠态摘要的最大字符数。
const SUMMARY_MAX_CHARS: usize = 160;
/// 工具块内可选文本段的 seq 起点：同一消息里 Markdown 块从 0 编号，工具段排在其后。
const SEGMENT_SEQ_BASE: usize = 1 << 20;
/// 执行命令的工具：摘要显示 `$ command`，输出首行是会话元数据。
const SHELL_TOOLS: &[&str] = &["run_command", "shell_session_run", "shell_bg_spawn"];

/// 渲染工具块所需的本帧共享状态。
pub(super) struct ToolBlockStyle<'a> {
    pub palette: &'a ResolvedPalette,
    pub texts: &'a SelectableTexts,
    pub selection: Option<TextSelection>,
    pub selection_background: Hsla,
}

impl ComposerView {
    pub(super) fn toggle_tool_block(&mut self, call_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tools.remove(call_id) {
            self.expanded_tools.insert(call_id.to_owned());
        }
        cx.notify();
    }

    /// 一次工具调用的块。`group` 为所属消息的选区分组，`ordinal` 为该消息内的调用序号；
    /// `result` 缺失表示调用尚未返回（运行中或被审批/中断挂起）。
    pub(super) fn tool_block(
        &self,
        group: usize,
        ordinal: usize,
        call: &ToolCall,
        result: Option<&ToolResult>,
        style: &ToolBlockStyle<'_>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let p = style.palette;
        let expanded = self.expanded_tools.contains(&call.id);
        let is_shell = SHELL_TOOLS.contains(&call.name.as_str());
        let output = result.map(|result| CommandOutput::parse(&result.output, is_shell));
        let (status, status_color) = match (result, &output) {
            (Some(result), Some(output)) if result.ok => (
                output.exit_code.as_deref().map_or_else(
                    || t!("composer.tool.succeeded").to_string(),
                    |code| tf!("composer.tool.exit", "code" => code).to_string(),
                ),
                if output.exit_code.as_deref().is_none_or(|code| code == "0") {
                    p.status[1]
                } else {
                    p.status[3]
                },
            ),
            (Some(_), _) => (t!("composer.tool.failed").to_string(), p.status[3]),
            (None, _) if self.busy => (t!("composer.tool.running").to_string(), p.status[0]),
            (None, _) => (t!("composer.tool.pending").to_string(), p.status[2]),
        };
        let line_count = output
            .as_ref()
            .map(|output| output.body.lines().count())
            .filter(|count| *count > 0);
        let call_id = call.id.clone();
        let header = div()
            .id(SharedString::from(format!(
                "composer-tool-{group}-{ordinal}"
            )))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .cursor_pointer()
            .hover(|row| row.bg(crate::ui::hover_wash(p)))
            .child(crate::ui::icon(
                if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                icon_size::XS,
                crate::ui::muted(p),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(crate::monospace_font::default_family())
                    .text_xs()
                    .child(SharedString::from(call_summary(call, is_shell))),
            )
            .when_some(line_count.filter(|_| !expanded), |row, count| {
                row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(crate::ui::muted(p))
                        .child(tf!("composer.tool.lines", "count" => count)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(crate::ui::color(status_color))
                    .child(SharedString::from(status)),
            )
            // 点击只切换折叠，不开始文本选取。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_tool_block(&call_id, cx);
                }),
            );
        let seq = SEGMENT_SEQ_BASE + ordinal * 2;
        let body = expanded.then(|| {
            let arguments = (!is_shell)
                .then(|| pretty_arguments(&call.arguments))
                .filter(|arguments| !arguments.is_empty());
            let (output_text, truncated_from) = output
                .as_ref()
                .map(|output| output_tail(&output.body))
                .unwrap_or_default();
            let mono_block = |key_seq: usize, text: String| {
                div()
                    .p_1()
                    .rounded_sm()
                    .bg(crate::ui::alpha(p.foreground, 0.05))
                    .font_family(crate::monospace_font::default_family())
                    .text_xs()
                    .cursor_text()
                    .children(style.texts.text(
                        SegmentKey {
                            group,
                            seq: key_seq,
                        },
                        text.into(),
                        None,
                        Vec::new(),
                        style.selection,
                        style.selection_background,
                    ))
            };
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .pb_2()
                .when_some(arguments, |body, arguments| {
                    body.child(mono_block(seq, arguments))
                })
                .when_some(truncated_from, |body, total| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(crate::ui::color(p.status[2]))
                            .child(tf!(
                                "composer.tool.output_truncated",
                                "shown" => OUTPUT_TAIL_LINES.min(total),
                                "total" => total
                            )),
                    )
                })
                .child(if output_text.is_empty() {
                    div()
                        .text_xs()
                        .text_color(crate::ui::muted(p))
                        .child(if result.is_some() {
                            t!("composer.tool.no_output")
                        } else {
                            t!("composer.tool.waiting_output")
                        })
                } else {
                    mono_block(seq + 1, output_text)
                })
        });
        div()
            .flex()
            .flex_col()
            .min_w(px(0.0))
            .rounded_md()
            .border_1()
            .border_color(crate::ui::border(p))
            .overflow_hidden()
            .child(header)
            .children(body)
            .into_any_element()
    }
}

/// 折叠态的一行摘要：命令类工具显示 `$ command`，其余显示工具名与主要参数。
fn call_summary(call: &ToolCall, is_shell: bool) -> String {
    let args = serde_json::from_str::<Value>(&call.arguments).unwrap_or(Value::Null);
    let field = |key: &str| args.get(key).and_then(Value::as_str);
    let summary = if is_shell {
        format!("$ {}", field("command").unwrap_or_default())
    } else {
        let detail = ["path", "pattern", "query", "session_id", "from", "agent"]
            .into_iter()
            .find_map(field);
        match detail {
            Some(detail) => format!("{} {detail}", call.name),
            None => call.name.clone(),
        }
    };
    // 多行命令折叠成一行显示，完整内容在展开后的参数/输出里。
    let single_line = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() > SUMMARY_MAX_CHARS {
        let mut cut = single_line
            .chars()
            .take(SUMMARY_MAX_CHARS)
            .collect::<String>();
        cut.push('…');
        cut
    } else {
        single_line
    }
}

fn pretty_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(map)) if map.is_empty() => String::new(),
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| arguments.into()),
        Err(_) => arguments.trim().to_owned(),
    }
}

/// 工具结果文本：命令类工具首行是 `session_id=… exit=N …` 元数据，拆出退出码后只留正文；
/// 正文去除终端控制序列（持久 shell 走 PTY，输出带 ANSI 颜色与光标控制）。
#[derive(Debug, PartialEq, Eq)]
struct CommandOutput {
    exit_code: Option<String>,
    body: String,
}

impl CommandOutput {
    fn parse(output: &str, is_shell: bool) -> Self {
        let (exit_code, body) = match output.split_once('\n') {
            Some((first, rest)) if is_shell && first.starts_with("session_id=") => {
                (metadata_field(first, "exit"), rest)
            }
            None if is_shell && output.starts_with("session_id=") => {
                (metadata_field(output, "exit"), "")
            }
            _ => (None, output),
        };
        Self {
            exit_code: exit_code.filter(|code| code != "unknown"),
            body: strip_control_sequences(body.as_bytes())
                .trim_end()
                .to_owned(),
        }
    }
}

fn metadata_field(line: &str, key: &str) -> Option<String> {
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_owned)
}

/// 取输出末尾至多 [`OUTPUT_TAIL_LINES`] 行、[`OUTPUT_TAIL_BYTES`] 字节；
/// 发生截断时返回原始总行数。
fn output_tail(text: &str) -> (String, Option<usize>) {
    let lines = text.lines().collect::<Vec<_>>();
    let total = lines.len();
    let start = total.saturating_sub(OUTPUT_TAIL_LINES);
    let mut tail = lines[start..].join("\n");
    let mut truncated = start > 0;
    if tail.len() > OUTPUT_TAIL_BYTES {
        let mut cut = tail.len() - OUTPUT_TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
        truncated = true;
    }
    (tail, truncated.then_some(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn shell_summary_shows_the_command_on_one_line() {
        let summary = call_summary(
            &call(
                "run_command",
                r#"{"command":"cargo test\n  -p termior-ai"}"#,
            ),
            true,
        );
        assert_eq!(summary, "$ cargo test -p termior-ai");
    }

    #[test]
    fn other_tools_show_their_main_argument() {
        assert_eq!(
            call_summary(&call("read_file", r#"{"path":"src/lib.rs"}"#), false),
            "read_file src/lib.rs"
        );
        assert_eq!(call_summary(&call("todo_read", "{}"), false), "todo_read");
    }

    #[test]
    fn shell_output_splits_metadata_and_strips_ansi() {
        let parsed = CommandOutput::parse(
            "session_id=s1 state=Exited exit=101 timed_out=false cursor=3\n\x1b[31merror\x1b[0m\r\n",
            true,
        );
        assert_eq!(parsed.exit_code.as_deref(), Some("101"));
        assert_eq!(parsed.body, "error");
        let plain = CommandOutput::parse("session_id=x is just text", false);
        assert_eq!(plain.exit_code, None);
        assert_eq!(plain.body, "session_id=x is just text");
    }

    #[test]
    fn long_output_keeps_the_tail_and_reports_the_total() {
        let text = (0..250)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let (tail, total) = output_tail(&text);
        assert_eq!(total, Some(250));
        assert!(tail.starts_with("50\n"));
        assert!(tail.ends_with("249"));
        assert_eq!(output_tail("short"), ("short".into(), None));
    }
}
