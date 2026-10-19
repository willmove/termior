//! Repo map invalidation, authorization, relevance and background indexing (Stage C 3.3).

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use termior_ai::context_engine::{ContextError, HeuristicTokenEstimator, TokenEstimator};
use termior_ai::repo_map::{RepoMap, RepoMapConfig, RepoMapService, SymbolExtraction, SymbolKind};
use termior_security::workspace::WorkspaceAuthRegistry;

fn write(root: &Path, relative: &str, content: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Push mtimes out of the racy window so metadata alone proves a file unchanged.
fn age(root: &Path, relative: &str) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(relative))
        .unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

fn sample_repo(root: &Path) {
    write(
        root,
        "src/runtime.rs",
        "pub struct AgentRuntime { budget: Budget }\n\
         impl AgentRuntime {\n    pub fn drive(&mut self, steps: u32) -> Outcome {\n        retry_with_backoff(steps)\n    }\n}\n",
    );
    write(
        root,
        "src/retry.rs",
        "pub fn retry_with_backoff(steps: u32) -> Outcome { Outcome::Done }\npub enum Outcome { Done }\n",
    );
    write(
        root,
        "src/budget.rs",
        "pub struct Budget { pub tokens: u64 }\n",
    );
    write(
        root,
        "src/caller.rs",
        "fn start(runtime: &mut AgentRuntime) { runtime.drive(3); }\n",
    );
    write(
        root,
        "web/panel.ts",
        "export class Panel { render(): void {} }\n",
    );
    write(
        root,
        "tools/report.py",
        "class Report:\n    def render(self):\n        pass\n",
    );
    write(root, "README.md", "Termior agent workbench.\n");
}

fn relatives(slice: &termior_ai::repo_map::RepoSlice) -> Vec<&str> {
    slice
        .entries
        .iter()
        .map(|entry| entry.relative.as_str())
        .collect()
}

#[test]
fn tree_sitter_outline_drives_ranking_signatures_and_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    sample_repo(dir.path());
    let map = RepoMap::build(dir.path(), RepoMapConfig::default()).unwrap();
    assert_eq!(map.len(), 7);

    let runtime = map.entry("src/runtime.rs").unwrap();
    assert_eq!(runtime.extraction(), SymbolExtraction::TreeSitter);
    assert!(runtime.defines("AgentRuntime"));
    assert!(runtime.references("retry_with_backoff"));
    let drive = runtime
        .symbols()
        .iter()
        .find(|symbol| symbol.name == "drive")
        .unwrap();
    assert_eq!(drive.kind, SymbolKind::Method);
    assert_eq!(drive.container.as_deref(), Some("AgentRuntime"));
    assert_eq!(
        map.entry("README.md").unwrap().extraction(),
        SymbolExtraction::Lexical
    );

    let slice = map.slice("Why does AgentRuntime stop early?", 400);
    let order = relatives(&slice);
    assert_eq!(order.first(), Some(&"src/runtime.rs"), "{order:?}");
    assert!(
        order.contains(&"src/caller.rs"),
        "callers that reference the symbol are relevant: {order:?}"
    );
    let retry = slice
        .entries
        .iter()
        .find(|entry| entry.relative == "src/retry.rs")
        .expect("definitions the top file depends on are pulled in");
    assert!(retry.dependency);
    assert!(
        !order.contains(&"web/panel.ts") && !order.contains(&"tools/report.py"),
        "unrelated files must not fill the budget: {order:?}"
    );
    let text = slice.render();
    assert!(
        text.contains("pub fn drive(&mut self, steps: u32) -> Outcome"),
        "{text}"
    );
    assert!(text.contains("src/runtime.rs (rust)"), "{text}");

    let render = map.slice("render", 400);
    let mut hits = relatives(&render);
    hits.sort();
    assert_eq!(hits, vec!["tools/report.py", "web/panel.ts"]);
    assert!(map.slice("", 400).is_empty());
    assert!(map.slice("AgentRuntime", 0).is_empty());
}

#[test]
fn slices_never_exceed_the_token_budget() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..40 {
        let body = (0..30)
            .map(|n| format!("pub fn handler_{index}_{n}(value: u32) -> u32 {{ value }}\n"))
            .collect::<String>();
        write(dir.path(), &format!("src/handler_{index}.rs"), &body);
    }
    let map = RepoMap::build(dir.path(), RepoMapConfig::default()).unwrap();
    for budget in [1, 7, 30, 120, 1_200] {
        let slice = map.slice("handler value", budget);
        let rendered = HeuristicTokenEstimator.estimate(&slice.render()).tokens;
        assert!(slice.tokens <= budget, "{} > {budget}", slice.tokens);
        assert!(rendered <= budget, "rendered {rendered} > {budget}");
        assert!(slice.entries.len() + slice.budget_excluded <= slice.candidates);
    }
    let tight = map.slice("handler value", 120);
    assert!(tight.budget_excluded > 0);
    assert!(tight.entries.iter().all(|entry| entry.symbols.len() <= 12));
}

