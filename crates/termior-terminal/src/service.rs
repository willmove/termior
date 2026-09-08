//! Thread-safe local command registry with cursor-based retained output (FR-ATERM).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use futures::StreamExt;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_terminal_core::command::{
    CommandCapabilities, CommandCreate, CommandSession, CommandSessionId, Controller,
    FakeTerminalService, OutputCursor, OutputRead, OutputStream, TerminalService,
    TerminalServiceError, WaitResult,
};

use crate::{PtySessionConfig, TerminalBridge, WriterHandle};

struct ProcessHandle {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
}

struct PtyHandle {
    bridge: Mutex<TerminalBridge>,
    writer: WriterHandle,
}

/// Local process implementation of [`TerminalService`].
///
/// Every spawned command gets a stable opaque ID. Reader and watcher threads update the shared
/// retained-output registry, so callers never block the UI thread on process I/O.
pub struct LocalTerminalService {
    state: Arc<FakeTerminalService>,
    processes: Mutex<HashMap<CommandSessionId, Arc<ProcessHandle>>>,
    ptys: Mutex<HashMap<CommandSessionId, Arc<PtyHandle>>>,
    pane_writers: Mutex<HashMap<CommandSessionId, WriterHandle>>,
}

impl LocalTerminalService {
    pub fn new(output_budget_bytes: usize) -> Self {
        Self {
            state: Arc::new(FakeTerminalService::with_output_budget(output_budget_bytes)),
            processes: Mutex::new(HashMap::new()),
            ptys: Mutex::new(HashMap::new()),
            pane_writers: Mutex::new(HashMap::new()),
        }
    }

    fn process(&self, id: &CommandSessionId) -> Result<Arc<ProcessHandle>, TerminalServiceError> {
        self.processes
            .lock()
            .map_err(|_| lock_error())?
            .get(id)
            .cloned()
            .ok_or_else(|| TerminalServiceError::NotFound(id.clone()))
    }

    pub fn session_ids(&self) -> Vec<CommandSessionId> {
        let mut ids: Vec<CommandSessionId> = self
            .processes
            .lock()
            .map(|processes| processes.keys().cloned().collect())
            .unwrap_or_default();
        if let Ok(ptys) = self.ptys.lock() {
            ids.extend(ptys.keys().cloned());
        }
        if let Ok(panes) = self.pane_writers.lock() {
            ids.extend(panes.keys().cloned());
        }
        ids
    }

    pub fn register_pane(
        &self,
        request: CommandCreate,
        writer: WriterHandle,
    ) -> Result<CommandSessionId, TerminalServiceError> {
        validate_scope(&request)?;
        let id = self.state.create_with_capabilities(
            request,
            CommandCapabilities {
                stdin: true,
                resize: false,
                kill: false,
                detach: false,
            },
        )?;
        self.pane_writers
            .lock()
            .map_err(|_| lock_error())?
            .insert(id.clone(), writer);
        Ok(id)
    }

    pub fn observe_pane_output(
        &self,
        id: &CommandSessionId,
        bytes: Vec<u8>,
    ) -> Result<OutputCursor, TerminalServiceError> {
        self.state.push_output(id, OutputStream::Pty, bytes)
    }

    pub fn observe_pane_exit(
        &self,
        id: &CommandSessionId,
        exit_code: Option<i32>,
    ) -> Result<(), TerminalServiceError> {
        match exit_code {
            Some(code) => self.state.finish(id, code),
            None => self.state.mark_unknown(id),
        }
    }

    /// Best-effort cancellation used when the owning Agent task is cancelled.
    /// Returns false when any active process could not be confirmed stopped.
    pub fn kill_all(&self) -> bool {
        let mut all_stopped = true;
        for id in self.session_ids() {
            all_stopped &= self.kill(&id).is_ok();
        }
        all_stopped
    }
}

impl Default for LocalTerminalService {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}

