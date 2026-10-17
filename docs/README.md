# 文档地图

`docs/` 分成三层。产品规格与用户说明在 `user/`，已采纳的架构决策在 `adr/`，实施与运维过程记录在 `process/`。阶段结束后的过程记录放进 `process/archive/`，不与仍在使用的 runbook 混放。

编码代理的操作约定仍在 `agents/`（`AGENTS.md` 直接指向这里）。NFR 门禁读取的数字在 `baselines/nfr-baselines.json`，路径被 CI 固定，说明见 `process/nfr-baselines.md`。

## 用户文档 `user/`

- [`user/termior-spec.md`](user/termior-spec.md)：权威产品与工程规格（SDD，v0.2 草案）。
- [`user/ssh.md`](user/ssh.md)：SSH / SFTP 的使用与验证说明。

## 架构决策 `adr/`

单上下文仓库的 ADR 目录，编号连续，不在此目录之外另建架构决策。

- [`adr/0002-remove-embedded-webview.md`](adr/0002-remove-embedded-webview.md)
- [`adr/0003-no-gpui-component.md`](adr/0003-no-gpui-component.md)
- [`adr/0004-yolo-auto-approve-at-gate.md`](adr/0004-yolo-auto-approve-at-gate.md)
- [`adr/0005-hybrid-agent-host-architecture.md`](adr/0005-hybrid-agent-host-architecture.md)
- [`adr/0006-openssh-sessions-and-sftp.md`](adr/0006-openssh-sessions-and-sftp.md)
- [`adr/0007-i18n-embedded-json-tables.md`](adr/0007-i18n-embedded-json-tables.md)

领域词汇在仓库根目录的 `CONTEXT.md`。

## 过程记录 `process/`

仍在使用的 runbook、路线和阶段验收：

- [`process/release-process.md`](process/release-process.md)：GitHub Release 发布流程。已发布说明的归档位置见该文第 7 节。
- [`process/nightly-integration.md`](process/nightly-integration.md)：ignored 集成测试与 nightly 步骤。
- [`process/nfr-baselines.md`](process/nfr-baselines.md)：NFR harness 与回归门禁。基线数字仍在 [`baselines/nfr-baselines.json`](baselines/nfr-baselines.json)。
- [`process/desktop-milestone.md`](process/desktop-milestone.md)：当前桌面交付边界。
- [`process/ai-agent-evolution-roadmap.md`](process/ai-agent-evolution-roadmap.md)：Agent M6–M10 实施索引。
- [`process/ai-agent-implementation-status.md`](process/ai-agent-implementation-status.md)：A–E 的实现证据与平台边界。勾选清单以各 OpenSpec `tasks.md` 为准。
- [`process/codex-app-server-compat.md`](process/codex-app-server-compat.md)：Codex app-server 兼容性核验。
- [`process/ai-agent-stage-b-validation.md`](process/ai-agent-stage-b-validation.md) 至 [`process/ai-agent-stage-e-validation.md`](process/ai-agent-stage-e-validation.md)：进行中阶段的验收步骤。

### 已结束的过程记录 `process/archive/`

- [`process/archive/p0-implementation-report.md`](process/archive/p0-implementation-report.md)：P0 纯逻辑阶段的历史快照。
- [`process/archive/ai-agent-stage-a-validation.md`](process/archive/ai-agent-stage-a-validation.md)：Stage A 验收。对应 change 已归档。
- [`process/archive/zed-embedded-webview-research.md`](process/archive/zed-embedded-webview-research.md)：一次性调研。决策已落入 ADR 0002。
- [`process/archive/volc-sh-claude-session-fix-bugs-20260718.md`](process/archive/volc-sh-claude-session-fix-bugs-20260718.md)：2026-07-18 的一次性排障记录。
