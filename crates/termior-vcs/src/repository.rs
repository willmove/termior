use crate::graph::{allocate_lanes, CommitNode, LaneCommit};
use git2::{DiffFormat, DiffOptions, Oid, Repository, Signature, Status, StatusOptions};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use termior_security::workspace::WorkspaceAuthRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeGroup {
    Unstaged,
    Staged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub group: ChangeGroup,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchState {
    pub name: Option<String>,
    pub detached: bool,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub id: String,
    pub summary: String,
    pub author: String,
    pub timestamp: i64,
    pub parents: Vec<String>,
    pub decorations: Vec<String>,
    pub lane: Option<LaneCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDiffHunk {
    pub header: String,
    /// A complete, apply-ready patch containing the file header and this hunk only.
    pub patch: String,
}

/// Split a git patch into independently applicable hunks while retaining its file header.
pub fn parse_diff_hunks(patch: &str) -> Vec<GitDiffHunk> {
    let mut file_header = String::new();
    let mut current_header = None::<String>;
    let mut current_body = String::new();
    let mut hunks = Vec::new();
    for line in patch.split_inclusive('\n') {
        if line.starts_with("@@") {
            if let Some(header) = current_header.take() {
                hunks.push(GitDiffHunk {
                    header,
                    patch: format!("{file_header}{current_body}"),
                });
                current_body.clear();
            }
            current_header = Some(line.trim_end().to_owned());
            current_body.push_str(line);
        } else if current_header.is_some() {
            current_body.push_str(line);
        } else {
            file_header.push_str(line);
        }
    }
    if let Some(header) = current_header {
        hunks.push(GitDiffHunk {
            header,
            patch: format!("{file_header}{current_body}"),
        });
    }
    hunks
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteOperation {
    Fetch,
    PullFfOnly,
    Push,
}

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("workspace is not authorized: {0}")]
    Unauthorized(PathBuf),
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git command failed: {0}")]
    Command(String),
    #[error("path is outside repository: {0}")]
    OutsideRepository(PathBuf),
}

pub struct GitRepository {
    repo: Repository,
    root: PathBuf,
}

impl std::fmt::Debug for GitRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitRepository")
            .field("root", &self.root)
            .finish()
    }
}

