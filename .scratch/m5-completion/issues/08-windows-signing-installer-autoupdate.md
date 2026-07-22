# 08 — Windows 代码签名 + 原生安装器 + 自动更新（发行 tracer bullet 首弹）

**What to build:** Windows 端跑通"签名二进制 → 原生安装器 → 自动更新"完整链路，作为三平台发行的 tracer bullet。macOS / Linux 后续票复制此模式。

**Blocked by:** 01（NFR 基准——产物体积门禁作为发行门禁的一部分）

**Status:** ready-for-agent

- [ ] Windows release 二进制完成代码签名
- [ ] 原生安装器（MSIX/MSI/NSIS 选一）端到端安装可用
- [ ] 自动更新通道：检测新版本 → 下载 → 校验 → 安装替换
- [ ] 签名/更新产物体积通过 01 的体积门禁
- [ ] 现有 CI 的 tag→release 流水线与本链路衔接（不破坏现有三平台 portable archive）
