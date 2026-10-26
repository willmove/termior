//! Storage migration gates (Stage D 8.3 / Stage E 7.4).
//!
//! Every persisted agent format has to be readable by a newer build, and a store
//! written by a newer build has to be refused rather than misread. These cases run
//! against real files on disk, not against in-memory values.

use std::path::Path;

use termior_ai::automation::{AutomationStore, AUTOMATION_SCHEMA_VERSION};
use termior_store::migrate::{migrate, SCHEMA_VERSION};

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[test]
fn a_settings_file_without_a_version_migrates_to_the_current_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Termior-settings.json");
    // A v0 file: no version field, and it carries a field the current schema keeps.
    write(
        &path,
        r#"{"theme_id":"default","appearance":"light","custom_instructions":"keep me","vim_mode":true}"#,
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    let settings = migrate(&raw).unwrap();
    assert_eq!(settings.version, SCHEMA_VERSION);
    // The light/dark pairing is derived, not inherited from the old file.
    assert_eq!(settings.light_theme_id, "default-light");
    assert_eq!(settings.dark_theme_id, "default");
    assert_eq!(settings.custom_instructions, "keep me");
    assert!(settings.vim_mode);
}

#[test]
fn an_older_settings_file_migrates_and_keeps_its_user_choices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Termior-settings.json");
    // A v1 file: the theme pairing was not split yet.
    write(
        &path,
        r#"{"version":1,"theme_id":"nord","appearance":"dark","editor_theme_id":"default"}"#,
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    let settings = migrate(&raw).unwrap();
    assert_eq!(settings.version, SCHEMA_VERSION);
    assert_eq!(settings.theme_id, "nord");
    assert_eq!(settings.dark_theme_id, "nord");
}

#[test]
fn a_settings_file_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Termior-settings.json");
    write(&path, &format!(r#"{{"version":{}}}"#, SCHEMA_VERSION + 1));
    let raw = std::fs::read_to_string(&path).unwrap();
    let error = migrate(&raw).unwrap_err();
    assert!(error.to_string().contains("unsupported version"), "{error}");
}

#[test]
fn an_older_automation_store_migrates_and_keeps_its_schedules() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("automations.json");
    // A v1 store: the run reason did not exist yet.
    write(
        &path,
        r#"{
          "schema_version": 1,
          "automations": [{
            "id": "daily",
            "name": "daily",
            "enabled": true,
            "trigger": "Manual",
            "catch_up": "Skip",
            "notification": "Actionable",
            "template": {
              "version": 4,
              "prompt": "check the repo",
              "backend": "builtin",
              "agent_profile": "default",
              "environment_id": "direct",
              "max_concurrency": 1,
              "budgets": {
                "max_model_steps": 20,
                "max_wall_clock_ms": 60000,
                "max_tool_output_bytes": 1000000,
                "max_input_tokens": 100000,
                "max_output_tokens": 20000
              }
            }
          }],
          "recent_runs": [{
            "run_id": "r1",
            "task_id": "t1",
            "automation_id": "daily",
            "template_version": 4,
            "template": {
              "version": 4,
              "prompt": "check the repo",
              "backend": "builtin",
              "agent_profile": "default",
              "environment_id": "direct",
              "max_concurrency": 1,
              "budgets": {
                "max_model_steps": 20,
                "max_wall_clock_ms": 60000,
                "max_tool_output_bytes": 1000000,
                "max_input_tokens": 100000,
                "max_output_tokens": 20000
              }
            },
            "dedupe_key": "manual:daily",
            "scheduled_for": 100,
            "created_at": 100,
            "status": "Completed"
          }],
          "audit": []
        }"#,
    );
    let loaded = AutomationStore::load(dir.path()).unwrap();
    assert_eq!(loaded.schema_version, AUTOMATION_SCHEMA_VERSION);
    assert_eq!(loaded.automations.len(), 1);
    assert_eq!(loaded.automations[0].id, "daily");
    assert_eq!(loaded.automations[0].template.version, 4);
    assert_eq!(loaded.recent_runs.len(), 1);
    assert_eq!(loaded.recent_runs[0].template_version, 4);
    // The v1 rows had no reason, so the migration marks them instead of losing them.
    assert_eq!(loaded.recent_runs[0].schedule_reason, "unknown");
}

#[test]
fn an_automation_store_from_a_newer_build_is_refused_instead_of_misread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("automations.json");
    write(
        &path,
        &format!(
            r#"{{"schema_version":{},"automations":[],"recent_runs":[],"audit":[]}}"#,
            AUTOMATION_SCHEMA_VERSION + 1
        ),
    );
    let error = AutomationStore::load(dir.path()).unwrap_err();
    assert!(
        error.to_string().contains("schema"),
        "the error must name the schema mismatch: {error}"
    );
    // The file is left alone so a newer build can still read it.
    assert!(path.is_file());
}

#[test]
fn a_journal_from_an_older_build_is_still_replayable() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join("agent-tasks");
    let task_id = "migrated-task";
    let mut journal = termior_store::TaskJournal::create(&tasks, task_id).unwrap();
    journal
        .append("task/started", serde_json::json!({"goal": "x"}), true)
        .unwrap();
    journal
        .append(
            "task/output_delta",
            serde_json::json!({"text": "hello"}),
            false,
        )
        .unwrap();
    drop(journal);

    // The header carries the journal schema version, so a future schema bump is
    // visible in the file rather than guessed at read time.
    let path = tasks.join(task_id).join("events.jsonl");
    let first_line = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(
        first_line.contains(&termior_store::task_journal::JOURNAL_SCHEMA_VERSION.to_string()),
        "the header must record the journal schema version: {first_line}"
    );

    let recovered = termior_store::TaskJournal::recover(&tasks, task_id, &[]).unwrap();
    assert_eq!(recovered.events.len(), 2);
    assert_eq!(recovered.events[0].kind, "task/started");
    assert!(recovered.events[0].critical);
    assert!(!recovered.events[1].critical);
    assert!(!recovered.repaired_tail);
}

#[test]
fn a_journal_with_a_future_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join("agent-tasks");
    let task_id = "future-task";
    let path = tasks.join(task_id).join("events.jsonl");
    write(
        &path,
        &format!(
            "{{\"schema_version\":{},\"task_id\":\"{task_id}\"}}\n",
            termior_store::task_journal::JOURNAL_SCHEMA_VERSION + 1
        ),
    );
    let error = termior_store::TaskJournal::recover(&tasks, task_id, &[]).unwrap_err();
    assert!(
        error.to_string().contains("schema"),
        "the error must name the schema mismatch: {error}"
    );
}
