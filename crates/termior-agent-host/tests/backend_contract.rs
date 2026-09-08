use serde_json::json;
use std::time::Duration;
use termior_agent_host::{
    map_codex_message, AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent,
    BackendInfo, BackendTaskProjector, CapabilityProfile, CapabilitySupport, CodexProtocol,
    InProcessBackend, JsonRpcLineCodec, ManagedAgentSession, ManagedSessionState, PumpOutcome,
    StdioTransport, TaskLaunch, TransportCommand, TurnLaunch,
};

#[test]
fn undeclared_capabilities_are_unknown_and_disable_actions() {
    let profile = CapabilityProfile::default();
    assert_eq!(profile.session_resume, CapabilitySupport::Unknown);
    assert!(!profile.can_resume());
    assert!(!profile.can_fork());
}

#[test]
fn codex_profile_exposes_only_verified_stable_capabilities() {
    let profile = CodexProtocol::stable_0_137().capabilities();
    assert!(profile.can_resume());
    assert!(profile.can_cancel());
    assert!(profile.approval_requests.is_supported());
    assert_eq!(profile.host_terminal, CapabilitySupport::Unknown);
    assert!(!profile.can_fork());
}

#[test]
fn initialize_explicitly_opts_out_of_experimental_api() {
    let request = CodexProtocol::stable_0_137().initialize_request(7);
    assert_eq!(request["method"], "initialize");
    assert_eq!(request["params"]["capabilities"]["experimentalApi"], false);
    assert_eq!(request["params"]["clientInfo"]["name"], "termior");
}

#[test]
fn codec_accepts_partial_frames_and_multiple_json_lines() {
    let mut codec = JsonRpcLineCodec::default();
    assert!(codec
        .push(b"{\"jsonrpc\":\"2.0\",\"id\":2")
        .unwrap()
        .is_empty());
    let messages = codec
        .push(b",\"result\":{}}\n{\"jsonrpc\":\"2.0\",\"method\":\"unknown\"}\n")
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["id"], 2);
    assert_eq!(messages[1]["method"], "unknown");
}

#[test]
fn unknown_notifications_remain_diagnostics_without_faking_state() {
    let raw = json!({"jsonrpc":"2.0", "method":"future/event", "params":{"x":1}});
    assert!(matches!(
        map_codex_message(&raw),
        BackendEvent::Diagnostic { method, .. } if method == "future/event"
    ));
}

#[test]
fn stable_codex_notifications_map_to_transport_neutral_events() {
    let delta = json!({
        "jsonrpc":"2.0",
        "method":"item/agentMessage/delta",
        "params":{"threadId":"th-1", "turnId":"turn-1", "itemId":"item-1", "delta":"hello"}
    });
    assert!(matches!(
        map_codex_message(&delta),
        BackendEvent::AgentMessageDelta { text, .. } if text == "hello"
    ));

    let completed = json!({
        "jsonrpc":"2.0",
        "method":"turn/completed",
        "params":{"threadId":"th-1", "turn":{"id":"turn-1", "status":"completed", "items":[]}}
    });
    assert!(matches!(
        map_codex_message(&completed),
        BackendEvent::TurnCompleted { turn_id, success: true, .. } if turn_id == "turn-1"
    ));
}

#[test]
fn approval_mapping_preserves_backend_request_and_tool_ids() {
    let request = json!({
        "jsonrpc":"2.0",
        "id":42,
        "method":"item/commandExecution/requestApproval",
        "params":{"threadId":"th-1", "turnId":"turn-1", "itemId":"tool-9", "command":"cargo test"}
    });
    assert!(matches!(
        map_codex_message(&request),
        BackendEvent::ApprovalRequested { backend_request_id, tool_call_id, .. }
            if backend_request_id == "42" && tool_call_id == "tool-9"
    ));
}

#[test]
fn protocol_rejects_incompatible_codex_versions() {
    let protocol = CodexProtocol::stable_0_137();
    assert!(protocol.accepts_version("codex-cli 0.137.8"));
    assert!(!protocol.accepts_version("codex-cli 0.138.0"));
    assert!(!protocol.accepts_version("unknown"));
    assert_eq!(protocol.request_timeout(), Duration::from_secs(30));
}

#[test]
fn current_stable_protocol_accepts_each_explicitly_verified_minor_line() {
    let protocol = CodexProtocol::stable_verified();
    assert!(protocol.accepts_version("codex-cli 0.137.0"));
    assert!(protocol.accepts_version("codex-cli 0.153.4"));
    assert!(!protocol.accepts_version("codex-cli 0.154.0"));
}

