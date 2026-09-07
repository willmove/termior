//! `termior-ai` — AI Provider、Agent 运行时、工具注册表、审批网关、会话记忆（P0 逻辑部分）。
//!
//! 对应 Spec §6.9 FR-PROV、§6.10 FR-AGENT、§6.12 FR-SESS、§6.14 FR-SEC 的逻辑层。
//!
//! 模块：
//! - [`message`]：统一消息 / 工具调用 / 流式事件模型。
//! - [`provider`]：`Provider` trait + `MockProvider`（FR-PROV）。
//! - [`http_provider`]：OpenAI-compatible、Anthropic、Gemini 与 Ollama 等 HTTP/SSE 适配。
//! - [`model_registry`]：模型选择器数据结构（FR-PROV-03）。
//! - [`secret_store`]：密钥接口（FR-PROV-04 / FR-SEC-06 / INV-5）。
//! - [`tools`]：工具注册表 + 两级门控（FR-AGENT-09）。
//! - [`approval`]：审批数据模型（FR-AGENT-10 / FR-SEC-01）。
//! - [`agent`]：Agent 循环状态机（FR-AGENT-08）。
//! - [`context`]：实时上下文桥契约（FR-AGENT-07）。
//! - [`session`]：会话与项目记忆（FR-SESS-01/02）。
//!
//! `HttpProvider` 承担真实网络调用；`MockProvider` 用于确定性跑通 Agent 循环测试。

#![forbid(unsafe_code)]

pub mod agent;
pub mod approval;
pub mod composer;
pub mod context;
pub mod executor;
pub mod http_provider;
pub mod inline_completion;
#[cfg(feature = "keyring-backend")]
pub mod keyring_store;
pub mod message;
pub mod mode;
pub mod model_registry;
pub mod plan;
pub mod provider;
pub mod runtime;
pub mod secret_store;
pub mod session;
pub mod task;
pub mod tools;

pub use agent::{Agent, AgentError, AgentOutcome, AgentState, MAX_AGENT_STEPS};
pub use approval::{ApprovalDecision, ApprovalRequest};
pub use composer::{
    Attachment, AttachmentSource, ComposerDraft, ComposerError, ComposerPayload, Snippet,
    SnippetStore, TodoItem, TodoStore,
};
pub use context::{TerminalContext, TerminalContextProvider};
pub use executor::{EditProposalSummary, ToolExecutor};
pub use http_provider::{HttpProvider, ProviderConfig, ProviderTransportError};
pub use inline_completion::{
    completion_user_prompt, normalize_completion, InlineCompleter, InlineCompletionContext,
    InlineCompletionResult, INLINE_COMPLETION_SYSTEM_PROMPT,
};
#[cfg(feature = "keyring-backend")]
pub use keyring_store::KeyringSecretStore;
pub use message::{ChatEvent, Message, Role, ToolCall, ToolResult};
pub use mode::Mode;
pub use model_registry::{ModelRegistry, ProviderKind};
pub use plan::{
    run_subagent, AgentDefinition, AgentDefinitionStore, Plan, PlanError, PlanStep, PlanStepKind,
    PlanStepStatus, SubagentResult,
};
pub use provider::{MockProvider, Provider, ProviderRequest};
pub use runtime::{
    CancellationToken, ChangeReviewExecution, RuntimeError, RuntimeToolExecutor, TaskRuntime,
    ToolExecution,
};
pub use secret_store::{InMemorySecretStore, SecretStore, SecretStoreError};
pub use session::{ProjectMemory, Session, SessionStore};
pub use task::{
    AcceptanceCheck, AcceptanceCriterion, AcceptanceReport, AcceptanceStatus, ApprovalPolicy,
    BudgetDimension, ChangeSetId, DecisionSource, RuntimeBudgets, RuntimeUsage, Task, TaskCommand,
    TaskConfig, TaskEvent, TaskEventKind, TaskId, TaskState, TaskStateTransitionError, TaskSummary,
    TaskSummaryStore, ToolAttempt, ToolDecision, ToolInvocation, ToolState, Turn, TurnId,
    WaitingReason, TASK_EVENT_SCHEMA_VERSION,
};
pub use tools::{
    ApprovalClass, Idempotency, SideEffectClass, ToolContract, ToolDescriptor, ToolError,
    ToolLevelSerde, ToolRegistry,
};
