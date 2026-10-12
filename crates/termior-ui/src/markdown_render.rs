//! Markdown → GPUI 块级渲染器。
//!
//! 同一套渲染同时服务 Markdown 预览页（[`Density::PREVIEW`]）与 Agent 会话消息
//! （[`Density::CHAT`]），对应 SDD 的依赖决策：pulldown-cmark 解析 + GPUI 原生
//! 渲染，不为同一能力引入第二套管线。视图层只负责外壳（工具栏、滚动容器等）。

use crate::text_selection::{SegmentKey, SelectableTexts, TextSelection};
use crate::ui;
use gpui::{
    div, prelude::*, px, AnyElement, FontStyle, FontWeight, HighlightStyle, Hsla, SharedString,
    StrikethroughStyle, StyledText, UnderlineStyle,
};
use std::ops::Range;
use termior_preview::{MarkdownDocument, MarkdownNode};
use termior_theme::ResolvedPalette;

/// 渲染密度：预览页的正常排版与聊天面板的紧凑排版。所有数值单位为 px。
#[derive(Clone, Copy)]
pub(crate) struct Density {
    /// 正文与代码块字号。
    pub base: f32,
    /// 段落/引用/列表与相邻内容的间距。
    pub block_gap: f32,
    /// 代码块/表格与相邻内容的间距。
    pub major_gap: f32,
    /// 代码块、HTML 块与展示公式的内边距。
    pub code_padding: f32,
    /// 列表标记列宽。
    pub marker_width: f32,
    /// 列表项之间的间距。
    pub item_gap: f32,
    /// 分隔线上下外边距。
    pub rule_gap: f32,
    /// 表格单元格内边距与最小宽度。
    pub cell_px: f32,
    pub cell_py: f32,
    pub cell_min_width: f32,
    /// 引用块左侧缩进。
    pub quote_indent: f32,
    /// 标题字号与上下边距，索引 0..=3 对应 h1..h4+。
    pub headings: [(f32, f32, f32); 4],
}

impl Density {
    /// Markdown 预览页：正文 14px（text_sm 同级），宽松间距。
    pub(crate) const PREVIEW: Density = Density {
        base: 14.0,
        block_gap: 12.0,
        major_gap: 16.0,
        code_padding: 12.0,
        marker_width: 28.0,
        item_gap: 4.0,
        rule_gap: 16.0,
        cell_px: 12.0,
        cell_py: 8.0,
        cell_min_width: 120.0,
        quote_indent: 16.0,
        headings: [
            (30.0, 20.0, 12.0),
            (24.0, 18.0, 10.0),
            (20.0, 16.0, 8.0),
            (16.0, 14.0, 6.0),
        ],
    };

    /// Agent 聊天消息：正文 12px（text_xs 同级），紧凑间距。
    pub(crate) const CHAT: Density = Density {
        base: 12.0,
        block_gap: 6.0,
        major_gap: 8.0,
        code_padding: 8.0,
        marker_width: 22.0,
        item_gap: 2.0,
        rule_gap: 8.0,
        cell_px: 8.0,
        cell_py: 4.0,
        cell_min_width: 80.0,
        quote_indent: 12.0,
        headings: [
            (16.0, 8.0, 4.0),
            (14.0, 8.0, 4.0),
            (13.0, 6.0, 3.0),
            (12.0, 6.0, 3.0),
        ],
    };
}

/// 把整篇文档渲染为块级元素列表。
///
/// `id_prefix` 用于生成可滚动块（代码块/表格）的元素 id。可滚动元素的 id 在
/// 同一帧内必须互不相同，调用方要保证前缀唯一：预览页用视图实体 id，聊天用
/// 消息序号。同一文档内由 `seq` 递增保证后缀唯一且与内容顺序确定。
pub(crate) fn render_blocks(
    document: &MarkdownDocument,
    density: Density,
    id_prefix: &str,
    p: &ResolvedPalette,
) -> Vec<AnyElement> {
    Renderer::new(density, id_prefix, p, None).blocks(document)
}

/// 可选取渲染：每段文字登记到 `texts`，并按 `selection` 绘制选区背景。
pub(crate) struct Selectable<'a> {
    pub texts: &'a SelectableTexts,
    /// 本文档在选区坐标中的分组（聊天里为消息序号）。
    pub group: usize,
    pub selection: Option<TextSelection>,
    pub background: Hsla,
}

