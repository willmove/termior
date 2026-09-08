//! Content-addressed file checkpoints with conflict-aware restore planning.

use crate::{atomic_write, atomic_write_bytes};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointFile {
    pub relative_path: PathBuf,
    pub before_hash: Option<String>,
    pub expected_after_hash: Option<String>,
    pub blob: Option<String>,
    pub existed: bool,
    #[serde(default)]
    pub unix_mode: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointManifest {
    pub id: String,
    pub task_id: String,
    pub sequence: u64,
    pub change_set_id: String,
    pub tool_call_id: String,
    pub files: Vec<CheckpointFile>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    Restore {
        relative_path: PathBuf,
        blob: String,
        unix_mode: Option<u32>,
    },
    DeleteCreated {
        relative_path: PathBuf,
    },
    SkipConflict {
        relative_path: PathBuf,
    },
    AlreadyRestored {
        relative_path: PathBuf,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub checkpoint_id: String,
    pub actions: Vec<RestoreAction>,
}
pub struct CheckpointStore {
    root: PathBuf,
}
impl CheckpointStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn create(
        &self,
        workspace: &Path,
        task_id: &str,
        sequence: u64,
        change_set_id: &str,
        tool_call_id: &str,
        expected_after: &[(PathBuf, Option<Vec<u8>>)],
    ) -> Result<CheckpointManifest, CheckpointError> {
        std::fs::create_dir_all(self.root.join("blobs"))?;
        std::fs::create_dir_all(self.root.join("manifests"))?;
        let workspace = std::fs::canonicalize(workspace)?;
        let mut files = Vec::new();
        for (relative, after) in expected_after {
            let path = resolve_relative(&workspace, relative)?;
            reject_links(&path)?;
            let before = std::fs::read(&path).ok();
            let before_hash = before.as_deref().map(digest);
            let blob = if let Some(bytes) = &before {
                let hash = digest(bytes);
                let blob_path = self.root.join("blobs").join(&hash);
                if !blob_path.exists() {
                    atomic_write_bytes(&blob_path, bytes)?;
                }
                Some(hash)
            } else {
                None
            };
            files.push(CheckpointFile {
                relative_path: relative.clone(),
                before_hash,
                expected_after_hash: after.as_deref().map(digest),
                blob,
                existed: before.is_some(),
                unix_mode: unix_mode(&path),
            });
        }
        let id = format!("checkpoint-{task_id}-{sequence}");
        let manifest = CheckpointManifest {
            id: id.clone(),
            task_id: task_id.into(),
            sequence,
            change_set_id: change_set_id.into(),
            tool_call_id: tool_call_id.into(),
            files,
        };
        atomic_write(
            &self.root.join("manifests").join(format!("{id}.json")),
            &serde_json::to_string_pretty(&manifest)?,
        )?;
        Ok(manifest)
    }

    pub fn load_manifest(&self, id: &str) -> Result<CheckpointManifest, CheckpointError> {
        let bytes = std::fs::read(self.root.join("manifests").join(format!("{id}.json")))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn list_manifests(&self) -> Result<Vec<CheckpointManifest>, CheckpointError> {
        let directory = self.root.join("manifests");
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut manifests = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            manifests.push(serde_json::from_slice(&std::fs::read(path)?)?);
        }
        manifests.sort_by(|left: &CheckpointManifest, right: &CheckpointManifest| {
            right
                .sequence
                .cmp(&left.sequence)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(manifests)
    }
    pub fn plan_restore(
        &self,
        workspace: &Path,
        manifest: &CheckpointManifest,
    ) -> Result<RestorePlan, CheckpointError> {
        let workspace = std::fs::canonicalize(workspace)?;
        let mut actions = Vec::new();
        for file in &manifest.files {
            let path = resolve_relative(&workspace, &file.relative_path)?;
            reject_links(&path)?;
            let current = std::fs::read(&path).ok().as_deref().map(digest);
            let action = if current == file.before_hash {
                RestoreAction::AlreadyRestored {
                    relative_path: file.relative_path.clone(),
                }
            } else if current == file.expected_after_hash {
                match (&file.blob, file.existed) {
                    (Some(blob), true) => RestoreAction::Restore {
                        relative_path: file.relative_path.clone(),
                        blob: blob.clone(),
                        unix_mode: file.unix_mode,
                    },
                    (None, false) => RestoreAction::DeleteCreated {
                        relative_path: file.relative_path.clone(),
                    },
                    _ => RestoreAction::SkipConflict {
                        relative_path: file.relative_path.clone(),
                    },
                }
            } else {
                RestoreAction::SkipConflict {
                    relative_path: file.relative_path.clone(),
                }
            };
            actions.push(action);
        }
        Ok(RestorePlan {
            checkpoint_id: manifest.id.clone(),
            actions,
        })
    }
    pub fn apply_restore(
        &self,
        workspace: &Path,
        plan: &RestorePlan,
    ) -> Result<(), CheckpointError> {
        let workspace = std::fs::canonicalize(workspace)?;
        for action in &plan.actions {
            match action {
                RestoreAction::Restore {
                    relative_path,
                    blob,
                    unix_mode,
                } => {
                    let bytes = std::fs::read(self.root.join("blobs").join(blob))?;
                    let path = resolve_relative(&workspace, relative_path)?;
                    atomic_write_bytes(&path, &bytes)?;
                    restore_unix_mode(&path, *unix_mode)?;
                }
                RestoreAction::DeleteCreated { relative_path } => {
                    let path = resolve_relative(&workspace, relative_path)?;
                    if path.exists() {
                        std::fs::remove_file(path)?;
                    }
                }
                RestoreAction::SkipConflict { .. } | RestoreAction::AlreadyRestored { .. } => {}
            }
        }
        Ok(())
    }
}
fn reject_links(path: &Path) -> Result<(), CheckpointError> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(CheckpointError::UnsupportedLink(path.to_path_buf()));
        }
        if hard_link_count(path, &metadata) > 1 {
            return Err(CheckpointError::UnsupportedHardLink(path.to_path_buf()));
        }
    }
    Ok(())
}

