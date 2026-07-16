//! 模型选择器数据结构（FR-PROV-03）。
//!
//! 当前 Provider 模型注册表 + 收藏 + 最近；默认聊天模型与默认补全模型独立设置。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Provider 种类（FR-PROV-01 / FR-PROV-02）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// 云：Anthropic / OpenAI / Google / Groq / xAI / Cerebras / OpenRouter / DeepSeek / Mistral / OpenAI-compatible
    Anthropic,
    OpenAi,
    Google,
    Groq,
    Xai,
    Cerebras,
    OpenRouter,
    DeepSeek,
    Mistral,
    OpenAiCompatible,
    /// 本地：LM Studio / MLX / Ollama
    LmStudio,
    Mlx,
    Ollama,
}

impl ProviderKind {
    pub fn from_settings_id(id: &str) -> Option<Self> {
        Some(match id {
            "anthropic" => Self::Anthropic,
            "open_ai" | "openai" => Self::OpenAi,
            "google" | "gemini" => Self::Google,
            "groq" => Self::Groq,
            "xai" => Self::Xai,
            "cerebras" => Self::Cerebras,
            "open_router" | "openrouter" => Self::OpenRouter,
            "deep_seek" | "deepseek" => Self::DeepSeek,
            "mistral" => Self::Mistral,
            "open_ai_compatible" | "compatible" => Self::OpenAiCompatible,
            "lm_studio" | "lm-studio" => Self::LmStudio,
            "mlx" => Self::Mlx,
            "ollama" => Self::Ollama,
            _ => return None,
        })
    }
}

/// 一个模型条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub provider: ProviderKind,
    pub display_name: String,
    /// 是否支持工具调用。
    pub supports_tools: bool,
}

/// 模型注册表：可用模型 + 收藏 + 最近使用 + 默认聊天/补全。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub favorites: HashSet<String>,
    #[serde(default)]
    pub recent: Vec<String>,
    /// 默认聊天模型 id（FR-PROV-03 独立于补全）。
    pub default_chat_model: Option<String>,
    /// 默认补全模型 id（FR-EDIT-05，P1）。
    #[serde(default)]
    pub default_completion_model: Option<String>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        // P0 首发：Anthropic + OpenAI + OpenAI-compatible 各一个示例模型
        let models = vec![
            ModelEntry {
                id: "claude-sonnet-4".into(),
                provider: ProviderKind::Anthropic,
                display_name: "Claude Sonnet 4".into(),
                supports_tools: true,
            },
            ModelEntry {
                id: "gpt-4o".into(),
                provider: ProviderKind::OpenAi,
                display_name: "GPT-4o".into(),
                supports_tools: true,
            },
            ModelEntry {
                id: "local-default".into(),
                provider: ProviderKind::OpenAiCompatible,
                display_name: "OpenAI-compatible (local)".into(),
                supports_tools: false,
            },
        ];
        Self {
            models,
            favorites: HashSet::new(),
            recent: vec![],
            default_chat_model: None,
            default_completion_model: None,
        }
    }
}

impl ModelRegistry {
    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// 记录一次模型使用（推进到 recent 首位，去重，保留最多 N 条）。
    pub fn touch_recent(&mut self, model_id: &str) {
        self.recent.retain(|m| m != model_id);
        self.recent.insert(0, model_id.to_string());
        if self.recent.len() > 10 {
            self.recent.truncate(10);
        }
    }

    pub fn toggle_favorite(&mut self, model_id: &str) -> bool {
        if self.favorites.contains(model_id) {
            self.favorites.remove(model_id);
            false
        } else {
            self.favorites.insert(model_id.to_string());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_p0_providers() {
        let r = ModelRegistry::default();
        let providers: Vec<_> = r.models.iter().map(|m| m.provider).collect();
        assert!(providers.contains(&ProviderKind::Anthropic));
        assert!(providers.contains(&ProviderKind::OpenAi));
        assert!(providers.contains(&ProviderKind::OpenAiCompatible));
    }

    #[test]
    fn touch_recent_dedups_and_caps() {
        let mut r = ModelRegistry::default();
        for i in 0..15 {
            r.touch_recent(&format!("m{i}"));
        }
        assert_eq!(r.recent.len(), 10);
        assert_eq!(r.recent[0], "m14");
    }

    #[test]
    fn toggle_favorite() {
        let mut r = ModelRegistry::default();
        assert!(r.toggle_favorite("gpt-4o"));
        assert!(r.favorites.contains("gpt-4o"));
        assert!(!r.toggle_favorite("gpt-4o"));
    }

    #[test]
    fn default_chat_and_completion_independent() {
        let r = ModelRegistry {
            default_chat_model: Some("claude-sonnet-4".into()),
            default_completion_model: Some("gpt-4o".into()),
            ..Default::default()
        };
        assert_ne!(r.default_chat_model, r.default_completion_model);
    }
}
