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
    #[error("cannot rename or delete the explorer root")]
    RootMutation,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid file or directory name: {0}")]
    InvalidName(String),
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
        let root = root.into();
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| !selected.starts_with(&root))
        {
            self.selected = None;
        }
        self.root = root;
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

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub fn create_file(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.checked_creation(relative)?;
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
        let path = self.checked_creation(relative)?;
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Create a single file directly under an existing workspace directory.
    /// Unlike `create_file`, this API intentionally rejects path separators so
    /// an inline name prompt can never be used as an implicit traversal input.
    pub fn create_file_in(
        &self,
        parent: impl AsRef<Path>,
        name: &str,
    ) -> Result<PathBuf, TreeError> {
        let path = self.checked_child(parent, name)?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(path)
    }

    /// Create a single directory directly under an existing workspace
    /// directory. The parent is relative to the explorer root.
    pub fn create_directory_in(
        &self,
        parent: impl AsRef<Path>,
        name: &str,
    ) -> Result<PathBuf, TreeError> {
        let path = self.checked_child(parent, name)?;
        std::fs::create_dir(&path)?;
        Ok(path)
    }

    pub fn rename(&self, relative: impl AsRef<Path>, new_name: &str) -> Result<PathBuf, TreeError> {
        validate_name(new_name)?;
        let path = self.checked_existing(relative)?;
        let target = path.with_file_name(new_name);
        self.ensure_creation_target_under_root(&target)?;
        std::fs::rename(&path, &target)?;
        Ok(target)
    }

    pub fn delete(&self, relative: impl AsRef<Path>) -> Result<(), TreeError> {
        let path = self.checked_existing(relative)?;
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

    fn checked_creation(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.checked(relative)?;
        self.ensure_creation_target_under_root(&path)?;
        Ok(path)
    }

    fn checked_existing(&self, relative: impl AsRef<Path>) -> Result<PathBuf, TreeError> {
        let path = self.checked(relative)?;
        let root = self.root.canonicalize()?;
        let existing = path.canonicalize()?;
        if existing == root {
            return Err(TreeError::RootMutation);
        }
        if !existing.starts_with(&root) {
            return Err(TreeError::OutsideRoot(path));
        }
        Ok(path)
    }

    fn checked_child(&self, parent: impl AsRef<Path>, name: &str) -> Result<PathBuf, TreeError> {
        validate_name(name)?;
        let parent = self.checked(parent)?;
        if !parent.is_dir() {
            return Err(TreeError::Io(std::io::Error::new(
                // `NotADirectory` is newer than the workspace MSRV.
                std::io::ErrorKind::InvalidInput,
                format!("parent is not a directory: {}", parent.display()),
            )));
        }
        let root = self.root.canonicalize()?;
        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&root) {
            return Err(TreeError::OutsideRoot(parent));
        }
        let path = parent.join(name);
        self.ensure_under_root(&path)?;
        Ok(path)
    }

    /// Validate the nearest existing ancestor as well as the lexical path.
    /// This prevents a child path from escaping through a symlink contained in
    /// the workspace while still allowing the legacy APIs to create nested
    /// directories in one operation.
    fn ensure_creation_target_under_root(&self, path: &Path) -> Result<(), TreeError> {
        self.ensure_under_root(path)?;
        let root = self.root.canonicalize()?;
        let mut ancestor = path.parent().unwrap_or(path);
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| TreeError::OutsideRoot(path.to_path_buf()))?;
        }
        let ancestor = ancestor.canonicalize()?;
        if ancestor == root || ancestor.starts_with(&root) {
            Ok(())
        } else {
            Err(TreeError::OutsideRoot(path.to_path_buf()))
        }
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

fn validate_name(name: &str) -> Result<(), TreeError> {
    let trimmed = name.trim();
    let is_single_component = Path::new(trimmed).components().count() == 1;
    let has_separator = trimmed.contains('/') || trimmed.contains('\\');
    let invalid_windows_character = trimmed
        .chars()
        .any(|character| matches!(character, ':' | '*' | '?' | '"' | '<' | '>' | '|'));
    let windows_stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved_windows_name = matches!(
        windows_stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if trimmed.is_empty()
        || trimmed != name
        || trimmed.ends_with('.')
        || matches!(trimmed, "." | "..")
        || !is_single_component
        || has_separator
        || invalid_windows_character
        || reserved_windows_name
        || trimmed.chars().any(char::is_control)
    {
        Err(TreeError::InvalidName(name.to_owned()))
    } else {
        Ok(())
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
        tree.select("/a/old.txt");
        tree.set_root("/a/src");
        assert!(tree.is_expanded("/a/src"));
        assert_eq!(tree.selected(), None);
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
        assert!(matches!(tree.delete(""), Err(TreeError::RootMutation)));
        assert!(matches!(
            tree.rename("", "renamed-root"),
            Err(TreeError::RootMutation)
        ));
    }

    #[test]
    fn prompt_operations_are_scoped_to_one_child_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let tree = TreeState::new(dir.path());

        let file = tree.create_file_in("src", "main.rs").unwrap();
        let folder = tree.create_directory_in("", "docs").unwrap();

        assert_eq!(file, dir.path().join("src/main.rs"));
        assert_eq!(folder, dir.path().join("docs"));
        for invalid in [
            "",
            ".",
            "..",
            "../escape",
            "nested/name",
            "a:b",
            "CON",
            "nul.txt",
            "trailing.",
            " padded ",
        ] {
            assert!(matches!(
                tree.create_file_in("", invalid),
                Err(TreeError::InvalidName(_))
            ));
        }
    }
}
