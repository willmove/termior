use serde_json::json;
use std::io::Write;
use termior_store::*;

#[test]
fn journal_hash_chain_snapshot_and_tail_repair_are_deterministic() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = TaskJournal::create(dir.path(), "task-1").unwrap();
    journal.append("task-started", json!({}), true).unwrap();
    journal
        .append("model-delta", json!({"text":"hi"}), false)
        .unwrap();
    journal.write_snapshot(json!({"state":"running"})).unwrap();
    let path = dir.path().join("task-1/events.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{partial")
        .unwrap();
    let recovered =
        TaskJournal::recover(dir.path(), "task-1", &["task-started", "model-delta"]).unwrap();
    assert!(recovered.repaired_tail);
    assert_eq!(recovered.events.len(), 2);
    assert!(recovered.events.iter().all(|event| event.hash.len() == 64));
    assert_eq!(
        TaskJournal::load_snapshot(dir.path(), "task-1")
            .unwrap()
            .unwrap()
            .last_sequence,
        2
    );
}

#[test]
fn journal_redacts_known_and_shaped_secrets_before_hashing_and_snapshotting() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal =
        TaskJournal::create_with_secrets(dir.path(), "redacted", ["private-value".into()]).unwrap();
    journal
        .append(
            "output",
            json!({"known":"private-value", "shaped":"sk-12345678901234567890"}),
            true,
        )
        .unwrap();
    journal
        .write_snapshot(json!({"message":"private-value"}))
        .unwrap();
    let files = [
        dir.path().join("redacted/events.jsonl"),
        dir.path().join("redacted/snapshot.json"),
    ];
    for path in files {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("private-value"));
        assert!(!text.contains("sk-123"));
        assert!(text.contains("REDACTED"));
    }
}

#[test]
fn retention_deletes_retrievable_blobs_first_and_preserves_recovery_evidence() {
    use std::sync::atomic::AtomicBool;
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("task/tool-output/large.txt");
    let journal = dir.path().join("task/events.jsonl");
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, vec![b'x'; 4096]).unwrap();
    std::fs::write(&journal, b"trusted").unwrap();
    let manager = RetentionManager::new(
        dir.path(),
        RetentionPolicy {
            max_age_days: u64::MAX,
            max_disk_bytes: 32,
        },
    );
    let report = manager.cleanup(u64::MAX, &AtomicBool::new(false)).unwrap();
    assert!(report.deleted.contains(&blob));
    assert!(journal.exists());
    assert!(report.after_bytes < report.before_bytes);
}

#[test]
fn middle_corruption_is_isolated_instead_of_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = TaskJournal::create(dir.path(), "task-2").unwrap();
    journal.append("one", json!({}), true).unwrap();
    journal.append("two", json!({}), true).unwrap();
    let path = dir.path().join("task-2/events.jsonl");
    let text = std::fs::read_to_string(&path).unwrap().replacen(
        "\"kind\":\"one\"",
        "\"kind\":\"tampered\"",
        1,
    );
    std::fs::write(path, text).unwrap();
    assert!(matches!(
        TaskJournal::recover(dir.path(), "task-2", &[]),
        Err(JournalError::Corrupt { .. })
    ));
}

#[test]
fn unknown_events_are_preserved_for_forward_compatible_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let mut journal = TaskJournal::create(dir.path(), "task-3").unwrap();
    journal
        .append("future-event", json!({"opaque":true}), true)
        .unwrap();
    let recovered = TaskJournal::recover(dir.path(), "task-3", &["known"]).unwrap();
    assert_eq!(recovered.unknown_events[0].kind, "future-event");
}

#[test]
fn checkpoint_restore_skips_user_changes_but_restores_expected_after_content() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let file = workspace.path().join("a.txt");
    std::fs::write(&file, b"before").unwrap();
    let store = CheckpointStore::new(storage.path());
    let manifest = store
        .create(
            workspace.path(),
            "task",
            7,
            "change",
            "call",
            &[("a.txt".into(), Some(b"after".to_vec()))],
        )
        .unwrap();
    assert_eq!(store.list_manifests().unwrap(), vec![manifest.clone()]);
    assert_eq!(store.load_manifest(&manifest.id).unwrap(), manifest);
    std::fs::write(&file, b"after").unwrap();
    let plan = store.plan_restore(workspace.path(), &manifest).unwrap();
    assert!(matches!(plan.actions[0], RestoreAction::Restore { .. }));
    store.apply_restore(workspace.path(), &plan).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"before");
    std::fs::write(&file, b"user edit").unwrap();
    assert!(matches!(
        store
            .plan_restore(workspace.path(), &manifest)
            .unwrap()
            .actions[0],
        RestoreAction::SkipConflict { .. }
    ));
}

#[test]
fn checkpoint_rejects_parent_escape_symlinks_and_hard_links() {
    let workspace = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let store = CheckpointStore::new(storage.path());
    assert!(matches!(
        store.create(
            workspace.path(),
            "task",
            1,
            "change",
            "call",
            &[("../escape.txt".into(), Some(b"x".to_vec()))],
        ),
        Err(CheckpointError::EscapedPath(_))
    ));

    let original = workspace.path().join("original.txt");
    let linked = workspace.path().join("linked.txt");
    std::fs::write(&original, b"same inode").unwrap();
    std::fs::hard_link(&original, &linked).unwrap();
    assert!(matches!(
        store.create(
            workspace.path(),
            "task",
            2,
            "change",
            "call",
            &[("linked.txt".into(), Some(b"changed".to_vec()))],
        ),
        Err(CheckpointError::UnsupportedHardLink(_))
    ));
}

#[test]
fn streaming_redactor_covers_known_and_shaped_credentials() {
    let redactor = StreamingRedactor::new(["private-value".into()]);
    let report = redactor.redact("a private-value b sk-123456789012345 c");
    assert_eq!(report.replacements, 2);
    assert!(!report.text.contains("private-value"));
    assert!(!report.text.contains("sk-123"));
}

#[test]
fn incomplete_work_recovers_unknown_and_retry_gets_a_new_attempt() {
    let mut operation = RecoveredOperation::from_incomplete("call-9", "process", 42, None);
    assert_eq!(operation.state, RecoveredOperationState::Unknown);
    let attempt = operation.apply(RecoveryCommand::Retry).unwrap();
    assert_eq!(attempt, "call-9-attempt-2");
    assert_eq!(operation.state, RecoveredOperationState::Waiting);
}

#[test]
fn backend_resume_only_changes_state_when_the_session_is_queryable() {
    let mut local = RecoveredOperation::from_incomplete("call", "tool", 1, None);
    local.apply(RecoveryCommand::Resume {
        verified_state: RecoveredOperationState::Succeeded,
    });
    assert_eq!(local.state, RecoveredOperationState::Unknown);
    let mut external =
        RecoveredOperation::from_incomplete("turn", "model", 1, Some("session".into()));
    external.apply(RecoveryCommand::Resume {
        verified_state: RecoveredOperationState::Succeeded,
    });
    assert_eq!(external.state, RecoveredOperationState::Succeeded);
}