/// 与 [`render_blocks`] 相同，但文字可被鼠标选取。
pub(crate) fn render_selectable_blocks(
    document: &MarkdownDocument,
    density: Density,
    id_prefix: &str,
    p: &ResolvedPalette,
    selectable: Selectable<'_>,
) -> Vec<AnyElement> {
    Renderer::new(density, id_prefix, p, Some(selectable)).blocks(document)
}

struct Renderer<'a> {
    density: Density,
    id_prefix: &'a str,
    p: &'a ResolvedPalette,
    /// 可滚动块的 id 序号。
    seq: usize,
    selectable: Option<Selectable<'a>>,
    /// 文字段序号（选区坐标）。
    text_seq: usize,
    /// 待前置到下一段 Markdown 源码的前缀（标题的 `## `、列表项的 `- `）。
    /// 渲染文本不受影响；`text()` 消费一次，消费后立即清空防泄漏。
    pending_source_prefix: Option<String>,
}

impl<'a> Renderer<'a> {
    fn new(
        density: Density,
        id_prefix: &'a str,
        p: &'a ResolvedPalette,
        selectable: Option<Selectable<'a>>,
    ) -> Self {
        Self {
            density,
            id_prefix,
            p,
            seq: 0,
            selectable,
            text_seq: 0,
            pending_source_prefix: None,
        }
    }

    fn blocks(mut self, document: &MarkdownDocument) -> Vec<AnyElement> {
        document
            .blocks
            .iter()
            .map(|node| self.block(node))
            .collect()
    }

    fn next_id(&mut self, kind: &str) -> SharedString {
        let id = SharedString::from(format!("{}-{kind}-{}", self.id_prefix, self.seq));
        self.seq += 1;
        id
    }

    /// 一段文字：可选取时附带登记元素，否则就是普通 `StyledText`。
    /// `source` 是该段渲染文本对应的 Markdown 源码，完整选区复制时保留
    /// 格式标记。
    fn text(
        &mut self,
        text: String,
        source: Option<String>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
    ) -> Vec<AnyElement> {
        let source = match (source, self.pending_source_prefix.take()) {
            (Some(source), Some(prefix)) => Some(prefix + &source),
            (Some(source), None) => Some(source),
            (None, _) => None,
        };
        match &self.selectable {
            Some(selectable) => {
                let key = SegmentKey {
                    group: selectable.group,
                    seq: self.text_seq,
                };
                self.text_seq += 1;
                selectable
                    .texts
                    .text(
                        key,
                        text.into(),
                        source,
                        highlights,
                        selectable.selection,
                        selectable.background,
                    )
                    .into()
            }
            None => vec![StyledText::new(text)
                .with_highlights(highlights)
                .into_any_element()],
        }
    }

    fn inline(&mut self, nodes: &[MarkdownNode]) -> Vec<AnyElement> {
        let mut text = String::new();
        let mut highlights = Vec::<(Range<usize>, HighlightStyle)>::new();
        append_inline(
            nodes,
            InlineStyle::default(),
            &mut text,
            &mut highlights,
            self.p,
        );
        let source = inline_markdown(nodes);
        self.text(text, Some(source), highlights)
    }

    fn children(&mut self, children: &[MarkdownNode]) -> Vec<AnyElement> {
        children.iter().map(|child| self.block(child)).collect()
    }

