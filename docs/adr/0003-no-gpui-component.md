# ADR 0003 — 不采用 gpui-component，自建轻量控件层

- 状态：已采纳（Accepted）
- 日期：2026-07-25
- 决策者：Termior 维护者（经 OpenSpec change `ui-design-system-overhaul` / design D1）
- 取代：`termior-spec.md` v0.1 §3.1「有限采用 gpui-component」与开放问题 Q2
- 相关：`openspec/changes/ui-design-system-overhaul/design.md`（D1）、`crates/termior-ui-kit`

## 背景

产品规格一度计划通过内部 `Termior-ui-kit`「有限封装」[gpui-component](https://github.com/longbridge/gpui-component)（longbridge），以复用 Dock、输入、弹层、虚拟列表等通用桌面控件，并在 M0 用三组 Release 基线决定保留范围（开放问题 Q2）。

与此同时，本仓库将 gpui 钉在 Zed `3565c49`，且已 vendor 并修补 `gpui_windows`（窗口关闭竞态等）。gpui-component 在其 `Cargo.toml` 中把 gpui 钉在不同 rev（当时为 `1d217ee`）。要对齐就要迁移 gpui rev、在新 rev 上重打 Windows 补丁并重新验证关闭路径，之后每次跟随上游升级都要重演。

## 决策

**不引入 gpui-component。** 通用桌面控件（Icon、IconButton、Tooltip、Input chrome、ListRow、Menu、EmptyState、尺寸 token 等）在 `termior-ui-kit` 内自建，可选 `gpui-kit` feature 隔离 GPUI 依赖；业务 crate 只消费 ui-kit 与 GPUI，不得直接依赖 gpui-component。

此决策关闭 `docs/termior-spec.md` 开放问题 Q2，并废止 §3.1 中「有限采用 + M0 三组基线」的路径。

## 理由

1. **rev 冲突的维护成本高于控件收益。** 对齐 gpui-component 等于把已稳定的 vendored Windows 补丁推倒重来，且长期绑定其升级节奏。
2. **按需 vendor 单组件更脏。** 其控件往往连带主题系统与内部工具函数，复制后仍要维护分叉。
3. **M0 基线实验无法改变结论。** 体积/冷启动增量未测之前，rev 冲突已足以否决引入；基线只会量化「引入后有多贵」，不会消除补丁重打成本。
4. **实际需要的控件面可控。** 当前工作区 chrome、设置窗、侧栏与 Composer 所需的通用件已能在 ui-kit 内以 token + 薄封装交付，无需整库 Dock/Tiles 栈。

## 后果

- **正向**：依赖树不含 gpui-component；Windows 补丁与 gpui rev 决策独立；控件 API 完全由本仓库拥有。
- **负向 / 取舍**：Dock、成熟虚拟列表、部分表单交互需继续自研或延后；工程量落在 `termior-ui-kit` 而非上游复用。
- **被消解的开放问题**：Q2（是否整体满足 NFR-11 / 保留哪些控件）关闭——答案是「不采用，故不适用该门槛实验」。

## 一致性变更

- `termior-ui-kit`：自建 Icon / Tooltip / IconButton / Input / ListRow / Menu / EmptyState 与尺寸 token（见 OpenSpec `ui-design-system`）。
- `docs/termior-spec.md`：§3 技术选型、§3.1、NFR-11、风险 R4/R6、开放问题 Q2 同步修订为「不采用 gpui-component」。
- 业务 crate（`termior` 等）禁止新增对 gpui-component 的依赖。

## 未来工作

- 若上游 gpui-component 与本仓库 gpui rev 长期对齐且 Windows 补丁可无摩擦合并，可重开 ADR 评估局部引入；默认仍保持自建。
