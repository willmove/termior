# 09 — macOS 代码签名 + 原生安装器 + 自动更新

**What to build:** 复制 08 在 Windows 跑通的发行链路，落到 macOS：签名（Developer ID + notarization）、`.app` 打包 / dmg、Sparkle 或等价自动更新。

**Blocked by:** 08（先在 Windows 把发行 tracer bullet 跑通，再复制到 macOS）

**Status:** ready-for-agent

- [ ] macOS `.app` 完成 Developer ID 签名与 notarization
- [ ] dmg/原生安装器端到端安装可用
- [ ] 自动更新通道跑通（检测/下载/校验/安装）
- [ ] 产物体积通过 01 的体积门禁
