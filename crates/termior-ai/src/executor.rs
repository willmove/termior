//! Concrete, workspace-confined tool execution (FR-AGENT-07/09, FR-EDIT-04, FR-SEC).

use crate::context::TerminalContextProvider;
use crate::runtime::{
    CancellationToken, ChangeReviewExecution, RuntimeToolExecutor, ToolExecution,
};
use crate::task::ChangeSetId;
use crate::tools::{ToolError, ToolRegistry};
use futures::channel::mpsc::Receiver;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termior_diff::{apply_acceptances, diff_hunks, render_unified, Hunk};
use termior_explorer::{ContentSearch, FileIndex};
use termior_security::deny_list::Direction;
use termior_terminal::{PtyData, PtySessionConfig, ShellKind, TerminalBridge};
use wait_timeout::ChildExt;

const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditProposalSummary {
    pub id: String,
    pub path: String,
    pub baseline_digest: String,
    pub hunk_ids: Vec<usize>,
    pub patch: String,
}

#[derive(Clone)]
struct EditProposal {
    path: PathBuf,
    old: String,
    hunks: Vec<Hunk>,
}

struct PersistentShell {
    bridge: TerminalBridge,
    output: Receiver<PtyData>,
    kind: ShellKind,
}

/// Executes built-in tools after the Agent approval gate makes the policy decision.
pub struct ToolExecutor {
    root: PathBuf,
    registry: ToolRegistry,
    context: Option<Arc<dyn TerminalContextProvider>>,
    proposals: Mutex<HashMap<String, EditProposal>>,
    proposal_counter: AtomicU64,
    shell: Mutex<Option<PersistentShell>>,
    background: Mutex<HashMap<u32, Child>>,
}

impl ToolExecutor {
    pub fn new(root: impl AsRef<Path>, mut registry: ToolRegistry) -> Result<Self, ToolError> {
        let requested_root = root.as_ref();
        let was_authorized = registry
            .workspace_auth
            .is_authorized(&requested_root.to_string_lossy());
        let root = std::fs::canonicalize(requested_root)
            .map_err(|error| ToolError::Io(format!("workspace root: {error}")))?;
        // Windows canonicalization adds a `\\?\` device prefix. Preserve an authorization that
        // was granted for the user-facing path by registering its filesystem-canonical twin.
        if was_authorized {
            registry.workspace_auth.authorize(&root.to_string_lossy());
        }
        if !registry
            .workspace_auth
            .is_authorized(&root.to_string_lossy())
        {
            return Err(ToolError::NotAuthorized(root.display().to_string()));
        }
        Ok(Self {
            root,
            registry,
            context: None,
            proposals: Mutex::new(HashMap::new()),
            proposal_counter: AtomicU64::new(1),
            shell: Mutex::new(None),
            background: Mutex::new(HashMap::new()),
        })
    }

    pub fn with_terminal_context(mut self, context: Arc<dyn TerminalContextProvider>) -> Self {
        self.context = Some(context);
        self
    }

    /// Execute a read-only tool. Approval-level tools are rejected at this entry point.
    pub fn execute_auto(&self, tool: &str, arguments: &str) -> Result<String, ToolError> {
        if self.registry.requires_approval(tool)? {
            return Err(ToolError::ApprovalRequired(tool.to_owned()));
        }
        let arguments = self.registry.validate_and_normalize(tool, arguments)?;
        self.execute_inner(tool, &arguments)
    }

    /// Execute a tool after the UI has recorded explicit user approval.
    pub fn execute_approved(&self, tool: &str, arguments: &str) -> Result<String, ToolError> {
        self.registry.requires_approval(tool)?;
        let arguments = self.registry.validate_and_normalize(tool, arguments)?;
        self.execute_inner(tool, &arguments)
    }

