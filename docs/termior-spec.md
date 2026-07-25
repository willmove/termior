# Termior — 终端优先 AI 原生开发工作台（ADE）SDD 规格说明书

> **版本**：v0.1 草案 · 2026-07
> **架构基线**：Rust + GPUI（单进程原生 GPU 渲染；Web 预览统一交给系统浏览器，详见 ADR 0002）
> **产品代号**：`Termior`（占位名，可全局替换；对应 bundle id、配置文件名、记忆文件名等均随之替换）

---

## 0. 文档用途与 SDD 使用说明

本文档是 Spec-Driven Development（SDD）流程的顶层规格（Spec），供 Claude Code 等编码代理消费。使用约定：

- 功能需求编号 `FR-<模块>-<序号>`，非功能需求编号 `NFR-<序号>`；每条需求标注优先级 **P0**（MVP 必须）/ **P1**(正式版必须) / **P2**（增强项）。
- 每个模块给出「验收标准」，作为 `/tasks` 拆分与验收测试的依据。
- 「技术要点」小节记录 GPUI 侧的实现注记与决策，属于 plan 层信息，编码时优先遵循；若实现中发现更优方案，允许偏离但必须在 PR 描述中记录偏离原因。
- 第 9 节里程碑定义了增量交付顺序，SDD 任务拆分应按里程碑分批进行，不要一次性生成全量任务。
- 本项目从零开始设计与实现；产品行为、模块边界和验收标准均以本 Spec 为唯一依据。引入第三方 crate、字体或图标资产时，必须核验其许可并保留必要声明。

---

## 1. 产品概述

### 1.1 一句话定位

Termior 是一款开源、跨平台、**终端优先**的 AI 原生开发工作台（ADE, Agentic Development Environment）：在单一原生窗口中集成真 PTY 终端、代码编辑器、文件浏览器、Git 源码管理、Web 预览，以及一个具备工具调用与审批流的 Agentic AI 侧栏。BYOK（自带 API Key），支持完全本地推理，无遥测、无账号。

### 1.2 产品形态与架构原则

Termior 是一个从零开始设计的新项目，产品能力、交互模型与技术架构均由本 Spec 独立定义。核心形态如下：

- **单进程原生应用**：核心 UI 由 GPUI 直接渲染，终端仿真、编辑器、AI Agent 运行时均以原生 Rust 模块运行，通过进程内 async channel 与 GPUI Entity 状态模型通信；
- **终端优先工作流**：终端是工作区的主要交互面，编辑器、文件浏览器、Git、预览和 AI 围绕活动终端的 cwd 与上下文协同；
- **状态持续存在**：tab 切换只改变可见性，不销毁 PTY、编辑缓冲、undo 栈或预览页面状态；
- **按需隔离 Web 内容**：核心工作区不依赖 WebView；Web 预览统一交给系统浏览器打开（ADR 0002）。

### 1.3 核心价值主张（必须写进 README）

1. **原生低延迟**：核心 UI 无 JS 运行时与 IPC 序列化边界，终端和编辑器直接使用原生 GPU 渲染，常态目标 60 fps，高刷屏目标 120 fps；
2. **终端与 AI 深度协同**：真 PTY、实时 cwd/缓冲上下文、工具调用、审批与 hunk 级 diff 审阅集中在同一工作区；
3. **本地优先与用户掌控**：BYOK、密钥只进系统钥匙串、无遥测、无账号，并支持完全本地推理；
4. **明确的安全边界**：所有触盘、触 shell、触密钥和触网操作统一经过授权、审批与安全策略门控。

---

## 2. 目标与非目标

### 2.1 目标（Goals）

- G1：交付完整的一体化开发工作台子系统 — 终端、编辑器、文件浏览器、源码管理、Web 预览、主题、通知、AI（Provider/Composer/Agent/Plan/子代理/会话记忆/终端代理检测/安全审批）。
- G2：macOS、Linux 与 Windows 均为一等支持平台，三者的核心功能与质量门槛均按 P0 交付。
- G3：所有 AI 危险操作有显式边界：审批门控、hunk 级 edit diff、secret deny-list、workspace 授权注册表、SSRF 防护。
- G4：中文/CJK 输入（IME）为一等公民 — 终端、编辑器、Composer 三处输入面全部支持。
- G5：可被编码代理高效开发：模块边界清晰、crate 划分与本 Spec 的模块编号一一对应。

### 2.2 非目标（Non-Goals）

- NG1：不做 LSP 全功能 IDE（无补全引擎、无调试器）；编辑器定位是「终端旁的顺手编辑器 + AI diff 审阅面」。
- NG2：不做插件系统 / 扩展市场（自定义 Agent 与自定义主题除外）。
- NG3：不做远程开发（SSH 工作区）— P2 之后再议。
- NG4：不以极限压缩安装包体积为首要目标；优先保证原生渲染性能、功能完整性与跨平台稳定性。
- NG5：不内置任何托管模型代理服务；永远 BYOK。

---

## 3. 技术栈决策表

