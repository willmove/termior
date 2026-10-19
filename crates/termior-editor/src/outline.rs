//! Tree-sitter symbol outline: definitions, signatures, and identifier references (FR-ACTX-05).
//!
//! The repo map in `termior-ai` keys these outlines by file content hash. Extraction never reads
//! the filesystem and never fails: a source with syntax errors still yields whatever definitions
//! tree-sitter recovered, and languages without a definition table return `None` so callers can
//! degrade to path or lexical matching.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Tree};

use crate::SyntaxLanguage;

/// Upper bound on distinct reference names kept per file. The most frequent names win.
pub const MAX_REFERENCES_PER_FILE: usize = 512;
const MAX_SIGNATURE_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Enum,
    Interface,
    Trait,
    TypeAlias,
    Constant,
    Module,
    Macro,
}

impl SymbolKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "fn",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::TypeAlias => "type",
            Self::Constant => "const",
            Self::Module => "mod",
            Self::Macro => "macro",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// Enclosing impl/class/namespace name, when the definition is nested in one.
    pub container: Option<String>,
    /// Header text up to the body, whitespace-collapsed and capped.
    pub signature: String,
    /// 1-based inclusive line range.
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOutline {
    pub language: SyntaxLanguage,
    pub symbols: Vec<OutlineSymbol>,
    /// Distinct identifier names used in the file but not defined by it, sorted.
    pub references: Vec<String>,
    /// The tree contained error nodes; the outline is best-effort.
    pub has_error: bool,
}

impl SyntaxLanguage {
    /// Whether [`OutlineExtractor`] has a definition table for this language.
    pub fn has_outline(self) -> bool {
        !matches!(
            self,
            Self::Html | Self::Css | Self::Json | Self::Markdown | Self::PlainText
        )
    }
}

/// Reuses one parser per language. Not `Sync`; keep one per indexing thread.
#[derive(Default)]
pub struct OutlineExtractor {
    parsers: Vec<(SyntaxLanguage, Parser)>,
}

impl std::fmt::Debug for OutlineExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlineExtractor")
            .field("languages", &self.parsers.len())
            .finish()
    }
}

impl OutlineExtractor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extract(&mut self, language: SyntaxLanguage, source: &str) -> Option<FileOutline> {
        if !language.has_outline() {
            return None;
        }
        let tree = self.parse(language, source)?;
        Some(outline_from_tree(language, &tree, source))
    }

    fn parse(&mut self, language: SyntaxLanguage, source: &str) -> Option<Tree> {
        if !self.parsers.iter().any(|(lang, _)| *lang == language) {
            let mut parser = Parser::new();
            parser
                .set_language(&language.tree_sitter_language()?)
                .ok()?;
            self.parsers.push((language, parser));
        }
        let (_, parser) = self
            .parsers
            .iter_mut()
            .find(|(lang, _)| *lang == language)?;
        parser.parse(source, None)
    }
}