impl GitRepository {
    pub fn open(path: impl AsRef<Path>, auth: &WorkspaceAuthRegistry) -> Result<Self, GitError> {
        let repo = Repository::discover(path.as_ref())?;
        let root = repo.workdir().unwrap_or_else(|| repo.path()).to_path_buf();
        let root_string = root.to_string_lossy().replace('\\', "/");
        if !auth.is_authorized(&root_string) {
            return Err(GitError::Unauthorized(root));
        }
        Ok(Self { repo, root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn status(&self) -> Result<Vec<ChangedFile>, GitError> {
        let mut options = StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true);
        let statuses = self.repo.statuses(Some(&mut options))?;
        let mut files = Vec::new();
        for entry in statuses.iter() {
            let path = entry.path().unwrap_or_default().replace('\\', "/");
            let status = entry.status();
            if status.contains(Status::WT_NEW) {
                files.push(changed(&path, ChangeGroup::Untracked, "new"));
            }
            if status.intersects(
                Status::WT_MODIFIED
                    | Status::WT_DELETED
                    | Status::WT_RENAMED
                    | Status::WT_TYPECHANGE,
            ) {
                files.push(changed(
                    &path,
                    ChangeGroup::Unstaged,
                    status_name(status, false),
                ));
            }
            if status.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            ) {
                files.push(changed(
                    &path,
                    ChangeGroup::Staged,
                    status_name(status, true),
                ));
            }
        }
        files.sort_by(|a, b| (group_order(a.group), &a.path).cmp(&(group_order(b.group), &b.path)));
        Ok(files)
    }

    pub fn stage_file(&self, relative: impl AsRef<Path>) -> Result<(), GitError> {
        let relative = self.checked_relative(relative)?;
        let mut index = self.repo.index()?;
        if self.root.join(&relative).exists() {
            index.add_path(&relative)?;
        } else {
            index.remove_path(&relative)?;
        }
        index.write()?;
        Ok(())
    }

    pub fn unstage_file(&self, relative: impl AsRef<Path>) -> Result<(), GitError> {
        let relative = self.checked_relative(relative)?;
        self.run_git(["reset", "--", &relative.to_string_lossy()], None)
            .map(|_| ())
    }

    /// Apply an already reviewed unified patch to the index. Callers produce this patch from a
    /// selected hunk; no shell interpolation occurs.
    pub fn stage_hunk(&self, patch: &str) -> Result<(), GitError> {
        self.run_git(
            ["apply", "--cached", "--unidiff-zero", "-"],
            Some(patch.as_bytes()),
        )
        .map(|_| ())
    }

    pub fn unstage_hunk(&self, patch: &str) -> Result<(), GitError> {
        self.run_git(
            ["apply", "--cached", "--reverse", "--unidiff-zero", "-"],
            Some(patch.as_bytes()),
        )
        .map(|_| ())
    }

    pub fn discard_file(&self, relative: impl AsRef<Path>) -> Result<(), GitError> {
        let relative = self.checked_relative(relative)?;
        if self.repo.status_file(&relative)?.contains(Status::WT_NEW) {
            let path = self.root.join(&relative);
            if path.is_file() || path.is_symlink() {
                std::fs::remove_file(path)?;
                return Ok(());
            }
        }
        self.run_git(["checkout", "--", &relative.to_string_lossy()], None)
            .map(|_| ())
    }

    pub fn diff_file(&self, relative: impl AsRef<Path>, staged: bool) -> Result<String, GitError> {
        let relative = self.checked_relative(relative)?;
        let mut options = DiffOptions::new();
        options.pathspec(&relative);
        let diff = if staged {
            let head_tree = self
                .repo
                .head()
                .ok()
                .and_then(|head| head.peel_to_tree().ok());
            self.repo
                .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut options))?
        } else {
            self.repo.diff_index_to_workdir(None, Some(&mut options))?
        };
        let mut bytes = Vec::new();
        diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                bytes.push(line.origin() as u8);
            }
            bytes.extend_from_slice(line.content());
            true
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn commit(&self, message: &str) -> Result<Oid, GitError> {
        let signature = self
            .repo
            .signature()
            .or_else(|_| Signature::now("Termior User", "termior@localhost"))?;
        let mut index = self.repo.index()?;
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;
        let parent = self
            .repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|id| self.repo.find_commit(id).ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        Ok(self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )?)
    }

    pub fn branch_state(&self) -> Result<BranchState, GitError> {
        let head = self.repo.head()?;
        let detached = !head.is_branch();
        let name = if detached {
            None
        } else {
            Some(head.shorthand()?.to_owned())
        };
        let (ahead, behind) = if !detached {
            let local = head.target();
            let upstream = name
                .as_deref()
                .and_then(|name| self.repo.find_branch(name, git2::BranchType::Local).ok())
                .and_then(|branch| branch.upstream().ok())
                .and_then(|branch| branch.get().target());
            match (local, upstream) {
                (Some(local), Some(upstream)) => self.repo.graph_ahead_behind(local, upstream)?,
                _ => (0, 0),
            }
        } else {
            (0, 0)
        };
        Ok(BranchState {
            name,
            detached,
            ahead,
            behind,
        })
    }

    pub fn branches(&self) -> Result<Vec<String>, GitError> {
        let mut names = self
            .repo
            .branches(Some(git2::BranchType::Local))?
            .filter_map(Result::ok)
            .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    pub fn create_branch(&self, name: &str, switch: bool) -> Result<(), GitError> {
        validate_branch_name(name)?;
        let head = self.repo.head()?.peel_to_commit()?;
        self.repo.branch(name, &head, false)?;
        if switch {
            self.switch_branch(name)?;
        }
        Ok(())
    }

    pub fn switch_branch(&self, name: &str) -> Result<(), GitError> {
        validate_branch_name(name)?;
        self.run_git(["switch", name], None).map(|_| ())
    }

    pub fn remote(&self, operation: RemoteOperation) -> Result<String, GitError> {
        match operation {
            RemoteOperation::Fetch => self.run_git(["fetch", "--prune"], None),
            RemoteOperation::PullFfOnly => self.run_git(["pull", "--ff-only"], None),
            RemoteOperation::Push => {
                let state = self.branch_state()?;
                if let Some(branch) = state.name {
                    let has_upstream = self
                        .repo
                        .find_branch(&branch, git2::BranchType::Local)
                        .ok()
                        .and_then(|branch| branch.upstream().ok())
                        .is_some();
                    if has_upstream {
                        self.run_git(["push"], None)
                    } else {
                        self.run_git(["push", "--set-upstream", "origin", &branch], None)
                    }
                } else {
                    Err(GitError::Command("cannot push detached HEAD".into()))
                }
            }
        }
    }

    pub fn history(&self, limit: usize, query: Option<&str>) -> Result<Vec<CommitInfo>, GitError> {
        let mut refs_by_oid: HashMap<Oid, Vec<String>> = HashMap::new();
        if let Ok(refs) = self.repo.references() {
            for reference in refs.flatten() {
                if let (Some(oid), Ok(name)) = (reference.target(), reference.shorthand()) {
                    refs_by_oid.entry(oid).or_default().push(name.to_owned());
                }
            }
        }
        let mut walk = self.repo.revwalk()?;
        if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        let query = query.unwrap_or_default().to_ascii_lowercase();
        let mut commits = Vec::new();
        for oid in walk {
            let oid = oid?;
            let commit = self.repo.find_commit(oid)?;
            let summary = commit.summary()?.unwrap_or_default().to_owned();
            if !query.is_empty()
                && !summary.to_ascii_lowercase().contains(&query)
                && !oid.to_string().contains(&query)
            {
                continue;
            }
            commits.push(CommitInfo {
                id: oid.to_string(),
                summary,
                author: commit.author().name().unwrap_or_default().to_owned(),
                timestamp: commit.time().seconds(),
                parents: commit.parent_ids().map(|id| id.to_string()).collect(),
                decorations: refs_by_oid.remove(&oid).unwrap_or_default(),
                lane: None,
            });
            if commits.len() >= limit {
                break;
            }
        }
        let nodes: Vec<CommitNode> = commits
            .iter()
            .map(|commit| CommitNode {
                id: commit.id.clone(),
                parents: commit.parents.clone(),
            })
            .collect();
        for (commit, lane) in commits.iter_mut().zip(allocate_lanes(&nodes)) {
            commit.lane = Some(lane);
        }
        Ok(commits)
    }

    pub fn commit_files(&self, commit: &str) -> Result<Vec<String>, GitError> {
        let commit = self.repo.find_commit(Oid::from_str(commit)?)?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        let mut files = diff
            .deltas()
            .filter_map(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(files)
    }

    pub fn diff_commit_file(&self, commit: &str, relative: &str) -> Result<String, GitError> {
        let relative = self.checked_relative(relative)?;
        let commit = self.repo.find_commit(Oid::from_str(commit)?)?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
        let mut options = DiffOptions::new();
        options.pathspec(&relative);
        let diff =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut options))?;
        render_patch(&diff)
    }

    pub fn remote_commit_url(&self, commit: &str) -> Option<String> {
        let remote = self.repo.find_remote("origin").ok()?;
        let url = remote.url().ok()?;
        let base = if let Some(rest) = url.strip_prefix("git@") {
            let (host, path) = rest.split_once(':')?;
            format!("https://{host}/{}", path.trim_end_matches(".git"))
        } else {
            url.trim_end_matches(".git").to_owned()
        };
        Some(format!("{base}/commit/{commit}"))
    }

    fn checked_relative(&self, path: impl AsRef<Path>) -> Result<PathBuf, GitError> {
        let path = path.as_ref();
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(GitError::OutsideRepository(path.to_path_buf()));
        }
        Ok(path.to_path_buf())
    }

    fn run_git<const N: usize>(
        &self,
        args: [&str; N],
        stdin: Option<&[u8]>,
    ) -> Result<String, GitError> {
        let mut command = Command::new("git");
        command.arg("-C").arg(&self.root).args(args);
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input)?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(GitError::Command(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }
}