| 领域 | Termior 方案 | 理由 / 备注 |
|---|---|---|
| 应用框架 | **GPUI**（zed-industries/gpui） | 单进程原生 GPU 渲染，使用 Entity/Context 构建事件驱动 UI |
| UI 组件 | **GPUI + `Termior-ui-kit`（自建轻量控件）** | 不采用 gpui-component（ADR 0003）；通用控件与尺寸 token 落在 ui-kit，业务状态不得泄漏进控件层 |
| 应用状态 | GPUI `Entity`/`Context` 模型 | 每个子系统维护一个核心 Entity，通过事件更新状态 |
| 终端仿真 | **alacritty_terminal**（VTE 解析 + 网格模型）+ GPUI 自绘 | 解析、网格状态和渲染职责清晰分离，兼顾性能与兼容性 |
| PTY | **portable-pty** | 提供跨平台 PTY 抽象；Windows 侧补充 Job Object 与 ConPTY 串行化定制 |
| 编辑器 | **ropey + tree-sitter** 自研 | 最大工作量项，按 6.3 分阶段交付；不依赖第三方 GPUI 编辑器组件 |
| AI SDK | **自研 Provider 抽象层**（reqwest + SSE 解析）+ 原生 Agent 循环 | 统一不同模型服务的流式消息、工具调用和错误语义 |
| 模块通信 | 进程内 `async channel` + GPUI 事件 | 避免序列化边界，保持异步 IO 与 UI 状态解耦 |
| 异步运行时 | GPUI executor（主线程调度）+ **独立 tokio 运行时线程**承载网络/PTY IO，channel 桥接 | 隔离 UI 调度与阻塞或高吞吐 IO |
| 持久化 | serde_json + 原子写（tempfile + rename）+ 启动 schema 迁移 | 满足 6.14 与第 7 节的数据安全要求 |
| 密钥存储 | **keyring crate** | 统一接入系统钥匙串，避免密钥进入应用数据文件 |
| Web 预览 | **系统默认浏览器**（经 `Termior-platform::open_external`） | 不内嵌 WebView（ADR 0002）；与上游 Zed 处理 URL/OAuth 一致，消除 z-order/焦点与 WebView2 重入风险 |
| 系统通知 | macOS: UNUserNotificationCenter；Windows: WinRT Toast；Linux: notify-rust | 统一封装进 `platform` crate |
| 语音输入 | **cpal** 采集 + Provider STT（云 Whisper 或本地 whisper-rs） | P2 |
| Git | **gix（gitoxide）或 git2** 做 status/diff/log/graph；push/pull/fetch 走系统 `git` CLI | CLI 可直接使用用户现有 credential helper 与 SSH 配置 |
| 模糊匹配 | **nucleo** | 文件查找与 `@path` 引用共用 |
| 内容搜索 | grep-* crates | 使用 ripgrep 搜索引擎 |
| 文件遍历/忽略 | ignore crate | 尊重 `.gitignore`/`.ignore` |
| 文件图标 | 内嵌 SVG 资源 + Rust 侧 resolver | 图标集需确认许可后打包 |
| Markdown 渲染 tab | pulldown-cmark + GPUI 渲染 | 同时服务 AI 消息渲染，避免为单一能力无条件扩大依赖面 |

### 3.1 UI 控件层：不采用 gpui-component（ADR 0003）

**决策**：不引入 gpui-component。通用桌面控件（Icon、IconButton、Tooltip、Input chrome、ListRow、Menu、EmptyState、尺寸 token 等）在内部 crate `Termior-ui-kit` 自建；除该 crate 外，业务 crate 只依赖 GPUI 与 ui-kit，禁止新增对 gpui-component 的依赖。

**权衡依据**：gpui-component 将其 gpui 钉在与本仓库不同的 Zed rev，而 Termior 已 vendor 并修补 `gpui_windows`（窗口关闭竞态等）。对齐意味着迁移 gpui rev、重打补丁并长期跟随其升级节奏，成本高于自建当前所需的薄控件面。详见 `docs/adr/0003-no-gpui-component.md`。

**控件层职责边界**：

- `Termior-ui-kit`：无业务状态的可复用控件与 token；可选 `gpui-kit` feature 隔离 GPUI。
- 不得作为控件层状态边界：终端网格/PTY、最终代码编辑器与 AI diff、Workspace/Agent/会话、中央主题源模型、Web 预览（系统浏览器，ADR 0002）。
- 不启用包含全部语法的 `tree-sitter-languages` 聚合 feature，只按实际支持语言逐项引入语法 crate。

**版本与依赖纪律**：gpui 锁定确切 tag/rev；升级集中在显式升级窗口中进行。`cargo tree` 与产物体积报告进入 CI，新增 UI 依赖必须说明其 Release 体积、冷启动和空闲内存影响。

---

## 4. 平台与运行环境

- **macOS** ≥ 13（Metal），Apple Silicon 优先优化 — P0
- **Linux** X11/Wayland（Vulkan；提供软件渲染回退开关）— P0
- **Windows** 10 22H2+（DirectX 11+，ConPTY）— P0
- 单窗口 = 单工作区；支持多窗口多工作区（P1）
- 发行物：macOS `.dmg`（签名+公证）、Linux `.AppImage`/`.deb`、Windows `.msi`

---

## 5. 系统架构

### 5.1 进程与线程模型

```
┌──────────────────────────── Termior 进程 ────────────────────────────┐
│  主线程（GPUI executor）                                             │
│    Workspace / 各视图 Entity / 主题引擎 / keymap 分发                 │
│      ▲ GPUI 事件、Entity 更新（cx.spawn / channel 桥接）              │
│      │                                                               │
│  ├── PTY reader 线程 × N ──── VTE 解析 → 网格 diff → 事件通道         │
│  │        └── OSC 7/133/777 字节过滤器（cwd / 命令边界 / 代理信号）    │
│  ├── tokio 运行时线程池 ──── AI HTTP/SSE、git CLI 子进程、STT          │
│  └── fs 索引线程 ──── ignore walk / nucleo 匹配 / grep 搜索           │
└──────────────────────────────────────────────────────────────────────┘
```

### 5.2 Cargo workspace 划分

