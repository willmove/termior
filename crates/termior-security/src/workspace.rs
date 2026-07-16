//! Workspace 授权注册表（FR-SEC-04）。
//!
//! AI 工具、git 命令、PTY spawn **共用同一注册表**。新工作区首次提示授权一次；
//! 代理不可触达未显式打开的同级目录。
//!
//! 判定基于「目标路径是否落在某个已授权工作区目录树下」（canonicalize 之后比较），
//! 因此 `..` 穿越、符号链接指向工作区外、跨盘符绝对路径均无法绕过。

use std::collections::HashSet;

use crate::deny_list::canonicalize_logical;

/// 授权状态（供 UI 展示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceAuthStatus {
    /// 落在已授权工作区内。
    Authorized,
    /// 未授权（需要首次提示）。
    NeedsAuthorization,
}

/// 以工作区根目录（已规范化、正斜杠）为 key 的注册表。
#[derive(Debug, Clone, Default)]
pub struct WorkspaceAuthRegistry {
    roots: HashSet<String>,
}

impl WorkspaceAuthRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用一组初始授权根构造。
    pub fn with_roots(roots: impl IntoIterator<Item = String>) -> Self {
        Self {
            roots: roots.into_iter().map(|r| normalize_root(&r)).collect(),
        }
    }

    /// 显式授权一个工作区目录（canonicalize 后存入）。
    pub fn authorize(&mut self, root: &str) {
        self.roots.insert(normalize_root(root));
    }

    /// 撤销一个工作区授权。
    pub fn revoke(&mut self, root: &str) -> bool {
        self.roots.remove(&normalize_root(root))
    }

    /// 判定目标路径是否落在某个已授权工作区内。
    pub fn check(&self, target: &str) -> WorkspaceAuthStatus {
        if self.is_authorized(target) {
            WorkspaceAuthStatus::Authorized
        } else {
            WorkspaceAuthStatus::NeedsAuthorization
        }
    }

    /// 判定目标路径是否落在某个已授权工作区内（含等价或子路径）。
    pub fn is_authorized(&self, target: &str) -> bool {
        let t = canonicalize_logical(target);
        // 去掉末尾分隔符以规范化比较
        let t = t.trim_end_matches('/').to_string();
        for r in &self.roots {
            if t == *r {
                return true;
            }
            // 子路径：t 必须以 `<root>/` 开头，避免 `/home/u-evil` 伪匹配 `/home/u`
            let prefix = format!("{r}/");
            if t.starts_with(&prefix) {
                return true;
            }
        }
        false
    }

    /// 当前已授权根列表（已规范化）。
    pub fn roots(&self) -> Vec<String> {
        let mut v: Vec<_> = self.roots.iter().cloned().collect();
        v.sort();
        v
    }
}

fn normalize_root(root: &str) -> String {
    let c = canonicalize_logical(root);
    c.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_root_allows_itself_and_children() {
        let reg = WorkspaceAuthRegistry::with_roots(["/home/u/proj".to_string()]);
        assert_eq!(reg.check("/home/u/proj"), WorkspaceAuthStatus::Authorized);
        assert_eq!(
            reg.check("/home/u/proj/src/main.rs"),
            WorkspaceAuthStatus::Authorized
        );
        assert_eq!(reg.check("/home/u/proj/"), WorkspaceAuthStatus::Authorized);
    }

    #[test]
    fn sibling_dir_not_authorized() {
        // FR-SEC-04：代理不可触达未显式打开的同级目录
        let reg = WorkspaceAuthRegistry::with_roots(["/home/u/proj".to_string()]);
        assert_eq!(
            reg.check("/home/u/other-proj"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
        assert_eq!(
            reg.check("/home/u/other-proj/x.rs"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
    }

    #[test]
    fn prefix_attack_blocked() {
        // `/home/u/proj-evil` 不应被 `/home/u/proj` 误判为子路径
        let reg = WorkspaceAuthRegistry::with_roots(["/home/u/proj".to_string()]);
        assert_eq!(
            reg.check("/home/u/proj-evil"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
        assert_eq!(
            reg.check("/home/u/projx"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
    }

    #[test]
    fn traversal_cannot_escape() {
        let reg = WorkspaceAuthRegistry::with_roots(["/home/u/proj".to_string()]);
        // `..` 穿出到工作区外
        assert_eq!(
            reg.check("/home/u/proj/src/../../secret"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
        // 穿到兄弟目录
        assert_eq!(
            reg.check("/home/u/proj/../other/x"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
    }

    #[test]
    fn authorize_and_revoke() {
        let mut reg = WorkspaceAuthRegistry::new();
        assert_eq!(
            reg.check("/work/a"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
        reg.authorize("/work/a");
        assert_eq!(reg.check("/work/a/file"), WorkspaceAuthStatus::Authorized);
        assert!(reg.revoke("/work/a"));
        assert_eq!(
            reg.check("/work/a/file"),
            WorkspaceAuthStatus::NeedsAuthorization
        );
    }

    #[test]
    fn multiple_roots() {
        let reg = WorkspaceAuthRegistry::with_roots([
            "/home/u/proj".to_string(),
            "/srv/data".to_string(),
        ]);
        assert!(reg.is_authorized("/home/u/proj/x"));
        assert!(reg.is_authorized("/srv/data/y"));
        assert!(!reg.is_authorized("/home/u/z"));
    }

    #[test]
    fn backslash_normalized_on_windows() {
        let reg = WorkspaceAuthRegistry::with_roots([r"C:\Users\u\proj".to_string()]);
        assert!(reg.is_authorized(r"C:\Users\u\proj\src\main.rs"));
        assert!(reg.is_authorized("C:/Users/u/proj/src/main.rs"));
    }

    #[test]
    fn windows_drive_root_not_traversed_past() {
        let reg = WorkspaceAuthRegistry::with_roots([r"C:\proj".to_string()]);
        // 穿到 C:\Windows
        assert!(!reg.is_authorized(r"C:\proj\..\Windows\System32"));
    }

    #[test]
    fn roots_listed_normalized() {
        let reg = WorkspaceAuthRegistry::with_roots(["/a/b/../c".to_string()]);
        assert_eq!(reg.roots(), vec!["/a/c".to_string()]);
    }
}
