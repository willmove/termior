## Why

当前 Agent 已能流式响应、调用工具、等待一次审批并继续，也能生成 hunk diff；但执行生命周期仍由 Composer 临时状态承载。一次响应包含多个工具调用时，遇到首个审批会提前返回；修改提案与最终落盘之间没有继续执行验证的闭环；工具参数只向部分 Provider 声明通用 object；后台任务也无法取消。继续增加模型、MCP 或子代理会放大这些不确定状态。

M6 先建立可靠单代理基线，使一次任务的每个动作都能被识别、排队、批准、执行、验证和终止。

## What Changes

- 新增独立于 GPUI 的任务、Turn、运行事件、工具调用与验收状态模型
- 用显确工具状态机替代“返回一个 pending approval”的单槽流程，一次模型响应中的全部调用均被保存
- 为所有内置工具提供严格 JSON Schema、副作用分类、审批策略、超时与输出限制
- 引入任务取消、累计步数/时间/用量预算和有限的 Provider 重试语义
- 把变更集审阅纳入原 Turn：审阅结果后向 Agent 回填真实应用结果，再执行验收命令
- 建立不访问外网的固定 Agent 评测 harness，记录完成、验证、干预和安全指标

## Capabilities

### New Capabilities

- `reliable-agent-runtime`：任务/Turn/事件模型、完整工具队列、严格工具契约、取消/预算、变更后验证与本地评测基线

### Modified Capabilities

- 无；`openspec/specs/` 尚无已归档 Agent capability。本 change 细化 `docs/termior-spec.md` 的 FR-ARUN、FR-ACHG-01/02/03 与 FR-AORCH-07。

## Impact

- `crates/termior-ai`：新增 task/runtime/event/tool-contract/evaluation 模块；重构 `Agent::drive` 和审批恢复路径
- `crates/termior-diff`：变更集基线与冲突检测所需纯函数
- `crates/termior-store`：M6 只保存兼容的任务摘要；完整追加事件日志留到 M9
- `crates/termior/src/composer_view.rs`：从拥有执行状态改为订阅任务状态并发送 command
- Provider 请求结构与测试 fixture 会变化，但现有设置和 API key 存储格式不变

## Dependencies

- 无新增阶段依赖；必须保持现有 FR-SEC、ADR 0004 和用户未提交改动安全。
- 本 change 完成后才能开始 `agent-stage-b-terminal-workbench`。
