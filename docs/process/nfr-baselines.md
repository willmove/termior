# NFR 基准 harness、基线与回归门禁

> 对应工单 `01-nfr-benchmarks-harness`；对齐 [`termior-spec.md`](../user/termior-spec.md) §8 非功能需求（NFR-01~04）。

## 目标

把「终端/编辑器感觉流畅」这种主观判断，换成**可测量、可对比、可回归**的硬指标，
并接到 CI 上自动把关：冷启动耗时、常驻 RSS、稳态帧率、键入回显、PTY 吞吐各落一组
基线数字，CI 每次把当前测量与基线对比，退化超阈值即 fail。键入回显目前在 Linux 与
macOS 上设了基线；Windows 的回显方差太大，先只记录。

## 组件

实现位于 `crates/termior-bench`：

| 组件 | 作用 |
|---|---|
| `src/lib.rs`（`MetricKind` / `Sample` / `BaselineSet` / `compare`） | 纯函数的基线对比与回归门禁逻辑，平台无关、确定性、有单测 |
| `benches/pty_throughput.rs` | criterion 统计稳态的 PTY/VTE 吞吐 bench（NFR-02） |
| `src/bin/nfr-pty.rs` | 一次性 PTY 吞吐取值，打印 `NfrPayload::PtyThroughput` JSON |
| `src/bin/nfr-run.rs`（feature `run-harness`） | 启动 release 应用，采集冷启动/RSS/FPS，打印 `NfrPayload::Run` JSON |
| `src/bin/nfr-context.rs` + `src/context_harness.rs` | 合成仓库上测仓库地图的 Agent 上下文延迟，打印 `NfrPayload::ContextLatency` JSON |
| `src/bin/nfr-gate.rs` | 读 payload + 基线，调用 `compare`，打印摘要，有回归则退出码非 0 |

应用侧插桩在 `crates/termior/src/main.rs` 的 `schedule_nfr_measurement`（`TERMIOR_NFR_MEASURE=1`）：

| 协议行 | 含义 |
|---|---|
| `TERMIOR_NFR_PHASE <name>=<ms>` | 冷启动分阶段时间戳（相对 main 入口）：`settings` / `workspace_root` / `app_init` / `fonts` / `window_created` / `workspace_view` / `runtime_restored` |
| `TERMIOR_NFR_FIRST_FRAME` | 首帧 **draw + present 之后**打印（GPUI 的 next-frame 回调先于同 tick 的 draw 执行，故在第二次回调里打印；最多多计一个刷新间隔，偏保守） |
| `TERMIOR_NFR_WARMUP_MS=NN shell_ready=0\|1` | 首帧后强制重绘预热：至少 1s，且 shell 首次输出后再稳定 1s；上限 8s |
| `TERMIOR_NFR_FPS=NN` / `TERMIOR_NFR_FPS_WINDOWS=a,b,..` / `TERMIOR_NFR_FRAME_P99_MS=NN.N` | 预热后采样 5 个 1s 窗口，**每个 tick 强制整窗重绘**（`window.refresh()` 绕过视图缓存）；报告窗口帧率中位数、逐窗口值与全程帧间隔 p99 |
| `TERMIOR_NFR_WINDOW_ACTIVE=0\|1` | 采样期间窗口是否一直在前台 |
| `TERMIOR_NFR_ECHO_P99_MS=NN.N samples=N` | 键入回显 p99（`TERMIOR_NFR_ECHO_SAMPLES=N` 时，帧率采样后运行），随后应用自行退出 |

## 四组指标怎么测

