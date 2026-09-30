use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownDocument {
    pub blocks: Vec<MarkdownNode>,
}

impl MarkdownDocument {
    pub fn parse(source: &str) -> Self {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_SMART_PUNCTUATION
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_MATH
            | Options::ENABLE_GFM
            | Options::ENABLE_DEFINITION_LIST
            | Options::ENABLE_SUPERSCRIPT
            | Options::ENABLE_SUBSCRIPT
            | Options::ENABLE_WIKILINKS;
        let mut roots = Vec::new();
        let mut frames = Vec::<Frame>::new();

        for event in Parser::new_ext(source, options) {
            match event {
                Event::Start(tag) => frames.push(Frame::new(tag)),
                Event::End(end) => finish_frame(end, &mut frames, &mut roots),
                Event::Text(text) => push_node(
                    MarkdownNode::Text(text.into_string()),
                    &mut frames,
                    &mut roots,
                ),
                Event::Code(code) => push_node(
                    MarkdownNode::InlineCode(code.into_string()),
                    &mut frames,
                    &mut roots,
                ),
                Event::InlineMath(math) => push_node(
                    MarkdownNode::InlineMath(math.into_string()),
                    &mut frames,
                    &mut roots,
                ),
                Event::DisplayMath(math) => push_node(
                    MarkdownNode::DisplayMath(math.into_string()),
                    &mut frames,
                    &mut roots,
                ),
                // Raw HTML is deliberately represented as text. The native preview never executes
                // scripts or injects document markup into the Web preview backend.
                Event::Html(html) | Event::InlineHtml(html) => push_node(
                    MarkdownNode::RawHtml(html.into_string()),
                    &mut frames,
                    &mut roots,
                ),
                Event::FootnoteReference(label) => push_node(
                    MarkdownNode::Text(format!("[^{label}]")),
                    &mut frames,
                    &mut roots,
                ),
                Event::SoftBreak => {
                    push_node(MarkdownNode::Text(" ".into()), &mut frames, &mut roots)
                }
                Event::HardBreak => {
                    push_node(MarkdownNode::Text("\n".into()), &mut frames, &mut roots)
                }
                Event::Rule => push_node(MarkdownNode::Rule, &mut frames, &mut roots),
                Event::TaskListMarker(checked) => {
                    push_node(MarkdownNode::TaskMarker(checked), &mut frames, &mut roots)
                }
            }
        }

        while let Some(frame) = frames.pop() {
            push_node(frame.into_node(), &mut frames, &mut roots);
        }
        Self { blocks: roots }
    }

    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for node in &self.blocks {
            node.push_plain_text(&mut text);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownNode {
    Paragraph(Vec<Self>),
    Heading {
        level: u8,
        children: Vec<Self>,
    },
    BlockQuote(Vec<Self>),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    List {
        start: Option<u64>,
        items: Vec<Self>,
    },
    Item(Vec<Self>),
    Emphasis(Vec<Self>),
    Strong(Vec<Self>),
    Strikethrough(Vec<Self>),
    Superscript(Vec<Self>),
    Subscript(Vec<Self>),
    Link {
        destination: String,
        children: Vec<Self>,
    },
    Image {
        destination: String,
        children: Vec<Self>,
    },
    Table(Vec<Self>),
    TableHead(Vec<Self>),
    TableRow(Vec<Self>),
    TableCell(Vec<Self>),
    FootnoteDefinition {
        label: String,
        children: Vec<Self>,
    },
    DefinitionList(Vec<Self>),
    DefinitionTitle(Vec<Self>),
    Definition(Vec<Self>),
    Container(Vec<Self>),
    Text(String),
    InlineCode(String),
    InlineMath(String),
    DisplayMath(String),
    RawHtml(String),
    TaskMarker(bool),
    Rule,
}

impl MarkdownNode {
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        self.push_plain_text(&mut text);
        text
    }

    fn push_plain_text(&self, output: &mut String) {
        match self {
            Self::Text(text)
            | Self::InlineCode(text)
            | Self::InlineMath(text)
            | Self::DisplayMath(text)
            | Self::RawHtml(text) => output.push_str(text),
            Self::TaskMarker(checked) => output.push_str(if *checked { "[x] " } else { "[ ] " }),
            Self::CodeBlock { code, .. } => output.push_str(code),
            Self::Link { children, .. }
            | Self::Image { children, .. }
            | Self::Paragraph(children)
            | Self::Heading { children, .. }
            | Self::BlockQuote(children)
            | Self::Item(children)
            | Self::Emphasis(children)
            | Self::Strong(children)
            | Self::Strikethrough(children)
            | Self::Superscript(children)
            | Self::Subscript(children)
            | Self::Table(children)
            | Self::TableHead(children)
            | Self::TableRow(children)
            | Self::TableCell(children)
            | Self::DefinitionList(children)
            | Self::DefinitionTitle(children)
            | Self::Definition(children)
            | Self::Container(children)
            | Self::FootnoteDefinition { children, .. } => {
                for child in children {
                    child.push_plain_text(output);
                }
            }
            Self::List { items, .. } => {
                for item in items {
                    item.push_plain_text(output);
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            Self::Rule => output.push_str("---"),
        }
    }
}

#[derive(Debug)]
struct Frame {
    kind: FrameKind,
    children: Vec<MarkdownNode>,
}

impl Frame {
    fn new(tag: Tag<'_>) -> Self {
        let kind = match tag {
            Tag::Paragraph => FrameKind::Paragraph,
            Tag::Heading { level, .. } => FrameKind::Heading(level as u8),
            Tag::BlockQuote(_) => FrameKind::BlockQuote,
            Tag::CodeBlock(kind) => FrameKind::CodeBlock(match kind {
                CodeBlockKind::Indented => None,
                CodeBlockKind::Fenced(language) => language
                    .split_whitespace()
                    .next()
                    .filter(|language| !language.is_empty())
                    .map(str::to_owned),
            }),
            Tag::HtmlBlock => FrameKind::HtmlBlock,
            Tag::List(start) => FrameKind::List(start),
            Tag::Item => FrameKind::Item,
            Tag::FootnoteDefinition(label) => FrameKind::FootnoteDefinition(label.into_string()),
            Tag::DefinitionList => FrameKind::DefinitionList,
            Tag::DefinitionListTitle => FrameKind::DefinitionTitle,
            Tag::DefinitionListDefinition => FrameKind::Definition,
            Tag::Table(_) => FrameKind::Table,
            Tag::TableHead => FrameKind::TableHead,
            Tag::TableRow => FrameKind::TableRow,
            Tag::TableCell => FrameKind::TableCell,
            Tag::Emphasis => FrameKind::Emphasis,
            Tag::Strong => FrameKind::Strong,
            Tag::Strikethrough => FrameKind::Strikethrough,
            Tag::Superscript => FrameKind::Superscript,
            Tag::Subscript => FrameKind::Subscript,
            Tag::Link { dest_url, .. } => FrameKind::Link(dest_url.into_string()),
            Tag::Image { dest_url, .. } => FrameKind::Image(dest_url.into_string()),
            Tag::MetadataBlock(_) => FrameKind::Container,
        };
        Self {
            kind,
            children: Vec::new(),
        }
    }

    fn into_node(self) -> MarkdownNode {
        match self.kind {
            FrameKind::Paragraph => MarkdownNode::Paragraph(self.children),
            FrameKind::Heading(level) => MarkdownNode::Heading {
                level,
                children: self.children,
            },
            FrameKind::BlockQuote => MarkdownNode::BlockQuote(self.children),
            FrameKind::CodeBlock(language) => MarkdownNode::CodeBlock {
                language,
                code: flatten_text(&self.children),
            },
            FrameKind::HtmlBlock => MarkdownNode::RawHtml(flatten_text(&self.children)),
            FrameKind::List(start) => MarkdownNode::List {
                start,
                items: self.children,
            },
            FrameKind::Item => MarkdownNode::Item(group_item_inline_runs(self.children)),
            FrameKind::FootnoteDefinition(label) => MarkdownNode::FootnoteDefinition {
                label,
                children: self.children,
            },
            FrameKind::DefinitionList => MarkdownNode::DefinitionList(self.children),
            FrameKind::DefinitionTitle => MarkdownNode::DefinitionTitle(self.children),
            FrameKind::Definition => MarkdownNode::Definition(self.children),
            FrameKind::Table => MarkdownNode::Table(self.children),
            FrameKind::TableHead => MarkdownNode::TableHead(self.children),
            FrameKind::TableRow => MarkdownNode::TableRow(self.children),
            FrameKind::TableCell => MarkdownNode::TableCell(self.children),
            FrameKind::Emphasis => MarkdownNode::Emphasis(self.children),
            FrameKind::Strong => MarkdownNode::Strong(self.children),
            FrameKind::Strikethrough => MarkdownNode::Strikethrough(self.children),
            FrameKind::Superscript => MarkdownNode::Superscript(self.children),
            FrameKind::Subscript => MarkdownNode::Subscript(self.children),
            FrameKind::Link(destination) => MarkdownNode::Link {
                destination,
                children: self.children,
            },
            FrameKind::Image(destination) => MarkdownNode::Image {
                destination,
                children: self.children,
            },
            FrameKind::Container => MarkdownNode::Container(self.children),
        }
    }
}

#[derive(Debug)]
enum FrameKind {
    Paragraph,
    Heading(u8),
    BlockQuote,
    CodeBlock(Option<String>),
    HtmlBlock,
    List(Option<u64>),
    Item,
    FootnoteDefinition(String),
    DefinitionList,
    DefinitionTitle,
    Definition,
    Table,
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strikethrough,
    Superscript,
    Subscript,
    Link(String),
    Image(String),
    Container,
}

fn finish_frame(_end: TagEnd, frames: &mut Vec<Frame>, roots: &mut Vec<MarkdownNode>) {
    if let Some(frame) = frames.pop() {
        push_node(frame.into_node(), frames, roots);
    }
}

/// 紧凑列表项（如 `- HEAD 停在 \`main\``）不经过 Paragraph 帧，内联节点直接挂在
/// Item 下。块级渲染器按块处理每个子节点，裸内联序列会被拆成一行一个片段；把
/// 连续的内联子节点聚合为 Paragraph（与松散列表项的表示一致），嵌套块保持原位。
fn group_item_inline_runs(children: Vec<MarkdownNode>) -> Vec<MarkdownNode> {
    fn is_inline(node: &MarkdownNode) -> bool {
        matches!(
            node,
            MarkdownNode::Text(_)
                | MarkdownNode::InlineCode(_)
                | MarkdownNode::InlineMath(_)
                | MarkdownNode::DisplayMath(_)
                | MarkdownNode::RawHtml(_)
                | MarkdownNode::Emphasis(_)
                | MarkdownNode::Strong(_)
                | MarkdownNode::Strikethrough(_)
                | MarkdownNode::Superscript(_)
                | MarkdownNode::Subscript(_)
                | MarkdownNode::Link { .. }
                | MarkdownNode::Image { .. }
                | MarkdownNode::TaskMarker(_)
        )
    }

    let mut grouped = Vec::new();
    let mut inline_run = Vec::new();
    for child in children {
        if is_inline(&child) {
            inline_run.push(child);
        } else {
            if !inline_run.is_empty() {
                grouped.push(MarkdownNode::Paragraph(std::mem::take(&mut inline_run)));
            }
            grouped.push(child);
        }
    }
    if !inline_run.is_empty() {
        grouped.push(MarkdownNode::Paragraph(inline_run));
    }
    grouped
}

fn push_node(node: MarkdownNode, frames: &mut [Frame], roots: &mut Vec<MarkdownNode>) {
    if let Some(frame) = frames.last_mut() {
        frame.children.push(node);
    } else {
        roots.push(node);
    }
}

fn flatten_text(nodes: &[MarkdownNode]) -> String {
    let mut text = String::new();
    for node in nodes {
        node.push_plain_text(&mut text);
    }
    text
}

pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "markdown" | "mdown" | "mkd" | "mkdn"
            )
        })
}

