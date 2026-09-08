//! Git source-control subsystem (FR-VCS).

#![forbid(unsafe_code)]

mod graph;
mod repository;
mod worktree;

pub use graph::{allocate_lanes, CommitNode, LaneCommit};
pub use repository::{
    parse_diff_hunks, BranchState, ChangeGroup, ChangedFile, CommitInfo, GitDiffHunk, GitError,
    GitRepository, RemoteOperation,
};
pub use worktree::{IntegrationPreview, WorktreeEnvironment, WorktreeError, WorktreeManager};
