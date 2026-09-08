//! Concrete, workspace-confined tool execution (FR-AGENT-07/09, FR-EDIT-04, FR-SEC).

use crate::context::TerminalContextProvider;
use crate::runtime::{
    CancellationToken, ChangeReviewExecution, RuntimeToolExecutor, ToolExecution,
};
use crate::task::ChangeSetId;
use crate::tools::{ToolError, ToolRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termior_diff::{apply_acceptances, diff_hunks, render_unified, Hunk};
use termior_explorer::{ContentSearch, FileIndex};
use termior_security::deny_list::Direction;
use termior_terminal::{
    CommandCreate, CommandOwner, CommandSessionId, Controller, LocalTerminalService, OutputCursor,
    ShellKind, TerminalService,
};

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

pub trait ExternalToolHandler: Send + Sync {
    fn call(&self, arguments: Value) -> Result<String, String>;
}

#[derive(Clone)]
struct ProposedFile {
    path: PathBuf,
    old: String,
    hunks: Vec<Hunk>,
}

#[derive(Clone)]
struct EditProposal {
    files: Vec<ProposedFile>,
}

struct PersistentShell {
    session_id: CommandSessionId,
    cursor: OutputCursor,
    kind: ShellKind,
}

/// Executes built-in tools after the Agent approval gate makes the policy decision.
pub struct ToolExecutor {
    root: PathBuf,
    environment_id: String,
    registry: ToolRegistry,
    context: Option<Arc<dyn TerminalContextProvider>>,
    proposals: Mutex<HashMap<String, EditProposal>>,
    proposal_counter: AtomicU64,
    shell: Mutex<Option<PersistentShell>>,
    terminal: Arc<LocalTerminalService>,
    owner_task: Mutex<String>,
    hooks: Option<Arc<termior_hooks::HookPipeline>>,
    checkpoints: Option<Arc<termior_store::CheckpointStore>>,
    checkpoint_sequence: AtomicU64,
    external_handlers: HashMap<String, Arc<dyn ExternalToolHandler>>,
}

impl ToolExecutor {
    pub fn new(root: impl AsRef<Path>, registry: ToolRegistry) -> Result<Self, ToolError> {
        Self::new_in_environment(root, registry, "direct")
    }

    pub fn new_in_environment(
        root: impl AsRef<Path>,
        mut registry: ToolRegistry,
        environment_id: impl Into<String>,
    ) -> Result<Self, ToolError> {
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
            environment_id: environment_id.into(),
            registry,
            context: None,
            proposals: Mutex::new(HashMap::new()),
            proposal_counter: AtomicU64::new(1),
            shell: Mutex::new(None),
            terminal: termior_terminal::shared_terminal_service(),
            owner_task: Mutex::new("termior-agent".into()),
            hooks: None,
            checkpoints: None,
            checkpoint_sequence: AtomicU64::new(1),
            external_handlers: HashMap::new(),
        })
    }

    pub fn with_terminal_context(mut self, context: Arc<dyn TerminalContextProvider>) -> Self {
        self.context = Some(context);
        self
    }

    pub fn with_hook_pipeline(mut self, hooks: Arc<termior_hooks::HookPipeline>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    pub fn with_checkpoint_store(mut self, root: impl Into<PathBuf>) -> Self {
        self.checkpoints = Some(Arc::new(termior_store::CheckpointStore::new(root)));
        self
    }

    pub fn with_external_handler(
        mut self,
        qualified_tool_name: impl Into<String>,
        handler: Arc<dyn ExternalToolHandler>,
    ) -> Result<Self, ToolError> {
        let name = qualified_tool_name.into();
        let contract = self.registry.contract(&name)?;
        if contract.side_effect != crate::SideEffectClass::External {
            return Err(ToolError::InvalidArguments(format!(
                "{name} is not an external tool contract"
            )));
        }
        self.external_handlers.insert(name, handler);
        Ok(self)
    }

    pub fn set_task_owner(&self, task_id: impl Into<String>) {
        if let Ok(mut owner) = self.owner_task.lock() {
            *owner = task_id.into();
        }
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
        let mut prepared = Vec::with_capacity(proposal.files.len());
        for file in &proposal.files {
            self.check_path("write_file", &file.path, Direction::Write)?;
            let current = match std::fs::read_to_string(&file.path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(io_error(error)),
            };
            let expected = termior_diff::content_digest(&file.old);
            let actual = termior_diff::content_digest(&current);
            if expected != actual {
                return Err(ToolError::EditConflict(format!(
                    "{} expected {expected}, found {actual}",
                    file.path.display()
                )));
            }
            let result = apply_acceptances(&current, &file.hunks, accepted_hunks);
            prepared.push((file.path.clone(), result));
        }
        let checkpoint_id = if let Some(checkpoints) = self.checkpoints.as_ref() {
            let expected_after = prepared
                .iter()
                .map(|(path, result)| {
                    let relative = path.strip_prefix(&self.root).map_err(|_| {
                        ToolError::Io(format!(
                            "checkpoint path escaped workspace: {}",
                            path.display()
                        ))
                    })?;
                    Ok((relative.to_path_buf(), Some(result.as_bytes().to_vec())))
                })
                .collect::<Result<Vec<_>, ToolError>>()?;
            let sequence = self.checkpoint_sequence.fetch_add(1, Ordering::Relaxed);
            let manifest = checkpoints
                .create(
                    &self.root,
                    &self.task_owner(),
                    sequence,
                    proposal_id,
                    proposal_id,
                    &expected_after,
                )
                .map_err(|error| ToolError::Io(format!("checkpoint failed: {error}")))?;
            Some(manifest.id)
        } else {
            None
        };
        for (path, result) in prepared {
            termior_store::atomic_write(&path, &result)
                .map_err(|error| ToolError::Io(error.to_string()))?;
        }
        self.proposals
            .lock()
            .map_err(|_| ToolError::Io("edit proposal lock poisoned".into()))?
            .remove(proposal_id);
        let accepted: std::collections::HashSet<usize> = accepted_hunks.iter().copied().collect();
        let rejected = proposal
            .files
            .iter()
            .flat_map(|file| file.hunks.iter())
            .filter(|hunk| !accepted.contains(&hunk.id))
            .count();
        Ok(format!(
            "applied={} rejected={} conflicted=0 files={} checkpoint={}",
            accepted_hunks.len(),
            rejected,
            proposal.files.len(),
            checkpoint_id.as_deref().unwrap_or("disabled")
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
            "command_status" => self.command_status(&args),
            "command_read_output" => self.command_read_output(&args),
            "command_wait" => self.command_wait(&args),
            "command_kill" => self.command_kill(&args),
            "command_claim" => self.command_claim(&args),
            "command_write_input" => self.command_write_input(&args),
            "run_subagent" => Err(ToolError::Io(
                "run_subagent is executed by the Agent orchestrator".into(),
            )),
            _ => self
                .external_handlers
                .get(tool)
                .ok_or_else(|| ToolError::Unknown(tool.to_owned()))?
                .call(args)
                .map_err(ToolError::Io),
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
        self.propose_write_batch(std::slice::from_ref(args))
    }

    fn propose_write_batch(&self, arguments: &[Value]) -> Result<String, ToolError> {
        let id = format!(
            "edit-{}",
            self.proposal_counter.fetch_add(1, Ordering::Relaxed)
        );
        let mut files = Vec::with_capacity(arguments.len());
        let mut hunk_ids = Vec::new();
        let mut patch = String::new();
        let mut next_hunk_id = 0usize;
        for args in arguments {
            let path = self.path_arg(args, "path", true)?;
            self.check_path("write_file", &path, Direction::Write)?;
            let content = string_arg(args, "content")?.to_owned();
            let old = match std::fs::read_to_string(&path) {
                Ok(old) => old,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(io_error(error)),
            };
            let mut hunks = diff_hunks(&old, &content, 3);
            for hunk in &mut hunks {
                hunk.id = next_hunk_id;
                hunk_ids.push(next_hunk_id);
                next_hunk_id += 1;
            }
            if !patch.is_empty() {
                patch.push('\n');
            }
            patch.push_str(&format!("--- {}\n+++ {}\n", path.display(), path.display()));
            patch.push_str(&render_unified(&old, &hunks));
            files.push(ProposedFile { path, old, hunks });
        }
        let path = if files.len() == 1 {
            files[0].path.display().to_string()
        } else {
            format!("{} files", files.len())
        };
        let baseline_digest = files
            .iter()
            .map(|file| termior_diff::content_digest(&file.old))
            .collect::<Vec<_>>()
            .join(",");
        let summary = EditProposalSummary {
            id: id.clone(),
            path,
            baseline_digest,
            hunk_ids,
            patch,
        };
        self.proposals
            .lock()
            .map_err(|_| ToolError::Io("edit proposal lock poisoned".into()))?
            .insert(id, EditProposal { files });
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
        let id = self.start_command(command, &cwd, false)?;
        let waited = self
            .terminal
            .wait(&id, COMMAND_TIMEOUT)
            .map_err(terminal_error)?;
        let output = self
            .terminal
            .read_output(&id, None, 1024 * 1024)
            .map_err(terminal_error)?;
        Ok(format!(
            "session_id={} state={:?} exit={} timed_out={} cursor={} truncated_before={}\n{}",
            id.0,
            waited.state,
            waited
                .exit_code
                .map_or_else(|| "unknown".into(), |code| code.to_string()),
            waited.timed_out,
            output.next.sequence,
            output
                .truncated_before
                .map_or_else(|| "none".into(), |cursor| cursor.sequence.to_string()),
            String::from_utf8_lossy(&output.bytes()).trim_end()
        ))
    }

    fn shell_session_run(&self, args: &Value) -> Result<String, ToolError> {
        let command = string_arg(args, "command")?;
        let mut guard = self
            .shell
            .lock()
            .map_err(|_| ToolError::Io("agent shell lock poisoned".into()))?;
        if guard.is_none() {
            let kind = termior_terminal::pty::default_shell();
            let session_id = self
                .terminal
                .create(CommandCreate {
                    owner: CommandOwner::Task(self.task_owner()),
                    project_dir: self.root.display().to_string(),
                    environment_id: self.environment_id.clone(),
                    cwd: self.root.display().to_string(),
                    shell: shell_program(kind).into(),
                    command: String::new(),
                    interactive: true,
                    rows: 24,
                    cols: 80,
                })
                .map_err(terminal_error)?;
            *guard = Some(PersistentShell {
                session_id,
                cursor: OutputCursor::START,
                kind,
            });
        }
        let shell = guard.as_mut().expect("initialized above");
        let token = self.proposal_counter.fetch_add(1, Ordering::Relaxed);
        let marker = format!("__TERMIOR_DONE_{token}__");
        let script = persistent_script(shell.kind, command, &marker);
        self.terminal
            .write_stdin(
                &shell.session_id,
                &Controller::Task(self.task_owner()),
                script.as_bytes(),
            )
            .map_err(terminal_error)?;
        collect_until_marker(
            self.terminal.as_ref(),
            &shell.session_id,
            &mut shell.cursor,
            &marker,
            COMMAND_TIMEOUT,
        )
    }

    fn shell_bg_spawn(&self, args: &Value) -> Result<String, ToolError> {
        let command = string_arg(args, "command")?;
        let cwd = self.optional_cwd(args)?;
        let id = self.start_command(command, &cwd, false)?;
        Ok(format!("session_id={} state=running", id.0))
    }

    fn start_command(
        &self,
        command: &str,
        cwd: &Path,
        interactive: bool,
    ) -> Result<CommandSessionId, ToolError> {
        self.terminal
            .create(CommandCreate {
                owner: CommandOwner::Task(self.task_owner()),
                project_dir: self.root.display().to_string(),
                environment_id: self.environment_id.clone(),
                cwd: cwd.display().to_string(),
                shell: if cfg!(windows) {
                    "cmd".into()
                } else {
                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
                },
                command: command.into(),
                interactive,
                rows: 24,
                cols: 80,
            })
            .map_err(terminal_error)
    }

    fn task_owner(&self) -> String {
        self.owner_task
            .lock()
            .map(|owner| owner.clone())
            .unwrap_or_else(|_| "termior-agent".into())
    }

    fn command_status(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        serde_json::to_string(&self.terminal.session(&id).map_err(terminal_error)?)
            .map_err(|error| ToolError::Io(error.to_string()))
    }

    fn command_read_output(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        let after = args
            .get("after")
            .and_then(Value::as_u64)
            .map(|sequence| OutputCursor { sequence });
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(256 * 1024) as usize;
        let output = self
            .terminal
            .read_output(&id, after, max_bytes)
            .map_err(terminal_error)?;
        Ok(format!(
            "state={:?} exit={} cursor={} truncated_before={}\n{}",
            output.state,
            output
                .exit_code
                .map_or_else(|| "unknown".into(), |code| code.to_string()),
            output.next.sequence,
            output
                .truncated_before
                .map_or_else(|| "none".into(), |cursor| cursor.sequence.to_string()),
            String::from_utf8_lossy(&output.bytes())
        ))
    }

    fn command_wait(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(30_000);
        serde_json::to_string(
            &self
                .terminal
                .wait(&id, Duration::from_millis(timeout_ms))
                .map_err(terminal_error)?,
        )
        .map_err(|error| ToolError::Io(error.to_string()))
    }

    fn command_kill(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        self.terminal.kill(&id).map_err(terminal_error)?;
        Ok(format!("session_id={} state=terminated", id.0))
    }

    fn command_claim(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        let cursor = self
            .terminal
            .hand_back(&id, Controller::Task(self.task_owner()))
            .map_err(terminal_error)?;
        Ok(format!(
            "session_id={} controller=task cursor={}",
            id.0, cursor.sequence
        ))
    }

    fn command_write_input(&self, args: &Value) -> Result<String, ToolError> {
        let id = CommandSessionId(string_arg(args, "session_id")?.into());
        let data = string_arg(args, "data")?;
        self.terminal
            .write_stdin(&id, &Controller::Task(self.task_owner()), data.as_bytes())
            .map_err(terminal_error)?;
        Ok(format!("session_id={} bytes={}", id.0, data.len()))
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

    fn revalidate_hook_arguments(&self, tool: &str, value: &Value) -> Result<(), String> {
        let serialized = serde_json::to_string(value).map_err(|error| error.to_string())?;
        self.registry
            .validate_and_normalize(tool, &serialized)
            .map_err(|error| error.to_string())?;
        if let Some(path) = value.get("path").and_then(Value::as_str) {
            let direction = if self
                .registry
                .contract(tool)
                .map_err(|error| error.to_string())?
                .side_effect
                == crate::tools::SideEffectClass::Read
            {
                Direction::Read
            } else {
                Direction::Write
            };
            let resolved = self
                .resolve_path(path, true)
                .map_err(|error| error.to_string())?;
            self.check_path(tool, &resolved, direction)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl RuntimeToolExecutor for ToolExecutor {
    fn prepare(
        &self,
        _call_id: &str,
        tool: &str,
        normalized_arguments: &str,
    ) -> Result<Option<String>, String> {
        let Some(hooks) = self.hooks.as_ref() else {
            return Ok(None);
        };
        let original: Value = serde_json::from_str(normalized_arguments)
            .map_err(|error| format!("normalized tool arguments are invalid: {error}"))?;
        let event = termior_hooks::HookEvent {
            schema_version: termior_hooks::HOOK_SCHEMA_VERSION,
            point: termior_hooks::HookPoint::PreTool,
            task_id: self.task_owner(),
            tool: Some(tool.into()),
            arguments: Some(original),
        };
        let outcome = hooks
            .apply_pre_tool(&event, |value| self.revalidate_hook_arguments(tool, value))
            .map_err(|error| error.to_string())?;
        if outcome.replaced {
            serde_json::to_string(&outcome.arguments)
                .map(Some)
                .map_err(|error| error.to_string())
        } else {
            Ok(None)
        }
    }

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

    fn execute_change_batch(
        &self,
        calls: &[(String, String)],
        cancellation: &CancellationToken,
    ) -> Result<ToolExecution, String> {
        if cancellation.is_cancelled() {
            return Err("change set cancelled before proposal generation".into());
        }
        let arguments = calls
            .iter()
            .map(|(_, arguments)| {
                serde_json::from_str::<Value>(arguments).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = self
            .propose_write_batch(&arguments)
            .map_err(|error| error.to_string())?;
        let summary: EditProposalSummary =
            serde_json::from_str(&output).map_err(|error| error.to_string())?;
        Ok(ToolExecution::ChangeProposed {
            change_set_id: ChangeSetId(summary.id),
            summary: output,
        })
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
            Err(ToolError::EditConflict(conflict)) => Ok(ChangeReviewExecution::Conflict(conflict)),
            Err(error) => Err(error.to_string()),
        }
    }

    fn cancel_active(&self) -> bool {
        let mut confirmed = self.terminal.kill_all();
        if let Ok(mut shell) = self.shell.lock() {
            shell.take();
        } else {
            confirmed = false;
        }
        confirmed
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

fn terminal_error(error: termior_terminal::TerminalServiceError) -> ToolError {
    ToolError::Io(error.to_string())
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
    terminal: &dyn TerminalService,
    session_id: &CommandSessionId,
    cursor: &mut OutputCursor,
    marker: &str,
    timeout: Duration,
) -> Result<String, ToolError> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    loop {
        let read = terminal
            .read_output(session_id, Some(*cursor), 256 * 1024)
            .map_err(terminal_error)?;
        *cursor = read.next;
        bytes.extend(read.bytes());
        let text = String::from_utf8_lossy(&bytes);
        if let Some(position) = text.find(marker) {
            let suffix = &text[position + marker.len()..];
            let exit = suffix
                .trim_start_matches(':')
                .split(|character: char| character.is_whitespace())
                .next()
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown");
            return Ok(format!(
                "session_id={} state=running exit={} cursor={}\n{}",
                session_id.0,
                exit,
                cursor.sequence,
                text[..position].trim()
            ));
        }
        if read.state.is_terminal() {
            return Err(ToolError::Io(format!(
                "persistent shell {} ended before completion marker ({:?})",
                session_id.0, read.state
            )));
        }
        if Instant::now() >= deadline {
            return Err(ToolError::Timeout(format!(
                "persistent shell command in {}; captured={}",
                session_id.0,
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(512)
                    .collect::<String>()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn shell_program(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Zsh => "zsh",
        ShellKind::Bash => "bash",
        ShellKind::Fish => "fish",
        ShellKind::Pwsh => "pwsh",
        ShellKind::PowerShell => "powershell",
        ShellKind::Cmd => "cmd",
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
    fn accepted_edit_creates_a_restorable_checkpoint_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoint_dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old\n").unwrap();
        let executor = executor(dir.path()).with_checkpoint_store(checkpoint_dir.path());
        executor.set_task_owner("task-checkpoint");
        let summary: EditProposalSummary = serde_json::from_str(
            &executor
                .execute_approved("write_file", r#"{"path":"a.txt","content":"new\n"}"#)
                .unwrap(),
        )
        .unwrap();
        let result = executor
            .accept_edit(&summary.id, &summary.hunk_ids)
            .unwrap();
        assert!(result.contains("checkpoint=checkpoint-task-checkpoint-1"));
        let manifest_path = checkpoint_dir
            .path()
            .join("manifests")
            .join("checkpoint-task-checkpoint-1.json");
        let manifest: termior_store::CheckpointManifest =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        let store = termior_store::CheckpointStore::new(checkpoint_dir.path());
        let plan = store.plan_restore(dir.path(), &manifest).unwrap();
        store.apply_restore(dir.path(), &plan).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "old\n");
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

    #[test]
    fn agent_must_claim_a_visible_pane_before_writing_and_user_can_take_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let executor = executor(dir.path());
        executor.set_task_owner("task-pane-tool");
        let service = termior_terminal::shared_terminal_service();
        let mut bridge =
            termior_terminal::TerminalBridge::spawn(&termior_terminal::PtySessionConfig::default())
                .unwrap();
        let id = service
            .register_pane(
                CommandCreate {
                    owner: CommandOwner::User,
                    project_dir: dir.path().display().to_string(),
                    environment_id: "pane-test".into(),
                    cwd: dir.path().display().to_string(),
                    shell: "test".into(),
                    command: String::new(),
                    interactive: true,
                    rows: 24,
                    cols: 80,
                },
                bridge.writer(),
            )
            .unwrap();
        let input = serde_json::json!({"session_id":&id.0, "data":"echo claimed\n"}).to_string();
        assert!(matches!(
            executor.execute_approved("command_write_input", &input),
            Err(ToolError::Io(_))
        ));
        executor
            .execute_approved(
                "command_claim",
                &serde_json::json!({"session_id":&id.0}).to_string(),
            )
            .unwrap();
        executor
            .execute_approved("command_write_input", &input)
            .unwrap();
        service.take_over(&id).unwrap();
        assert!(executor
            .execute_approved("command_write_input", &input)
            .is_err());
        service.observe_pane_exit(&id, None).unwrap();
        service.release(&id, false).unwrap();
        let _ = bridge.kill();
    }

    #[test]
    fn background_command_is_queryable_by_stable_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let executor = executor(dir.path());
        let output = executor
            .execute_approved("shell_bg_spawn", r#"{"command":"echo retained-output"}"#)
            .unwrap();
        let id = output
            .strip_prefix("session_id=")
            .and_then(|value| value.split_whitespace().next())
            .unwrap();
        let waited = executor
            .execute_auto(
                "command_wait",
                &format!(r#"{{"session_id":"{id}","timeout_ms":5000}}"#),
            )
            .unwrap();
        assert!(waited.contains("exit_code"));
        let retained = executor
            .execute_auto(
                "command_read_output",
                &format!(r#"{{"session_id":"{id}","after":0}}"#),
            )
            .unwrap();
        assert!(retained.contains("retained-output"));
        assert!(executor
            .execute_auto("command_status", &format!(r#"{{"session_id":"{id}"}}"#))
            .unwrap()
            .contains("exit_code"));
    }

    #[test]
    fn persistent_shell_uses_one_observable_command_session() {
        let dir = tempfile::tempdir().unwrap();
        let executor = executor(dir.path());
        executor.set_task_owner("task-persistent");
        let first = executor
            .execute_approved("shell_session_run", r#"{"command":"echo first"}"#)
            .unwrap();
        let second = executor
            .execute_approved("shell_session_run", r#"{"command":"echo second"}"#)
            .unwrap();
        let first_id = first
            .split_whitespace()
            .find_map(|part| part.strip_prefix("session_id="))
            .unwrap();
        let second_id = second
            .split_whitespace()
            .find_map(|part| part.strip_prefix("session_id="))
            .unwrap();
        assert_eq!(first_id, second_id);
        assert!(first.contains("first"));
        assert!(second.contains("second"));
    }
}