/// Hypertext Markup Language documents that can be opened in the system browser.
pub fn is_html_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "html" | "htm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_markdown_paths_case_insensitively() {
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("Guide.MARKDOWN")));
        assert!(!is_markdown_path(Path::new("component.mdx")));
        assert!(!is_markdown_path(Path::new("index.html")));
    }

    #[test]
    fn recognizes_html_paths_case_insensitively() {
        assert!(is_html_path(Path::new("index.html")));
        assert!(is_html_path(Path::new("about.HTM")));
        assert!(!is_html_path(Path::new("README.md")));
        assert!(!is_html_path(Path::new("component.htmlx")));
    }

    #[test]
    fn parses_common_markdown_without_losing_visible_text() {
        let document = MarkdownDocument::parse(
            "# Termior\n\nA **native** *preview* with `code` and [docs](https://example.com).",
        );
        assert!(matches!(
            document.blocks.first(),
            Some(MarkdownNode::Heading { level: 1, .. })
        ));
        let text = document.plain_text();
        assert!(text.contains("Termior"));
        assert!(text.contains("native preview with code and docs"));
    }

    #[test]
    fn parses_gfm_lists_tables_and_code_blocks() {
        let document = MarkdownDocument::parse(
            "- [x] fixed\n- [ ] verify\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n```rust\nfn main() {}\n```",
        );
        assert!(document
            .blocks
            .iter()
            .any(|node| matches!(node, MarkdownNode::List { .. })));
        assert!(document
            .blocks
            .iter()
            .any(|node| matches!(node, MarkdownNode::Table(_))));
        assert!(document.blocks.iter().any(|node| matches!(
            node,
            MarkdownNode::CodeBlock {
                language: Some(language),
                code,
            } if language == "rust" && code.contains("fn main")
        )));
    }

    #[test]
    fn raw_html_is_data_not_an_executable_render_target() {
        let document = MarkdownDocument::parse("<script>alert('no')</script>");
        assert!(document
            .blocks
            .iter()
            .any(|node| matches!(node, MarkdownNode::RawHtml(html) if html.contains("script"))));
    }

    #[test]
    fn tight_list_items_group_inline_runs_into_paragraphs() {
        // 紧凑列表项不带 Paragraph 帧。若内联节点直接挂在 Item 下，块级渲染器会
        // 一行一个片段地排版（Agent 消息碎行缺陷），解析层必须聚合为段落。
        let document = MarkdownDocument::parse(
            "- HEAD 停在 `main`，而 `feature/workspace` 落后于 `main`（早 9 个提交）。\n- 推进 **Composer / Agent 功能**（`FR-AGENT-*`）以及 `termior-ui` 拆分。\n",
        );
        let Some(MarkdownNode::List { items, .. }) = document.blocks.first() else {
            panic!("expected a list, got {:?}", document.blocks.first());
        };
        assert_eq!(items.len(), 2);
        for item in items {
            let MarkdownNode::Item(children) = item else {
                panic!("expected an item, got {item:?}");
            };
            assert_eq!(
                children.len(),
                1,
                "tight item must render as a single paragraph, got {children:?}"
            );
            assert!(matches!(children[0], MarkdownNode::Paragraph(_)));
        }
    }

    #[test]
    fn tight_item_keeps_nested_blocks_beside_the_paragraph() {
        // 内联内容与嵌套块（子列表/代码块）混排时，段落聚合不得吞掉块节点。
        let document = MarkdownDocument::parse("- run `make`\n  - nested\n");
        let Some(MarkdownNode::List { items, .. }) = document.blocks.first() else {
            panic!("expected a list, got {:?}", document.blocks.first());
        };
        let MarkdownNode::Item(children) = &items[0] else {
            panic!("expected an item");
        };
        assert_eq!(
            children.len(),
            2,
            "paragraph + nested list, got {children:?}"
        );
        assert!(matches!(children[0], MarkdownNode::Paragraph(_)));
        assert!(matches!(children[1], MarkdownNode::List { .. }));
    }
}