| crate | 职责 | 对应 Spec 模块 |
|---|---|---|
| `Termior` | 入口、窗口、全局 keymap、装配 | 6.1 |
| `Termior-ui` | tabs/panes、sidebar、statusbar、header、settings 窗口、composer 视图 | 6.1 / 6.10 / 6.15 |
| `Termior-ui-kit` | 稳定的通用 UI 接口、Termior 主题 token 适配；唯一允许依赖 gpui-component 的 crate | 3.1 / 6.1 / 6.15 |
| `Termior-terminal` | PTY 管理、alacritty_terminal 封装、shell integration 注入、渲染元素、agent_detect | 6.2 / 6.13 |
| `Termior-editor` | rope 缓冲、tree-sitter 高亮、编辑器视图、Vim 层、AI diff 视图 | 6.3 |
| `Termior-explorer` | 目录树、fs 索引、fuzzy、grep | 6.4 |
| `Termior-vcs` | git 状态/暂存/提交/图谱/远端操作 | 6.5 |
| `Termior-ai` | Provider 抽象、Agent 运行时、工具注册表、审批网关、会话存储、子代理 | 6.9–6.12 |
| `Termior-security` | deny-list、workspace 授权注册表、SSRF guard | 6.14 |
| `Termior-store` | settings/sessions/agents/snippets/todos/themes 持久化与迁移 | 7 |
| `Termior-platform` | keychain、系统通知、dirs、打开外部链接 | 6.8 / 7 |

### 5.3 关键架构不变量（Invariants）

- **INV-1**：tab 一经创建，其 Entity 在显式关闭前始终存活；切换 tab 只切换渲染，PTY 流、编辑器 undo 栈、预览页面状态全部在后台保持。
- **INV-2**：一切触盘/触 shell/触密钥的 AI 行为必须经过 `Termior-security` 的门控 API，UI 层不得绕行直接调用 fs/process。
- **INV-3**：deny-list 与 workspace 授权在**路径 canonicalize 之后**强制执行，与调用来源无关（提示词注入、路径穿越均不可绕过）。
- **INV-4**：终端代理状态转换只能由显式 OSC 777 序列触发，绝不基于原始输出启发式推断。
- **INV-5**：API Key 只存在于 OS 钥匙串与调用瞬间的内存中；任何持久化文件、日志、panic 报告不得出现 key 明文。

---

## 6. 功能规格

### 6.1 工作区与窗口布局（FR-WS）

**概述**：单窗口应用，主区域为 tab + 分栏，左侧 sidebar（活动栏 + 可折叠面板），底部状态栏，顶部 header，底部停靠 AI Composer；设置为独立窗口。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-WS-01 | 支持 8 种 tab 类型：`terminal` / `editor` / `preview` / `markdown` / `ai-diff` / `git-diff` / `git-history` / `git-commit-file` | P0（preview/git 类随对应模块里程碑落地） |
| FR-WS-02 | tab 切换不销毁状态（见 INV-1）；新 tab 继承活动 tab 的 cwd | P0 |
| FR-WS-03 | 任意 tab 可分栏：`Cmd+D` 右分、`Cmd+Shift+D` 下分、`Cmd+[` / `Cmd+]` 切换焦点、`Cmd+W` 关闭焦点 pane（最后一个 pane 时关闭 tab）；pane 可拖拽调整尺寸、独立关闭 | P0 |
| FR-WS-04 | sidebar 活动栏含三个面板：文件浏览器、源码管理、Git 历史；`Cmd+B` 折叠/展开，`Cmd+Shift+E` 聚焦浏览器 | P0 |
| FR-WS-05 | 状态栏为上下文条：终端显示 cwd 面包屑（OSC 7），编辑器显示文件路径与行列；右侧含工作区名、git 分支、AI 状态（工具计数仅在 >0 时显示）、localhost 预览 pill | P0 |
| FR-WS-06 | 自绘标题栏：标签栏与窗口控制合并为一行；操作区仅通知铃铛与设置；分栏入口在 pane 上下文菜单；主题选择在设置窗；工作区切换（本地 + Windows WSL）在状态栏 | P0（WSL 为 P1） |
| FR-WS-07 | 设置为独立 GPUI 窗口（`Cmd+,`），左侧竖向导航六页：General / Models / Themes / Shortcuts / Agents / About；自动保存（敏感凭据仍显式确认） | P0 |

**验收标准**：终端 tab 中运行 `npm run dev`，切走再切回，输出无丢帧无重启；分栏内两个 PTY 独立收发；关闭窗口时所有布局状态持久化并在下次启动恢复（P1）。

**技术要点**：分栏通过 `Termior-ui-kit` 的 `PaneLayout` 实现；主窗口 `TitlebarOptions { appears_transparent: true }` 自绘 chrome；tab 内容以 Entity 持有，隐藏时跳过 paint 但保留状态；cwd 继承读取终端模块暴露的 `latest_cwd()`。

### 6.2 终端（FR-TERM）

**概述**：产品核心。真 PTY + alacritty_terminal 网格模型 + GPUI 自绘渲染。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-TERM-01 | 每个终端 = 独立 PTY 会话（portable-pty），reader 线程解析 VTE 字节流写入共享网格，UI 按帧消费脏区 | P0 |
| FR-TERM-02 | 多 tab：`Cmd+T` 新建（继承 cwd）、`Cmd+R` 新建私有终端（不继承 cwd/env）、`Cmd+1..9` 跳转、`Ctrl+Tab` 循环 | P0 |
| FR-TERM-03 | 后台 tab 持续消费 PTY 输出，回滚缓冲不丢 | P0 |
| FR-TERM-04 | 行内搜索 `Cmd+F`：高亮、上一个/下一个、大小写切换；搜索 overlay 与编辑器复用 | P0 |
| FR-TERM-05 | Shell 支持：Unix 跟随 `$SHELL`（zsh/bash/fish）；Windows 依次探测 pwsh → powershell → cmd | P0 |
| FR-TERM-06 | shell integration 自动注入，用户无需改 rc 文件：zsh 经 `ZDOTDIR` 四件套、bash 经 `--rcfile`、pwsh 经 `-File profile.ps1`（内部 source 用户真实配置）；注入 OSC 7（cwd）与 OSC 133 A/B/C/D（提示符与命令边界） | P0 |
| FR-TERM-07 | 进程生命周期：关 tab 杀 shell 直接子进程；Windows 上每会话绑定 Job Object（`KILL_ON_JOB_CLOSE`），宿主进程无论如何退出都级联杀死整棵子进程树；ConPTY spawn 加互斥锁串行化 | P0 |
| FR-TERM-08 | 渲染：true color、超链接检测（可点击）、平滑滚动、调色板由中央主题引擎驱动 | P0 |
| FR-TERM-09 | 设置项：字体族、字号（8–32，预设档位）、字距、回滚行数（200–50,000，预设档位） | P0 |
| FR-TERM-10 | IME/CJK 输入支持（预编辑串内联显示） | P0 |

