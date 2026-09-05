//! Composer 提交模式（FR-AGENT-11）：三档互斥的审批策略。
//!
//! 语义见 `CONTEXT.md`「提交模式」与 `docs/adr/0004-yolo-auto-approve-at-gate.md`：
//! Yolo 只跳过人工审批（审批门自动应答 approve），不改变任何安全护栏。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// 只读工具自动执行；门控工具弹审批卡片等待人工决定。
    #[default]
    Auto,
    /// 先产出计划，确认前零写入；确认后执行阶段照常出审批卡。
    Plan,
    /// 门控工具自动批准（含 shell 执行）；安全护栏不变。
    Yolo,
}

impl Mode {
    /// 工具栏 chip 与下拉菜单显示的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Mode::Auto => "Auto",
            Mode::Plan => "Plan",
            Mode::Yolo => "Yolo",
        }
    }

    /// 下拉菜单里每档的一句话说明。
    pub fn description(self) -> &'static str {
        match self {
            Mode::Auto => "Approve gated tools manually",
            Mode::Plan => "Plan first, zero writes until confirmed",
            Mode::Yolo => "Auto-approve gated tools (incl. shell)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_auto_and_serde_uses_snake_case() {
        assert_eq!(Mode::default(), Mode::Auto);
        assert_eq!(serde_json::to_string(&Mode::Yolo).unwrap(), r#""yolo""#);
        assert_eq!(
            serde_json::from_str::<Mode>(r#""plan""#).unwrap(),
            Mode::Plan
        );
    }
}
