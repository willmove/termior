# 桌面依赖许可专项核对（Issue #64）

核对日期：2026-10-30。基线：Termior `6f95626c9f6429da4ed6ef4008fd511f529466f6`。
固定 GPUI/Zed revision：`3565c49dad884e8437b86a1b1beaa76bfac67fc3`。
可机器读取的版本、来源、manifest SHA256 与结果见
[`desktop-license-evidence.json`](desktop-license-evidence.json)。本次不修改 Rust 业务代码、
Cargo.lock、GPUI revision、Audit 范围或 GPL 策略；#64 仍需维护者决策，不能关闭。

## 实际结果与边界

Linux 主机安装仓库指定的 Rust 1.98.1 和 cargo-deny 0.20.2，检查完整虚拟 workspace，
目标过滤为 Windows MSVC，没有排除三个桌面 crate，也没有排除 build dependencies。
这是 Windows **依赖图**检查，不是 Windows 原生构建、点击验收或安装包验收。

| 检查 | 原配置 | 本次配置 |
|---|---|---|
| licenses | 拒绝 bzip2 与三个 GPL crate；两个 GPUI crate 缺字段警告 | 仅拒绝三个 GPL crate；两个缺字段警告消失 |
| bans / sources | 通过（仍有既有警告） | 通过（仍有既有警告） |
| advisories | 失败 | 失败；不在本次修复范围 |

在线完整检查拉取的 RustSec 数据库 revision 为
`2295466c4e9df26647fd67d8fa1c705ac2fc7f81`，报告
`RUSTSEC-2026-0418`（libz-sys 1.1.29）与 `RUSTSEC-2026-0414`（pulldown-cmark 0.13.4）。
随后 offline 重放仅代表该缓存，不能复用旧记录的 advisories 通过结论。
本次不升级这些依赖、不加 advisory ignore；完整 `check` 仍失败。

复现：先安装工具并获取依赖、数据库，再运行（失败退出是当前预期）：

```sh
cargo-deny --locked --target x86_64-pc-windows-msvc check
cargo-deny --offline --locked --target x86_64-pc-windows-msvc check
cargo-deny --offline --locked --target x86_64-pc-windows-msvc check licenses
cargo tree --offline --locked --target x86_64-pc-windows-msvc -i zlog
cargo tree --offline --locked --target x86_64-pc-windows-msvc -i libbz2-rs-sys
```

## 1. GPUI：确定的元数据缺口

两个 crate 均为 0.1.0。各自 manifest 没有 `license` / `license-file`，
但固定 revision 的 Git tree 在两个目录下都明确放置了 mode `120000` 的
`LICENSE-APACHE`，blob `1cd601d0a3affae83854be02a0afdec3b7a9ec4d`，
内容为 `../../LICENSE-APACHE`。目标正文 blob 为
`461a0fe5ba20a096c281a2686da08fef79f534a3`，含 Zed 版权声明与 Apache-2.0 正文。
授权证据是 **crate 自身的许可链接及其目标**，不是从 GPUI 母库许可推断所有依赖。

固定来源：

