use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use termior_security::deny_list::{check_path, Direction};
use termior_terminal::{
    CommandCreate, CommandOwner, CommandSession, CommandSessionId, Controller,
    LocalTerminalService, OutputCursor, TerminalService,
};

use crate::{AcpClientHandler, BackendError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcpHostPolicy {
    pub allow_file_writes: bool,
    pub allow_terminal: bool,
}

/// Workspace-confined ACP client callbacks backed by Termior's command-session service.
pub struct TermiorAcpClientHandler {
    root: PathBuf,
    task_id: String,
    environment_id: String,
    policy: AcpHostPolicy,
    terminal: Arc<LocalTerminalService>,
}

impl TermiorAcpClientHandler {
    pub fn new(
        root: impl AsRef<Path>,
        task_id: impl Into<String>,
        environment_id: impl Into<String>,
        policy: AcpHostPolicy,
    ) -> Result<Self, BackendError> {
        Ok(Self {
            root: std::fs::canonicalize(root)
                .map_err(|error| BackendError::Transport(error.to_string()))?,
            task_id: task_id.into(),
            environment_id: environment_id.into(),
            policy,
            terminal: termior_terminal::shared_terminal_service(),
        })
    }

    fn path(&self, value: &Value, direction: Direction) -> Result<PathBuf, BackendError> {
        let raw = value
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| BackendError::Protocol("ACP filesystem callback has no path".into()))?;
        let relative = Path::new(raw);
        if relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(BackendError::Protocol(
                "ACP filesystem callback escapes the task workspace".into(),
            ));
        }
        let candidate = if relative.is_absolute() {
            relative.to_path_buf()
        } else {
            self.root.join(relative)
        };
        let resolved = if candidate.exists() {
            std::fs::canonicalize(&candidate)
        } else {
            let parent = candidate.parent().unwrap_or(&self.root);
            std::fs::canonicalize(parent)
                .map(|parent| parent.join(candidate.file_name().unwrap_or_default()))
        }
        .map_err(|error| BackendError::Transport(error.to_string()))?;
        if !resolved.starts_with(&self.root)
            || check_path(&resolved.to_string_lossy(), direction).is_some()
        {
            return Err(BackendError::Protocol(
                "ACP filesystem callback was denied by workspace policy".into(),
            ));
        }
        Ok(resolved)
    }

    fn session(&self, params: &Value) -> Result<CommandSession, BackendError> {
        let id = CommandSessionId(
            params
                .get("sessionId")
                .or_else(|| params.get("terminalId"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    BackendError::Protocol("ACP terminal callback has no sessionId".into())
                })?
                .into(),
        );
        let session = self
            .terminal
            .session(&id)
            .map_err(|error| BackendError::Protocol(error.to_string()))?;
        if session.owner != CommandOwner::Task(self.task_id.clone())
            || std::fs::canonicalize(&session.project_dir).ok().as_ref() != Some(&self.root)
        {
            return Err(BackendError::Protocol(
                "ACP terminal callback references another task or workspace".into(),
            ));
        }
        Ok(session)
    }

    fn create_terminal(&self, params: &Value) -> Result<Value, BackendError> {
        if !self.policy.allow_terminal {
            return Err(BackendError::Unsupported("ACP host terminal"));
        }
        let cwd = match params.get("cwd").and_then(Value::as_str) {
            Some(cwd) if !cwd.is_empty() => self.path(&json!({"path":cwd}), Direction::Read)?,
            _ => self.root.clone(),
        };
        let command = command_text(params)?;
        let id = self
            .terminal
            .create(CommandCreate {
                owner: CommandOwner::Task(self.task_id.clone()),
                project_dir: self.root.display().to_string(),
                environment_id: self.environment_id.clone(),
                cwd: cwd.display().to_string(),
                shell: if cfg!(windows) { "cmd" } else { "/bin/sh" }.into(),
                command,
                interactive: false,
                rows: 24,
                cols: 80,
            })
            .map_err(|error| BackendError::Transport(error.to_string()))?;
        Ok(json!({"sessionId":id.0}))
    }
}

