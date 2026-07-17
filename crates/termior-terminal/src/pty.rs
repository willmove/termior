//! PTY 会话封装（FR-TERM-01/05/06/07）。
//!
//! 基于 `portable-pty`：openpty → spawn shell → 暴露 reader/writer/resize。
//! spawn 时注入 shell integration（OSC 7 cwd / OSC 133 prompt 边界），复用
//! `termior_terminal_core::shell_integration` 生成的脚本片段。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize};
use termior_terminal_core::shell_integration::{ShellIntegrationSnippets, ShellKind};
use thiserror::Error;

/// PTY spawn 或 IO 失败。
#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("pty open failed: {0}")]
    Open(String),
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("shell integration write failed: {0}")]
    Integration(String),
}

/// PTY 会话配置：选择 shell、初始尺寸、工作目录。
#[derive(Debug, Clone)]
pub struct PtySessionConfig {
    /// None 时按平台探测默认 shell。
    pub shell: Option<ShellKind>,
    /// Optional executable path/name. Manual shell settings use this field.
    pub shell_program: Option<String>,
    /// Keep shell integration opt-in so the plain interactive shell remains the reliability
    /// baseline while integration evolves independently.
    pub shell_integration: bool,
    pub rows: u16,
    pub cols: u16,
    pub cwd: Option<String>,
    /// Authorization granted when the workspace was explicitly opened. A configured cwd is
    /// rejected unless it is inside this registry (FR-SEC-04).
    pub workspace_auth: Option<termior_security::workspace::WorkspaceAuthRegistry>,
}

impl Default for PtySessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            shell_program: None,
            shell_integration: false,
            rows: 24,
            cols: 80,
            cwd: None,
            workspace_auth: None,
        }
    }
}

/// 一个活的 PTY 会话：持有 master、writer、child 与 shell integration 临时目录。
///
/// reader 由 [`crate::bridge::TerminalBridge`] 在独立线程里消费（`take_reader`），
/// 故此处不持有 reader。`integration_dir` 让注入脚本在会话存活期间不被清理。
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// 保持 shell integration 脚本文件存活；drop 时随会话清理。
    _integration_dir: Option<tempfile::TempDir>,
    #[cfg(windows)]
    _job: WindowsJob,
}

impl PtySession {
    /// 打开 PTY 并 spawn shell，按配置注入 shell integration。
    pub fn spawn(config: &PtySessionConfig) -> Result<Self, SpawnError> {
        if let Some(cwd) = &config.cwd {
            let authorized = config
                .workspace_auth
                .as_ref()
                .is_some_and(|registry| registry.is_authorized(cwd));
            if !authorized {
                return Err(SpawnError::Spawn(format!(
                    "workspace is not authorized for PTY cwd: {cwd}"
                )));
            }
        }
        #[cfg(windows)]
        let _spawn_guard = conpty_spawn_lock()
            .lock()
            .map_err(|_| SpawnError::Spawn("ConPTY spawn lock poisoned".into()))?;
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SpawnError::Open(e.to_string()))?;

        let inferred_kind = config
            .shell_program
            .as_deref()
            .and_then(shell_kind_from_program);
        let kind = config.shell.or(inferred_kind).unwrap_or_else(default_shell);
        let program = config
            .shell_program
            .clone()
            .unwrap_or_else(|| shell_program(kind));
        let known_integration_kind =
            config.shell.is_some() || inferred_kind.is_some() || config.shell_program.is_none();
        let integration_enabled = config.shell_integration && known_integration_kind;
        log::info!(
            "PTY spawning shell: {:?} ({program}), integration={integration_enabled}",
            kind
        );
        let (cmd, integration_dir) = build_command(kind, &program, integration_enabled, config)?;