    fn block(&mut self, node: &MarkdownNode) -> AnyElement {
        let density = self.density;
        let p = self.p;
        match node {
            MarkdownNode::Heading { level, children } => {
                let index = (*level as usize).saturating_sub(1).min(3);
                let (size, top, bottom) = density.headings[index];
                self.pending_source_prefix = Some(format!("{} ", "#".repeat(*level as usize)));
                let inline = self.inline(children);
                self.pending_source_prefix = None;
                div()
                    .mt(px(top))
                    .mb(px(bottom))
                    .text_size(px(size))
                    .font_weight(FontWeight::BOLD)
                    .when(*level <= 2, |heading| {
                        heading.pb_2().border_b_1().border_color(ui::border(p))
                    })
                    .children(inline)
                    .into_any_element()
            }
            MarkdownNode::Paragraph(children) => div()
                .mb(px(density.block_gap))
                .text_size(px(density.base))
                .children(self.inline(children))
                .into_any_element(),
            MarkdownNode::BlockQuote(children) => div()
                .mb(px(density.block_gap))
                .pl(px(density.quote_indent))
                .py_1()
                .border_l_2()
                .border_color(ui::color(p.accent))
                .text_color(ui::muted(p))
                .children(self.children(children))
                .into_any_element(),
            MarkdownNode::CodeBlock { language, code } => {
                let id = self.next_id("code");
                let body = self.text(code.clone(), Some(fence_code(language, code)), Vec::new());
                div()
                    .mb(px(density.major_gap))
                    .rounded_md()
                    .border_1()
                    .border_color(ui::border(p))
                    .bg(ui::color(p.elevated))
                    .when_some(language.clone(), |block, language| {
                        block.child(
                            div()
                                .px_3()
                                .py_1()
                                .border_b_1()
                                .border_color(ui::border(p))
                                .text_size(px(density.base - 2.0))
                                .text_color(ui::muted(p))
                                .child(SharedString::from(language)),
                        )
                    })
                    .child(
                        div()
                            .id(id)
                            .overflow_x_scroll()
                            .p(px(density.code_padding))
                            .font_family(crate::monospace_font::default_family())
                            .text_size(px(density.base))
                            .whitespace_nowrap()
                            .children(body),
                    )
                    .into_any_element()
            }
            MarkdownNode::List { start, items } => {
                let start = start.unwrap_or(1);
                let numbered = matches!(node, MarkdownNode::List { start: Some(_), .. });
                let items = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let marker = if node_has_task_marker(item) {
                            String::new()
                        } else if start > 0 && numbered {
                            format!("{}. ", start + index as u64)
                        } else {
                            "• ".to_owned()
                        };
                        // 源码前缀：任务项的 `[x] ` 由 TaskMarker 子节点自己带出，
                        // 这里只补列表标记。
                        let source_marker = if node_has_task_marker(item) {
                            "- ".to_owned()
                        } else if start > 0 && numbered {
                            format!("{}. ", start + index as u64)
                        } else {
                            "- ".to_owned()
                        };
                        self.pending_source_prefix = Some(source_marker);
                        let body = self.children(item_children(item));
                        self.pending_source_prefix = None;
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .mb(px(density.item_gap))
                            .child(
                                div()
                                    .w(px(density.marker_width))
                                    .flex_shrink_0()
                                    .text_right()
                                    .text_color(ui::muted(p))
                                    .child(SharedString::from(marker)),
                            )
                            .child(div().flex_1().min_w(px(0.0)).children(body))
                    })
                    .collect::<Vec<_>>();
                div()
                    .mb(px(density.block_gap))
                    .children(items)
                    .into_any_element()
            }
            MarkdownNode::Item(children) => {
                div().children(self.children(children)).into_any_element()
            }
            MarkdownNode::Table(children) => self.table(children),
            MarkdownNode::Rule => div()
                .w_full()
                .h(px(1.0))
                .my(px(density.rule_gap))
                .bg(ui::border(p))
                .into_any_element(),
            MarkdownNode::RawHtml(html) => div()
                .mb(px(density.block_gap))
                .rounded_md()
                .border_1()
                .border_color(ui::border(p))
                .bg(ui::color(p.elevated))
                .p(px(density.code_padding))
                .font_family(crate::monospace_font::default_family())
                .text_size(px(density.base - 2.0))
                .text_color(ui::muted(p))
                .children(self.text(html.clone(), Some(html.clone()), Vec::new()))
                .into_any_element(),
            MarkdownNode::DisplayMath(math) => div()
                .mb(px(density.block_gap))
                .rounded_md()
                .bg(ui::color(p.elevated))
                .p(px(density.code_padding))
                .font_family(crate::monospace_font::default_family())
                .text_center()
                .children(self.text(
                    math.clone(),
                    Some(format!("$$\n{}$$", ensure_trailing_newline(math))),
                    Vec::new(),
                ))
                .into_any_element(),
            MarkdownNode::FootnoteDefinition { label, children } => div()
                .flex()
                .gap_2()
                .mb_2()
                .text_size(px(density.base - 2.0))
                .child(
                    div()
                        .text_color(ui::color(p.accent))
                        .child(SharedString::from(format!("[^{label}]"))),
                )
                .child(div().flex_1().children(self.children(children)))
                .into_any_element(),
            MarkdownNode::DefinitionList(children)
            | MarkdownNode::DefinitionTitle(children)
            | MarkdownNode::Definition(children)
            | MarkdownNode::Container(children)
            | MarkdownNode::TableHead(children)
            | MarkdownNode::TableRow(children)
            | MarkdownNode::TableCell(children) => {
                div().children(self.children(children)).into_any_element()
            }
            _ => div()
                .mb_2()
                .text_size(px(density.base))
                .children(self.inline(std::slice::from_ref(node)))
                .into_any_element(),
        }
    }

    fn table(&mut self, children: &[MarkdownNode]) -> AnyElement {
        let density = self.density;
        let p = self.p;
        let element_id = self.next_id("table");
        let mut rows = Vec::<(&[MarkdownNode], bool)>::new();
        for child in children {
            match child {
                MarkdownNode::TableHead(head) => {
                    if head
                        .iter()
                        .all(|node| matches!(node, MarkdownNode::TableCell(_)))
                    {
                        rows.push((head, true));
                    } else {
                        for row in head {
                            if let MarkdownNode::TableRow(cells) = row {
                                rows.push((cells, true));
                            }
                        }
                    }
                }
                MarkdownNode::TableRow(cells) => rows.push((cells, false)),
                _ => {}
            }
        }
        let rows = rows
            .into_iter()
            .map(|(cells, header)| {
                let cells = cells
                    .iter()
                    .map(|cell| {
                        let content = match cell {
                            MarkdownNode::TableCell(children) => children.as_slice(),
                            _ => std::slice::from_ref(cell),
                        };
                        div()
                            .flex_1()
                            .min_w(px(density.cell_min_width))
                            .px(px(density.cell_px))
                            .py(px(density.cell_py))
                            .border_r_1()
                            .border_color(ui::border(p))
                            .when(header, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                            .children(self.inline(content))
                    })
                    .collect::<Vec<_>>();
                div()
                    .flex()
                    .w_full()
                    .border_b_1()
                    .border_color(ui::border(p))
                    .when(header, |row| row.bg(ui::color(p.elevated)))
                    .children(cells)
            })
            .collect::<Vec<_>>();
        div()
            .id(element_id)
            .mb(px(density.major_gap))
            .overflow_x_scroll()
            .rounded_md()
            .border_1()
            .border_color(ui::border(p))
            .children(rows)
            .into_any_element()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct InlineStyle {
    emphasis: bool,
    strong: bool,
    strikethrough: bool,
    code: bool,
    link: bool,
    muted: bool,
}

fn append_inline(
    nodes: &[MarkdownNode],
    style: InlineStyle,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
    p: &ResolvedPalette,
) {
    for node in nodes {
        match node {
            MarkdownNode::Text(value) => append_run(value, style, text, highlights, p),
            MarkdownNode::InlineCode(value) => append_run(
                value,
                InlineStyle {
                    code: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::InlineMath(value) => append_run(
                value,
                InlineStyle {
                    code: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Emphasis(children) => append_inline(
                children,
                InlineStyle {
                    emphasis: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Strong(children) => append_inline(
                children,
                InlineStyle {
                    strong: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Strikethrough(children) => append_inline(
                children,
                InlineStyle {
                    strikethrough: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Superscript(children) | MarkdownNode::Subscript(children) => {
                append_inline(children, style, text, highlights, p)
            }
            MarkdownNode::Link { children, .. } => append_inline(
                children,
                InlineStyle {
                    link: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Image { children, .. } => {
                append_run(
                    "Image: ",
                    InlineStyle {
                        muted: true,
                        ..style
                    },
                    text,
                    highlights,
                    p,
                );
                append_inline(
                    children,
                    InlineStyle {
                        link: true,
                        ..style
                    },
                    text,
                    highlights,
                    p,
                );
            }
            MarkdownNode::TaskMarker(checked) => append_run(
                if *checked { "☑ " } else { "☐ " },
                InlineStyle {
                    strong: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::RawHtml(value) => append_run(
                value,
                InlineStyle {
                    code: true,
                    muted: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::Rule => append_run("—", style, text, highlights, p),
            MarkdownNode::CodeBlock { code, .. } | MarkdownNode::DisplayMath(code) => append_run(
                code,
                InlineStyle {
                    code: true,
                    ..style
                },
                text,
                highlights,
                p,
            ),
            MarkdownNode::List { items, .. } => append_inline(items, style, text, highlights, p),
            MarkdownNode::FootnoteDefinition { children, .. }
            | MarkdownNode::Paragraph(children)
            | MarkdownNode::Heading { children, .. }
            | MarkdownNode::BlockQuote(children)
            | MarkdownNode::Item(children)
            | MarkdownNode::Table(children)
            | MarkdownNode::TableHead(children)
            | MarkdownNode::TableRow(children)
            | MarkdownNode::TableCell(children)
            | MarkdownNode::DefinitionList(children)
            | MarkdownNode::DefinitionTitle(children)
            | MarkdownNode::Definition(children)
            | MarkdownNode::Container(children) => {
                append_inline(children, style, text, highlights, p)
            }
        }
    }
}

fn append_run(
    value: &str,
    style: InlineStyle,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
    p: &ResolvedPalette,
) {
    let start = text.len();
    text.push_str(value);
    let end = text.len();
    if start == end || style == InlineStyle::default() {
        return;
    }
    let mut highlight = HighlightStyle::default();
    if style.strong {
        highlight.font_weight = Some(FontWeight::BOLD);
    }
    if style.emphasis {
        highlight.font_style = Some(FontStyle::Italic);
    }
    if style.code {
        highlight.background_color = Some(ui::color(p.elevated).into());
        highlight.color = Some(ui::color(p.status[0]).into());
    } else if style.link {
        highlight.color = Some(ui::color(p.accent).into());
        highlight.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(ui::color(p.accent).into()),
            wavy: false,
        });
    } else if style.muted || style.strikethrough {
        highlight.color = Some(ui::muted(p).into());
    }
    if style.strikethrough {
        highlight.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(ui::muted(p).into()),
        });
    }
    highlights.push((start..end, highlight));
}

fn item_children(node: &MarkdownNode) -> &[MarkdownNode] {
    match node {
        MarkdownNode::Item(children) => children,
        _ => std::slice::from_ref(node),
    }
}

fn node_has_task_marker(node: &MarkdownNode) -> bool {
    match node {
        MarkdownNode::TaskMarker(_) => true,
        MarkdownNode::Item(children) | MarkdownNode::Paragraph(children) => {
            children.iter().any(node_has_task_marker)
        }
        _ => false,
    }
}

/// 把内联节点序列化回 Markdown 源码，供选区复制保留 `**`、链接、行内码
/// 等标记。语法树不保留软换行等原文细节（解析时已折成空格），得到的是
/// 语义等价的 Markdown，不是逐字节还原。
fn inline_markdown(nodes: &[MarkdownNode]) -> String {
    let mut out = String::new();
    push_inline_markdown(nodes, &mut out);
    out
}

fn push_inline_markdown(nodes: &[MarkdownNode], out: &mut String) {
    for node in nodes {
        match node {
            MarkdownNode::Text(value) => out.push_str(value),
            MarkdownNode::InlineCode(value) => {
                // 围栏反引号数必须多于内容里最长的连续反引号串；首尾是
                // 反引号或空格时补一个空格隔离，否则围栏失效。
                let longest_run = value
                    .split(|c| c != '`')
                    .filter(|run| !run.is_empty())
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                let fence = "`".repeat(longest_run + 1);
                let pad = value.starts_with('`')
                    || value.ends_with('`')
                    || value.starts_with(' ')
                    || value.ends_with(' ');
                out.push_str(&fence);
                if pad {
                    out.push(' ');
                }
                out.push_str(value);
                if pad {
                    out.push(' ');
                }
                out.push_str(&fence);
            }
            MarkdownNode::InlineMath(math) => {
                out.push('$');
                out.push_str(math);
                out.push('$');
            }
            MarkdownNode::DisplayMath(math) => {
                out.push_str("$$");
                out.push_str(math);
                out.push_str("$$");
            }
            MarkdownNode::RawHtml(html) => out.push_str(html),
            MarkdownNode::TaskMarker(checked) => {
                out.push_str(if *checked { "[x] " } else { "[ ] " })
            }
            MarkdownNode::Rule => out.push_str("\n---\n"),
            MarkdownNode::CodeBlock { language, code } => out.push_str(&fence_code(language, code)),
            MarkdownNode::Emphasis(children) => wrap_inline(children, "*", out),
            MarkdownNode::Strong(children) => wrap_inline(children, "**", out),
            MarkdownNode::Strikethrough(children) => wrap_inline(children, "~~", out),
            MarkdownNode::Superscript(children) => wrap_inline(children, "^", out),
            MarkdownNode::Subscript(children) => wrap_inline(children, "~", out),
            MarkdownNode::Link {
                destination,
                children,
            } => {
                out.push('[');
                push_inline_markdown(children, out);
                out.push_str("](");
                out.push_str(destination);
                out.push(')');
            }
            MarkdownNode::Image {
                destination,
                children,
            } => {
                out.push_str("![");
                push_inline_markdown(children, out);
                out.push_str("](");
                out.push_str(destination);
                out.push(')');
            }
            // 容器节点出现在内联上下文（如单元格里的段落）时按子节点递归；
            // 结构标记（缩进、管道、引用符）不在此重建。
            MarkdownNode::Paragraph(children)
            | MarkdownNode::Heading { children, .. }
            | MarkdownNode::BlockQuote(children)
            | MarkdownNode::Item(children)
            | MarkdownNode::Table(children)
            | MarkdownNode::TableHead(children)
            | MarkdownNode::TableRow(children)
            | MarkdownNode::TableCell(children)
            | MarkdownNode::FootnoteDefinition { children, .. }
            | MarkdownNode::DefinitionList(children)
            | MarkdownNode::DefinitionTitle(children)
            | MarkdownNode::Definition(children)
            | MarkdownNode::Container(children) => push_inline_markdown(children, out),
            MarkdownNode::List { items, .. } => push_inline_markdown(items, out),
        }
    }
}

fn wrap_inline(children: &[MarkdownNode], marker: &str, out: &mut String) {
    out.push_str(marker);
    push_inline_markdown(children, out);
    out.push_str(marker);
}

/// 重建带语言标注的代码围栏；`code` 保证以换行结尾再闭合。
fn fence_code(language: &Option<String>, code: &str) -> String {
    let mut fenced = String::from("```");
    if let Some(language) = language {
        fenced.push_str(language);
    }
    fenced.push('\n');
    fenced.push_str(&ensure_trailing_newline(code));
    fenced.push_str("```");
    fenced
}

fn ensure_trailing_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_owned()
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> ResolvedPalette {
        termior_theme::default_theme().palette().clone()
    }

    const SAMPLE: &str = concat!(
        "# Heading\n\n",
        "paragraph with **bold**, *em*, `code` and [link](https://example.com)\n\n",
        "> quoted\n\n",
        "- item one\n",
        "- [ ] task\n\n",
        "```rust\nfn main() {}\n```\n\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
        "---\n\n",
        "text with <b>html</b>\n"
    );

    #[test]
    fn renders_common_blocks_in_both_densities() {
        let p = palette();
        let document = MarkdownDocument::parse(SAMPLE);
        for density in [Density::PREVIEW, Density::CHAT] {
            let blocks = render_blocks(&document, density, "test-md", &p);
            // 标题/段落/引用/列表/代码块/表格/分隔线/段落 = 8 个块。
            assert_eq!(blocks.len(), 8, "density base {}", density.base);
        }
    }

    #[test]
    fn inline_markdown_round_trips_markers() {
        let document = MarkdownDocument::parse(concat!(
            "段落：**粗体**、*强调*、~~删除~~、`code`、",
            "[链接](https://example.com/a)、![图](https://example.com/b.png)"
        ));
        let [MarkdownNode::Paragraph(children), ..] = &document.blocks[..] else {
            panic!("expected a paragraph");
        };
        assert_eq!(
            inline_markdown(children),
            concat!(
                "段落：**粗体**、*强调*、~~删除~~、`code`、",
                "[链接](https://example.com/a)、![图](https://example.com/b.png)"
            )
        );
    }

    #[test]
    fn inline_code_fence_grows_past_embedded_backticks() {
        let document = MarkdownDocument::parse("双引号 ``a ` b`` 与单 `x`");
        let [MarkdownNode::Paragraph(children), ..] = &document.blocks[..] else {
            panic!("expected a paragraph");
        };
        assert_eq!(inline_markdown(children), "双引号 ``a ` b`` 与单 `x`");
    }

    #[test]
    fn empty_source_renders_no_blocks() {
        let p = palette();
        assert!(render_blocks(&MarkdownDocument::parse(""), Density::CHAT, "empty", &p).is_empty());
        assert!(render_blocks(
            &MarkdownDocument::parse("   \n"),
            Density::CHAT,
            "blank",
            &p
        )
        .is_empty());
    }

    #[test]
    fn unterminated_fence_still_renders() {
        // 流式输出中代码块尚未闭合是常态，渲染不能因此 panic。
        let p = palette();
        let document = MarkdownDocument::parse("```rust\nfn partial(");
        let blocks = render_blocks(&document, Density::CHAT, "streaming", &p);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn chat_density_is_compact() {
        // 聊天排版必须比预览更紧凑，否则停靠面板里正文显得松散；
        // 密度是常量，直接在编译期断言。
        const _: () = {
            assert!(Density::CHAT.base < Density::PREVIEW.base);
            assert!(Density::CHAT.block_gap < Density::PREVIEW.block_gap);
            assert!(Density::CHAT.major_gap < Density::PREVIEW.major_gap);
        };
    }
}
