//! schema 校验与版本迁移（FR-DATA）。
//!
//! 启动时读取持久化 JSON，校验 schema、按版本号迁移到当前 [`SCHEMA_VERSION`]。
//! 非法 JSON 返回结构化错误（供设置 About 页展示）。未知字段保留（向前兼容）。

use crate::settings::Settings;

/// 当前 settings schema 版本。
pub const SCHEMA_VERSION: u32 = 1;

/// 迁移错误。
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported version: found {found}, supported <= {max}")]
    UnsupportedVersion { found: u32, max: u32 },
}

/// 把一段原始 settings JSON 迁移到当前版本。
///
/// 当前仅支持 v1；未来 v2 出现时在此追加迁移步骤。
pub fn migrate(raw: &str) -> Result<Settings, MigrationError> {
    // 先解析为 Value 探测版本号，避免版本字段缺失时整体失败。
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let found = value
        .get("version")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(0);

    if found > SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedVersion {
            found,
            max: SCHEMA_VERSION,
        });
    }

    // v0（无版本号）→ v1：补默认值。
    let mut settings: Settings = if found == 0 {
        // 用默认值打底，再用 JSON 覆盖已知字段（serde flatten 行为）
        let defaults = serde_json::to_value(crate::settings::default_settings())?;
        let merged = merge_values(defaults, value);
        serde_json::from_value(merged)?
    } else {
        serde_json::from_str::<Settings>(raw)?
    };
    settings.version = SCHEMA_VERSION;
    Ok(settings)
}

/// 递归合并：`over` 中的键覆盖 `base`；同为对象时递归合并（保留 base 中缺失的字段）。
fn merge_values(base: serde_json::Value, over: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match (base, over) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let merged = if let Some(existing) = b.remove(&k) {
                    merge_values(existing, v)
                } else {
                    v
                };
                b.insert(k, merged);
            }
            Value::Object(b)
        }
        (_, o) => o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_settings;

    #[test]
    fn current_version_roundtrip() {
        let s = default_settings();
        let json = serde_json::to_string(&s).unwrap();
        let migrated = migrate(&json).unwrap();
        assert_eq!(migrated.version, SCHEMA_VERSION);
        assert_eq!(migrated.theme_id, s.theme_id);
    }

    #[test]
    fn v0_no_version_migrates_with_defaults() {
        // 缺 version 字段：按默认值打底
        let raw = r#"{"theme_id":"nord"}"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.version, SCHEMA_VERSION);
        assert_eq!(m.theme_id, "nord");
        // 缺失字段取默认值
        assert!(m.show_dotfiles);
    }

    #[test]
    fn invalid_json_rejected() {
        let raw = "{not valid json";
        let err = migrate(raw).unwrap_err();
        assert!(matches!(err, MigrationError::InvalidJson(_)));
    }

    #[test]
    fn future_version_rejected() {
        let raw = r#"{"version": 999}"#;
        let err = migrate(raw).unwrap_err();
        assert!(matches!(
            err,
            MigrationError::UnsupportedVersion { found: 999, max: SCHEMA_VERSION }
        ));
    }

    #[test]
    fn unknown_fields_preserved_or_ignored() {
        // 未知字段不应导致迁移失败（向前兼容）
        let raw = r#"{"version":1,"theme_id":"nord","future":"x"}"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.theme_id, "nord");
    }

    #[test]
    fn v0_with_partial_terminal() {
        let raw = r#"{"terminal":{"font_size":20}}"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.terminal.font_size, 20);
        // 其余 terminal 字段取默认
        assert!(m.terminal.scrollback_lines > 0);
    }
}
