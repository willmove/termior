<p align="center">
  <img src="assets/termior-logo.svg" width="128" height="128" alt="Termior logo">
</p>

# Termior

[English](#english) · [简体中文](#简体中文)

---

<a id="english"></a>

## English

> An open-source, cross-platform, terminal-first AI-native development environment (ADE). BYOK, local-first, no account, no telemetry.

Termior brings a real PTY terminal, a lightweight code editor, a file explorer, Git, web preview, and a built-in/external agent with tool calling, approvals, change review, and recovery into a single native window. The authoritative product and engineering specification is [docs/termior-spec.md](docs/termior-spec.md).

### Core values

1. **Native low latency** — The core UI has no JS runtime and no IPC serialization boundary. The terminal and editor render directly on the GPU, targeting 60 fps in steady state and 120 fps on high-refresh displays.
2. **Deep terminal–AI collaboration** — A real PTY, live cwd/buffer context, tool calls, approvals, and hunk-level diff review all live in the same workspace.
3. **Local-first and user control** — BYOK, keys stored only in the OS keychain, no telemetry, no account, and support for local inference servers such as LM Studio, MLX, and Ollama.
4. **Explicit security boundaries** — Every disk, shell, secret, and network operation performed by the built-in agent goes through workspace authorization, approvals, a deny-list, and SSRF policy gates. External agents must display their backend, execution environment, actual isolation capabilities, and unknowns instead of passing off their own operations as if they were covered by the built-in gates.

### Current desktop milestone

The workspace has moved from a pure-logic prototype to a buildable GPUI desktop workbench. It currently includes:

- Persistent workspaces, 8 tab kinds, splits, Explorer / Source Control / History sidebars, a status bar, and a separate settings window;
- A real terminal built on portable-pty and alacritty_terminal: shell integration (bash/zsh/fish/PowerShell), OSC 7/133/777, IME, search, scrollback, URL and localhost detection, and Windows Job Objects; a Settings default-shell picker (system default / detected shells incl. Git Bash and WSL distros / manual path) plus an optional ask-on-new-terminal shell menu; a copy/paste/select-all context menu;
- [SSH sessions and SFTP](docs/ssh.md): connection profiles with sidebar quick-connect, OS credential store, password/MFA/key/agent authentication, jump hosts, host key verification, and file/directory transfer with resume; a dual-pane SFTP browser plus a File Explorer that follows the active remote tab;
- Rope edit buffers, a virtualized viewport, incremental tree-sitter highlighting, search, undo/redo, inline completion ghost text, 10 editor themes, and a Vim interaction layer;
- File indexing with shallow indexing and debounced watching, gitignore awareness, fuzzy find, streaming background grep, keyboard tree navigation, and full context menus; per-tab project folders with `cd` injection and sidebar follow;
- Dedicated Git diff / history / commit-file tabs, file- and hunk-level stage/unstage, confirmed discard, commit, branch, fetch/pull/push, and a commit graph, with status refresh moved off the UI thread;
- Web preview: localhost URL detection, URL validation, and hand-off to the system browser (no embedded WebView — see [ADR 0002](docs/adr/0002-remove-embedded-webview.md)); a native Markdown preview kept in sync with the shared edit buffer;
- 12 application themes with a custom theme model, plus a GPU-rendered blurred window background;
- Real HTTP/SSE adapters for OpenAI, Anthropic, Gemini, Groq, xAI, Cerebras, OpenRouter, DeepSeek, Mistral, OpenAI-compatible, LM Studio, MLX, and Ollama;
- OS keychain, session/project memory, file, image, clipboard, and `@path` attachments, snippets/TODO, and Auto / Plan / Yolo submit modes;
- Serializable Task/Turn, budgets and cancellation, real tool execution, approval cards, command timeouts, persistent shells, background processes, and the `write_file → AI diff → per-hunk decision → atomic write` safety loop;
- A built-in agent and the Codex app-server, switchable inside the Composer; the standalone `termior-agent-host` provides the structured backend contract, Codex/ACP adapters, capability negotiation, and execution-environment description;
- Layered `AGENTS.md` rules, a Context Inspector, Agent Skills, MCP over stdio and Streamable HTTP, bounded Hooks, reviewable Memory, custom agents, and sub-task orchestration;
- journal/snapshot recovery, a Recovery Center, content-addressed checkpoints, direct/worktree/sandboxed execution environments, and entry points for the task tree and automation queue;
- Unified notification routing for built-in and terminal agents: suppressed while the target is visible, themed toast when hidden, system notification when the window is unfocused, with a header bell listing current states;
- Safe, idempotent installation and removal of Claude Code OSC hooks.

This is still a staged milestone, not final acceptance of the whole spec. The baseline desktop scope is documented in [docs/desktop-milestone.md](docs/desktop-milestone.md); implementation evidence for Agent stages A–E, along with platform sandboxing, remote MCP OAuth, a real ACP client, and background automation execution boundaries, is documented in [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md).

The latest tagged release is `v0.1.8`; the workspace version lives in [Cargo.toml](Cargo.toml).

### Workspace

```text
crates/
  termior                  GPUI desktop application and view wiring
  termior-ui               persisted tab / sidebar / workspace state
  termior-ui-kit           shared controls, pane layout, and search models
  termior-terminal         PTY sessions, process lifecycle, and the byte bridge
  termior-ssh              OpenSSH connection profiles, auth options, SFTP transfer commands
  termior-terminal-core    OSC, shell integration, search
  termior-editor           Rope, tree-sitter, Vim, completion state
  termior-explorer         file index, tree, search, watcher
  termior-explorer-core    pure-logic fuzzy matching and globs
  termior-vcs              Git status, diff, history, and remote operations
  termior-preview          localhost detection and preview state
  termior-ai               providers, task runtime, tools, context, orchestration
  termior-agent-host       built-in/external agent backend contract, Codex, ACP, MCP
  termior-diff             hunk diff and acceptance-set application
  termior-security         authorization, deny-list, SSRF, tool gating
  termior-store            atomic JSON, migrations, settings, task journal, checkpoints
  termior-theme            central semantic palette and theme library
  termior-hooks            Claude Code hooks
  termior-platform         system notifications, external URLs, auto-update platform boundary
  termior-bench            cold start, memory, frame rate, and PTY throughput gates
```

### Build and verify

Stable Rust, the platform-native build toolchain, and GPUI's system dependencies are required.

#### Windows: activate the MSVC environment first

The build compiles C code from `libgit2-sys`, `libz-sys`, and others, so `cl.exe` must be able to find the C standard library headers (for example `time.h`). Running `cargo build` directly from Git Bash or a plain terminal leaves `INCLUDE`, `LIB`, and `VCINSTALLDIR` unset, and `cl.exe` fails with `fatal error C1083: Cannot open include file: 'time.h'` (visible in cc-rs logs as `command did not execute successfully (status code exit code: 2)`). Activate the MSVC environment before building.

Pick one of the following so that `INCLUDE` / `LIB` / `VCINSTALLDIR` are no longer empty:

- **x64 Native Tools Command Prompt for VS 2022** (simplest): open it from the Start menu — the variables are already set — and run the commands below.
- **PowerShell / cmd**: run `vcvars64.bat` first, then build:
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**: wrap the call in cmd so the `.bat` runs under the right interpreter:
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

Verify the environment:

```bat
echo %INCLUDE%   &  rem should point at MSVC headers plus the Windows SDK ucrt/shared/um directories
echo %LIB%       &  rem should point at the matching library directories
echo %VCINSTALLDIR%
```

#### Verify

These commands match the core/desktop split used in CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude termior -- -D warnings
cargo test --workspace --exclude termior --all-targets
cargo test -p termior-ui-kit --no-default-features
cargo check -p termior
cargo clippy -p termior --all-targets -- -D warnings
cargo test -p termior --all-targets
cargo build -p termior --release
cargo llvm-cov --workspace --exclude termior --tests --fail-under-lines 80
cargo bench -p termior-editor --bench large_file -- --quick
cargo bench -p termior-bench --bench pty_throughput -- --quick
```

CI implements the same split: `ci.yml` runs an icon-generation check, the core job on all three platforms, the desktop job (check, clippy, tests, release build, binary-size gate, Windows smoke frames, split-pane smoke, NFR process gate), a coverage job with an 80% line threshold, and a benchmark job with PTY throughput gating. `audit.yml` runs `cargo-deny` on every push and weekly to catch licence and CVE drift.

#### Application icons

`assets/termior-logo.svg` is the single source of truth for application icons. After editing the SVG, regenerate the Windows ICO, macOS ICNS, generic PNGs, and Linux hicolor assets with Node.js 22+:

```bash
npm ci
npm run icons
npm run icons:check
```

#### Run the desktop app

```bash
cargo run -p termior
```

#### Release

A release tag (`v<workspace-version>`) triggers a three-platform release build that produces portable ZIP/TAR archives with SHA-256 checksums plus per-platform installers: an Inno Setup `*-setup.exe` on Windows, a drag-to-install `.dmg` on macOS, and a `cargo-deb`-generated `.deb` on Linux. The pipeline enforces the 60 MiB single-binary limit and the 100 MiB archive/installer limit from Spec NFR-05. Locally you can reproduce the check and packaging with `scripts/check-release-binary.ps1` and `scripts/package-release.ps1` (the Windows installer requires Inno Setup 6 on the machine).

Release size comes mainly from the GPUI native rendering stack, the terminal/VTE layer, per-language tree-sitter grammars, and TLS, keychain, and provider clients. Web preview hands off to the system browser instead of embedding a WebView (ADR 0002), which keeps the dependency surface identical across platforms; PNG decoding and tree-sitter languages use explicit minimal features, so new UI, preview, or grammar capabilities must be evaluated against the CI dependency-tree and size reports.

Provider endpoints, models, and enablement live in `Termior-settings.json`; API keys are written to the OS keychain through the settings window only, and are never serialized into settings or session files.

#### Auto-update

**Settings → About** exposes an auto-update toggle (on by default), a manual check, and release notes. The app checks GitHub stable releases 15 seconds after launch and every 6 hours after that, downloads the installer matching the OS and CPU architecture, and verifies its SHA-256. When the download completes, the title bar shows **Update available**; open About and choose **Install update…** to launch the system installer. Finish or stop terminal tasks before quitting the app to complete the installation — the updater never closes the app or its terminals on its own. With auto-update off, manual checks still work.

Windows uses Inno Setup, macOS opens a DMG to drag-replace the app, and Linux uses the DEB installer. For portable builds, distributions without DEB support, or missing architecture packages, update manually from the release download page. Network or checksum failures show an error and keep the current version; files handed to the system installer stay in the system temp directory and can be deleted after installation. Checksums only verify file integrity — the current Windows and macOS installers are not yet signed.

### Privacy and security

- No telemetry, no account, no automatic uploads;
- Local providers can be used fully offline;
- Sensitive paths such as `.env`, `.ssh`, and credentials are rejected in both directions after canonicalization;
- Cloud provider egress always passes URL and post-resolution IP SSRF checks;
- File writes never land on disk directly from the model — they must pass tool approval and hunk review.

### Internationalization (i18n)

The interface ships in seven languages: English, Simplified Chinese, Traditional Chinese, Japanese, Korean, Spanish, and German. **Settings → General → Language** switches immediately (the main window follows the debounced save); the default **Follow system** tracks your OS locale. Translations are flat JSON tables embedded at compile time in `crates/termior-i18n/locales/` — `en.json` is the source of truth, every locale must keep an identical key set, and a CI test enforces parity plus non-empty values. See [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md) for the design.

### License

The project itself is under the [MIT License](LICENSE). Third-party dependency licences are audited with `cargo deny --exclude termior check`; [LICENSE-APACHE](LICENSE-APACHE) exists only to preserve the upstream Apache-2.0 terms of the vendored `gpui_windows`, and the GPUI upstream dependency tree is verified separately.

---

<a id="简体中文"></a>

## 简体中文

> 开源、跨平台、终端优先的 AI 原生开发工作台（ADE）。BYOK、本地优先、无账号、无遥测。

Termior 在单一原生窗口中组合真 PTY 终端、轻量代码编辑器、文件浏览器、Git、Web 预览，以及具有工具调用、审批、变更审阅与恢复能力的内置/外部 Agent。产品与工程规格见 [docs/termior-spec.md](docs/termior-spec.md)。

### 核心价值

1. **原生低延迟**：核心 UI 无 JS 运行时与 IPC 序列化边界，终端和编辑器直接使用原生 GPU 渲染，常态目标 60 fps，高刷屏目标 120 fps。
2. **终端与 AI 深度协同**：真 PTY、实时 cwd/缓冲上下文、工具调用、审批与 hunk 级 diff 审阅集中在同一工作区。
3. **本地优先与用户掌控**：BYOK、密钥只进系统钥匙串、无遥测、无账号，并支持 LM Studio、MLX、Ollama 等本地推理服务。
4. **明确的安全边界**：内置 Agent 的触盘、触 shell、触密钥和触网操作统一经过 workspace 授权、审批、deny-list 与 SSRF 策略门控；外部 Agent 则明确显示其后端、执行环境、实际隔离能力与未知项，不把后端自有操作伪装成已受内置门控。

### 当前桌面里程碑

当前分支已经从纯逻辑原型推进到可编译的 GPUI 桌面工作台，主要包含：

- 持久化工作区、8 类 tab、分栏、Explorer/Source Control/History 侧栏、状态栏、独立设置窗口；
- portable-pty + alacritty_terminal 真终端，shell integration（bash/zsh/fish/PowerShell）、OSC 7/133/777、IME、搜索、回滚滚动、URL/localhost 检测和 Windows Job Object；设置页可选默认 Shell（系统默认 / 探测到的 shell，含 Git Bash 与 WSL 发行版 / 手动路径），并可开启「新建终端时选择 Shell」；提供复制/粘贴/全选的上下文菜单；
- [SSH 会话与 SFTP](docs/ssh.md)：连接配置与侧栏快捷连接、系统凭据库、密码/MFA/密钥/agent 认证、跳板机、主机指纹校验、文件/目录上传下载与续传，并提供双栏 SFTP 浏览器与跟随活动远程 tab 的 File Explorer；
- Rope 编辑缓冲、虚拟可视区、tree-sitter 增量高亮、搜索、撤销重做、行内补全 ghost text、10 套编辑器主题和 Vim 交互层；
- 文件索引（浅层索引与防抖监听）、gitignore、模糊查找、后台流式 grep、键盘树导航与完整上下文菜单；按 tab 记忆项目文件夹并注入 `cd`，侧栏跟随该锚点；
- Git 专用 diff/history/commit-file 页签、文件/hunk stage/unstage、确认 discard、commit、branch、fetch/pull/push 与 commit graph，状态刷新已移出 UI 线程；
- Web 预览：localhost URL 检测 + URL 校验 + 系统浏览器打开（不内嵌 WebView，见 [ADR 0002](docs/adr/0002-remove-embedded-webview.md)）；Markdown 原生渲染预览与共享编辑缓冲实时同步；
- 12 套应用主题、自定义主题模型和 GPU 渲染的窗口背景虚化；
- OpenAI、Anthropic、Gemini、Groq、xAI、Cerebras、OpenRouter、DeepSeek、Mistral、OpenAI-compatible、LM Studio、MLX、Ollama 的真实 HTTP/SSE 适配；
- OS 钥匙串、会话/项目记忆、文件/图片/剪贴板/`@path` 附件、snippets/TODO，以及 Auto/Plan/Yolo 三档提交模式；
- 可序列化 Task/Turn、预算与取消、真实工具执行、审批卡片、命令超时、持久 shell、后台进程，以及 `write_file → AI diff → 逐 hunk 决策 → 原子写` 安全闭环；
- 内置 Agent 与 Codex app-server 可在 Composer 中切换；独立 `termior-agent-host` 提供结构化后端契约、Codex/ACP 适配、能力协商和执行环境描述；
- `AGENTS.md` 分层规则、Context Inspector、Agent Skills、MCP stdio/Streamable HTTP、受限 Hooks、可审阅 Memory、自定义 Agent 与子任务编排；
- journal/snapshot 恢复、Recovery Center、内容寻址 checkpoint、direct/worktree/sandboxed 执行环境，以及任务树与自动化排队入口；
- 内置/终端 Agent 统一通知路由：可见时抑制、隐藏时主题 toast、窗口失焦时系统通知，并在 header 铃铛列出当前状态；
- Claude Code OSC hooks 的安全、幂等安装与卸载。

这仍是阶段性里程碑，不等于整份 spec 已最终验收。基础桌面范围见 [docs/desktop-milestone.md](docs/desktop-milestone.md)；Agent A–E 的实现证据与平台 sandbox、远端 MCP OAuth、真实 ACP 客户端和自动化后台执行等边界见 [docs/ai-agent-implementation-status.md](docs/ai-agent-implementation-status.md)。

最新发布 tag 为 `v0.1.8`，工作区版本号定义在 [Cargo.toml](Cargo.toml)。

### Workspace

```text
crates/
  termior                  GPUI 桌面应用与视图接线
  termior-ui               tab/sidebar/workspace 持久状态
  termior-ui-kit           通用控件、pane 布局与共享搜索模型
  termior-terminal         PTY 会话、进程生命周期和字节桥
  termior-ssh              OpenSSH 连接配置、认证选项与 SFTP 传输命令
  termior-terminal-core    OSC、shell integration、搜索
  termior-editor           Rope、tree-sitter、Vim、补全状态
  termior-explorer         文件索引、树、搜索、watcher
  termior-explorer-core    模糊匹配与 glob 纯逻辑
  termior-vcs              Git 状态、diff、历史和远端操作
  termior-preview          localhost 检测与预览状态
  termior-ai               Provider、任务运行时、工具、上下文与编排
  termior-agent-host        内置/外部 Agent 后端契约、Codex、ACP 与 MCP
  termior-diff             hunk diff 与接受集应用
  termior-security         授权、deny-list、SSRF、工具门控
  termior-store            原子 JSON、迁移、设置、任务 journal 与 checkpoint
  termior-theme            中央语义色板与主题库
  termior-hooks            Claude Code hooks
  termior-platform         系统通知、外部 URL 与自动更新平台边界
  termior-bench            冷启动、内存、帧率与 PTY 吞吐门禁
```

### 构建与验证

需要 stable Rust、平台原生编译工具，以及 GPUI 对应的系统依赖。

#### Windows：先加载 MSVC 开发环境

项目编译 `libgit2-sys`、`libz-sys` 等 C 代码，依赖 `cl.exe` 能找到 C 标准库头文件（如 `time.h`）。
在 Git Bash / 普通终端中直接运行 `cargo build` 时，`INCLUDE`、`LIB`、`VCINSTALLDIR` 等变量为空，
`cl.exe` 会报 `fatal error C1083: Cannot open include file: 'time.h'`（cc-rs 日志里表现为
`command did not execute successfully (status code exit code: 2)`）。因此构建前需先激活 MSVC 环境。

任选一种方式，使 `INCLUDE` / `LIB` / `VCINSTALLDIR` 不再为空：

- **x64 Native Tools Command Prompt for VS 2022**（最简单）：从开始菜单打开，其中已内置上述变量，直接执行下方命令即可。
- **PowerShell / cmd**：先运行 `vcvars64.bat` 再编译：
  ```bat
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
  cargo build
  ```
- **Git Bash**：用 cmd 包一层，确保 `.bat` 在正确的解释器里执行：
  ```bash
  cmd //c '"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" && cargo build'
  ```

验证环境是否就绪：

```bat
echo %INCLUDE%   &  rem 应指向 MSVC 头文件与 Windows SDK 的 ucrt/shared/um 目录
echo %LIB%       &  rem 应指向对应的库目录
echo %VCINSTALLDIR%
```

#### 验证

以下主要命令与 CI 的 core/desktop 分工保持一致：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude termior -- -D warnings
cargo test --workspace --exclude termior --all-targets
cargo test -p termior-ui-kit --no-default-features
cargo check -p termior
cargo clippy -p termior --all-targets -- -D warnings
cargo test -p termior --all-targets
cargo build -p termior --release
cargo llvm-cov --workspace --exclude termior --tests --fail-under-lines 80
cargo bench -p termior-editor --bench large_file -- --quick
cargo bench -p termior-bench --bench pty_throughput -- --quick
```

CI 按同一划分执行：`ci.yml` 包含图标生成校验、三平台 core job、desktop job（check、clippy、测试、release 构建、二进制体积门禁、Windows 渲染烟测、分栏烟测、NFR 进程门禁）、80% 行覆盖率的 coverage job，以及带 PTY 吞吐门禁的 benchmark job；`audit.yml` 在每次 push 与每周定时运行 `cargo-deny`，捕捉许可与 CVE 漂移。

#### 应用图标

应用图标以 `assets/termior-logo.svg` 为唯一源文件。修改 SVG 后，使用 Node.js 22+
重新生成 Windows ICO、macOS ICNS、通用 PNG 与 Linux hicolor 资源：

```bash
npm ci
npm run icons
npm run icons:check
```

#### 运行桌面应用

```bash
cargo run -p termior
```

#### 发布

发布 tag（`v<workspace-version>`）会触发三平台 release 构建，产出 portable ZIP/TAR 与 SHA-256 校验文件，并附带各平台安装包：Windows 为 Inno Setup 安装器 `*-setup.exe`，macOS 为拖拽安装的 `.dmg`，Linux 为 `cargo-deb` 生成的 `.deb`。流水线按 Spec NFR-05 将单二进制限制为 60 MiB，并将压缩包/安装包限制为 100 MiB；本地可用 `scripts/check-release-binary.ps1` 和 `scripts/package-release.ps1` 复现检查与打包（Windows 安装器需本机装有 Inno Setup 6）。

Release 体积主要来自 GPUI 原生渲染栈、终端/VTE、按语言引入的 tree-sitter grammar，以及 TLS、钥匙串和 Provider 客户端。Web 预览统一交系统浏览器打开、不内嵌 WebView（ADR 0002），三平台依赖面一致；PNG 解码和 tree-sitter 语言均采用显式最小 feature，新增 UI、预览或语法能力时须结合 CI 的依赖树和体积报告评估增量。

Provider 的 endpoint、模型和启用状态保存在 `Termior-settings.json`；API key 只通过设置窗口写入 OS 钥匙串，绝不会序列化进设置或会话文件。

#### 自动更新

设置 → **About** 提供自动更新开关（默认开启）、手动检查和发行说明入口。应用在启动15 秒后、此后每 6 小时检查 GitHub 稳定版；发现新版本时按系统和 CPU 架构下载安装包，并核验 SHA-256。下载完成后标题栏显示 **Update available**，点击进入 About，选择 **Install update…** 打开系统安装程序。请先结束终端任务，再关闭应用完成安装；更新器不会自动关闭应用或终端。关闭自动更新后仍可手动检查。

Windows 使用 Inno Setup，macOS 打开 DMG 后拖拽替换应用，Linux 使用 DEB 安装器。便携版、不支持 DEB 的发行版、缺少对应架构安装包时，请使用发行下载页手动更新。网络或校验失败会显示错误并保留当前版本；交给系统安装器的文件保留在系统临时目录，安装完成后可删除。校验和只用于检查文件完整性；当前 Windows 与 macOS 安装包尚未签名。

### 隐私与安全

- 无遥测、无账号、无自动上传
- 本地 Provider 可离线使用
- `.env`、`.ssh`、credentials 等敏感路径在 canonicalize 后双向拒绝
- 云 Provider 出网统一经过 URL 和解析后 IP 的 SSRF 检查
- 写文件不会由模型直接落盘，必须经过工具审批和 hunk 审阅

### 国际化（i18n）

界面内置七种语言：英语、简体中文、繁體中文、日本語、한국어、Español、Deutsch。**设置 → General → Language** 即选即切（主窗口随防抖保存同步）；默认「跟随系统」使用操作系统语言。翻译表是编译期内嵌的 flat JSON（`crates/termior-i18n/locales/`），`en.json` 为唯一事实来源，各语言键集必须完全一致，CI 测试强制校验键奇偶与非空值。设计见 [ADR 0007](docs/adr/0007-i18n-embedded-json-tables.md)。

### 许可

项目本身采用 [MIT License](LICENSE)。第三方依赖许可由 `cargo deny --exclude termior check` 审计；[LICENSE-APACHE](LICENSE-APACHE) 仅为 vendored `gpui_windows` 保留其上游 Apache-2.0 条款，GPUI 上游依赖树另行核验。
