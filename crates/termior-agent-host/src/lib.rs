//! Transport-neutral host contracts for in-process and external Agent backends (FR-AEXT).
//!
//! This crate intentionally has no GPUI dependency. UI code consumes [`BackendEvent`] only after
//! it has been normalized by a backend adapter.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod acp;
mod acp_host;
mod backend;
mod cli;
mod codex;
mod environment;
mod in_process;
mod mcp;
mod projection;
mod restore;
mod session;
mod transport;

pub use acp::{map_acp_message, AcpBackend, AcpClientHandler, ACP_PROTOCOL_VERSION};
pub use acp_host::{AcpHostPolicy, TermiorAcpClientHandler};
pub use backend::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    BackendSessionReport, CapabilityProfile, CapabilitySupport, TaskLaunch, TurnLaunch,
};
pub use cli::{find_program, AgentBackendKind, CliAgentFeatures, CliAgentProfile, CLI_AGENTS};
pub use codex::{
    map_codex_message, permission_entries, permissions_approval_result, CodexBackend,
    CodexProtocol, PermissionEntry, CODEX_COMPATIBLE_MINOR, PERMISSIONS_APPROVAL_METHOD,
};
pub use environment::{
    ensure_confined, EnvironmentKind, ExecutionEnvironment, PlatformSandbox, SandboxBackend,
    SandboxCapability, SandboxCommand, SandboxError, SandboxRequest, SandboxedProcess, TrustLevel,
};
pub use in_process::InProcessBackend;
pub use mcp::{
    confine_workspace_path, ingest_tool_declarations, sanitize_untrusted_description, McpCatalog,
    McpDiscovery, McpHttpClient, McpHttpToolHandler, McpIngest, McpServerPool, McpStdioClient,
    McpStdioSpec, McpStdioToolHandler, McpToolDeclaration, McpTransport, OAuthPkceRequest,
    QualifiedMcpTool, ServerLink, ServerLinkState, MCP_DESCRIPTION_MAX_CHARS, MCP_MAX_OUTPUT_BYTES,
    MCP_PROTOCOL_VERSION, TOOLS_LIST_CHANGED,
};
pub use projection::BackendTaskProjector;
pub use restore::{
    checkpoint_session, reconcile_persisted_backend_bindings, restore_external_session,
    ExternalRecoveryTarget, ExternalRestoreReport, ExternalSessionCheckpoint,
};
pub use session::{ManagedAgentSession, ManagedSessionState, PumpOutcome};
pub use transport::{
    JsonRpcLineCodec, StdioCapture, StdioTransport, TransportCommand, TransportMessage,
};