#[test]
fn unchanged_content_is_not_read_or_reparsed() {
    let dir = tempfile::tempdir().unwrap();
    sample_repo(dir.path());
    for relative in [
        "src/runtime.rs",
        "src/retry.rs",
        "src/budget.rs",
        "src/caller.rs",
        "web/panel.ts",
        "tools/report.py",
        "README.md",
    ] {
        age(dir.path(), relative);
    }
    let mut map = RepoMap::new(dir.path(), RepoMapConfig::default()).unwrap();
    let first = map.refresh().unwrap();
    assert_eq!(first.parsed, 7);
    let generation = map.generation();

    let second = map.refresh().unwrap();
    assert_eq!(second.listed, 7);
    assert_eq!(second.unchanged, 7, "{second:?}");
    assert_eq!((second.parsed, second.rehashed), (0, 0));
    assert_eq!(map.generation(), generation, "no content changed");

    write(
        dir.path(),
        "src/budget.rs",
        "pub struct Budget { pub tokens: u64, pub cost: u64 }\n",
    );
    let third = map.refresh().unwrap();
    assert_eq!(third.parsed, 1, "{third:?}");
    assert_eq!(third.unchanged, 6);
    assert!(map.generation() > generation);
    assert_eq!(
        map.entry("src/budget.rs").unwrap().generation,
        map.generation()
    );
}

#[test]
fn same_length_edit_inside_the_racy_window_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "lib.rs", "pub fn alpha() {}\n");
    let mut map = RepoMap::build(dir.path(), RepoMapConfig::default()).unwrap();
    assert_eq!(relatives(&map.slice("alpha", 100)), vec!["lib.rs"]);

    write(dir.path(), "lib.rs", "pub fn omega() {}\n");
    let stale = map.slice("alpha", 100);
    assert!(
        stale.entries.is_empty(),
        "an old signature was sent as current"
    );
    assert_eq!(stale.stale_excluded, vec!["lib.rs".to_owned()]);

    let stats = map.refresh().unwrap();
    assert_eq!(stats.parsed, 1, "{stats:?}");
    assert!(map.slice("alpha", 100).is_empty());
    assert_eq!(relatives(&map.slice("omega", 100)), vec!["lib.rs"]);
}

#[test]
fn rename_reuses_the_outline_keyed_by_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/old_name.rs", "pub fn moved_symbol() {}\n");
    write(dir.path(), "src/other.rs", "pub fn other() {}\n");
    let mut map = RepoMap::build(dir.path(), RepoMapConfig::default()).unwrap();

    std::fs::rename(
        dir.path().join("src/old_name.rs"),
        dir.path().join("src/new_name.rs"),
    )
    .unwrap();
    let before = map.slice("moved_symbol", 100);
    assert!(before.entries.is_empty());
    assert_eq!(before.stale_excluded, vec!["src/old_name.rs".to_owned()]);

    let stats = map
        .refresh_paths(&[
            dir.path().join("src/old_name.rs"),
            dir.path().join("src/new_name.rs"),
        ])
        .unwrap();
    assert_eq!(
        (stats.reused, stats.parsed, stats.removed),
        (1, 0, 1),
        "{stats:?}"
    );
    assert!(map.entry("src/old_name.rs").is_none());
    let renamed = map.entry("src/new_name.rs").unwrap();
    assert!(renamed.defines("moved_symbol"));
    assert_eq!(
        relatives(&map.slice("moved_symbol", 100)),
        vec!["src/new_name.rs"]
    );

    write(dir.path(), "src/copy.rs", "pub fn moved_symbol() {}\n");
    let copy = map.refresh().unwrap();
    assert_eq!((copy.reused, copy.parsed), (1, 0), "{copy:?}");
    assert!(std::sync::Arc::ptr_eq(
        &map.entry("src/copy.rs").unwrap().outline,
        &map.entry("src/new_name.rs").unwrap().outline
    ));
}

