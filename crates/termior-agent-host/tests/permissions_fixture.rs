//! Schema-faithful `item/permissions/requestApproval` fixtures.
//!
//! The committed JSONL is the redacted form. The preimage (a synthetic home
//! directory and token) lives only in this test and is passed through
//! `termior_store::StreamingRedactor` before it is compared to disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use termior_agent_host::{
    permission_entries, permissions_approval_result, AgentBackend, BackendDecision, BackendEvent,
    CodexBackend, PERMISSIONS_APPROVAL_METHOD,
};
use termior_store::StreamingRedactor;

const HOME: &str = "/home/codex-user";
const TOKEN: &str = "sk-fixturesecrettokenvalue";

const REQUESTS: &str = include_str!("fixtures/codex-permissions-request-approval/requests.jsonl");
const RESPONSES: &str = include_str!("fixtures/codex-permissions-request-approval/responses.jsonl");

struct Replay {
    id: u64,
    decision: &'static str,
    /// `None` grants every entry. `Some` is the checked subset (ignored on deny).
    only: Option<&'static [&'static str]>,
}

const REPLAYS: &[Replay] = &[
    Replay {
        id: 61,
        decision: "approve-all",
        only: None,
    },
    Replay {
        id: 61,
        decision: "partial",
        only: Some(&["fileSystem.write[0]"]),
    },
    Replay {
        id: 61,
        decision: "deny",
        only: None,
    },
    Replay {
        id: 62,
        decision: "approve-all",
        only: None,
    },
    Replay {
        id: 62,
        decision: "partial",
        only: Some(&["fileSystem.entries[1]"]),
    },
    Replay {
        id: 62,
        decision: "deny",
        only: None,
    },
    Replay {
        id: 63,
        decision: "approve-all",
        only: None,
    },
    Replay {
        id: 63,
        decision: "partial",
        only: Some(&["fileSystem.write[0]", "network"]),
    },
    Replay {
        id: 63,
        decision: "deny",
        only: None,
    },
];

#[test]
fn recorded_permission_fixtures_match_the_redacted_schema_and_the_approval_mapping() {
    let requests = redacted_requests_jsonl();
    let responses = response_jsonl(&requests);
    assert_redacted(&requests);
    assert_eq!(requests, REQUESTS, "redacted request fixture drifted");
    assert_redacted(&responses);
    assert_eq!(responses, RESPONSES, "permission response fixture drifted");
}

#[test]
fn fake_app_server_replays_permission_responses_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let executable = compile_fake_codex(dir.path());
    for replay in REPLAYS {
        let request = request_line(replay);
        fs::write(dir.path().join("request.jsonl"), format!("{request}\n")).unwrap();
        let capture = dir.path().join("captured-response.json");
        let _ = fs::remove_file(&capture);

        let mut backend = CodexBackend::new(executable.to_string_lossy().as_ref());
        backend
            .initialize()
            .unwrap_or_else(|error| panic!("fake codex initialize failed: {error}"));
        let binding = backend
            .create_task(termior_agent_host::TaskLaunch {
                task_id: "task-fixture".into(),
                project_dir: dir.path().display().to_string(),
                environment_id: "direct".into(),
                model: None,
            })
            .unwrap_or_else(|error| panic!("thread/start failed: {error}"));
        backend
            .start_turn(termior_agent_host::TurnLaunch {
                task_id: binding.task_id.clone(),
                backend_session_id: binding.backend_session_id.clone(),
                text: "replay permission approval".into(),
            })
            .unwrap_or_else(|error| panic!("turn/start failed: {error}"));

        let event = backend
            .next_event(Duration::from_secs(5))
            .unwrap()
            .unwrap_or_else(|| panic!("fake server sent no approval for id {}", replay.id));
        let BackendEvent::ApprovalRequested {
            backend_request_id,
            request_kind,
            raw,
            ..
        } = &event
        else {
            panic!("expected ApprovalRequested, got {event:?}");
        };
        assert_eq!(backend_request_id, &replay.id.to_string());
        assert_eq!(request_kind, PERMISSIONS_APPROVAL_METHOD);
        let fixture: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(raw, &fixture["params"]);

        let expected = committed_response(replay);
        let granted = expected["granted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_string())
            .collect();
        let approved = replay.decision != "deny";
        backend
            .respond(BackendDecision {
                backend_request_id: backend_request_id.clone(),
                approved,
                granted_permission_ids: Some(granted),
            })
            .unwrap_or_else(|error| panic!("respond failed: {error}"));

        let captured = wait_for_json(&capture);
        let expected = expected["message"].clone();
        assert_eq!(
            captured, expected,
            "stdio response for id {} {}",
            replay.id, replay.decision
        );
        backend.shutdown(Duration::from_secs(2)).unwrap();
    }
}

fn redacted_requests_jsonl() -> String {
    let redactor = StreamingRedactor::new([HOME.to_string()]);
    let mut body = String::new();
    for request in unredacted_requests() {
        let line = serde_json::to_string(&request).unwrap();
        let report = redactor.redact(&line);
        assert!(
            report.replacements > 0,
            "schema preimage was not redacted: {line}"
        );
        body.push_str(&report.text);
        body.push('\n');
    }
    body
}

