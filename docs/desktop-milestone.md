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
- 会话、项目记忆、设置、布局、自定义代理/snippets/TODO 使用原子 JSON 存储；
- 内置 Agent 与终端代理 OSC 状态进入统一通知路由：目标可见时抑制、隐藏时显示主题 toast、窗口失焦时发送系统通知；header 铃铛列出状态并可跳转目标，Claude Code hooks 可安全安装/卸载；

## 本阶段验证门槛

发布本阶段提交前执行：

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace --all-targets`
4. `cargo build --workspace --release`
5. `cargo deny --exclude termior-app check`（若本机已安装 cargo-deny；GPUI 上游许可单独审计）

## 后续仍需完成的 spec 项

- Pane 分隔条拖拽、完整键盘焦点与多窗口工作区切换；
- Explorer 新建/重命名/删除的完整上下文菜单和查找器结果弹层；
- Git diff/commit/history 的完整可视化交互，而不只是业务核心与侧栏摘要；
- Markdown、git-diff、git-history、git-commit-file 和独立 ai-diff tab 的专用 renderer；
- WSL 发行版切换、背景图片实际 GPU 渲染、语音输入；
- 行内补全 Provider 的停顿调度与 ghost text 网络接线；
- Linux 内嵌 WebView 的发行策略和三平台安装包；
- NFR 冷启动、RSS、帧率、PTY 吞吐、二进制体积的正式基准与达标证据。

因此，本里程碑的含义是“主要架构与安全链路已可构建、可测试、可继续迭代”，并不把尚未验收的条目宣称为完成。