#[test]
fn deleted_files_and_directories_leave_the_map() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "keep.rs", "pub fn keep() {}\n");
    write(dir.path(), "gone.rs", "pub fn vanished() {}\n");
    write(dir.path(), "pkg/a.rs", "pub fn in_pkg_a() {}\n");
    write(dir.path(), "pkg/b.rs", "pub fn in_pkg_b() {}\n");
    let mut map = RepoMap::build(dir.path(), RepoMapConfig::default()).unwrap();
    assert_eq!(map.len(), 4);

    std::fs::remove_file(dir.path().join("gone.rs")).unwrap();
    let slice = map.slice("vanished", 100);
    assert!(slice.entries.is_empty());
    assert_eq!(slice.stale_excluded, vec!["gone.rs".to_owned()]);
    let stats = map.refresh_paths(&[dir.path().join("gone.rs")]).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(map.entry("gone.rs").is_none());

    std::fs::remove_dir_all(dir.path().join("pkg")).unwrap();
    let stats = map.refresh_paths(&[dir.path().join("pkg")]).unwrap();
    assert_eq!(stats.removed, 2, "{stats:?}");
    assert_eq!(map.len(), 1);
    assert!(map.slice("in_pkg_a in_pkg_b", 100).entries.is_empty());
    assert_eq!(map.slice("vanished", 100).candidates, 0);
}

#[test]
fn large_files_are_listed_by_path_but_never_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let config = RepoMapConfig {
        max_file_bytes: 256,
        ..RepoMapConfig::default()
    };
    write(dir.path(), "gen/schema.rs", "pub struct Schema;\n");
    let mut map = RepoMap::build(dir.path(), config).unwrap();
    assert!(map.entry("gen/schema.rs").unwrap().defines("Schema"));

    let big = format!("pub struct Schema;\n{}", "// padding\n".repeat(64));
    write(dir.path(), "gen/schema.rs", &big);
    assert!(
        map.slice("Schema", 100).entries.is_empty(),
        "grown file is stale"
    );
    let stats = map.refresh().unwrap();
    assert_eq!((stats.large, stats.parsed), (1, 0), "{stats:?}");
    let entry = map.entry("gen/schema.rs").unwrap();
    assert_eq!(entry.extraction(), SymbolExtraction::LargeFile);
    assert!(entry.symbols().is_empty() && entry.digest.is_empty());
    assert_eq!(entry.bytes, big.len() as u64);

    let by_path = map.slice("schema", 100);
    assert_eq!(relatives(&by_path), vec!["gen/schema.rs"]);
    assert!(by_path.render().contains("large file, not parsed"));
    assert_eq!(map.refresh().unwrap().large, 1);

    write(
        dir.path(),
        "gen/schema.rs",
        "pub struct Schema;\npub fn shrink() {}\n",
    );
    map.refresh().unwrap();
    let entry = map.entry("gen/schema.rs").unwrap();
    assert_eq!(entry.extraction(), SymbolExtraction::TreeSitter);
    assert!(entry.defines("shrink"));
}

#[test]
fn ignore_rules_deny_list_and_link_escapes_are_respected() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = workspace.path();
    write(root, ".gitignore", "target/\n*.log\n");
    write(root, "src/lib.rs", "pub fn visible() {}\n");
    write(root, "target/debug/build.rs", "pub fn ignored_build() {}\n");
    write(root, "trace.log", "fn ignored_log() {}\n");
    write(root, ".env", "fn dotenv_secret() {}\n");
    write(root, "config/credentials", "fn credential_secret() {}\n");
    write(root, "blob.bin", "");
    std::fs::write(root.join("blob.bin"), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
    write(outside.path(), "private.rs", "pub fn outside_secret() {}\n");
    std::fs::hard_link(outside.path().join("private.rs"), root.join("linked.rs")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path().join("private.rs"), root.join("alias.rs")).unwrap();

    let mut map = RepoMap::new(root, RepoMapConfig::default()).unwrap();
    let stats = map.refresh().unwrap();
    let indexed = map
        .entries()
        .map(|entry| entry.relative.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(indexed.contains("src/lib.rs"));
    for excluded in [
        "target/debug/build.rs",
        "trace.log",
        ".env",
        "config/credentials",
        "linked.rs",
        "alias.rs",
    ] {
        assert!(
            !indexed.contains(excluded),
            "{excluded} indexed: {indexed:?}"
        );
    }
    assert_eq!(stats.denied, 1, "{stats:?}");
    assert!(stats.unauthorized >= 1, "{stats:?}");
    assert_eq!(
        map.entry("blob.bin").unwrap().extraction(),
        SymbolExtraction::Binary
    );
    for secret in [
        "ignored_build",
        "ignored_log",
        "dotenv_secret",
        "credential_secret",
        "outside_secret",
    ] {
        assert_eq!(map.slice(secret, 500).candidates, 0, "{secret}");
    }
}

#[test]
fn root_must_be_authorized_when_a_registry_is_supplied() {
    let allowed = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let registry =
        WorkspaceAuthRegistry::with_roots([allowed.path().to_string_lossy().replace('\\', "/")]);
    let denied = RepoMap::new(
        other.path(),
        RepoMapConfig {
            authorization: Some(registry.clone()),
            ..RepoMapConfig::default()
        },
    );
    assert!(matches!(denied, Err(ContextError::OutsideWorkspace(_))));
    assert!(RepoMap::new(
        allowed.path(),
        RepoMapConfig {
            authorization: Some(registry),
            ..RepoMapConfig::default()
        },
    )
    .is_ok());
}

#[test]
fn max_files_bounds_a_sweep() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..5 {
        write(
            dir.path(),
            &format!("f{index}.rs"),
            &format!("fn f{index}() {{}}\n"),
        );
    }
    let map = RepoMap::build(
        dir.path(),
        RepoMapConfig {
            max_files: 2,
            ..RepoMapConfig::default()
        },
    )
    .unwrap();
    assert_eq!(map.len(), 2);
    assert!(map.truncated());
}