fn unredacted_requests() -> Vec<Value> {
    let inputs = format!("{HOME}/work/inputs");
    let outputs = format!("{HOME}/work/outputs");
    let shared = format!("{HOME}/work/shared");
    let cwd = format!("{HOME}/work/repo");
    let subpath = format!("{HOME}/work/sub");
    vec![
        // 0.137 and 0.153 `From<CoreFileSystemPermissions>` emit legacy roots and `entries`.
        approval_request(
            61,
            "call_fixture",
            Value::Null,
            format!("Select a workspace root {TOKEN}"),
            &cwd,
            json!({
                "network": {"enabled": true},
                "fileSystem": {
                    "read": [inputs.clone()],
                    "write": [outputs.clone()],
                    "entries": [
                        sandbox_entry(json!({"type": "path", "path": inputs}), "read"),
                        sandbox_entry(json!({"type": "path", "path": outputs}), "write")
                    ]
                }
            }),
        ),
        // Non-legacy entries: deny globs and special paths are not read/write roots.
        approval_request(
            62,
            "call_entries",
            json!("local"),
            "Select a workspace root".into(),
            &cwd,
            json!({
                "network": null,
                "fileSystem": {
                    "read": null,
                    "write": null,
                    "entries": [
                        sandbox_entry(json!({"type": "glob_pattern", "pattern": "**/*.log"}), "deny"),
                        sandbox_entry(json!({
                            "type": "special",
                            "value": {"kind": "project_roots", "subpath": subpath}
                        }), "write")
                    ]
                }
            }),
        ),
        // `entries` is optional. The protocol test deserializes read/write roots alone.
        approval_request(
            63,
            "call_roots",
            Value::Null,
            "Select a workspace root".into(),
            &cwd,
            json!({
                "network": {"enabled": true},
                "fileSystem": {
                    "read": [format!("{HOME}/work/inputs")],
                    "write": [format!("{HOME}/work/outputs"), shared]
                }
            }),
        ),
    ]
}

fn approval_request(
    id: u64,
    item_id: &str,
    environment_id: Value,
    reason: String,
    cwd: &str,
    permissions: Value,
) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": PERMISSIONS_APPROVAL_METHOD,
        "params": {
            "threadId": "thr_fixture",
            "turnId": "turn_fixture",
            "itemId": item_id,
            "environmentId": environment_id,
            "startedAtMs": 1,
            "cwd": cwd,
            "reason": reason,
            "permissions": permissions
        }
    })
}

fn sandbox_entry(path: Value, access: &str) -> Value {
    json!({"path": path, "access": access})
}

fn response_jsonl(requests: &str) -> String {
    let mut body = String::new();
    for replay in REPLAYS {
        let line = serde_json::to_string(&response_value(replay, requests)).unwrap();
        body.push_str(&line);
        body.push('\n');
    }
    body
}

fn committed_response(replay: &Replay) -> Value {
    parse_jsonl(RESPONSES)
        .into_iter()
        .find(|value| {
            value.get("id").and_then(Value::as_u64) == Some(replay.id)
                && value.get("decision").and_then(Value::as_str) == Some(replay.decision)
        })
        .unwrap_or_else(|| panic!("missing response {} {}", replay.id, replay.decision))
}

fn response_value(replay: &Replay, requests: &str) -> Value {
    let request = parse_jsonl(requests)
        .into_iter()
        .find(|value| value.get("id").and_then(Value::as_u64) == Some(replay.id))
        .unwrap_or_else(|| panic!("missing request {}", replay.id));
    let event = termior_agent_host::map_codex_message(&request);
    let BackendEvent::ApprovalRequested {
        request_kind, raw, ..
    } = &event
    else {
        panic!(
            "fixture id {} did not map to an approval: {event:?}",
            replay.id
        );
    };
    assert_eq!(request_kind, PERMISSIONS_APPROVAL_METHOD);
    assert!(raw.get("scope").is_none());
    let profile = raw.get("permissions").cloned().unwrap_or(Value::Null);
    let granted = granted_ids(&profile, replay);
    let approved = replay.decision != "deny";
    if replay.decision == "partial" {
        let all = permission_entries(&profile);
        assert!(!granted.is_empty());
        assert!(granted.len() < all.len());
    }
    let result = permissions_approval_result(&profile, approved, Some(&granted));
    assert_eq!(result["scope"], "turn");
    json!({
        "id": replay.id,
        "decision": replay.decision,
        "granted": granted,
        "message": {
            "jsonrpc": "2.0",
            "id": replay.id,
            "result": result
        }
    })
}

fn granted_ids(profile: &Value, replay: &Replay) -> Vec<String> {
    if replay.decision == "deny" {
        return Vec::new();
    }
    let all = permission_entries(profile)
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    match replay.only {
        None => all,
        Some(selected) => all
            .into_iter()
            .filter(|id| selected.contains(&id.as_str()))
            .collect(),
    }
}

fn request_line(replay: &Replay) -> String {
    REQUESTS
        .lines()
        .find(|line| {
            serde_json::from_str::<Value>(line)
                .unwrap()
                .get("id")
                .and_then(Value::as_u64)
                == Some(replay.id)
        })
        .unwrap_or_else(|| panic!("missing request line {}", replay.id))
        .to_string()
}

fn parse_jsonl(body: &str) -> Vec<Value> {
    body.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_redacted(body: &str) {
    for leaked in [
        HOME,
        "codex-user",
        "/home/",
        "/Users/",
        TOKEN,
        "sk-",
        "ghp_",
        "xai-",
    ] {
        assert!(
            !body.contains(leaked),
            "fixture still contains {leaked}: {body}"
        );
    }
    assert!(
        body.contains("[REDACTED]"),
        "fixture has no redaction marker"
    );
}

fn compile_fake_codex(dir: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-permissions-request-approval/fake_app_server.rs");
    let output = dir.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    let compiled = Command::new("rustc")
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn rustc: {error}"));
    assert!(
        compiled.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    output
}

fn wait_for_json(path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
                return value;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "fake server did not capture a response at {}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}
