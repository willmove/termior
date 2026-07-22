//! 基线存储模型与查找（`docs/baselines/nfr-baselines.json`）。
//!
//! 基线是**固化的一组数字**：CI 每次把当前测量与基线对比，回归超阈值即 fail。
//! 这让性能回归有可对比证据，而非凭「感觉流畅」（ticket 目标）。

use crate::metric::{MetricKind, Sample};
use serde::{Deserialize, Serialize};

/// 单条基线：对齐一个 `(kind, platform, scenario)` 的目标值与回归阈值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub kind: MetricKind,
    pub platform: String,
    pub scenario: String,
    /// 基线测量值（与 [`Sample::value`] 同基本单位）。
    pub value: f64,
    /// 回归阈值：当前测量相对基线允许的**百分比**偏差（如 `15.0` = ±15%）。
    /// 超出即判回归（CI fail）。见 [`crate::compare`]。
    pub threshold_percent: f64,
}

impl Baseline {
    /// 构造一个基线条目。
    pub fn new(
        kind: MetricKind,
        platform: impl Into<String>,
        scenario: impl Into<String>,
        value: f64,
        threshold_percent: f64,
    ) -> Self {
        Self {
            kind,
            platform: platform.into(),
            scenario: scenario.into(),
            value,
            threshold_percent,
        }
    }
}

/// 一组基线（整份 `nfr-baselines.json` 反序列化目标）。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineSet {
    /// 基线集人类可读说明（来源、采集机器、日期等）。
    #[serde(default)]
    pub notes: String,
    pub baselines: Vec<Baseline>,
}

impl BaselineSet {
    /// 按采样三元组查找对应基线。
    pub fn find(&self, sample: &Sample) -> Option<&Baseline> {
        self.baselines.iter().find(|b| {
            b.kind == sample.kind && b.platform == sample.platform && b.scenario == sample.scenario
        })
    }

    /// 从 JSON 文本解析。
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// 序列化为格式化 JSON（稳定键序，便于 diff 审查与 CI 比对）。
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        let buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
        let mut ser = serde_json::Serializer::with_formatter(buf, formatter);
        Serialize::serialize(self, &mut ser)?;
        // serde_json 序列化结果一定是 UTF-8；from_utf8 失败属于不变量违反，转 io 错误。
        String::from_utf8(ser.into_inner()).map_err(|e| {
            serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })
    }
}
