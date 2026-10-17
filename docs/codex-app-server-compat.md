# Codex app-server 兼容性核验（Stage B 0.2）

核验日期：2026-10-11。对象：`crates/termior-agent-host/src/codex.rs` 中的 `CodexBackend` / `CodexProtocol`。

## 官方来源

- Codex App Server 文档：<https://developers.openai.com/codex/app-server>（现 308 跳转至 <https://learn.chatgpt.com/docs/app-server>）
- 版本对应的协议定义以本机 CLI 生成为准：`codex app-server generate-json-schema --out ./schemas` 或 `codex app-server generate-ts --out ./schemas`。官方说明生成物与运行它的 Codex 版本一一对应；文档没有给出正式的版本兼容承诺。

## 适配器使用的协议面与官方分级

| 适配器使用 | 官方状态（2026-10-11） | 备注 |
| --- | --- | --- |
| stdio transport（JSONL） | 默认 transport | WebSocket transport 官方标为 experimental/unsupported，适配器不使用 |
| `initialize` → `initialized` | 必需握手 | 适配器发送 `capabilities.experimentalApi = false`，因此服务端会拒绝任何实验方法/字段 |
| `thread/start`、`thread/resume` | 核心方法，不在实验清单 | `thread/fork` 存在但适配器未启用，CapabilityProfile 报告 fork unsupported |
| `turn/start`、`turn/interrupt` | 核心方法 | `turn/steer` 存在但未启用，mid-turn steer 报告 unsupported |
| 通知 `thread/started`、`turn/started`、`item/started`、`item/completed`、`item/agentMessage/delta`、`turn/completed` | 核心通知 | `turn/completed` 的状态为 `completed` / `interrupted` / `failed` |
| 审批 `item/commandExecution/requestApproval`、`item/fileChange/requestApproval` | 核心服务端请求 | 决议取值 `accept` / `acceptForSession` / `decline` / `cancel` |

官方实验清单（需 `experimentalApi`）包括 `thread/turns/list`、`thread/items/list`、`thread/backgroundTerminals/*`、`process/*`、`environment/info`、`dynamicTools`、`item/tool/call`、`tool/requestUserInput` 等；适配器一项都不使用。`plugin/*` 标为 under development。

## 已知差距

- 官方新增了 `item/permissions/requestApproval`（权限审批，响应 scope 为 `session` / `turn`）。适配器已把它映射为 `ApprovalRequested`（保留原始 request ID），不再静默挂起。Composer 审批卡列出每项请求的权限（默认勾选）；请求参数里若带 `scope` 会原样显示。批准时只回传勾选子集 `{permissions: <子集>, scope: "turn"}`，拒绝时回传 `{permissions: {}, scope: "turn"}`；无论服务端提议 `turn`、`session` 还是未知 scope，授权都只限当前 turn。`rust-v0.137.0` 与 `rust-v0.153.0` 的生成 schema 里，scope 在响应上，不在请求参数上。脱敏样本在 `crates/termior-agent-host/tests/fixtures/codex-permissions-request-approval/`：按这两个 tag 的 schema 与 `permissions.rs` 序列化规则重建，并用 `StreamingRedactor` 脱敏。构建该样本时环境里没有 `codex`，没有重新跑 `generate-json-schema`。
- 官方已标记弃用 `thread/rollback`、`thread/compacted`、`item/fileChange/outputDelta`；适配器都没有使用。

## 锁定策略

1. 只接受已验证的 CLI 版本线：`CodexProtocol::stable_verified()` 当前允许 `0.137.x` 与 `0.153.x`，其他版本在 `initialize` 前就以 `Unavailable` 拒绝。
2. 始终以 `experimentalApi = false` 握手，由服务端强制拒绝实验面，而不是依赖客户端自觉。
3. 新增版本线的流程：本机运行 `generate-json-schema`，对比上表方法与字段；把脱敏后的真实会话录制成 `crates/termior-agent-host/tests/fixtures/codex-<minor>-stable.jsonl`；回归测试通过后再把该 minor 加入 `stable_verified()`。
4. 真实 Codex smoke（`cargo test -p termior-agent-host --test codex_smoke -- --ignored`）默认 ignored。需要本机 PATH 上有 `codex-cli 0.137.x` 或 `0.153.x`，并在 CLI 要求时先 `codex login`。托管 runner 没有这份已登录 CLI，nightly 不运行它。手动步骤见 `docs/nightly-integration.md`。本次核验所在主机没有安装 `codex`，未重新执行。
