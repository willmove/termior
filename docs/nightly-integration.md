# 被忽略的集成测试

`cargo test` 默认不运行带 `#[ignore]` 的测试。`.github/workflows/nightly.yml` 在 Ubuntu 托管 runner 上每天 03:00 UTC 运行其中可以自动执行的部分，也可以用 `workflow_dispatch` 手动触发。定时任务只从默认分支启动。`ci.yml` 不跑这些测试。

## 托管 runner 上运行

nightly 安装 `openssh-client`、`openssh-server`、Python 3 虚拟环境、`paramiko==5.0.0`，以及编译 `termior-ssh` 所需的 `pkg-config` 与 `libdbus-1-dev`（keyring 的链接依赖；凭据库测试本身不在这里跑），然后执行：

```bash
python3 -m venv "$RUNNER_TEMP/ssh-test-venv"
"$RUNNER_TEMP/ssh-test-venv/bin/python" -m pip install 'paramiko==5.0.0'
export TERMIOR_SSH_TEST_PYTHON="$RUNNER_TEMP/ssh-test-venv/bin/python"
cargo test -p termior-terminal --test ssh_roundtrip -- --ignored --nocapture
cargo test -p termior-platform live_release_download -- --ignored --nocapture
```

覆盖的测试：

- `ssh_and_sftp_real_roundtrip`、`ssh_port_forwards_real_roundtrip`（`crates/termior-terminal/tests/ssh_roundtrip.rs`）。fixture 只监听 loopback，密钥和文件都在临时目录。系统需要 PATH 上的 `ssh` 与 `sftp`。
- `live_release_download`（`crates/termior-platform/src/update.rs`）。从 GitHub 拉取当前平台已发布的稳定安装包并校验 SHA-256，不启动安装程序。Ubuntu runner 需要仓库里已有 `linux-x86_64` 的 `.deb` 与 `.sha256`。

nightly **不**设置 `TERMIOR_SSH_ASKPASS_EXE`。`ssh_roundtrip` 里那段路径会把密码和私钥口令写入系统凭据库，留在下面的手动步骤。

不要对整个 workspace 使用 `cargo test --workspace -- --ignored`。那样还会启动下面两个无法在托管 runner 上成功的测试。

## 手动运行：Codex smoke

`installed_codex_completes_the_stable_stdio_handshake`（`crates/termior-agent-host/tests/codex_smoke.rs`）需要本机 PATH 上有兼容的 Codex CLI。托管 runner 没有这份 CLI，也不能代为登录。测试只做 stdio 握手（`initialize` / `initialized`），不发起模型回合，但 CLI 本身必须能启动 `codex app-server`。

1. 安装 Codex CLI，并在需要时完成登录（`codex login`）。`codex --version` 必须是已核验的版本线：`codex-cli 0.137.x` 或 `codex-cli 0.153.x`（见 `CodexProtocol::stable_verified`）。
2. 可执行文件名必须是 `codex` 且在 PATH 上。应用用的 `TERMIOR_CODEX_PATH` 不会传给这个测试。
3. 运行：

```bash
cargo test -p termior-agent-host --test codex_smoke -- --ignored --nocapture
```

## 手动运行：系统凭据库

`first_prompt_remember_persists_only_opt_in_and_can_be_cleared`（`crates/termior-ui/src/ssh_view.rs`）会向系统凭据库写入一条隔离的测试项，并在结束时删除。`keyring` 使用各平台原生后端，没有明文回退。GitHub 托管的 Ubuntu 没有已解锁的 Secret Service 会话，所以 nightly 不跑它。

先准备一台凭据库可用的机器：

- Windows：使用系统凭据管理器，从 PowerShell 运行（不要用 Git Bash）。
- macOS：登录钥匙串已解锁。
- Linux：当前会话有 D-Bus，并且 Secret Service（GNOME Keyring 或 KWallet）已解锁。编译 `termior-ui` 还需要与 desktop CI 相同的原生库：`libasound2-dev`、`libdbus-1-dev`、`libfontconfig-dev`、`libfreetype-dev`、`libx11-dev`、`libx11-xcb-dev`、`libxcb1-dev`、`libxkbcommon-dev`、`libxkbcommon-x11-dev`、`libwayland-dev`、`libegl1-mesa-dev`、`libudev-dev`。

然后：

```bash
cargo test -p termior-ui first_prompt_remember -- --ignored --nocapture
```

同一套凭据库还可以覆盖 `ssh_roundtrip` 里的 opt-in 复用路径。除了上面的 paramiko 虚拟环境和 `TERMIOR_SSH_TEST_PYTHON`，把 `TERMIOR_SSH_ASKPASS_EXE` 设为已构建的 Termior 可执行文件，再运行：

```bash
cargo test -p termior-terminal --test ssh_roundtrip -- --ignored --nocapture
```

这段会保存并删除测试用的密码和私钥口令，并检查连续 SSH 与 SFTP 不再向终端回显密钥。未设置 `TERMIOR_SSH_ASKPASS_EXE` 时，该分支不执行。
