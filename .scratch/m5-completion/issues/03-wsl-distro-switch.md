# 03 — WSL 发行版切换（Windows）

**What to build:** Windows 上能枚举已安装的 WSL 发行版，选择某个 distro 后 spawn 进入它的 PTY，shell integration（OSC 7/133）在该 distro 下生效。

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [x] 枚举本机已安装 WSL 发行版 — `list_wsl_distributions()` + UTF-16LE/BOM 解析
- [ ] 新建终端 tab 时可选择目标 distro — **延后**：无 GPUI distro picker UI，目前只能手改 settings.json 的 `wsl_distribution`（后续 ticket）
- [x] 选定 distro 后正确 spawn 到该 distro 的默认 shell — WSL 模式强制 Linux shell（默认 Bash），丢弃 pwsh/powershell/cmd
- [x] shell integration（OSC 7/133/777）在 WSL shell 下生效，cwd 嗅探正确 — bash/zsh 经 `windows_to_wsl_path` 转 `/mnt/`；OSC 777 为既有缺口
- [x] 切换 distro 时不污染 Windows 原生终端路径 — `config.wsl_distribution.is_some()` 干净分流

