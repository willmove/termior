//! 会话与项目记忆（FR-SESS-01/02）。
//!
//! - 命名持久会话：会话列表 + activeId + 每会话完整消息历史；标题由首条用户消息自动生成；
//!   每次消息变更镜像落盘（`Termior-ai-sessions.json`，经 termior-store 原子写）。
//! - 项目记忆：从工作区根加载 `Termior.md`（对齐 CLAUDE.md/AGENTS.md 惯例），每会话
//!   加载一次前置到工作上下文；支持 `AGENTS.md` 内容为单行 `Termior.md` 的重定向。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::message::Message;

/// 一个持久会话。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    /// 创建时活动代理 id（FR-PLAN-03，P1；预留）。
    #[serde(default)]
    pub agent_id: Option<String>,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: "New session".into(),
            messages: vec![],
            agent_id: None,
        }
    }

    /// 追加一条用户消息，并根据首条用户消息自动生成标题（FR-SESS-01）。
    pub fn push_user(&mut self, content: impl Into<String>) {
        let content = content.into();
        let is_first_user = !self
            .messages
            .iter()
            .any(|m| m.role == crate::message::Role::User);
        if is_first_user {
            self.title = derive_title(&content);
        }
        self.messages.push(Message::user(content));
    }

    pub fn push_assistant(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn rename(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    /// 追加任意消息，若超过 `cap` 则按 [`truncate_for_context`] 截断（保留 system 前置 +
    /// 最近 `cap` 条）。`cap == 0` 表示不限。
    pub fn push_and_cap(&mut self, msg: Message, cap: usize) {
        self.messages.push(msg);
        if cap > 0 && self.messages.len() > cap {
            self.messages = truncate_for_context(self.messages.clone(), cap);
        }
    }

    /// 估算消息历史的近似字节数（用于决定何时从单文件拆分为每会话一文件，Q3）。
    pub fn approx_bytes(&self) -> usize {
        self.messages
            .iter()
            .map(|m| {
                m.content.len()
                    + m.tool_calls
                        .iter()
                        .map(|c| c.arguments.len() + 32)
                        .sum::<usize>()
            })
            .sum()
    }
}

/// 会话内消息历史膨胀策略（Q3）。
///
/// 保留：
/// - 所有 system 消息（前置记忆/指令不应丢失）
/// - 最近 `keep_recent` 条非 system 消息
///
/// 中段被丢弃。返回新 Vec。
pub fn truncate_for_context(mut messages: Vec<Message>, keep_recent: usize) -> Vec<Message> {
    use crate::message::Role;
    // 先抽出所有 system 消息（保持原顺序）
    let system: Vec<Message> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .cloned()
        .collect();
    let mut tail: Vec<Message> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .rev()
        .take(keep_recent)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let mut out = system;
    out.append(&mut tail);
    // 静默 suppress 未使用警告
    let _ = &mut messages;
    out
}

/// 从首条用户消息生成标题：取首行非空文本，截断到 ~40 字符。
pub fn derive_title(content: &str) -> String {
    let first_line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let max = 40;
    let chars: Vec<char> = first_line.chars().collect();
    if chars.len() <= max {
        first_line.to_string()
    } else {
        let mut t: String = chars[..max].iter().collect();
        t.push('…');
        t
    }
}

/// 会话存储（`Termior-ai-sessions.json`）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SessionStore {
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub active_id: Option<String>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建新会话并设为 active，返回其 id。
    pub fn create(&mut self, id: impl Into<String>) -> String {
        let id = id.into();
        let s = Session::new(id.clone());
        self.sessions.push(s);
        self.active_id = Some(id.clone());
        id
    }

    pub fn get(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        let removed = self.sessions.len() < before;
        if removed && self.active_id.as_deref() == Some(id) {
            self.active_id = self.sessions.first().map(|s| s.id.clone());
        }
        removed
    }

    /// 镜像落盘到 `Termior-ai-sessions.json`（FR-SESS-01，经 termior-store 原子写）。
    pub fn persist(&self, dir: &Path) -> Result<PathBuf, std::io::Error> {
        let json =
            serde_json::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        let path = dir.join("Termior-ai-sessions.json");
        termior_store::atomic_write(&path, &json)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(path)
    }

    /// 从文件加载（启动时；非法 JSON 由上层迁移处理）。
    pub fn load(dir: &Path) -> Result<Self, std::io::Error> {
        let path = dir.join("Termior-ai-sessions.json");
        let text = termior_store::atomic::read_text(&path)?;
        serde_json::from_str(&text).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

/// 项目记忆（FR-SESS-02）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMemory {
    /// 已解析的记忆文本（来自 `Termior.md` 或重定向后的真实文件）。
    pub content: String,
    /// 来源文件相对路径。
    pub source: String,
}

impl ProjectMemory {
    /// 从工作区根加载项目记忆。
    ///
    /// 优先级：`Termior.md` > `CLAUDE.md` > `AGENTS.md`。
    /// `AGENTS.md` 内容为单行 `Termior.md` 时按重定向解析（FR-SESS-02）。
    pub fn load_from(workspace_root: &Path) -> Option<ProjectMemory> {
        for name in ["Termior.md", "CLAUDE.md", "AGENTS.md"] {
            let p = workspace_root.join(name);
            if let Ok(text) = std::fs::read_to_string(&p) {
                let trimmed = text.trim();
                // 重定向写法：AGENTS.md 仅含 `Termior.md`（单行）
                if name == "AGENTS.md" {
                    let single = trimmed.lines().count() == 1;
                    if single && trimmed.lines().next() == Some("Termior.md") {
                        let redirected = workspace_root.join("Termior.md");
                        if let Ok(real) = std::fs::read_to_string(&redirected) {
                            return Some(ProjectMemory {
                                content: real,
                                source: "Termior.md".into(),
                            });
                        }
                        // 重定向目标不存在：不返回空记忆
                        return None;
                    }
                }
                return Some(ProjectMemory {
                    content: text,
                    source: name.into(),
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn title_from_first_user_message() {
        let mut s = Session::new("s1");
        s.push_user("Refactor the auth module\nwith these steps");
        assert_eq!(s.title, "Refactor the auth module");
        // 第二条用户消息不改标题
        s.push_user("another message that is way too long to be a title honestly it really is");
        assert_eq!(s.title, "Refactor the auth module");
    }

    #[test]
    fn title_truncates_long_messages() {
        let long = "x".repeat(100);
        let t = derive_title(&long);
        assert!(t.chars().count() <= 41); // 40 + …
        assert!(t.ends_with('…'));
    }

    #[test]
    fn title_uses_first_non_empty_line() {
        let t = derive_title("\n\n  hello world  \nsecond");
        assert_eq!(t, "hello world");
    }

    #[test]
    fn session_store_create_active_delete() {
        let mut store = SessionStore::new();
        let id = store.create("s1");
        assert_eq!(store.active_id.as_deref(), Some(id.as_str()));
        store.create("s2");
        assert_eq!(store.active_id.as_deref(), Some("s2"));
        // 删除 active 后回退到第一个
        assert!(store.delete("s2"));
        assert_eq!(store.active_id.as_deref(), Some("s1"));
        assert!(!store.delete("nope"));
    }

    #[test]
    fn session_store_persist_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let mut store = SessionStore::new();
        let id = store.create("s1");
        store.get_mut(&id).unwrap().push_user("hello");
        let path = store.persist(dir.path()).unwrap();
        assert!(path.ends_with("Termior-ai-sessions.json"));

        let loaded = SessionStore::load(dir.path()).unwrap();
        assert_eq!(loaded, store);
    }

    #[test]
    fn project_memory_prefers_termior_md() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Termior.md"), "# Termior memory").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Claude memory").unwrap();
        let m = ProjectMemory::load_from(dir.path()).unwrap();
        assert_eq!(m.source, "Termior.md");
        assert!(m.content.contains("Termior memory"));
    }

    #[test]
    fn project_memory_falls_back_to_claude_md() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Claude").unwrap();
        let m = ProjectMemory::load_from(dir.path()).unwrap();
        assert_eq!(m.source, "CLAUDE.md");
    }

    #[test]
    fn project_memory_agents_md_redirect() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Termior.md").unwrap();
        std::fs::write(dir.path().join("Termior.md"), "real memory").unwrap();
        let m = ProjectMemory::load_from(dir.path()).unwrap();
        assert_eq!(m.source, "Termior.md");
        assert_eq!(m.content, "real memory");
    }

    #[test]
    fn project_memory_none_when_no_file() {
        let dir = tempdir().unwrap();
        assert!(ProjectMemory::load_from(dir.path()).is_none());
    }

    #[test]
    fn session_rename() {
        let mut s = Session::new("s1");
        s.rename("custom title");
        assert_eq!(s.title, "custom title");
    }

    // —— Q3 消息历史膨胀策略 ——
    #[test]
    fn truncate_keeps_system_and_recent_tail() {
        use crate::message::Role;
        let msgs = vec![
            Message::system("memory"),
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
            Message::user("u3"),
        ];
        let out = truncate_for_context(msgs, 2);
        // system 保留 + 最近 2 条非 system
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].role, Role::System);
        assert_eq!(out[1].content, "a2");
        assert_eq!(out[2].content, "u3");
    }

    #[test]
    fn truncate_with_no_system() {
        let msgs = vec![
            Message::user("u1"),
            Message::user("u2"),
            Message::user("u3"),
        ];
        let out = truncate_for_context(msgs, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content, "u2");
        assert_eq!(out[1].content, "u3");
    }

    #[test]
    fn push_and_cap_truncates_when_exceeded() {
        use crate::message::Role;
        let mut s = Session::new("s1");
        s.messages.push(Message::system("mem"));
        for i in 0..20 {
            s.push_and_cap(Message::user(format!("u{i}")), 5);
        }
        // system(1) + 最近 5 条
        assert_eq!(s.messages.len(), 6);
        assert_eq!(s.messages[0].role, Role::System);
        // 最后一条是 u19
        assert_eq!(s.messages.last().unwrap().content, "u19");
    }

    #[test]
    fn push_and_cap_zero_means_no_limit() {
        let mut s = Session::new("s1");
        for i in 0..100 {
            s.push_and_cap(Message::user(format!("u{i}")), 0);
        }
        assert_eq!(s.messages.len(), 100);
    }

    #[test]
    fn approx_bytes_estimates_size() {
        let mut s = Session::new("s1");
        s.messages.push(Message::user("hello")); // 5 字节
        s.messages.push(Message::assistant("world!")); // 6 字节
        let bytes = s.approx_bytes();
        assert!(bytes >= 11, "got {bytes}");
    }

    #[test]
    fn title_preserved_after_truncation() {
        let mut s = Session::new("s1");
        s.push_user("First important question");
        for i in 0..10 {
            s.push_and_cap(Message::assistant(format!("a{i}")), 3);
        }
        // 标题不应被截断策略影响
        assert_eq!(s.title, "First important question");
    }
}