| NFR | 指标 | 单位 | 怎么测 | 单位方向 |
|---|---|---|---|---|
| NFR-01 | 冷启动 | ms | `nfr-run` 计时：进程 spawn → 应用打印 `TERMIOR_NFR_FIRST_FRAME`（首帧可交互） | 越小越好 |
| NFR-04 | 常驻 RSS | MiB | `nfr-run` 用 sysinfo 轮询子进程 `Process::memory()`，取 dwell 窗口峰值 | 越小越好 |
| NFR-03 | 稳态帧率 | fps | 应用内预热后每 tick 强制整窗重绘，5 个 1s 窗口取中位数；`nfr-run --runs 3` 再取 3 次启动的中位数 | 越大越好 |
| NFR-03 | 帧间隔 p99 | ms | 同一采样窗口内相邻 draw 的间隔 p99（60Hz ≈ 16.7ms） | 越小越好 |
| NFR-02 | 键入回显 p99 | ms | `x`/Backspace 交替写入 PTY → shell 回显进入网格 → 包含它的那一帧 `render`；不含该帧剩余 paint/present（由帧间隔 p99 覆盖） | 越小越好 |
| NFR-03 | 空闲零重绘 | frames | `scripts/idle-redraw-smoke.ps1`：`TERMIOR_IDLE_REDRAW_PROBE` 在 settle 后统计 `WorkspaceView::render` 次数，期望 `TERMIOR_IDLE_REDRAW_FRAMES=0` | 必须为 0 |
| NFR-02 | PTY/VTE 吞吐 | MiB/s | `nfr-pty`/bench：5 MiB 合成 `cat` 输出 → `vte::Processor::advance` 喂 alacritty `Term` 网格 | 越大越好 |
| Agent 上下文 | 切片延迟 p99（`context_latency_p99_ms`） | ms | `nfr-context`：调用线程（相当于 UI 线程）向 `RepoMapService` 取 1,200 token 相关切片，后台全量建索引与增量刷新期间持续采样 | 越小越好 |
| Agent 上下文 | 增量刷新（`repo_map_incremental_ms`） | ms | `nfr-context`：约 1% 文件改动加约 0.2% 改名/删除/新增后，一次后台 sweep 从请求到落定 | 越小越好 |

### 为什么帧率必须强制重绘

GPUI 的 next-frame 回调在每个显示刷新 tick 都会执行，**与这一 tick 是否真的 draw 无关**；
Termior 又要求空闲零重绘。旧口径只数回调次数，量到的是显示器刷新节拍，而不是渲染开销
（旧口径在 CI 上 macOS 40fps、Windows 58fps 即如此）。新口径按强制整窗重绘实测。
2026-10-18 的 hosted runner 水位是：macOS 稳定 60fps，Linux xvfb 为 31 或 63
（30Hz / 60Hz 的 present 上限），Windows WARP 为 16–36fps。真机 GPU 会高于 Windows 这档。
此外 GPUI 对**非前台窗口**把帧间隔限到 ~33ms：CI 窗口拿不到前台时帧率上限约 30fps，
`window_active=false` 时判读帧率要打折扣。Linux 上 31fps 那一档的 `window_active` 仍是
true，帧间隔 p99 锁在 32ms，所以它是刷新上限，不是这条约 30fps 的后台限制。

### 为什么要预热、分窗取中位数、多次启动

旧做法在首帧后立刻采样 1.5s、算一个平均值。GitHub-hosted Windows runner 上，这 1.5s
正好与 PowerShell 冷启动（.NET JIT、加载 profile、首屏输出）重叠，它和 WARP 软件
光栅化抢同一批 vCPU，PTY 输出也要在主线程消化，于是窗口里出现 100-200ms 的卡顿：
2026-09-30 ～ 2026-10-15 同一口径 26 次 CI 实测 16-36fps（中位数 21），`frame_p99_ms`
92-205ms，低帧率与高 p99 同时出现。门禁因此在代码无关的提交上偶发失败（16fps 对
基线 27，退化 40.7%，阈值 40%）。现在的测量分三层去掉这类噪声，而不放宽对真实回归的判断：

1. **预热**：首帧后先强制重绘到 shell 有输出并稳定 1s（至少 1s，最多 8s），再开始计数。
   帧率是稳态指标，启动期已经由冷启动覆盖。
2. **分窗中位数**：5 个 1s 窗口各算帧率，报告中位数。一次偶发卡顿只拉低一个窗口；
   渲染变慢则拉低所有窗口，中位数照样越线。逐窗口值写进 payload 的 `fps_windows`。
3. **多次启动中位数**：CI 用 `nfr-run --runs 3`，每次全新数据目录，冷启动/RSS/帧率等
   各取 3 次的中位数；原始测量写进 payload 的 `runs`，日志里逐次打印。

那次只改了测量，基线先留在 27 / 40%。`nfr-gate` 读到的仍是一份 `Run` payload。
随后的 25 次新口径样本里，Windows 仍有整次作业三次启动都是 16–17fps，失败线
16.2fps 还是会误伤与渲染无关的提交。2026-10-18 把 Windows FPS 重锚到实测地板，
见下面的基线表。

### 为什么 PTY 吞吐是 headless 主干项

NFR-02 的可量化内核是「VTE 解析 + 网格写入速率」，与 GPU/显示无关，故能在无显示的
ubuntu CI runner 上确定性复现，是回归门禁最稳的项。`cat` 大文件期间「UI 不掉帧」
由 NFR-03（帧率）覆盖；两者正交。

### 为什么进程级指标在三个 desktop job 上都过门禁

GPUI 需要真实显示环境才能开窗。ubuntu 的 headless benchmark job 只跑 PTY 吞吐 +
criterion bench。三个 desktop job 都跑 `nfr-run --runs 3` 并过 `nfr-gate`：