        // child spawn 在 slave 上（CommandBuilder 按值消费）。
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SpawnError::Spawn(e.to_string()))?;

        #[cfg(windows)]
        let mut child = child;

        #[cfg(windows)]
        let job = match child
            .process_id()
            .ok_or_else(|| SpawnError::Spawn("spawned shell has no process id".into()))
            .and_then(WindowsJob::for_process)
        {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                return Err(error);
            }
        };

        // 关键：spawn 后释放 slave 句柄，否则 PTY 不回 EOF、reader 可能挂死（spec 技术要点）。
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| SpawnError::Open(format!("take_writer: {e}")))?;

        Ok(Self {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            child,
            _integration_dir: integration_dir,
            #[cfg(windows)]
            _job: job,
        })
    }

    /// 克隆 reader（在调用方线程里 read PTY 输出）。可多次克隆。
    pub fn take_reader(&self) -> Result<Box<dyn Read + Send>, SpawnError> {
        self.master
            .try_clone_reader()
            .map_err(|e| SpawnError::Open(format!("try_clone_reader: {e}")))
    }

    /// 写句柄（线程安全包装，供键盘转发并发写入）。
    pub fn writer(&self) -> Arc<Mutex<Box<dyn Write + Send>>> {
        Arc::clone(&self.writer)
    }

    /// resize 终端尺寸。
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), SpawnError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SpawnError::Io(std::io::Error::other(e.to_string())))
    }

    /// 等待子进程退出（关闭 tab 时调用）。
    pub fn wait(mut self) -> Result<(), SpawnError> {
        self.child
            .wait()
            .map_err(|e| SpawnError::Spawn(format!("wait: {e}")))?;
        Ok(())
    }

    /// 尝试 kill 子进程。
    pub fn kill(&mut self) -> Result<(), SpawnError> {
        self.child
            .kill()
            .map_err(|e| SpawnError::Spawn(format!("kill: {e}")))
    }
}

/// 按平台探测默认 shell（FR-TERM-05）。
/// Unix 跟随 `$SHELL`（zsh/bash/fish）；Windows 依次探测 pwsh → powershell → cmd。
pub fn default_shell() -> ShellKind {
    if cfg!(windows) {
        if which("pwsh").is_ok() {
            ShellKind::Pwsh
        } else if which("powershell").is_ok() {
            ShellKind::PowerShell
        } else {
            ShellKind::Cmd
        }
    } else {
        match std::env::var("SHELL").ok().as_deref() {
            Some(s) if s.contains("zsh") => ShellKind::Zsh,
            Some(s) if s.contains("fish") => ShellKind::Fish,
            Some(s) if s.contains("bash") => ShellKind::Bash,
            _ => ShellKind::Bash,
        }
    }
}

#[cfg(windows)]
fn conpty_spawn_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(windows)]
struct WindowsJob(isize);

