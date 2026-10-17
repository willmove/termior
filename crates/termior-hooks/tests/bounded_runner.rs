use serde_json::json;
use std::time::Duration;
use termior_hooks::runner::*;

#[test]
fn replacement_is_revalidated_and_cannot_reuse_the_old_decision() {
    let runner = HookRunner::default();
    let changed = HookDecision::Replace {
        arguments: json!({"path":"../secret"}),
    };
    let error = runner
        .apply_pre_tool(&json!({"path":"safe"}), changed, |args| {
            if args["path"].as_str().unwrap().starts_with("..") {
                Err("outside workspace".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error, HookError::Revalidation("outside workspace".into()));
}

#[test]
fn default_runner_is_bounded_and_noninteractive_contract_is_versioned() {
    let runner = HookRunner::default();
    assert_eq!(runner.timeout, Duration::from_secs(5));
    let event = HookEvent {
        schema_version: HOOK_SCHEMA_VERSION,
        point: HookPoint::PreTool,
        task_id: "task-1".into(),
        tool: Some("read_file".into()),
        arguments: Some(json!({"path":"a"})),
    };
    let encoded = serde_json::to_value(event).unwrap();
    assert_eq!(encoded["schema_version"], 1);
}

#[test]
fn deny_is_not_downgraded_to_a_warning() {
    let runner = HookRunner::default();
    assert!(
        matches!(runner.apply_pre_tool(&json!({}), HookDecision::Deny { reason: "policy".into() }, |_| Ok(())), Err(HookError::Denied(reason)) if reason == "policy")
    );
}

#[test]
fn subprocess_runner_parses_a_bounded_json_decision() {
    let event = HookEvent {
        schema_version: 1,
        point: HookPoint::PreTask,
        task_id: "task".into(),
        tool: None,
        arguments: None,
    };
    let (program, args) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "$null=[Console]::In.ReadToEnd(); [Console]::Out.Write('{\"decision\":\"allow\"}')"
                    .into(),
            ],
        )
    } else {
        (
            "sh",
            vec![
                "-c".into(),
                "cat >/dev/null; printf '{\"decision\":\"allow\"}'".into(),
            ],
        )
    };
    let run = HookRunner::default().run(program, &args, &event).unwrap();
    assert_eq!(run.decision, HookDecision::Allow);
}

#[test]
fn hook_audit_file_redacts_known_secrets_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (program, args) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "$null=[Console]::In.ReadToEnd(); [Console]::Error.WriteLine('private-value'); [Console]::Out.Write('{\"decision\":\"allow\"}')".into(),
            ],
        )
    } else {
        (
            "sh",
            vec![
                "-c".into(),
                "cat >/dev/null; echo private-value >&2; printf '%s' '{\"decision\":\"allow\"}'"
                    .into(),
            ],
        )
    };
    let pipeline = HookPipeline::new(vec![HookConfig {
        id: "audit".into(),
        point: HookPoint::PreTool,
        program: program.into(),
        args,
        enabled: true,
        fail_closed: true,
        source: "user".into(),
        timeout_ms: 5_000,
    }])
    .with_known_secrets(["private-value".into()]);
    let event = HookEvent {
        schema_version: HOOK_SCHEMA_VERSION,
        point: HookPoint::PreTool,
        task_id: "task".into(),
        tool: Some("read_file".into()),
        arguments: Some(json!({})),
    };
    pipeline.apply_pre_tool(&event, |_| Ok(())).unwrap();
    let path = dir.path().join("hooks.json");
    pipeline.persist_audit(&path).unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!text.contains("private-value"), "{text}");
    assert!(text.contains("[REDACTED]"), "{text}");
    let record = &pipeline.recent()[0];
    assert_eq!(record.decision, "allow");
    assert!(!record.modified_arguments);
    assert!(record.duration_ms < 5_000, "{}", record.duration_ms);
}

#[test]
fn subprocess_timeout_follows_fail_closed_policy() {
    let event = HookEvent {
        schema_version: 1,
        point: HookPoint::PreTask,
        task_id: "task".into(),
        tool: None,
        arguments: None,
    };
    let (program, args) = if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "$null=[Console]::In.ReadToEnd(); Start-Sleep -Seconds 2".into(),
            ],
        )
    } else {
        ("sh", vec!["-c".into(), "cat >/dev/null; sleep 2".into()])
    };
    let runner = HookRunner {
        timeout: Duration::from_millis(50),
        ..HookRunner::default()
    };
    assert!(
        matches!(runner.run(program, &args, &event), Err(HookError::FailedClosed(message)) if message.contains("timed out"))
    );
}

fn missing_program() -> HookConfig {
    HookConfig {
        id: "skip".into(),
        point: HookPoint::PreTool,
        program: "definitely-not-a-real-hook-binary".into(),
        args: Vec::new(),
        enabled: true,
        fail_closed: true,
        source: "user".into(),
        timeout_ms: 5_000,
    }
}

fn pre_tool_event() -> HookEvent {
    HookEvent {
        schema_version: HOOK_SCHEMA_VERSION,
        point: HookPoint::PreTool,
        task_id: "task".into(),
        tool: Some("read_file".into()),
        arguments: Some(json!({"path": "a.txt"})),
    }
}

#[test]
fn disabled_hook_does_not_run() {
    let mut config = missing_program();
    config.enabled = false;
    let pipeline = HookPipeline::new(vec![config]);
    let outcome = pipeline
        .apply_pre_tool(&pre_tool_event(), |_| {
            panic!("a disabled hook must not revalidate")
        })
        .unwrap();
    assert!(!outcome.replaced);
    assert!(outcome.records.is_empty());
    assert!(pipeline.recent().is_empty());
}

#[test]
fn disabling_a_hook_persists_and_the_next_pipeline_skips_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = termior_hooks::lifecycle_hooks_path(dir.path());
    let pipeline = HookPipeline::new(vec![missing_program()]).with_catalog_path(&path);
    assert!(pipeline.set_enabled("skip", false));
    let loaded = termior_hooks::HookCatalog::load(&path).unwrap();
    assert!(!loaded.hooks[0].enabled);

    let outcome = pipeline
        .apply_pre_tool(&pre_tool_event(), |_| panic!("disabled hook must not run"))
        .unwrap();
    assert!(outcome.records.is_empty());

    let again = HookPipeline::new(loaded.hooks);
    let outcome = again
        .apply_pre_tool(&pre_tool_event(), |_| {
            panic!("reloaded disabled hook must not run")
        })
        .unwrap();
    assert!(outcome.records.is_empty());
}

#[test]
fn catalog_on_disk_stops_a_live_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let path = termior_hooks::lifecycle_hooks_path(dir.path());
    let config = missing_program();
    let mut catalog = termior_hooks::HookCatalog {
        hooks: vec![config.clone()],
    };
    catalog.save(&path).unwrap();
    let pipeline = HookPipeline::new(vec![config]).with_catalog_path(&path);
    catalog.set_enabled("skip", false);
    catalog.save(&path).unwrap();
    let outcome = pipeline
        .apply_pre_tool(&pre_tool_event(), |_| {
            panic!("disk disable must stop the live pipeline")
        })
        .unwrap();
    assert!(outcome.records.is_empty());
    assert_eq!(outcome.arguments, json!({"path": "a.txt"}));
}
