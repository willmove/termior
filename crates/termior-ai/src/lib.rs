//! `termior-ai` — AI Provider、Agent 运行时、工具注册表、审批网关、会话记忆（P0 逻辑部分）。
//!
//! 对应 Spec §6.9 FR-PROV、§6.10 FR-AGENT、§6.12 FR-SESS、§6.14 FR-SEC 的逻辑层。
//!
//! 模块：
//! - [`message`]：统一消息 / 工具调用 / 流式事件模型。
//! - [`provider`]：`Provider` trait + `MockProvider`（FR-PROV）。
//! - [`model_registry`]：模型选择器数据结构（FR-PROV-03）。
//! - [`secret_store`]：密钥接口（FR-PROV-04 / FR-SEC-06 / INV-5）。
//! - [`tools`]：工具注册表 + 两级门控（FR-AGENT-09）。
//! - [`approval`]：审批网关（FR-AGENT-10 / FR-SEC-01）。
//! - [`agent`]：Agent 循环状态机（FR-AGENT-08）。
//! - [`context`]：实时上下文桥契约（FR-AGENT-07）。
//! - [`session`]：会话与项目记忆（FR-SESS-01/02）。
//!
//! 本 crate 不接真实 HTTP 网络（FR-PROV-01 各家适配器留待可编译验证里程碑）；
//! 用 `MockProvider` 跑通 Agent 循环全链路单测。

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval;
pub mod context;
pub mod message;
pub mod model_registry;
pub mod provider;
pub mod secret_store;
pub mod session;
pub mod tools;

pub use agent::{Agent, AgentError, AgentOutcome, AgentState, MAX_AGENT_STEPS};
pub use approval::{ApprovalDecision, ApprovalGate, ApprovalRequest};
pub use context::{TerminalContext, TerminalContextProvider};
pub use message::{ChatEvent, Message, Role, ToolCall, ToolResult};
pub use model_registry::{ModelRegistry, ProviderKind};
pub use provider::{MockProvider, Provider, ProviderRequest};
pub use secret_store::{InMemorySecretStore, SecretStore, SecretStoreError};
pub use session::{ProjectMemory, Session, SessionStore};
pub use tools::{ToolError, ToolRegistry};
