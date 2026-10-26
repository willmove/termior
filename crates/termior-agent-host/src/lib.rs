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
mod matrix;
mod mcp;
mod mcp_oauth;
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
    map_codex_message, permission_entries, permissions_approval_result, sandbox_provisioning_hint,
    CodexBackend, CodexProtocol, CodexSandboxMode, PermissionEntry, CODEX_COMPATIBLE_MINOR,
    PERMISSIONS_APPROVAL_METHOD,
};
#[cfg(windows)]
pub use environment::WindowsAppContainerSandbox;
pub use environment::{
    ensure_confined, EnvironmentKind, ExecutionEnvironment, PlatformSandbox, SandboxBackend,
    SandboxCapability, SandboxCommand, SandboxError, SandboxRequest, SandboxStream,
    SandboxedProcess, TrustLevel,
};
pub use in_process::InProcessBackend;
pub use matrix::{apply_sandbox_probe, capability_matrix, CapabilityMatrixRow};
pub use mcp::{
    confine_workspace_path, ingest_tool_declarations, sanitize_untrusted_description, McpCatalog,
    McpDiscovery, McpHttpClient, McpHttpToolHandler, McpIngest, McpServerPool, McpStdioClient,
    McpStdioSpec, McpStdioToolHandler, McpToolDeclaration, McpTransport, OAuthPkceRequest,
    QualifiedMcpTool, ServerLink, ServerLinkState, MCP_DESCRIPTION_MAX_CHARS, MCP_MAX_OUTPUT_BYTES,
    MCP_PROTOCOL_VERSION, TOOLS_LIST_CHANGED,
};
pub use mcp_oauth::{
    discover_authorization, login_with_browser, logout_mcp_server, mcp_oauth_key, mcp_servers_path,
    oauth_code_challenge_s256, system_clock, DiscoveredAuthorization, McpClock, McpLoginStatus,
    McpLogoutReport, McpOAuthBinding, McpRemoteServer, McpServerCatalog, PkceLoginOptions,
    MCP_AUTH_DETAIL_REFRESH_REJECTED, MCP_AUTH_DETAIL_REVOKE_FAILED,
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
