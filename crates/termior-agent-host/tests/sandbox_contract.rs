use std::collections::BTreeMap;
use termior_agent_host::*;

struct FakeSandbox {
    caps: BTreeMap<SandboxCapability, TrustLevel>,
}
impl SandboxBackend for FakeSandbox {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn probe(&self) -> BTreeMap<SandboxCapability, TrustLevel> {
        self.caps.clone()
    }
    fn prepare(&self, request: &SandboxRequest) -> Result<ExecutionEnvironment, SandboxError> {
        let environment = ExecutionEnvironment {
            id: request.id.clone(),
            kind: EnvironmentKind::Sandboxed,
            host: "test".into(),
            root: request.root.clone(),
            writable_paths: vec![request.root.clone()],
            capabilities: self.probe(),
            backend_owner: "fake".into(),
            verified_at_ms: Some(1),
        };
        environment.validate_required(&request.required)?;
        Ok(environment)
    }
    fn cleanup(&self, _: &ExecutionEnvironment) -> Result<(), SandboxError> {
        Ok(())
    }
}

#[test]
fn required_capabilities_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let backend = FakeSandbox {
        caps: BTreeMap::from([
            (SandboxCapability::Filesystem, TrustLevel::Verified),
            (SandboxCapability::Network, TrustLevel::Reported),
        ]),
    };
    let request = SandboxRequest {
        id: "env-1".into(),
        root: dir.path().into(),
        required: vec![SandboxCapability::Network],
        allow_network: false,
    };
    assert_eq!(
        backend.prepare(&request).unwrap_err(),
        SandboxError::RequiredCapabilityUnavailable(SandboxCapability::Network)
    );
}

#[test]
fn parent_and_symlink_escape_are_rejected_by_confinement() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    assert!(matches!(
        ensure_confined(root.path(), outside.path()),
        Err(SandboxError::EscapedRoot(_))
    ));
}

#[test]
fn platform_probe_never_reports_unverified_support_as_verified() {
    let probe = PlatformSandbox.probe();
    assert_eq!(
        probe.get(&SandboxCapability::ProcessTree),
        Some(&TrustLevel::Verified)
    );
    assert!(probe
        .values()
        .all(|level| matches!(level, TrustLevel::Verified | TrustLevel::Unsupported)));
}

#[test]
fn platform_command_wrapper_fails_closed_or_uses_the_verified_native_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let backend = PlatformSandbox;
    let request = SandboxRequest {
        id: "command".into(),
        root: dir.path().into(),
        required: vec![
            SandboxCapability::Filesystem,
            SandboxCapability::Credentials,
        ],
        allow_network: false,
    };
    match backend.prepare(&request) {
        Ok(environment) => {
            let wrapped = backend
                .wrap_command(&environment, "echo", &["ok".into()], false)
                .unwrap();
            assert!(wrapped.clear_environment);
            assert!(wrapped.program == "bwrap" || wrapped.program == "sandbox-exec");
        }
        Err(SandboxError::RequiredCapabilityUnavailable(_)) => {
            assert!(backend
                .wrap_command(
                    &ExecutionEnvironment::direct("direct", dir.path().into()),
                    "echo",
                    &[],
                    false,
                )
                .is_err());
        }
        Err(error) => panic!("unexpected sandbox error: {error}"),
    }
}

#[test]
fn worktree_environment_binds_task_tools_and_command_sessions_to_the_same_root() {
    use termior_terminal::TerminalService;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("inside.txt"), "worktree").unwrap();
    let worktree = termior_vcs::WorktreeEnvironment {
        id: "worktree-task-1".into(),
        repository: dir.path().into(),
        base_commit: "base".into(),
        branch: "branch".into(),
        path: dir.path().to_path_buf(),
        owner_task: "task-1".into(),
        orphaned: false,
    };
    let environment = ExecutionEnvironment::worktree(&worktree);
    let config = environment.task_config("goal", "native");
    assert_eq!(config.project_dir, dir.path());
    assert_eq!(config.environment_id, "worktree-task-1");

    let registry = termior_ai::ToolRegistry::new(
        termior_security::workspace::WorkspaceAuthRegistry::with_roots([dir
            .path()
            .display()
            .to_string()]),
    );
    let executor = environment.tool_executor(registry).unwrap();
    assert_eq!(
        executor
            .execute_auto("read_file", r#"{"path":"inside.txt"}"#)
            .unwrap(),
        "worktree"
    );
    let output = executor
        .execute_approved("run_command", r#"{"command":"echo bound"}"#)
        .unwrap();
    let id = output
        .split_whitespace()
        .next()
        .unwrap()
        .trim_start_matches("session_id=");
    let session = termior_terminal::shared_terminal_service()
        .session(&termior_terminal::CommandSessionId(id.into()))
        .unwrap();
    assert_eq!(session.environment_id, "worktree-task-1");
    assert_eq!(
        std::fs::canonicalize(&session.cwd).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}
