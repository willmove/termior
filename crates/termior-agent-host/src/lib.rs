//! Transport-neutral host contracts for in-process and external Agent backends (FR-AEXT).
//!
//! This crate intentionally has no GPUI dependency. UI code consumes [`BackendEvent`] only after
//! it has been normalized by a backend adapter.

#![forbid(unsafe_code)]

mod acp;
mod acp_host;
mod backend;
mod codex;
mod environment;
mod in_process;
mod mcp;
mod projection;
mod session;
mod transport;

pub use acp::{map_acp_message, AcpBackend, AcpClientHandler, ACP_PROTOCOL_VERSION};
pub use acp_host::{AcpHostPolicy, TermiorAcpClientHandler};
pub use backend::{
    AgentBackend, BackendBinding, BackendDecision, BackendError, BackendEvent, BackendInfo,
    CapabilityProfile, CapabilitySupport, TaskLaunch, TurnLaunch,
};
pub use codex::{map_codex_message, CodexBackend, CodexProtocol, CODEX_COMPATIBLE_MINOR};
pub use environment::{
    ensure_confined, EnvironmentKind, ExecutionEnvironment, PlatformSandbox, SandboxBackend,
    SandboxCapability, SandboxCommand, SandboxError, SandboxRequest, TrustLevel,
};
pub use in_process::InProcessBackend;
pub use mcp::{
    McpCatalog, McpDiscovery, McpHttpClient, McpHttpToolHandler, McpStdioClient,
    McpStdioToolHandler, McpToolDeclaration, McpTransport, OAuthPkceRequest, QualifiedMcpTool,
    MCP_PROTOCOL_VERSION,
};
pub use projection::BackendTaskProjector;
pub use session::{ManagedAgentSession, ManagedSessionState, PumpOutcome};
pub use transport::{JsonRpcLineCodec, StdioTransport, TransportCommand, TransportMessage};