- [gpui_shared_string 目录](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui_shared_string)
- [gpui_util 目录](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/gpui_util)
- [Apache 目标正文](https://github.com/zed-industries/zed/blob/3565c49dad884e8437b86a1b1beaa76bfac67fc3/LICENSE-APACHE)

Linux 正常解析符号链接时，原配置实际能从正文识别 Apache，表现为缺字段警告；
Windows 未启用 symlink checkout 时可能仅得到路径文本，不能依赖该扫描结果。
本次用 `licenses.clarify` 指定 Apache-2.0，并核对上游根正文和各自 manifest 的 hash，
不 vendor 代码、不忽略 crate。使用 `../../LICENSE-APACHE` 避免依赖主机的符号链接能力。
通用 allow 已有 Apache，不增加许可例外。上游理想修复是给两个 manifest 补 `license`，
取得该修复后再在独立 GPUI 升级窗口移除 clarification。

hash 按 cargo-deny 0.20.2 的换行归一化与 XXH32(seed 0) 算法计算：
正文 `0x076d9a33`、shared manifest `0x2960186d`、util manifest `0xaffbf8a1`。
**hash 是审计约束，不是新授权**。cargo-deny 在 hash 不匹配时放弃 override，可能继续
扫描已读入的文件；不能把它描述为任何源码变动都会自动失败的完整性门禁。
相同 crate 版本也可能出现在不同 Git revision，升级时必须重新核查许可链接与来源。

## 2. bzip2：已核对的宽松许可，原文不是无效 SPDX

实际 Windows 路径：`termior → gpui 0.2.2 → http_client 0.1.0 →
async-compression 0.4.42 → compression-codecs 0.4.38 → bzip2 0.6.1 → libbz2-rs-sys 0.2.5`。
上游 workspace 的 async-compression 启用了 bzip2、gzip、futures-io；这是常规依赖路径。

已下载并核对 **crates.io 发布包**：archive SHA256
`34b357333733e8260735ba5894eb928c02ecc69c78715f01a8019e7fa7f2db4c`，与 Cargo.lock 一致；
`.cargo_vcs_info.json` 指向 `f47b114db88889068c1efc8394b2e38d6a1e6dd1`。
发布包 LICENSE SHA256：`54e1fd7bd53273e601c9599eb162a004dacddee031ae297ee7109a7beb93b1c2`。

- [上游 v0.2.5 manifest](https://github.com/trifectatechfoundation/libbzip2-rs/blob/v0.2.5/Cargo.toml)
- [上游 v0.2.5 LICENSE](https://github.com/trifectatechfoundation/libbzip2-rs/blob/v0.2.5/LICENSE)
- [发布版本源码](https://docs.rs/crate/libbz2-rs-sys/0.2.5/source/)
- [SPDX 的 bzip2-1.0.6 条目](https://spdx.org/licenses/bzip2-1.0.6.html)

`bzip2-1.0.6` 是有效标识，cargo-deny 0.20.2 能解析。原拒绝原因是未获 allow，
不是解析错误。LICENSE 增加了 Rust 翻译贡献者的版权信息，末尾版本字符串为 1.1.0；
四项再分发条件及免责声明与 SPDX 模板相符，模板允许这些版权、版本和日期差异。
这些差异不支持擅自改写成 MIT 或 Zlib。

条款要求保留源码版权/条款/免责，不冒充原作者，标明源码修改，不未经许可作作者背书；
没有要求整个桌面程序采用 GPL 的条款。按已核对文本，本次只对确切版本增加
`libbz2-rs-sys@0.2.5` 例外，并把发布包完整 LICENSE 原样附入 NOTICE。
不扩大通用允许列表，不需要改变业务代码或替换压缩算法。

## 3. GPL：声明明确，部分文件不一致，仍阻断发行结论

完整 Windows 路径：`termior / termior-ui / termior-ui-kit → gpui 0.2.2 →
sum_tree 0.1.0 → ztracing 0.1.0 → zlog 0.1.0 / ztracing_macro 0.1.0`。
三者都来自上述固定 Zed revision。不是单纯 test-support 或 tracy feature 引入。

| crate | manifest SPDX | 目录中的许可链接 |
|---|---|---|
| sum_tree | Apache-2.0 | Apache |
| zlog | GPL-3.0-or-later | GPL |
| ztracing | GPL-3.0-or-later | Apache **及 GPL** |
| ztracing_macro | GPL-3.0-or-later | Apache **及 GPL** |

上游证据：[sum_tree](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/sum_tree)、
[zlog](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/zlog)、
[ztracing](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/ztracing)、
[ztracing_macro](https://github.com/zed-industries/zed/tree/3565c49dad884e8437b86a1b1beaa76bfac67fc3/crates/ztracing_macro)。

不能将“放了两个文本”直接改写成 `Apache-2.0 OR GPL-3.0-or-later`。
应请权利人明确这些 crate 和指定 revision 是否提供 Apache 授权；zlog 目前无 Apache 证据。
ztracing 的构建脚本根据 ZTRACING/ZTRACING_WITH_MEMORY 设置 cfg；未设置时 instrument
来自无操作 proc-macro，span 是空实现。但三个依赖在 manifest 中仍为非 optional，
关闭 tracy、设置 default-features=false 或依赖链接器裁剪都不能证明消除了许可问题。
proc-macro 的输出与宿主构建依赖应区别评估，也不能由此忽略 zlog/ztracing 的运行时依赖。

以下是工程估算，尚未实施或验证，不是许可兼容性结论：

| 方案 | 兼容性与工作量 | 维护者需决定 |
|---|---|---|
| 上游明确授权并修正 manifest | 本地改动小；等待授权耗时未知；升级须重放既有 GPUI 补丁 | 授权是否覆盖本 revision/全部所需 crate |
| 从 Apache sum_tree 移除非必需 instrumentation 的窄补丁 | 约 1–3 人日原型；保留 tree 算法与 API，移除 ztracing import/属性和依赖；需审计 cursor 等全部模块，跑 sum_tree 与桌面回归 | 接受维护一个 Apache sum_tree 分叉及 profiler 能力差异 |
| 在该补丁中用标准 tracing 重建观测 | 约 3–5 人日含 cfg/feature、性能与三平台验证；tracing::instrument 默认行为与原无操作宏不同 | 是否需要保留 profiling，如何保证开销不变 |
| 更换 GPUI revision/独立 GPUI 来源 | 数日至数周；必须检查完整新图，不保证天然无 GPL；重打 Windows/wgpu 补丁，符合 ADR 0003 的一致性要求 | 接受升级兼容性和长期跟随成本 |
| 更换 UI 框架或进程拆分 | 数周以上；UI 大量改写。进程边界本身不自动解决许可义务 | 产品架构与发行策略，需另外立项 |
| 保留 GPL 依赖并按相应条件分发 | 工程改动未必大，需核查完整组合、源码交付、通知及安装信息等义务 | 由维护者及适当的法律审查决定；本次不假定成立 |

优先建议：先争取明确授权；若无法取得，评估 Apache sum_tree 的窄补丁。
不能仅删除 zlog 的日志调用、只替换 proc-macro、复制 GPL 实现再换许可头，或靠扩大 allow
宣称问题已解决。本 PR 不实施这些策略方案。

## 4. 验证与打包剩余项

- `cargo test --locked -p termior-terminal-core`：53 单测 + 6/8 两组集成测试通过（67）。
- `cargo test --locked -p termior-ui-kit --no-default-features`：19 通过。
- `cargo test --locked -p termior-ui --lib`：已尝试，构建被主机缺少 dbus-1 开发库阻断，未运行测试。
- 使用完整 Windows cargo metadata，并仅把两个 GPUI crate 的 manifest 路径指向临时副本，
  将许可链接模拟为普通路径文本：两项 clarification 生效；仍只有三个 GPL 拒绝。
  改变 util manifest 后 override 失效并回到扫描警告，验证了 hash 匹配分支；不是原生 Windows 验收。
- `git diff --check` 通过；Cargo.lock 与业务源码无变更。

NOTICE 补齐本次核准的 GPUI 版权与 bzip2 文本，现有 LICENSE-APACHE 说明扩大到 NOTICE
所列 Apache 组件。打包脚本将 LICENSE-APACHE 带入归档；Windows Inno 安装包新增安装
LICENSE、LICENSE-APACHE、NOTICE。此前只展示 MIT 安装协议、未将 NOTICE 安装到目标目录。
本机未生成 EXE 安装包，需 Windows 构建后检查归档和实际安装目录。

上述是局部确定性修复，**不代表全部第三方通知已完备或可以发行**。待维护者确定 GPL
路径后，重新检查完整三平台图、全部依赖/字体/图标等资产通知及归档/安装产物，再决定
如何补完整桌面许可 CI 与发布说明。现有 Audit 排除项保持原状；绿色 Audit 不能作为发行依据。
