# Termior

> 开源、跨平台、终端优先的 AI 原生开发工作台（ADE）。BYOK、本地优先、无账号、无遥测。

Termior 在单一原生窗口中组合真 PTY 终端、轻量代码编辑器、文件浏览器、Git、Web 预览，以及具有工具调用、审批和 AI diff 审阅能力的 Agent。产品与工程规格见 [docs/termior-spec.md](docs/termior-spec.md)。

## 核心价值

1. **原生低延迟**：核心 UI 无 JS 运行时与 IPC 序列化边界，终端和编辑器直接使用原生 GPU 渲染，常态目标 60 fps，高刷屏目标 120 fps。
2. **终端与 AI 深度协同**：真 PTY、实时 cwd/缓冲上下文、工具调用、审批与 hunk 级 diff 审阅集中在同一工作区。
3. **本地优先与用户掌控**：BYOK、密钥只进系统钥匙串、无遥测、无账号，并支持 LM Studio、MLX、Ollama 等本地推理服务。
4. **明确的安全边界**：所有触盘、触 shell、触密钥和触网操作统一经过 workspace 授权、审批、deny-list 与 SSRF 策略门控。

## 当前桌面里程碑

当前分支已经从纯逻辑原型推进到可编译的 GPUI 桌面工作台，主要包含：

- 持久化工作区、8 类 tab、分栏、Explorer/Source Control/History 侧栏、状态栏、独立设置窗口；
- portable-pty + alacritty_terminal 真终端，shell integration、OSC 7/133/777、IME、搜索、回滚滚动、URL/localhost 检测和 Windows Job Object；
- Rope 编辑缓冲、tree-sitter 多语言高亮、搜索、撤销重做、10 套独立编辑器主题和 Vim 交互层；
- 文件索引、gitignore、模糊查找、grep 内容搜索、文件监听与文件树状态；
- Git 状态、文件/hunk stage、commit、branch、fetch/pull/push、历史与 commit graph 核心；
- wry 子 WebView 预览（Windows/macOS）及外部浏览器降级；
- 10 套应用主题、自定义主题模型和全窗背景配置；
- OpenAI、Anthropic、Gemini、Groq、xAI、Cerebras、OpenRouter、DeepSeek、Mistral、OpenAI-compatible、LM Studio、MLX、Ollama 的真实 HTTP/SSE 适配；
- OS 钥匙串、会话/项目记忆、附件/snippets/TODO、Plan mode、子代理、自定义代理；
- 真实 Agent 工具执行、审批卡片、命令超时、持久 shell、后台进程，以及 `write_file → AI diff → 逐 hunk 决策 → 原子写` 安全闭环；
- Claude Code OSC hooks 的安全、幂等安装与卸载。

这仍是阶段性里程碑，不等于整份 spec 已最终验收。尚待继续完善的交互和非功能项记录在 [docs/desktop-milestone.md](docs/desktop-milestone.md)。

## Workspace

```text
crates/
  termior-app              GPUI 桌面应用与视图接线
  termior-ui               tab/sidebar/workspace 持久状态
  termior-ui-kit           pane 布局与共享搜索模型
  termior-terminal         PTY 会话、进程生命周期和字节桥
  termior-terminal-core    OSC、shell integration、搜索
  termior-editor           Rope、tree-sitter、Vim、补全状态
  termior-explorer         文件索引、树、搜索、watcher
  termior-explorer-core    模糊匹配与 glob 纯逻辑
  termior-vcs              Git 状态、diff、历史和远端操作
  termior-preview          localhost 检测与预览状态
  termior-ai               Provider、Agent、工具、会话和 Composer
  termior-diff             hunk diff 与接受集应用
  termior-security         授权、deny-list、SSRF、工具门控
  termior-store            原子 JSON、迁移、设置、键位
  termior-theme            中央语义色板与主题库
  termior-hooks            Claude Code hooks
  termior-platform         系统通知与外部 URL 边界
```

## 构建与验证

需要 stable Rust、平台原生编译工具，以及 GPUI/wry 对应的系统依赖。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
```

运行桌面应用：

```bash
cargo run -p termior-app
```

Provider 的 endpoint、模型和启用状态保存在 `Termior-settings.json`；API key 只通过设置窗口写入 OS 钥匙串，绝不会序列化进设置或会话文件。

## 隐私与安全

- 无遥测、无账号、无自动上传；
- 本地 Provider 可离线使用；
- `.env`、`.ssh`、credentials 等敏感路径在 canonicalize 后双向拒绝；
- 云 Provider 出网统一经过 URL 和解析后 IP 的 SSRF 检查；
- 写文件不会由模型直接落盘，必须经过工具审批和 hunk 审阅。

## 许可

Apache License 2.0。第三方依赖许可由 `cargo deny --exclude termior-app check` 审计；
GPUI 上游依赖树另行核验。