fn resolve_relative(workspace: &Path, relative: &Path) -> Result<PathBuf, CheckpointError> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(CheckpointError::EscapedPath(relative.to_path_buf()));
    }
    let candidate = workspace.join(relative);
    let boundary = if candidate.exists() {
        std::fs::canonicalize(&candidate)?
    } else {
        let parent = candidate.parent().unwrap_or(workspace);
        std::fs::canonicalize(parent)?.join(
            candidate
                .file_name()
                .ok_or_else(|| CheckpointError::EscapedPath(relative.to_path_buf()))?,
        )
    };
    if !boundary.starts_with(workspace) {
        return Err(CheckpointError::EscapedPath(relative.to_path_buf()));
    }
    Ok(candidate)
}

#[cfg(unix)]
fn hard_link_count(_path: &Path, metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(windows)]
fn hard_link_count(path: &Path, _metadata: &std::fs::Metadata) -> u64 {
    std::process::Command::new("fsutil")
        .args(["hardlink", "list"])
        .arg(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count() as u64
        })
        // The probe is advisory on Windows versions where fsutil is unavailable; atomic replace
        // still prevents writes through an existing hard link from mutating its sibling.
        .unwrap_or(1)
}

#[cfg(not(any(unix, windows)))]
fn hard_link_count(_path: &Path, _metadata: &std::fs::Metadata) -> u64 {
    1
}

#[cfg(unix)]
fn unix_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode())
}

#[cfg(not(unix))]
fn unix_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn restore_unix_mode(path: &Path, mode: Option<u32>) -> Result<(), CheckpointError> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restore_unix_mode(_path: &Path, _mode: Option<u32>) -> Result<(), CheckpointError> {
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("checkpoint I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("checkpoint JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("checkpoint atomic write failed: {0}")]
    Atomic(#[from] crate::AtomicWriteError),
    #[error("symbolic links are not checkpointed: {0}")]
    UnsupportedLink(PathBuf),
    #[error("hard-linked files are not checkpointed: {0}")]
    UnsupportedHardLink(PathBuf),
    #[error("checkpoint path escapes workspace: {0}")]
    EscapedPath(PathBuf),
}