fn outline_from_tree(language: SyntaxLanguage, tree: &Tree, source: &str) -> FileOutline {
    let bytes = source.as_bytes();
    let mut symbols = Vec::new();
    let mut name_ranges = HashSet::new();
    let mut reference_counts = BTreeMap::<&str, usize>::new();
    let mut stack = vec![tree.root_node()];
    let mut cursor = tree.walk();
    while let Some(node) = stack.pop() {
        if let Some((kind, name_node)) = definition(language, node) {
            if let Ok(name) = name_node.utf8_text(bytes) {
                let name = last_segment(name);
                if !name.is_empty() {
                    name_ranges.insert(name_node.byte_range());
                    symbols.push(OutlineSymbol {
                        name: name.to_owned(),
                        kind: refine_kind(kind, node, name_node),
                        container: container_name(language, node, bytes),
                        signature: signature(node, source),
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                    });
                }
            }
        }
        if node.child_count() == 0 {
            if is_reference_kind(node.kind()) && !name_ranges.contains(&node.byte_range()) {
                if let Ok(text) = node.utf8_text(bytes) {
                    if text.len() >= 2 {
                        *reference_counts.entry(text).or_default() += 1;
                    }
                }
            }
            continue;
        }
        let children = node.children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    symbols.sort_by_key(|symbol| (symbol.start_line, symbol.end_line));
    let defined = symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect::<HashSet<_>>();
    let mut references = reference_counts
        .into_iter()
        .filter(|(name, _)| !defined.contains(name))
        .collect::<Vec<_>>();
    if references.len() > MAX_REFERENCES_PER_FILE {
        references.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        references.truncate(MAX_REFERENCES_PER_FILE);
    }
    let mut references = references
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect::<Vec<_>>();
    references.sort();
    FileOutline {
        language,
        symbols,
        references,
        has_error: tree.root_node().has_error(),
    }
}

fn definition(language: SyntaxLanguage, node: Node<'_>) -> Option<(SymbolKind, Node<'_>)> {
    use SymbolKind::*;
    let named = |kind| node.child_by_field_name("name").map(|name| (kind, name));
    match language {
        SyntaxLanguage::Rust => match node.kind() {
            "function_item" | "function_signature_item" => named(Function),
            "struct_item" | "union_item" => named(Struct),
            "enum_item" => named(Enum),
            "trait_item" => named(Trait),
            "type_item" => named(TypeAlias),
            "const_item" | "static_item" => named(Constant),
            "mod_item" => named(Module),
            "macro_definition" => named(Macro),
            _ => None,
        },
        SyntaxLanguage::Python => match node.kind() {
            "function_definition" => named(Function),
            "class_definition" => named(Class),
            _ => None,
        },
        SyntaxLanguage::JavaScript | SyntaxLanguage::TypeScript | SyntaxLanguage::Tsx => match node
            .kind()
        {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                named(Function)
            }
            "class_declaration" | "abstract_class_declaration" => named(Class),
            "method_definition" | "method_signature" | "abstract_method_signature" => named(Method),
            "interface_declaration" => named(Interface),
            "type_alias_declaration" => named(TypeAlias),
            "enum_declaration" => named(Enum),
            "internal_module" | "module" => named(Module),
            "variable_declarator" => {
                let value = node.child_by_field_name("value")?;
                matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function" | "generator_function"
                )
                .then_some(())?;
                named(Function)
            }
            _ => None,
        },
        SyntaxLanguage::Go => match node.kind() {
            "function_declaration" => named(Function),
            "method_declaration" => named(Method),
            "type_spec" | "type_alias" => {
                let kind = match node.child_by_field_name("type").map(|ty| ty.kind()) {
                    Some("struct_type") => Struct,
                    Some("interface_type") => Interface,
                    _ => TypeAlias,
                };
                named(kind)
            }
            "const_spec" => named(Constant),
            _ => None,
        },
        SyntaxLanguage::C | SyntaxLanguage::Cpp => match node.kind() {
            "function_definition" => {
                declarator_name(node.child_by_field_name("declarator")?).map(|n| (Function, n))
            }
            "declaration" | "field_declaration" => {
                let declarator = node.child_by_field_name("declarator")?;
                has_function_declarator(declarator).then_some(())?;
                declarator_name(declarator).map(|n| (Function, n))
            }
            "struct_specifier" | "union_specifier" => with_body(node, Struct),
            "enum_specifier" => with_body(node, Enum),
            "class_specifier" => with_body(node, Class),
            "type_definition" => {
                declarator_name(node.child_by_field_name("declarator")?).map(|n| (TypeAlias, n))
            }
            "alias_declaration" => named(TypeAlias),
            "namespace_definition" => named(Module),
            "preproc_def" | "preproc_function_def" => named(Macro),
            _ => None,
        },
        SyntaxLanguage::Java => match node.kind() {
            "class_declaration" | "record_declaration" => named(Class),
            "interface_declaration" | "annotation_type_declaration" => named(Interface),
            "enum_declaration" => named(Enum),
            "method_declaration" | "constructor_declaration" => named(Method),
            _ => None,
        },
        SyntaxLanguage::Html
        | SyntaxLanguage::Css
        | SyntaxLanguage::Json
        | SyntaxLanguage::Markdown
        | SyntaxLanguage::PlainText => None,
    }
}

/// C/C++ `struct foo *p` is a use, not a definition; only specifiers with a body define a type.
fn with_body(node: Node<'_>, kind: SymbolKind) -> Option<(SymbolKind, Node<'_>)> {
    node.child_by_field_name("body")?;
    node.child_by_field_name("name").map(|name| (kind, name))
}

fn declarator_name(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        match node.kind() {
            "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
            | "operator_name" => return Some(node),
            "qualified_identifier" => {
                node = node.child_by_field_name("name")?;
            }
            _ => {
                node = node.child_by_field_name("declarator")?;
            }
        }
    }
}

