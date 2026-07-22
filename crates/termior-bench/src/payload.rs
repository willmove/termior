//! Harness 二进制与 criterion bench 产出的 JSON 契约。
//!
//! 协议：harness 二进制把 `NfrPayload` 以 JSON 打到 stdout 一行；CI 脚本读它、
//! 与 `docs/baselines/nfr-baselines.json` 比对。这样采样代码（平台相关）与门禁
//! 逻辑（纯函数，本 crate）解耦，便于在 headless CI 复用同一条门禁。

use serde::{Deserialize, Serialize};

use crate::metric::{MetricKind, Sample};

/// PTY/VTE 吞吐 bench 产出的最小载荷（criterion bench 之外也要能人工跑）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyThroughputPayload {
    /// 解析的总字节数。
    pub bytes: u64,
    /// 耗时（秒）。
    pub elapsed_secs: f64,
}

impl PtyThroughputPayload {
    /// 折算成 MiB/s 采样。
    pub fn to_sample(&self, platform: &str) -> Sample {
        let mib = self.bytes as f64 / (1024.0 * 1024.0);
        let value = if self.elapsed_secs > 0.0 {
            mib / self.elapsed_secs
        } else {
            0.0
        };
        Sample::new(MetricKind::PtyThroughputMiBps, platform, "vte_parse", value)
    }
}

/// 一次进程级 harness 运行的载荷（冷启动 / RSS / 帧率）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunPayload {
    /// 冷启动耗时（ms）。首帧可交互到进程启动。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_start_ms: Option<f64>,
    /// 常驻 RSS（MiB）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_mib: Option<f64>,
    /// 稳态帧率（fps）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
}

impl RunPayload {
    /// 按平台/场景展开成若干 [`Sample`]。
    pub fn to_samples(&self, platform: &str, scenario: &str) -> Vec<Sample> {
        let mut out = Vec::new();
        if let Some(v) = self.cold_start_ms {
            out.push(Sample::new(MetricKind::ColdStartMs, platform, scenario, v));
        }
        if let Some(v) = self.rss_mib {
            out.push(Sample::new(MetricKind::RssMiB, platform, scenario, v));
        }
        if let Some(v) = self.fps {
            out.push(Sample::new(MetricKind::Fps, platform, scenario, v));
        }
        out
    }
}

/// harness 二进制最终打印的顶层 JSON。CI 脚本据此分流到 PTY bench 或进程级 run。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NfrPayload {
    PtyThroughput(PtyThroughputPayload),
    Run(RunPayload),
}

/// 完整 harness 报告：标注平台后，CI 可直接喂给 [`crate::compare`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessReport {
    pub platform: String,
    pub payloads: Vec<NfrPayload>,
}

impl HarnessReport {
    /// 展开为采样列表。
    pub fn to_samples(&self) -> Vec<Sample> {
        let mut out = Vec::new();
        for payload in &self.payloads {
            match payload {
                NfrPayload::PtyThroughput(p) => out.push(p.to_sample(&self.platform)),
                NfrPayload::Run(r) => out.extend(r.to_samples(&self.platform, "typical")),
            }
        }
        out
    }
}

/// 解析 harness 输出的一行 JSON（容忍前后空白与多行 pretty JSON）。
pub fn parse_harness_report(json: &str) -> Result<HarnessReport, serde_json::Error> {
    serde_json::from_str(json.trim())
}
