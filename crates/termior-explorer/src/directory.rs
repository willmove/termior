//! Shallow, bounded listing for the user-owned local SFTP browser. Unlike the
//! workspace index, a file manager must include dotfiles and ignored files.
use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

/// Call on a background executor. No recursion or workspace-context mutation.
pub fn list_directory(path: &Path) -> io::Result<Vec<DirectoryEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        if entries.len() == 20_000 {
            return Err(io::Error::other("目录超过 20,000 项，请选择更小的目录"));
        }
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path(),
            is_dir: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            size: metadata.len(),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn includes_hidden_ignored_files_and_sorts_directories_first() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(dir.path().join("ignored"), "123").unwrap();
        std::fs::write(dir.path().join("中文 [1].txt"), "ok").unwrap();
        std::fs::create_dir(dir.path().join("z-dir")).unwrap();
        let entries = list_directory(dir.path()).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].name, "z-dir");
        assert!(entries[0].is_dir);
        assert_eq!(
            entries.iter().find(|e| e.name == "ignored").unwrap().size,
            3
        );
        assert!(entries.iter().all(|e| e.path.parent() == Some(dir.path())));
        assert!(list_directory(&dir.path().join("missing")).is_err());
    }
}
