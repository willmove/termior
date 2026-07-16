use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct TreeState {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    selected: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("path escapes explorer root: {0}")]
    OutsideRoot(PathBuf),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl TreeState {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ..Self::default()
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Change the cwd-driven root without resetting expansion state (FR-EXPL-02).
    pub fn set_root(&mut self, root: impl Into<PathBuf>) {
        self.root = root.into();
    }

    pub fn toggle_expanded(&mut self, path: impl Into<PathBuf>) -> bool {
        let path = path.into();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
            true
        } else {
            false
        }
    }

    pub fn is_expanded(&self, path: impl AsRef<Path>) -> bool {
        self.expanded.contains(path.as_ref())
    }

    pub fn select(&mut self, path: impl Into<PathBuf>) {
        self.selected = Some(path.into());
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn create_file(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.checked(relative)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(path)
    }

    pub fn create_directory(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.checked(relative)?;
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    pub fn rename(&self, relative: impl AsRef<Path>, new_name: &str) -> Result<PathBuf, TreeError> {
        let path = self.checked(relative)?;
        let target = path.with_file_name(new_name);
        self.ensure_under_root(&target)?;
        std::fs::rename(&path, &target)?;
        Ok(target)
    }

    pub fn delete(&self, relative: impl AsRef<Path>) -> Result<(), TreeError> {
        let path = self.checked(relative)?;
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn checked(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.root.join(relative.as_ref());
        self.ensure_under_root(&path)?;
        Ok(path)
    }

    fn ensure_under_root(&self, path: &Path) -> Result<(), TreeError> {
        let normalized = logical_normalize(path);
        let root = logical_normalize(&self.root);
        if normalized == root || normalized.starts_with(&root) {
            Ok(())
        } else {
            Err(TreeError::OutsideRoot(path.to_path_buf()))
        }
    }
}

fn logical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_change_keeps_expansion() {
        let mut tree = TreeState::new("/a");
        tree.toggle_expanded("/a/src");
        tree.set_root("/a/src");
        assert!(tree.is_expanded("/a/src"));
    }

    #[test]
    fn mutations_work_and_traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tree = TreeState::new(dir.path());
        let file = tree.create_file("src/a.txt").unwrap();
        assert!(file.exists());
        let renamed = tree.rename("src/a.txt", "b.txt").unwrap();
        assert!(renamed.exists());
        tree.delete("src/b.txt").unwrap();
        assert!(!renamed.exists());
        assert!(matches!(
            tree.create_file("../escape"),
            Err(TreeError::OutsideRoot(_))
        ));
    }
}
