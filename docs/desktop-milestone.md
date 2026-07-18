# Termior 桌面里程碑说明

本文记录从 P0 纯逻辑原型推进到原生桌面应用后的阶段性交付边界。权威产品要求仍为 [`termior-spec.md`](termior-spec.md)。

## 本阶段已贯通的端到端链路

- 启动 GPUI 主窗口，恢复工作区布局并创建/恢复 PTY 终端；
- 终端 OSC cwd 驱动状态栏、Explorer 根目录、新 tab cwd 与 Agent 实时上下文；
- Explorer 打开文件进入 Rope/tree-sitter 编辑器，编辑缓冲在 tab 切换时保持；
- PTY 输出中的 localhost URL 可创建内嵌预览或走系统浏览器降级；
- 设置窗口可编辑 General/Models/Themes/Agents 关键配置，并把 API key 单独写入 OS 钥匙串；
- Composer 构建经过 secret deny-list 的附件载荷，调用真实 Provider 和只读工具；
- 危险工具暂停在审批卡片，`write_file` 仅生成 diff，逐 hunk 决策后原子写入；
- AI 写入提议自动打开独立 `ai-diff` tab；Composer 支持文件、图片、剪贴板与 nucleo `@path` chip；
- 编辑器采用 GPUI 虚拟行列表，只复制/高亮可视窗口，并用 tree-sitter `InputEdit` 增量更新语法树；
- Pane 分隔条可拖拽并持久化比例；Explorer 支持键盘树导航和后台流式 grep；
- Git 状态项打开专用 `git-diff`，支持文件/hunk stage/unstage 与确认 discard；`git-history` 和 `git-commit-file` 已接通 commit graph、搜索、文件列表与远端跳转；
- 会话、项目记忆、设置、布局、自定义代理/snippets/TODO 使用原子 JSON 存储；
- 内置 Agent 与终端代理 OSC 状态进入统一通知路由：目标可见时抑制、隐藏时显示主题 toast、窗口失焦时发送系统通知；header 铃铛列出状态并可跳转目标，Claude Code hooks 可安全安装/卸载；

## 本阶段验证门槛

发布本阶段提交前执行：

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace --all-targets`
4. `cargo build --workspace --release`
5. `cargo deny --exclude termior check`（若本机已安装 cargo-deny；GPUI 上游许可单独审计）
6. `cargo llvm-cov --workspace --exclude termior --tests --fail-under-lines 80`
7. `cargo bench -p termior-editor --bench large_file -- --quick`

GitHub Actions 另在 Windows/macOS/Linux 构建 release 桌面二进制、记录依赖树并执行 Spec NFR-05 的 60 MiB 二进制门禁；tag `v*` 经版本校验后生成三平台 portable archive、SHA-256，并以 100 MiB 压缩包上限发布 GitHub Release。

## 后续仍需完成的 spec 项

- 多窗口工作区切换与跨窗口 tab 管理；
- Markdown 专用 renderer 与预览同步；
- WSL 发行版切换、背景图片实际 GPU 渲染、语音输入；
- 行内补全 Provider 的停顿调度与 ghost text 网络接线；
- Linux 内嵌 WebView 的发行策略、代码签名与原生安装器/自动更新；
- NFR 冷启动、RSS、帧率与 PTY 吞吐的正式基准与达标证据（5 MiB 编辑器基准和二进制/压缩包体积门禁已进入 CI）。

因此，本里程碑的含义是“主要架构与安全链路已可构建、可测试、可继续迭代”，并不把尚未验收的条目宣称为完成。
