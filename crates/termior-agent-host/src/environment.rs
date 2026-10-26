//! Explicit execution-environment and sandbox capability descriptions (Stage D / M9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::time::Duration;
use thiserror::Error;
use wait_timeout::ChildExt;

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
    /// Windows AppContainer confinement. When set, the command is launched inside the
    /// Termior AppContainer instead of a plain child process.
    pub app_container: bool,
}

impl SandboxCommand {
    pub fn spawn(&self) -> Result<SandboxedProcess, SandboxError> {
        if self.app_container {
            return self.spawn_app_container();
        }
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
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        // sandbox-exec does not call setsid. A new process group lets cleanup signal every
        // descendant that has not left the group. bwrap calls setsid itself (--new-session),
        // so forcing a group here would make that call fail.
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| SandboxError::Io(error.to_string()))?;
        Ok(SandboxedProcess {
            child: SandboxChild::Plain(child),
            reaped: false,
        })
    }

    /// Launch the command inside the Windows AppContainer.
    #[cfg(windows)]
    fn spawn_app_container(&self) -> Result<SandboxedProcess, SandboxError> {
        use termior_terminal::windows_sandbox::{AppContainer, APP_CONTAINER_NAME};

        let container = AppContainer::resolve(APP_CONTAINER_NAME)
            .map_err(|error| SandboxError::Io(error.to_string()))?;
        let program = PathBuf::from(&self.program);
        let process = container
            .spawn_piped(&program, &self.args, &self.cwd)
            .map_err(|error| SandboxError::Io(error.to_string()))?;
        Ok(SandboxedProcess {
            child: SandboxChild::Confined(process),
            reaped: false,
        })
    }

    #[cfg(not(windows))]
    fn spawn_app_container(&self) -> Result<SandboxedProcess, SandboxError> {
        Err(SandboxError::CommandIsolationUnsupported)
    }
}

/// A standard stream of a sandboxed process.
pub enum SandboxStream {
    Stdout(ChildStdout),
    Stderr(ChildStderr),
    /// A pipe owned by the Windows AppContainer backend.
    #[cfg(windows)]
    File(std::fs::File),
}

impl std::io::Read for SandboxStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Stdout(stream) => stream.read(buffer),
            Self::Stderr(stream) => stream.read(buffer),
            #[cfg(windows)]
            Self::File(stream) => stream.read(buffer),
        }
    }
}

/// The child process of a sandboxed command.
enum SandboxChild {
    /// A plain child process, used by the Linux and macOS wrappers.
    Plain(Child),
    /// A process running inside the Windows AppContainer.
    #[cfg(windows)]
    Confined(termior_terminal::windows_sandbox::PipedConfinedProcess),
}

/// Child of a sandbox wrapper. Drop, wait, and kill also signal the process group.
pub struct SandboxedProcess {
    child: SandboxChild,
    reaped: bool,
}

impl SandboxedProcess {
    pub fn id(&self) -> u32 {
        match &self.child {
            SandboxChild::Plain(child) => child.id(),
            #[cfg(windows)]
            SandboxChild::Confined(process) => process.pid(),
        }
    }

    pub fn take_stdout(&mut self) -> Option<SandboxStream> {
        match &mut self.child {
            SandboxChild::Plain(child) => child.stdout.take().map(SandboxStream::Stdout),
            #[cfg(windows)]
            SandboxChild::Confined(process) => process.take_stdout().map(SandboxStream::File),
        }
    }

    pub fn take_stderr(&mut self) -> Option<SandboxStream> {
        match &mut self.child {
            SandboxChild::Plain(child) => child.stderr.take().map(SandboxStream::Stderr),
            #[cfg(windows)]
            SandboxChild::Confined(process) => process.take_stderr().map(SandboxStream::File),
        }
    }

