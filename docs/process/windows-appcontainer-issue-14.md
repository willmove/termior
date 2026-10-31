# Issue #14 Windows AppContainer 专项验证

复核日期：2026-11-01。基线：`6f95626c9f6429da4ed6ef4008fd511f529466f6`。

## 已核实证据

- [2026-10-28 Windows Core 原始日志](https://github.com/willmove/termior/actions/runs/40631069903/job/122627312675)：`black_box_escape_suite` 在启动已暂存的 `escape-payload.exe` 时错误 2，cwd 含 `\\?\`。未执行隔离断言，不能称为逃逸测试通过。
- 当前主干已在 `wide_path` 剥离 cwd 前缀。单凭日志里的原始 cwd 不能证明 Windows API 实际收到相同字符串，也不能据此确认错误 2 根因。
- 当前主干 `probe()` 只依赖 SID/Profile 可解析；这不是进程启动、网络或凭据隔离证据。Windows 原型共享 `TermiorSandbox` SID，目录授权长期保留，`cleanup()` 空实现；跨工作区边界尚未成立。

## 修复与验证策略

保留 Windows 文件/网络/凭据 `unsupported`，请求这些能力时在 ACL 修改之前拒绝执行。进程树保留既有 Job Object 能力；不把它当作其他能力证据。能力矩阵直接使用生产 probe，无测试开关可以将生产能力提升为 Verified。

候选修复保证安全属性缓冲区活到 `CreateProcessW` 返回，显式传入可执行路径（与 cwd 一样经路径边界转换），让两种启动方式都原子加入 Job，关闭线程句柄与 stdin 父端，根进程退出/kill/drop 时终止子树。修正 `CreateFileW` 的无效句柄判定及 ACL 写入失败的缓冲区释放。

Windows escape suite 直接启动**未验证原型**，不依赖生产 Verified 标签，不因生产 fail-closed 而省略测试。它不授权整个编译输出目录，只暂存 payload 到工作区。生产拒绝行为由 `sandbox_contract` 单独断言。低层测试新增真实复制 EXE、空格与 canonical 路径、AppContainer token 证明、工作区读取及 stdin EOF；初始化失败必须失败，不记作成功。

## Windows 可复现命令

在 Windows PowerShell、Rust stable/MSVC Build Tools 环境执行：

```powershell
cargo test --locked -p termior-terminal --test windows_appcontainer -- --nocapture --test-threads=1
cargo test --locked -p termior-agent-host --test sandbox_contract --test escape_suite -- --nocapture --test-threads=1
cargo fmt --all -- --check
```

独立工作流 `.github/workflows/windows-sandbox.yml` 在真实 Windows runner 上先执行固定主干 escape suite，并将其退出码单独记录，再执行候选版本两组测试。候选测试任何失败均使作业失败；不依赖 workspace Clippy 先通过。原始日志作为 `windows-appcontainer-evidence` 上传。

## 结果与限制

本地宿主是 Linux，未安装 Rust，不能将静态审阅称为 Windows 运行验证。Windows 结果待 CI 原始日志核实。

| 边界 | 生产状态 | 验证范围/剩余工作 |
|---|---|---|
| 文件 | unsupported | 单工作区读写与父/同级目录、硬链接；共享 SID 授权累积、ACL 撤销、junction/reparse point 尚未解决 |
| 网络 | unsupported | 回环/可用私网有宿主连通对照；TEST-NET 地址失败不证明真实公网隔离，需真实公网正向对照及 IPv6/UDP 等测试 |
| 凭据 | unsupported | 重建环境与环境 secret；不能据此推导 Credential Manager、DPAPI、宿主凭据文件均不可访问 |
| 进程树 | verified（既有 Job Object） | 候选增加两种启动的 Job 生命周期；真实 Windows 退出、kill/drop 结果仍待验证 |

即便单工作区测试通过，也不能启用完整 Windows sandbox：仍须完成每环境独立身份/授权与清理及上述边界验证。Issue #14 保持开放，不因本 PR 自动关闭。
