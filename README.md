# Termior

> 终端优先的 AI 原生开发工作台（ADE）— 开源、跨平台、BYOK、本地优先、无遥测。
>
> Spec：[`docs/termior-spec.md`](docs/termior-spec.md)

本仓库当前包含 Spec 的 **P0 纯逻辑核心 + 单元测试**（8 个 Rust crate，216 个测试全绿）。
GPUI 渲染层（窗口/tab/终端渲染/编辑器视图/Composer UI）待后续里程碑在可编译验证环境接入。

## 架构（Cargo workspace）

```
crates/
  termior-security/        安全模型：deny-list / workspace 授权 / SSRF guard / 工具门控  (FR-SEC)
  termior-diff/            AI edit diff：hunk 级接受/拒绝 + unified patch 渲染            (FR-EDIT-04)
  termior-theme/           中央主题引擎 + 2 套内置主题                                    (FR-THEME)
  termior-store/           持久化 + 原子写 + schema 迁移 + 默认键位                       (FR-DATA/FR-SET)
  termior-terminal-core/   OSC 7/133/777 解析 + shell integration 注入                   (FR-TERM-06)
  termior-explorer-core/   fuzzy 查找 + glob/gitignore 纯判定                            (FR-EXPL)
  termior-ai/              Provider / Agent 循环 / 工具 / 审批 / 会话记忆 / 密钥接口      (FR-PROV/AGENT/SESS)
                           + keyring-backend feature（OS 钥匙串）+ 集成测试（E2E/红队）
  termior-hooks/           Claude Code hooks 安装器（幂等/原子/卸载）                     (FR-TAGENT)
  termior-app/             GPUI 入口：最小窗口挂载主题引擎（渲染层起点）                  (FR-WS/FR-THEME)
```

完整需求映射见 [`docs/p0-implementation-report.md`](docs/p0-implementation-report.md)。

## 构建

```bash
cargo build --workspace              # 纯逻辑 crate
cargo test  --workspace              # 249 单测 + 20 集成 passed
cargo build -p termior-app           # GPUI 渲染层（gpui warm 编译 ~5min）
cargo test  -p termior-ai --features keyring-backend   # OS 钥匙串后端
cargo clippy --workspace --all-targets --exclude termior-app
```

## 许可

Apache-2.0（见 Spec NFR-09）。
