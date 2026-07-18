//! 数据目录解析（FR-DATA 路径表）。
//!
//! 应用数据目录经 `dirs` crate 解析，bundle id `app.termior.Termior`，
//! **绝不裸读** `$HOME`/`%APPDATA%`。

use std::path::PathBuf;

/// bundle id（占位，对齐 Spec Q1 开放问题）。
pub const BUNDLE_ID: &str = "app.termior.Termior";

#[derive(Debug, thiserror::Error)]
pub enum AppDataError {
    #[error("could not resolve app data directory for this platform")]
    NotFound,
}

/// 返回应用数据目录（FR-DATA）。
///
/// | 平台 | 路径 |
/// |---|---|
/// | macOS | `~/Library/Application Support/app.termior.Termior/` |
/// | Linux | `~/.local/share/app.termior.Termior/` |
/// | Windows | `%APPDATA%\app.termior.Termior\` |
pub fn app_data_dir() -> Result<PathBuf, AppDataError> {
    // Desktop smoke tests need isolated state so they cannot overwrite a developer's
    // real workspace/session files. Normal launches do not set this variable.
    if let Some(path) = std::env::var_os("TERMIOR_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    // 使用 dirs 的 data_dir，跨平台一致映射到上表。
    let base = dirs::data_dir().ok_or(AppDataError::NotFound)?;
    Ok(base.join(BUNDLE_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_data_dir_resolves() {
        // 真实运行时应能解析；CI 环境通常也能。
        if let Ok(d) = app_data_dir() {
            let s = d.to_string_lossy();
            assert!(s.contains(BUNDLE_ID), "got {s}");
        }
    }

    #[test]
    fn bundle_id_is_termior() {
        assert_eq!(BUNDLE_ID, "app.termior.Termior");
    }
}
