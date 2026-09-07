//! 工具注册表 + 两级门控（FR-AGENT-09 / FR-SEC-01）。
//!
//! 工具分两级（对齐 [`termior_security::gating`]）：
//! - 自动执行（只读）：`read_file`、`list_directory`、`fs_search`、`fs_grep`、`get_terminal_context`
//! - 审批门控（写/执行）：`write_file`、`create_directory`、`rename`、`delete`、
//!   `run_command`、`shell_session_run`、`shell_bg_spawn`
//!
//! 本模块在工具执行前做安全门控（deny-list / workspace 授权），命中则拒绝。
//! `@path` 引用读取内容前也经此过滤（FR-AGENT-03）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use termior_security::deny_list::{canonicalize_logical, check_path, DenyReason, Direction};
use termior_security::workspace::WorkspaceAuthRegistry;

/// 工具执行错误。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("path denied by deny-list: {reason}")]
    DenyList { reason: String },
    #[error("workspace not authorized for path: {0}")]
    NotAuthorized(String),
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("approval required for tool: {0}")]
    ApprovalRequired(String),
    #[error("tool is not enabled for this agent: {0}")]
    NotAllowed(String),
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
    #[error("tool I/O failed: {0}")]
    Io(String),
    #[error("tool timed out: {0}")]
    Timeout(String),
    #[error("edit proposal not found: {0}")]
    ProposalNotFound(String),
    #[error("edit proposal conflicts with current disk content: {0}")]
    EditConflict(String),
}

/// Tool effects and approval are separate policy dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectClass {
    Read,
    LocalWrite,
    Process,
    Network,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalClass {
    Automatic,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    Idempotent,
    NonIdempotent,
    Unknown,
}

/// The single source of truth for Provider declarations, validation and UI disclosure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolContract {
    pub name: String,
    pub level: ToolLevelSerde,
    pub description: String,
    pub parameters: Value,
    pub side_effect: SideEffectClass,
    pub approval: ApprovalClass,
    pub default_timeout_ms: u64,
    pub max_output_bytes: u64,
    pub idempotency: Idempotency,
    pub parallel_safe: bool,
}

pub type ToolDescriptor = ToolContract;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLevelSerde {
    Auto,
    Approval,
}

/// 工具注册表：声明全部内置工具 + 关联安全上下文（deny-list + workspace 授权）。
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    pub workspace_auth: WorkspaceAuthRegistry,
    allowed_tools: Option<HashSet<String>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            workspace_auth: WorkspaceAuthRegistry::new(),
            allowed_tools: None,
        }
    }
}

impl ToolRegistry {
    pub fn new(workspace_auth: WorkspaceAuthRegistry) -> Self {
        Self {
            workspace_auth,
            allowed_tools: None,
        }
    }