**验收标准**：`vim`、`htop`、`tmux` 正常渲染交互；`cat` 一个 50MB 日志不冻结 UI（解析在 reader 线程，UI 限帧消费）；`cd` 后状态栏面包屑即时更新；杀掉 Termior 进程后 Windows 上由其启动的 dev server 不残留。

**技术要点**：网格模型与解析直接使用 `alacritty_terminal`，渲染层自绘等宽文本 runs、背景色块与光标；链接检测使用 alacritty 的 regex hint 机制；OSC 7/133/777 在 reader 线程的字节过滤器截获（见 6.13），不进入网格。

### 6.3 编辑器（FR-EDIT）

**概述**：终端旁的原生代码编辑器，重点是「顺手编辑 + AI diff 审阅」。自研工作量最大的模块，明确分阶段。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-EDIT-01 | 基础编辑：打开/保存（`Cmd+E` 新建编辑器 tab）、多光标不要求、撤销/重做（`Cmd+Z`/`Cmd+Y`）、文件内查找 `Cmd+F` | P0 |
| FR-EDIT-02 | tree-sitter 语法高亮：TS/JS、Rust、Python、Go、C/C++、Java、HTML/CSS、JSON、Markdown；按扩展名探测 | P0 |
| FR-EDIT-03 | tab 状态保持：光标、undo 历史、选区跨切换存活（INV-1） | P0 |
| FR-EDIT-04 | AI edit diff：AI 提议的文件修改**不直接写盘**，打开 `ai-diff` tab 并排对比，**逐 hunk 接受/拒绝**，写工具只对已接受结果执行；主代理/子代理/自定义代理走同一机制 | P0 |
| FR-EDIT-05 | 行内 AI 自动补全：停顿后请求独立配置的补全 Provider，ghost text 内联渲染，`Tab` 接受、继续输入即拒绝；可整体关闭 | P1 |
| FR-EDIT-06 | Vim mode：动作、寄存器、标记、可视模式、`:` 命令行；设置中开关 | P1 |
| FR-EDIT-07 | 10 套编辑器主题（Atom One、Aura、Copilot、GitHub 深/浅、Gruvbox Dark、Nord、Tokyo Night、Xcode 深/浅），独立于应用主题选择 | P1（P0 先内置 2 套） |

**验收标准**：打开 5MB JSON 不卡顿（rope + 增量高亮 + 视口渲染）；AI 提议 5 个 hunk、接受 4 拒 1，落盘结果精确等于接受集；Vim `ciw`、`d2j`、可视块选择行为正确。

**技术要点**：缓冲使用 `ropey`，高亮使用 `tree-sitter` 增量解析并仅渲染视口；diff 计算使用 `similar` crate（hunk 粒度）；Vim 层实现为 GPUI keymap context 状态机。最终编辑器必须直接基于 GPUI 自研；M2 早期若用 gpui-component 编辑器占位，只能封装在 `Termior-editor` 内部，且替换时保持对外 API 不变。

### 6.4 文件浏览器（FR-EXPL）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-EXPL-01 | 目录树：文件类型图标（Catppuccin/Material 风格映射）、键盘导航（方向键/Enter/左右折叠）、行内重命名、右键菜单（新建文件/目录、重命名、删除、在系统中显示）、dotfiles 显隐开关 | P0 |
| FR-EXPL-02 | 树根跟随活动 tab cwd；路径统一正斜杠规范化（Windows 在边界层转换），OSC 7 触发时树状态稳定不重置展开态 | P0 |
| FR-EXPL-03 | 后台线程用 `ignore` crate 建索引，尊重 `.gitignore`/`.ignore`；文件系统变更 watch 增量更新（`notify` crate） | P0 |
| FR-EXPL-04 | 模糊文件查找 `Cmd+Shift+F`：nucleo 排序，Enter 打开、Esc 关闭 | P0 |
| FR-EXPL-05 | 同一查找器切换内容搜索模式：grep-* 引擎全文搜索，支持 glob 过滤，结果按文件分组流式渲染，含行号与命中高亮 | P0 |
| FR-EXPL-06 | Attach to AI：右键文件附加到 Composer；任意 tab 中选中文本 `Cmd+L` 作为 selection chip 附加（chip 化，不污染输入框文本） | P0（依赖 6.10） |

**验收标准**：10 万文件仓库中模糊查找首屏 < 100ms；grep 结果边搜边出；被 .gitignore 覆盖的文件不出现在查找与 grep 中。

### 6.5 源码管理（FR-VCS）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-VCS-01 | 状态面板（`Cmd+G`）：按 Unstaged / Staged / Untracked 分组；文件级与 **hunk 级** stage/unstage；discard 带确认；文件可打开为 `git-diff` tab 行内对比 | P1 |
| FR-VCS-02 | 提交：消息输入框 + `Cmd+Enter` 提交暂存集；分支指示器（含 detached HEAD 态） | P1 |
| FR-VCS-03 | push（首推自动建 upstream，其余显示 ahead/behind）、pull（默认 ff-only）、fetch | P1 |
| FR-VCS-04 | 提交历史：真实 commit graph（merge/分支 lane 分配、本地/远端 ref 与 tag 标签）；点击 commit 看文件列表、单文件 diff（`git-commit-file` tab）、跳转远端 commit 页（GitHub/GitLab 等）；顶部搜索过滤 | P1 |
| FR-VCS-05 | 全部 git 操作经 workspace 授权注册表门控（与 PTY spawn、AI 工具同一注册表，见 6.14） | P1 |
| FR-VCS-06 | diff 颜色由中央主题引擎提供 | P1 |

