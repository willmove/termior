# 10 — Linux 内嵌 WebView 发行策略 + 安装器 + 自动更新

**What to build:** Linux 端的发行链路。形态取决于 07 的决策：若保留内嵌 WebView，则处理 Linux 上 WebView2/WebKitGTK 的发行与依赖策略；若退回系统浏览器，则链路相应简化。

**Blocked by:** 07（WebView 去留决策决定本票范围）, 08（先跑通 Windows 发行 tracer bullet）

**Status:** needs-triage（等 07 决策落地后才 ready-for-agent）

- [ ] 依据 07 的 ADR 确定是否保留 Linux 内嵌 WebView
- [ ] 选定 Linux 原生包格式（deb/rpm/AppImage 之一或组合）
- [ ] 自动更新通道跑通
- [ ] 产物体积通过 01 的体积门禁