    /// Restrict a custom/sub-agent to an explicit tool subset (FR-PLAN-02/03).
    pub fn subset<I, S>(mut self, tools: I) -> Result<Self, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut allowed = HashSet::new();
        for name in tools {
            let name = name.into();
            termior_security::gating::ToolId::from_name(&name)
                .ok_or_else(|| ToolError::Unknown(name.clone()))?;
            allowed.insert(name);
        }
        self.allowed_tools = Some(allowed);
        Ok(self)
    }

    pub fn allows(&self, tool_name: &str) -> bool {
        self.allowed_tools
            .as_ref()
            .map_or(true, |tools| tools.contains(tool_name))
    }

    fn ensure_allowed(&self, tool_name: &str) -> Result<(), ToolError> {
        if self.allows(tool_name) {
            Ok(())
        } else {
            Err(ToolError::NotAllowed(tool_name.to_owned()))
        }
    }

    /// Return enabled contracts. Every built-in contract has a closed object schema.
    pub fn contracts(&self) -> Vec<ToolContract> {
        use termior_security::gating::{ToolLevel, ALL_TOOLS};
        ALL_TOOLS
            .iter()
            .filter(|tool| self.allows(tool.name()))
            .map(|tool| {
                let level = match tool.level() {
                    ToolLevel::Auto => ToolLevelSerde::Auto,
                    ToolLevel::Approval => ToolLevelSerde::Approval,
                };
                ToolContract {
                    name: tool.name().to_string(),
                    level,
                    description: description_for(*tool).to_string(),
                    parameters: parameters_for(*tool),
                    side_effect: side_effect_for(*tool),
                    approval: match level {
                        ToolLevelSerde::Auto => ApprovalClass::Automatic,
                        ToolLevelSerde::Approval => ApprovalClass::User,
                    },
                    default_timeout_ms: timeout_for(*tool),
                    max_output_bytes: output_limit_for(*tool),
                    idempotency: idempotency_for(*tool),
                    parallel_safe: false,
                }
            })
            .collect()
    }

    /// Backwards-compatible name used by Provider adapters.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.contracts()
    }

    pub fn contract(&self, tool_name: &str) -> Result<ToolContract, ToolError> {
        self.ensure_allowed(tool_name)?;
        let tool = termior_security::gating::ToolId::from_name(tool_name)
            .ok_or_else(|| ToolError::Unknown(tool_name.to_owned()))?;
        self.contracts()
            .into_iter()
            .find(|contract| contract.name == tool.name())
            .ok_or_else(|| ToolError::Unknown(tool_name.to_owned()))
    }

    /// Parse, validate and canonicalize arguments using the Provider-visible contract.
    pub fn validate_and_normalize(
        &self,
        tool_name: &str,
        arguments: &str,
    ) -> Result<String, ToolError> {
        let contract = self.contract(tool_name)?;
        let value: Value = serde_json::from_str(arguments).map_err(|error| {
            invalid_arguments(tool_name, "$", &format!("invalid JSON: {error}"))
        })?;
        validate_value(tool_name, "$", &value, &contract.parameters)?;
        serde_json::to_string(&value).map_err(|error| {
            invalid_arguments(tool_name, "$", &format!("normalization failed: {error}"))
        })
    }

    /// 判定工具是否需要审批（FR-SEC-01）。
    pub fn requires_approval(&self, tool_name: &str) -> Result<bool, ToolError> {
        self.ensure_allowed(tool_name)?;
        let id = termior_security::gating::ToolId::from_name(tool_name)
            .ok_or_else(|| ToolError::Unknown(tool_name.to_string()))?;
        Ok(matches!(
            id.level(),
            termior_security::gating::ToolLevel::Approval
        ))
    }

    /// 对一条带 `path` 参数的工具调用做安全门控（deny-list + workspace 授权）。
    ///
    /// 用于 read_file / write_file / list_directory / fs_search / fs_grep / rename / delete。
    /// `direction` 表示读或写（deny-list 双向禁止）。
    pub fn check_path_access(
        &self,
        tool_name: &str,
        path: &str,
        direction: Direction,
    ) -> Result<(), ToolError> {
        // 工具必须存在
        self.ensure_allowed(tool_name)?;
        let _ = termior_security::gating::ToolId::from_name(tool_name)
            .ok_or_else(|| ToolError::Unknown(tool_name.to_string()))?;

        // INV-3：canonicalize 之后强制
        let canon = canonicalize_logical(path);

        // deny-list
        if let Some(reason) = check_path(&canon, direction) {
            return Err(ToolError::DenyList {
                reason: reason_message(&reason),
            });
        }
        // workspace 授权（FR-SEC-04）
        if !self.workspace_auth.is_authorized(&canon) {
            return Err(ToolError::NotAuthorized(canon));
        }
        Ok(())
    }
}

fn reason_message(r: &DenyReason) -> String {
    r.to_string()
}

fn description_for(t: termior_security::gating::ToolId) -> &'static str {
    use termior_security::gating::ToolId;
    match t {
        ToolId::ReadFile => "Read a file's contents (read-only, auto-executed).",
        ToolId::ListDirectory => "List directory entries (read-only, auto-executed).",
        ToolId::FsSearch => "Fuzzy file search (read-only, auto-executed).",
        ToolId::FsGrep => "Content search with grep (read-only, auto-executed).",
        ToolId::GetTerminalContext => {
            "Snapshot the active terminal's cwd + recent output (read-only, auto-executed)."
        }
        ToolId::WriteFile => "Write a file (approval-gated; rendered as hunk diff).",
        ToolId::CreateDirectory => "Create a directory (approval-gated).",
        ToolId::Rename => "Rename/move a path (approval-gated).",
        ToolId::Delete => "Delete a path (approval-gated).",
        ToolId::RunCommand => "Run a one-shot subshell command (approval-gated).",
        ToolId::ShellSessionRun => "Run a command in the persistent agent shell (approval-gated).",
        ToolId::ShellBgSpawn => "Spawn a long-running background process (approval-gated).",
        ToolId::RunSubagent => {
            "Delegate a bounded task to a restricted child agent (approval-gated)."
        }
    }
}

