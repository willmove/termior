use crate::{editor_view::EditorView, ui};
use gpui::{
    div, prelude::*, px, relative, AnyElement, Context, Entity, FontStyle, FontWeight,
    HighlightStyle, Render, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window,
};
use std::{ops::Range, path::PathBuf};
use termior_preview::{MarkdownDocument, MarkdownNode};
use termior_theme::ResolvedPalette;

pub struct MarkdownPreviewView {
    path: PathBuf,
    source: Entity<EditorView>,
    source_revision: u64,
    source_text: String,
    document: MarkdownDocument,
}

impl MarkdownPreviewView {
    pub fn new(path: PathBuf, source: Entity<EditorView>, cx: &mut Context<Self>) -> Self {
        let (source_revision, source_text) = {
            let source = source.read(cx);
            (source.revision(), source.text())
        };
        let document = MarkdownDocument::parse(&source_text);
        cx.observe(&source, |preview, source, cx| {
            let source_revision = source.read(cx).revision();
            if source_revision == preview.source_revision {
                return;
            }
            preview.source_revision = source_revision;
            preview.source_text = source.read(cx).text();
            preview.document = MarkdownDocument::parse(&preview.source_text);
            cx.notify();
        })
        .detach();
        Self {
            path,
            source,
            source_revision,
            source_text,
            document,
        }
    }

    pub fn contains_text(&self, expected: &str) -> bool {
        self.document.plain_text().contains(expected)
    }

    pub fn source(&self) -> Entity<EditorView> {
        self.source.clone()
    }

    pub fn set_path_after_rename(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = path.clone();
        self.source
            .update(cx, |source, _| source.set_path_after_rename(path));
        cx.notify();
    }
}

impl Render for MarkdownPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let title = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown");
        let blocks = self
            .document
            .blocks
            .iter()
            .map(|node| render_block(node, &p))
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.0))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(ui::border(&p))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child("Markdown Preview"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(ui::muted(&p))
                            .child(SharedString::from(title.to_owned())),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_full()
                            .bg(ui::selected_wash(&p))
                            .text_xs()
                            .text_color(ui::color(p.accent))
                            .child("Live"),
                    ),
            )
            .child(
                div()
                    .id("markdown-preview-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(
                        div().flex().justify_center().w_full().child(
                            div()
                                .w_full()
                                .max_w(px(880.0))
                                .px_8()
                                .py_6()
                                .line_height(relative(1.55))
                                .children(blocks),
                        ),
                    ),
            )
    }
}

fn render_block(node: &MarkdownNode, p: &ResolvedPalette) -> AnyElement {
    match node {
        MarkdownNode::Heading { level, children } => {
            let (size, top, bottom) = match level {
                1 => (30.0, 20.0, 12.0),
                2 => (24.0, 18.0, 10.0),
                3 => (20.0, 16.0, 8.0),
                _ => (16.0, 14.0, 6.0),
            };
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
            .mb_3()
            .text_sm()
            .child(styled_inline(children, p))
            .into_any_element(),
        MarkdownNode::BlockQuote(children) => div()
            .mb_3()
            .pl_4()
            .py_1()
            .border_l_2()
            .border_color(ui::color(p.accent))
            .text_color(ui::muted(p))
            .children(children.iter().map(|child| render_block(child, p)))
            .into_any_element(),
        MarkdownNode::CodeBlock { language, code } => div()
            .mb_4()
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
                        .text_xs()
                        .text_color(ui::muted(p))
                        .child(SharedString::from(language)),
                )
            })
            .child(
                div()
                    .id(("markdown-code-block", node as *const MarkdownNode as usize))
                    .overflow_x_scroll()
                    .p_3()
                    .font_family("monospace")
                    .text_sm()
                    .whitespace_nowrap()
                    .child(SharedString::from(code.clone())),
            )
            .into_any_element(),
        MarkdownNode::List { start, items } => {
            let start = start.unwrap_or(1);
            div()
                .mb_3()
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
                        .mb_1()
                        .child(
                            div()
                                .w(px(28.0))
                                .flex_shrink_0()
                                .text_right()
                                .text_color(ui::muted(p))
                                .child(SharedString::from(marker)),
                        )
                        .child(
                            div().flex_1().min_w(px(0.0)).children(
                                item_children(item)
                                    .iter()
                                    .map(|child| render_block(child, p)),
                            ),
                        )
                }))
                .into_any_element()
        }
        MarkdownNode::Item(children) => div()
            .children(children.iter().map(|child| render_block(child, p)))
            .into_any_element(),
        MarkdownNode::Table(children) => {
            render_table(children, node as *const MarkdownNode as usize, p)
        }
        MarkdownNode::Rule => div()
            .w_full()
            .h(px(1.0))
            .my_4()
            .bg(ui::border(p))
            .into_any_element(),
        MarkdownNode::RawHtml(html) => div()
            .mb_3()
            .rounded_md()
            .border_1()
            .border_color(ui::border(p))
            .bg(ui::color(p.elevated))
            .p_3()
            .font_family("monospace")
            .text_xs()
            .text_color(ui::muted(p))
            .child(SharedString::from(html.clone()))
            .into_any_element(),
        MarkdownNode::DisplayMath(math) => div()
            .mb_3()
            .rounded_md()
            .bg(ui::color(p.elevated))
            .p_3()
            .font_family("monospace")
            .text_center()
            .child(SharedString::from(math.clone()))
            .into_any_element(),
        MarkdownNode::FootnoteDefinition { label, children } => div()
            .flex()
            .gap_2()
            .mb_2()
            .text_xs()
            .child(
                div()
                    .text_color(ui::color(p.accent))
                    .child(SharedString::from(format!("[^{label}]"))),
            )
            .child(
                div()
                    .flex_1()
                    .children(children.iter().map(|child| render_block(child, p))),
            )
            .into_any_element(),
        MarkdownNode::DefinitionList(children)
        | MarkdownNode::DefinitionTitle(children)
        | MarkdownNode::Definition(children)
        | MarkdownNode::Container(children)
        | MarkdownNode::TableHead(children)
        | MarkdownNode::TableRow(children)
        | MarkdownNode::TableCell(children) => div()
            .children(children.iter().map(|child| render_block(child, p)))
            .into_any_element(),
        _ => div()
            .mb_2()
            .text_sm()
            .child(styled_inline(std::slice::from_ref(node), p))
            .into_any_element(),
    }
}

fn render_table(children: &[MarkdownNode], element_id: usize, p: &ResolvedPalette) -> AnyElement {
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
        .id(("markdown-table", element_id))
        .mb_4()
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
                        .min_w(px(120.0))
                        .px_3()
                        .py_2()
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
