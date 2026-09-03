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

/// Why a walk entry was skipped. Values are user-facing labels, not raw OS text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    PermissionDenied,
    NotFound,
    Loop,
    Unreadable,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "Permission denied",
            Self::NotFound => "Not found",
            Self::Loop => "Symbolic link loop",
            Self::Unreadable => "Unable to read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    /// Path relative to the workspace root when possible; otherwise absolute.
    pub path: PathBuf,
    pub reason: SkipReason,
}

impl SkippedEntry {
    /// Platform-native separators for UI (no mixed `/` and `\`).
    pub fn display_path(&self) -> String {
        platform_path(&self.path)
    }
}

#[derive(Debug, Clone)]
pub struct FileIndex {
    root: PathBuf,
    show_dotfiles: bool,
    entries: Vec<FileEntry>,
    skipped: Vec<SkippedEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("workspace root is not a directory: {0}")]
    NotDirectory(PathBuf),
}

/// Walk depth that yields only the workspace root's direct children.
///
/// Matches `ignore::WalkBuilder::max_depth(Some(1))`: depth 0 is the root itself
/// (skipped), depth 1 is immediate children.
pub const SHALLOW_MAX_DEPTH: usize = 1;

impl FileIndex {
    /// Full recursive index (respects gitignore / ignore files).
    pub fn build(root: impl AsRef<Path>, show_dotfiles: bool) -> Result<Self, IndexError> {
        Self::build_with_max_depth(root, show_dotfiles, None)
    }

    /// Index only direct children of `root` for a fast first paint.
    ///
    /// A subsequent [`Self::build`] / [`Self::refresh`] can replace this atomically
    /// with a deep index for the same root.
    pub fn build_shallow(root: impl AsRef<Path>, show_dotfiles: bool) -> Result<Self, IndexError> {
        Self::build_with_max_depth(root, show_dotfiles, Some(SHALLOW_MAX_DEPTH))
    }

