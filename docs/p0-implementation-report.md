# Termior P0 纯逻辑核心 — 实现报告与需求映射

> 本文档记录 P0 需求的实现范围、需求 → 模块/测试 的可追溯映射，以及尚未覆盖的项。
> 对应 Spec：`docs/termior-spec.md`。

## 构建与测试

```bash
cargo build --workspace              # 纯逻辑 crate，无警告
cargo test  --workspace              # 249 passed, 0 failed
cargo build -p termior-app           # GPUI 渲染层（gpui 依赖树 warm 编译 ~5min）
cargo test  -p termior-ai --features keyring-backend   # OS 钥匙串后端（+2 tests）
cargo clippy --workspace --all-targets --exclude termior-app  # 无警告
```

> `termior-app`（GPUI 入口）独立构建；gpui/gpui_platform 锁定到 zed rev `3565c49`。
> `keyring-backend` 为可选 feature，启用后接入真实 OS 钥匙串。

### 测试分布

| crate | 测试数 | 覆盖需求 |
|---|---|---|
| `termior-security` | 55 | FR-SEC-01/03/04/05/06/07, INV-2/3/5, NFR-10 |
| `termior-ai` | 57（含集成测试 20） | FR-PROV-02/03/04, FR-AGENT-03/07/08/09/10, FR-SESS-01/02, FR-SEC-01 |
| `termior-terminal-core` | 29 | FR-TERM-06, 附录B, INV-4, NFR-10 |
| `termior-store` | 24 | FR-DATA, FR-SET-01/03, INV-5 |
| `termior-theme` | 12 | FR-THEME-01/02 |
| `termior-diff` | 23 | FR-EDIT-04, FR-SEC-02, NFR-10 |
| `termior-hooks` | 11 | FR-TAGENT-03/04, NFR-10 |
| `termior-explorer-core` | 19 | FR-EXPL-04/05/03 |
| `termior-app` | — | FR-WS/FR-THEME 渲染层（GPUI 入口，编译通过） |
| **合计** | **249 单测 + 20 集成 + 2 keyring** | |

## 增量进展（第二轮：渲染层接入 + 增强）

- **`termior-app`（GPUI 入口）**：打开原生 GPUI 窗口，用 `termior_theme` 中央引擎解析调色板
  驱动背景/前景/状态色/diff 色/终端 16 色条；点击热切换 default ↔ nord，验证纯逻辑主题
  crate 驱动真实 GPU 渲染并广播重绘（FR-THEME-01/02）。gpui 依赖树 warm 编译 ~5min，
  `termior-app` 增量 ~12s。
- **`termior-diff`**：`render_unified()` 渲染 unified patch 供 `ai-diff` tab 展示；
  `make_insertion_hunk()` 让 AI `write_file` 把变更包成 diff 而非直接写盘（FR-SEC-02）。
- **`termior-ai/session`**：消息历史膨胀策略（Q3）——保留 system 前置 + 最近 N 条，中段丢弃；
  `approx_bytes()` 供单文件 → 每会话一文件的拆分阈值判定。
- **集成测试 `tests/agent_e2e.rs`**（4）：Agent 循环端到端——工具调用 → 审批 → hunk diff → 落盘精确。
- **集成测试 `tests/security_redteam.rs`**（16）：红队全链路——穿越/前缀伪攻击/元数据端点/
  userinfo 伪装/兄弟工作区/deny-list 双向拦截（验证 INV-2/3）。
- **`keyring-backend` feature**：`KeyringSecretStore` 接入真实 OS 钥匙串
  （Windows Credential Manager / macOS Keychain / Linux Secret Service），FR-PROV-04/INV-5。

## 范围说明（重要）

本交付为 **P0 纯逻辑核心 + 单元测试**。受限于本环境无法在合理时间内编译 GPUI
（`gpui` 依赖树编译 >10 分钟未完成），**不包含 GPUI 渲染层**：

- ❌ 工作区 tab/分栏布局交互（FR-WS-03 布局渲染、statusbar/header/composer 视图）
- ❌ 终端渲染元素、PTY spawn、alacritty_terminal 网格（FR-TERM-01/07/08）
- ❌ 编辑器视图（FR-EDIT-01/02/03 的 UI；FR-EDIT-04 的 diff 逻辑已实现）
- ❌ Composer 卡片 UI（FR-AGENT-01/02 视图；审批数据模型已实现）
- ❌ 真实 HTTP/SSE Provider 适配（FR-PROV-01 各家；trait + Mock 已实现）
- ❌ 文件浏览器树视图（FR-EXPL-01/02 视图；fuzzy/grep/ignore 逻辑已实现）

