//! 审批网关（FR-AGENT-10 / FR-SEC-01）。
//!
//! 审批门控工具执行前，Agent 循环挂起等待用户决议（接受/拒绝），接受后自动续跑。
//! 真实实现为 `oneshot` channel 等待；本模块提供纯逻辑接口与数据结构。

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
}

/// 用户决议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

/// 审批网关：纯逻辑接口。真实实现持有 oneshot::Sender，UI 决议后 resolve。
///
/// 单测用同步决议：调用 [`ApprovalGate::resolve`] 后，[`ApprovalGate::await_decision`]
/// 立即返回。
pub struct ApprovalGate {
    request: ApprovalRequest,
    decision: std::sync::Mutex<Option<ApprovalDecision>>,
}

impl ApprovalGate {
    pub fn new(request: ApprovalRequest) -> Self {
        Self { request, decision: std::sync::Mutex::new(None) }
    }

    pub fn request(&self) -> &ApprovalRequest {
        &self.request
    }

    /// UI 决议写入。
    pub fn resolve(&self, decision: ApprovalDecision) {
        *self.decision.lock().unwrap() = Some(decision);
    }

    /// 等待决议（同步语义；真实实现阻塞在 oneshot 上）。
    pub fn await_decision(&self) -> ApprovalDecision {
        // 自旋等待（仅单测语义；真实用 oneshot::Receiver 阻塞）
        loop {
            if let Some(d) = *self.decision.lock().unwrap() {
                return d;
            }
            // 让出 CPU，避免忙等（单测中 resolve 在同一线程先于 await 调用）
            std::thread::yield_now();
        }
    }

    /// 是否已决议。
    pub fn is_resolved(&self) -> bool {
        self.decision.lock().unwrap().is_some()
    }
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
        }
    }

    #[test]
    fn approve_then_await() {
        let g = ApprovalGate::new(sample());
        g.resolve(ApprovalDecision::Approve);
        assert_eq!(g.await_decision(), ApprovalDecision::Approve);
        assert!(g.is_resolved());
    }

    #[test]
    fn reject_then_await() {
        let g = ApprovalGate::new(sample());
        g.resolve(ApprovalDecision::Reject);
        assert_eq!(g.await_decision(), ApprovalDecision::Reject);
    }

    #[test]
    fn await_blocks_until_resolved() {
        let g = std::sync::Arc::new(ApprovalGate::new(sample()));
        let g2 = g.clone();
        let handle = std::thread::spawn(move || g2.await_decision());
        // 稍后决议
        std::thread::sleep(std::time::Duration::from_millis(20));
        g.resolve(ApprovalDecision::Approve);
        assert_eq!(handle.join().unwrap(), ApprovalDecision::Approve);
    }

    #[test]
    fn decision_serializes() {
        let json = serde_json::to_string(&ApprovalDecision::Approve).unwrap();
        assert_eq!(json, "\"approve\"");
        let back: ApprovalDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ApprovalDecision::Approve);
    }

    #[test]
    fn request_carries_exact_arguments() {
        let g = ApprovalGate::new(sample());
        assert_eq!(g.request().tool_name, "write_file");
        assert!(g.request().arguments.contains("/proj/a.txt"));
    }
}
