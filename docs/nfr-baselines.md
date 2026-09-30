# NFR 基准 harness、基线与回归门禁

> 对应工单 `01-nfr-benchmarks-harness`；对齐 `termior-spec.md` §8 非功能需求（NFR-01~04）。

## 目标

把「终端/编辑器感觉流畅」这种主观判断，换成**可测量、可对比、可回归**的硬指标，
并接到 CI 上自动把关：冷启动耗时、常驻 RSS、稳态帧率、PTY 吞吐四组基准各落一组
基线数字，CI 每次把当前测量与基线对比，退化超阈值即 fail。

## 组件

实现位于 `crates/termior-bench`：

| 组件 | 作用 |
|---|---|
| `src/lib.rs`（`MetricKind` / `Sample` / `BaselineSet` / `compare`） | 纯函数的基线对比与回归门禁逻辑，平台无关、确定性、有单测 |
| `benches/pty_throughput.rs` | criterion 统计稳态的 PTY/VTE 吞吐 bench（NFR-02） |
| `src/bin/nfr-pty.rs` | 一次性 PTY 吞吐取值，打印 `NfrPayload::PtyThroughput` JSON |
| `src/bin/nfr-run.rs`（feature `run-harness`） | 启动 release 应用，采集冷启动/RSS/FPS，打印 `NfrPayload::Run` JSON |
| `src/bin/nfr-gate.rs` | 读 payload + 基线，调用 `compare`，打印摘要，有回归则退出码非 0 |

应用侧插桩在 `crates/termior/src/main.rs` 的 `schedule_nfr_measurement`（`TERMIOR_NFR_MEASURE=1`）：

| 协议行 | 含义 |
|---|---|
| `TERMIOR_NFR_PHASE <name>=<ms>` | 冷启动分阶段时间戳（相对 main 入口）：`settings` / `workspace_root` / `app_init` / `fonts` / `window_created` / `workspace_view` / `runtime_restored` |
| `TERMIOR_NFR_FIRST_FRAME` | 首帧 **draw + present 之后**打印（GPUI 的 next-frame 回调先于同 tick 的 draw 执行，故在第二次回调里打印；最多多计一个刷新间隔，偏保守） |
| `TERMIOR_NFR_FPS=NN` / `TERMIOR_NFR_FRAME_P99_MS=NN.N` | 采样 1.5s，**每个 tick 强制整窗重绘**（`window.refresh()` 绕过视图缓存）后的帧率与帧间隔 p99 |
| `TERMIOR_NFR_WINDOW_ACTIVE=0\|1` | 采样期间窗口是否一直在前台 |
| `TERMIOR_NFR_ECHO_P99_MS=NN.N samples=N` | 键入回显 p99（`TERMIOR_NFR_ECHO_SAMPLES=N` 时，帧率采样后运行），随后应用自行退出 |

## 四组指标怎么测

| NFR | 指标 | 单位 | 怎么测 | 单位方向 |
|---|---|---|---|---|
| NFR-01 | 冷启动 | ms | `nfr-run` 计时：进程 spawn → 应用打印 `TERMIOR_NFR_FIRST_FRAME`（首帧可交互） | 越小越好 |
| NFR-04 | 常驻 RSS | MiB | `nfr-run` 用 sysinfo 轮询子进程 `Process::memory()`，取 dwell 窗口峰值 | 越小越好 |
| NFR-03 | 稳态帧率 | fps | 应用内每 tick 强制整窗重绘，采样 1.5s 计真实 draw 帧数 | 越大越好 |
| NFR-03 | 帧间隔 p99 | ms | 同一采样窗口内相邻 draw 的间隔 p99（60Hz ≈ 16.7ms） | 越小越好 |
| NFR-02 | 键入回显 p99 | ms | `x`/Backspace 交替写入 PTY → shell 回显进入网格 → 包含它的那一帧 `render`；不含该帧剩余 paint/present（由帧间隔 p99 覆盖） | 越小越好 |
| NFR-03 | 空闲零重绘 | frames | `scripts/idle-redraw-smoke.ps1`：`TERMIOR_IDLE_REDRAW_PROBE` 在 settle 后统计 `WorkspaceView::render` 次数，期望 `TERMIOR_IDLE_REDRAW_FRAMES=0` | 必须为 0 |
| NFR-02 | PTY/VTE 吞吐 | MiB/s | `nfr-pty`/bench：5 MiB 合成 `cat` 输出 → `vte::Processor::advance` 喂 alacritty `Term` 网格 | 越大越好 |

