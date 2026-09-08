//! Git worktree execution environments (Stage D / M9).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeEnvironment {
    pub id: String,
    pub repository: PathBuf,
    pub base_commit: String,
    pub branch: String,
    pub path: PathBuf,
    pub owner_task: String,
    pub orphaned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPreview {
    pub base_commit: String,
    pub target_commit: String,
    pub dirty: bool,
    pub patch: String,
    pub conflicts: Vec<String>,
}

pub struct WorktreeManager;
impl WorktreeManager {
    pub fn create(
        repository: &Path,
        destination: &Path,
        base: &str,
        branch: &str,
        task_id: &str,
    ) -> Result<WorktreeEnvironment, WorktreeError> {
        let repository = std::fs::canonicalize(repository)?;
        if destination.exists() {
            return Err(WorktreeError::DestinationExists(destination.into()));
        }
        let result = git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &destination.to_string_lossy(),
                base,
            ],
        );
        if let Err(error) = result {
            if destination.exists() {
                let _ = std::fs::remove_dir_all(destination);
            }
            return Err(error);
        }
        let path = std::fs::canonicalize(destination)?;
        let base_commit = git(&repository, &["rev-parse", base])?.trim().into();
        Ok(WorktreeEnvironment {
            id: format!("worktree-{task_id}"),
            repository,
            base_commit,
            branch: branch.into(),
            path,
            owner_task: task_id.into(),
            orphaned: false,
        })
    }

    pub fn status(environment: &WorktreeEnvironment) -> Result<String, WorktreeError> {
        git(&environment.path, &["status", "--porcelain=v1", "--branch"])
    }

    pub fn integration_preview(
        environment: &WorktreeEnvironment,
        target_ref: &str,
    ) -> Result<IntegrationPreview, WorktreeError> {
        let target_commit = git(&environment.repository, &["rev-parse", target_ref])?
            .trim()
            .into();
        let status = git(&environment.path, &["status", "--porcelain"])?;
        let patch = git(
            &environment.path,
            &[
                "diff",
                "--binary",
                &format!("{}..HEAD", environment.base_commit),
            ],
        )?;
        let conflicts = status
            .lines()
            .filter(|line| {
                line.starts_with("UU ") || line.starts_with("AA ") || line.starts_with("DD ")
            })
            .map(str::to_owned)
            .collect();
        Ok(IntegrationPreview {
            base_commit: environment.base_commit.clone(),
            target_commit,
            dirty: !status.trim().is_empty(),
            patch,
            conflicts,
        })
    }

    pub fn remove(
        environment: &WorktreeEnvironment,
        active_processes: bool,
    ) -> Result<(), WorktreeError> {
        if active_processes {
            return Err(WorktreeError::ActiveProcesses);
        }
        if !git(&environment.path, &["status", "--porcelain"])?
            .trim()
            .is_empty()
        {
            return Err(WorktreeError::Dirty);
        }
        git(
            &environment.repository,
            &["worktree", "remove", &environment.path.to_string_lossy()],
        )?;
        Ok(())
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| WorktreeError::Git(error.to_string()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(WorktreeError::Git(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ))
    }
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("worktree I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("git worktree operation failed: {0}")]
    Git(String),
    #[error("worktree destination exists: {0}")]
    DestinationExists(PathBuf),
    #[error("worktree has uncommitted changes")]
    Dirty,
    #[error("worktree still owns active processes")]
    ActiveProcesses,
}