    pub fn wait(&mut self) -> Result<ExitStatus, SandboxError> {
        match &mut self.child {
            SandboxChild::Plain(child) => {
                let status = child
                    .wait()
                    .map_err(|error| SandboxError::Io(error.to_string()))?;
                self.finish();
                Ok(status)
            }
            #[cfg(windows)]
            SandboxChild::Confined(process) => {
                let code = process
                    .wait(Duration::from_secs(u32::MAX as u64))
                    .ok_or_else(|| {
                        SandboxError::Io("the sandboxed process did not exit".to_string())
                    })?;
                self.finish();
                Ok(exit_status_from_code(code))
            }
        }
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, SandboxError> {
        match &mut self.child {
            SandboxChild::Plain(child) => {
                let status = child
                    .wait_timeout(timeout)
                    .map_err(|error| SandboxError::Io(error.to_string()))?;
                if status.is_some() {
                    self.finish();
                }
                Ok(status)
            }
            #[cfg(windows)]
            SandboxChild::Confined(process) => {
                let code = process.wait(timeout);
                if code.is_some() {
                    self.finish();
                }
                Ok(code.map(exit_status_from_code))
            }
        }
    }

    pub fn kill(&mut self) -> Result<(), SandboxError> {
        match &mut self.child {
            SandboxChild::Plain(child) => {
                child
                    .kill()
                    .map_err(|error| SandboxError::Io(error.to_string()))?;
                reap_process_group(child.id());
                let _ = child.wait();
            }
            #[cfg(windows)]
            SandboxChild::Confined(process) => {
                process.kill();
            }
        }
        self.reaped = true;
        Ok(())
    }

    fn finish(&mut self) {
        if self.reaped {
            return;
        }
        self.reaped = true;
        if let SandboxChild::Plain(child) = &self.child {
            reap_process_group(child.id());
        }
    }
}

impl Drop for SandboxedProcess {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        match &mut self.child {
            SandboxChild::Plain(child) => {
                let _ = child.kill();
                reap_process_group(child.id());
                let _ = child.wait();
            }
            #[cfg(windows)]
            SandboxChild::Confined(process) => {
                process.kill();
            }
        }
        self.reaped = true;
    }
}

/// Build an `ExitStatus` from a raw Windows exit code.
#[cfg(windows)]
fn exit_status_from_code(code: u32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code)
}

pub struct PlatformSandbox;
impl SandboxBackend for PlatformSandbox {
    fn name(&self) -> &'static str {
        if cfg!(windows) {
            "windows-appcontainer"
        } else if cfg!(target_os = "macos") {
            "macos-sandbox"
        } else {
            "linux-namespace"
        }
    }
    fn probe(&self) -> BTreeMap<SandboxCapability, TrustLevel> {
        if cfg!(windows) {
            return WindowsAppContainerSandbox.probe();
        }
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
        if cfg!(windows) {
            return WindowsAppContainerSandbox.prepare(request);
        }
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
        if cfg!(windows) {
            return WindowsAppContainerSandbox.wrap_command(
                environment,
                program,
                args,
                allow_network,
            );
        }
        let mut required = vec![
            SandboxCapability::Filesystem,
            SandboxCapability::Credentials,
            SandboxCapability::ProcessTree,
        ];
        if !allow_network {
            required.push(SandboxCapability::Network);
        }
        environment.validate_required(&required)?;
        if cfg!(target_os = "linux") {
            return linux_sandbox_command(environment, program, args, allow_network);
        }
        if cfg!(target_os = "macos") {
            return macos_sandbox_command(environment, program, args, allow_network);
        }
        Err(SandboxError::CommandIsolationUnsupported)
    }
}

/// Windows sandbox backend based on an AppContainer identity (Stage D task 6.2).
///
/// An AppContainer is a low-privilege Windows identity. Filesystem access is granted
/// per directory through an ACL, the process gets no network capability, and it starts
/// from a rebuilt environment instead of the parent's. What the backend cannot enforce
/// stays unsupported so a task that requires it fails closed.
#[cfg(windows)]
pub struct WindowsAppContainerSandbox;