#[test]
fn stdio_transport_keeps_notifications_and_stderr_separate_from_responses() {
    let (program, args) = if cfg!(windows) {
        (
            "powershell".to_string(),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "$null=[Console]::In.ReadLine(); [Console]::Error.WriteLine('diagnostic-noise sk-12345678901234567890'); [Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"method\":\"future/event\",\"params\":{}}'); [Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}')".into(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".into(),
                "read line; echo 'diagnostic-noise sk-12345678901234567890' >&2; echo '{\"jsonrpc\":\"2.0\",\"method\":\"future/event\",\"params\":{}}'; echo '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}'".into(),
            ],
        )
    };
    let mut transport = StdioTransport::spawn(&TransportCommand { program, args }).unwrap();
    let result = transport
        .request("test", json!({}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(result["ok"], true);
    let event = transport
        .next_event(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert_eq!(event["method"], "future/event");
    std::thread::sleep(Duration::from_millis(20));
    let stderr = transport.stderr_tail();
    assert!(stderr.contains("diagnostic-noise"));
    assert!(stderr.contains("REDACTED"));
    assert!(!stderr.contains("sk-123"));
}

#[test]
fn built_in_runtime_passes_the_same_backend_lifecycle_contract() {
    struct NoTools;
    impl termior_ai::RuntimeToolExecutor for NoTools {
        fn execute(
            &self,
            _call_id: &str,
            _tool: &str,
            _normalized_arguments: &str,
            _cancellation: &termior_ai::CancellationToken,
        ) -> Result<termior_ai::ToolExecution, String> {
            Err("unexpected tool".into())
        }
    }

    let provider = std::sync::Arc::new(termior_ai::MockProvider::single(vec![
        termior_ai::ChatEvent::TextDelta("hello".into()),
        termior_ai::ChatEvent::Done(termior_ai::Message::assistant("hello")),
    ]));
    let mut backend = InProcessBackend::new(provider, std::sync::Arc::new(NoTools));
    let info = backend.initialize().unwrap();
    assert_eq!(info.id, "termior-built-in");
    let binding = backend
        .create_task(TaskLaunch {
            task_id: "task-contract".into(),
            project_dir: std::env::current_dir().unwrap().display().to_string(),
            environment_id: "direct".into(),
            model: Some("test".into()),
        })
        .unwrap();
    let binding = backend
        .start_turn(TurnLaunch {
            task_id: binding.task_id.clone(),
            backend_session_id: binding.backend_session_id.clone(),
            text: "hi".into(),
        })
        .unwrap();
    assert!(binding.backend_turn_id.is_some());

    let events =
        std::iter::from_fn(|| backend.next_event(Duration::ZERO).unwrap()).collect::<Vec<_>>();
    assert!(events.iter().any(
        |event| matches!(event, BackendEvent::AgentMessageDelta { text, .. } if text == "hello")
    ));
    assert!(events
        .iter()
        .any(|event| matches!(event, BackendEvent::TurnCompleted { success: true, .. })));
}

#[test]
fn sanitized_codex_fixture_is_a_non_panicking_protocol_regression_suite() {
    let fixture = include_str!("fixtures/codex-0.137-stable.jsonl");
    let events = fixture
        .lines()
        .map(|line| map_codex_message(&serde_json::from_str(line).unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 6);
    assert!(events.iter().any(|event| matches!(event, BackendEvent::SessionCreated { session_id, .. } if session_id == "thr_fixture")));
    assert!(events.iter().any(|event| matches!(event, BackendEvent::ApprovalRequested { backend_request_id, .. } if backend_request_id == "17")));
    assert!(events.iter().any(|event| matches!(event, BackendEvent::Diagnostic { method, .. } if method == "future/nonCritical")));
    assert!(matches!(
        events.last(),
        Some(BackendEvent::TurnCompleted { success: true, .. })
    ));
}

#[test]
fn backend_events_are_projected_before_they_drive_task_state() {
    let mut projector = BackendTaskProjector::new(termior_ai::TaskId("task-external".into()));
    let unknown = BackendEvent::Diagnostic {
        method: "future/event".into(),
        raw: json!({}),
    };
    assert_eq!(projector.apply(&unknown).len(), 1);
    assert_eq!(projector.state(), termior_ai::TaskState::Idle);

    let approval = BackendEvent::ApprovalRequested {
        backend_request_id: "1".into(),
        session_id: "thread".into(),
        turn_id: "turn".into(),
        tool_call_id: "call".into(),
        request_kind: "command".into(),
        raw: json!({}),
    };
    let projected = projector.apply(&approval);
    assert_eq!(projector.state(), termior_ai::TaskState::WaitingApproval);
    assert!(projected.iter().any(|event| matches!(
        &event.kind,
        termior_ai::TaskEventKind::Waiting { reason: termior_ai::WaitingReason::Approval { call_id } }
            if call_id == "call"
    )));
}

struct ApprovalBackend {
    events: std::collections::VecDeque<BackendEvent>,
    responded: bool,
}

impl AgentBackend for ApprovalBackend {
    fn initialize(&mut self) -> Result<BackendInfo, BackendError> {
        Ok(BackendInfo {
            id: "fake".into(),
            display_name: "Fake".into(),
            version: "1".into(),
            authentication_source: "test".into(),
            model_source: "test".into(),
            capabilities: CapabilityProfile {
                cancel: CapabilitySupport::Supported,
                approval_requests: CapabilitySupport::Supported,
                ..CapabilityProfile::default()
            },
        })
    }

    fn capabilities(&self) -> CapabilityProfile {
        CapabilityProfile {
            cancel: CapabilitySupport::Supported,
            approval_requests: CapabilitySupport::Supported,
            ..CapabilityProfile::default()
        }
    }

    fn create_task(&mut self, launch: TaskLaunch) -> Result<BackendBinding, BackendError> {
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: "session-1".into(),
            backend_turn_id: None,
        })
    }

    fn resume_task(&mut self, binding: BackendBinding) -> Result<BackendBinding, BackendError> {
        Ok(binding)
    }

    fn start_turn(&mut self, launch: TurnLaunch) -> Result<BackendBinding, BackendError> {
        self.events.push_back(BackendEvent::AgentMessageDelta {
            session_id: launch.backend_session_id.clone(),
            turn_id: "turn-1".into(),
            item_id: "message-1".into(),
            text: "working".into(),
        });
        self.events.push_back(BackendEvent::ApprovalRequested {
            backend_request_id: "request-7".into(),
            session_id: launch.backend_session_id.clone(),
            turn_id: "turn-1".into(),
            tool_call_id: "tool-9".into(),
            request_kind: "command".into(),
            raw: json!({"command":"cargo test"}),
        });
        Ok(BackendBinding {
            task_id: launch.task_id,
            backend_session_id: launch.backend_session_id,
            backend_turn_id: Some("turn-1".into()),
        })
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<Option<BackendEvent>, BackendError> {
        Ok(self.events.pop_front())
    }

    fn respond(&mut self, decision: BackendDecision) -> Result<(), BackendError> {
        assert_eq!(decision.backend_request_id, "request-7");
        assert!(decision.approved);
        self.responded = true;
        self.events.push_back(BackendEvent::TurnCompleted {
            session_id: "session-1".into(),
            turn_id: "turn-1".into(),
            success: true,
            status: "completed".into(),
            raw: json!({}),
        });
        Ok(())
    }

    fn cancel(&mut self, _binding: &BackendBinding) -> Result<(), BackendError> {
        Ok(())
    }

    fn shutdown(&mut self, _timeout: Duration) -> Result<(), BackendError> {
        Ok(())
    }
}

#[test]
fn managed_session_preserves_process_binding_across_approval_and_completion() {
    let backend = ApprovalBackend {
        events: std::collections::VecDeque::new(),
        responded: false,
    };
    let mut session = ManagedAgentSession::connect(
        Box::new(backend),
        TaskLaunch {
            task_id: "task-managed".into(),
            project_dir: std::env::current_dir().unwrap().display().to_string(),
            environment_id: "direct".into(),
            model: None,
        },
    )
    .unwrap();
    session.start_turn("implement").unwrap();
    let mut observed = Vec::new();
    let paused = session
        .pump_for(Duration::from_secs(1), &mut |event| {
            observed.push(event.clone())
        })
        .unwrap();
    assert!(matches!(
        paused,
        PumpOutcome::WaitingApproval {
            backend_request_id,
            tool_call_id,
            ..
        } if backend_request_id == "request-7" && tool_call_id == "tool-9"
    ));
    assert_eq!(session.state(), ManagedSessionState::WaitingApproval);
    assert!(observed.iter().any(
        |event| matches!(event, BackendEvent::AgentMessageDelta { text, .. } if text == "working")
    ));

    let completed = session
        .respond(true, &mut |_| {}, Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        completed,
        PumpOutcome::Completed { success: true, .. }
    ));
    assert_eq!(session.state(), ManagedSessionState::Completed);
}
