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
    pub rows: u16,
    pub cols: u16,
    pub cwd: Option<String>,
}

impl Default for PtySessionConfig {
    fn default() -> Self {
        Self {
            shell: None,
            rows: 24,
            cols: 80,
            cwd: None,
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
}

impl PtySession {
    /// 打开 PTY 并 spawn shell，按配置注入 shell integration。
    pub fn spawn(config: &PtySessionConfig) -> Result<Self, SpawnError> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| SpawnError::Open(e.to_string()))?;

        let kind = config.shell.unwrap_or_else(default_shell);
        let (cmd, integration_dir) = build_command(kind, config)?;

        // child spawn 在 slave 上（CommandBuilder 按值消费）。
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| SpawnError::Spawn(e.to_string()))?;

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
        which("pwsh")
            .or_else(|_| which("powershell"))
            .map(|_| ShellKind::Pwsh)
            .unwrap_or(ShellKind::Cmd)
    } else {
        match std::env::var("SHELL").ok().as_deref() {
            Some(s) if s.contains("zsh") => ShellKind::Zsh,
            Some(s) if s.contains("bash") => ShellKind::Bash,
            _ => ShellKind::Bash,
        }
    }
}

/// 构造 spawn 命令：落盘 shell integration 脚本，设置程序/args/env。
///
/// 返回 `(CommandBuilder, Option<TempDir>)`；tempdir 由调用方持有以保持脚本存活。
fn build_command(
    kind: ShellKind,
    config: &PtySessionConfig,
) -> Result<(CommandBuilder, Option<tempfile::TempDir>), SpawnError> {
    let program = shell_program(kind);
    let mut cmd = CommandBuilder::new(&program);
    cmd.env("TERM", "xterm-256color");

    let integration_dir = match termior_terminal_core::shell_integration::snippets_for(kind) {
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
            // bash：--rcfile <临时 rcfile>（snippets 只给了 flag，补路径）。
            let rcfile = tempdir.join("termior-bashrc.sh");
            cmd.arg("--rcfile");
            cmd.arg(rcfile);
        }
        ShellKind::Pwsh | ShellKind::PowerShell => {
            // pwsh：-NoProfile -File <profile.ps1>，snippets 已给 flag，补路径。
            let profile = tempdir.join("profile.ps1");
            cmd.arg("-File");
            cmd.arg(profile);
        }
        ShellKind::Cmd => {
            // cmd 无 shell integration（snippets_for 返回 None，不会到这里）。
        }
    }
    for (k, v) in &snippets.env {
        cmd.env(k, v);
    }
    // snippets.args 里剩余的非 flag 参数（目前为空）补上。
    for a in &snippets.args {
        cmd.arg(a);
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
