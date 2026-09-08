use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use crate::BackendError;

#[derive(Debug, Default)]
pub struct JsonRpcLineCodec {
    pending: Vec<u8>,
}

impl JsonRpcLineCodec {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, serde_json::Error> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=end).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            messages.push(serde_json::from_slice(&line)?);
        }
        Ok(messages)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl TransportCommand {
    pub fn codex(executable: impl Into<String>) -> Self {
        Self {
            program: executable.into(),
            args: vec!["app-server".into(), "--stdio".into()],
        }
    }
}

#[derive(Debug)]
pub enum TransportMessage {
    Json(Value),
    ProtocolError(String),
    Eof,
}

pub struct StdioTransport {
    child: Child,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    incoming: Receiver<TransportMessage>,
    next_id: AtomicU64,
    pending_responses: HashMap<u64, Value>,
    events: VecDeque<Value>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl StdioTransport {
    pub fn spawn(spec: &TransportCommand) -> Result<Self, BackendError> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| BackendError::Transport("backend stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError::Transport("backend stdout unavailable".into()))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| BackendError::Transport("backend stderr unavailable".into()))?;
        let (tx, incoming) = mpsc::channel();
        thread::Builder::new()
            .name("termior-agent-host-stdout".into())
            .spawn(move || read_stdout(stdout, tx))
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_target = stderr.clone();
        thread::Builder::new()
            .name("termior-agent-host-stderr".into())
            .spawn(move || read_stderr(stderr_pipe, stderr_target))
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        Ok(Self {
            child,
            stdin: Arc::new(Mutex::new(Some(stdin))),
            incoming,
            next_id: AtomicU64::new(1),
            pending_responses: HashMap::new(),
            events: VecDeque::new(),
            stderr,
        })
    }

    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        if let Some(response) = self.pending_responses.remove(&id) {
            return decode_response(response);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BackendError::Timeout(method.into()));
            }
            match self.incoming.recv_timeout(remaining) {
                Ok(TransportMessage::Json(value)) => {
                    if let Some(response_id) = value.get("id").and_then(Value::as_u64) {
                        if value.get("method").is_none() {
                            if response_id == id {
                                return decode_response(value);
                            }
                            self.pending_responses.insert(response_id, value);
                            continue;
                        }
                    }
                    self.events.push_back(value);
                }
                Ok(TransportMessage::ProtocolError(error)) => {
                    return Err(BackendError::Protocol(error))
                }
                Ok(TransportMessage::Eof) => return Err(BackendError::Exited(self.stderr_tail())),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(BackendError::Timeout(method.into()))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(BackendError::Exited(self.stderr_tail()))
                }
            }
        }
    }

    /// Send a request without waiting for its response. Duplex protocols such as ACP use this for
    /// long prompts so client callbacks can be serviced while the agent is still working.
    pub fn begin_request(&self, method: &str, params: Value) -> Result<u64, BackendError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        Ok(id)
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), BackendError> {
        self.send(&json!({"jsonrpc":"2.0", "method":method, "params":params}))
    }

    pub fn respond(&self, id: Value, result: Value) -> Result<(), BackendError> {
        self.send(&json!({"jsonrpc":"2.0", "id":id, "result":result}))
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), BackendError> {
        self.send(&json!({
            "jsonrpc":"2.0",
            "id":id,
            "error":{"code":code,"message":message}
        }))
    }

    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Value>, BackendError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        match self.incoming.recv_timeout(timeout) {
            Ok(TransportMessage::Json(value)) => Ok(Some(value)),
            Ok(TransportMessage::ProtocolError(error)) => Err(BackendError::Protocol(error)),
            Ok(TransportMessage::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(BackendError::Exited(self.stderr_tail()))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        }
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_else(|_| "stderr buffer unavailable".into())
    }

    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), BackendError> {
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        if self
            .child
            .wait_timeout(timeout)
            .map_err(|error| BackendError::Transport(error.to_string()))?
            .is_none()
        {
            self.child
                .kill()
                .map_err(|error| BackendError::Transport(error.to_string()))?;
            self.child
                .wait()
                .map_err(|error| BackendError::Transport(error.to_string()))?;
        }
        Ok(())
    }

    fn send(&self, value: &Value) -> Result<(), BackendError> {
        let mut guard = self
            .stdin
            .lock()
            .map_err(|_| BackendError::Transport("backend stdin lock poisoned".into()))?;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| BackendError::Exited("backend stdin closed".into()))?;
        serde_json::to_writer(&mut *stdin, value)
            .map_err(|error| BackendError::Protocol(error.to_string()))?;
        stdin
            .write_all(b"\n")
            .and_then(|()| stdin.flush())
            .map_err(|error| BackendError::Transport(error.to_string()))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.shutdown(Duration::from_millis(250));
    }
}

fn read_stdout(stdout: impl Read, tx: mpsc::Sender<TransportMessage>) {
    let mut codec = JsonRpcLineCodec::default();
    let mut reader = BufReader::new(stdout);
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => match codec.push(&buffer[..count]) {
                Ok(messages) => {
                    for message in messages {
                        if tx.send(TransportMessage::Json(message)).is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(TransportMessage::ProtocolError(error.to_string()));
                    return;
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = tx.send(TransportMessage::ProtocolError(error.to_string()));
                return;
            }
        }
    }
    let _ = tx.send(TransportMessage::Eof);
}

fn read_stderr(stderr: impl Read, target: Arc<Mutex<VecDeque<String>>>) {
    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
        let line = termior_store::StreamingRedactor::new([]).redact(&line).text;
        if let Ok(mut lines) = target.lock() {
            lines.push_back(line);
            while lines.len() > 100 {
                lines.pop_front();
            }
        }
    }
}

fn decode_response(value: Value) -> Result<Value, BackendError> {
    if let Some(error) = value.get("error") {
        return Err(BackendError::Protocol(error.to_string()));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| BackendError::Protocol("JSON-RPC response has no result".into()))
}
