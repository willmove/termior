//! PTY 会话封装（FR-TERM-01/05/06/07）。
//!
//! 基于 `portable-pty`：openpty → spawn shell → 暴露 reader/writer/resize。
//! spawn 时注入 shell integration（OSC 7 cwd / OSC 133 prompt 边界），复用
//! `termior_terminal_core::shell_integration` 生成的脚本片段。

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, MasterPty, PtySize};
#[cfg(windows)]
use termior_terminal_core::shell_integration::windows_to_wsl_path;
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
    /// Whether the child receives the full application environment. Private terminals retain
    /// only the small set of variables required to locate and start the user's shell.
    pub inherit_environment: bool,
    pub rows: u16,
    pub cols: u16,
    pub cwd: Option<String>,
    /// Authorization granted when the workspace was explicitly opened. A configured cwd that is
    /// not inside this registry is dropped (spawn falls back to the default directory) so a bad
    /// cwd can never kill the terminal, while unauthorized directories stay unused (FR-SEC-04).
    pub workspace_auth: Option<termior_security::workspace::WorkspaceAuthRegistry>,
    /// Optional WSL distribution name (Windows, FR-WS-06). When set, the PTY spawns into that
    /// distribution via `wsl.exe -d <distro>` instead of a native Windows shell. Ignored on
    /// non-Windows. [`ShellKind::Bash`] is assumed inside a distribution.
    pub wsl_distribution: Option<String>,
}

impl Default for PtySessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            shell_program: None,
            shell_integration: false,
            inherit_environment: true,
            rows: 24,
            cols: 80,
            cwd: None,
            workspace_auth: None,
            wsl_distribution: None,
        }
    }
}

/// 一个活的 PTY 会话：持有 master、writer、child 与 shell integration 临时目录。
///
/// reader 由 [`crate::bridge::TerminalBridge`] 在独立线程里消费（`take_reader`），
/// 故此处不持有 reader。`integration_dir` 让注入脚本在会话存活期间不被清理。
/// child 句柄可由 [`PtySession::take_child`] 取走交给退出监听线程（阻塞 `wait`），
/// 会话侧保留 `clone_killer` 得到的 killer，kill 能力不受影响。
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// spawn 时解析出的实际 shell 类型（含 WSL 改写与平台默认探测）。
    shell_kind: ShellKind,
    /// 是否为 WSL 会话。
    is_wsl: bool,
    /// 保持 shell integration 脚本文件存活；drop 时随会话清理。
    _integration_dir: Option<tempfile::TempDir>,
    #[cfg(windows)]
    _job: WindowsJob,
}