fn has_function_declarator(mut node: Node<'_>) -> bool {
    loop {
        if node.kind() == "function_declarator" {
            return true;
        }
        match node.child_by_field_name("declarator") {
            Some(next) => node = next,
            None => return false,
        }
    }
}

fn refine_kind(kind: SymbolKind, definition: Node<'_>, name: Node<'_>) -> SymbolKind {
    if kind != SymbolKind::Function {
        return kind;
    }
    if name.kind() == "field_identifier"
        || name.parent().map(|p| p.kind()) == Some("qualified_identifier")
    {
        return SymbolKind::Method;
    }
    let mut current = definition.parent();
    while let Some(node) = current {
        match node.kind() {
            "impl_item"
            | "trait_item"
            | "class_definition"
            | "class_body"
            | "class_declaration"
            | "field_declaration_list"
            | "interface_body" => return SymbolKind::Method,
            "function_item"
            | "function_definition"
            | "function_declaration"
            | "source_file"
            | "module"
            | "program"
            | "translation_unit" => return kind,
            _ => current = node.parent(),
        }
    }
    kind
}

fn container_name(language: SyntaxLanguage, definition: Node<'_>, source: &[u8]) -> Option<String> {
    let mut current = definition.parent();
    while let Some(node) = current {
        let field = match (language, node.kind()) {
            (SyntaxLanguage::Rust, "impl_item") => Some("type"),
            (SyntaxLanguage::Rust, "trait_item" | "mod_item") => Some("name"),
            (SyntaxLanguage::Python, "class_definition") => Some("name"),
            (
                SyntaxLanguage::JavaScript | SyntaxLanguage::TypeScript | SyntaxLanguage::Tsx,
                "class_declaration" | "abstract_class_declaration" | "interface_declaration",
            ) => Some("name"),
            (
                SyntaxLanguage::C | SyntaxLanguage::Cpp,
                "class_specifier" | "struct_specifier" | "namespace_definition",
            ) => Some("name"),
            (
                SyntaxLanguage::Java,
                "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration",
            ) => Some("name"),
            (SyntaxLanguage::Go, "method_declaration") => None,
            _ => None,
        };
        if let Some(text) = field
            .and_then(|field| node.child_by_field_name(field))
            .and_then(|name| name.utf8_text(source).ok())
        {
            return Some(collapse_whitespace(text));
        }
        current = node.parent();
    }
    if language == SyntaxLanguage::Go && definition.kind() == "method_declaration" {
        let receiver = definition.child_by_field_name("receiver")?;
        let text = receiver.utf8_text(source).ok()?;
        let ty = text
            .trim_matches(|ch| ch == '(' || ch == ')')
            .split_whitespace()
            .last()?
            .trim_start_matches('*');
        return Some(ty.to_owned());
    }
    None
}

fn signature(node: Node<'_>, source: &str) -> String {
    let start = node.start_byte();
    let end = body_start(node).unwrap_or_else(|| {
        let text = &source[start..node.end_byte()];
        start + text.find('\n').unwrap_or(text.len())
    });
    let header = source.get(start..end).unwrap_or_default();
    let collapsed = collapse_whitespace(header);
    let trimmed = collapsed
        .trim_end_matches(|ch: char| ch == '{' || ch == ':' || ch.is_whitespace())
        .to_owned();
    if trimmed.chars().count() <= MAX_SIGNATURE_CHARS {
        trimmed
    } else {
        let mut cut = trimmed
            .chars()
            .take(MAX_SIGNATURE_CHARS)
            .collect::<String>();
        cut.push('…');
        cut
    }
}

fn body_start(node: Node<'_>) -> Option<usize> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body.start_byte());
    }
    let value = node.child_by_field_name("value")?;
    value
        .child_by_field_name("body")
        .map(|body| body.start_byte())
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name).trim()
}