#[cfg(windows)]
impl WindowsJob {
    fn for_process(pid: u32) -> Result<Self, SpawnError> {
        use std::mem::{size_of, zeroed};
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        // SAFETY: All handles are checked for null, the information pointer and length match the
        // Windows structure, and ownership is transferred into WindowsJob only after assignment.
        unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(SpawnError::Spawn(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(SpawnError::Spawn(error.to_string()));
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(SpawnError::Spawn(error.to_string()));
            }
            let assigned = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assigned == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(SpawnError::Spawn(error.to_string()));
            }
            Ok(Self(job as isize))
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        // SAFETY: This is the unique owned job handle; closing it triggers KILL_ON_JOB_CLOSE.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(
                self.0 as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

/// 构造 spawn 命令：落盘 shell integration 脚本，设置程序/args/env。
///
/// 返回 `(CommandBuilder, Option<TempDir>)`；tempdir 由调用方持有以保持脚本存活。
fn build_command(
    kind: ShellKind,
    program: &str,
    integration_enabled: bool,
    config: &PtySessionConfig,
) -> Result<(CommandBuilder, Option<tempfile::TempDir>), SpawnError> {
    let mut cmd = CommandBuilder::new(program);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Termior");

    let snippets = integration_enabled
        .then(|| termior_terminal_core::shell_integration::snippets_for(kind))
        .flatten();
    let integration_dir = match snippets {
        Some(snippets) => {
            let tempdir = tempfile::tempdir()
                .map_err(|e| SpawnError::Integration(format!("shell integration tempdir: {e}")))?;
            for (name, content) in &snippets.files {
                let path = tempdir.path().join(name);
                std::fs::write(&path, content)?;
            }
            apply_integration(&mut cmd, &snippets, tempdir.path(), kind);
            Some(tempdir)
        }
        None => None, // cmd 等无 shell integration 的 shell
    };

    if let Some(cwd) = &config.cwd {
        cmd.cwd(cwd);
    }

    Ok((cmd, integration_dir))
}

/// shell 可执行名。
fn shell_program(kind: ShellKind) -> String {
    match kind {
        ShellKind::Zsh => "zsh".into(),
        ShellKind::Bash => "bash".into(),
        ShellKind::Fish => "fish".into(),
        ShellKind::Pwsh => "pwsh".into(),
        ShellKind::PowerShell => "powershell".into(),
        ShellKind::Cmd => "cmd".into(),
    }
}

/// 按 shell 类型应用注入参数与环境（FR-TERM-06）。
fn apply_integration(
    cmd: &mut CommandBuilder,
    snippets: &ShellIntegrationSnippets,
    tempdir: &std::path::Path,
    kind: ShellKind,
) {
    match kind {
        ShellKind::Zsh => {
            // ZDOTDIR 四件套：把临时目录设为 ZDOTDIR，让 zsh 读我们的 .zshenv/.zshrc。
            // 真实用户配置通过 TERMior_REAL_ZDOTDIR / HOME 回退 source（见 snippets）。
            if let Some(real) = std::env::var_os("ZDOTDIR") {
                cmd.env("TERMior_REAL_ZDOTDIR", real);
            }
            cmd.env("ZDOTDIR", tempdir);
        }
        ShellKind::Bash => {
            // bash：--rcfile <临时 rcfile>。
            let rcfile = tempdir.join("termior-bashrc.sh");
            cmd.arg("--rcfile");
            cmd.arg(rcfile);
        }
        ShellKind::Pwsh | ShellKind::PowerShell => {
            // `-File` alone exits after running the wrapper. Flags must precede the script path;
            // tokens after `-File <path>` are script arguments rather than host options.
            let profile = tempdir.join("profile.ps1");
            cmd.arg("-NoProfile");
            cmd.arg("-NoExit");
            cmd.arg("-File");
            cmd.arg(profile);
        }
        ShellKind::Fish | ShellKind::Cmd => {
            // These shells currently have no integration wrapper.
        }
    }
    for (k, v) in &snippets.env {
        cmd.env(k, v);
    }
}

fn shell_kind_from_program(program: &str) -> Option<ShellKind> {
    let normalized = program.replace('\\', "/");
    let file_name = normalized.rsplit('/').next()?;
    let lowercase = file_name.to_ascii_lowercase();
    let name = lowercase.strip_suffix(".exe").unwrap_or(&lowercase);
    match name {
        "zsh" => Some(ShellKind::Zsh),
        "bash" => Some(ShellKind::Bash),
        "fish" => Some(ShellKind::Fish),
        "pwsh" => Some(ShellKind::Pwsh),
        "powershell" => Some(ShellKind::PowerShell),
        "cmd" => Some(ShellKind::Cmd),
        _ => None,
    }
}

/// PATH 上找可执行（探测 pwsh/powershell 用）。
fn which(name: &str) -> std::io::Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").ok_or_else(|| std::io::Error::other("no PATH"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if cfg!(windows) {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Ok(with_ext);
            }
        }
    }
    Err(std::io::Error::other(format!("{name} not found in PATH")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(command: &CommandBuilder) -> Vec<String> {
        command
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plain_shell_is_the_default_launch_path() {
        let config = PtySessionConfig::default();
        let (command, integration_dir) =
            build_command(ShellKind::Pwsh, "pwsh", false, &config).unwrap();
        assert_eq!(argv(&command), vec!["pwsh"]);
        assert!(integration_dir.is_none());
        assert_eq!(
            command.get_env("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        );
        assert_eq!(
            command.get_env("COLORTERM"),
            Some(std::ffi::OsStr::new("truecolor"))
        );
    }

    #[test]
    fn powershell_integration_keeps_the_shell_interactive() {
        let config = PtySessionConfig::default();
        let (command, integration_dir) =
            build_command(ShellKind::Pwsh, "pwsh", true, &config).unwrap();
        let args = argv(&command);
        assert_eq!(&args[..4], ["pwsh", "-NoProfile", "-NoExit", "-File"]);
        assert_eq!(
            args.len(),
            5,
            "PowerShell host flags must not be duplicated"
        );
        assert!(args[4].ends_with("profile.ps1"));
        assert!(integration_dir.is_some());
    }

    #[test]
    fn bash_rcfile_flag_is_not_duplicated() {
        let config = PtySessionConfig::default();
        let (command, integration_dir) =
            build_command(ShellKind::Bash, "bash", true, &config).unwrap();
        let args = argv(&command);
        assert_eq!(args[0], "bash");
        assert_eq!(args.iter().filter(|arg| *arg == "--rcfile").count(), 1);
        assert_eq!(args.len(), 3);
        assert!(args[2].ends_with("termior-bashrc.sh"));
        assert!(integration_dir.is_some());
    }

    #[test]
    fn manual_shell_kind_is_inferred_from_executable_path() {
        assert_eq!(
            shell_kind_from_program(r"C:\Program Files\PowerShell\7\PWSH.EXE"),
            Some(ShellKind::Pwsh)
        );
        assert_eq!(
            shell_kind_from_program("/usr/bin/fish"),
            Some(ShellKind::Fish)
        );
        assert_eq!(shell_kind_from_program("nu"), None);
    }
}