**技术要点**：status/diff/log/graph 用 `gix`（备选 `git2`）；push/pull/fetch 以子进程调用系统 `git`（继承用户 credential helper 与 SSH 配置，绕开 libgit2 鉴权兼容性坑）；graph lane 分配算法独立纯函数，单测覆盖 merge/octopus 场景。

### 6.6 Web 预览（FR-PREV）

> 经 ADR 0002：Termior 不内嵌 WebView。预览 tab 检测并校验 URL 后统一交给系统默认浏览器打开；localhost 检测、URL 校验与原生 Markdown 预览等正交能力全部保留。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-PREV-01 | 在 PTY 输出中检测 localhost URL（localhost / 127.0.0.1，Vite/Next 等格式），状态栏出现「Open Web Preview」pill；检测基于 PTY 输出流而非终端缓冲快照，重绘型 TUI 不误触发 | P1 |
| FR-PREV-02 | `Cmd+P` 手动新建预览 tab，可输入任意 http(s) URL；输入经 `normalize_preview_url` 校验（HTTP/HTTPS-only，必须有 host） | P1 |
| FR-PREV-03 | 预览 tab 不承载网页渲染，仅作为「Open in browser」入口，URL 与校验状态在 tab 存活期间按 INV-1 保留；点击即在系统默认浏览器打开 | P1 |
| FR-PREV-04 | 统一降级即默认路径：所有平台一律经 `Termior-platform::open_external` 打开，无内嵌后端，故无「初始化失败」分支 | P1 |

**技术要点**：预览域逻辑（localhost 正则扫描、URL 校验、`PreviewTab` 状态机、原生 Markdown 解析）位于无 IO 的 `Termior-preview` crate；系统浏览器打开由 `Termior-platform::open_external` 跨平台分发（Windows `rundll32 url.dll,FileProtocolHandler`、macOS `open`、Linux `xdg-open`），且同样强制 http/https 边界。预览 tab 的 GPUI 视图只渲染占位面板与「Open in browser」主按钮。

### 6.7 主题与背景（FR-THEME）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-THEME-01 | 中央主题引擎：一份 Rust 主题 token 结构（语义色板）同时驱动 UI 组件、终端 16+ 色调色板、diff 颜色、通知样式 | P0 |
| FR-THEME-02 | 内置应用主题各带原生外观归属（Light/Dark）；选择主题即锁定到其原生外观。`FollowSystem` 使用浅色槽位 + 深色槽位主题配对（候选按原生外观过滤）。内置含 default / default-light、nord / nord-light 及 tide、catppuccin、tokyo-night、caffeine、claude、gruvbox、sage、rose-pine 等 | P0（首发子集，其余 P1） |
| FR-THEME-03 | 编辑器主题独立选择（见 FR-EDIT-07），允许深色编辑器配浅色应用 | P1 |
| FR-THEME-04 | 自定义主题：基于任意预设在应用内改 token、保存并列展示；JSON 含 `native_appearance` 与单色板；支持导入/导出（旧双色板格式仍可导入） | P1 |
| FR-THEME-05 | 背景图：任选图片，透明度滑杆（与背景色混合）+ 高斯模糊滑杆（模糊图片不糊前景）；作用于整窗最底层；图片解码一次缓存 | P2 |

**技术要点**：Termior `Theme` + `NativeAppearance` 与全局 Entity 是唯一主题源，切换时广播重绘；`Termior-ui-kit` 把语义 token 映射到自研控件样式；不再做算法推导的浅色变体。背景模糊用离屏一次性高斯模糊缓存纹理，不做逐帧后处理。

### 6.8 通知（FR-NOTIF）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-NOTIF-01 | 统一路由（单一决策点）：目标 tab 可见且窗口聚焦 → 抑制；窗口聚焦但 tab 隐藏 → 应用内 toast（随主题）；窗口失焦 → 系统原生通知 | P1 |
| FR-NOTIF-02 | 内置 Agent 状态接入同一路由：等待审批/出错 → attention；忙/闲/完成 → working/finished | P1 |
| FR-NOTIF-03 | 终端内编码代理（见 6.13）的 started/working/attention/finished/exited 信号接入同一路由 | P1 |
| FR-NOTIF-04 | header 通知铃铛：列出所有内置与终端侧代理的当前状态 | P1 |
| FR-NOTIF-05 | 设置 → Agents 中全局开关 | P1 |

### 6.9 AI Provider 与密钥（FR-PROV）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-PROV-01 | 云 Provider：OpenAI、Anthropic、Google（Gemini）、Groq、xAI、Cerebras、OpenRouter、DeepSeek、Mistral、OpenAI-compatible（自定义 base URL）；每家一张设置卡片，填 key 选模型即用 | P0（首发 Anthropic + OpenAI + OpenAI-compatible，其余 P1） |
| FR-PROV-02 | 本地推理（key 可选）：LM Studio（默认 `http://127.0.0.1:1234/v1`）、MLX（`:8080/v1`）、Ollama（`:11434`）；base URL 可覆盖；保存时可达性 ping 指示 | P0 |
| FR-PROV-03 | 模型选择器：当前 Provider 模型注册表 + 收藏 + 最近；默认聊天模型与默认补全模型独立设置 | P0 |
| FR-PROV-04 | 密钥入 OS 钥匙串（keyring crate，service `Termior-ai`：macOS Keychain / Windows Credential Manager / Linux Secret Service，headless 文件回退）；settings 文件、日志、环境变量永不出现明文 key；切换活动 key 时清空内存会话映射，盘上会话保留并重绑定 | P0 |
| FR-PROV-05 | 统一出网层：所有 Provider HTTP 流量经 `Termior-security` 的 SSRF guard（拦截 loopback/link-local/私网段，显式配置的本地 Provider base URL 白名单放行） | P0 |

