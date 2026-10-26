//! NFR 指标种类与单次采样模型（spec §8 NFR-01~04）。
//!
//! 所有指标统一以纳秒（时间）/ 字节（内存，RSS）/ 帧每秒（帧率）/ 字节每秒（吞吐）
//! 的**基本单位**表示。`[Sample::value]` 始终是浮点数，避免整数溢出与单位歧义。

use serde::{Deserialize, Serialize};

/// NFR 指标种类。对齐 spec §8 表中 NFR-01~04 的可测量项。
///
/// 同一种类用 `(kind, platform, scenario)` 三元组在基线里唯一定位。
/// 序列化名手工固定（不用 `rename_all`，避免 `RssMiB` 被拆成 `rss_mi_b`），
/// 让 `docs/baselines/nfr-baselines.json` 可读、稳定、跨版本兼容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MetricKind {
    /// 冷启动耗时（NFR-01）：进程启动 → 主窗口首帧可交互，单位毫秒。
    #[serde(rename = "cold_start_ms")]
    ColdStartMs,
    /// 常驻 RSS（NFR-04）：单位 MiB。
    #[serde(rename = "rss_mib")]
    RssMiB,
    /// 稳态帧率（NFR-03）：单位 fps。
    #[serde(rename = "fps")]
    Fps,
    /// PTY/VTE 吞吐（NFR-02）：单位 MiB/s（渲染跟上速率）。
    #[serde(rename = "pty_throughput_mibps")]
    PtyThroughputMiBps,
    /// 强制重绘下的帧间隔 p99（NFR-03）：单位毫秒。60fps 对应 ≈16.7ms。
    #[serde(rename = "frame_p99_ms")]
    FrameP99Ms,
    /// 键入回显延迟 p99（NFR-02）：键入写入 PTY → 回显进入的那一帧 render，单位毫秒。
    #[serde(rename = "echo_p99_ms")]
    EchoP99Ms,
    /// Agent 上下文延迟 p99：调用线程（UI）向仓库地图取一次相关切片的耗时，单位毫秒。
    /// 采样期间后台线程在全量建索引或增量刷新，测的是读路径会不会被写路径挡住。
    #[serde(rename = "context_latency_p99_ms")]
    ContextLatencyP99Ms,
    /// 仓库地图增量刷新耗时：少量文件改动/改名/删除后，后台一次 sweep 落定的墙钟时间，单位毫秒。
    #[serde(rename = "repo_map_incremental_ms")]
    RepoMapIncrementalMs,
    /// Agent 任务日志的批量写入耗时：一次流式回合的非关键事件批量落盘，单位毫秒。
    #[serde(rename = "agent_io_burst_ms")]
    AgentIoBurstMs,
    /// Agent 任务日志的关键事件写入耗时：每个关键事件都单独 flush，单位毫秒。
    #[serde(rename = "agent_io_critical_ms")]
    AgentIoCriticalMs,
    /// Agent 任务日志的重放耗时：重启时重建任务状态要读的日志量，单位毫秒。
    #[serde(rename = "agent_io_replay_ms")]
    AgentIoReplayMs,
    /// 一轮自动化运行留下的磁盘增量（每个周期）：长期运行必须保持平坦，单位字节。
    #[serde(rename = "residual_bytes_per_cycle")]
    ResidualBytesPerCycle,
}

impl MetricKind {
    /// 该指标「越小越好」还是「越大越好」，决定回归方向（[`crate::compare`]）。
    pub fn lower_is_better(self) -> bool {
        match self {
            MetricKind::ColdStartMs
            | MetricKind::RssMiB
            | MetricKind::FrameP99Ms
            | MetricKind::EchoP99Ms
            | MetricKind::ContextLatencyP99Ms
            | MetricKind::RepoMapIncrementalMs
            | MetricKind::AgentIoBurstMs
            | MetricKind::AgentIoCriticalMs
            | MetricKind::AgentIoReplayMs
            | MetricKind::ResidualBytesPerCycle => true,
            MetricKind::Fps | MetricKind::PtyThroughputMiBps => false,
        }
    }

    /// 展示用的单位标签。
    pub fn unit(self) -> &'static str {
        match self {
            MetricKind::ColdStartMs
            | MetricKind::FrameP99Ms
            | MetricKind::EchoP99Ms
            | MetricKind::ContextLatencyP99Ms
            | MetricKind::RepoMapIncrementalMs
            | MetricKind::AgentIoBurstMs
            | MetricKind::AgentIoCriticalMs
            | MetricKind::AgentIoReplayMs => "ms",
            MetricKind::RssMiB => "MiB",
            MetricKind::Fps => "fps",
            MetricKind::PtyThroughputMiBps => "MiB/s",
            MetricKind::ResidualBytesPerCycle => "B/cycle",
        }
    }
}

/// 单次基准采样结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub kind: MetricKind,
    /// 平台标签（`windows` / `macos` / `linux`）。PTY 吞吐等跨平台一致项可填 `any`。
    pub platform: String,
    /// 场景标签（如 `empty`、`typical`、`steady_scroll`、`vte_parse`）。
    pub scenario: String,
    /// 测量值（基本单位，见 [`MetricKind`]）。
    pub value: f64,
}

impl Sample {
    /// 构造一个采样，省去重复写单位推导。
    pub fn new(
        kind: MetricKind,
        platform: impl Into<String>,
        scenario: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            kind,
            platform: platform.into(),
            scenario: scenario.into(),
            value,
        }
    }
}