fn is_reference_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
            | "namespace_identifier"
            | "package_identifier"
            | "statement_identifier"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline(language: SyntaxLanguage, source: &str) -> FileOutline {
        OutlineExtractor::new().extract(language, source).unwrap()
    }

    fn names(outline: &FileOutline) -> Vec<(String, SymbolKind)> {
        outline
            .symbols
            .iter()
            .map(|symbol| (symbol.name.clone(), symbol.kind))
            .collect()
    }

    fn has(names: &[(String, SymbolKind)], name: &str, kind: SymbolKind) -> bool {
        names.iter().any(|(n, k)| n == name && *k == kind)
    }

    #[test]
    fn rust_definitions_signatures_and_references() {
        let source = "pub struct AgentRuntime { tools: ToolRegistry }\n\
                      impl AgentRuntime {\n    pub fn drive(&mut self, steps: u32) -> Result<(), Error> {\n        helper(steps)\n    }\n}\n\
                      pub trait Provider { fn send(&self); }\n\
                      pub enum Mode { A }\nconst LIMIT: usize = 4;\nmacro_rules! m { () => {} }\nmod inner {}\n\
                      fn helper(n: u32) -> Result<(), Error> { Ok(()) }\n";
        let outline = outline(SyntaxLanguage::Rust, source);
        assert!(!outline.has_error);
        let names = names(&outline);
        for expected in [
            ("AgentRuntime", SymbolKind::Struct),
            ("drive", SymbolKind::Method),
            ("Provider", SymbolKind::Trait),
            ("send", SymbolKind::Method),
            ("Mode", SymbolKind::Enum),
            ("LIMIT", SymbolKind::Constant),
            ("m", SymbolKind::Macro),
            ("inner", SymbolKind::Module),
            ("helper", SymbolKind::Function),
        ] {
            assert!(
                has(&names, expected.0, expected.1),
                "{expected:?} missing from {names:?}"
            );
        }
        let drive = outline.symbols.iter().find(|s| s.name == "drive").unwrap();
        assert_eq!(
            drive.signature,
            "pub fn drive(&mut self, steps: u32) -> Result<(), Error>"
        );
        assert_eq!(drive.container.as_deref(), Some("AgentRuntime"));
        assert_eq!((drive.start_line, drive.end_line), (3, 5));
        assert!(outline.references.contains(&"ToolRegistry".to_owned()));
        assert!(outline.references.contains(&"Error".to_owned()));
        assert!(
            !outline.references.contains(&"helper".to_owned()),
            "own definitions are not references"
        );
    }

    #[test]
    fn python_classes_and_methods() {
        let source = "class Store:\n    def load(self, path):\n        return parse(path)\n\n@cache\ndef parse(path: str) -> dict:\n    return {}\n";
        let outline = outline(SyntaxLanguage::Python, source);
        let names = names(&outline);
        assert!(has(&names, "Store", SymbolKind::Class));
        assert!(has(&names, "load", SymbolKind::Method));
        assert!(has(&names, "parse", SymbolKind::Function));
        let parse = outline.symbols.iter().find(|s| s.name == "parse").unwrap();
        assert_eq!(parse.signature, "def parse(path: str) -> dict");
    }

    #[test]
    fn javascript_and_typescript_definitions() {
        let js = "export function render(view) { return draw(view); }\nclass Panel { open() {} }\nconst onClick = (event) => handle(event);\n";
        let names_js = names(&outline(SyntaxLanguage::JavaScript, js));
        assert!(has(&names_js, "render", SymbolKind::Function));
        assert!(has(&names_js, "Panel", SymbolKind::Class));
        assert!(has(&names_js, "open", SymbolKind::Method));
        assert!(has(&names_js, "onClick", SymbolKind::Function));

        let ts = "interface Shape { area(): number }\ntype Id = string;\nenum Color { Red }\nexport abstract class Base { abstract run(): void }\n";
        let outline_ts = outline(SyntaxLanguage::TypeScript, ts);
        let names_ts = names(&outline_ts);
        assert!(has(&names_ts, "Shape", SymbolKind::Interface));
        assert!(has(&names_ts, "area", SymbolKind::Method));
        assert!(has(&names_ts, "Id", SymbolKind::TypeAlias));
        assert!(has(&names_ts, "Color", SymbolKind::Enum));
        assert!(has(&names_ts, "Base", SymbolKind::Class));
        let tsx = outline(
            SyntaxLanguage::Tsx,
            "export function App() { return <div/>; }\n",
        );
        assert_eq!(names(&tsx), vec![("App".to_owned(), SymbolKind::Function)]);
    }

    #[test]
    fn go_c_cpp_and_java_definitions() {
        let go = "package main\ntype Server struct { addr string }\ntype Handler interface { Serve() }\nfunc (s *Server) Start() error { return listen(s.addr) }\nfunc listen(a string) error { return nil }\nconst Port = 80\n";
        let outline_go = outline(SyntaxLanguage::Go, go);
        let names_go = names(&outline_go);
        assert!(has(&names_go, "Server", SymbolKind::Struct));
        assert!(has(&names_go, "Handler", SymbolKind::Interface));
        assert!(has(&names_go, "Start", SymbolKind::Method));
        assert!(has(&names_go, "listen", SymbolKind::Function));
        assert!(has(&names_go, "Port", SymbolKind::Constant));
        let start = outline_go
            .symbols
            .iter()
            .find(|s| s.name == "Start")
            .unwrap();
        assert_eq!(start.container.as_deref(), Some("Server"));

        let c = "#define MAX 4\nstruct point { int x; };\ntypedef struct point point_t;\nint add(int a, int b) { return a + b; }\nint *alloc(void);\nstruct point *origin;\n";
        let names_c = names(&outline(SyntaxLanguage::C, c));
        assert!(has(&names_c, "MAX", SymbolKind::Macro));
        assert!(has(&names_c, "point", SymbolKind::Struct));
        assert!(has(&names_c, "point_t", SymbolKind::TypeAlias));
        assert!(has(&names_c, "add", SymbolKind::Function));
        assert!(has(&names_c, "alloc", SymbolKind::Function));
        assert!(!has(&names_c, "origin", SymbolKind::Function));
        assert_eq!(
            names_c.iter().filter(|(name, _)| name == "point").count(),
            1,
            "a pointer to a struct is not a definition: {names_c:?}"
        );

        let cpp = "namespace net { class Socket { public: void close(); }; }\nvoid net::Socket::close() {}\n";
        let names_cpp = names(&outline(SyntaxLanguage::Cpp, cpp));
        assert!(has(&names_cpp, "net", SymbolKind::Module));
        assert!(has(&names_cpp, "Socket", SymbolKind::Class));
        assert!(has(&names_cpp, "close", SymbolKind::Method));

        let java = "class Account { Account() {} void deposit(int amount) { log(amount); } }\ninterface Ledger {}\nenum Kind { A }\n";
        let names_java = names(&outline(SyntaxLanguage::Java, java));
        assert!(has(&names_java, "Account", SymbolKind::Class));
        assert!(has(&names_java, "deposit", SymbolKind::Method));
        assert!(has(&names_java, "Ledger", SymbolKind::Interface));
        assert!(has(&names_java, "Kind", SymbolKind::Enum));
    }

    #[test]
    fn broken_source_still_yields_recovered_definitions() {
        let outline = outline(
            SyntaxLanguage::Rust,
            "fn ok() {}\nfn broken( {\nstruct After;\n",
        );
        assert!(outline.has_error);
        assert!(outline.symbols.iter().any(|symbol| symbol.name == "ok"));
    }

    #[test]
    fn languages_without_a_definition_table_return_none() {
        let mut extractor = OutlineExtractor::new();
        for language in [
            SyntaxLanguage::Markdown,
            SyntaxLanguage::Json,
            SyntaxLanguage::Html,
            SyntaxLanguage::Css,
            SyntaxLanguage::PlainText,
        ] {
            assert!(extractor.extract(language, "x").is_none(), "{language:?}");
        }
    }

    #[test]
    fn long_signatures_are_capped_and_deep_nesting_does_not_recurse() {
        let params = (0..80)
            .map(|i| format!("a{i}: u32"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("fn wide({params}) {{}}\n");
        let outline = outline(SyntaxLanguage::Rust, &source);
        let wide = &outline.symbols[0];
        assert!(wide.signature.chars().count() <= MAX_SIGNATURE_CHARS + 1);
        assert!(wide.signature.ends_with('…'));

        let deep = format!(
            "fn deep() {{ {}1{} }}\n",
            "(".repeat(20_000),
            ")".repeat(20_000)
        );
        let outline = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || OutlineExtractor::new().extract(SyntaxLanguage::Rust, &deep))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert!(outline.symbols.iter().any(|symbol| symbol.name == "deep"));
    }

    #[test]
    fn reference_list_is_bounded() {
        let source = (0..(MAX_REFERENCES_PER_FILE + 50))
            .map(|i| format!("let _ = name_{i};"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!("fn many() {{\n{source}\n}}\n");
        let outline = outline(SyntaxLanguage::Rust, &source);
        assert_eq!(outline.references.len(), MAX_REFERENCES_PER_FILE);
    }
}
