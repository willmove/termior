# ADR 0007 — 界面国际化：内嵌 JSON 翻译表 + 轻量运行时

- 状态：已采纳（Accepted）
- 日期：2026-09-22
- 决策者：Termior 维护者
- 相关：`docs/termior-spec.md` §3.1（版本与依赖纪律）、NFR-05（体积门禁）、CONTEXT.md（领域词汇表）

## 背景

Termior 的界面文案长期中英混杂：`workspace_view` / `settings_view` 等以英文硬编码为主，`ssh_view` / `sftp_browser` 则整段简体中文，`termior-ui-kit/src/text.rs` 集中了一部分常量并注明「方便日后接入 i18n」。用户至少需要简体中文、繁体中文、英文等 6 种主流语言的完整界面。

可选项：

1. **fluent（Mozilla）+ i18n-embed**：CLDR 完整复数/性别/选择器，生态成熟；但依赖链长（fluent-syntax、unic-langid、rust-embed、parking_lot…），运行时按需加载与懒初始化逻辑复杂，对六语言规模是过度设计。
2. **rust-i18n**：宏 + 编译期内嵌，社区常用；但 `t!` 返回 `Cow<String>` 每帧取值有分配，且复数规则支持有限，宏的全局状态不易与 GPUI 的逐帧渲染配合。
3. **自建轻量运行时**：flat JSON 表 + `Arc<str>` 存储 + 原子 locale 槽位。

## 决策

**新增 `crates/termior-i18n`（零 GPUI 依赖），翻译表以 `locales/*.json` 在编译期内嵌，运行时提供 `text`/`format_key`/`plural` 三个查询原语；应用层 `termior` crate 用 `t!`/`tf!`/`tn!` 三个宏把返回值包成 `SharedString`。**

要点：

- **语言集合**：en、zh-CN、zh-TW、ja、ko、es、de（7 种，覆盖需求的 6 种并多一种主流欧语）。BCP 47 风格标签经 `canonicalize` 归并（`zh-Hans`/`zh-SG`→`zh-CN`，`zh-Hant`/`zh-HK`→`zh-TW`，地区后缀忽略）；「跟随系统」用 `sys-locale` 检测（已是依赖树既有 crate）。
- **热路径零分配**：表值解析后存 `Arc<str>`，`text()` 返回引用计数克隆；`SharedString: From<Arc<str>>` 让逐帧取文案不产生堆分配（对齐 NFR-03 帧率纪律）。带占位符/复数的 `tf!`/`tn!` 才构建 `String`，只用于动态消息。
- **回退链**：当前语言 → 英文 → 键名本身。缺翻译不 panic、不空白；奇偶校验测试强制各语言键集与英文完全一致、值非空。
- **复数**：语言表中仅 en/es/de 区分 one（n==1）/other，zh/ja/ko 恒 other；键名约定 `key.one`/`key.other`。新增语言若需更多 CLDR 类别（俄语 few/many），在 `plural_category` 扩充。
- **键组织**：flat 键按来源模块分命名空间（`chrome.`/`settings.`/`ssh.`/`sftp.`/`composer.`/`ws.`/…），与 JSON 分组注释一一对应。原 `text.rs` 集中模块删除，职责并入 `en.json`。
- **切换与持久化**：`Settings.language: Option<String>`（`None` = 跟随系统，serde 向后兼容）。设置页 General 顶部提供语言下拉（选项显示母语名，不翻译）；选择即 `termior_i18n::init` 并重渲染设置窗，主窗口经既有防抖保存回调（≤400ms）同步——与主题实时预览同一条路径。启动时（含 askpass 独立进程与 workspace 选择对话框）在读设置后立即初始化。
- **文案边界**（沿用 `text.rs` 的旧约定）：按钮/菜单/tooltip/aria/空状态/状态提示进翻译表；日志、panic、协议字符串（`TERMIOR_*`）、字体名、元素 id 不进。

## 理由

1. **依赖纪律**：新 crate 只依赖 `serde_json`（工作区已有）与 `sys-locale`（依赖树既有），无 fluent/ICU 传递面，过 cargo-deny 与 NFR-05 体积门禁无压力。
2. **与 GPUI 的所有权模型契合**：`Arc<str>` → `SharedString` 的引用计数升级是零成本桥梁，避免了每帧 `String` 分配或 `Cow` 生命周期纠缠。
3. **可测性**：运行时是无状态查询 + 一个原子槽位，单测可覆盖解析、回退、插值、复数与奇偶校验；翻译表本身是数据，CI 用测试守门而非人工 review。
4. **翻译协作**：flat JSON 对社区译者与翻译平台（Crowdin/Transifex 均支持 JSON）友好；en.json 是唯一事实来源。

## 后果

- 新增 UI 文案必须同时在 7 个 `locales/*.json` 补齐（奇偶校验测试会拦下漏翻）；贡献指南需注明。
- 复数能力是 CLDR 的简化子集，扩展语言集合时需人工核对 `plural_category` 规则。
- `include_str!` 内嵌意味着翻译更新需重新编译发行；桌面应用的发布节奏（GitHub Releases）下可接受。
