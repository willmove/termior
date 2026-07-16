use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use termior_explorer_core::fuzzy::{fuzzy_match, FuzzyHit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Folder,
    Rust,
    JavaScript,
    TypeScript,
    Python,
    Go,
    Java,
    Html,
    Css,
    Json,
    Markdown,
    Image,
    Config,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub relative: String,
    pub depth: usize,
    pub is_dir: bool,
    pub icon: IconKind,
}

#[derive(Debug, Clone)]
pub struct FileIndex {
    root: PathBuf,
    show_dotfiles: bool,
    entries: Vec<FileEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("workspace root is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("walk error: {0}")]
    Walk(String),
}

impl FileIndex {
    pub fn build(root: impl AsRef<Path>, show_dotfiles: bool) -> Result<Self, IndexError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(IndexError::NotDirectory(root.to_path_buf()));
        }
        let mut index = Self {
            root: root.to_path_buf(),
            show_dotfiles,
            entries: Vec::new(),
        };
        index.refresh()?;
        Ok(index)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub fn set_show_dotfiles(&mut self, show: bool) -> Result<(), IndexError> {
        self.show_dotfiles = show;
        self.refresh()
    }

    pub fn refresh(&mut self) -> Result<(), IndexError> {
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(!self.show_dotfiles)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false)
            .ignore(true)
            .parents(true);
        let mut entries = Vec::new();
        for result in builder.build() {
            let dent = result.map_err(|error| IndexError::Walk(error.to_string()))?;
            if dent.path() == self.root {
                continue;
            }
            let relative_path = dent.path().strip_prefix(&self.root).unwrap_or(dent.path());
            let relative = normalize_path(relative_path);
            let is_dir = dent.file_type().is_some_and(|kind| kind.is_dir());
            entries.push(FileEntry {
                path: dent.path().to_path_buf(),
                depth: relative_path.components().count().saturating_sub(1),
                icon: icon_for(relative_path, is_dir),
                relative,
                is_dir,
            });
        }
        entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.relative.to_lowercase().cmp(&b.relative.to_lowercase()),
        });
        self.entries = entries;
        Ok(())
    }

    pub fn fuzzy(&self, query: &str, limit: usize) -> Vec<FuzzyHit> {
        fuzzy_match(
            query,
            self.entries
                .iter()
                .filter(|entry| !entry.is_dir)
                .map(|entry| entry.relative.as_str()),
        )
        .into_iter()
        .take(limit)
        .collect()
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn icon_for(path: &Path, is_dir: bool) -> IconKind {
    if is_dir {
        return IconKind::Folder;
    }
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let ext = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(name, "Cargo.toml" | "package.json" | "pyproject.toml") {
        return IconKind::Config;
    }
    match ext.as_str() {
        "rs" => IconKind::Rust,
        "js" | "jsx" | "mjs" | "cjs" => IconKind::JavaScript,
        "ts" | "tsx" | "mts" | "cts" => IconKind::TypeScript,
        "py" | "pyi" => IconKind::Python,
        "go" => IconKind::Go,
        "java" => IconKind::Java,
        "html" | "htm" => IconKind::Html,
        "css" | "scss" | "sass" | "less" => IconKind::Css,
        "json" | "jsonc" => IconKind::Json,
        "md" | "markdown" => IconKind::Markdown,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => IconKind::Image,
        "toml" | "yaml" | "yml" | "ini" => IconKind::Config,
        _ => IconKind::File,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_respects_gitignore_and_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();
        fs::write(dir.path().join("ignored.log"), "x").unwrap();
        fs::write(dir.path().join(".hidden"), "x").unwrap();
        fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();
        let index = FileIndex::build(dir.path(), false).unwrap();
        let paths: Vec<&str> = index
            .entries()
            .iter()
            .map(|e| e.relative.as_str())
            .collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(!paths.contains(&"ignored.log"));
        assert!(!paths.contains(&".hidden"));
    }

    #[test]
    fn fuzzy_returns_files_not_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "x").unwrap();
        let index = FileIndex::build(dir.path(), true).unwrap();
        let hits = index.fuzzy("main", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/main.rs");
    }
}
