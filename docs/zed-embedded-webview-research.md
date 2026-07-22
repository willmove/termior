# Zed 是否使用内嵌 WebView：源码调研

> 调研日期：2026-07-22  
> 调研对象：Zed 上游 `main`（检索时 HEAD `4ebc1545d299b1270bc76813fa841357ee711b19`），以及 Termior 当前锁定的 Zed/GPUI 提交 `3565c49dad884e8437b86a1b1beaa76bfac67fc3`。

## 结论

截至上述提交，**Zed 桌面编辑器没有使用内嵌 WebView 来承载主界面，也没有内置网页预览 WebView 或可供扩展使用的 WebView API**。Zed 的桌面 UI 由 Rust 编写的 GPUI 原生绘制；打开 HTTP(S) 链接和 OAuth 登录时会交给系统默认浏览器。

Zed 曾经出现过一个使用 `wry` 渲染 Jupyter HTML 输出的原型 PR，但它没有合入，已于 2026-03-12 关闭。正式合入的替代实现会先把 HTML 转为 Markdown，再用现有 GPUI/Markdown 渲染链路显示。因此，当前 Zed 对这类 HTML 的支持不等于运行网页、JavaScript 或嵌入浏览器引擎。

这也意味着 Termior 当前的 `gpui-wry`/`wry` 子 WebView 是 Termior 自己附加在 GPUI 之上的产品能力，并不是 Zed 或 GPUI 桌面运行所必需的组成部分。

## 证据

### 1. Zed 的界面是 GPUI 原生 GPU 渲染，不是 Web UI 容器