impl PtySession {
    /// 打开 PTY 并 spawn shell，按配置注入 shell integration。
    pub fn spawn(config: &PtySessionConfig) -> Result<Self, SpawnError> {
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

        let wsl = config.wsl_distribution.clone();
        let inferred_kind = config
            .shell_program
            .as_deref()
            .and_then(shell_kind_from_program);
        // 在 WSL 发行版里，默认 shell 是 Linux 的；把 Windows 原生探测（pwsh/powershell/cmd）
        // 过滤掉，只有用户显式选了 bash/zsh/fish 才尊重，否则按 Bash（WSL 发行版的通用默认）。
        let wsl_native_kind = |kind: ShellKind| match kind {
            ShellKind::Bash | ShellKind::Zsh | ShellKind::Fish => Some(kind),
            ShellKind::Pwsh | ShellKind::PowerShell | ShellKind::Cmd => None,
        };
        let kind = if wsl.is_some() {
            config
                .shell
                .or(inferred_kind)
                .and_then(wsl_native_kind)
                .unwrap_or(ShellKind::Bash)
        } else {
            config.shell.or(inferred_kind).unwrap_or_else(default_shell)
        };
        // 在 WSL 里，尊重用户显式设置的 Linux shell 程序；否则用按 kind 推出的可执行名。
        // 若用户设了 Windows shell（pwsh/powershell/cmd），上面已把 kind 改写成 Bash，
        // 这里同样丢弃那个 Windows 程序名，避免 `wsl.exe ... -- powershell` 的矛盾组合。
        let program = if wsl.is_some() {
            config
                .shell_program
                .as_deref()
                .filter(|p| {
                    wsl_native_kind(shell_kind_from_program(p).unwrap_or(ShellKind::Cmd)).is_some()
                })
                .map(String::from)
                .unwrap_or_else(|| shell_program(kind))
        } else {
            config
                .shell_program
                .clone()
                .unwrap_or_else(|| shell_program(kind))
        };
        let known_integration_kind =
            config.shell.is_some() || inferred_kind.is_some() || config.shell_program.is_none();
        let integration_enabled = config.shell_integration && known_integration_kind;
        log::info!(
            "PTY spawning shell: {:?} ({program}), integration={integration_enabled}, wsl={:?}",
            kind,
            wsl
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

        let killer = child.clone_killer();

        Ok(Self {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            killer,
            child: Some(child),
            shell_kind: kind,
            is_wsl: wsl.is_some(),
            _integration_dir: integration_dir,
            #[cfg(windows)]
            _job: job,
        })
    }

    /// 本会话实际使用的 shell 类型（spawn 时解析的终值）。
    pub fn shell_kind(&self) -> ShellKind {
        self.shell_kind
    }

    /// 是否为 WSL 会话。
    pub fn is_wsl(&self) -> bool {
        self.is_wsl
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

    /// 取走子进程句柄（一次性）：由调用方线程阻塞 `wait()` 监听退出。
    /// 之后 [`PtySession::kill`] 仍有效（killer 是独立克隆的句柄）。
    pub fn take_child(&mut self) -> Option<Box<dyn portable_pty::Child + Send + Sync>> {
        self.child.take()
    }

    /// 尝试 kill 子进程。
    pub fn kill(&mut self) -> Result<(), SpawnError> {
        self.killer
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
    if config.wsl_distribution.is_some() {
        return build_wsl_command(kind, integration_enabled, config);
    }
    build_native_command(kind, program, integration_enabled, config)
}

/// 构造原生（非 WSL）shell 命令。
fn build_native_command(
    kind: ShellKind,
    program: &str,
    integration_enabled: bool,
    config: &PtySessionConfig,
) -> Result<(CommandBuilder, Option<tempfile::TempDir>), SpawnError> {
    let mut cmd = CommandBuilder::new(program);
    if !config.inherit_environment {
        let retained = private_environment();
        cmd.env_clear();
        for (key, value) in retained {
            cmd.env(key, value);
        }
    }
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

    if let Some(cwd) = resolve_cwd(config) {
        cmd.cwd(cwd);
    }

    Ok((cmd, integration_dir))
}

/// 构造 WSL 命令（Windows, FR-WS-06）。spawn `wsl.exe -d <distro> --cd <cwd> -- bash <args>`，
/// 让 shell integration 在 Linux 发行版里生效。
#[cfg(windows)]
fn build_wsl_command(
    kind: ShellKind,
    integration_enabled: bool,
    config: &PtySessionConfig,
) -> Result<(CommandBuilder, Option<tempfile::TempDir>), SpawnError> {
    let distro = config.wsl_distribution.as_deref().unwrap_or_default();
    let mut cmd = CommandBuilder::new("wsl.exe");
    cmd.arg("-d");
    cmd.arg(distro);
    if let Some(cwd) = resolve_cwd(config) {
        cmd.arg("--cd");
        cmd.arg(cwd);
    }
    cmd.arg("--");
    cmd.arg(shell_program(kind));

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
            apply_wsl_integration(&mut cmd, &snippets, tempdir.path(), kind);
            Some(tempdir)
        }
        None => None,
    };
    Ok((cmd, integration_dir))
}

#[cfg(not(windows))]
fn build_wsl_command(
    _kind: ShellKind,
    _integration_enabled: bool,
    _config: &PtySessionConfig,
) -> Result<(CommandBuilder, Option<tempfile::TempDir>), SpawnError> {
    Err(SpawnError::Spawn(
        "WSL distributions are only supported on Windows".into(),
    ))
}

/// `wsl.exe --cd` 接受 Windows 路径并自动转换；shell 内部读取的 rcfile / ZDOTDIR 需要是
/// `/mnt/<drive>/...` 形式，由 [`windows_to_wsl_path`] 转换。
#[cfg(windows)]
fn apply_wsl_integration(
    cmd: &mut CommandBuilder,
    snippets: &ShellIntegrationSnippets,
    tempdir: &std::path::Path,
    kind: ShellKind,
) {
    match kind {
        ShellKind::Bash => {
            let rcfile = windows_to_wsl_path(&tempdir.join("termior-bashrc.sh"));
            cmd.arg("--rcfile");
            cmd.arg(rcfile);
        }
        ShellKind::Zsh => {
            if let Some(real) = std::env::var_os("ZDOTDIR") {
                cmd.env("TERMior_REAL_ZDOTDIR", real);
            }
            cmd.env("ZDOTDIR", windows_to_wsl_path(tempdir));
        }
        ShellKind::Pwsh | ShellKind::PowerShell | ShellKind::Fish | ShellKind::Cmd => {
            // PowerShell/Cmd/Fish 在 WSL 里通常不适用，跳过 integration wrapper。
        }
    }
    for (k, v) in &snippets.env {
        cmd.env(k, v);
    }
}

/// 决定 spawn 实际使用的 cwd（FR-SEC-04）。
///
/// 无效 cwd 不应让整个终端 spawn 失败：**丢弃 cwd（继承默认目录）严格安全于
/// 使用它**——授权检查依旧强制，只是降级为告警而非报错。丢弃场景：
/// - 未授权（不在 [`WorkspaceAuthRegistry`]，或未提供注册表）；
/// - 目录不存在（Windows 上过期 cwd 会让 ConPTY spawn 直接失败）。
///
/// 已授权且存在的 cwd 原样使用。
fn resolve_cwd(config: &PtySessionConfig) -> Option<String> {
    let cwd = config.cwd.as_ref()?;
    let authorized = config
        .workspace_auth
        .as_ref()
        .is_some_and(|registry| registry.is_authorized(cwd));
    if !authorized {
        log::warn!("PTY cwd is not authorized; spawning without a cwd: {cwd}");
        return None;
    }
    if !std::path::Path::new(cwd).is_dir() {
        log::warn!("PTY cwd is not an existing directory; spawning without a cwd: {cwd}");
        return None;
    }
    Some(cwd.clone())
}

/// Keep the platform variables required to resolve executables, home directories and temporary
/// files without leaking arbitrary application/session variables into a private terminal.
fn private_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(key, _)| private_environment_key(key))
        .collect()
}

