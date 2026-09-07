//! 审批数据模型（FR-AGENT-10 / FR-SEC-01）。
//!
//! 审批门控工具执行前，Agent 循环挂起等待用户决议（接受/拒绝），接受后自动续跑。
//! 真实机制为两阶段函数调用：[`crate::agent::Agent::run`] 返回
//! [`AgentOutcome`](crate::agent::AgentOutcome) 携带 `pending_approval`，UI 决议后调用
//! [`Agent::resume`](crate::agent::Agent::resume) 续跑。决策是 `resume` 的函数参数，
//! 不通过阻塞 channel 或自旋等待传递（GPUI 主线程不可阻塞）。

use serde::{Deserialize, Serialize};

/// 一次审批请求（展示精确参数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// 触发该请求的工具调用 id。
    pub call_id: String,
    pub tool_name: String,
    /// 参数（JSON 文本），原样展示给用户。
    pub arguments: String,
    /// 人类可读摘要（UI 用）。
    pub summary: String,
    /// Task-scoped model step count at the suspension point.
    #[serde(default)]
    pub steps: usize,
}

/// 用户决议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ApprovalRequest {
        ApprovalRequest {
            call_id: "c1".into(),
            tool_name: "write_file".into(),
            arguments: r#"{"path":"/proj/a.txt","content":"hi"}"#.into(),
            summary: "write /proj/a.txt".into(),
            steps: 1,
        }
    }

    #[test]
    fn decision_serializes() {
        let json = serde_json::to_string(&ApprovalDecision::Approve).unwrap();
        assert_eq!(json, "\"approve\"");
        let back: ApprovalDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApprovalDecision::Approve);
    }

    #[test]
    fn reject_decision_serializes_to_snake_case() {
        let json = serde_json::to_string(&ApprovalDecision::Reject).unwrap();
        assert_eq!(json, "\"reject\"");
        let back: ApprovalDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApprovalDecision::Reject);
    }

    #[test]
    fn request_carries_exact_arguments() {
        let req = sample();
        assert_eq!(req.tool_name, "write_file");
        assert!(req.arguments.contains("/proj/a.txt"));
        assert_eq!(req.summary, "write /proj/a.txt");
        assert_eq!(req.call_id, "c1");
    }

    #[test]
    fn request_round_trips_json() {
        let req = sample();
        let json = serde_json::to_string(&req).unwrap();
        let back: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }
}
