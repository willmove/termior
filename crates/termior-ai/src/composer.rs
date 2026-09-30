//! Composer attachments, @path chips, #snippets and application TODOs (FR-AGENT-01..04,
//! FR-SESS-04).

use crate::tools::{ToolError, ToolRegistry};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use termior_security::deny_list::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSource {
    Terminal,
    Editor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    Image {
        name: String,
        mime: String,
        data_base64: String,
    },
    File {
        path: PathBuf,
    },
    Selection {
        source: AttachmentSource,
        label: String,
        text: String,
    },
}

impl Attachment {
    pub fn image(name: impl Into<String>, mime: impl Into<String>, bytes: &[u8]) -> Self {
        Self::Image {
            name: name.into(),
            mime: mime.into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    pub fn chip_label(&self) -> String {
        match self {
            Self::Image { name, .. } => name.clone(),
            Self::File { path } => format!("@{}", path.to_string_lossy().replace('\\', "/")),
            Self::Selection { label, .. } => label.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComposerDraft {
    pub input: String,
    pub attachments: Vec<Attachment>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerPayload {
    pub text: String,
    /// Image payloads stay structured for multimodal providers.
    pub images: Vec<(String, String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposerError {
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("failed to read attachment {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl ComposerDraft {
    pub fn attach_image(&mut self, name: impl Into<String>, mime: impl Into<String>, bytes: &[u8]) {
        self.attachments.push(Attachment::image(name, mime, bytes));
    }

    pub fn attach_file(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if !self
            .attachments
            .iter()
            .any(|item| matches!(item, Attachment::File { path: existing } if existing == &path))
        {
            self.attachments.push(Attachment::File { path });
        }
    }

    pub fn attach_selection(
        &mut self,
        source: AttachmentSource,
        label: impl Into<String>,
        text: impl Into<String>,
    ) {
        self.attachments.push(Attachment::Selection {
            source,
            label: label.into(),
            text: text.into(),
        });
    }

    pub fn remove_attachment(&mut self, index: usize) -> Option<Attachment> {
        (index < self.attachments.len()).then(|| self.attachments.remove(index))
    }

    pub fn build_payload(&self, tools: &ToolRegistry) -> Result<ComposerPayload, ComposerError> {
        let mut text = self.input.clone();
        let mut images = Vec::new();
        for attachment in &self.attachments {
            match attachment {
                Attachment::File { path } => {
                    let normalized = path.to_string_lossy().replace('\\', "/");
                    tools.check_path_access("read_file", &normalized, Direction::Read)?;
                    let content =
                        std::fs::read_to_string(path).map_err(|source| ComposerError::Read {
                            path: path.clone(),
                            source,
                        })?;
                    text.push_str(&format!(
                        "\n\n<file path=\"{}\">\n{}\n</file>",
                        escape_attribute(&normalized),
                        content
                    ));
                }
                Attachment::Selection {
                    source,
                    label,
                    text: selection,
                } => {
                    text.push_str(&format!(
                        "\n\n<selection source=\"{}\" label=\"{}\">\n{}\n</selection>",
                        match source {
                            AttachmentSource::Terminal => "terminal",
                            AttachmentSource::Editor => "editor",
                        },
                        escape_attribute(label),
                        selection
                    ));
                }
                Attachment::Image {
                    name,
                    mime,
                    data_base64,
                } => images.push((name.clone(), mime.clone(), data_base64.clone())),
            }
        }
        Ok(ComposerPayload { text, images })
    }
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    pub handle: String,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetStore {
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

/// 片段 handle 非法（FR-AGENT-04）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnippetError {
    #[error("usage: /snippet <handle> <text>")]
    MissingBody,
    #[error("snippet handles use letters, digits, '-' or '_': {0}")]
    InvalidHandle(String),
}

impl SnippetStore {
    pub fn upsert(&mut self, handle: impl Into<String>, body: impl Into<String>) {
        let handle = handle.into().trim_start_matches('#').to_owned();
        let body = body.into();
        if let Some(snippet) = self.snippets.iter_mut().find(|item| item.handle == handle) {
            snippet.body = body;
        } else {
            self.snippets.push(Snippet { handle, body });
        }
    }

    /// 解析 `/snippet <handle> <text>` 的参数并保存；返回规范化后的 handle。
    pub fn define(&mut self, args: &str) -> Result<String, SnippetError> {
        let args = args.trim();
        let (handle, body) = args
            .split_once(char::is_whitespace)
            .ok_or(SnippetError::MissingBody)?;
        let handle = handle.trim_start_matches('#');
        if !is_valid_handle(handle) {
            return Err(SnippetError::InvalidHandle(handle.to_owned()));
        }
        let body = body.trim();
        if body.is_empty() {
            return Err(SnippetError::MissingBody);
        }
        self.upsert(handle, body);
        Ok(handle.to_owned())
    }

    pub fn remove(&mut self, handle: &str) -> bool {
        let before = self.snippets.len();
        self.snippets.retain(|item| item.handle != handle);
        self.snippets.len() < before
    }

    /// `#` 补全候选：handle 前缀匹配优先，其次子串匹配；同档保持保存顺序。
    pub fn suggest(&self, query: &str) -> Vec<&Snippet> {
        let query = query.to_lowercase();
        let (mut prefix, substring): (Vec<_>, Vec<_>) = self
            .snippets
            .iter()
            .filter(|item| item.handle.to_lowercase().contains(&query))
            .partition(|item| item.handle.to_lowercase().starts_with(&query));
        prefix.extend(substring);
        prefix
    }

    /// 把 `#handle` 词元替换为片段正文，其余文本（含换行与缩进）原样保留。
    pub fn expand(&self, input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        let mut rest = input;
        while !rest.is_empty() {
            let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (word, tail) = rest.split_at(word_end);
            let body = word
                .strip_prefix('#')
                .and_then(|handle| self.snippets.iter().find(|item| item.handle == handle))
                .map(|snippet| snippet.body.as_str());
            output.push_str(body.unwrap_or(word));
            let space_end = tail
                .find(|character: char| !character.is_whitespace())
                .unwrap_or(tail.len());
            output.push_str(&tail[..space_end]);
            rest = &tail[space_end..];
        }
        output
    }
}

fn is_valid_handle(handle: &str) -> bool {
    !handle.is_empty()
        && handle
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
}

/// 光标前的最后一个词元若以 `#` 开头，返回其后的查询串（可为空）。
pub fn snippet_query(input: &str, cursor: usize) -> Option<&str> {
    let end = input
        .char_indices()
        .nth(cursor)
        .map_or(input.len(), |(byte, _)| byte);
    input[..end]
        .rsplit(char::is_whitespace)
        .next()?
        .strip_prefix('#')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoStore {
    #[serde(default)]
    pub items: Vec<TodoItem>,
}

/// 单次 `todo_write` 允许的最大条目数（FR-SESS-04）。
pub const MAX_TODO_ITEMS: usize = 100;
const MAX_TODO_TEXT_CHARS: usize = 2_000;

/// `todo_write` 的一条输入；缺省 `id` 时由存储分配。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TodoDraft {
    #[serde(default)]
    pub id: Option<String>,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TodoError {
    #[error("at most {MAX_TODO_ITEMS} todo items are allowed")]
    TooMany,
    #[error("todo text must be 1..={MAX_TODO_TEXT_CHARS} characters")]
    InvalidText,
    #[error("duplicate todo id: {0}")]
    DuplicateId(String),
    #[error("todo text looks like a secret and was not stored")]
    SecretLike,
}

impl TodoStore {
    pub fn add(&mut self, id: impl Into<String>, text: impl Into<String>) {
        self.items.push(TodoItem {
            id: id.into(),
            text: text.into(),
            done: false,
        });
    }

    pub fn set_done(&mut self, id: &str, done: bool) -> bool {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        item.done = done;
        true
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.id != id);
        self.items.len() < before
    }

    /// 以整表替换语义应用 `todo_write`：全部校验通过才生效，重放同一输入结果相同。
    pub fn replace_all(&mut self, drafts: Vec<TodoDraft>) -> Result<(), TodoError> {
        if drafts.len() > MAX_TODO_ITEMS {
            return Err(TodoError::TooMany);
        }
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
        for draft in &drafts {
            let length = draft.text.trim().chars().count();
            if length == 0 || length > MAX_TODO_TEXT_CHARS {
                return Err(TodoError::InvalidText);
            }
            if termior_security::assert_no_secret_fields(&draft.text).is_err() {
                return Err(TodoError::SecretLike);
            }
            if let Some(id) = &draft.id {
                if !used.insert(id.clone()) {
                    return Err(TodoError::DuplicateId(id.clone()));
                }
            }
        }
        let mut next = 1usize;
        self.items = drafts
            .into_iter()
            .map(|draft| {
                let id = draft.id.unwrap_or_else(|| loop {
                    let candidate = format!("todo-{next}");
                    next += 1;
                    if used.insert(candidate.clone()) {
                        break candidate;
                    }
                });
                TodoItem {
                    id,
                    text: draft.text.trim().to_owned(),
                    done: draft.done,
                }
            })
            .collect();
        Ok(())
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|item| !item.done).count()
    }
}

pub fn normalized_workspace_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_security::workspace::WorkspaceAuthRegistry;

    #[test]
    fn chips_do_not_pollute_input_until_submission() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "file body").unwrap();
        let root = dir.path().to_string_lossy().replace('\\', "/");
        let tools = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([root]));
        let mut draft = ComposerDraft {
            input: "question".into(),
            ..Default::default()
        };
        draft.attach_file(&file);
        draft.attach_selection(AttachmentSource::Editor, "a.txt:1", "selected");
        assert_eq!(draft.input, "question");
        let payload = draft.build_payload(&tools).unwrap();
        assert!(payload.text.contains("<file"));
        assert!(payload.text.contains("<selection source=\"editor\""));
    }

    #[test]
    fn secret_attachment_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".env");
        std::fs::write(&file, "SECRET=x").unwrap();
        let root = dir.path().to_string_lossy().replace('\\', "/");
        let tools = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([root]));
        let mut draft = ComposerDraft::default();
        draft.attach_file(file);
        assert!(matches!(
            draft.build_payload(&tools),
            Err(ComposerError::Tool(_))
        ));
    }

    #[test]
    fn snippets_and_todos_work() {
        let mut snippets = SnippetStore::default();
        snippets.upsert("review", "review this diff");
        assert_eq!(snippets.expand("please #review"), "please review this diff");
        assert_eq!(
            snippets.expand("line one\n  #review\tthen #unknown"),
            "line one\n  review this diff\tthen #unknown",
            "expansion must keep the user's newlines and indentation"
        );
        let mut todos = TodoStore::default();
        todos.add("1", "ship");
        assert!(todos.set_done("1", true));
        assert!(todos.items[0].done);
    }

    #[test]
    fn snippet_definitions_are_validated() {
        let mut snippets = SnippetStore::default();
        assert_eq!(
            snippets.define("#fix-it  please fix\nthis"),
            Ok("fix-it".into())
        );
        assert_eq!(snippets.snippets[0].body, "please fix\nthis");
        assert_eq!(snippets.define("fix-it"), Err(SnippetError::MissingBody));
        assert_eq!(snippets.define(""), Err(SnippetError::MissingBody));
        assert!(matches!(
            snippets.define("bad/handle text"),
            Err(SnippetError::InvalidHandle(_))
        ));
        assert_eq!(snippets.define("fix-it replaced"), Ok("fix-it".into()));
        assert_eq!(snippets.snippets.len(), 1);
        assert!(snippets.remove("fix-it"));
        assert!(!snippets.remove("fix-it"));
    }

    #[test]
    fn snippet_suggestions_rank_prefix_first() {
        let mut snippets = SnippetStore::default();
        snippets.upsert("prereview", "a");
        snippets.upsert("review", "b");
        snippets.upsert("other", "c");
        let handles: Vec<_> = snippets
            .suggest("rev")
            .iter()
            .map(|s| s.handle.as_str())
            .collect();
        assert_eq!(handles, ["review", "prereview"]);
        assert_eq!(snippets.suggest("").len(), 3);
    }

    #[test]
    fn snippet_query_tracks_the_token_before_the_cursor() {
        assert_eq!(snippet_query("please #rev", 11), Some("rev"));
        assert_eq!(snippet_query("#", 1), Some(""));
        assert_eq!(snippet_query("please #rev now", 15), None);
        assert_eq!(snippet_query("issue#12", 8), None);
        assert_eq!(snippet_query("中文 #片段", 5), Some("片"));
    }

    #[test]
    fn todo_replace_all_validates_before_mutating() {
        let mut todos = TodoStore::default();
        todos.add("keep", "existing");
        let draft = |id: Option<&str>, text: &str| TodoDraft {
            id: id.map(str::to_owned),
            text: text.into(),
            done: false,
        };
        assert_eq!(
            todos.replace_all(vec![draft(Some("a"), "x"), draft(Some("a"), "y")]),
            Err(TodoError::DuplicateId("a".into()))
        );
        assert_eq!(
            todos.replace_all(vec![draft(None, "  ")]),
            Err(TodoError::InvalidText)
        );
        assert_eq!(
            todos.replace_all(vec![draft(
                None,
                "use sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123"
            )]),
            Err(TodoError::SecretLike)
        );
        assert_eq!(
            todos.replace_all(vec![draft(None, "x"); MAX_TODO_ITEMS + 1]),
            Err(TodoError::TooMany)
        );
        assert_eq!(todos.items[0].id, "keep", "failed writes must not mutate");

        todos
            .replace_all(vec![
                draft(Some("todo-1"), " write tests "),
                draft(None, "fix bug"),
                TodoDraft {
                    done: true,
                    ..draft(None, "ship")
                },
            ])
            .unwrap();
        let ids: Vec<_> = todos.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["todo-1", "todo-2", "todo-3"]);
        assert_eq!(todos.items[0].text, "write tests");
        assert_eq!(todos.open_count(), 2);
        assert!(todos.remove("todo-2"));
        assert!(!todos.remove("todo-2"));
    }
}
