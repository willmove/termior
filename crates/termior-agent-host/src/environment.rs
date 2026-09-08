//! Explicit execution-environment and sandbox capability descriptions (Stage D / M9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentKind {
    Direct,
    Worktree,
    Sandboxed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLevel {
    Verified,
    Reported,
    Unknown,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxCapability {
    Filesystem,
    Network,
    Credentials,
    ProcessTree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnvironment {
    pub id: String,
    pub kind: EnvironmentKind,
    pub host: String,
    pub root: PathBuf,
    pub writable_paths: Vec<PathBuf>,
    pub capabilities: BTreeMap<SandboxCapability, TrustLevel>,
    pub backend_owner: String,
    pub verified_at_ms: Option<u64>,
}
impl ExecutionEnvironment {
    pub fn direct(id: impl Into<String>, root: PathBuf) -> Self {
        Self {
            id: id.into(),
            kind: EnvironmentKind::Direct,
            host: std::env::consts::OS.into(),
            writable_paths: vec![root.clone()],
            root,
            capabilities: BTreeMap::from([
                (SandboxCapability::Filesystem, TrustLevel::Reported),
                (SandboxCapability::Network, TrustLevel::Unknown),
                (SandboxCapability::Credentials, TrustLevel::Unknown),
                (SandboxCapability::ProcessTree, TrustLevel::Verified),
            ]),
            backend_owner: "termior".into(),
            verified_at_ms: None,
        }
    }

    pub fn worktree(worktree: &termior_vcs::WorktreeEnvironment) -> Self {
        Self {
            id: worktree.id.clone(),
            kind: EnvironmentKind::Worktree,
            host: std::env::consts::OS.into(),
            root: worktree.path.clone(),
            writable_paths: vec![worktree.path.clone()],
            capabilities: BTreeMap::from([
                (SandboxCapability::Filesystem, TrustLevel::Verified),
                (SandboxCapability::Network, TrustLevel::Unknown),
                (SandboxCapability::Credentials, TrustLevel::Unknown),
                (SandboxCapability::ProcessTree, TrustLevel::Verified),
            ]),
            backend_owner: "termior-worktree".into(),
            verified_at_ms: Some(now_ms()),
        }
    }
    pub fn validate_required(&self, required: &[SandboxCapability]) -> Result<(), SandboxError> {
        for capability in required {
            if self.capabilities.get(capability) != Some(&TrustLevel::Verified) {
                return Err(SandboxError::RequiredCapabilityUnavailable(*capability));
            }
        }
        Ok(())
    }

    pub fn task_config(
        &self,
        goal: impl Into<String>,
        backend_id: impl Into<String>,
    ) -> termior_ai::TaskConfig {
        termior_ai::TaskConfig::new(goal, self.root.clone(), backend_id, self.id.clone())
    }

    pub fn tool_executor(
        &self,
        registry: termior_ai::ToolRegistry,
    ) -> Result<termior_ai::ToolExecutor, termior_ai::ToolError> {
        termior_ai::ToolExecutor::new_in_environment(&self.root, registry, self.id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRequest {
    pub id: String,
    pub root: PathBuf,
    pub required: Vec<SandboxCapability>,
    pub allow_network: bool,
}
pub trait SandboxBackend {
    fn name(&self) -> &'static str;
    fn probe(&self) -> BTreeMap<SandboxCapability, TrustLevel>;
    fn prepare(&self, request: &SandboxRequest) -> Result<ExecutionEnvironment, SandboxError>;
    fn cleanup(&self, environment: &ExecutionEnvironment) -> Result<(), SandboxError>;
    fn wrap_command(
        &self,
        _environment: &ExecutionEnvironment,
        _program: &str,
        _args: &[String],
        _allow_network: bool,
    ) -> Result<SandboxCommand, SandboxError> {
        Err(SandboxError::CommandIsolationUnsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub clear_environment: bool,
}

impl SandboxCommand {
    pub fn spawn(&self) -> Result<Child, SandboxError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.clear_environment {
            command.env_clear().env("PATH", safe_path());
        }
        command
            .spawn()
            .map_err(|error| SandboxError::Io(error.to_string()))
    }
}

pub struct PlatformSandbox;
impl SandboxBackend for PlatformSandbox {
    fn name(&self) -> &'static str {
        if cfg!(windows) {
            "windows-job-acl"
        } else if cfg!(target_os = "macos") {
            "macos-sandbox"
        } else {
            "linux-namespace"
        }
    }
    fn probe(&self) -> BTreeMap<SandboxCapability, TrustLevel> {
        let filesystem = if (cfg!(target_os = "linux") && command_exists("bwrap"))
            || (cfg!(target_os = "macos") && command_exists("sandbox-exec"))
        {
            TrustLevel::Verified
        } else {
            TrustLevel::Unsupported
        };
        BTreeMap::from([
            (SandboxCapability::Filesystem, filesystem),
            (
                SandboxCapability::Network,
                if filesystem == TrustLevel::Verified {
                    TrustLevel::Verified
                } else {
                    TrustLevel::Unsupported
                },
            ),
            (
                SandboxCapability::Credentials,
                if filesystem == TrustLevel::Verified {
                    TrustLevel::Verified
                } else {
                    TrustLevel::Unsupported
                },
            ),
            (SandboxCapability::ProcessTree, TrustLevel::Verified),
        ])
    }
    fn prepare(&self, request: &SandboxRequest) -> Result<ExecutionEnvironment, SandboxError> {
        let root = std::fs::canonicalize(&request.root)
            .map_err(|error| SandboxError::Io(error.to_string()))?;
        let capabilities = self.probe();
        let environment = ExecutionEnvironment {
            id: request.id.clone(),
            kind: EnvironmentKind::Sandboxed,
            host: std::env::consts::OS.into(),
            root: root.clone(),
            writable_paths: vec![root],
            capabilities,
            backend_owner: self.name().into(),
            verified_at_ms: Some(now_ms()),
        };
        environment.validate_required(&request.required)?;
        Ok(environment)
    }
    fn cleanup(&self, _environment: &ExecutionEnvironment) -> Result<(), SandboxError> {
        Ok(())
    }
    fn wrap_command(
        &self,
        environment: &ExecutionEnvironment,
        program: &str,
        args: &[String],
        allow_network: bool,
    ) -> Result<SandboxCommand, SandboxError> {
        environment.validate_required(&[
            SandboxCapability::Filesystem,
            SandboxCapability::Credentials,
            SandboxCapability::ProcessTree,
        ])?;
        if cfg!(target_os = "linux") {
            let mut wrapped = vec![
                "--die-with-parent".into(),
                "--new-session".into(),
                "--unshare-pid".into(),
                "--ro-bind".into(),
                "/".into(),
                "/".into(),
                "--bind".into(),
                environment.root.display().to_string(),
                environment.root.display().to_string(),
                "--chdir".into(),
                environment.root.display().to_string(),
                "--clearenv".into(),
                "--setenv".into(),
                "PATH".into(),
                safe_path(),
            ];
            if !allow_network {
                wrapped.push("--unshare-net".into());
            }
            wrapped.push("--".into());
            wrapped.push(program.into());
            wrapped.extend_from_slice(args);
            return Ok(SandboxCommand {
                program: "bwrap".into(),
                args: wrapped,
                cwd: environment.root.clone(),
                clear_environment: true,
            });
        }
        if cfg!(target_os = "macos") {
            let root = environment.root.display().to_string().replace('"', "\\\"");
            let network = if allow_network {
                "(allow network*)"
            } else {
                "(deny network*)"
            };
            let profile = format!(
                "(version 1)(deny default)(allow process*)(allow file-read*)\
                 (allow file-write* (subpath \"{root}\")){network}"
            );
            let mut wrapped = vec!["-p".into(), profile, program.into()];
            wrapped.extend_from_slice(args);
            return Ok(SandboxCommand {
                program: "sandbox-exec".into(),
                args: wrapped,
                cwd: environment.root.clone(),
                clear_environment: true,
            });
        }
        Err(SandboxError::CommandIsolationUnsupported)
    }
}

fn safe_path() -> String {
    if cfg!(windows) {
        r"C:\Windows\System32;C:\Windows".into()
    } else {
        "/usr/local/bin:/usr/bin:/bin".into()
    }
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|path| {
            let base = path.join(program);
            base.is_file() || (cfg!(windows) && base.with_extension("exe").is_file())
        })
    })
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SandboxError {
    #[error("required sandbox capability is unavailable: {0:?}")]
    RequiredCapabilityUnavailable(SandboxCapability),
    #[error("sandbox I/O failed: {0}")]
    Io(String),
    #[error("sandbox root escapes workspace: {0}")]
    EscapedRoot(PathBuf),
    #[error("this platform has no verified command isolation wrapper")]
    CommandIsolationUnsupported,
}

pub fn ensure_confined(root: &Path, candidate: &Path) -> Result<PathBuf, SandboxError> {
    let root = std::fs::canonicalize(root).map_err(|error| SandboxError::Io(error.to_string()))?;
    let candidate =
        std::fs::canonicalize(candidate).map_err(|error| SandboxError::Io(error.to_string()))?;
    if candidate.starts_with(&root) {
        Ok(candidate)
    } else {
        Err(SandboxError::EscapedRoot(candidate))
    }
}
