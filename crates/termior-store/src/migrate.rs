//! schema 校验与版本迁移（FR-DATA）。
//!
//! 启动时读取持久化 JSON，校验 schema、按版本号迁移到当前 [`SCHEMA_VERSION`]。
//! 非法 JSON 返回结构化错误（供设置 About 页展示）。未知字段保留（向前兼容）。

use crate::settings::{Appearance, Settings};

/// 当前 settings schema 版本。
pub const SCHEMA_VERSION: u32 = 2;

/// 迁移错误。
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported version: found {found}, supported <= {max}")]
    UnsupportedVersion { found: u32, max: u32 },
}

/// 把一段原始 settings JSON 迁移到当前版本。
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

    // v0（无版本号）→ 打底默认值再覆盖。
    let mut settings: Settings = if found == 0 {
        let defaults = serde_json::to_value(crate::settings::default_settings())?;
        let merged = merge_values(defaults, value);
        serde_json::from_value(merged)?
    } else {
        serde_json::from_str::<Settings>(raw)?
    };

    if found < 2 {
        migrate_v1_theme_pairing(&mut settings);
    }

    settings.version = SCHEMA_VERSION;
    Ok(settings)
}

/// v1 → v2：补齐浅/深主题配对，并把旧「同主题双色板」用法映射到独立主题 id。
fn migrate_v1_theme_pairing(settings: &mut Settings) {
    let theme_id = settings.theme_id.clone();
    let (light_id, dark_id, fixed_id) = match (settings.appearance, theme_id.as_str()) {
        (Appearance::Light, "default") => ("default-light", "default", "default-light"),
        (Appearance::Light, "nord") => ("nord-light", "nord", "nord-light"),
        (Appearance::Light, other) => ("default-light", other, other),
        (Appearance::Dark, "default-light") => ("default-light", "default", "default"),
        (Appearance::Dark, "nord-light") => ("nord-light", "nord", "nord"),
        (Appearance::Dark, other) => ("default-light", other, other),
        (Appearance::FollowSystem, "nord" | "nord-light") => ("nord-light", "nord", "nord"),
        (Appearance::FollowSystem, other) => ("default-light", other, other),
    };

    settings.light_theme_id = light_id.to_owned();
    settings.dark_theme_id = dark_id.to_owned();

    match settings.appearance {
        Appearance::Light | Appearance::Dark => {
            settings.theme_id = fixed_id.to_owned();
            // 若固定外观与主题原生冲突（例如 Light + tokyo-night），锁定到主题原生侧。
            if matches!(settings.appearance, Appearance::Light)
                && !matches!(fixed_id, "default-light" | "nord-light")
                && fixed_id == dark_id
                && fixed_id != light_id
            {
                // 深色原生主题被旧配置锁在 Light：改为 Dark，避免再走已删除的浅色推导。
                settings.appearance = Appearance::Dark;
            }
        }
        Appearance::FollowSystem => {
            settings.theme_id = dark_id.to_owned();
        }
    }
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
        assert_eq!(migrated.light_theme_id, s.light_theme_id);
        assert_eq!(migrated.dark_theme_id, s.dark_theme_id);
    }

    #[test]
    fn v0_no_version_migrates_with_defaults() {
        // 缺 version 字段：按默认值打底
        let raw = r#"{"theme_id":"nord"}"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.version, SCHEMA_VERSION);
        assert_eq!(m.theme_id, "nord");
        assert_eq!(m.light_theme_id, "nord-light");
        assert_eq!(m.dark_theme_id, "nord");
        // 缺失字段取默认值
        assert!(m.show_dotfiles);
    }

    #[test]
    fn v1_follow_system_gains_theme_pairing() {
        let raw = r#"{
            "version": 1,
            "appearance": "follow_system",
            "theme_id": "tokyo-night",
            "editor_theme_id": "default"
        }"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.version, 2);
        assert_eq!(m.appearance, Appearance::FollowSystem);
        assert_eq!(m.light_theme_id, "default-light");
        assert_eq!(m.dark_theme_id, "tokyo-night");
        assert_eq!(m.theme_id, "tokyo-night");
    }

    #[test]
    fn v1_light_default_maps_to_default_light() {
        let raw = r#"{
            "version": 1,
            "appearance": "light",
            "theme_id": "default",
            "editor_theme_id": "default"
        }"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.appearance, Appearance::Light);
        assert_eq!(m.theme_id, "default-light");
        assert_eq!(m.light_theme_id, "default-light");
        assert_eq!(m.dark_theme_id, "default");
    }

    #[test]
    fn v1_light_tokyo_night_snaps_to_dark_native() {
        let raw = r#"{
            "version": 1,
            "appearance": "light",
            "theme_id": "tokyo-night",
            "editor_theme_id": "default"
        }"#;
        let m = migrate(raw).unwrap();
        assert_eq!(m.appearance, Appearance::Dark);
        assert_eq!(m.theme_id, "tokyo-night");
        assert_eq!(m.dark_theme_id, "tokyo-night");
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
            MigrationError::UnsupportedVersion {
                found: 999,
                max: SCHEMA_VERSION
            }
        ));
    }

    #[test]
    fn unknown_fields_preserved_or_ignored() {
        // 未知字段不应导致迁移失败（向前兼容）
        let raw = r#"{"version":2,"theme_id":"nord","future":"x"}"#;
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
