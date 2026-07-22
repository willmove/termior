# 07 — 【决策，非实现】Linux 内嵌 WebView 去留与发行策略

**What to build:** 这是一张**决策票**，不是实现票。`docs/zed-embedded-webview-research.md` 已确认 Zed 根本不用内嵌 WebView，`gpui-wry` 是 Termior 自加的能力。需要决定：继续维护内嵌子 WebView（含 Linux 上的发行成本），还是退回"系统浏览器降级"模型。

> 建议流程：先走 `/grill-with-docs`（有 codebase，沉淀到 CONTEXT.md + ADR），输入用那份调研文档。决策清楚后再拆 implement 票（可能直接消解，或变成"移除 gpui-wry"）。在决策落地前，10（Linux WebView 发行）被它阻塞。

**Blocked by:** None — can start immediately（决策可立刻开始）

**Status:** done — 结论为「移除内嵌 WebView」，已落地为 ADR 0002 与一次实现提交。

- [x] 用 `/grill-with-docs` 消化 `zed-embedded-webview-research.md`，形成"保留 / 移除 / 平台差异化"的明确结论 → **移除**
- [x] 结论写成 ADR 落到 `docs/adr/` → `docs/adr/0002-remove-embedded-webview.md`
- [x] 依据结论拆出后续 implement 票或关闭本决策 → 已直接实现（移除 `gpui-wry`/`wry`/`raw-window-handle`，重写 `preview_view.rs` 为系统浏览器降级，删除 `scripts/preview-smoke.ps1`）

## 决策摘要

移除内嵌 WebView，统一退化为「系统浏览器降级」。保留与该能力正交的全部子系统：localhost 检测、URL 校验、系统浏览器打开、原生 Markdown 预览。详见 `docs/adr/0002-remove-embedded-webview.md`。

**对下游工单的影响**：`10`（Linux 发行）不再被「内嵌 WebView 发行策略」阻塞，其范围收窄为纯打包/签名/自动更新。
