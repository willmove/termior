use std::time::Duration;

use termior_agent_host::{AgentBackend, CodexBackend};

/// Optional local smoke: requires a compatible stable Codex CLI, but starts no model turn.
#[test]
#[ignore = "requires a locally installed compatible Codex CLI"]
fn installed_codex_completes_the_stable_stdio_handshake() {
    let executable = std::env::var("TERMIOR_CODEX_PATH").unwrap_or_else(|_| "codex".into());
    let mut backend = CodexBackend::new(executable);
    let info = backend.initialize().unwrap();
    assert_eq!(info.id, "codex-app-server");
    assert!(info.capabilities.can_resume());
    backend.shutdown(Duration::from_secs(2)).unwrap();
}
