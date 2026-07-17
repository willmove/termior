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

    pub fn expand(&self, input: &str) -> String {
        input
            .split_whitespace()
            .map(|word| {
                word.strip_prefix('#')
                    .and_then(|handle| self.snippets.iter().find(|item| item.handle == handle))
                    .map(|snippet| snippet.body.as_str())
                    .unwrap_or(word)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
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
        let mut todos = TodoStore::default();
        todos.add("1", "ship");
        assert!(todos.set_done("1", true));
        assert!(todos.items[0].done);
    }
}
