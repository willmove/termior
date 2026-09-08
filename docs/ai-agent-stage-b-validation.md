# Stage B 验证指南：终端工作台与外部 Agent

Stage B 把一次性命令、后台命令、持久 PTY 和可见终端窗格统一成 `CommandSession`。同一个稳定 session ID 可用于读取增量输出、等待、终止、接管和交还控制权。

## 自动化验证

```powershell
cargo test -p termior-terminal-core
cargo test -p termior-terminal --test command_service
cargo test -p termior-agent-host --test backend_contract
cargo test -p termior-agent-host --test codex_smoke -- --ignored --nocapture
```

前三条完全离线。它们验证输出游标与截断、超时不杀进程、PTY 控制权、窗格注册、进程树终止、JSON-RPC 分帧、乱序/未知事件、稳定协议映射和审批 ID。最后一条要求本机 PATH 中存在兼容的 Codex CLI，只作为真实集成 smoke。

## 手工验证

1. 启动 Termior，打开一个测试项目和 Composer。
2. 点击 Composer 底部的 Agent 选择器，直到显示 `Codex app-server`。这里不需要 Termior 模型 API key；状态应说明认证和模型由 Codex 管理。
   如 Codex CLI 不在 PATH，可先设置 `TERMIOR_CODEX_PATH` 为完整可执行文件路径后启动 Termior。
3. 发送一个只读问题。应看到流式文本；再次发送问题时应复用同一后端会话。
4. 让 Codex 运行命令或修改文件。审批卡应显示 backend request ID、tool call ID、请求类型和脱敏参数。分别验证批准和拒绝。
5. 在长任务中点击 Stop。任务应结束为 cancelled；若后端退出而无法确认结果，应显示 unknown。
6. 让内置 Agent 获取活动终端上下文，再调用 `command_claim` 和 `command_write_input`。未批准或未持有 controller 时写入必须失败。Agent 持有控制权时，用户在该终端键入任意内容会立即 Take over，后续 Agent 写入必须失败，直到再次获批 `command_claim`。
7. 把鼠标移到 Agent chip 上。Codex 完成握手后，提示应列出认证/模型来源以及 resume、fork、steer、cancel、approval、terminal 的实际能力；未声明的能力显示 Unknown/Unsupported。

Codex 适配器只启用仓库中明确锁定和 fixture 验证过的 stable 方法。发现其他 CLI minor 版本时会拒绝启动，避免把未知协议行为伪装成已支持。