**技术要点**：Provider 抽象为 `trait Provider { fn stream_chat(...) -> impl Stream<Item = ChatEvent>; }`，统一消息/工具调用/流式增量事件模型，各家做请求/响应适配器；SSE 解析用 `eventsource-stream` 或手写行协议解析；网络全部在 tokio 线程池，事件经 channel 进 GPUI。

### 6.10 Composer 与 Agent 运行时（FR-AGENT）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-AGENT-01 | Composer 停靠输入栏，`Cmd+I` 开关；应用根部常驻挂载，与选区附加、浏览器附加等入口共享状态 | P0 |
| FR-AGENT-02 | 三类附件：图片（粘贴/拖拽/选择）、文本文件（提交时包成 `<file path="...">` 块）、选区（`<selection source="terminal|editor">` 块）；选区与文件以 chip 呈现，不注入输入框文本 | P0 |
| FR-AGENT-03 | `@path` 文件引用：输入 `@` 后对工作区 nucleo 模糊匹配插入 chip，提交时读取内容，**经 secret deny-list 过滤** | P0 |
| FR-AGENT-04 | `#handle` 片段：可复用提示词片段库（`Termior-ai-snippets.json`） | P1 |
| FR-AGENT-05 | `/` 命令面板：不离开 Composer 执行应用内动作 | P1 |
| FR-AGENT-06 | 语音输入：麦克风按钮流式转写入输入框 | P2 |
| FR-AGENT-07 | 实时上下文桥：`get_terminal_context` 工具惰性抓取活动终端「此刻」状态 — 当前 cwd（OSC 7）+ 活动 PTY 缓冲末 ~300 行；执行时快照，不预缓存 | P0 |
| FR-AGENT-08 | Agent 循环：流式响应、工具调用、步数上限（`MAX_AGENT_STEPS`）、系统提示词可配置；支持派生子代理（6.11） | P0 |
| FR-AGENT-09 | 工具分两级 — 自动执行（只读）：`read_file`、`list_directory`、`fs_search`、`fs_grep`；审批门控：`write_file`、`create_directory`、`rename`、`delete`、`run_command`（一次性子 shell）、`shell_session_run`（持久代理 shell）、`shell_bg_spawn`（长驻后台进程） | P0 |
| FR-AGENT-10 | 审批卡片在 Composer 内联渲染，展示精确参数；Agent 循环挂起等待接受/拒绝，接受后自动续跑 | P0 |

**技术要点**：审批门实现为工具执行前的 `oneshot` channel 等待，UI 决议后 resolve；Agent 状态机（idle/busy/awaiting-approval/finished/error）作为 Entity 供通知路由与状态栏消费；工具全部经 `Termior-security` API（INV-2）。

### 6.11 Plan mode、子代理与自定义代理（FR-PLAN）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-PLAN-01 | Plan mode 开关：开启后代理首先产出计划（步骤/文件路径/大致范围），确认前零写入；确认后逐步执行，触发审批工具时仍出卡片；步骤可单独拒绝 | P1 |
| FR-PLAN-02 | 子代理：主代理可调 `run_subagent` 把窄任务委托给「窄提示词 + 工具子集」的子代理，完成后结果回报父代理 | P1 |
| FR-PLAN-03 | 自定义代理（设置 → Agents）：独立系统提示词 + 任意工具子集 + 图标颜色；持久化于 `Termior-ai-agents.json`；Composer 代理选择器切换；每个会话记住创建时的活动代理 | P1 |
| FR-PLAN-04 | Plan mode 与子代理可组合：计划中列出子代理调用步骤，可在 spawn 前拒绝 | P1 |

### 6.12 会话与记忆（FR-SESS）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-SESS-01 | 命名持久会话：会话列表 + activeId + 每会话完整消息历史（`Termior-ai-sessions.json`）；标题由首条用户消息自动生成；可重命名/删除；每次消息变更镜像落盘 | P0 |
| FR-SESS-02 | 项目记忆：从工作区根加载 `Termior.md` 作为代理记忆（对齐 CLAUDE.md/AGENTS.md 惯例），每会话加载一次前置到工作上下文；支持 `AGENTS.md` 内容为单行 `Termior.md` 的重定向写法 | P0 |
| FR-SESS-03 | 自定义指令（设置 → General）：跨项目全局 system prompt 附加段 | P1 |
| FR-SESS-04 | snippets 库与 todos 存储（`Termior-ai-todos.json`，代理可读写的应用内 TODO） | P1 |

### 6.13 终端内编码代理检测（FR-TAGENT）

**概述**：把终端 tab 里跑的 CLI 编码代理（Claude Code 为首发目标）当一等公民 — 检测其状态并接入统一通知面。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-TAGENT-01 | PTY reader 线程字节过滤器：`OSC 133;C;<cmd>` 出现时自锁定检测器，随后读取 `OSC 777;notify;Termior;<event>` 载荷，事件枚举：started / working / attention / finished / exited；任意 shell（含 tmux 内）有效；无代理运行时过滤器为空转零成本 | P1 |
| FR-TAGENT-02 | 状态转换只由显式 OSC 序列触发（INV-4），高频重绘的 TUI 不抖动通知状态 | P1 |
| FR-TAGENT-03 | Claude Code hooks 一键安装（设置 → Agents）：向 `~/.claude/settings.json` 写入三条 hook（UserPromptSubmit→working、Notification→attention、Stop→finished），各为输出含 `terminalSequence`（OSC 777）JSON 的单行命令，以 `Termior_TERMINAL` 环境变量守卫使其在其他终端中为空操作 | P1 |
| FR-TAGENT-04 | 安装器安全性：现存 settings.json 非法 JSON 时拒绝写入绝不覆盖；临时文件 + rename 原子写；幂等（重跑迁移不重复）；卸载只删除自有标记（命令串含 `notify;Termior;`）的 hooks，清理遗留空 hook 组；提供安装状态查询 | P1 |

### 6.14 安全模型（FR-SEC）

