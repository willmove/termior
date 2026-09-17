# SSH 与 SFTP

连接管理有三个入口：标签栏 **＋旁的下拉菜单 → SSH / SFTP 连接…**、
**系统偏好设置 → General → SSH / SFTP**，以及左侧 **SSH / SFTP → 管理连接…**。
左侧列表点击已保存的连接名称即可打开 SSH；「打开 SFTP」启动对应文件会话。
保存、编辑或删除后列表立即更新。小窗口支持右侧滚动条拖动及滚轮，窄窗口自动上下排列；
Tab/Shift+Tab 切换输入框并滚动到焦点，长输入内容随光标横向滚动。

## 连接和认证

填写连接名称和主机。主机可填写 DNS、IPv4、IPv6 或现有 OpenSSH `Host` 别名，
不要把 `user@` 或端口拼进主机字段。用户名、端口留空时沿用 SSH config。
可设置私钥路径以及 `user@bastion:2222` 形式的跳板机；多个跳板用逗号分隔。
点击保存持久化配置；连接和选择传输文件前也会保存当前配置。

- 自动：沿用 OpenSSH 认证能力，包括 SSH config、agent 和默认密钥。
- 密码 / MFA：使用 keyboard-interactive/password。
- 密钥：使用指定密钥，支持加密私钥，保留 MFA/密码后续认证。
- SSH Agent：仅使用 agent 提供的公钥身份；先用系统 `ssh-add` 管理密钥。

点击「连接 SSH」或「打开 SFTP」，到主窗口的新标签完成主机指纹、密码、口令或
OTP 输入。首次指纹请通过可信渠道核对；已知密钥变化时 OpenSSH 拒绝连接。
勾选「使用系统凭据库保存密码和私钥口令」，填写登录密码或加密私钥口令后保存，
后续 SSH 重连和 SFTP 传输可自动复用。输入框始终遮蔽凭据；留空保留已有凭据。
Windows 使用 Credential Manager，macOS 使用 Keychain，Linux 使用系统密钥服务；
系统凭据库不可用时明确报错，不回退为明文文件。配置、环境变量、命令行和终端输出
不包含凭据。私钥文件继续由 OpenSSH/SSH Agent 使用，应用不复制私钥内容。

启用保存后，OpenSSH 通过独立认证窗口请求缺失或被拒绝的凭据；首次主机指纹仍须确认，
OTP 和任意 keyboard-interactive 提示不会从密码库自动填充。普通 password 认证优先于
keyboard-interactive（公钥仍优先）。代理/跳板连接因 OpenSSH 无法向认证助手区分来源跳，
仍需手动输入或使用 SSH Agent，避免把目标密码交给跳板机。

「清除该连接的已保存凭据」删除对应目标的密码与私钥口令；关闭保存选项并保存、
删除连接或修改目标后，会清理不再被其他已保存配置引用的旧凭据。重命名连接可继续复用，
修改主机、用户、端口、私钥或跳板机不会把旧密码带到新目标。Termior 不保存 OTP，
也不会自动删除 known_hosts 记录。

标签后台保持连接，分栏打开独立会话。断开或关闭标签会终止其连接；失败/退出保留
终端输出。重新连接为显式操作。重启只恢复连接信息，不自动登录或重放传输。
连接管理的删除只删配置，不终止既有连接。

## 文件传输

在连接管理填写「SFTP 远程路径」：上传时是目标路径，下载时是源路径。
点击「选择上传…」或「下载到…」，通过系统文件对话框选本地文件，核对显示的
方向、路径和覆盖说明后点击「确认路径并开始传输」。每个传输占一个独立标签，
可以并行运行，在终端中查看进度和诊断；状态由 OpenSSH 的退出码判定。

勾选「整个目录」进行递归传输。已有目标目录时遵循 OpenSSH SFTP 的目录嵌套语义。
勾选「断点续传」只适用于已有内容确实是源文件相同前缀的情况；不验证这一点会
造成损坏。默认传输会覆盖目标同名文件。取消/连接中断可能留下部分文件，
之后由用户决定重新传输或续传；应用不会自动重试。

「打开 SFTP」提供交互文件管理：

| 命令 | 用途 |
|---|---|
| `pwd` / `ls -la` / `cd` | 远端目录浏览 |
| `lpwd` / `lcd` | 本地目录 |
| `put` / `get` | 上传 / 下载 |
| `put -R` / `get -R` | 目录递归传输 |
| `reput` / `reget` | 显式续传 |
| `mkdir` / `rename` / `rm` / `rmdir` / `chmod` | 远端文件管理 |
| `Ctrl+C` / `bye` | 中止操作 / 退出 |

