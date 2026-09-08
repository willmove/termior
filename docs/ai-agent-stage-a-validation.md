# Stage A 验证指南：可靠 Agent Runtime

本文用于验证 `agent-stage-a-reliable-runtime` 的实现结果。自动化验证不需要真实模型或网络；手工验证需要在 Termior 设置中配置一个可用模型。

## 自动化验证

在仓库根目录运行：

```powershell
cargo test -p termior-ai -p termior-diff -p termior-security
cargo test -p termior-ai --test runtime_reliability
cargo test -p termior-ai --test security_redteam
cargo clippy --workspace --all-targets -- -D warnings
openspec validate agent-stage-a-reliable-runtime --strict --no-interactive
```

`runtime_reliability` 覆盖以下关键场景：

- 同一响应中的 `read_file`、`write_file`、`run_command` 完整排队，审批恢复后不重复执行
- 所有内置工具使用封闭 JSON Schema，缺少必填参数或多余参数时在审批前失败
- OpenAI、Anthropic、Google Provider 使用同一份工具契约
- 人工批准、拒绝、Yolo 与策略拒绝均保留决议来源
- 任务取消、未知副作用结果、跨审批累计预算和显式增加预算
- Provider 仅在首个响应增量前重试，收到增量后不重放请求
- 单文件和多文件 ChangeSet、部分 hunk 接受、磁盘基线冲突
- 工具输出上限与审批参数脱敏
- AcceptanceReport 的 verified、unverified、failed 推导，以及测试失败后修正并重新验证
- Task、Turn、TaskEvent、ToolInvocation 的序列化和随机状态转换
- 离线评测 JSON 包含完成、验证、工具调用、重复副作用、人工决议和安全拒绝指标

全部命令退出码为 `0`，且 Clippy 没有 warning，才满足 Stage A 自动化门禁。

## 手工验证

### 1. 工具审批与队列连续性

1. 启动 Termior 并打开一个测试仓库。
2. 使用 Auto 模式，让 Agent 先读取一个文件、修改该文件，再运行一个无破坏性的检查命令。
3. 确认审批卡显示工具名、稳定 call ID、规范化参数和副作用分类。
4. 拒绝一次命令。Agent 应收到对应 ToolResult 并继续处理后续调用，而不是直接结束任务。
5. 重试并批准。已完成的读取或写入提案不应重复执行。

### 2. 变更审阅闭环

1. 让 Agent 修改一个或多个文件。
2. 批准 `write_file` 后，磁盘内容应保持不变，并显示 AI Diff 审阅。
3. 对不同 hunk 分别选择接受和拒绝。
4. 提交审阅后，状态应先显示应用与验证过程；Agent 收到的结果应包含 `applied`、`rejected`、`conflicted`。
5. 如果没有配置验收命令，最终状态应明确显示 `unverified`，不能显示 verified。

### 3. 基线冲突保护

1. 在 AI Diff 等待审阅时，用编辑器或外部工具修改目标文件并保存。
2. 回到 Diff 选择接受。
3. Termior 应报告 conflict，目标文件必须保留用户刚保存的内容，不能被 Agent 覆盖。

### 4. 取消与预算

1. 启动一个需要较长模型响应或命令的任务。
2. 点击发送区域中的 Stop 按钮。
3. 状态应经过 cancelling，最终为 cancelled；如果操作系统无法确认副作用结果，则必须显示 unknown。
4. 当任务达到预算时，界面应显示具体预算项和已用/上限值。点击 “Continue (+25 steps / +15 min)” 后，累计步数不能归零。

### 5. Yolo 安全边界

1. 启用 Yolo 模式。
2. 尝试让 Agent 读取或写入 `.env`、SSH 私钥、工作区外路径。
3. 操作必须仍被 deny-list 或 workspace guard 拒绝；Yolo 只跳过人工审批。

## 结果判定

满足下列条件可判定 Stage A 完成：

- 自动化命令全部通过
- 工具调用没有丢失或重复副作用
- 审阅前不落盘，冲突时不覆盖用户内容
- 取消后不启动新调用
- verified 状态始终有 AcceptanceReport 证据
- UI 能区分 waiting、cancelled、unknown、completed-verified 和 completed-unverified