**设计前提**：假设 AI 会出错、提示词会被投毒。每个触盘/触 shell/触密钥/触网的动作前都有显式边界。

| ID | 需求 | 优先级 |
|---|---|---|
| FR-SEC-01 | 工具两级门控 + 审批卡片（同 FR-AGENT-09/10） | P0 |
| FR-SEC-02 | AI edit diff：`write_file` 永不直接写盘，hunk 级审阅（同 FR-EDIT-04） | P0 |
| FR-SEC-03 | secret deny-list：`.env`、`.env.*`、`.ssh/`、`credentials`、`.netrc`、`.aws/credentials`、钥匙串目录等，AI 工具读写双向禁止；在 Rust 工具层、路径 canonicalize 之后强制（INV-3）；扩充名单只能改代码，不提供运行时配置 | P0 |
| FR-SEC-04 | workspace 授权注册表：AI 工具、git 命令、PTY spawn 共用同一注册表；新工作区首次提示授权一次；代理不可触达未显式打开的同级目录 | P0 |
| FR-SEC-05 | SSRF guard（同 FR-PROV-05）：投毒提示词无法诱导代理经 Provider 调用打内网服务 | P0 |
| FR-SEC-06 | 密钥永不落盘（同 FR-PROV-04、INV-5） | P0 |
| FR-SEC-07 | 无遥测、无账号、无自动上报；崩溃报告仅本地留存 | P0 |

### 6.15 设置与快捷键（FR-SET）

| ID | 需求 | 优先级 |
|---|---|---|
| FR-SET-01 | 六页签设置窗口：General（shell、字体、autocomplete、自定义指令、dotfiles、WebGL→GPU 渲染等价开关）、Models、Themes、Shortcuts、Agents、About（版本、数据迁移错误展示） | P0 |
| FR-SET-02 | 所有快捷键可重绑定；GPUI keymap + context 系统实现，冲突检测 | P1（P0 固定默认键位） |
| FR-SET-03 | 默认键位采用跨平台桌面应用的通用习惯（见附录 A），macOS 使用 Cmd，Win/Linux 映射为 Ctrl | P0 |

---

## 7. 数据与持久化（FR-DATA）

应用数据目录经 `dirs` crate 解析（bundle id 建议 `app.<org>.Termior`），绝不裸读 `$HOME`/`%APPDATA%`：

| 平台 | 路径 |
|---|---|
| macOS | `~/Library/Application Support/app.<org>.Termior/` |
| Linux | `~/.local/share/app.<org>.Termior/` |
| Windows | `%APPDATA%\app.<org>.Termior\` |

| 文件 | 内容 |
|---|---|
| `Termior-settings.json` | 应用偏好：主题、字体、快捷键、补全开关、代理开关、WSL 发行版等 |
| `Termior-ai-sessions.json` | 会话列表、activeId、每会话消息历史 |
| `Termior-ai-agents.json` | 自定义代理（系统提示词 + 工具子集） |
| `Termior-ai-snippets.json` | Composer `#handle` 片段 |
| `Termior-ai-todos.json` | 代理可读写的 TODO |
| `Termior-custom-themes.json` | 用户自建主题 |
| `themes/` | 背景图与主题资产 |

要求：全部原子写（temp + rename）；启动时 schema 校验与迁移，错误浮出到设置 About 页；目录可整体备份/跨机同步（关闭应用后）；关闭状态下手动编辑 JSON 合法，下次启动校验迁移。API key 永不进入上述任何文件（见 FR-PROV-04）。

---

## 8. 非功能需求（NFR）

| ID | 需求 | 指标 |
|---|---|---|
| NFR-01 | 冷启动 | Apple Silicon < 300ms 出首帧；x86 Linux 与 Windows 10/11 < 800ms |
| NFR-02 | 终端性能 | VTE 解析吞吐达 alacritty_terminal 同量级；`cat` 大文件期间 UI 不掉帧；键入回显 p99 < 16ms |
| NFR-03 | 渲染 | 常态 ≥ 60fps，高刷屏目标 120fps；空闲零重绘 |
| NFR-04 | 内存 | 空载（1 终端 tab）< 150MB |
| NFR-05 | 体积 | 单二进制（含 tree-sitter 语法与图标资产）≤ 60MB；README 说明主要体积构成与可选功能的影响 |
| NFR-06 | 稳定性 | PTY reader panic 不拖垮进程（会话标记为死亡可重开）；会话/设置数据在崩溃后完好（原子写保证） |
| NFR-07 | 输入法 | 终端/编辑器/Composer 全面支持 IME 预编辑（中文输入一等公民） |
| NFR-08 | 隐私 | 无遥测、无账号、离线可用（本地 Provider 路径全功能） |
| NFR-09 | 许可 | 项目以 Apache-2.0 发布；所有第三方 crate、图标、字体与其他打包资产均需核验许可并保留必要声明 |
| NFR-10 | 可测性 | 网格解析、diff/hunk、graph lane、deny-list、SSRF 判定、hooks 安装器均为纯函数/独立模块，单测覆盖率 ≥ 80% |
| NFR-11 | UI 依赖预算 | 不引入 gpui-component（ADR 0003）；新增 UI 依赖须说明 Release 体积、冷启动与空闲 RSS 影响，并由 NFR-01/04/05 门禁把关；空闲零重绘见 NFR-03 |

---

## 9. 里程碑

