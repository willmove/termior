use std::collections::{BTreeMap, BTreeSet};
use termior_ai::context_engine::*;

fn item(id: &str, category: ContextCategory, content: &str, priority: u16) -> ContextItem {
    ContextItem::inline(id, category, ContextSource::User, content, priority)
}

#[test]
fn assembler_is_deterministic_and_never_drops_required_invariants() {
    let assembler = ContextAssembler::new(
        80,
        BTreeMap::from([
            (ContextCategory::Invariant, 60),
            (ContextCategory::Evidence, 20),
        ]),
    );
    let items = vec![
        item("log", ContextCategory::Evidence, &"x".repeat(400), 50),
        item(
            "goal",
            ContextCategory::Invariant,
            "goal and acceptance",
            100,
        ),
        item(
            "safety",
            ContextCategory::Invariant,
            "workspace policy",
            200,
        ),
    ];
    let first = assembler.plan(&items).unwrap();
    let second = assembler.plan(&items).unwrap();
    assert_eq!(first, second);
    assert!(first
        .entries
        .iter()
        .filter(|entry| entry.item_id == "goal" || entry.item_id == "safety")
        .all(|entry| entry.disposition == ContextDisposition::Included));
    assert!(first.total_tokens <= 80);
}

#[test]
fn context_plan_materializes_exactly_the_selected_inline_content() {
    let assembler = ContextAssembler::new(6, BTreeMap::new());
    let items = vec![
        item("system", ContextCategory::Invariant, "rule", 100),
        item("terminal", ContextCategory::Evidence, &"x".repeat(40), 10),
        item("excluded", ContextCategory::Evidence, "later", 1),
    ];
    let plan = assembler.plan(&items).unwrap();
    let materialized = plan.materialize_inline(&items);
    assert_eq!(materialized[0].item_id, "system");
    assert_eq!(materialized[0].content, "rule");
    let terminal = materialized
        .iter()
        .find(|item| item.item_id == "terminal")
        .unwrap();
    assert_eq!(terminal.disposition, ContextDisposition::Truncated);
    assert!(terminal.content.contains("[context truncated"));
    assert!(!materialized.iter().any(|item| item.item_id == "excluded"));
}

#[test]
fn oversized_required_attachment_fails_instead_of_silently_truncating() {
    let assembler = ContextAssembler::new(10, BTreeMap::new());
    let mut attachment = item(
        "user-file",
        ContextCategory::Attachment,
        &"a".repeat(500),
        100,
    );
    attachment.required = true;
    assert!(matches!(
        assembler.plan(&[attachment]),
        Err(ContextError::RequiredItemExceedsHardLimit { .. })
    ));
}

#[test]
fn task_brief_keeps_pending_and_unknown_state_across_compaction() {
    let brief = TaskBrief {
        schema_version: 1,
        goal: "ship".into(),
        acceptance: vec!["tests pass".into()],
        user_constraints: vec!["no network".into()],
        decisions: vec!["use adapter".into()],
        completed_actions: vec!["read spec".into()],
        pending_approvals: vec![PendingInvariant {
            id: "call-7".into(),
            detail: "write a.rs".into(),
        }],
        pending_changes: vec![PendingInvariant {
            id: "edit-2".into(),
            detail: "two hunks".into(),
        }],
        unknown_side_effects: vec![PendingInvariant {
            id: "cmd-9".into(),
            detail: "kill unconfirmed".into(),
        }],
        failure_evidence: vec!["test failed".into()],
        next_steps: vec!["inspect".into()],
        covered_event_range: (1, 50),
        summary_model: Some("local".into()),
    };
    let compacted = Compactor::new(100).compact(&brief, "narrative", 0).unwrap();
    assert_eq!(compacted.brief.pending_approvals[0].id, "call-7");
    assert_eq!(compacted.brief.unknown_side_effects[0].id, "cmd-9");
}

#[test]
fn compaction_thrash_stops_after_the_configured_limit() {
    let brief = TaskBrief::minimal("goal");
    let compactor = Compactor::new(1);
    assert!(matches!(
        compactor.compact(&brief, "still huge", 1),
        Err(ContextError::ContextThrashing { .. })
    ));
}

#[test]
fn nested_agents_rules_are_scoped_per_target_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("frontend/src")).unwrap();
    std::fs::create_dir_all(dir.path().join("backend/src")).unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "root rule").unwrap();
    std::fs::write(dir.path().join("frontend/AGENTS.md"), "frontend rule").unwrap();
    std::fs::write(dir.path().join("backend/AGENTS.md"), "backend rule").unwrap();
    let resolver = RuleResolver::new(dir.path()).unwrap();
    let front = resolver
        .for_path(&dir.path().join("frontend/src/a.ts"))
        .unwrap();
    let back = resolver
        .for_path(&dir.path().join("backend/src/a.rs"))
        .unwrap();
    assert_eq!(front.len(), 2);
    assert!(front.iter().any(|rule| rule.content.contains("frontend")));
    assert!(!front.iter().any(|rule| rule.content.contains("backend")));
    assert_eq!(RuleResolver::common(&[front, back]).len(), 1);
}