fn string_schema(max_length: u64) -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": max_length})
}

fn object_schema(
    required: &[&str],
    properties: impl IntoIterator<Item = (&'static str, Value)>,
) -> Value {
    let properties = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect::<Map<String, Value>>();
    json!({
        "type": "object",
        "required": required,
        "properties": properties,
        "additionalProperties": false
    })
}

fn parameters_for(tool: termior_security::gating::ToolId) -> Value {
    use termior_security::gating::ToolId;
    match tool {
        ToolId::ReadFile | ToolId::ListDirectory | ToolId::CreateDirectory | ToolId::Delete => {
            object_schema(&["path"], [("path", string_schema(32_768))])
        }
        ToolId::FsSearch | ToolId::FsGrep => {
            object_schema(&["query"], [("query", string_schema(4_096))])
        }
        ToolId::GetTerminalContext => object_schema(&[], []),
        ToolId::WriteFile => object_schema(
            &["path", "content"],
            [
                ("path", string_schema(32_768)),
                (
                    "content",
                    json!({"type": "string", "maxLength": 8_388_608u64}),
                ),
            ],
        ),
        ToolId::Rename => object_schema(
            &["source", "destination"],
            [
                ("source", string_schema(32_768)),
                ("destination", string_schema(32_768)),
            ],
        ),
        ToolId::RunCommand | ToolId::ShellBgSpawn => object_schema(
            &["command"],
            [
                ("command", string_schema(262_144)),
                ("cwd", string_schema(32_768)),
            ],
        ),
        ToolId::ShellSessionRun => {
            object_schema(&["command"], [("command", string_schema(262_144))])
        }
        ToolId::RunSubagent => object_schema(
            &["agent_id", "task"],
            [
                ("agent_id", string_schema(256)),
                ("task", string_schema(262_144)),
                (
                    "max_steps",
                    json!({"type": "integer", "minimum": 1, "maximum": 50}),
                ),
            ],
        ),
    }
}

fn side_effect_for(tool: termior_security::gating::ToolId) -> SideEffectClass {
    use termior_security::gating::ToolId;
    match tool {
        ToolId::ReadFile
        | ToolId::ListDirectory
        | ToolId::FsSearch
        | ToolId::FsGrep
        | ToolId::GetTerminalContext => SideEffectClass::Read,
        ToolId::WriteFile | ToolId::CreateDirectory | ToolId::Rename | ToolId::Delete => {
            SideEffectClass::LocalWrite
        }
        ToolId::RunCommand
        | ToolId::ShellSessionRun
        | ToolId::ShellBgSpawn
        | ToolId::RunSubagent => SideEffectClass::Process,
    }
}

fn timeout_for(tool: termior_security::gating::ToolId) -> u64 {
    use termior_security::gating::ToolId;
    match tool {
        ToolId::RunCommand | ToolId::ShellSessionRun => 30_000,
        ToolId::ShellBgSpawn | ToolId::RunSubagent => 60_000,
        _ => 10_000,
    }
}

fn output_limit_for(tool: termior_security::gating::ToolId) -> u64 {
    use termior_security::gating::ToolId;
    match tool {
        ToolId::ReadFile => 2 * 1024 * 1024,
        ToolId::RunCommand | ToolId::ShellSessionRun => 1024 * 1024,
        _ => 512 * 1024,
    }
}

fn idempotency_for(tool: termior_security::gating::ToolId) -> Idempotency {
    use termior_security::gating::ToolId;
    match tool {
        ToolId::ReadFile
        | ToolId::ListDirectory
        | ToolId::FsSearch
        | ToolId::FsGrep
        | ToolId::GetTerminalContext
        | ToolId::WriteFile
        | ToolId::CreateDirectory => Idempotency::Idempotent,
        ToolId::Rename
        | ToolId::Delete
        | ToolId::RunCommand
        | ToolId::ShellSessionRun
        | ToolId::ShellBgSpawn
        | ToolId::RunSubagent => Idempotency::NonIdempotent,
    }
}

fn invalid_arguments(tool: &str, path: &str, message: &str) -> ToolError {
    ToolError::InvalidArguments(format!("{tool} at {path}: {message}"))
}

fn validate_value(tool: &str, path: &str, value: &Value, schema: &Value) -> Result<(), ToolError> {
    let expected = schema.get("type").and_then(Value::as_str).unwrap_or("object");
    let type_matches = match expected {
        "object" => value.is_object(),
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        _ => false,
    };
    if !type_matches {
        return Err(invalid_arguments(
            tool,
            path,
            &format!("expected {expected}"),
        ));
    }

    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|minimum| length < minimum)
        {
            return Err(invalid_arguments(tool, path, "string is too short"));
        }
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
        {
            return Err(invalid_arguments(tool, path, "string is too long"));
        }
    }

    if let Some(number) = value.as_i64() {
        if schema
            .get("minimum")
            .and_then(Value::as_i64)
            .is_some_and(|minimum| number < minimum)
        {
            return Err(invalid_arguments(tool, path, "number is below minimum"));
        }
        if schema
            .get("maximum")
            .and_then(Value::as_i64)
            .is_some_and(|maximum| number > maximum)
        {
            return Err(invalid_arguments(tool, path, "number exceeds maximum"));
        }
    }

    if let Some(object) = value.as_object() {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_arguments(tool, path, "contract has no properties"))?;
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(field) {
                    return Err(invalid_arguments(
                        tool,
                        &format!("{path}.{field}"),
                        "missing required field",
                    ));
                }
            }
        }
        for (field, child) in object {
            let child_path = format!("{path}.{field}");
            let child_schema = properties.get(field).ok_or_else(|| {
                invalid_arguments(tool, &child_path, "additional field is not allowed")
            })?;
            validate_value(tool, &child_path, child, child_schema)?;
        }
    }

    if let Some(array) = value.as_array() {
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_value(tool, &format!("{path}[{index}]"), item, item_schema)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use termior_security::workspace::WorkspaceAuthRegistry;

    fn reg_with(root: &str) -> ToolRegistry {
        ToolRegistry::new(WorkspaceAuthRegistry::with_roots([root.to_string()]))
    }

    #[test]
    fn descriptors_cover_all_tools() {
        let r = ToolRegistry::default();
        let descriptors = r.descriptors();
        let names: Vec<&str> = descriptors.iter().map(|d| d.name.as_str()).collect();
        for expected in [
            "read_file",
            "write_file",
            "run_command",
            "get_terminal_context",
            "fs_grep",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn read_file_in_workspace_allowed() {
        let r = reg_with("/proj");
        r.check_path_access("read_file", "/proj/src/main.rs", Direction::Read)
            .unwrap();
    }

    #[test]
    fn denylist_blocks_dotenv_even_in_workspace() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/proj/.env", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn denylist_blocks_dotenv_on_write_too() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("write_file", "/proj/.env", Direction::Write)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn traversal_to_dotenv_blocked() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/proj/src/../../.env", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }

    #[test]
    fn path_outside_workspace_blocked() {
        let r = reg_with("/proj");
        let err = r
            .check_path_access("read_file", "/etc/passwd", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::NotAuthorized(_)));
    }

    #[test]
    fn write_tools_require_approval() {
        let r = ToolRegistry::default();
        assert!(r.requires_approval("write_file").unwrap());
        assert!(r.requires_approval("run_command").unwrap());
        assert!(!r.requires_approval("read_file").unwrap());
        assert!(!r.requires_approval("get_terminal_context").unwrap());
    }

    #[test]
    fn unknown_tool_errors() {
        let r = ToolRegistry::default();
        assert!(matches!(
            r.requires_approval("nope"),
            Err(ToolError::Unknown(_))
        ));
        assert!(matches!(
            r.check_path_access("nope", "/proj/x", Direction::Read),
            Err(ToolError::Unknown(_))
        ));
    }

    #[test]
    fn subset_hides_and_rejects_other_tools() {
        let registry = ToolRegistry::default()
            .subset(["read_file", "fs_search"])
            .unwrap();
        assert!(registry.allows("read_file"));
        assert!(!registry.allows("write_file"));
        assert_eq!(registry.descriptors().len(), 2);
        assert!(matches!(
            registry.requires_approval("write_file"),
            Err(ToolError::NotAllowed(_))
        ));
    }

    #[test]
    fn ssh_key_blocked_even_in_workspace() {
        let r = reg_with("/home/u");
        let err = r
            .check_path_access("read_file", "/home/u/.ssh/id_rsa", Direction::Read)
            .unwrap_err();
        assert!(matches!(err, ToolError::DenyList { .. }));
    }
}
