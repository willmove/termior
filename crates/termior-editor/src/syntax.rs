//! Incremental tree-sitter document and viewport spans (FR-EDIT-02).

use std::ops::Range;
use std::path::Path;
use tree_sitter::{InputEdit, Language, Parser, Point, Tree};

use crate::BufferEdit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxLanguage {
    TypeScript,
    Tsx,
    JavaScript,
    Rust,
    Python,
    Go,
    C,
    Cpp,
    Java,
    Html,
    Css,
    Json,
    Markdown,
    PlainText,
}

impl SyntaxLanguage {
    /// 稳定的语言 id（如 `"rust"`、`"typescript"`），作为补全 Provider 的语言提示。
    pub fn id(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Java => "java",
            Self::Html => "html",
            Self::Css => "css",
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::PlainText => "plaintext",
        }
    }

    pub fn detect(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match ext.as_str() {
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" => Self::Cpp,
            "java" => Self::Java,
            "html" | "htm" => Self::Html,
            "css" => Self::Css,
            "json" | "jsonc" => Self::Json,
            "md" | "markdown" | "mdx" => Self::Markdown,
            _ => Self::PlainText,
        }
    }

    fn tree_sitter_language(self) -> Option<Language> {
        match self {
            Self::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Self::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            Self::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            Self::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            Self::Python => Some(tree_sitter_python::LANGUAGE.into()),
            Self::Go => Some(tree_sitter_go::LANGUAGE.into()),
            Self::C => Some(tree_sitter_c::LANGUAGE.into()),
            Self::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
            Self::Java => Some(tree_sitter_java::LANGUAGE.into()),
            Self::Html => Some(tree_sitter_html::LANGUAGE.into()),
            Self::Css => Some(tree_sitter_css::LANGUAGE.into()),
            Self::Json => Some(tree_sitter_json::LANGUAGE.into()),
            Self::Markdown => Some(tree_sitter_md::LANGUAGE.into()),
            Self::PlainText => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Comment,
    String,
    Number,
    Keyword,
    Function,
    Type,
    Property,
    Punctuation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightSpan {
    pub byte_range: Range<usize>,
    pub kind: HighlightKind,
}

pub struct SyntaxDocument {
    language: SyntaxLanguage,
    parser: Option<Parser>,
    tree: Option<Tree>,
}

impl std::fmt::Debug for SyntaxDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxDocument")
            .field("language", &self.language)
            .field("has_tree", &self.tree.is_some())
            .finish()
    }
}

impl SyntaxDocument {
    pub fn new(language: SyntaxLanguage, source: &str) -> Self {
        let mut parser = language.tree_sitter_language().map(|lang| {
            let mut parser = Parser::new();
            parser
                .set_language(&lang)
                .expect("bundled grammar must match tree-sitter ABI");
            parser
        });
        let tree = parser.as_mut().and_then(|p| p.parse(source, None));
        Self {
            language,
            parser,
            tree,
        }
    }

    pub fn language(&self) -> SyntaxLanguage {
        self.language
    }

    pub fn reparse(&mut self, source: &str) {
        if let Some(parser) = &mut self.parser {
            self.tree = parser.parse(source, None);
        }
    }

    /// Apply rope mutation coordinates before asking tree-sitter to incrementally reparse.
    pub fn reparse_incremental(&mut self, source: &str, edits: &[BufferEdit]) {
        if edits.is_empty() {
            return;
        }
        if let Some(tree) = &mut self.tree {
            for edit in edits {
                tree.edit(&InputEdit {
                    start_byte: edit.start_byte,
                    old_end_byte: edit.old_end_byte,
                    new_end_byte: edit.new_end_byte,
                    start_position: point(edit.start_position),
                    old_end_position: point(edit.old_end_position),
                    new_end_position: point(edit.new_end_position),
                });
            }
        }
        if let Some(parser) = &mut self.parser {
            self.tree = parser.parse(source, self.tree.as_ref());
        }
    }

    pub fn has_error(&self) -> bool {
        self.tree
            .as_ref()
            .is_some_and(|tree| tree.root_node().has_error())
    }

    /// Return semantic-enough spans for the visible byte range. Tree-sitter remains the source of
    /// syntax boundaries; the renderer maps these stable categories to the selected editor theme.
    pub fn highlight_spans(&self, visible: Range<usize>) -> Vec<HighlightSpan> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let mut spans = Vec::new();
        collect_spans(tree.root_node(), &visible, &mut spans);
        spans.sort_by_key(|span| (span.byte_range.start, span.byte_range.end));
        spans
    }
}