交互操作直接遵循 OpenSSH 行为；图形传输的覆盖确认不适用于手工输入的命令。
参考：[OpenSSH SFTP 手册](https://man.openbsd.org/sftp.1)。

当当前活动 Tab 是 SSH 或 SFTP 会话时，左侧 **File Explorer** 自动显示同一主机的
远程目录，而不是本地 `project_dir`。远程 shell 提供 OSC 7 cwd 时跟随该目录；否则
从 SFTP 登录目录开始。点击文件夹进入，顶部向上箭头返回父目录；右键可新建文件/文件夹、
重命名、移动、递归删除、上传文件/文件夹或下载。上传和下载仍建立独立传输 Tab，并在
可能覆盖现有目标前确认。切回本地 Tab 后，本地文件树立即恢复。

Explorer 操作通过 OpenSSH SFTP 的结构化 batch stdin 执行，不拼接远端 shell 命令。
它使用与连接相同的主机校验和认证助手；未保存的密码可能需要为新的 SFTP 连接再次输入，
使用 SSH Agent 或明确启用系统凭据库可避免重复输入。递归删除先枚举远端树并设置深度与
条目上限，超限即停止，不会退化为远端 shell `rm`。

## 平台和边界

本机须安装 `ssh` 和 `sftp` 并可从 PATH 启动。Windows 可使用系统 OpenSSH Client，
macOS 通常自带，Linux 可安装发行版 OpenSSH 客户端包。传输任务要求支持 `-N` 的
SFTP 客户端；验证版本为 Windows OpenSSH 9.5p2。缺少程序或版本不兼容时标签中
显示失败诊断，不自动安装或更改系统设置。

连接配置保存在应用数据目录的 `Termior-ssh.json`，schema v1。无效或较新 schema
会显示错误并禁止覆盖原文件。可选高级 JSON 字段 `connect_timeout_secs`（1–300，
默认 15）、`keepalive_secs`（1–3600，默认 30）和 `known_hosts_file`（默认空，
沿用系统配置）。默认不转发 agent、X11 或端口；这不是端口转发管理器。

SSH/SFTP 属于用户控制的远程会话；File Explorer 仅在活动远程 Tab 中切换为该主机的
SFTP 视图，Git、Composer Agent 与 `project_dir` 仍在本机。远端 OSC 7 只更新该 Tab
的远程 Explorer 路径；其他远端 OSC 和 localhost 输出不会重定向本地上下文。本轮没有
远程 Agent、远程编辑器、双栏文件树或无人值守的后台传输服务。Windows OpenSSH 会改写
字面反斜杠，因此这种罕见远端文件名被拒绝，避免传错文件。

## 验证

```powershell
cargo test -p termior-ssh -p termior-terminal -p termior-ui
cargo check -p termior
python -m venv target/ssh-test-venv
target/ssh-test-venv/Scripts/python.exe -m pip install paramiko==5.0.0
$env:TERMIOR_SSH_TEST_PYTHON = (Resolve-Path target/ssh-test-venv/Scripts/python.exe).Path
cargo test -p termior-terminal --test ssh_roundtrip -- --ignored
```

Unix 将变量设为相应 venv 的 `bin/python`。fixture 仅监听 loopback，生成临时
host key、用户密钥、known_hosts 与文件，不接触真实主机或用户 SSH 配置。
测试覆盖生产 PTY 上的密码、MFA、加密私钥认证、resize、文件字节一致性、中文/空格/方括号名称、
续传、递归传输、失败退出码与变更 host key 拒绝。
设置 `TERMIOR_SSH_ASKPASS_EXE` 为构建后的 Termior 可执行文件路径，可额外测试真实
系统凭据库中的密码/私钥口令在连续 SSH 连接及 SFTP 下载中复用；测试凭据结束后删除。
macOS/Linux 尚需在对应平台运行。

Windows 本机已通过上述真实连接测试、相关 crate 回归测试、桌面测试和 Clippy。
连接管理输入的中文/emoji 光标、IME 替换、高级配置保留、小窗口滚动和长输入光标也有 GPUI 控件测试。
本次桌面截图工具对 GPUI 主窗口与连接窗口均返回黑帧，视觉布局仍待人工复核；
可设置 `TERMIOR_OPEN_SSH_MANAGER=1` 在启动后打开管理窗口，配合隔离的
`TERMIOR_DATA_DIR` 做 UI 检查。
