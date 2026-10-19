//! Worktree tasks read and write only the worktree checkout (Stage D task 4.3).
//!
//! The tool registry is authorized for both the main checkout and the worktree, which is the
//! shape Composer has after the workspace is opened. Path traversal, symlinks, and hardlinks
//! back into the main checkout must still fail, and the main files must stay unchanged.

use std::path::Path;
use std::process::Command;
use termior_agent_host::ExecutionEnvironment;
use termior_ai::context_engine::{RepoMap, RuleResolver};
use termior_ai::ToolRegistry;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_vcs::WorktreeManager;

const MAIN_SECRET: &str = "main-workspace-secret";
const WORKTREE_BODY: &str = "worktree-only-body";

#[test]
fn worktree_tools_rules_and_repo_map_cannot_touch_the_main_checkout() {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("main");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "core.autocrlf", "false"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Termior Test"]);
    std::fs::write(repository.join("shared.txt"), "base\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "base"]);

    let destination = root.path().join("checkout");
    let environment =
        WorktreeManager::create(&repository, &destination, "HEAD", "task-branch", "task-17")
            .unwrap();

    std::fs::write(repository.join("secret.txt"), MAIN_SECRET).unwrap();
    std::fs::write(repository.join("a.txt"), "user-main\n").unwrap();
    std::fs::write(repository.join("AGENTS.md"), "MAIN_ONLY\n").unwrap();
    std::fs::write(repository.join("only_main.rs"), "fn only_main() {}\n").unwrap();
    std::fs::write(environment.path.join("inside.txt"), WORKTREE_BODY).unwrap();
    std::fs::write(environment.path.join("AGENTS.md"), "WORKTREE_ONLY\n").unwrap();
    std::fs::write(
        environment.path.join("only_worktree.rs"),
        "fn only_worktree() {}\n",
    )
    .unwrap();
    std::fs::hard_link(
        repository.join("secret.txt"),
        environment.path.join("leaked-hardlink"),
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        repository.join("secret.txt"),
        environment.path.join("escape-link"),
    )
    .unwrap();

    let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([
        repository.display().to_string(),
        environment.path.display().to_string(),
    ]));
    let executor = ExecutionEnvironment::worktree(&environment)
        .tool_executor(registry)
        .unwrap();

    assert_eq!(
        executor
            .execute_auto("read_file", r#"{"path":"inside.txt"}"#)
            .unwrap(),
        WORKTREE_BODY
    );
    let shared = std::fs::read_to_string(environment.path.join("shared.txt")).unwrap();
    assert_eq!(
        executor
            .execute_auto("read_file", r#"{"path":"shared.txt"}"#)
            .unwrap(),
        shared,
        "read_file must return the worktree checkout, not the main working copy"
    );
    assert_eq!(shared.replace("\r\n", "\n"), "base\n");

    for args in [
        serde_json::json!({"path": repository.join("secret.txt")}).to_string(),
        serde_json::json!({"path": "../main/secret.txt"}).to_string(),
        serde_json::json!({"path": "../main/a.txt"}).to_string(),
    ] {
        let error = executor
            .execute_auto("read_file", &args)
            .expect_err("escaped read");
        assert!(
            !error.to_string().contains(MAIN_SECRET),
            "read error leaked main contents: {error}"
        );
    }
    let listing = executor
        .execute_auto("list_directory", r#"{"path":"."}"#)
        .unwrap();
    assert!(listing.contains("inside.txt"), "{listing}");
    assert!(!listing.contains("secret.txt"), "{listing}");
    assert!(executor
        .execute_auto("list_directory", r#"{"path":".."}"#)
        .is_err());
    let grep = executor
        .execute_auto("fs_grep", r#"{"query":"main-workspace-secret"}"#)
        .unwrap();
    assert!(
        !grep.contains(MAIN_SECRET),
        "grep read the main checkout: {grep}"
    );

    let write = serde_json::json!({
        "path": repository.join("a.txt"),
        "content": "pwned\n",
    })
    .to_string();
    assert!(executor.execute_approved("write_file", &write).is_err());
    assert!(executor
        .execute_approved(
            "write_file",
            r#"{"path":"../main/a.txt","content":"pwned\n"}"#,
        )
        .is_err());
    assert!(executor
        .execute_approved("run_command", r#"{"command":"pwd","cwd":".."}"#)
        .is_err());

    #[cfg(unix)]
    {
        let error = executor
            .execute_auto("read_file", r#"{"path":"escape-link"}"#)
            .expect_err("symlink read");
        assert!(!error.to_string().contains(MAIN_SECRET));
        assert!(executor
            .execute_approved(
                "write_file",
                r#"{"path":"escape-link","content":"pwned\n"}"#,
            )
            .is_err());
    }
    let error = executor
        .execute_auto("read_file", r#"{"path":"leaked-hardlink"}"#)
        .expect_err("hardlink read");
    assert!(!error.to_string().contains(MAIN_SECRET));

    assert_eq!(
        std::fs::read_to_string(repository.join("secret.txt")).unwrap(),
        MAIN_SECRET
    );
    assert_eq!(
        std::fs::read_to_string(repository.join("a.txt")).unwrap(),
        "user-main\n"
    );

    let pwd = if cfg!(windows) { "cd" } else { "pwd" };
    let output = executor
        .execute_approved("run_command", &format!(r#"{{"command":"{pwd}"}}"#))
        .unwrap();
    let canonical_worktree = std::fs::canonicalize(&environment.path).unwrap();
    assert!(
        output_contains_path(&output, &canonical_worktree)
            || output_contains_path(&output, &environment.path),
        "command cwd was not the worktree: {output}"
    );

    #[cfg(unix)]
    {
        std::fs::rename(
            environment.path.join("AGENTS.md"),
            environment.path.join("AGENTS.md.real"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            repository.join("AGENTS.md"),
            environment.path.join("AGENTS.md"),
        )
        .unwrap();
        let escaped = RuleResolver::new(&environment.path)
            .unwrap()
            .for_path(&environment.path);
        match escaped {
            Err(_) => {}
            Ok(rules) => assert!(
                rules.iter().all(|rule| !rule.content.contains("MAIN_ONLY")),
                "symlink rule resolved into the main checkout"
            ),
        }
        std::fs::remove_file(environment.path.join("AGENTS.md")).unwrap();
        std::fs::rename(
            environment.path.join("AGENTS.md.real"),
            environment.path.join("AGENTS.md"),
        )
        .unwrap();
    }

    let rules = RuleResolver::new(&environment.path)
        .unwrap()
        .for_path(&environment.path)
        .unwrap();
    let rule_text = rules
        .iter()
        .map(|rule| rule.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rule_text.contains("WORKTREE_ONLY"));
    assert!(!rule_text.contains("MAIN_ONLY"));
    assert!(rules
        .iter()
        .all(|rule| rule.path.starts_with(&environment.path)));

    let mut map = RepoMap::default();
    map.index_workspace(&environment.path).unwrap();
    let worktree_root = std::fs::canonicalize(&environment.path).unwrap();
    let worktree_hits = map.relevant("only_worktree", 2_000);
    assert!(worktree_hits.iter().any(|entry| {
        entry.path.starts_with(&worktree_root)
            && entry.symbols.iter().any(|symbol| symbol == "only_worktree")
    }));
    let main_hits = map.relevant("only_main", 2_000);
    assert!(
        main_hits.iter().all(|entry| {
            entry.path.starts_with(&worktree_root)
                && !entry.path.ends_with("only_main.rs")
                && entry.symbols.iter().all(|symbol| symbol != "only_main")
        }),
        "repo map included the main checkout: {main_hits:?}"
    );
}

fn output_contains_path(output: &str, path: &Path) -> bool {
    let display = path.display().to_string();
    let stripped = display
        .trim_start_matches(r"\\?\")
        .trim_start_matches("//?/")
        .to_string();
    let variants = [
        display.clone(),
        stripped.clone(),
        stripped.replace('\\', "/"),
        stripped.replace('/', "\\"),
    ];
    let folded = output.to_ascii_lowercase();
    variants
        .iter()
        .any(|variant| folded.contains(&variant.to_ascii_lowercase()))
}

fn git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}
