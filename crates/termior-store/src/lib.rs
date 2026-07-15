//! `termior-store` — 设置/会话/数据的持久化与迁移（FR-DATA / FR-SET-01 / FR-SET-03）。
//!
//! - [`settings`]：应用偏好结构 + 默认值（FR-SET-01）。
//! - [`atomic`]：原子写（temp + rename）（FR-DATA 要求）。
//! - [`migrate`]：schema 校验与版本迁移（FR-DATA 要求）。
//! - [`keymap`]：默认键位表（附录A）+ Win/Linux Ctrl 映射（FR-SET-03）。
//! - [`paths`]：数据目录解析（FR-DATA 路径表）。
//!
//! **INV-5 / FR-SEC-06**：API key 永不进入任何持久化文件。本 crate 不持有 key 字段；
//! 写盘前可选经 [`assert_no_persistent_secret`] 扫描（默认在写 [`settings`] 时启用）。

#![forbid(unsafe_code)]

pub mod atomic;
pub mod keymap;
pub mod migrate;
pub mod paths;
pub mod settings;

pub use atomic::{atomic_write, AtomicWriteError};
pub use keymap::{default_keymap, KeyAction, KeyBinding, KeymapEntry, Platform};
pub use migrate::{migrate, MigrationError, SCHEMA_VERSION};
pub use paths::{app_data_dir, AppDataError};
pub use settings::{default_settings, Settings, TerminalSettings};

/// FR-SEC-06 / INV-5：扫描待落盘文本是否含形似密钥的明文。命中返回错误。
///
/// 这是 settings 落盘前的兜底校验；真正的密钥存储在 OS 钥匙串（见 termior-ai::SecretStore）。
pub fn assert_no_persistent_secret(text: &str) -> Result<(), PersistentSecretError> {
    // 识别常见密钥前缀与键值对形态。
    for tok in text.split_whitespace() {
        let t = tok.trim_matches(|c: char| matches!(c, '"' | ',' | ':' | '\n'));
        if looks_like_api_key(t) {
            return Err(PersistentSecretError::Leak {
                snippet: t.chars().take(8).collect(),
            });
        }
    }
    Ok(())
}

fn looks_like_api_key(t: &str) -> bool {
    let prefixes = ["sk-ant-", "sk-", "xai-", "AIza", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    prefixes.iter().any(|p| t.starts_with(p) && t.len() > p.len() + 8)
}

#[derive(Debug, thiserror::Error)]
pub enum PersistentSecretError {
    #[error("potential secret leak detected near: {snippet}")]
    Leak { snippet: String },
}