fn render_patch(diff: &git2::Diff<'_>) -> Result<String, GitError> {
    let mut bytes = Vec::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        true
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn changed(path: &str, group: ChangeGroup, status: &str) -> ChangedFile {
    ChangedFile {
        path: path.to_owned(),
        group,
        status: status.to_owned(),
    }
}

fn validate_branch_name(name: &str) -> Result<(), GitError> {
    let reference = format!("refs/heads/{name}");
    if name.trim().is_empty() || !git2::Reference::is_valid_name(&reference) {
        return Err(GitError::Command(format!("invalid branch name: {name}")));
    }
    Ok(())
}

fn status_name(status: Status, index: bool) -> &'static str {
    if status.intersects(if index {
        Status::INDEX_DELETED
    } else {
        Status::WT_DELETED
    }) {
        "deleted"
    } else if status.intersects(if index {
        Status::INDEX_RENAMED
    } else {
        Status::WT_RENAMED
    }) {
        "renamed"
    } else if status.intersects(if index {
        Status::INDEX_NEW
    } else {
        Status::WT_NEW
    }) {
        "new"
    } else {
        "modified"
    }
}

fn group_order(group: ChangeGroup) -> u8 {
    match group {
        ChangeGroup::Unstaged => 0,
        ChangeGroup::Staged => 1,
        ChangeGroup::Untracked => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo() -> (tempfile::TempDir, WorkspaceAuthRegistry, GitRepository) {
        let dir = tempfile::tempdir().unwrap();
        let raw = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let mut index = raw.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = raw.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        raw.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        // Authorize the same libgit2-normalized workdir representation that `open` validates.
        // This avoids platform aliases such as `/var` → `/private/var` and Windows temp paths.
        let root_path = raw.workdir().unwrap_or_else(|| raw.path()).to_path_buf();
        drop(tree);
        drop(raw);
        let root = root_path.to_string_lossy().replace('\\', "/");
        let auth = WorkspaceAuthRegistry::with_roots([root]);
        let repo = GitRepository::open(dir.path(), &auth).unwrap();
        (dir, auth, repo)
    }

    #[test]
    fn unauthorized_repository_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        assert!(matches!(
            GitRepository::open(dir.path(), &WorkspaceAuthRegistry::new()),
            Err(GitError::Unauthorized(_))
        ));
    }

    #[test]
    fn status_stage_diff_commit_history() {
        let (dir, _auth, repo) = repo();
        fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let status = repo.status().unwrap();
        assert!(status
            .iter()
            .any(|file| file.group == ChangeGroup::Unstaged));
        assert!(repo.diff_file("a.txt", false).unwrap().contains("+two"));
        repo.stage_file("a.txt").unwrap();
        assert!(repo
            .status()
            .unwrap()
            .iter()
            .any(|file| file.group == ChangeGroup::Staged));
        repo.commit("second").unwrap();
        let history = repo.history(10, None).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].summary, "second");
        assert!(history.iter().all(|commit| commit.lane.is_some()));
        let files = repo.commit_files(&history[0].id).unwrap();
        assert_eq!(files, vec!["a.txt"]);
        assert!(repo
            .diff_commit_file(&history[0].id, "a.txt")
            .unwrap()
            .contains("+two"));
    }

    #[test]
    fn patch_parser_keeps_file_header_on_each_hunk() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n@@ -4,0 +5 @@\n+tail\n";
        let hunks = parse_diff_hunks(patch);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].header, "@@ -1 +1 @@");
        assert!(hunks
            .iter()
            .all(|hunk| hunk.patch.starts_with("diff --git")));
        assert!(hunks[1].patch.contains("@@ -4,0 +5 @@"));
        assert!(!hunks[1].patch.contains("@@ -1 +1 @@"));
    }

    #[test]
    fn individual_worktree_hunk_can_be_staged() {
        let (dir, _auth, repo) = repo();
        let original = (0..20)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(dir.path().join("a.txt"), &original).unwrap();
        repo.stage_file("a.txt").unwrap();
        repo.commit("lines").unwrap();
        let changed = original
            .replace("line 1\n", "line one\n")
            .replace("line 18\n", "line eighteen\n");
        fs::write(dir.path().join("a.txt"), changed).unwrap();
        let hunks = parse_diff_hunks(&repo.diff_file("a.txt", false).unwrap());
        assert_eq!(hunks.len(), 2);
        repo.stage_hunk(&hunks[0].patch).unwrap();
        let status = repo.status().unwrap();
        assert!(status
            .iter()
            .any(|file| file.path == "a.txt" && file.group == ChangeGroup::Staged));
        assert!(status
            .iter()
            .any(|file| file.path == "a.txt" && file.group == ChangeGroup::Unstaged));
    }

    #[test]
    fn confirmed_discard_removes_an_untracked_file() {
        let (dir, _auth, repo) = repo();
        let path = dir.path().join("scratch.txt");
        fs::write(&path, "temporary").unwrap();
        repo.discard_file("scratch.txt").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn remote_url_converts_ssh() {
        let (_dir, _auth, repo) = repo();
        repo.repo
            .remote("origin", "git@github.com:acme/demo.git")
            .unwrap();
        assert_eq!(
            repo.remote_commit_url("abc").as_deref(),
            Some("https://github.com/acme/demo/commit/abc")
        );
    }

    #[test]
    fn deleted_files_and_branches_are_writable() {
        let (dir, _auth, repo) = repo();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        repo.stage_file("a.txt").unwrap();
        assert!(repo.status().unwrap().iter().any(|file| {
            file.path == "a.txt" && file.group == ChangeGroup::Staged && file.status == "deleted"
        }));

        let initial_branch = repo.branch_state().unwrap().name.unwrap();
        repo.create_branch("feature/test", true).unwrap();
        assert_eq!(
            repo.branch_state().unwrap().name.as_deref(),
            Some("feature/test")
        );
        assert!(repo
            .branches()
            .unwrap()
            .iter()
            .any(|name| name == "feature/test"));
        repo.switch_branch(&initial_branch).unwrap();
        assert_eq!(
            repo.branch_state().unwrap().name.as_deref(),
            Some(initial_branch.as_str())
        );
    }
}
