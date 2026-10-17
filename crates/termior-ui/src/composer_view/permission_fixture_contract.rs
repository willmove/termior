use super::orchestrator::ExternalPendingApproval;
use termior_agent_host::{
    map_codex_message, permissions_approval_result, BackendEvent, PERMISSIONS_APPROVAL_METHOD,
};

const REQUESTS: &str = include_str!(
    "../../../termior-agent-host/tests/fixtures/codex-permissions-request-approval/requests.jsonl"
);
const RESPONSES: &str = include_str!(
    "../../../termior-agent-host/tests/fixtures/codex-permissions-request-approval/responses.jsonl"
);

#[test]
fn recorded_permission_fixtures_map_through_the_approval_card() {
    for response in jsonl(RESPONSES) {
        let id = response["id"].as_u64().unwrap();
        let decision = response["decision"].as_str().unwrap();
        let granted = strings(&response["granted"]);
        let request = jsonl(REQUESTS)
            .into_iter()
            .find(|value| value["id"].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("missing request {id}"));
        let event = map_codex_message(&request);
        let BackendEvent::ApprovalRequested {
            backend_request_id,
            tool_call_id,
            request_kind,
            raw,
            ..
        } = event
        else {
            panic!("fixture {id} did not map to an approval");
        };
        assert_eq!(request_kind, PERMISSIONS_APPROVAL_METHOD);
        assert_eq!(backend_request_id, id.to_string());
        assert!(raw.get("scope").is_none());
        let mut pending = ExternalPendingApproval::from_codex(
            backend_request_id,
            tool_call_id,
            request_kind,
            &raw,
        );
        assert!(pending.scope.is_empty());
        assert!(pending.is_permissions());
        if decision == "partial" {
            for choice in &mut pending.permissions {
                choice.checked = granted.iter().any(|entry| entry == &choice.id);
            }
        }
        let approved = decision != "deny";
        let ids = pending.granted_permission_ids(approved).unwrap();
        assert_eq!(ids, granted);
        let profile = raw
            .get("permissions")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        assert_eq!(
            permissions_approval_result(&profile, approved, Some(&ids)),
            response["message"]["result"]
        );
        assert!(
            !pending.arguments.contains("codex-user") && !pending.arguments.contains("sk-"),
            "{}",
            pending.arguments
        );
        for choice in &pending.permissions {
            assert!(!choice.label.contains("codex-user"), "{}", choice.label);
        }
    }
}

fn jsonl(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}
