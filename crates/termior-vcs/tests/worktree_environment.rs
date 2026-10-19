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

#[test]
fn fast_forward_merge_back_applies_and_validation_failure_does_not() {
    let fixture = repo_with_worktree();
    let environment = &fixture.environment;
    std::fs::write(environment.path.join("a.txt"), "from-worktree\n").unwrap();
    git(&environment.path, &["add", "a.txt"]);
    git(&environment.path, &["commit", "-m", "task"]);
    let blocked = WorktreeManager::merge_back(environment, "HEAD", false).unwrap();
    assert!(matches!(
        blocked,
        MergeBack::Review {
            reason: MergeBackReason::ValidationFailed,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(fixture.repository.join("a.txt")).unwrap(),
        "base\n"
    );
    let applied = WorktreeManager::merge_back(environment, "HEAD", true).unwrap();
    assert!(matches!(
        applied,
        MergeBack::Applied {
            method: MergeBackMethod::FastForward,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(fixture.repository.join("a.txt")).unwrap(),
        "from-worktree\n"
    );
}

#[test]
fn clean_diverged_merge_back_cherry_picks() {
    let fixture = repo_with_worktree();
    std::fs::write(fixture.repository.join("b.txt"), "user-side\n").unwrap();
    git(&fixture.repository, &["add", "b.txt"]);
    git(&fixture.repository, &["commit", "-m", "user side"]);
    std::fs::write(fixture.environment.path.join("c.txt"), "task-side\n").unwrap();
    git(&fixture.environment.path, &["add", "c.txt"]);
    git(&fixture.environment.path, &["commit", "-m", "task side"]);
    let outcome = WorktreeManager::merge_back(&fixture.environment, "HEAD", true).unwrap();
    assert!(
        matches!(
            outcome,
            MergeBack::Applied {
                method: MergeBackMethod::CherryPick,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repository.join("a.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repository.join("b.txt")).unwrap(),
        "user-side\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repository.join("c.txt")).unwrap(),
        "task-side\n"
    );
}

#[test]
fn merge_back_conflict_preserves_user_changes() {
    let fixture = repo_with_worktree();
    let repository = &fixture.repository;
    let environment = &fixture.environment;
    std::fs::write(repository.join("a.txt"), "user-commit\n").unwrap();
    git(repository, &["add", "a.txt"]);
    git(repository, &["commit", "-m", "user"]);
    let user_head = rev_parse(repository);
    std::fs::write(environment.path.join("a.txt"), "task-commit\n").unwrap();
    git(&environment.path, &["add", "a.txt"]);
    git(&environment.path, &["commit", "-m", "task"]);

    let outcome = WorktreeManager::merge_back(environment, "HEAD", true).unwrap();
    assert!(
        matches!(
            outcome,
            MergeBack::Review {
                reason: MergeBackReason::Conflict,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("a.txt")).unwrap(),
        "user-commit\n"
    );
    assert_eq!(rev_parse(repository), user_head);
    assert!(repository.join("a.txt").is_file());
    assert!(!repository.join(".git").join("MERGE_HEAD").exists());

    std::fs::write(repository.join("notes.txt"), "user scratch\n").unwrap();
    let dirty = WorktreeManager::merge_back(environment, "HEAD", true).unwrap();
    assert!(matches!(
        dirty,
        MergeBack::Review {
            reason: MergeBackReason::DirtyTarget,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(repository.join("a.txt")).unwrap(),
        "user-commit\n"
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("notes.txt")).unwrap(),
        "user scratch\n"
    );
    assert_eq!(rev_parse(repository), user_head);
}

struct Checkout {
    _root: tempfile::TempDir,
    repository: std::path::PathBuf,
    environment: WorktreeEnvironment,
}

fn repo_with_worktree() -> Checkout {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("main");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Termior Test"]);
    std::fs::write(repository.join("a.txt"), "base\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "base"]);
    let destination = root.path().join("checkout");
    let environment =
        WorktreeManager::create(&repository, &destination, "HEAD", "task-branch", "task-1")
            .unwrap();
    Checkout {
        _root: root,
        repository,
        environment,
    }
}

fn rev_parse(path: &std::path::Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