    fn build_with_max_depth(
        root: impl AsRef<Path>,
        show_dotfiles: bool,
        max_depth: Option<usize>,
    ) -> Result<Self, IndexError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(IndexError::NotDirectory(root.to_path_buf()));
        }
        let mut index = Self {
            root: root.to_path_buf(),
            show_dotfiles,
            entries: Vec::new(),
            skipped: Vec::new(),
        };
        index.refresh_with_max_depth(max_depth)?;
        Ok(index)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    pub fn skipped(&self) -> &[SkippedEntry] {
        &self.skipped
    }

    pub fn set_show_dotfiles(&mut self, show: bool) -> Result<(), IndexError> {
        self.show_dotfiles = show;
        self.refresh()
    }

    pub fn refresh(&mut self) -> Result<(), IndexError> {
        self.refresh_with_max_depth(None)
    }

    /// Replace entries with a shallow (direct-children) listing of the same root.
    pub fn refresh_shallow(&mut self) -> Result<(), IndexError> {
        self.refresh_with_max_depth(Some(SHALLOW_MAX_DEPTH))
    }

    fn refresh_with_max_depth(&mut self, max_depth: Option<usize>) -> Result<(), IndexError> {
        if !self.root.is_dir() {
            return Err(IndexError::NotDirectory(self.root.clone()));
        }
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(!self.show_dotfiles)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false)
            .ignore(true)
            .parents(true)
            .max_depth(max_depth);
        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        for result in builder.build() {
            let dent = match result {
                Ok(dent) => dent,
                Err(error) => {
                    // Soft ignore-file / glob issues must not abort or clutter the UI.
                    if is_soft_walk_error(&error) {
                        continue;
                    }
                    let path = walk_error_path(&error).unwrap_or_else(|| self.root.clone());
                    if path == self.root {
                        return Err(IndexError::NotDirectory(self.root.clone()));
                    }
                    let relative = path
                        .strip_prefix(&self.root)
                        .unwrap_or(path.as_path())
                        .to_path_buf();
                    skipped.push(SkippedEntry {
                        path: relative,
                        reason: classify_walk_error(&error),
                    });
                    continue;
                }
            };
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
        skipped.sort_by(|a, b| {
            platform_path(&a.path)
                .to_lowercase()
                .cmp(&platform_path(&b.path).to_lowercase())
        });
        self.entries = entries;
        self.skipped = skipped;
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

fn is_soft_walk_error(error: &ignore::Error) -> bool {
    if error.is_partial() {
        return true;
    }
    matches!(
        error,
        ignore::Error::Glob { .. }
            | ignore::Error::UnrecognizedFileType(_)
            | ignore::Error::InvalidDefinition
    )
}

fn walk_error_path(error: &ignore::Error) -> Option<PathBuf> {
    match error {
        ignore::Error::WithPath { path, .. } => Some(path.clone()),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => {
            walk_error_path(err)
        }
        ignore::Error::Partial(errors) => errors.iter().find_map(walk_error_path),
        ignore::Error::Loop { child, .. } => Some(child.clone()),
        _ => None,
    }
}

fn classify_walk_error(error: &ignore::Error) -> SkipReason {
    if walk_error_is_loop(error) {
        return SkipReason::Loop;
    }
    match error.io_error().map(|io| io.kind()) {
        Some(std::io::ErrorKind::PermissionDenied) => SkipReason::PermissionDenied,
        Some(std::io::ErrorKind::NotFound) => SkipReason::NotFound,
        _ => SkipReason::Unreadable,
    }
}

fn walk_error_is_loop(error: &ignore::Error) -> bool {
    match error {
        ignore::Error::Loop { .. } => true,
        ignore::Error::WithPath { err, .. }
        | ignore::Error::WithDepth { err, .. }
        | ignore::Error::WithLineNumber { err, .. } => walk_error_is_loop(err),
        ignore::Error::Partial(errors) => errors.iter().any(walk_error_is_loop),
        _ => false,
    }
}

/// Internal index paths stay `/`-normalized for fuzzy matching.
fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// UI / skipped-entry paths use the platform separator consistently.
fn platform_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join(std::path::MAIN_SEPARATOR_STR)
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
    #[cfg(windows)]
    use std::process::Command;

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
        assert!(index.skipped().is_empty());
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

    #[test]
    fn missing_root_is_the_only_hard_failure() {
        let missing = tempfile::tempdir().unwrap().path().join("does-not-exist");
        let error = FileIndex::build(&missing, false).unwrap_err();
        assert!(matches!(error, IndexError::NotDirectory(_)));
    }

    #[test]
    fn shallow_index_lists_only_direct_children() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        fs::write(dir.path().join("readme.md"), "hi").unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();
        fs::write(dir.path().join("src/nested/deep.rs"), "x").unwrap();

        let shallow = FileIndex::build_shallow(dir.path(), true).unwrap();
        let paths: Vec<&str> = shallow
            .entries()
            .iter()
            .map(|e| e.relative.as_str())
            .collect();
        assert!(paths.contains(&"readme.md"), "{paths:?}");
        assert!(paths.contains(&"src"), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains('/')),
            "shallow must not include nested paths: {paths:?}"
        );

        let deep = FileIndex::build(dir.path(), true).unwrap();
        let deep_paths: Vec<&str> = deep.entries().iter().map(|e| e.relative.as_str()).collect();
        assert!(deep_paths.contains(&"src/main.rs"), "{deep_paths:?}");
        assert!(deep_paths.contains(&"src/nested/deep.rs"), "{deep_paths:?}");
    }

    #[test]
    fn shallow_then_deep_can_atomically_replace() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "x").unwrap();

        let mut index = FileIndex::build_shallow(dir.path(), true).unwrap();
        assert_eq!(index.root(), dir.path());
        assert!(index.entries().iter().all(|e| !e.relative.contains('/')));

        index.refresh().unwrap();
        assert_eq!(index.root(), dir.path());
        assert!(
            index.entries().iter().any(|e| e.relative == "src/main.rs"),
            "deep refresh must replace shallow entries"
        );
    }

    #[test]
    fn shallow_stays_fast_on_wide_nested_tree() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        for i in 0..150 {
            let nested = dir.path().join(format!("bucket{i}/nested/deep"));
            fs::create_dir_all(&nested).unwrap();
            fs::write(nested.join("file.txt"), "x").unwrap();
        }
        let start = Instant::now();
        let shallow = FileIndex::build_shallow(dir.path(), true).unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "shallow listing should stay interactive, took {elapsed:?}"
        );
        assert_eq!(shallow.entries().len(), 150);
        assert!(shallow.entries().iter().all(|e| !e.relative.contains('/')));
    }

    #[test]
    fn shallow_build_survives_unreadable_child() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("ok.txt"), "x").unwrap();
        let secret = dir.path().join("secret");
        fs::create_dir_all(&secret).unwrap();
        let _guard = deny_read(&secret);

        let index = FileIndex::build_shallow(dir.path(), true).unwrap();
        let paths: Vec<&str> = index
            .entries()
            .iter()
            .map(|e| e.relative.as_str())
            .collect();
        assert!(paths.contains(&"ok.txt"), "{paths:?}");
        assert!(paths.contains(&"src"), "{paths:?}");
    }

    #[test]
    fn unreadable_subdirectory_is_skipped_and_rest_indexed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();
        fs::write(dir.path().join("readme.md"), "hi").unwrap();
        let secret = dir.path().join("secret");
        fs::create_dir_all(&secret).unwrap();
        fs::write(secret.join("hidden.txt"), "nope").unwrap();

        let _guard = deny_read(&secret);

        let index = FileIndex::build(dir.path(), true).unwrap();
        let paths: Vec<&str> = index
            .entries()
            .iter()
            .map(|e| e.relative.as_str())
            .collect();
        assert!(
            paths.contains(&"src/main.rs"),
            "readable files must still be indexed: {paths:?}"
        );
        assert!(paths.contains(&"readme.md"), "{paths:?}");
        assert!(
            !paths.iter().any(|p| p.contains("hidden")),
            "contents of unreadable dir must not appear: {paths:?}"
        );
        assert!(
            !index.skipped().is_empty(),
            "expected at least one skipped entry"
        );
        for skipped in index.skipped() {
            let shown = skipped.display_path();
            #[cfg(windows)]
            assert!(!shown.contains('/'), "windows path must use \\: {shown}");
            #[cfg(unix)]
            assert!(!shown.contains('\\'), "unix path must use /: {shown}");
            assert!(!skipped.reason.as_str().is_empty());
            // Must not surface raw OS / HRESULT text.
            assert!(!skipped.reason.as_str().contains("os error"));
            assert!(!skipped.reason.as_str().contains("0x"));
        }
    }

    struct DenyReadGuard(PathBuf);

    impl Drop for DenyReadGuard {
        fn drop(&mut self) {
            restore_read(&self.0);
        }
    }

    fn deny_read(path: &Path) -> DenyReadGuard {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o000);
            fs::set_permissions(path, perms).unwrap();
        }
        #[cfg(windows)]
        {
            // Deny Everyone (S-1-1-0) read on the directory so WalkBuilder cannot list it.
            let status = Command::new("icacls")
                .arg(path)
                .args(["/deny", "*S-1-1-0:(OI)(CI)R"])
                .status()
                .expect("icacls deny");
            assert!(status.success(), "icacls deny failed");
        }
        DenyReadGuard(path.to_path_buf())
    }

    fn restore_read(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(path, perms);
            }
        }
        #[cfg(windows)]
        {
            let _ = Command::new("icacls")
                .arg(path)
                .args(["/remove:d", "*S-1-1-0"])
                .status();
        }
    }
}
