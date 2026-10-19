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

/// How a conflict-free worktree was applied onto the target checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBackMethod {
    FastForward,
    CherryPick,
}

impl std::fmt::Display for MergeBackMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::FastForward => "fast-forward",
            Self::CherryPick => "cherry-pick",
        })
    }
}

/// Why automatic merge-back stopped and the existing Git review flow should open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeBackReason {
    ValidationFailed,
    DirtyWorktree,
    DirtyTarget,
    Conflict,
    NotSafe,
}

impl std::fmt::Display for MergeBackReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ValidationFailed => "validation-failed",
            Self::DirtyWorktree => "dirty-worktree",
            Self::DirtyTarget => "dirty-target",
            Self::Conflict => "conflict",
            Self::NotSafe => "not-safe",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBack {
    Applied {
        method: MergeBackMethod,
        commit: String,
    },
    Review {
        reason: MergeBackReason,
        preview: IntegrationPreview,
    },
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

    /// Apply the worktree onto `target_ref` only when that is a clean fast-forward or a
    /// conflict-free cherry-pick. Every other case returns [`MergeBack::Review`] and leaves
    /// both checkouts untouched, including uncommitted user edits in the target.
    pub fn merge_back(
        environment: &WorktreeEnvironment,
        target_ref: &str,
        validation_passed: bool,
    ) -> Result<MergeBack, WorktreeError> {
        let mut preview = Self::integration_preview(environment, target_ref)?;
        let repository = &environment.repository;
        let target_dirty = !git(repository, &["status", "--porcelain"])?
            .trim()
            .is_empty();
        let review = |reason: MergeBackReason, preview: IntegrationPreview| MergeBack::Review {
            reason,
            preview,
        };
        if !validation_passed {
            return Ok(review(MergeBackReason::ValidationFailed, preview));
        }
        if preview.dirty {
            return Ok(review(MergeBackReason::DirtyWorktree, preview));
        }
        if target_dirty {
            return Ok(review(MergeBackReason::DirtyTarget, preview));
        }
        if git_succeeded(repository, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
            || git_succeeded(
                repository,
                &["rev-parse", "-q", "--verify", "CHERRY_PICK_HEAD"],
            )?
        {
            return Ok(review(MergeBackReason::NotSafe, preview));
        }

        let head = git(&environment.path, &["rev-parse", "HEAD"])?
            .trim()
            .to_owned();
        let checked_out = git(repository, &["rev-parse", "HEAD"])?.trim().to_owned();
        if checked_out != preview.target_commit {
            return Ok(review(MergeBackReason::NotSafe, preview));
        }
        if git_succeeded(
            repository,
            &["merge-base", "--is-ancestor", &head, &checked_out],
        )? {
            return Ok(MergeBack::Applied {
                method: MergeBackMethod::FastForward,
                commit: checked_out,
            });
        }
        if git_succeeded(
            repository,
            &["merge-base", "--is-ancestor", &preview.target_commit, &head],
        )? {
            if git(repository, &["merge", "--ff-only", &head]).is_err() {
                return Ok(review(MergeBackReason::NotSafe, preview));
            }
            let commit = git(repository, &["rev-parse", "HEAD"])?.trim().to_owned();
            return Ok(MergeBack::Applied {
                method: MergeBackMethod::FastForward,
                commit,
            });
        }

        if let Some(conflicts) = merge_tree_conflicts(repository, &preview.target_commit, &head)? {
            preview.conflicts = conflicts;
            return Ok(review(MergeBackReason::Conflict, preview));
        }
        let range = format!("{}..{head}", preview.target_commit);
        match git(
            repository,
            &["rev-list", "--merges", "--max-count=1", &range],
        ) {
            Ok(text) if text.trim().is_empty() => {}
            _ => return Ok(review(MergeBackReason::NotSafe, preview)),
        }

        let before = checked_out;
        if git(repository, &["cherry-pick", &range]).is_err() {
            let _ = git(repository, &["cherry-pick", "--abort"]);
            let restored = git(repository, &["rev-parse", "HEAD"])
                .map(|commit| commit.trim() == before)
                .unwrap_or(false);
            if !restored {
                return Err(WorktreeError::Git(
                    "cherry-pick abort did not restore the target checkout".into(),
                ));
            }
            preview.conflicts = vec!["cherry-pick".into()];
            return Ok(review(MergeBackReason::Conflict, preview));
        }
        let commit = git(repository, &["rev-parse", "HEAD"])?.trim().to_owned();
        Ok(MergeBack::Applied {
            method: MergeBackMethod::CherryPick,
            commit,
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

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = git_command(cwd, args)
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

/// `true` when git exits 0. Exit code 1 is `false` (ancestor checks, missing refs).
fn git_succeeded(cwd: &Path, args: &[&str]) -> Result<bool, WorktreeError> {
    let status = git_command(cwd, args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| WorktreeError::Git(error.to_string()))?;
    if status.success() {
        Ok(true)
    } else if status.code() == Some(1) {
        Ok(false)
    } else {
        Err(WorktreeError::Git(format!(
            "git {args:?} exited with {status}"
        )))
    }
}

/// `Ok(None)` when the two trees merge cleanly. `Ok(Some)` lists conflicted paths and does not
/// touch either checkout. A git that cannot run `merge-tree --write-tree` is not a safe path.
fn merge_tree_conflicts(
    repository: &Path,
    target: &str,
    head: &str,
) -> Result<Option<Vec<String>>, WorktreeError> {
    let output = git_command(
        repository,
        &["merge-tree", "--write-tree", "--name-only", target, head],
    )
    .output()
    .map_err(|error| WorktreeError::Git(error.to_string()))?;
    if output.status.success() {
        return Ok(None);
    }
    if output.status.code() == Some(1) {
        let names = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        return Ok(Some(if names.is_empty() {
            vec!["conflict".into()]
        } else {
            names
        }));
    }
    Err(WorktreeError::Git(
        String::from_utf8_lossy(&output.stderr).trim().into(),
    ))
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