    /// Materialize only the hunks accepted in the AI diff UI.
    pub fn accept_edit(
        &self,
        proposal_id: &str,
        accepted_hunks: &[usize],
    ) -> Result<String, ToolError> {
        let proposal = self
            .proposals
            .lock()
            .map_err(|_| ToolError::Io("edit proposal lock poisoned".into()))?
            .get(proposal_id)
            .cloned()
            .ok_or_else(|| ToolError::ProposalNotFound(proposal_id.to_owned()))?;
        self.check_path("write_file", &proposal.path, Direction::Write)?;
        let current = match std::fs::read_to_string(&proposal.path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_error(error)),
        };
        let expected = termior_diff::content_digest(&proposal.old);
        let actual = termior_diff::content_digest(&current);
        if expected != actual {
            return Err(ToolError::EditConflict(format!(
                "{} expected {expected}, found {actual}",
                proposal.path.display()
            )));
        }
        let result = apply_acceptances(&current, &proposal.hunks, accepted_hunks);
        termior_store::atomic_write(&proposal.path, &result)
            .map_err(|error| ToolError::Io(error.to_string()))?;
        self.proposals
            .lock()
            .map_err(|_| ToolError::Io("edit proposal lock poisoned".into()))?
            .remove(proposal_id);
        let accepted: std::collections::HashSet<usize> = accepted_hunks.iter().copied().collect();
        let rejected = proposal
            .hunks
            .iter()
            .filter(|hunk| !accepted.contains(&hunk.id))
            .count();
        Ok(format!(
            "applied={} rejected={} conflicted=0 path={}",
            accepted_hunks.len(),
            rejected,
            proposal.path.display()
        ))
    }

    pub fn reject_edit(&self, proposal_id: &str) -> bool {
        self.proposals
            .lock()
            .map(|mut proposals| proposals.remove(proposal_id).is_some())
            .unwrap_or(false)
    }

    fn execute_inner(&self, tool: &str, arguments: &str) -> Result<String, ToolError> {
        let args: Value = serde_json::from_str(arguments)
            .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
        match tool {
            "read_file" => self.read_file(&args),
            "list_directory" => self.list_directory(&args),
            "fs_search" => self.fs_search(&args),
            "fs_grep" => self.fs_grep(&args),
            "get_terminal_context" => self.terminal_context(),
            "write_file" => self.propose_write(&args),
            "create_directory" => self.create_directory(&args),
            "rename" => self.rename(&args),
            "delete" => self.delete(&args),
            "run_command" => self.run_command(&args),
            "shell_session_run" => self.shell_session_run(&args),
            "shell_bg_spawn" => self.shell_bg_spawn(&args),
            "run_subagent" => Err(ToolError::Io(
                "run_subagent is executed by the Agent orchestrator".into(),
            )),
            _ => Err(ToolError::Unknown(tool.to_owned())),
        }
    }

    fn read_file(&self, args: &Value) -> Result<String, ToolError> {
        let path = self.path_arg(args, "path", false)?;
        self.check_path("read_file", &path, Direction::Read)?;
        let metadata = std::fs::metadata(&path).map_err(io_error)?;
        if metadata.len() > MAX_READ_BYTES {
            return Err(ToolError::Io(format!(
                "{} exceeds the 2 MiB tool read limit",
                path.display()
            )));
        }
        std::fs::read_to_string(path).map_err(io_error)
    }

    fn list_directory(&self, args: &Value) -> Result<String, ToolError> {
        let path = self.path_arg(args, "path", false)?;
        self.check_path("list_directory", &path, Direction::Read)?;
        let mut entries = std::fs::read_dir(path)
            .map_err(io_error)?
            .map(|entry| {
                entry.map(|entry| {
                    let suffix = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        "/"
                    } else {
                        ""
                    };
                    format!("{}{}", entry.file_name().to_string_lossy(), suffix)
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_error)?;
        entries.sort_by_key(|entry| entry.to_ascii_lowercase());
        Ok(entries.join("\n"))
    }

    fn fs_search(&self, args: &Value) -> Result<String, ToolError> {
        let query = string_arg(args, "query")?;
        let index = FileIndex::build(&self.root, false)
            .map_err(|error| ToolError::Io(error.to_string()))?;
        Ok(index
            .fuzzy(query, 100)
            .into_iter()
            .map(|hit| hit.path)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn fs_grep(&self, args: &Value) -> Result<String, ToolError> {
        let query = string_arg(args, "query")?;
        let index = FileIndex::build(&self.root, false)
            .map_err(|error| ToolError::Io(error.to_string()))?;
        let paths = index
            .entries()
            .iter()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let mut lines = Vec::new();
        ContentSearch::default()
            .search_paths(query, paths, |hit| {
                if let Ok(relative) = hit.path.strip_prefix(&self.root) {
                    lines.push(format!(
                        "{}:{}:{}",
                        relative.to_string_lossy().replace('\\', "/"),
                        hit.line_number,
                        hit.line
                    ));
                }
                lines.len() < 200
            })
            .map_err(|error| ToolError::Io(error.to_string()))?;
        Ok(lines.join("\n"))
    }

    fn terminal_context(&self) -> Result<String, ToolError> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| ToolError::Io("no active terminal context".into()))?
            .snapshot();
        serde_json::to_string(&context).map_err(|error| ToolError::Io(error.to_string()))
    }

    fn propose_write(&self, args: &Value) -> Result<String, ToolError> {
        let path = self.path_arg(args, "path", true)?;
        self.check_path("write_file", &path, Direction::Write)?;
        let content = string_arg(args, "content")?.to_owned();
        let old = match std::fs::read_to_string(&path) {
            Ok(old) => old,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_error(error)),
        };
        let hunks = diff_hunks(&old, &content, 3);
        let id = format!(
            "edit-{}",
            self.proposal_counter.fetch_add(1, Ordering::Relaxed)
        );
        let summary = EditProposalSummary {
            id: id.clone(),
            path: path.display().to_string(),
            baseline_digest: termior_diff::content_digest(&old),
            hunk_ids: hunks.iter().map(|hunk| hunk.id).collect(),
            patch: render_unified(&old, &hunks),
        };
        self.proposals
            .lock()
            .map_err(|_| ToolError::Io("edit proposal lock poisoned".into()))?
            .insert(id, EditProposal { path, old, hunks });
        serde_json::to_string(&summary).map_err(|error| ToolError::Io(error.to_string()))
    }

    fn create_directory(&self, args: &Value) -> Result<String, ToolError> {
        let path = self.path_arg(args, "path", true)?;
        self.check_path("create_directory", &path, Direction::Write)?;
        std::fs::create_dir_all(&path).map_err(io_error)?;
        Ok(format!("created {}", path.display()))
    }

    fn rename(&self, args: &Value) -> Result<String, ToolError> {
        let source = self.path_arg(args, "source", false)?;
        let destination = self.path_arg(args, "destination", true)?;
        self.check_path("rename", &source, Direction::Write)?;
        self.check_path("rename", &destination, Direction::Write)?;
        std::fs::rename(&source, &destination).map_err(io_error)?;
        Ok(format!(
            "renamed {} to {}",
            source.display(),
            destination.display()
        ))
    }

    fn delete(&self, args: &Value) -> Result<String, ToolError> {
        let path = self.path_arg(args, "path", false)?;
        self.check_path("delete", &path, Direction::Write)?;
        if path == self.root {
            return Err(ToolError::DenyList {
                reason: "workspace root cannot be deleted".into(),
            });
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            std::fs::remove_dir_all(&path).map_err(io_error)?;
        } else {
            std::fs::remove_file(&path).map_err(io_error)?;
        }
        Ok(format!("deleted {}", path.display()))
    }

    fn run_command(&self, args: &Value) -> Result<String, ToolError> {
        let command = string_arg(args, "command")?;
        let cwd = self.optional_cwd(args)?;
        let output = run_shell_command(command, &cwd, COMMAND_TIMEOUT)?;
        Ok(format_output(output))
    }

    fn shell_session_run(&self, args: &Value) -> Result<String, ToolError> {
        let command = string_arg(args, "command")?;
        let mut guard = self
            .shell
            .lock()
            .map_err(|_| ToolError::Io("agent shell lock poisoned".into()))?;
        if guard.is_none() {
            let kind = termior_terminal::pty::default_shell();
            let mut bridge = TerminalBridge::spawn(&PtySessionConfig {
                cwd: Some(self.root.display().to_string()),
                workspace_auth: Some(self.registry.workspace_auth.clone()),
                ..PtySessionConfig::default()
            })
            .map_err(|error| ToolError::Io(error.to_string()))?;
            let output = bridge
                .take_output()
                .ok_or_else(|| ToolError::Io("agent shell output unavailable".into()))?;
            *guard = Some(PersistentShell {
                bridge,
                output,
                kind,
            });
        }
        let shell = guard.as_mut().expect("initialized above");
        let token = self.proposal_counter.fetch_add(1, Ordering::Relaxed);
        let marker = format!("__TERMIOR_DONE_{token}__");
        let script = persistent_script(shell.kind, command, &marker);
        shell
            .bridge
            .writer()
            .write_all(script.as_bytes())
            .map_err(io_error)?;
        collect_until_marker(&mut shell.output, &marker, COMMAND_TIMEOUT)
    }

    fn shell_bg_spawn(&self, args: &Value) -> Result<String, ToolError> {
        let command = string_arg(args, "command")?;
        let cwd = self.optional_cwd(args)?;
        let child = shell_command(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(io_error)?;
        let pid = child.id();
        self.background
            .lock()
            .map_err(|_| ToolError::Io("background process lock poisoned".into()))?
            .insert(pid, child);
        Ok(format!("spawned background process {pid}"))
    }

    fn optional_cwd(&self, args: &Value) -> Result<PathBuf, ToolError> {
        match args.get("cwd").and_then(Value::as_str) {
            Some(path) => {
                let path = self.resolve_path(path, false)?;
                self.check_path("run_command", &path, Direction::Read)?;
                Ok(path)
            }
            None => Ok(self.root.clone()),
        }
    }

    fn path_arg(
        &self,
        args: &Value,
        name: &str,
        may_not_exist: bool,
    ) -> Result<PathBuf, ToolError> {
        self.resolve_path(string_arg(args, name)?, may_not_exist)
    }

    fn resolve_path(&self, raw: &str, may_not_exist: bool) -> Result<PathBuf, ToolError> {
        let candidate = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.root.join(raw)
        };
        if !may_not_exist || candidate.exists() {
            return std::fs::canonicalize(candidate).map_err(io_error);
        }
        let parent = candidate
            .parent()
            .ok_or_else(|| ToolError::InvalidArguments(format!("path has no parent: {raw}")))?;
        let parent = std::fs::canonicalize(parent).map_err(io_error)?;
        Ok(parent.join(
            candidate
                .file_name()
                .ok_or_else(|| ToolError::InvalidArguments(format!("path has no name: {raw}")))?,
        ))
    }

    fn check_path(&self, tool: &str, path: &Path, direction: Direction) -> Result<(), ToolError> {
        self.registry
            .check_path_access(tool, &path.to_string_lossy(), direction)
    }
}