impl TerminalService for LocalTerminalService {
    fn create(&self, request: CommandCreate) -> Result<CommandSessionId, TerminalServiceError> {
        validate_scope(&request)?;
        if request.interactive {
            return self.create_pty(request);
        }
        let mut command = shell_command(&request);
        command
            .current_dir(&request.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if request.interactive {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| TerminalServiceError::Io(error.to_string()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();
        let capabilities = CommandCapabilities {
            stdin: request.interactive,
            resize: false,
            kill: true,
            detach: true,
        };
        let id = self.state.create_with_capabilities(request, capabilities)?;
        let handle = Arc::new(ProcessHandle {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
        });
        self.processes
            .lock()
            .map_err(|_| lock_error())?
            .insert(id.clone(), handle.clone());

        if let Some(stdout) = stdout {
            spawn_reader(self.state.clone(), id.clone(), OutputStream::Stdout, stdout)?;
        }
        if let Some(stderr) = stderr {
            spawn_reader(self.state.clone(), id.clone(), OutputStream::Stderr, stderr)?;
        }
        spawn_watcher(self.state.clone(), id.clone(), handle)?;
        Ok(id)
    }

    fn session(&self, id: &CommandSessionId) -> Result<CommandSession, TerminalServiceError> {
        self.state.session(id)
    }

    fn read_output(
        &self,
        id: &CommandSessionId,
        after: Option<OutputCursor>,
        max_bytes: usize,
    ) -> Result<OutputRead, TerminalServiceError> {
        self.state.read_output(id, after, max_bytes)
    }

    fn wait(
        &self,
        id: &CommandSessionId,
        timeout: Duration,
    ) -> Result<WaitResult, TerminalServiceError> {
        self.state.wait(id, timeout)
    }

    fn write_stdin(
        &self,
        id: &CommandSessionId,
        controller: &Controller,
        bytes: &[u8],
    ) -> Result<(), TerminalServiceError> {
        self.state.write_stdin(id, controller, bytes)?;
        if let Some(pty) = self.ptys.lock().map_err(|_| lock_error())?.get(id).cloned() {
            return pty
                .writer
                .write_all(bytes)
                .map_err(|error| TerminalServiceError::Io(error.to_string()));
        }
        if let Some(writer) = self
            .pane_writers
            .lock()
            .map_err(|_| lock_error())?
            .get(id)
            .cloned()
        {
            return writer
                .write_all(bytes)
                .map_err(|error| TerminalServiceError::Io(error.to_string()));
        }
        let process = self.process(id)?;
        let mut stdin = process.stdin.lock().map_err(|_| lock_error())?;
        stdin
            .as_mut()
            .ok_or(TerminalServiceError::Unsupported("stdin"))?
            .write_all(bytes)
            .and_then(|()| stdin.as_mut().expect("stdin checked above").flush())
            .map_err(|error| TerminalServiceError::Io(error.to_string()))
    }

    fn resize(
        &self,
        id: &CommandSessionId,
        rows: u16,
        cols: u16,
    ) -> Result<(), TerminalServiceError> {
        if let Some(pty) = self.ptys.lock().map_err(|_| lock_error())?.get(id).cloned() {
            return pty
                .bridge
                .lock()
                .map_err(|_| lock_error())?
                .resize(rows, cols)
                .map_err(|error| TerminalServiceError::Io(error.to_string()));
        }
        self.state.resize(id, rows, cols)
    }

    fn kill(&self, id: &CommandSessionId) -> Result<(), TerminalServiceError> {
        if self.state.session(id)?.state.is_terminal() {
            return Ok(());
        }
        if let Some(pty) = self.ptys.lock().map_err(|_| lock_error())?.get(id).cloned() {
            if pty.bridge.lock().map_err(|_| lock_error())?.kill().is_err() {
                self.state.mark_unknown(id)?;
                return Ok(());
            }
            return self.state.kill(id);
        }
        let process = self.process(id)?;
        let mut child = process.child.lock().map_err(|_| lock_error())?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| TerminalServiceError::Io(error.to_string()))?
        {
            let _ = self.state.finish(id, status.code().unwrap_or(-1));
            return Ok(());
        }
        let pid = child.id();
        terminate_process_tree(pid);
        if let Err(error) = child.kill() {
            if child.try_wait().ok().flatten().is_none() {
                return Err(TerminalServiceError::Io(error.to_string()));
            }
        }
        drop(child);
        self.state.kill(id)
    }

    fn release(&self, id: &CommandSessionId, detach: bool) -> Result<(), TerminalServiceError> {
        self.state.release(id, detach)?;
        self.processes.lock().map_err(|_| lock_error())?.remove(id);
        self.ptys.lock().map_err(|_| lock_error())?.remove(id);
        self.pane_writers
            .lock()
            .map_err(|_| lock_error())?
            .remove(id);
        Ok(())
    }

    fn take_over(&self, id: &CommandSessionId) -> Result<OutputCursor, TerminalServiceError> {
        self.state.take_over(id)
    }

    fn hand_back(
        &self,
        id: &CommandSessionId,
        controller: Controller,
    ) -> Result<OutputCursor, TerminalServiceError> {
        self.state.hand_back(id, controller)
    }
}

static SHARED_TERMINAL_SERVICE: std::sync::OnceLock<Arc<LocalTerminalService>> =
    std::sync::OnceLock::new();

pub fn shared_terminal_service() -> Arc<LocalTerminalService> {
    SHARED_TERMINAL_SERVICE
        .get_or_init(|| Arc::new(LocalTerminalService::default()))
        .clone()
}

impl LocalTerminalService {
    fn create_pty(&self, request: CommandCreate) -> Result<CommandSessionId, TerminalServiceError> {
        let mut auth = WorkspaceAuthRegistry::new();
        auth.authorize(&request.project_dir);
        let mut bridge = TerminalBridge::spawn(&PtySessionConfig {
            shell_program: Some(request.shell.clone()),
            shell_integration: true,
            rows: request.rows,
            cols: request.cols,
            cwd: Some(request.cwd.clone()),
            workspace_auth: Some(auth),
            ..PtySessionConfig::default()
        })
        .map_err(|error| TerminalServiceError::Io(error.to_string()))?;
        let mut output = bridge
            .take_output()
            .ok_or_else(|| TerminalServiceError::Io("PTY output unavailable".into()))?;
        let writer = bridge.writer();
        let persistent = request.command.trim().is_empty();
        let id = self.state.create_with_capabilities(
            request.clone(),
            CommandCapabilities {
                stdin: true,
                resize: true,
                kill: true,
                detach: false,
            },
        )?;
        let handle = Arc::new(PtyHandle {
            bridge: Mutex::new(bridge),
            writer: writer.clone(),
        });
        self.ptys
            .lock()
            .map_err(|_| lock_error())?
            .insert(id.clone(), handle);
        let state = self.state.clone();
        let output_id = id.clone();
        let protocol_writer = writer.clone();
        thread::Builder::new()
            .name(format!("termior-command-pty-output-{}", id.0))
            .spawn(move || {
                let mut finished = false;
                let mut protocol_tail = Vec::new();
                while let Some(data) = futures::executor::block_on(output.next()) {
                    protocol_tail.extend_from_slice(&data.bytes);
                    if protocol_tail.windows(4).any(|window| window == b"\x1b[6n")
                        || protocol_tail.windows(5).any(|window| window == b"\x1b[?6n")
                    {
                        let _ = protocol_writer.write_all(b"\x1b[1;1R");
                    }
                    if protocol_tail.windows(3).any(|window| window == b"\x1b[c") {
                        let _ = protocol_writer.write_all(b"\x1b[?1;2c");
                    }
                    if protocol_tail.len() > 8 {
                        protocol_tail.drain(..protocol_tail.len() - 8);
                    }
                    let _ = state.push_output(&output_id, OutputStream::Pty, data.bytes);
                    if !finished {
                        for event in data.events {
                            if !persistent {
                                if let termior_terminal_core::OscEvent::CommandExit { code } = event
                                {
                                    let _ = state.finish(&output_id, code.unwrap_or(-1));
                                    finished = true;
                                }
                            }
                        }
                    }
                }
                if !finished {
                    let _ = state.mark_unknown(&output_id);
                }
            })
            .map_err(|error| TerminalServiceError::Io(error.to_string()))?;
        let line_ending = if cfg!(windows) { "\r\n" } else { "\n" };
        writer
            .write_all(format!("{}{line_ending}", request.command).as_bytes())
            .map_err(|error| TerminalServiceError::Io(error.to_string()))?;
        Ok(id)
    }
}

impl Drop for LocalTerminalService {
    fn drop(&mut self) {
        let _ = self.kill_all();
    }
}

fn validate_scope(request: &CommandCreate) -> Result<(), TerminalServiceError> {
    let project = std::fs::canonicalize(&request.project_dir)
        .map_err(|error| TerminalServiceError::Io(format!("project directory: {error}")))?;
    let cwd = std::fs::canonicalize(&request.cwd)
        .map_err(|error| TerminalServiceError::Io(format!("command cwd: {error}")))?;
    if !cwd.starts_with(project) {
        return Err(TerminalServiceError::Io(
            "command cwd is outside its fixed project anchor".into(),
        ));
    }
    Ok(())
}

fn shell_command(request: &CommandCreate) -> Command {
    let mut command = Command::new(&request.shell);
    #[cfg(windows)]
    {
        let shell = request.shell.to_ascii_lowercase();
        if shell.contains("powershell") || shell.contains("pwsh") {
            command.arg("-NoProfile").arg("-Command");
        } else {
            command.arg("/D").arg("/S").arg("/C");
        }
    }
    #[cfg(not(windows))]
    command.arg("-lc");
    command.arg(&request.command);
    command
}

fn spawn_reader(
    state: Arc<FakeTerminalService>,
    id: CommandSessionId,
    stream: OutputStream,
    mut reader: impl Read + Send + 'static,
) -> Result<(), TerminalServiceError> {
    thread::Builder::new()
        .name(format!("termior-command-output-{}", id.0))
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if state
                            .push_output(&id, stream, buffer[..count].to_vec())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .map(|_| ())
        .map_err(|error| TerminalServiceError::Io(error.to_string()))
}

fn spawn_watcher(
    state: Arc<FakeTerminalService>,
    id: CommandSessionId,
    process: Arc<ProcessHandle>,
) -> Result<(), TerminalServiceError> {
    thread::Builder::new()
        .name(format!("termior-command-watch-{}", id.0))
        .spawn(move || loop {
            let status = process
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok().flatten());
            if let Some(status) = status {
                let _ = state.finish(&id, status.code().unwrap_or(-1));
                break;
            }
            thread::sleep(Duration::from_millis(10));
        })
        .map(|_| ())
        .map_err(|error| TerminalServiceError::Io(error.to_string()))
}

#[cfg(windows)]
fn terminate_process_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn terminate_process_tree(pid: u32) {
    let _ = Command::new("kill")
        .args(["-TERM", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn lock_error() -> TerminalServiceError {
    TerminalServiceError::Io("local terminal service lock poisoned".into())
}
