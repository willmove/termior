# ADR 0002 — 移除内嵌 WebView，保留系统浏览器预览

- 状态：已采纳（Accepted）
- 日期：2026-07-22
- 决策者：Termior 维护者（经 `/grill-with-docs` 决策票 `.scratch/m5-completion/issues/07-webview-strategy-decision.md` 落地）
- 取代：`termior-spec.md` v0.1 中 §3（Web 预览 = `gpui-wry`/`wry` 子 WebView）、§6.6 FR-PREV-03、风险 R2 的“保留 wry”前提
- 相关：`docs/zed-embedded-webview-research.md`

## 背景

`termior-spec.md` v0.1 把 Web 预览设计为“在预览 tab 存活期间创建 `wry` 子 WebView”（§6.6 FR-PREV-03），Windows 用 WebView2、macOS 用 WKWebView，Linux 仅走系统浏览器降级。`docs/zed-embedded-webview-research.md` 随后确认：上游 Zed/GPUI **不使用** 任何内嵌 WebView，`gpui-wry`/`wry` 纯粹是 Termior 自加的产品能力，并带来一组已知缺陷——子 WebView 与 GPUI 合成存在 z-order/焦点限制、Windows 上 WebView2 同步构造会重入 Win32 消息泵导致 panic、增加三平台编译与发行成本、Linux 发行策略一直悬而未决。

围绕“继续维护内嵌 WebView 还是退回系统浏览器降级”，决策票 `07` 标记为 `needs-triage` 并阻塞了 Linux 发行票 `10`。

## 决策

**移除内嵌 WebView（`gpui-wry`/`wry` 及其在 Windows 上的 `raw-window-handle` 桥接），Web 预览统一退化为“系统浏览器降级”模型。** 保留与该能力正交的全部子系统的产品价值：

- **保留** PTY 输出中的 localhost URL 检测（`termior-preview::LocalhostDetector`，FR-PREV-01）；
- **保留** 预览 URL 校验（`normalize_preview_url`，HTTP/HTTPS-only 边界）；
- **保留** 系统浏览器打开（`termior-platform::open_external`，跨平台 `rundll32`/`open`/`xdg-open`）；
- **保留** 原生 Markdown 预览（`termior-preview::MarkdownDocument` + GPUI 渲染，不依赖任何 WebView）。

预览 tab 仍可被创建（`Cmd+P`、header 按钮、localhost pill、命令面板），但其唯一可见动作是“Open in browser”。这与上游 Zed 处理 URL/OAuth/文档链接的方式一致。

## 理由

1. **与上游对齐、消除自维护负担。** Zed 的 GPUI 从不内嵌浏览器引擎；Termior 维护 `wry` 等于长期独自承担 WebView2/WKWebView 的平台适配、版本跟踪与缺陷修复。
2. **消除已知架构风险（R2）。** z-order 遮盖、Windows WebView2/GPUI 重入 panic、焦点竞态等均随 `wry` 一并消失，无需再为预览 tab 专门禁用浮层 UI。
3. **缩小依赖与发行面。** `Cargo.lock` 不再含 `wry`/`gpui-wry`/`tao`/`webview2-com`/`webkit2gtk`，利于 NFR-05 体积门禁与 NFR-11 UI 依赖预算；Linux 发行不再被 WebView 的运行时依赖与打包问题阻塞。
4. **保留的价值远大于移除的价值。** 真正“应用内看 dev server”的差异化体验只对部分前端场景有价值，而 localhost 检测、URL 校验、系统浏览器打开、原生 Markdown 预览这些**正交**能力全部无须 WebView 即可工作。
5. **可逆性。** 若未来确有强需求，可在 `preview_view.rs` 之后重新引入独立 crate；本 ADR 记录的是“当前不内嵌”，不是“永不内嵌”。

## 后果

- **正向**：编译更快、依赖树更小、跨平台预览行为一致（一律系统浏览器）、Windows 启动路径不再有 WebView2 重入风险。
- **负向 / 取舍**：
  - 应用内不再能直接渲染 Vite/Next/Astro 的实时网页与 HMR；用户需在系统浏览器中查看。
  - 预览 tab 的“后台保活页面状态”（INV-1 对预览的含义）退化为“保留 URL 与校验状态”，页面 DOM 不再存活——这与上游一致，且 `PreviewTab` 的 `navigation_generation`/`url`/`last_error` 仍按 INV-1 语义保留。
- **被消解的工单**：决策票 `07` 关闭；Linux 发行票 `10` 不再受“内嵌 WebView 发行策略”阻塞，其范围收窄为纯打包/签名/自动更新。

## 一致性变更（本次落地）

- `crates/termior/Cargo.toml`：删除 `gpui-wry` / `wry`（Windows+macOS target）与 `raw-window-handle`（Windows target）。
- `crates/termior/src/preview_view.rs`：重写为仅渲染“Open in browser”占位面板；`initialize` / `set_active` 退化为 no-op，保留调用点签名以减少 diff。
- `crates/termior/src/workspace_view.rs`：删除 `start_preview_smoke`（专测内嵌 WebView 就绪）。
- `crates/termior/src/main.rs`：移除 `TERMIOR_PREVIEW_SMOKE_TEST` 分支。
- `scripts/preview-smoke.ps1`：删除（断言依赖内嵌 WebView 发起真实 HTTP 请求）。
- `Cargo.lock`：`wry`/`gpui-wry`/`tao`/`webview2-com` 等随之移除。
- `termior-spec.md`：§3、§6.6、风险 R2 同步修订（见正文）。
- 保留：`termior-preview`（`LocalhostDetector` / `normalize_preview_url` / `PreviewTab` / `MarkdownDocument`）、`termior-platform::open_external`、`markdown_preview_view.rs`、`scripts/markdown-preview-smoke.ps1`。

## 未来工作

- 若产品决策重新要求“应用内网页预览”，应作为独立的、可 feature-gate 的 crate 重新评估，并重开 ADR。
