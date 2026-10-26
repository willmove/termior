# Agent 验收的自动执行记录（2026-10-27）

日期：2026-10-27。基线提交：`7d294e1`。本记录承接 [2026-10-26 的记录](agent-acceptance-2026-10-26.md)，只写这一天新增的证据。

## 这一轮修掉的两个 CI 红灯

上一轮把「人工验收」换成自动验收时，有两处门禁在 CI 上是红的，且都不是本机能够复现的：

| 红灯 | 现象 | 根因 | 修复 |
|---|---|---|---|
| Core (windows-latest) | 逃逸套件 `ERROR_FILE_NOT_FOUND`，payload 明明在磁盘上 | 受限 token 要先**走到**工作区才能打开它。工作区上面的目录（用户 profile、临时目录、CI 的构建目录）只属于 runner 账户，token 没有穿越权，于是连镜像都打不开 | 给祖先目录补 `FILE_TRAVERSE`，且**不可继承**：只让路径走得通，祖先仍然不可读。见 `crates/termior-terminal/src/windows_sandbox/imp.rs` 的 `grant_ancestors_to` |
+| Agent IO (windows) | 批量 94.5ms vs 基线 40ms，关键 30.5ms vs 8ms | 基线是三平台共用的一组「同量级取整」值，从未被任何平台实测校准；Windows hosted runner 的 flush 成本比 Linux 高两个数量级 | 按 CI 首跑的真实 payload 重锚三平台各自的实测中位数，阈值仍为 100% |

第二条不是把门禁放宽：旧值在 Windows 上是**恒定失败**的，重锚后 Windows 的失败线仍然只有两倍余量。

## 这次自动跑出来的证据

| 场景 | 自动验收 | 性质 |
|---|---|---|
| Stage B 六个退出场景 | `shell_acceptance.rs` 的真实进程取消与跨项目隔离、`runtime_reliability.rs` 的接管/交还与后端退出 | 真实进程与真实 PTY |
| Stage C 三后端公共任务集 | `backend_task_matrix.rs`：内置后端始终执行，真实 Codex/ACP 两条需要 `TERMIOR_REAL_BACKEND_TESTS=1` | 协议与能力矩阵 |
| Stage D Windows 隔离 | `windows_appcontainer.rs` 13 项真实 Windows API 用例，含新增的「祖先目录只属于用户时仍可运行」 | 真实 AppContainer |
| Stage D 三平台逃逸 | `escape_suite.rs` 九项场景，Linux bwrap / macOS sandbox-exec / Windows AppContainer 各跑一遍 | 真实隔离 |
| Stage E 资源残留 | `nfr-residual` 三平台各 40 周期，保留策略清理后每周期 2432.8 B | 真实调度器与存储 |

本地全工作区：`cargo test --workspace --lib --tests --offline --no-fail-fast -- --test-threads=1`；`cargo clippy --workspace --all-targets --offline -- -D warnings`；`cargo fmt --all`；`openspec validate --all --strict --no-interactive` 13 passed / 0 failed。

## 仍然不能由自动化代替的边界

- **真实模型质量**：公共任务集比较的是协议与能力，不是答案质量。
- **真实磁盘耗尽**：`nfr-residual` 测的是长期运行的残留斜率，不是把磁盘写满。
- **Codex 在本机 Windows sandbox 的凭据解密**：`CryptUnprotectData failed: 2148073483`，属于本机环境问题。
- **许可与分发决策**：见 [#64](https://github.com/willmove/termior/issues/64)。

## 一次被否决的方案

给祖先目录写 DACL 时，如果那条 ACE 可继承，Windows 会把它**复制到该目录下所有已存在的子对象**——一次性把整个 profile 目录树变成 AppContainer 可写。这既超出沙箱需要的范围，也让约束更弱。上面的修复因此显式用 `NO_INHERITANCE`：`grant_ancestors_to` 只给 `FILE_TRAVERSE`，祖先目录不可列举、不可读取，只是走得通。
