use termior_diff::{apply_file_change, ChangeSet, FileChange, FileChangeError};

#[test]
fn accepted_hunks_apply_against_the_recorded_baseline() {
    let change = FileChange::new("a.txt", "one\ntwo\n", "one\nTWO\nthree\n", 0);
    let result = apply_file_change("one\ntwo\n", &change, &change.hunk_ids()).unwrap();
    assert_eq!(result.content, "one\nTWO\nthree\n");
    assert_eq!(result.applied_hunks, change.hunk_ids());
    assert!(result.rejected_hunks.is_empty());
}

#[test]
fn changed_disk_content_reports_conflict_without_producing_replacement_content() {
    let change = FileChange::new("a.txt", "old\n", "new\n", 0);
    let error = apply_file_change("user edit\n", &change, &[0]).unwrap_err();
    assert!(matches!(error, FileChangeError::BaselineConflict { .. }));
}

#[test]
fn one_change_set_can_group_multiple_files_and_source_calls() {
    let change_set = ChangeSet::new(
        "change-1",
        vec!["call-a".into(), "call-b".into()],
        vec![
            FileChange::new("a.txt", "a\n", "A\n", 0),
            FileChange::new("b.txt", "b\n", "B\n", 0),
        ],
    );
    assert_eq!(change_set.files.len(), 2);
    assert_eq!(change_set.source_call_ids.len(), 2);
}