impl RuntimeToolExecutor for ToolExecutor {
    fn execute(
        &self,
        _call_id: &str,
        tool: &str,
        normalized_arguments: &str,
        cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        if cancellation.is_cancelled() {
            return Err("tool call cancelled before execution".into());
        }
        let output = self
            .execute_inner(tool, normalized_arguments)
            .map_err(|error| error.to_string())?;
        if tool == "write_file" {
            let summary: EditProposalSummary =
                serde_json::from_str(&output).map_err(|error| error.to_string())?;
            Ok(ToolExecution::ChangeProposed {
                change_set_id: ChangeSetId(summary.id),
                summary: output,
            })
        } else {
            Ok(ToolExecution::Completed(output))
        }
    }

    fn review_change(
        &self,
        change_set_id: &ChangeSetId,
        accepted_hunks: &[usize],
        cancellation: &CancellationToken,
    ) -> Result<ChangeReviewExecution, String> {
        if cancellation.is_cancelled() {
            return Ok(ChangeReviewExecution::Unknown(
                "change review cancelled before the disk result was confirmed".into(),
            ));
        }
        match self.accept_edit(&change_set_id.0, accepted_hunks) {
            Ok(output) => Ok(ChangeReviewExecution::Applied(output)),
            Err(ToolError::EditConflict(conflict)) => {
                Ok(ChangeReviewExecution::Conflict(conflict))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn cancel_active(&self) -> bool {
        let mut confirmed = true;
        if let Ok(mut processes) = self.background.lock() {
            for child in processes.values_mut() {
                if child.kill().is_err() || child.wait().is_err() {
                    confirmed = false;
                }
            }
            processes.clear();
        } else {
            confirmed = false;
        }
        if let Ok(mut shell) = self.shell.lock() {
            shell.take();
        } else {
            confirmed = false;
        }
        confirmed
    }
}

impl Drop for ToolExecutor {
    fn drop(&mut self) {
        if let Ok(processes) = self.background.get_mut() {
            for child in processes.values_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments(format!("missing string field `{name}`")))
}

fn io_error(error: std::io::Error) -> ToolError {
    ToolError::Io(error.to_string())
}

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut process = Command::new("cmd");
        process.args(["/D", "/S", "/C", command]);
        process
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut process = Command::new(shell);
        process.args(["-lc", command]);
        process
    }
}

fn run_shell_command(command: &str, cwd: &Path, timeout: Duration) -> Result<Output, ToolError> {
    let mut child = shell_command(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_error)?;
    if child.wait_timeout(timeout).map_err(io_error)?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ToolError::Timeout(command.to_owned()));
    }
    child.wait_with_output().map_err(io_error)
}