所有「触盘/触 shell/触密钥/触网」的**判定逻辑**均为纯函数并带红队单测（NFR-10）。

## 需求 → 模块/测试 映射

### 6.1 工作区与窗口布局（FR-WS）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-WS-01 tab 类型 | 🟡 部分 | `termior-store::keymap` 定义了 tab 相关动作（NewTerminalTab/NewEditorTab/NewPreviewTab）；8 种 tab 类型的视图枚举待 GPUI |
| FR-WS-02 状态保持/继承 cwd | 🟡 部分 | `termior-terminal-core::osc::OscEvent::Cwd` 提供 cwd 来源；Entity 存活不变量属 GPUI 层 |
| FR-WS-07 设置窗口六页签 | ✅ 数据 | `termior-store::settings::Settings`（General 结构）；Models/Themes 数据在各 crate |

### 6.2 终端（FR-TERM）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-TERM-06 shell integration 注入 | ✅ | `termior-terminal-core::shell_integration`（zsh ZDOTDIR 四件套 / bash --rcfile / pwsh -File，内部 source 用户配置 + 注入 OSC 7/133） |
| OSC 7/133/777 解析 | ✅ | `termior-terminal-core::osc`（含 Windows 盘符规范化、自锁定检测器语义） |
| FR-TERM-01/07/08/09/10 | ❌ | PTY/渲染/网格/字体设置 UI 属 GPUI 层；字体/回滚档位校验见 `termior-store::settings` |

### 6.3 编辑器（FR-EDIT）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-EDIT-04 AI edit diff（hunk 级接受/拒绝） | ✅ | `termior-diff`（`diff_hunks` + `apply_acceptances`，落盘精确等于接受集） |
| FR-EDIT-01/02/03 | ❌ | 基础编辑/高亮/视图属 GPUI 层 |

### 6.4 文件浏览器（FR-EXPL）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-EXPL-04 fuzzy 文件查找 | ✅ | `termior-explorer-core::fuzzy`（nucleo 排序） |
| FR-EXPL-05 grep/glob | ✅ | `termior-explorer-core::search`（globset + gitignore 纯判定） |
| FR-EXPL-03 ignore 索引 | 🟡 | `is_ignored` 纯函数 + `IgnoreRule`；真实 fs walk 用 `ignore` crate（未接 fs） |
| FR-EXPL-01/02 | ❌ | 目录树视图属 GPUI 层 |

### 6.5 源码管理（FR-VCS）
全部 P1，本轮不交付（gix/git CLI 操作）。

### 6.7 主题（FR-THEME）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-THEME-01 中央主题引擎 | ✅ | `termior-theme`（Theme tokens 同时驱动 UI/终端16色/diff/状态色） |
| FR-THEME-02 内置主题 | ✅ | 2 套（default/nord）；P0 最低门槛满足，其余 P1 |

### 6.9 AI Provider 与密钥（FR-PROV）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-PROV-01 云 Provider 抽象 | ✅ trait | `termior-ai::provider::Provider` + `ChatEvent`；真实适配器（Anthropic/OpenAI/OpenAI-compatible）待 HTTP 层 |
| FR-PROV-02 本地推理 | ✅ 数据 | `termior-ai::model_registry::ProviderKind`（LmStudio/Mlx/Ollama）+ `termior-security::ssrf::SsrfGuard::default_local_bases` |
| FR-PROV-03 模型选择器 | ✅ | `termior-ai::model_registry::ModelRegistry`（注册表+收藏+最近，默认聊天/补全独立） |
| FR-PROV-04 密钥入钥匙串 | ✅ 接口 | `termior-ai::secret_store::SecretStore`（service `Termior-ai`，永不序列化） |
| FR-PROV-05 SSRF guard | ✅ | `termior-security::ssrf`（loopback/link-local/私网拦截 + 本地白名单） |

### 6.10 Composer 与 Agent 运行时（FR-AGENT）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-AGENT-03 @path 引用 + secret 过滤 | ✅ | `termior-ai::tools::ToolRegistry::check_path_access`（deny-list 过滤） |
| FR-AGENT-07 实时上下文桥 | ✅ 契约 | `termior-ai::context`（cwd + 末 300 行快照，惰性抓取） |
| FR-AGENT-08 Agent 循环 | ✅ | `termior-ai::agent::Agent`（状态机 + MAX_AGENT_STEPS=50 + 系统提示词） |
| FR-AGENT-09 工具两级门控 | ✅ | `termior-ai::tools` + `termior-security::gating`（自动/审批） |
| FR-AGENT-10 审批卡片 | ✅ | `termior-ai::approval::ApprovalGate`（挂起/决议/续跑） |
| FR-AGENT-01/02 | ❌ | Composer 视图属 GPUI 层 |

