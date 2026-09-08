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