#[test]
fn service_indexes_in_the_background_and_never_blocks_a_slice() {
    let dir = tempfile::tempdir().unwrap();
    let files = 1_500;
    for index in 0..files {
        write(
            dir.path(),
            &format!("pkg{}/module_{index}.rs", index % 25),
            &format!(
                "pub struct Item{index} {{ value: u32 }}\nimpl Item{index} {{\n    pub fn compute_{index}(&self) -> u32 {{ shared_helper(self.value) }}\n}}\n"
            ),
        );
    }
    write(
        dir.path(),
        "pkg0/helper.rs",
        "pub fn shared_helper(v: u32) -> u32 { v }\n",
    );

    let service = RepoMapService::spawn(dir.path(), RepoMapConfig::default()).unwrap();
    let started = Instant::now();
    let first = service.slice("compute_42 Item42", 600);
    let first_elapsed = started.elapsed();
    assert!(
        first_elapsed < Duration::from_secs(1),
        "slice waited for the index: {first_elapsed:?}"
    );
    let mut worst = first_elapsed;
    while service.status().sweeps == 0 {
        let started = Instant::now();
        let _ = service.slice("compute_42 Item42", 600);
        worst = worst.max(started.elapsed());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        worst < Duration::from_secs(1),
        "a slice during indexing took {worst:?}"
    );
    if first.indexing {
        assert!(first.generation <= service.status().generation);
    }
    assert!(service.wait_until_idle(Duration::from_secs(60)));
    let status = service.status();
    assert_eq!(status.files, files + 1);
    assert_eq!(status.last_refresh.parsed, files + 1, "{status:?}");
    assert!(status.last_error.is_none());

    let ready = service.slice("compute_42 Item42", 600);
    assert!(!ready.indexing);
    assert_eq!(
        ready.entries.first().map(|entry| entry.relative.as_str()),
        Some("pkg17/module_42.rs")
    );

    write(
        dir.path(),
        "pkg17/module_42.rs",
        "pub struct Item42 { value: u32 }\npub fn replaced_entry() {}\n",
    );
    service.notify_paths(vec![dir.path().join("pkg17/module_42.rs")]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !service.with_map(|map| {
        map.entry("pkg17/module_42.rs")
            .is_some_and(|entry| entry.defines("replaced_entry"))
    }) {
        assert!(Instant::now() < deadline, "path refresh never applied");
        std::thread::sleep(Duration::from_millis(10));
    }

    write(
        dir.path(),
        "pkg3/added.rs",
        "pub fn brand_new_symbol() {}\n",
    );
    service.request_refresh();
    assert!(service.wait_for_sweeps(2, Duration::from_secs(60)));
    assert!(service.wait_until_idle(Duration::from_secs(60)));
    let added = service.slice("brand_new_symbol", 200);
    assert_eq!(relatives(&added), vec!["pkg3/added.rs"]);
    assert!(added.entries[0].recently_changed);
}

#[test]
fn stale_hits_from_the_service_are_queued_for_reindexing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.rs", "pub fn before_edit() {}\n");
    let service = RepoMapService::spawn(dir.path(), RepoMapConfig::default()).unwrap();
    assert!(service.wait_until_idle(Duration::from_secs(30)));
    std::fs::remove_file(dir.path().join("a.rs")).unwrap();
    let slice = service.slice("before_edit", 100);
    assert!(slice.entries.is_empty());
    assert_eq!(slice.stale_excluded, vec!["a.rs".to_owned()]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while service.with_map(|map| map.entry("a.rs").is_some()) {
        assert!(
            Instant::now() < deadline,
            "stale entry was never re-checked"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
