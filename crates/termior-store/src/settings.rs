//! 应用偏好结构（FR-SET-01）。
//!
//! 六页签设置中 General 页对应的结构化数据：shell、字体、autocomplete、自定义指令、
//! dotfiles 显隐等。Models/Themes/Shortcuts/Agents/About 的数据分别由各 crate 持有，
//! settings 只保留跨页签的偏好转盘与版本号。

use serde::{Deserialize, Serialize};

/// 当前 schema 版本（由 [`crate::migrate`] 消费）。
pub const SETTINGS_VERSION: u32 = 1;

/// 应用偏好根结构（`Termior-settings.json`，FR-DATA）。
///
/// 关闭应用后手动编辑 JSON 应当合法——所有字段均带 `#[serde(default)]`，
/// 缺失字段取默认值（FR-DATA：「关闭状态下手动编辑 JSON 合法，下次启动校验迁移」）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default = "default_settings")]
pub struct Settings {
    /// schema 版本号，供迁移使用。
    #[serde(default = "default_version")]
    pub version: u32,
    pub appearance: Appearance,
    /// 应用主题 id（FR-THEME-02）。
    pub theme_id: String,
    /// 编辑器主题 id（独立于应用主题，FR-EDIT-07，P1）。
    pub editor_theme_id: String,
    pub terminal: TerminalSettings,
    /// autocomplete 开关（FR-EDIT-05，P1；结构预留）。
    pub autocomplete_enabled: bool,
    /// 自定义指令（设置 → General，FR-SESS-03 P1）。
    pub custom_instructions: String,
    /// dotfiles 显隐开关（FR-EXPL-01）。
    pub show_dotfiles: bool,
    /// 代理通知开关（FR-NOTIF-05 P1）。
    pub agent_notifications: bool,
    /// WebGL→GPU 渲染等价开关占位（FR-SET-01）。
    pub prefer_software_rendering: bool,
    /// Vim compatibility layer (FR-EDIT-06).
    pub vim_mode: bool,
    /// Optional WSL distribution selected by the workspace switcher (Windows, FR-WS-06).
    pub wsl_distribution: Option<String>,
    /// Whole-window background image configuration (FR-THEME-05).
    pub background: BackgroundSettings,
    /// Provider endpoints and model choices. Credentials are referenced by profile id and live
    /// exclusively in the OS keychain.
    pub models: ModelSettings,
    /// User-rebindable shortcuts, validated for duplicate chords before mutation.
    pub keymap: crate::keymap::UserKeymap,
}

fn default_version() -> u32 {
    SETTINGS_VERSION
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    FollowSystem,
}

/// 终端设置（FR-TERM-09）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalSettings {
    pub font_family: String,
    /// 字号 8–32（FR-TERM-09 预设档位）。
    pub font_size: u8,
    pub line_height: f32,
    /// Additional glyph spacing in logical pixels.
    #[serde(default)]
    pub letter_spacing: f32,
    /// 回滚行数 200–50,000（FR-TERM-09 预设档位）。
    pub scrollback_lines: u32,
    /// shell 探测策略（FR-TERM-05）。
    pub shell_detection: ShellDetection,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: default_font_family(),
            font_size: 14,
            line_height: 1.2,
            letter_spacing: 0.0,
            scrollback_lines: 10_000,
            shell_detection: ShellDetection::Auto,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        default_settings()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundSettings {
    pub image_path: Option<String>,
    /// 0 = invisible, 1 = fully opaque.
    pub opacity: f32,
    /// Gaussian blur radius in logical pixels.
    pub blur: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProviderSettings {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    pub base_url: String,
    pub model: String,
    pub local: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub profiles: Vec<ModelProviderSettings>,
    pub active_chat_profile: Option<String>,
    pub active_completion_profile: Option<String>,
    #[serde(default)]
    pub favorite_models: Vec<String>,
    #[serde(default)]
    pub recent_models: Vec<String>,
}

impl Default for ModelSettings {
    fn default() -> Self {
        let definitions = [
            (
                "anthropic",
                "anthropic",
                "Anthropic",
                "https://api.anthropic.com/v1",
                "claude-sonnet-4",
                false,
            ),
            (
                "openai",
                "open_ai",
                "OpenAI",
                "https://api.openai.com/v1",
                "gpt-4o",
                false,
            ),
            (
                "google",
                "google",
                "Google Gemini",
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-2.5-pro",
                false,
            ),
            (
                "groq",
                "groq",
                "Groq",
                "https://api.groq.com/openai/v1",
                "llama-3.3-70b-versatile",
                false,
            ),
            ("xai", "xai", "xAI", "https://api.x.ai/v1", "grok-3", false),
            (
                "cerebras",
                "cerebras",
                "Cerebras",
                "https://api.cerebras.ai/v1",
                "llama-3.3-70b",
                false,
            ),
            (
                "openrouter",
                "open_router",
                "OpenRouter",
                "https://openrouter.ai/api/v1",
                "openai/gpt-4o",
                false,
            ),
            (
                "deepseek",
                "deep_seek",
                "DeepSeek",
                "https://api.deepseek.com/v1",
                "deepseek-chat",
                false,
            ),
            (
                "mistral",
                "mistral",
                "Mistral",
                "https://api.mistral.ai/v1",
                "mistral-large-latest",
                false,
            ),
            (
                "compatible",
                "open_ai_compatible",
                "OpenAI compatible",
                "https://api.openai.com/v1",
                "default",
                false,
            ),
            (
                "lm-studio",
                "lm_studio",
                "LM Studio",
                "http://127.0.0.1:1234/v1",
                "local-model",
                true,
            ),
            (
                "mlx",
                "mlx",
                "MLX",
                "http://127.0.0.1:8080/v1",
                "local-model",
                true,
            ),
            (
                "ollama",
                "ollama",
                "Ollama",
                "http://127.0.0.1:11434",
                "llama3.2",
                true,
            ),
        ];
        Self {
            profiles: definitions
                .into_iter()
                .map(
                    |(id, provider, display_name, base_url, model, local)| ModelProviderSettings {
                        id: id.into(),
                        provider: provider.into(),
                        display_name: display_name.into(),
                        base_url: base_url.into(),
                        model: model.into(),
                        local,
                        enabled: local,
                    },
                )
                .collect(),
            active_chat_profile: None,
            active_completion_profile: None,
            favorite_models: Vec::new(),
            recent_models: Vec::new(),
        }
    }
}

impl Default for BackgroundSettings {
    fn default() -> Self {
        Self {
            image_path: None,
            opacity: 0.2,
            blur: 0.0,
        }
    }
}

/// shell 探测策略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellDetection {
    /// Unix 跟随 `$SHELL`；Windows 探测 pwsh → powershell → cmd。
    Auto,
    /// 显式指定 shell 路径。
    Manual { path: String },
}

