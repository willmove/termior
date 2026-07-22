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
}

impl MetricKind {
    /// 该指标「越小越好」还是「越大越好」，决定回归方向（[`crate::compare`]）。
    pub fn lower_is_better(self) -> bool {
        match self {
            MetricKind::ColdStartMs | MetricKind::RssMiB => true,
            MetricKind::Fps | MetricKind::PtyThroughputMiBps => false,
        }
    }

    /// 展示用的单位标签。
    pub fn unit(self) -> &'static str {
        match self {
            MetricKind::ColdStartMs => "ms",
            MetricKind::RssMiB => "MiB",
            MetricKind::Fps => "fps",
            MetricKind::PtyThroughputMiBps => "MiB/s",
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
