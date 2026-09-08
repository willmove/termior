use std::process::Command;
use termior_vcs::*;

fn git(path: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn worktree_creation_binds_task_and_removal_checks_dirty_and_processes() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init"]);
    git(root.path(), &["config", "user.email", "test@example.com"]);
    git(root.path(), &["config", "user.name", "Termior Test"]);
    std::fs::write(root.path().join("a.txt"), "base\n").unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-m", "base"]);
    let destination_parent = tempfile::tempdir().unwrap();
    let destination = destination_parent.path().join("checkout");
    let environment = WorktreeManager::create(
        root.path(),
        &destination,
        "HEAD",
        "termior-test-branch",
        "task-1",
    )
    .unwrap();
    assert_eq!(environment.owner_task, "task-1");
    assert_eq!(
        std::fs::read_to_string(environment.path.join("a.txt"))
            .unwrap()
            .trim(),
        "base"
    );
    assert!(matches!(
        WorktreeManager::remove(&environment, true),
        Err(WorktreeError::ActiveProcesses)
    ));
    std::fs::write(environment.path.join("a.txt"), "dirty\n").unwrap();
    assert!(matches!(
        WorktreeManager::remove(&environment, false),
        Err(WorktreeError::Dirty)
    ));
    git(&environment.path, &["restore", "a.txt"]);
    WorktreeManager::remove(&environment, false).unwrap();
    assert!(!destination.exists());
}