#[test]
fn skills_load_metadata_before_body_and_never_expand_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join(".agents/skills/review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: review\ndescription: Review code\nallowed-tools: read_file,write_file\n---\nDetailed workflow\n").unwrap();
    let index = SkillIndex::scan(&[dir.path().join(".agents/skills")], dir.path()).unwrap();
    assert_eq!(index.metadata()[0].name, "review");
    assert!(!index.metadata()[0].body_loaded);
    let activated = index
        .activate("review", &BTreeSet::from(["read_file".into()]))
        .unwrap();
    assert!(activated.body.contains("Detailed workflow"));
    assert_eq!(
        activated.effective_tools,
        BTreeSet::from(["read_file".into()])
    );
}

#[test]
fn memory_requires_evidence_and_rejects_secrets() {
    let candidate = MemoryCandidate {
        id: "m1".into(),
        content: "use compact output".into(),
        scope: ContextScope::Workspace,
        evidence: vec!["event-1".into()],
        confidence: "explicit user preference".into(),
        expires_at_ms: None,
    };
    assert!(candidate.clone().accept().is_ok());
    let secret = MemoryCandidate {
        content: "token sk-12345678901234567890".into(),
        ..candidate
    };
    assert!(matches!(
        secret.accept(),
        Err(ContextError::SensitiveMemory)
    ));
}

#[test]
fn memory_store_supports_review_edit_pause_expiry_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.json");
    let mut store = MemoryStore::default();
    store.propose(MemoryCandidate {
        id: "m".into(),
        content: "prefer concise output".into(),
        scope: ContextScope::Workspace,
        evidence: vec!["user:event-1".into()],
        confidence: "explicit".into(),
        expires_at_ms: Some(u64::MAX),
    });
    store.accept("m").unwrap();
    store.edit("m", "prefer tables".into()).unwrap();
    store.pause("m", true).unwrap();
    assert_eq!(store.status("m", 1), Some(MemoryStatus::Paused));
    store.persist(&path).unwrap();
    assert_eq!(MemoryStore::load(&path).unwrap(), store);
}

#[test]
fn skill_resources_are_lazy_and_cannot_escape_the_skill_directory() {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join(".agents/skills/lazy");
    std::fs::create_dir_all(skill.join("references")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: lazy\ndescription: Lazy\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(skill.join("references/info.md"), "on demand").unwrap();
    let index = SkillIndex::scan_sources(dir.path(), None).unwrap();
    assert_eq!(
        index
            .activate(
                "lazy",
                &BTreeSet::from(["read_file".into(), "fs_search".into()])
            )
            .unwrap()
            .effective_tools,
        BTreeSet::from(["read_file".into(), "fs_search".into()])
    );
    let resource = index
        .load_resource("lazy", std::path::Path::new("references/info.md"))
        .unwrap();
    assert!(
        matches!(resource.content, ContentRef::Inline { ref content } if content == "on demand")
    );
    assert!(index
        .load_resource("lazy", std::path::Path::new("../outside"))
        .is_err());
}

#[test]
fn content_references_round_trip_without_copying_payloads() {
    let reference = ContentRef::CommandOutputRange {
        session_id: "cmd-3".into(),
        start: 20,
        end: 42,
    };
    let json = serde_json::to_string(&reference).unwrap();
    assert_eq!(
        serde_json::from_str::<ContentRef>(&json).unwrap(),
        reference
    );
}

#[test]
fn large_tool_content_is_redacted_stored_and_digest_checked() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextContentStore::new(dir.path(), ["known-secret".into()]);
    let reference = store
        .put_tool_output("task/unsafe", "call:1", "hello known-secret")
        .unwrap();
    let resolved = store.resolve(&reference).unwrap();
    assert_eq!(resolved, "hello [REDACTED]");
    let ContentRef::Artifact { path, .. } = &reference else {
        panic!("expected artifact reference")
    };
    std::fs::write(path, "tampered").unwrap();
    assert!(matches!(
        store.resolve(&reference),
        Err(ContextError::StaleContentReference { .. })
    ));
}

#[test]
fn repo_map_invalidates_changed_files_and_obeys_query_budget() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("agent.rs");
    let b = dir.path().join("other.rs");
    std::fs::write(&a, "pub struct AgentRuntime;\npub fn drive_agent() {}\n").unwrap();
    std::fs::write(&b, "pub fn unrelated() {}\n").unwrap();
    let mut map = RepoMap::default();
    map.index_file(dir.path(), &a).unwrap();
    map.index_file(dir.path(), &b).unwrap();
    let slice = map.relevant("AgentRuntime", 30);
    assert_eq!(slice.first().unwrap().path.file_name().unwrap(), "agent.rs");
    std::fs::write(&a, "pub struct Renamed;\n").unwrap();
    map.invalidate_changed();
    assert!(map
        .relevant("AgentRuntime", 30)
        .iter()
        .all(|entry| entry.path != std::fs::canonicalize(&a).unwrap()));
}