#[cfg(windows)]
impl SandboxBackend for WindowsAppContainerSandbox {
    fn name(&self) -> &'static str {
        "windows-appcontainer"
    }

    fn probe(&self) -> BTreeMap<SandboxCapability, TrustLevel> {
        let available = termior_terminal::windows_sandbox::app_container_available();
        BTreeMap::from([
            (
                SandboxCapability::Filesystem,
                if available {
                    TrustLevel::Verified
                } else {
                    TrustLevel::Unsupported
                },
            ),
            // The container requests no network capability, so a confined process has
            // no network access at all. Network access is therefore denied, not merely
            // unmeasured, which makes the capability verifiable.
            (
                SandboxCapability::Network,
                if available {
                    TrustLevel::Verified
                } else {
                    TrustLevel::Unsupported
                },
            ),
            // The confined process starts from a rebuilt environment and runs under a
            // separate low-privilege identity, so the parent's credentials are absent.
            (
                SandboxCapability::Credentials,
                if available {
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
        // Fail closed before any directory is shared when the platform cannot confine.
        environment_capabilities_checked(&capabilities, &request.required)?;
        termior_terminal::windows_sandbox::grant_directory(&root)
            .map_err(|error| SandboxError::Io(error.to_string()))?;
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
        let mut required = vec![
            SandboxCapability::Filesystem,
            SandboxCapability::Credentials,
            SandboxCapability::ProcessTree,
        ];
        if !allow_network {
            required.push(SandboxCapability::Network);
        }
        environment.validate_required(&required)?;
        let program = resolve_host_program(program)?;
        Ok(SandboxCommand {
            program: program.display().to_string(),
            args: args.to_vec(),
            cwd: environment.root.clone(),
            clear_environment: true,
            app_container: true,
        })
    }
}

/// Fail closed when a required capability is not verified.
fn environment_capabilities_checked(
    capabilities: &BTreeMap<SandboxCapability, TrustLevel>,
    required: &[SandboxCapability],
) -> Result<(), SandboxError> {
    for capability in required {
        if capabilities.get(capability) != Some(&TrustLevel::Verified) {
            return Err(SandboxError::RequiredCapabilityUnavailable(*capability));
        }
    }
    Ok(())
}

fn linux_sandbox_command(
    environment: &ExecutionEnvironment,
    program: &str,
    args: &[String],
    allow_network: bool,
) -> Result<SandboxCommand, SandboxError> {
    let host_program = resolve_host_program(program)?;
    let mut wrapped = vec![
        "--die-with-parent".into(),
        "--new-session".into(),
        "--unshare-pid".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ];
    // System locations the dynamic linker needs. The host root is intentionally not mounted.
    for path in [
        "/usr",
        "/usr/local",
        "/lib",
        "/lib64",
        "/bin",
        "/sbin",
        "/etc",
        "/opt",
    ] {
        if Path::new(path).exists() {
            wrapped.push("--ro-bind".into());
            wrapped.push(path.into());
            wrapped.push(path.into());
        }
    }
    let inside_program = map_linux_program(&host_program, &mut wrapped)?;
    let host_root = environment.root.display().to_string();
    wrapped.extend([
        "--bind".into(),
        host_root,
        "/workspace".into(),
        // The bubblewrap root is a writable tmpfs until this point. Remount it
        // read-only after the workspace bind so `../` cannot create files there.
        "--remount-ro".into(),
        "/".into(),
        "--chdir".into(),
        "/workspace".into(),
        "--clearenv".into(),
        "--setenv".into(),
        "PATH".into(),
        safe_path(),
    ]);
    if !allow_network {
        wrapped.push("--unshare-net".into());
    }
    wrapped.push("--".into());
    wrapped.push(inside_program);
    wrapped.extend_from_slice(args);
    Ok(SandboxCommand {
        program: "bwrap".into(),
        args: wrapped,
        cwd: environment.root.clone(),
        clear_environment: true,
        app_container: false,
    })
}

fn map_linux_program(program: &Path, args: &mut Vec<String>) -> Result<String, SandboxError> {
    let text = program.display().to_string();
    const PREFIXES: &[&str] = &[
        "/usr/",
        "/lib/",
        "/lib64/",
        "/bin/",
        "/sbin/",
        "/etc/",
        "/opt/",
        "/usr/local/",
    ];
    if PREFIXES.iter().any(|prefix| text.starts_with(prefix)) {
        return Ok(text);
    }
    let file_name = program
        .file_name()
        .ok_or_else(|| SandboxError::Io(format!("sandbox program has no file name: {text}")))?;
    let parent = program
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            SandboxError::Io(format!("sandbox program has no parent directory: {text}"))
        })?;
    args.push("--ro-bind".into());
    args.push(parent.display().to_string());
    args.push("/termior-bin".into());
    Ok(format!("/termior-bin/{}", file_name.to_string_lossy()))
}

fn macos_sandbox_command(
    environment: &ExecutionEnvironment,
    program: &str,
    args: &[String],
    allow_network: bool,
) -> Result<SandboxCommand, SandboxError> {
    let host_program = resolve_host_program(program)?;
    let profile = macos_profile(&environment.root, &host_program, allow_network);
    let mut wrapped = vec!["-p".into(), profile, host_program.display().to_string()];
    wrapped.extend_from_slice(args);
    Ok(SandboxCommand {
        program: "sandbox-exec".into(),
        args: wrapped,
        cwd: environment.root.clone(),
        clear_environment: true,
        app_container: false,
    })
}

fn macos_profile(root: &Path, program: &Path, allow_network: bool) -> String {
    let root = scheme_escape(&root.display().to_string());
    // `(deny default)` also blocks the mach and mmap operations dyld and libsystem
    // use before main. Without this baseline, sandbox-exec SIGKILLs the child during
    // image load and the parent only sees an empty, unsuccessful exit.
    let mut rules = vec![
        "(version 1)".to_owned(),
        "(deny default)".to_owned(),
        "(allow process-fork process-exec)".to_owned(),
        "(allow process-info* (target self))".to_owned(),
        "(allow signal (target self))".to_owned(),
        "(allow mach-lookup)".to_owned(),
        "(allow mach-priv-host-port)".to_owned(),
        "(allow mach-register)".to_owned(),
        "(allow sysctl-read)".to_owned(),
        "(allow ipc-posix-shm)".to_owned(),
        "(allow ipc-posix-sem)".to_owned(),
        "(allow file-ioctl)".to_owned(),
        "(allow file-read-metadata)".to_owned(),
        "(allow file-map-executable)".to_owned(),
        // macOS 26 aborts during dyld if the root vnode itself is unreadable,
        // even when /usr and /System are allowed. A literal is not a subpath,
        // so this does not grant the directory tree under /.
        "(allow file-read* (literal \"/\"))".to_owned(),
    ];
    for subpath in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/opt",
        "/dev",
        "/private/etc",
        "/private/var/db",
        "/private/preboot",
        "/Applications",
    ] {
        rules.push(format!(
            "(allow file-read* (subpath \"{}\"))",
            scheme_escape(subpath)
        ));
    }
    if let Some(parent) = program.parent().and_then(scheme_subpath) {
        rules.push(format!("(allow file-read* (subpath \"{parent}\"))"));
    }
    rules.push(format!("(allow file-read* (subpath \"{root}\"))"));
    rules.push(format!("(allow file-write* (subpath \"{root}\"))"));
    rules.push("(allow file-write* (literal \"/dev/null\"))".to_owned());
    rules.push("(allow file-write* (literal \"/dev/dtracehelper\"))".to_owned());
    if allow_network {
        rules.push("(allow network*)".to_owned());
    } else {
        rules.push("(deny network*)".to_owned());
    }
    rules.join("\n")
}

fn scheme_subpath(path: &Path) -> Option<String> {
    let text = path.display().to_string();
    if text.is_empty() || text == "/" {
        None
    } else {
        Some(scheme_escape(&text))
    }
}

fn scheme_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn resolve_host_program(program: &str) -> Result<PathBuf, SandboxError> {
    let path = PathBuf::from(program);
    let candidate = if path.is_absolute() {
        path
    } else {
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).find_map(|dir| {
                    let candidate = dir.join(program);
                    candidate.is_file().then_some(candidate)
                })
            })
            .ok_or_else(|| SandboxError::Io(format!("sandbox program not found: {program}")))?
    };
    candidate
        .canonicalize()
        .map_err(|error| SandboxError::Io(error.to_string()))
}

fn reap_process_group(pid: u32) {
    // `--` stops procps from parsing `kill -KILL -PID` as a signal plus a short group id.
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
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