- windows / macOS：直接启动 release 二进制。
- Linux：`xvfb-run -s "-screen 0 1920x1080x24"` + openbox + Mesa lavapipe（软件 Vulkan）。
  裸 Xvfb 没有窗口管理器时窗口一帧都不会上屏，payload 只剩 RSS。`xvfb-run` 会把子进程
  stderr 折进 stdout，CI 从日志里抽出那一行 `Run` JSON 再交给门禁。

软件渲染让 Linux 的帧率 / 帧耗时偏悲观，所以 Linux FPS 基线锚在 30Hz 那一档，而不是
60Hz 那一档。冷启动和 RSS 在这套环境里同样稳定，一并进门禁。

### Agent 上下文延迟怎么测

`nfr-context` 每轮在临时目录生成 20,000 个文件的合成仓库（仓库地图的文件上限；Rust、
Python、TypeScript、Go、C 与 Markdown 轮换，另有被 `.gitignore` 排除的 `target/`），
mtime 回拨一小时，相当于已有的 checkout。随后：

1. 在调用线程上启动 `RepoMapService`，后台全量建索引期间每 2ms 取一次切片并计时；
2. 改动约 1% 的文件，外加约 0.2% 的改名、删除、新增，请求一次刷新，刷新期间继续取切片；
3. 空闲后再取 50 次；
4. 校验：文件数与改动后一致，增量 sweep 没有重读未改动文件，改名后的符号指向新路径，
   删除的文件不在索引里。校验失败时退出码非 0，CI 直接失败，不进入门禁比较。

切片延迟取全部采样的 p99：它度量 UI 线程拿上下文会不会被后台写路径挡住。增量刷新在
后台线程上，只要求有界。CI 跑 3 轮，各指标取中位数，原始轮次写进 payload 的 `runs`；
全量建索引耗时、启动耗时和 p50/max 只记录，不进门禁。三个平台各在
`Context latency (<platform>)` job 里过门禁，与 Desktop job 的帧率噪声互不影响。

### RSS 取 dwell 窗口峰值

`nfr-run` 用 sysinfo 的 `Process::memory()`（物理 RSS，即「常驻」语义）轮询，取 dwell
窗口内的**峰值**作为该次运行的 RSS。取峰值而非末值，是为了把「内存随使用增长」这类
回归也能拦下（最坏情况水位）。冷启动与帧率分别覆盖 NFR-01/03。

## 基线与门禁语义

基线固化在 [`baselines/nfr-baselines.json`](../baselines/nfr-baselines.json)。

- **门禁只看相对回归**：冷启动/RSS/FPS 的绝对值高度依赖 CI runner 硬件（同型号 runner
  也存在代际差异），所以门禁判据是「当前相对基线退化是否超 `threshold_percent`」，
  而非 spec 绝对目标。退化方向已按「越小越好 / 越大越好」归一化。
- **无基线的采样不判回归**：新平台/新场景首次采集只记录，供下次固化基线（避免
  「没有基线就 fail」的死锁）。`frame_p99_ms` 三平台仍是这种：Linux 锁在 16ms 或 32ms
  （跟 30Hz / 60Hz 两档刷新走），Windows 是 42–198ms，macOS 是 19–39ms。Windows 键入回显的
  三次中位数是 25–78ms，单次启动到过 929ms，也先不设基线。
- **spec 绝对目标是发布验收参考**（见下表），不进 CI 硬门禁，但在发版前人工核对。
- **方差大的指标锚在健康样本的差的那一端**，阈值只覆盖「比这批样本更差」的一段。
  锚在中位数上、再用阈值去罩住整段噪声，失败线会正好落在健康样本里（Windows FPS
  旧锚 27 / 40% 的失败线是 16.2fps，而这就是实测地板）。

### 当前 CI 基线（2026-10-18）

样本是 2026-10-16 至 2026-10-18 的 25 次 Desktop CI（main 与未改渲染的 PR），每次
`nfr-run --runs 3` 的中位数。失败线按 `current` 相对基线的退化百分比**严格大于**
阈值计算（等于阈值仍算通过）。