fn point(value: crate::TextPoint) -> Point {
    Point::new(value.row, value.column)
}

fn collect_spans(
    node: tree_sitter::Node<'_>,
    visible: &Range<usize>,
    spans: &mut Vec<HighlightSpan>,
) {
    let range = node.byte_range();
    if range.start >= visible.end || range.end <= visible.start {
        return;
    }
    let classified = classify(node.kind(), node.is_named());
    if node.child_count() == 0
        || matches!(
            classified,
            Some(
                HighlightKind::Comment
                    | HighlightKind::String
                    | HighlightKind::Number
                    | HighlightKind::Function
                    | HighlightKind::Type
                    | HighlightKind::Property
            )
        )
    {
        if let Some(kind) = classified {
            spans.push(HighlightSpan {
                byte_range: range.start.max(visible.start)..range.end.min(visible.end),
                kind,
            });
        }
        return;
    }
    for index in 0..node.child_count() {
        if let Some(child) = node.child(index) {
            collect_spans(child, visible, spans);
        }
    }
}

fn classify(kind: &str, named: bool) -> Option<HighlightKind> {
    let lower = kind.to_ascii_lowercase();
    let category = if lower.contains("comment") {
        HighlightKind::Comment
    } else if lower.contains("string") || lower.contains("char") {
        HighlightKind::String
    } else if lower.contains("number") || lower.contains("integer") || lower.contains("float") {
        HighlightKind::Number
    } else if lower.contains("function") || lower.contains("method") {
        HighlightKind::Function
    } else if lower.contains("type") || lower.contains("class") || lower.contains("struct") {
        HighlightKind::Type
    } else if lower.contains("property") || lower.contains("field") {
        HighlightKind::Property
    } else if matches!(
        lower.as_str(),
        "if" | "else"
            | "for"
            | "while"
            | "return"
            | "fn"
            | "let"
            | "const"
            | "use"
            | "mod"
            | "pub"
            | "impl"
            | "match"
            | "async"
            | "await"
            | "class"
            | "import"
            | "from"
            | "def"
            | "try"
            | "catch"
            | "throw"
            | "new"
            | "package"
    ) {
        HighlightKind::Keyword
    } else if !named {
        HighlightKind::Punctuation
    } else {
        HighlightKind::Other
    };
    Some(category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_required_languages() {
        let cases = [
            ("x.ts", SyntaxLanguage::TypeScript),
            ("x.tsx", SyntaxLanguage::Tsx),
            ("x.js", SyntaxLanguage::JavaScript),
            ("x.rs", SyntaxLanguage::Rust),
            ("x.py", SyntaxLanguage::Python),
            ("x.go", SyntaxLanguage::Go),
            ("x.c", SyntaxLanguage::C),
            ("x.cpp", SyntaxLanguage::Cpp),
            ("x.java", SyntaxLanguage::Java),
            ("x.html", SyntaxLanguage::Html),
            ("x.css", SyntaxLanguage::Css),
            ("x.json", SyntaxLanguage::Json),
            ("x.md", SyntaxLanguage::Markdown),
        ];
        for (path, expected) in cases {
            assert_eq!(SyntaxLanguage::detect(path), expected, "{path}");
        }
    }

    #[test]
    fn parses_rust_and_restricts_to_viewport() {
        let source = "// hi\nfn main() { let x = \"value\"; }\n";
        let mut doc = SyntaxDocument::new(SyntaxLanguage::Rust, source);
        assert!(!doc.has_error());
        let spans = doc.highlight_spans(0..7);
        assert!(spans.iter().all(|s| s.byte_range.end <= 7));
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Comment));
        doc.reparse("fn main() {}\n");
        assert!(!doc.has_error());
    }

    #[test]
    fn plain_text_has_no_tree() {
        let doc = SyntaxDocument::new(SyntaxLanguage::PlainText, "hello");
        assert!(doc.highlight_spans(0..5).is_empty());
    }

    #[test]
    fn applies_multiple_incremental_edits_before_reparse() {
        let mut buffer = crate::EditorBuffer::new("fn main() {\n}\n");
        let mut doc = SyntaxDocument::new(SyntaxLanguage::Rust, &buffer.text());
        buffer.set_cursor(12).unwrap();
        buffer.insert("let x = 1;\n").unwrap();
        buffer.insert("let y = x + 1;\n").unwrap();
        doc.reparse_incremental(&buffer.text(), &buffer.take_pending_edits());
        assert!(!doc.has_error());
        assert!(!doc.highlight_spans(0..buffer.text().len()).is_empty());
    }
}
