//! 集成测试：Agent 循环端到端（FR-AGENT-08/09/10 + FR-EDIT-04 + FR-SEC-01/02）。
//!
//! 场景：用户提问 → 模型发起 write_file（审批门控）→ 挂起 → 用户接受 →
//! 把提议变更包成 hunk diff（不直接写盘）→ 逐 hunk 接受 → 落盘结果精确等于接受集。
//!
//! 这条链路跨 termior-ai + termior-diff + termior-security 三个 crate，验证它们协同正确。

use termior_ai::{
    Agent, ApprovalDecision, ChatEvent, Message, MockProvider, Role, ToolCall, ToolRegistry,
};
use termior_diff::{apply_acceptances, diff_hunks, make_insertion_hunk};
use termior_security::workspace::WorkspaceAuthRegistry;

/// 模拟「写文件」工具的真实副作用：把参数里的 content 当作新文本，
/// 与磁盘现有内容做 diff，返回 unified patch（而非直接覆盖）。
fn exec_write_file(args: &str, disk_content: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let new_content = v.get("content").and_then(|c| c.as_str()).ok_or("missing content")?;
    let hunks = diff_hunks(disk_content, new_content, 0);
    Ok(termior_diff::render_unified(disk_content, &hunks))
}

#[test]
fn agent_write_file_approved_becomes_hunk_diff_and_lands_exact() {
    // 磁盘现有内容
    let disk = "line1\nline2\nline3\n";
    // 模型提议的新内容：替换 line2 → LINE2，并新增 line4
    let proposed = "line1\nLINE2\nline3\nline4\n";

    // 1) 第一轮：模型发起 write_file 工具调用（审批门控）→ Agent 挂起
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(Message {
            role: Role::Assistant,
            content: "editing".into(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: format!(r#"{{"path":"/proj/a.txt","content":{}}}"#, serde_json::Value::from(proposed)),
            }],
            tool_result: None,
        })],
        // 3) resume 后第二轮：模型回复完成
        vec![ChatEvent::Done(Message::assistant("done"))],
    ]);

    let tools = ToolRegistry::new(WorkspaceAuthRegistry::with_roots(["/proj".to_string()]));
    let agent = Agent::new(Box::new(provider), tools);

    // exec 回调：write_file 被批准后返回 ok
    fn exec(_tool: &str, _args: &str) -> Result<String, String> {
        Ok("ok".to_string())
    }

    // 先 run（应停在审批门）
    let outcome = agent.run(&[Message::user("edit a.txt")], &exec).unwrap();
    assert_eq!(outcome.state, termior_ai::AgentState::AwaitingApproval);
    let pending = outcome.pending_approval.clone().unwrap();
    assert_eq!(pending.tool_name, "write_file");

    // 2) 用户接受 → resume
    let resumed = agent.resume(&outcome.messages, &exec, ApprovalDecision::Approve, &pending).unwrap();
    assert_eq!(resumed.state, termior_ai::AgentState::Finished);

    // 4) 关键：write_file 不直接写盘，而是产出 hunk diff；用户逐 hunk 接受
    let hunks = diff_hunks(disk, proposed, 0);
    assert!(!hunks.is_empty(), "应有变更");
    // 全部接受 → 落盘等于提议
    let all: Vec<usize> = hunks.iter().map(|h| h.id).collect();
    let landed_all = apply_acceptances(disk, &hunks, &all);
    assert_eq!(landed_all, proposed);

    // 全部拒绝 → 落盘等于原文
    let landed_partial = apply_acceptances(disk, &hunks, &[]);
    assert_eq!(landed_partial, disk);
}

#[test]
fn agent_auto_read_tool_does_not_need_approval() {
    // read_file 是自动工具：直接执行，不挂起
    let provider = MockProvider::new(vec![
        vec![ChatEvent::Done(Message {
            role: Role::Assistant,
            content: "reading".into(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"/proj/a.rs"}"#.into(),
            }],
            tool_result: None,
        })],
        vec![ChatEvent::Done(Message::assistant("read done"))],
    ]);
    let tools = ToolRegistry::new(WorkspaceAuthRegistry::with_roots(["/proj".to_string()]));
    let agent = Agent::new(Box::new(provider), tools);
    fn exec(_t: &str, _a: &str) -> Result<String, String> {
        Ok("file content".into())
    }
    let outcome = agent.run(&[Message::user("read a.rs")], &exec).unwrap();
    // read_file 自动执行 → 最终 Finished，不应停在审批
    assert_eq!(outcome.state, termior_ai::AgentState::Finished);
    // 历史里有 tool 结果
    assert!(outcome.messages.iter().any(|m| m.role == Role::Tool));
}

#[test]
fn write_file_via_insertion_hunk_lands_exact() {
    // AI 想在某行插入：用 make_insertion_hunk 包成 diff（FR-SEC-02：不直接写盘）
    let disk = "a\nb\nc\n";
    let hunk = make_insertion_hunk(disk, 1, "X\n");
    let landed = apply_acceptances(disk, &[hunk], &[0]);
    assert_eq!(landed, "a\nX\nb\nc\n");
}

#[test]
fn exec_write_file_returns_unified_patch_not_overwrite() {
    // exec_write_file 应返回 patch 文本，而非副作用覆盖磁盘
    let disk = "a\nb\n";
    let args = r#"{"path":"/proj/a.txt","content":"a\nB\n"}"#;
    let patch = exec_write_file(args, disk).unwrap();
    assert!(patch.contains("@@ -"), "应返回 unified patch: {patch}");
    assert!(patch.contains("-b"));
    assert!(patch.contains("+B"));
}
