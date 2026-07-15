//! `termior-security` — 安全模型纯逻辑核心（FR-SEC 全量 P0）。
//!
//! 对齐 Spec §6.14 与 §5.3 关键架构不变量：
//! - [`deny_list`] — secret deny-list（FR-SEC-03 / INV-3）。
//! - [`workspace`] — workspace 授权注册表（FR-SEC-04）。
//! - [`ssrf`] — SSRF guard（FR-SEC-05 / FR-PROV-05）。
//! - [`gating`] — 工具两级门控模型（FR-SEC-01 / FR-AGENT-09）。
//! - [`secret`] — 密钥不落盘校验（FR-SEC-06 / INV-5）。
//!
//! 所有触盘/触 shell/触密钥/触网判定均为纯函数，路径在 canonicalize 之后强制（INV-3），
//! 与调用来源无关（提示词注入、路径穿越均不可绕过）。
//!
//! # 无遥测声明（FR-SEC-07）
//! 本 crate 及整个 `termior` workspace 不包含任何遥测、账号或自动上报代码；
//! 崩溃报告仅本地留存。本断言由 `NO_TELEMETRY` 常量固化，供上层展示与审计。

#![forbid(unsafe_code)]

pub mod deny_list;
pub mod gating;
pub mod secret;
pub mod ssrf;
pub mod workspace;

pub use deny_list::{check_path, DenyList, DenyReason, Direction};
pub use gating::{ToolId, ToolLevel};
pub use secret::{assert_no_secret_fields, SecretLeak};
pub use ssrf::{SsrfError, SsrfGuard};
pub use workspace::{WorkspaceAuthRegistry, WorkspaceAuthStatus};

/// FR-SEC-07：固化无遥测声明。任何引用本 crate 的代码均可断言此值为 `true`。
pub const NO_TELEMETRY: bool = true;

/// 安全判定的统一错误类型。
#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("path denied by deny-list: {reason} (path: {path})")]
    DenyList { reason: DenyReason, path: String },

    #[error("workspace not authorized: {0}")]
    NotAuthorized(String),

    #[error("SSRF guard rejected: {0}")]
    Ssrf(#[from] SsrfError),

    #[error("secret leak detected: {0}")]
    SecretLeak(#[from] SecretLeak),
}
