//! Markdown → GPUI 块级渲染器。
//!
//! 同一套渲染同时服务 Markdown 预览页（[`Density::PREVIEW`]）与 Agent 会话消息
//! （[`Density::CHAT`]），对应 SDD 的依赖决策：pulldown-cmark 解析 + GPUI 原生
//! 渲染，不为同一能力引入第二套管线。视图层只负责外壳（工具栏、滚动容器等）。

use crate::ui;
use gpui::{
    div, prelude::*, px, AnyElement, FontStyle, FontWeight, HighlightStyle, SharedString,
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
    let mut seq = 0usize;
    document
        .blocks
        .iter()
        .map(|node| render_block(node, density, id_prefix, &mut seq, p))
        .collect()
}

fn next_id(id_prefix: &str, kind: &str, seq: &mut usize) -> SharedString {
    let id = SharedString::from(format!("{id_prefix}-{kind}-{seq}"));
    *seq += 1;
    id
}

fn render_block(
    node: &MarkdownNode,
    density: Density,
    id_prefix: &str,
    seq: &mut usize,
    p: &ResolvedPalette,
) -> AnyElement {
    match node {
        MarkdownNode::Heading { level, children } => {
            let index = (*level as usize).saturating_sub(1).min(3);
            let (size, top, bottom) = density.headings[index];
            div()
                .mt(px(top))
                .mb(px(bottom))
                .text_size(px(size))
                .font_weight(FontWeight::BOLD)
                .when(*level <= 2, |heading| {
                    heading.pb_2().border_b_1().border_color(ui::border(p))
                })
                .child(styled_inline(children, p))
                .into_any_element()
        }
        MarkdownNode::Paragraph(children) => div()
            .mb(px(density.block_gap))
            .text_size(px(density.base))
            .child(styled_inline(children, p))
            .into_any_element(),
        MarkdownNode::BlockQuote(children) => div()
            .mb(px(density.block_gap))
            .pl(px(density.quote_indent))
            .py_1()
            .border_l_2()
            .border_color(ui::color(p.accent))
            .text_color(ui::muted(p))
            .children(
                children
                    .iter()
                    .map(|child| render_block(child, density, id_prefix, seq, p)),
            )
            .into_any_element(),
        MarkdownNode::CodeBlock { language, code } => div()
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
                    .id(next_id(id_prefix, "code", seq))
                    .overflow_x_scroll()
                    .p(px(density.code_padding))
                    .font_family(crate::monospace_font::default_family())
                    .text_size(px(density.base))
                    .whitespace_nowrap()
                    .child(SharedString::from(code.clone())),
            )
            .into_any_element(),
        MarkdownNode::List { start, items } => {
            let start = start.unwrap_or(1);
            div()
                .mb(px(density.block_gap))
                .children(items.iter().enumerate().map(|(index, item)| {
                    let marker = if node_has_task_marker(item) {
                        String::new()
                    } else if start > 0 && matches!(node, MarkdownNode::List { start: Some(_), .. })
                    {
                        format!("{}. ", start + index as u64)
                    } else {
                        "• ".to_owned()
                    };
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
                        .child(
                            div().flex_1().min_w(px(0.0)).children(
                                item_children(item)
                                    .iter()
                                    .map(|child| render_block(child, density, id_prefix, seq, p)),
                            ),
                        )
                }))
                .into_any_element()
        }
        MarkdownNode::Item(children) => div()
            .children(
                children
                    .iter()
                    .map(|child| render_block(child, density, id_prefix, seq, p)),
            )
            .into_any_element(),
        MarkdownNode::Table(children) => {
            render_table(children, next_id(id_prefix, "table", seq), density, p)
        }
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
            .child(SharedString::from(html.clone()))
            .into_any_element(),
        MarkdownNode::DisplayMath(math) => div()
            .mb(px(density.block_gap))
            .rounded_md()
            .bg(ui::color(p.elevated))
            .p(px(density.code_padding))
            .font_family(crate::monospace_font::default_family())
            .text_center()
            .child(SharedString::from(math.clone()))
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
            .child(
                div().flex_1().children(
                    children
                        .iter()
                        .map(|child| render_block(child, density, id_prefix, seq, p)),
                ),
            )
            .into_any_element(),
        MarkdownNode::DefinitionList(children)
        | MarkdownNode::DefinitionTitle(children)
        | MarkdownNode::Definition(children)
        | MarkdownNode::Container(children)
        | MarkdownNode::TableHead(children)
        | MarkdownNode::TableRow(children)
        | MarkdownNode::TableCell(children) => div()
            .children(
                children
                    .iter()
                    .map(|child| render_block(child, density, id_prefix, seq, p)),
            )
            .into_any_element(),
        _ => div()
            .mb_2()
            .text_size(px(density.base))
            .child(styled_inline(std::slice::from_ref(node), p))
            .into_any_element(),
    }
}

fn render_table(
    children: &[MarkdownNode],
    element_id: SharedString,
    density: Density,
    p: &ResolvedPalette,
) -> AnyElement {
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
    div()
        .id(element_id)
        .mb(px(density.major_gap))
        .overflow_x_scroll()
        .rounded_md()
        .border_1()
        .border_color(ui::border(p))
        .children(rows.into_iter().map(|(cells, header)| {
            div()
                .flex()
                .w_full()
                .border_b_1()
                .border_color(ui::border(p))
                .when(header, |row| row.bg(ui::color(p.elevated)))
                .children(cells.iter().map(|cell| {
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
                        .child(styled_inline(content, p))
                }))
        }))
        .into_any_element()
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

fn styled_inline(nodes: &[MarkdownNode], p: &ResolvedPalette) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::<(Range<usize>, HighlightStyle)>::new();
    append_inline(nodes, InlineStyle::default(), &mut text, &mut highlights, p);
    StyledText::new(text).with_highlights(highlights)
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