### 为什么帧率必须强制重绘

GPUI 的 next-frame 回调在每个显示刷新 tick 都会执行，**与这一 tick 是否真的 draw 无关**；
Termior 又要求空闲零重绘。旧口径只数回调次数，量到的是显示器刷新节拍，而不是渲染开销
（旧口径在 CI 上 macOS 40fps、Windows 58fps 即如此，58 也因此残留成了 Windows 的
过时锚点）。新口径按强制整窗重绘的实测重锚：macOS 40（实测 50-56）、Windows 27
（GitHub-hosted runner 走 WARP 软件光栅化，实测 20-27；真机 GPU 下远高于此）。
此外 GPUI 对**非前台窗口**把帧间隔限到 ~33ms：CI 窗口拿不到前台时帧率上限约 30fps，
`window_active=false` 时判读帧率要打折扣。

### 为什么 PTY 吞吐是 headless 主干项

NFR-02 的可量化内核是「VTE 解析 + 网格写入速率」，与 GPU/显示无关，故能在无显示的
ubuntu CI runner 上确定性复现，是回归门禁最稳的项。`cat` 大文件期间「UI 不掉帧」
由 NFR-03（帧率）覆盖；两者正交。

### 为什么进程级指标（冷启动/RSS/FPS）只在 windows/macos CI 跑

GPUI 需要真实显示环境才能开窗。ubuntu CI runner（headless）只跑 PTY 吞吐 + criterion
bench；windows/macos 的 desktop job 启用 `nfr-run` 并过门禁。Linux desktop job 在
`xvfb-run` + Mesa lavapipe（软件 Vulkan）下运行 `nfr-run`，**只记录不判回归**
（`continue-on-error`，且尚无 `linux` 基线条目）。软件渲染会让帧率/帧耗时偏悲观，冷启动与
RSS 更有参考价值；数字稳定后再固化为 `linux` 平台基线。

### RSS 取 dwell 窗口峰值

`nfr-run` 用 sysinfo 的 `Process::memory()`（物理 RSS，即「常驻」语义）轮询，取 dwell
窗口内的**峰值**作为该次运行的 RSS。取峰值而非末值，是为了把「内存随使用增长」这类
回归也能拦下（最坏情况水位）。冷启动与帧率分别覆盖 NFR-01/03。

## 基线与门禁语义

基线固化在 [`baselines/nfr-baselines.json`](baselines/nfr-baselines.json)。

- **门禁只看相对回归**：冷启动/RSS/FPS 的绝对值高度依赖 CI runner 硬件（同型号 runner
  也存在代际差异），所以门禁判据是「当前相对基线退化是否超 `threshold_percent`」，
  而非 spec 绝对目标。退化方向已按「越小越好 / 越大越好」归一化。
- **无基线的采样不判回归**：新平台/新场景首次采集只记录，供下次固化基线（避免
  「没有基线就 fail」的死锁）。
- **spec 绝对目标是发布验收参考**（见下表），不进 CI 硬门禁，但在发版前人工核对。

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
  --binary ./target/release/termior --dwell-secs 20 --echo-samples 40 > run.json
cargo run -p termior-bench --release --bin nfr-gate -- \
  --platform windows --payload run.json --baselines docs/baselines/nfr-baselines.json
```

门禁退出码：`0` = 通过，`1` = 有回归（CI 应 fail），`2` = 输入/参数错误。

## 更新基线

当一次**有意**的性能改动（重构、依赖升级）使测量稳定优于/劣于旧基线，且属可接受的新水位时：

1. 在代表性机器/CI runner 上采集几次，取中位数；
2. 更新 `docs/baselines/nfr-baselines.json` 对应条目的 `value`（必要时调 `threshold_percent`）；
3. PR 说明机器配置、采样次数与原因，附 `nfr-gate` 摘要。

> 基线不是 spec 目标——它是「当前已知良好水位」。两者区别：基线漂移要评审；
> spec 目标（§8）除非改 spec 否则不动。