| 平台 | 指标 | 25 次范围 | 基线 | 阈值 | 失败线 | 为什么是这个数 |
|---|---|---|---|---|---|---|
| linux | 冷启动 | 152–233ms（两档，约 160 与约 230） | 233ms | 25% | > 291ms | 最差中位数 232.8ms。慢档本身很齐（216–233），25% 是慢档之外的退化 |
| linux | RSS | 183–187MiB | 188MiB | 15% | > 216MiB | 最差 187.0。lavapipe 下高于 spec 的 150MB，门禁看的是这台 runner 上的回归 |
| linux | FPS | 31 或 62–63 | 31fps | 20% | < 24.8fps | 低档每次都是 31。门禁抓住「连 30Hz 都守不住」；60Hz runner 掉到 31 与健康的 30Hz runner 无法区分 |
| linux | 回显 p99 | 12.0–16.1ms | 16.1ms | 25% | > 20.1ms | 多数在 12–13ms，两次到 15.5 / 16.1。锚在最差那次 |
| macos | 冷启动 | 190–632ms（中位 340） | 632ms | 35% | > 853ms | 旧锚 1700ms 要超过 2380ms 才失败。单次启动最慢 761ms，三次中位数最慢 632ms |
| macos | RSS | 67.9–68.7MiB | 69MiB | 20% | > 82.8MiB | 旧锚 250MiB。这 25 次的波动不到 1MiB |
| macos | FPS | 中位数 59–60（单次最低 50） | 60fps | 15% | < 51fps | 旧锚 40 / 30% 要掉到 28fps 才失败。一次启动到 50 时，另外两次仍是 59–60，中位数滤得掉 |
| macos | 回显 p99 | 18.6–24.4ms | 25ms | 30% | > 32.5ms | 75 次启动里有一次 175ms，中位数把它滤掉了 |
| windows | 冷启动 | 120–213ms | 220ms | 30% | > 286ms | 旧锚 800ms（spec 目标）在这批 CI 上要退化大约 4 倍才报。本地开发机更慢，那不是这条门禁的锚 |
| windows | RSS | 54–59MiB | 60MiB | 20% | > 72MiB | 旧锚 300MiB。慢帧率那几台 runner 的 RSS 反而略低（约 54） |
| windows | FPS | 16–36（中位 23；5 次整作业 16–17） | 16fps | 25% | < 12fps | 旧锚 27 / 40% 在 16.2fps 失败，而 16fps 是健康样本的地板（#45、#48、#52 同提交重跑即到 28–36）。75 次启动最低 14fps |

PTY 吞吐仍是 `any` / `vte_parse` / 100MiB/s / 40%，不在这批进程级样本里。

### spec 绝对目标（NFR-01~04，发布验收参考）

| NFR | spec 指标 | 来源 |
|---|---|---|
| NFR-01 | Apple Silicon < 300ms 出首帧；x86 Linux 与 Windows 10/11 < 800ms | spec §8 |
| NFR-02 | VTE 解析吞吐达 alacritty_terminal 同量级；`cat` 大文件期间 UI 不掉帧；键入回显 p99 < 16ms | spec §8 |
| NFR-03 | 常态 ≥ 60fps，高刷屏目标 120fps；空闲零重绘 | spec §8 |
| NFR-04 | 空载（1 终端 tab）< 150MB | spec §8 |

## 本地复现

```bash
# 1) PTY 吞吐（headless 可跑）
cargo run -p termior-bench --release --bin nfr-pty > pty.json
cargo run -p termior-bench --release --bin nfr-gate -- \
  --platform any --payload pty.json --baselines docs/baselines/nfr-baselines.json

# 2) 进程级（需显示；先 cargo build -p termior --release）
cargo run -p termior-bench --features run-harness --release --bin nfr-run -- \
  --binary ./target/release/termior --dwell-secs 20 --echo-samples 40 --runs 3 > run.json
cargo run -p termior-bench --release --bin nfr-gate -- \
  --platform windows --payload run.json --baselines docs/baselines/nfr-baselines.json
# Linux 把 platform 换成 linux，二进制在 xvfb-run + openbox 里启动。

# 3) Agent 上下文延迟（headless 可跑；--files / --rounds 可调）
cargo run -p termior-bench --release --bin nfr-context -- --rounds 3 > context.json
cargo run -p termior-bench --release --bin nfr-gate -- \
  --platform linux --payload context.json --baselines docs/baselines/nfr-baselines.json
```

门禁退出码：`0` = 通过，`1` = 有回归（CI 应 fail），`2` = 输入/参数错误。

## 更新基线

当一次**有意**的性能改动（重构、依赖升级）使测量稳定优于/劣于旧基线，且属可接受的新水位时：

1. 在代表性机器/CI runner 上采集几次，取中位数；
2. 更新 `docs/baselines/nfr-baselines.json` 对应条目的 `value`（必要时调 `threshold_percent`）；
3. PR 说明机器配置、采样次数与原因，附 `nfr-gate` 摘要。

> 基线不是 spec 目标——它是「当前已知良好水位」。两者区别：基线漂移要评审；
> spec 目标（§8）除非改 spec 否则不动。