| 里程碑 | 范围 | 退出标准 |
|---|---|---|
| **M0 骨架** | macOS/Linux/Windows 三平台窗口、tab/pane 布局、sidebar/statusbar/header 空壳、主题引擎 + 2 套主题、settings 持久化与迁移框架；完成 3.1 的三组 UI 依赖基准 | 三平台均可启动且布局可交互、主题可热切换、设置重启后保留；gpui-component 满足 NFR-11，或已由 `Termior-ui-kit` 替换超预算组件并重新通过基准 |
| **M1 终端 MVP** | FR-TERM 全量（P0 项）、shell integration、OSC 7/133 管线、分屏、行内搜索；Windows ConPTY、Job Object 与 pwsh/powershell/cmd 支持 | 三平台终端验收通过；vim/htop/tmux 或对应平台终端程序可用；6.2 验收标准全过 |
| **M2 浏览器 + 编辑器基础** | FR-EXPL 全量、FR-EDIT-01/02/03、fuzzy/grep | 6.4 验收标准全过；大文件编辑流畅 |
| **M3 AI 核心** | FR-PROV（P0）、FR-AGENT 全 P0、FR-EDIT-04（ai-diff）、FR-SESS-01/02、FR-SEC 全量 | 端到端：提问 → 工具调用 → 审批 → hunk 审阅 → 落盘；deny-list 与授权注册表红队用例全拦截 |
| **M4 外围子系统** | FR-VCS、FR-PREV、FR-NOTIF、FR-TAGENT | git 日常流可用；localhost 检测 + 系统浏览器预览（ADR 0002，不内嵌 WebView）；Claude Code 状态进通知铃铛 |
| **M5 完善** | FR-PLAN、FR-SESS-03/04、FR-EDIT-05/06/07、FR-SET-02、其余 P1/P2、三平台发行与回归测试收尾 | Plan mode / 子代理 / 自定义代理可用；Vim + 补全可用；三平台发行物通过验收 |

---

## 10. 风险与开放问题

| ID | 风险 | 缓解 |
|---|---|---|
| R1 | Windows 的 IME、ConPTY、Job Object 与系统集成可能出现平台特有回归 | Windows 与 macOS/Linux 同列 P0；CI 持续运行三平台构建、单测与关键交互验收，所有里程碑均以三平台通过为门槛 |
| R2 | ~~wry 子 WebView 与 GPUI 合成的 z-order/焦点限制~~（已由 ADR 0002 移除内嵌 WebView 消解） | Web 预览改为系统浏览器打开（FR-PREV-03/04）；若未来重新引入内嵌后端，需重开本风险并恢复浮层规避策略 |
| R3 | 自研编辑器工作量（最大不确定项） | 分阶段（6.3）；编辑器实现藏在 `Termior-editor` API 后 |
| R4 | gpui 版本追踪与 API 波动 | 锁定确切 tag/rev，集中升级窗口；Windows 平台补丁维护在 vendor `gpui_windows` |
| R5 | tokio 与 GPUI executor 双运行时桥接复杂度 | 统一封装 `spawn_net()` 桥接工具，禁止业务代码直接触碰两个运行时 |
| R6 | 原生 UI、自研控件、语法解析器与内嵌资产可能推高二进制体积 | 同时执行 NFR-05 与 NFR-11；CI 持续输出三平台 `cargo tree`、产物体积与基线差值 |
| R7 | 语音输入的跨平台采集与 STT 成本 | 定为 P2；首选复用已配置 Provider 的转写端点 |

**开放问题**：

- Q1：正式产品名（`Termior` 为占位）与 bundle id 归属。
- Q2：~~gpui-component 是否满足 NFR-11 / 保留范围~~ — **已关闭**（ADR 0003：不采用 gpui-component，自建 `Termior-ui-kit`）。
- Q3：会话消息历史膨胀策略（单文件 JSON 何时拆分为每会话一文件）。
- Q4：Claude Code 之外第二个终端代理（Codex 等）的 hooks 适配排期。

---

## 附录 A：默认键位表（macOS 记法，Win/Linux 以 Ctrl 对应）

| 动作 | 键位 |
|---|---|
| 新终端 tab / 私有终端 | `Cmd+T` / `Cmd+R` |
| 新编辑器 tab | `Cmd+E` |
| 新预览 tab | `Cmd+P` |
| 关闭焦点 pane/tab | `Cmd+W` |
| 跳转 tab / 循环 tab | `Cmd+1..9` / `Ctrl+Tab`、`Ctrl+Shift+Tab` |
| 右分屏 / 下分屏 / pane 焦点 | `Cmd+D` / `Cmd+Shift+D` / `Cmd+[`、`Cmd+]` |
| 行内搜索 | `Cmd+F` |
| sidebar 开关 / 聚焦浏览器 / 文件查找 | `Cmd+B` / `Cmd+Shift+E` / `Cmd+Shift+F` |
| 源码管理面板 | `Cmd+G` |
| Composer 开关 / 选区问 AI | `Cmd+I` / `Cmd+L` |
| 提交暂存集 | `Cmd+Enter`（提交输入框内） |
| 设置窗口 | `Cmd+,` |
| 撤销 / 重做 | `Cmd+Z` / `Cmd+Y` |

## 附录 B：OSC 序列参考

| 序列 | 含义 | 用途 |
|---|---|---|
| `OSC 7 ; file://<host><path>` | shell 报告 cwd | 状态栏面包屑、浏览器树根、新 tab cwd 继承（Windows 侧做盘符规范化） |
| `OSC 133 ; A / B` | 提示符开始 / 结束 | 提示符与用户输入边界 |
| `OSC 133 ; C ; <cmd>` | 命令输出开始 | 命令边界；**自锁定**终端代理检测器 |
| `OSC 133 ; D ; <code>` | 命令退出码 | 命令结束与退出状态 |
| `OSC 777 ; notify ; Termior ; <event>` | 终端代理状态（Termior 扩展） | started / working / attention / finished / exited → 通知路由 |

## 附录 C：术语表

- **ADE**：Agentic Development Environment，围绕编码代理组织的开发工作台。
- **BYOK**：Bring Your Own Key，用户自带模型 API Key，产品不代理不加价。
- **hunk**：diff 中的一段连续修改块，AI 编辑的最小审阅/接受单位。
- **workspace 授权注册表**：以工作区目录为粒度的一次性授权表，统一门控 AI 工具、git 与 PTY spawn。
- **SSRF guard**：出网层对回环/链路本地/私网地址的拦截，防提示词注入打内网。
