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
  termior-terminal/        PTY 会话（portable-pty）+ reader 线程 + channel 桥接          (FR-TERM-01)
  termior-explorer-core/   fuzzy 查找 + glob/gitignore 纯判定                            (FR-EXPL)
  termior-ai/              Provider / Agent 循环 / 工具 / 审批 / 会话记忆 / 密钥接口      (FR-PROV/AGENT/SESS)
                           + keyring-backend feature（OS 钥匙串）+ 集成测试（E2E/红队）
  termior-hooks/           Claude Code hooks 安装器（幂等/原子/卸载）                     (FR-TAGENT)
  termior-app/             GPUI 入口：主题引擎 + 第一个真终端 tab（PTY→Term→canvas 渲染） (FR-WS/FR-THEME/FR-TERM)
```

完整需求映射见 [`docs/p0-implementation-report.md`](docs/p0-implementation-report.md)。

## 构建

纯逻辑核心（macOS / Linux / Windows 三平台，stable 工具链）：

```bash
cargo build --workspace --exclude termior-app --exclude termior-terminal  # 纯逻辑 crate
cargo test  --workspace --exclude termior-app --exclude termior-terminal  # 单测 + 集成
cargo fmt --all -- --check                                              # 格式校验
cargo clippy --workspace --all-targets \
    --exclude termior-app --exclude termior-terminal -- -D warnings
cargo deny check --exclude termior-app --exclude termior-terminal       # 许可核验（NFR-09）
```

M1 终端（运行时重型依赖 portable-pty + alacritty_terminal + GPUI）：

```bash
cargo build -p termior-terminal      # PTY 层（crates.io portable-pty 0.9 + alacritty_terminal 0.26）
cargo test  -p termior-terminal      # PTY 回环测试
cargo build -p termior-app           # GPUI 终端渲染层（gpui warm 编译 ~5min）
cargo run   -p termior-app           # 启动窗口 → 点「打开终端」spawn 第一个 PTY 终端 tab
```

可选 feature（默认关闭）：

```bash
cargo test  -p termior-ai --features keyring-backend  # OS 钥匙串后端（FR-PROV-04）
```

CI（`.github/workflows/ci.yml`）在 macOS/Linux/Windows 三平台并行跑纯逻辑门禁；
`audit.yml` 用 cargo-deny 做 weekly 许可与漏洞巡检。详见 spec §3.1 / NFR-09 / R6。

## 许可

Apache-2.0（见 Spec NFR-09）。
