## Why

M6 解决 Agent 内部执行可靠性后，Termior 仍只把终端作为“当前 cwd + 末尾文本”快照。一次性命令固定超时，后台命令只返回 PID 且丢弃输出，持久 shell 缺少统一 wait/input/kill 语义。这样的工具无法可靠处理构建、服务器、调试器和 REPL，也无法让用户安全接管。

同时，Termior 若只依靠自研 Agent 循环，会长期追赶成熟 Agent 的会话、恢复和工具生态。M7 建立结构化终端服务，并通过同一任务模型接入首个结构化外部 Agent 后端。

## What Changes

- 将 Agent 启动的每个进程建模为稳定的命令会话，支持增量输出、等待、输入、resize、终止和释放
- 明确用户与 Agent 对交互式 PTY 的控制权切换，支持接管与交回
- 让终端失败命令、选区和输出范围成为有来源的任务上下文
- 新增不依赖 GPUI 的 `termior-agent-host` crate 与 `AgentBackend` 契约
- 首个外部后端接入 Codex app-server 的稳定 stdio 接口，映射会话、Turn、事件、审批和取消
- 普通 CLI/TUI Agent 保持 PTY 路径，只做能力明确的增强

## Capabilities

### New Capabilities

- `terminal-agent-workbench`：结构化命令生命周期、交互式控制权、任务绑定终端上下文、Agent 后端契约及首个外部后端

### Modified Capabilities

- 无；本 change 细化 FR-ATERM 与 FR-AEXT-01/02/04/08。

## Impact

- `termior-terminal-core`：命令记录、输出游标和控制权纯模型
- `termior-terminal`：进程/PTY session manager 与有界输出存储
- 新增 `termior-agent-host`：后端契约、子进程 transport、Codex app-server adapter
- `termior-ai`：内置运行时实现统一 `AgentBackend`
- `termior` UI：任务/终端关联、控制权和外部后端状态
- 设置增加可选外部后端 executable/config；凭据仍由对应后端或系统钥匙串管理

## Dependencies

- Blocked by `agent-stage-a-reliable-runtime` 全部退出标准。
- 不依赖 M8 的通用 ACP/MCP；Codex 适配器只使用已固定版本的稳定 stdio 方法。
