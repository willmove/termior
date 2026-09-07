## Why

M7 让任务和终端可控，但长会话仍会把完整消息、文件和终端文本反复发送；项目规则只从根目录选择一个文件；没有上下文来源/预算界面，也没有标准 Skills、MCP 或通用外部 Agent 协议。继续扩大工具集合会增加 token 成本、提示词注入面和后端耦合。

M8 建立有来源、有预算、可压缩的上下文系统，并以标准扩展边界接入 Skills、MCP、Hooks 和第二个 ACP Agent 后端。

## What Changes

- 将发给模型的信息建模为带来源、时间、作用域、敏感级别和保留策略的 Context Item
- 建立上下文预算分配、工具输出回收、结构化压缩和“本 Turn 实际发送内容”检查器
- 支持全局指令、根规则与目录级 `AGENTS.md`，以及基于 tree-sitter 的轻量仓库地图
- 引入可管理的记忆候选，不让模型静默写入永久事实
- 兼容 Agent Skills 的渐进加载规则
- 实现 MCP stdio / Streamable HTTP 客户端和统一策略入口
- 实现有界、确定性的生命周期 Hooks
- 实现 ACP adapter，并以 Gemini CLI 或 OpenCode 作为第二个真实后端验证

## Capabilities

### New Capabilities

- `agent-context-ecosystem`：上下文预算/压缩/来源、规则与记忆、仓库地图、Skills、MCP、Hooks、ACP 后端与能力协商

### Modified Capabilities

- 无；本 change 细化 FR-ACTX 与 FR-AEXT-03/05/06/07。

## Impact

- `termior-ai`：context assembler、compactor、rule resolver、repo map、memory、skills、MCP 与 hooks
- `termior-agent-host`：ACP transport/adapter 和扩展后的能力档案
- `termior-explorer-core` / tree-sitter 语法 crates：符号与引用索引
- `termior-store`：memory、skills/MCP metadata 和无凭据设置迁移
- Composer/任务详情：上下文检查器、扩展状态与诊断

## Dependencies

- Blocked by `agent-stage-b-terminal-workbench`。
- MCP 远端授权 token 必须使用现有 keyring 路径；不得写入 settings 或任务事件。