fn private_environment_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    [
        "PATH",
        "HOME",
        "USERPROFILE",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
        "LANG",
        "LC_ALL",
        "SHELL",
    ]
    .iter()
    .any(|allowed| key.eq_ignore_ascii_case(allowed))
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

/// 列出本机已安装的 WSL 发行版（Windows, FR-WS-06）。非 Windows 或 wsl.exe 不可用时
/// 返回空列表（调用方据此隐藏 WSL 选项）。
pub fn list_wsl_distributions() -> std::io::Result<Vec<String>> {
    if !cfg!(windows) {
        return Ok(Vec::new());
    }
    let output = std::process::Command::new("wsl.exe")
        .arg("--list")
        .arg("--quiet")
        .output()?;
    Ok(parse_wsl_list_output(&output.stdout))
}

/// 解析 `wsl.exe --list --quiet` 的输出。该命令默认以 UTF-16LE 编码输出，且可能带 BOM。
/// 非法 UTF-16 的孤立代理项被替换；空行与空白条目被忽略。纯函数，便于测试。
pub fn parse_wsl_list_output(bytes: &[u8]) -> Vec<String> {
    let text = decode_wsl_output(bytes);
    text.lines()
        .map(|line| line.trim_matches(|c: char| c == '\r' || c == '\n' || c == ' ' || c == '\u{0}'))
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

/// `wsl.exe` 输出通常是 UTF-16LE（可能带 BOM）；退化到 UTF-8 兜底。
fn decode_wsl_output(bytes: &[u8]) -> String {
    if let Some(stripped) = bytes.strip_prefix(b"\xFF\xFE") {
        return decode_utf16_le(stripped);
    }
    if let Some(stripped) = bytes.strip_prefix(b"\xFE\xFF") {
        return decode_utf16_be(stripped);
    }
    // 启发式：全是对齐的零高字节 → 视作 UTF-16LE。
    if bytes.len() % 2 == 0 && bytes.iter().skip(1).step_by(2).all(|&b| b == 0) && !bytes.is_empty()
    {
        return decode_utf16_le(bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16_le(bytes: &[u8]) -> String {
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    String::from_utf16_lossy(&units.collect::<Vec<u16>>())
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]));
    String::from_utf16_lossy(&units.collect::<Vec<u16>>())
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
    fn private_terminal_keeps_only_required_environment() {
        assert!(private_environment_key(std::ffi::OsStr::new("PATH")));
        assert!(private_environment_key(std::ffi::OsStr::new("SystemRoot")));
        assert!(!private_environment_key(std::ffi::OsStr::new(
            "TERMIOR_PARENT_SESSION_SECRET"
        )));

        let config = PtySessionConfig {
            inherit_environment: false,
            ..PtySessionConfig::default()
        };
        let (command, _) = build_command(ShellKind::Cmd, "cmd", false, &config).unwrap();
        assert_eq!(
            command.get_env("TERM"),
            Some(std::ffi::OsStr::new("xterm-256color"))
        );
        if let Some((key, _)) =
            std::env::vars_os().find(|(key, _)| !private_environment_key(key) && key != "TERM")
        {
            assert_eq!(command.get_env(key), None);
        }
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
    fn unauthorized_cwd_is_dropped_not_fatal() {
        use termior_security::workspace::WorkspaceAuthRegistry;
        // 类似 Windows OSC 7 解析畸形产生的垃圾 cwd：不授权 → 丢弃，spawn 继续。
        let config = PtySessionConfig {
            cwd: Some("DESKTOP-ABCC:/Users/x/proj".into()),
            workspace_auth: Some(WorkspaceAuthRegistry::new()),
            ..PtySessionConfig::default()
        };
        assert_eq!(resolve_cwd(&config), None);
        let (command, _) = build_command(ShellKind::Bash, "bash", false, &config).unwrap();
        assert!(
            command.get_cwd().is_none(),
            "unauthorized cwd must be dropped, not passed to the child"
        );
    }

    #[test]
    fn missing_auth_registry_drops_cwd() {
        let config = PtySessionConfig {
            cwd: Some("/home/u/proj".into()),
            workspace_auth: None,
            ..PtySessionConfig::default()
        };
        assert_eq!(resolve_cwd(&config), None);
        let (command, _) = build_command(ShellKind::Bash, "bash", false, &config).unwrap();
        assert!(command.get_cwd().is_none());
    }

    #[test]
    fn authorized_existing_cwd_is_used() {
        use termior_security::workspace::WorkspaceAuthRegistry;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let mut registry = WorkspaceAuthRegistry::new();
        registry.authorize(&path);
        let config = PtySessionConfig {
            cwd: Some(path.clone()),
            workspace_auth: Some(registry),
            ..PtySessionConfig::default()
        };
        assert_eq!(resolve_cwd(&config), Some(path.clone()));
        let (command, _) = build_command(ShellKind::Bash, "bash", false, &config).unwrap();
        assert_eq!(
            command.get_cwd().map(|c| c.to_string_lossy().into_owned()),
            Some(path)
        );
    }

    #[test]
    fn authorized_but_nonexistent_cwd_is_dropped() {
        use termior_security::workspace::WorkspaceAuthRegistry;
        // Windows 上过期 cwd 会让 ConPTY spawn 失败：授权但不存在 → 丢弃。
        let stale = "/no/such/termior-stale-dir";
        let mut registry = WorkspaceAuthRegistry::new();
        registry.authorize(stale);
        let config = PtySessionConfig {
            cwd: Some(stale.into()),
            workspace_auth: Some(registry),
            ..PtySessionConfig::default()
        };
        assert_eq!(resolve_cwd(&config), None);
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

    #[test]
    fn parse_wsl_list_handles_utf16le_with_bom() {
        // `wsl.exe --list --quiet` 典型输出：UTF-16LE + BOM，每行末尾带 CRLF（也编码在
        // UTF-16 里）。行间夹的 NUL 是高字节。
        let names = ["Ubuntu", "Debian", "kali-linux"];
        let mut bytes = vec![0xFF, 0xFE];
        for name in &names {
            for ch in name.encode_utf16() {
                bytes.extend_from_slice(&ch.to_le_bytes());
            }
            bytes.extend_from_slice(&0x000D_u16.to_le_bytes()); // \r
            bytes.extend_from_slice(&0x000A_u16.to_le_bytes()); // \n
        }
        assert_eq!(parse_wsl_list_output(&bytes), names.to_vec());
    }

    #[test]
    fn parse_wsl_list_handles_plain_utf8() {
        // 非 Windows wsl 或被 `WSL_UTF8` 改写后可能直接是 UTF-8。
        let bytes = b"Ubuntu\nDebian\n\nkali-linux\n";
        assert_eq!(
            parse_wsl_list_output(bytes),
            vec!["Ubuntu", "Debian", "kali-linux"]
        );
    }

    #[test]
    fn parse_wsl_list_ignores_blank_and_whitespace_lines() {
        let bytes = b"   \nUbuntu\n\n \n";
        assert_eq!(parse_wsl_list_output(bytes), vec!["Ubuntu"]);
    }

    #[test]
    fn parse_wsl_list_empty_output_yields_empty() {
        assert!(parse_wsl_list_output(&[]).is_empty());
        assert!(parse_wsl_list_output(b"\xFF\xFE").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn wsl_build_command_targets_distribution_via_wsl_exe() {
        let config = PtySessionConfig {
            wsl_distribution: Some("Ubuntu".into()),
            ..PtySessionConfig::default()
        };
        let (command, integration_dir) =
            build_command(ShellKind::Bash, "bash", true, &config).unwrap();
        let args = argv(&command);
        assert_eq!(args[0], "wsl.exe");
        // -d <distro> 后跟 -- --rcfile 的 Linux 形式。
        let d_idx = args
            .iter()
            .position(|a| a == "-d")
            .expect("wsl -d flag present");
        assert_eq!(args[d_idx + 1], "Ubuntu");
        assert!(args.iter().any(|a| a == "--"));
        assert!(args.iter().any(|a| a == "bash"));
        // rcfile 必须是 /mnt/... 形式，不能是 Windows 盘符。
        let rcfile = args
            .iter()
            .find(|a| a.starts_with("/mnt/"))
            .expect("rcfile converted to a WSL-visible path");
        assert!(rcfile.ends_with("termior-bashrc.sh"));
        assert!(integration_dir.is_some());
    }

    #[cfg(windows)]
    #[test]
    fn wsl_build_command_without_integration_skips_rcfile() {
        let config = PtySessionConfig {
            wsl_distribution: Some("Debian".into()),
            shell_integration: false,
            ..PtySessionConfig::default()
        };
        let (command, integration_dir) =
            build_command(ShellKind::Bash, "bash", false, &config).unwrap();
        let args = argv(&command);
        assert_eq!(args[0], "wsl.exe");
        assert!(args.iter().any(|a| a == "bash"));
        assert!(
            !args.iter().any(|a| a.starts_with("/mnt/")),
            "no rcfile when integration is off"
        );
        assert!(integration_dir.is_none());
    }
}
