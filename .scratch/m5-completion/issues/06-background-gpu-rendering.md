# 06 — 背景图片实际 GPU 渲染

**What to build:** 设置里配置的背景图片走 GPUI GPU 纹理真实渲染（非占位 stub），且不拖垮稳态帧率（由 01 的帧率门禁验证）。

**Blocked by:** 01（NFR 基准——靠帧率门禁验证渲染不退化）

**Status:** ready-for-agent

- [ ] 背景图片经 GPU 纹理真实绘制，不再 stub
- [ ] 支持 spec 6.7 / FR-THEME 声明的背景图配置项
- [ ] 稳态帧率通过 01 定义的帧率门禁
- [ ] 背景图加载失败/缺失时优雅回退纯色