Zed 官方介绍称，为了获得稳定的高刷新率，他们实现了自己的 UI 框架 GPUI，并针对矩形、阴影、文字、图标和图像等原语直接设计 GPU 渲染路径，而不是采用 Electron/DOM 渲染：[Leveraging Rust and the GPU to render user interfaces at 120 FPS](https://zed.dev/blog/videogame)。官方 1.0 回顾也明确说明 GPUI 是从零开始用 Rust 构建的：[Zed is 1.0](https://zed.dev/blog/zed-1-0)。

源码层面，上游当前 [Cargo.toml](https://github.com/zed-industries/zed/blob/4ebc1545d299b1270bc76813fa841357ee711b19/Cargo.toml) 与 [Cargo.lock](https://github.com/zed-industries/zed/blob/4ebc1545d299b1270bc76813fa841357ee711b19/Cargo.lock) 中均没有 `wry`、`webview2-com`、`webkit2gtk` 等内嵌 WebView 依赖。Termior 锁定的 Zed 提交也相同：[Cargo.toml@3565c49](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/Cargo.toml)、[Cargo.lock@3565c49](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/Cargo.lock)。

本地对锁定提交的完整源码及清单执行精确检索，也没有发现 `WebView`、`WebView2`、`WKWebView`、`WebKitGTK` 或 `wry` 的实现/依赖命中。

### 2. URL 与登录流程使用系统默认浏览器

GPUI 的 `App::open_url` 文档直接描述为把 URL 交给平台默认浏览器：[gpui/src/app.rs](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui/src/app.rs#L1407-L1410)。Windows 实现调用平台的 `open_target`，macOS 实现调用 `NSWorkspace.openURL`，Linux 实现调用平台 URI opener，而不是创建子 WebView：

- [Windows `open_url`](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui_windows/src/platform.rs#L542-L554)
- [macOS `open_url`](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui_macos/src/platform.rs#L678-L689)
- [Linux `open_url`](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui_linux/src/linux/platform.rs#L382-L384)

Zed 官方认证文档也明确说明登录会打开默认浏览器完成 GitHub OAuth：[Authenticate with Zed](https://zed.dev/docs/authentication#signing-in)。

### 3. `Preview Tab` 不是网页预览

Zed 文档里的 Preview Tab 是一种临时文件页签：单击文件时临时打开，切换文件时可被替换，编辑或双击后转为永久页签。它不代表浏览器预览：[Project Panel — Navigating](https://zed.dev/docs/project-panel#navigating)、[All Settings — Preview tabs](https://zed.dev/docs/configuring-zed#preview-tabs)。

Zed 的 Markdown 预览也是原生 GPUI 视图。其依赖清单包含 `gpui`、`markdown`、`ui` 等，没有 WebView 依赖：[markdown_preview/Cargo.toml](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/markdown_preview/Cargo.toml)。

### 4. `gpui_web` 不是桌面内嵌 WebView

Zed 仓库存在名为 `gpui_web` 的 crate，但它的依赖被限制在 `cfg(target_family = "wasm")`，作用是让 GPUI 以 WebAssembly/Canvas 形式运行在浏览器环境，而不是让 Zed 桌面窗口嵌入浏览器控件：[gpui_web/Cargo.toml](https://github.com/zed-industries/zed/blob/4ebc1545d299b1270bc76813fa841357ee711b19/crates/gpui_web/Cargo.toml)。

因此，看到 `gpui_web`、`web-sys` 或 `wasm-bindgen` 不能据此判断 Zed 桌面端使用了 WebView。

### 5. Zed 试验过 `wry`，但没有合入

2026-02-01，有贡献者提交了使用 `wry` 在 Jupyter/REPL 输出中渲染 HTML 的原型 [PR #48157](https://github.com/zed-industries/zed/pull/48157)。PR 记录了几个关键问题：

- 需要新增系统依赖，并且当时只在 macOS 构建过；
- WebView 会覆盖其他 GPUI 元素；
- 键盘复制和交互式内容仍有缺陷；
- `wry` 会增加所有贡献者的编译负担。

该 PR 没有合并，于 2026-03-12 以“清理队列、只是原型”为由关闭并删除分支。

正式合入的是 [PR #49646](https://github.com/zed-industries/zed/pull/49646)：将 Jupyter HTML 输出转换成 Markdown，再交给现有渲染器。当前源码仍明确写着 “Convert HTML to Markdown for rendering in the REPL”：[repl/src/outputs/html.rs](https://github.com/zed-industries/zed/blob/4ebc1545d299b1270bc76813fa841357ee711b19/crates/repl/src/outputs/html.rs)。这能显示基础标题、列表、代码和表格，但不会提供完整 HTML/CSS/JavaScript 运行环境。

### 6. 网页预览和扩展 WebView 仍属于未交付需求

官方仓库中的“在 Zed 内预览网站”请求 [Issue #10533](https://github.com/zed-industries/zed/issues/10533) 已关闭为 not planned；“扩展使用 WebView”请求 [Issue #21208](https://github.com/zed-industries/zed/issues/21208) 截至调研日仍处于开放/待分流状态。这进一步说明 WebView 不是当前正式扩展 API 或已发布内置能力。

## 对 Termior 的含义

1. **删除 `gpui-wry`/`wry` 不会破坏 GPUI 或 Zed 基础架构。** Termior 只会失去自己实现的应用内 Web Preview。
2. **改用系统浏览器与 Zed 的处理方式一致。** URL、OAuth、文档链接都可以继续通过平台 `open_external` 边界完成。
3. **原生 Markdown 预览应保留。** Zed 同样把 Markdown/基础 HTML 内容转成结构化数据后用 GPUI 绘制；这不需要 WebView。
4. **若 Termior 仍需要 Vite/Next/Astro 的实时网页与 JavaScript/HMR 体验，它将是一个明确超出 Zed 当前能力的差异化功能。** 删除 WebView 后只能在系统浏览器中获得完整网页运行环境。
5. **WebView 的已知 GPUI 合成问题不是 Termior 独有。** Zed 的 `wry` 原型也遇到了原生子窗口覆盖 GPUI 浮层、输入和跨平台依赖问题，和 Termior 当前记录的 z-order/焦点风险相互印证。

## 判断边界

- 结论针对 Zed 官方仓库及上述提交，不覆盖第三方 Zed fork、独立应用或未合并分支。
- “没有 WebView”指正式桌面产品没有嵌入浏览器引擎；不表示 Zed 不处理 HTML、HTTP、OAuth、WASM 或外部浏览器链接。
- 上游随时可能合入新的预览或扩展能力，因此后续决策应重新检查 `Cargo.lock`、扩展 API 和相关议题状态。