fn default_font_family() -> String {
    // 跨平台默认等宽字体；真实实现应按平台选最优。
    if cfg!(target_os = "windows") {
        "Cascadia Code".into()
    } else if cfg!(target_os = "macos") {
        "SF Mono".into()
    } else {
        "JetBrains Mono".into()
    }
}

/// 默认设置（FR-SET-01）。
pub fn default_settings() -> Settings {
    Settings {
        version: SETTINGS_VERSION,
        appearance: Appearance::default(),
        theme_id: "default".into(),
        editor_theme_id: "default".into(),
        terminal: TerminalSettings::default(),
        autocomplete_enabled: false,
        custom_instructions: String::new(),
        show_dotfiles: true,
        agent_notifications: true,
        prefer_software_rendering: false,
        vim_mode: false,
        wsl_distribution: None,
        background: BackgroundSettings::default(),
        models: ModelSettings::default(),
        keymap: crate::keymap::UserKeymap::default(),
    }
}

impl Settings {
    /// 校验不变量（字号、回滚行数落在 FR-TERM-09 区间）。
    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(8..=32).contains(&self.terminal.font_size) {
            return Err(SettingsError::OutOfRange {
                field: "font_size".into(),
                value: self.terminal.font_size.to_string(),
            });
        }
        if !(200..=50_000).contains(&self.terminal.scrollback_lines) {
            return Err(SettingsError::OutOfRange {
                field: "scrollback_lines".into(),
                value: self.terminal.scrollback_lines.to_string(),
            });
        }
        if !(0.0..=1.0).contains(&self.background.opacity) {
            return Err(SettingsError::OutOfRange {
                field: "background.opacity".into(),
                value: self.background.opacity.to_string(),
            });
        }
        if !(0.0..=64.0).contains(&self.background.blur) {
            return Err(SettingsError::OutOfRange {
                field: "background.blur".into(),
                value: self.background.blur.to_string(),
            });
        }
        if !(-2.0..=8.0).contains(&self.terminal.letter_spacing) {
            return Err(SettingsError::OutOfRange {
                field: "letter_spacing".into(),
                value: self.terminal.letter_spacing.to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("field {field} out of range: {value}")]
    OutOfRange { field: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let s = default_settings();
        assert_eq!(s.version, SETTINGS_VERSION);
        assert!(s.show_dotfiles);
        assert!(s.agent_notifications);
        assert_eq!(s.theme_id, "default");
        s.validate().unwrap();
    }

    #[test]
    fn font_size_range_enforced() {
        let mut s = default_settings();
        s.terminal.font_size = 4;
        assert!(s.validate().is_err());
        s.terminal.font_size = 33;
        assert!(s.validate().is_err());
        s.terminal.font_size = 8;
        assert!(s.validate().is_ok());
        s.terminal.font_size = 32;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn scrollback_range_enforced() {
        let mut s = default_settings();
        s.terminal.scrollback_lines = 100;
        assert!(s.validate().is_err());
        s.terminal.scrollback_lines = 60_000;
        assert!(s.validate().is_err());
        s.terminal.scrollback_lines = 200;
        assert!(s.validate().is_ok());
        s.terminal.scrollback_lines = 50_000;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn settings_json_roundtrip() {
        let s = default_settings();
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn settings_with_unknown_fields_loads() {
        // 关闭应用后手动编辑 JSON 可加未知字段，启动应兼容（向前兼容）
        let json = r#"{
            "version": 1,
            "appearance": "dark",
            "theme_id": "nord",
            "editor_theme_id": "nord",
            "terminal": { "font_family": "x", "font_size": 14, "line_height": 1.2, "scrollback_lines": 1000, "shell_detection": "auto" },
            "future_field": "ignored"
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.theme_id, "nord");
    }

    #[test]
    fn shell_detection_serializes() {
        let json = serde_json::to_string(&ShellDetection::Auto).unwrap();
        assert_eq!(json, "\"auto\"");
    }

    #[test]
    fn background_and_letter_spacing_ranges_enforced() {
        let mut s = default_settings();
        s.background.opacity = 1.1;
        assert!(s.validate().is_err());
        s.background.opacity = 0.5;
        s.terminal.letter_spacing = 9.0;
        assert!(s.validate().is_err());
    }
}
