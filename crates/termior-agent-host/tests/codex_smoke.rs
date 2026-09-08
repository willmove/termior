use std::time::Duration;

use termior_agent_host::{AgentBackend, CodexBackend};

/// Optional local smoke: requires `codex-cli 0.137.x` on PATH, but does not start a model turn.
#[test]
#[ignore = "requires a locally installed compatible Codex CLI"]
fn installed_codex_completes_the_stable_stdio_handshake() {
    let mut backend = CodexBackend::new("codex");
    let info = backend.initialize().unwrap();
    assert_eq!(info.id, "codex-app-server");
    assert!(info.capabilities.can_resume());
    backend.shutdown(Duration::from_secs(2)).unwrap();
}
