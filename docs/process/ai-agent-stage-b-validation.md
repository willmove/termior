# Stage B 验证指南：终端工作台与外部 Agent

Stage B 把一次性命令、后台命令、持久 PTY 和可见终端窗格统一成 `CommandSession`。同一个稳定 session ID 可用于读取增量输出、等待、终止、接管和交还控制权。

## 自动化验证

```powershell
cargo test -p termior-terminal-core
cargo test -p termior-terminal --test command_service
cargo test -p termior-agent-host --test backend_contract
cargo test -p termior-agent-host --test codex_smoke -- --ignored --nocapture
```

前三条完全离线。它们验证输出游标与截断、超时不杀进程、PTY 控制权、窗格注册、进程树终止、JSON-RPC 分帧、乱序/未知事件、稳定协议映射和审批 ID。最后一条要求 `TERMIOR_CODEX_PATH` 或 PATH 中存在可运行的稳定 Codex CLI（`>=0.137.0, <1.0.0`），只作为真实握手 smoke，不发起模型 Turn；本机 `0.164.0` 已于 2026-10-26 通过。托管 runner 不提供这份 CLI，nightly 不运行它；手动步骤见 `docs/process/nightly-integration.md`。

## 桌面复验（可由自动驱动执行）

2026-10-26 已用隔离数据目录、本地 SSE fixture 和真实 GPUI 窗口代做长构建、后台服务、审批、REPL 接管/交还与审批中崩溃恢复，并补充真实进程取消、跨任务隔离与 owner 崩溃清理回归。复现、修复和覆盖边界见 [自动执行记录](agent-acceptance-2026-10-26.md)。这些操作不再要求维护者重复点击。真实 Codex 模型读取另被本机 Windows sandbox `CryptUnprotectData failed: 2148073483` 阻塞；版本范围放宽与握手通过不能代替该读取验收。

1. 启动 Termior，打开一个测试项目和 Composer。
2. 点击 Composer 底部的 Agent 选择器，直到显示 `Codex app-server`。这里不需要 Termior 模型 API key；状态应说明认证和模型由 Codex 管理。
   如 Codex CLI 不在 PATH，可先设置 `TERMIOR_CODEX_PATH` 为完整可执行文件路径后启动 Termior。
3. 发送一个只读问题。应看到流式文本；再次发送问题时应复用同一后端会话。
4. 让 Codex 运行命令或修改文件。审批卡应显示 backend request ID、tool call ID、请求类型和脱敏参数。分别验证批准和拒绝。
5. 在长任务中点击 Stop。任务应结束为 cancelled；若后端退出而无法确认结果，应显示 unknown。
6. 让内置 Agent 获取活动终端上下文，再调用 `command_claim` 和 `command_write_input`。未批准或未持有 controller 时写入必须失败。Agent 持有控制权时，用户在该终端键入任意内容会立即 Take over，后续 Agent 写入必须失败，直到再次获批 `command_claim`。
7. 把鼠标移到 Agent chip 上。Codex 完成握手后，提示应列出认证/模型来源以及 resume、fork、steer、cancel、approval、terminal 的实际能力；未声明的能力显示 Unknown/Unsupported。

Codex 适配器只启用已用 fixture 验证过的 stable 方法，握手保持 `experimentalApi = false`。兼容范围内的稳定 minor 更新不会被版本白名单阻断；实际协议连接仍须成功握手。