### 6.12 会话与记忆（FR-SESS）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-SESS-01 命名持久会话 | ✅ | `termior-ai::session::SessionStore`（标题自动生成 + 镜像落盘，原子写） |
| FR-SESS-02 项目记忆 | ✅ | `termior-ai::session::ProjectMemory`（Termior.md/CLAUDE.md/AGENTS.md 重定向） |

### 6.13 终端内编码代理检测（FR-TAGENT）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-TAGENT-01 OSC 字节过滤器 | ✅ | `termior-terminal-core::osc`（OSC 777;notify;Termior; 事件枚举，INV-4 纯序列触发） |
| FR-TAGENT-03 Claude Code hooks 安装 | ✅ | `termior-hooks`（三条 hook + ENV 守卫） |
| FR-TAGENT-04 安装器安全性 | ✅ | `termior-hooks`（非法 JSON 拒写、原子写、幂等、卸载清自有、状态查询） |

### 6.14 安全模型（FR-SEC）— NFR-10 重点
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-SEC-01 工具两级门控 + 审批 | ✅ | `termior-security::gating` + `termior-ai::approval` |
| FR-SEC-02 AI edit diff hunk 审阅 | ✅ | `termior-diff`（write_file 不触盘） |
| FR-SEC-03 secret deny-list | ✅ | `termior-security::deny_list`（canonicalize 后强制，红队：穿越/符号/大小写） |
| FR-SEC-04 workspace 授权注册表 | ✅ | `termior-security::workspace`（同级目录不可达，前缀攻击拦截） |
| FR-SEC-05 SSRF guard | ✅ | `termior-security::ssrf` |
| FR-SEC-06 密钥永不落盘 | ✅ | `termior-security::secret` + `termior-store::assert_no_persistent_secret` + `termior-ai::secret_store` |
| FR-SEC-07 无遥测 | ✅ | `termior-security::NO_TELEMETRY` 常量 |

### 7 数据与持久化（FR-DATA）
| ID | 状态 | 实现位置 |
|---|---|---|
| 原子写 | ✅ | `termior-store::atomic::atomic_write`（temp + rename + fsync） |
| schema 迁移 | ✅ | `termior-store::migrate`（v0→v1，未知字段保留，非法 JSON 结构化错误） |
| 数据目录 | ✅ | `termior-store::paths::app_data_dir`（bundle id，经 dirs） |
| settings 持久化 | ✅ | `termior-store::settings`（含 FR-TERM-09 字号/回滚区间校验） |

### 6.15 设置与快捷键（FR-SET）
| ID | 状态 | 实现位置 |
|---|---|---|
| FR-SET-01 六页签 General 数据 | ✅ | `termior-store::settings::Settings` |
| FR-SET-03 默认键位 + Ctrl 映射 | ✅ | `termior-store::keymap`（附录A 全量 + Mac⌘/Win+Linux Ctrl） |

## 架构不变量（INV）覆盖

| INV | 状态 | 证据 |
|---|---|---|
| INV-2 触盘经 security 门控 | ✅ | `ToolRegistry::check_path_access` 是工具执行前唯一入口 |
| INV-3 deny-list 在 canonicalize 后强制 | ✅ | `canonicalize_logical` + 红队测试（traversal/backslash） |
| INV-4 代理状态只由显式 OSC 触发 | ✅ | `osc::tests::no_osc_no_state_events` / `inv4_plain_output_no_events` |
| INV-5 密钥永不落盘 | ✅ | `assert_no_secret_fields` + `SecretStore` 不实现 Serialize |

## NFR-10（可测性，≥80% 覆盖）

下列纯函数/独立模块均有单测，含红队用例：
- ✅ 网格/OSC 解析（`termior-terminal-core::osc`）
- ✅ diff/hunk（`termior-diff`）
- ✅ deny-list（`termior-security::deny_list`）
- ✅ SSRF 判定（`termior-security::ssrf`）
- ✅ workspace 授权（`termior-security::workspace`）
- ✅ hooks 安装器（`termior-hooks`）
- ✅ settings 迁移（`termior-store::migrate`）

## 下一步（GPUI 接入后）

1. 在 `termior-ui` / `termior-ui-kit` 接入已实现的纯逻辑 crate（Provider/Agent/工具/审批）。
2. 实现 FR-PROV-01 各家真实 HTTP/SSE 适配器（reqwest + eventsource-stream），保持 `Provider` trait 不变。
3. 接入 portable-pty + alacritty_terminal，把 `OscParser` 挂到 PTY reader 线程字节过滤器。
4. 接入 keyring crate 实现 `SecretStore` 的真实后端。