impl AcpClientHandler for TermiorAcpClientHandler {
    fn filesystem_read_enabled(&self) -> bool {
        true
    }

    fn filesystem_write_enabled(&self) -> bool {
        self.policy.allow_file_writes
    }

    fn terminal_enabled(&self) -> bool {
        self.policy.allow_terminal
    }

    fn handle(&self, method: &str, params: &Value) -> Result<Value, BackendError> {
        match method {
            "fs/read_text_file" => {
                let path = self.path(params, Direction::Read)?;
                let bytes = std::fs::read(&path)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                if bytes.len() > 2 * 1024 * 1024 {
                    return Err(BackendError::Protocol(
                        "ACP file read exceeds the 2 MiB host limit".into(),
                    ));
                }
                let content = String::from_utf8(bytes)
                    .map_err(|_| BackendError::Protocol("ACP file is not UTF-8".into()))?;
                Ok(json!({"content":content}))
            }
            "fs/write_text_file" => {
                if !self.policy.allow_file_writes {
                    return Err(BackendError::Unsupported("ACP host file write"));
                }
                let path = self.path(params, Direction::Write)?;
                let content = params
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        BackendError::Protocol("ACP file write has no content".into())
                    })?;
                termior_store::atomic_write(&path, content)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                Ok(json!({}))
            }
            "terminal/create" => self.create_terminal(params),
            "terminal/output" => {
                let session = self.session(params)?;
                let after = params
                    .get("after")
                    .and_then(Value::as_u64)
                    .map(|sequence| OutputCursor { sequence });
                let output = self
                    .terminal
                    .read_output(&session.id, after, 1024 * 1024)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                Ok(json!({
                    "output":String::from_utf8_lossy(&output.bytes()),
                    "next":output.next.sequence,
                    "truncatedBefore":output.truncated_before.map(|cursor| cursor.sequence),
                    "exitCode":output.exit_code,
                    "state":format!("{:?}", output.state)
                }))
            }
            "terminal/wait_for_exit" => {
                let session = self.session(params)?;
                let timeout = params
                    .get("timeoutMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .min(30_000);
                let result = self
                    .terminal
                    .wait(&session.id, Duration::from_millis(timeout))
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                serde_json::to_value(result)
                    .map_err(|error| BackendError::Protocol(error.to_string()))
            }
            "terminal/write_stdin" => {
                let session = self.session(params)?;
                let data = params.get("data").and_then(Value::as_str).ok_or_else(|| {
                    BackendError::Protocol("ACP terminal input has no data".into())
                })?;
                self.terminal
                    .write_stdin(
                        &session.id,
                        &Controller::Task(self.task_id.clone()),
                        data.as_bytes(),
                    )
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                Ok(json!({}))
            }
            "terminal/kill" => {
                let session = self.session(params)?;
                self.terminal
                    .kill(&session.id)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                Ok(json!({}))
            }
            "terminal/release" => {
                let session = self.session(params)?;
                self.terminal
                    .release(&session.id, false)
                    .map_err(|error| BackendError::Transport(error.to_string()))?;
                Ok(json!({}))
            }
            _ => Err(BackendError::Unsupported("ACP client callback")),
        }
    }
}

fn command_text(params: &Value) -> Result<String, BackendError> {
    if let Some(Value::String(command)) = params.get("command") {
        let Some(args) = params.get("args").and_then(Value::as_array) else {
            return Ok(command.clone());
        };
        let mut command = quote_shell_argument(command);
        for argument in args {
            let argument = argument.as_str().ok_or_else(|| {
                BackendError::Protocol("ACP command contains a non-string argument".into())
            })?;
            command.push(' ');
            command.push_str(&quote_shell_argument(argument));
        }
        return Ok(command);
    }
    let parts = match params.get("command") {
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| {
                part.as_str().map(str::to_owned).ok_or_else(|| {
                    BackendError::Protocol("ACP command contains a non-string".into())
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(BackendError::Protocol(
                "ACP terminal create has no command".into(),
            ))
        }
    };
    Ok(parts
        .iter()
        .map(|part| quote_shell_argument(part))
        .collect::<Vec<_>>()
        .join(" "))
}

fn quote_shell_argument(value: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
