# 03 — WSL 发行版切换（Windows）

**What to build:** Windows 上能枚举已安装的 WSL 发行版，选择某个 distro 后 spawn 进入它的 PTY，shell integration（OSC 7/133）在该 distro 下生效。

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] 枚举本机已安装 WSL 发行版
- [ ] 新建终端 tab 时可选择目标 distro
- [ ] 选定 distro 后正确 spawn 到该 distro 的默认 shell
- [ ] shell integration（OSC 7/133/777）在 WSL shell 下生效，cwd 嗅探正确
- [ ] 切换 distro 时不污染 Windows 原生终端路径