fn format_output(output: Output) -> String {
    format!(
        "exit={}\nstdout:\n{}\nstderr:\n{}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).trim_end(),
        String::from_utf8_lossy(&output.stderr).trim_end()
    )
}

fn persistent_script(kind: ShellKind, command: &str, marker: &str) -> String {
    match kind {
        ShellKind::Pwsh | ShellKind::PowerShell => {
            format!("{command}\r\nWrite-Output \"{marker}:$LASTEXITCODE\"\r\n")
        }
        ShellKind::Cmd => format!("{command}\r\necho {marker}:%errorlevel%\r\n"),
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Fish => {
            format!("{command}\nprintf '\\n{marker}:%s\\n' $?\n")
        }
    }
}

fn collect_until_marker(
    output: &mut Receiver<PtyData>,
    marker: &str,
    timeout: Duration,
) -> Result<String, ToolError> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        match output.try_recv() {
            Ok(data) => {
                bytes.extend(data.bytes);
                let text = String::from_utf8_lossy(&bytes);
                if let Some(position) = text.find(marker) {
                    return Ok(text[..position].trim().to_owned());
                }
            }
            Err(_) if Instant::now() >= deadline => {
                return Err(ToolError::Timeout("persistent shell command".into()))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_security::workspace::WorkspaceAuthRegistry;

    fn executor(root: &Path) -> ToolExecutor {
        let registry = ToolRegistry::new(WorkspaceAuthRegistry::with_roots([root
            .display()
            .to_string()]));
        ToolExecutor::new(root, registry).unwrap()
    }

    #[test]
    fn auto_read_and_search_are_confined() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "needle\n").unwrap();
        let executor = executor(dir.path());
        assert_eq!(
            executor
                .execute_auto("read_file", r#"{"path":"hello.txt"}"#)
                .unwrap(),
            "needle\n"
        );
        assert!(executor
            .execute_auto("fs_search", r#"{"query":"hello"}"#)
            .unwrap()
            .contains("hello.txt"));
        assert!(executor
            .execute_auto("fs_grep", r#"{"query":"needle"}"#)
            .unwrap()
            .contains("hello.txt:1:needle"));
    }

    #[test]
    fn approval_tool_cannot_use_auto_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let executor = executor(dir.path());
        assert!(matches!(
            executor.execute_auto("write_file", r#"{"path":"a","content":"x"}"#),
            Err(ToolError::ApprovalRequired(_))
        ));
    }

    #[test]
    fn write_is_diff_until_hunks_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old\n").unwrap();
        let executor = executor(dir.path());
        let summary: EditProposalSummary = serde_json::from_str(
            &executor
                .execute_approved("write_file", r#"{"path":"a.txt","content":"new\n"}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
        executor
            .accept_edit(&summary.id, &summary.hunk_ids)
            .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "new\n");
    }

    #[test]
    fn edit_review_detects_a_changed_disk_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old\n").unwrap();
        let executor = executor(dir.path());
        let summary: EditProposalSummary = serde_json::from_str(
            &executor
                .execute_approved("write_file", r#"{"path":"a.txt","content":"new\n"}"#)
                .unwrap(),
        )
        .unwrap();
        std::fs::write(&path, "user edit\n").unwrap();

        let error = executor
            .accept_edit(&summary.id, &summary.hunk_ids)
            .unwrap_err();
        assert!(matches!(error, ToolError::EditConflict(_)));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "user edit\n");
    }

    #[test]
    fn outside_and_secret_paths_remain_denied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=x").unwrap();
        let executor = executor(dir.path());
        assert!(matches!(
            executor.execute_auto("read_file", r#"{"path":".env"}"#),
            Err(ToolError::DenyList { .. })
        ));
        assert!(executor
            .execute_auto("read_file", r#"{"path":"../outside"}"#)
            .is_err());
    }

    #[test]
    fn approved_command_runs_in_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let executor = executor(dir.path());
        let command = if cfg!(windows) { "cd" } else { "pwd" };
        let output = executor
            .execute_approved("run_command", &format!(r#"{{"command":"{command}"}}"#))
            .unwrap();
        assert!(output.contains("exit=0"));
    }
}
