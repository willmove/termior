//! 回归对比与 CI 门禁逻辑（纯函数，平台无关）。
//!
//! [`compare`] 把一批 [`crate::Sample`] 与 [`crate::BaselineSet`] 逐项对比：
//! - 有基线：当前测量相对基线的**退化百分比**超 [`crate::Baseline::threshold_percent`] 即回归。
//!   退化方向由 [`crate::MetricKind::lower_is_better`] 决定（冷启动/RSS 越小越好；
//!   帧率/吞吐越大越好）。
//! - 无基线（如新平台首次采集）：不算回归，仅记录，供下次固化基线。
//!
//! CI 据此把 [`Verdict::Failed`] 翻译成非零退出码。

use serde::{Deserialize, Serialize};

use crate::baselines::{Baseline, BaselineSet};
use crate::metric::{MetricKind, Sample};

/// 单项对比结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    /// 当前测量优于或等于基线。
    Pass,
    /// 有退化但未超阈值。
    WithinThreshold,
    /// 退化超阈值 → CI 应 fail。
    Failed,
}

/// 一项对比的详细结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regression {
    pub kind: MetricKind,
    pub platform: String,
    pub scenario: String,
    /// 基线值。
    pub baseline: f64,
    /// 当前测量值。
    pub current: f64,
    /// 退化百分比（正值=退化，负值=改善）。lower-is-better 指标退化方向已归一化。
    pub delta_percent: f64,
    /// 允许阈值（百分比，绝对值）。
    pub threshold_percent: f64,
    pub verdict: Verdict,
}

impl Regression {
    /// 退化百分比是否超阈值。
    pub fn is_regression(&self) -> bool {
        matches!(self.verdict, Verdict::Failed)
    }
}

/// 整批对比报告。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegressionReport {
    pub regressions: Vec<Regression>,
    /// 没有对应基线的采样三元组（新平台/新场景首次采集）。
    pub without_baseline: Vec<Sample>,
}

impl RegressionReport {
    /// 是否存在任一回归（CI fail 判据）。
    pub fn has_regression(&self) -> bool {
        self.regressions.iter().any(Regression::is_regression)
    }

    /// 文本摘要，给 CI 日志与本地 `cargo run` 阅读。
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for r in &self.regressions {
            let tag = match r.verdict {
                Verdict::Pass => "OK",
                Verdict::WithinThreshold => "warn",
                Verdict::Failed => "REGRESSION",
            };
            out.push_str(&format!(
                "[{tag}] {:?} {}/{}: baseline={:.3} current={:.3} delta={:+.1}% (threshold {:.0}%)\n",
                r.kind, r.platform, r.scenario, r.baseline, r.current, r.delta_percent, r.threshold_percent
            ));
        }
        for s in &self.without_baseline {
            out.push_str(&format!(
                "[new] {:?} {}/{}: {}={:.3} (no baseline yet)\n",
                s.kind,
                s.platform,
                s.scenario,
                s.kind.unit(),
                s.value
            ));
        }
        if self.has_regression() {
            out.push_str("\nNFR gate: FAILED — regression(s) exceed threshold.\n");
        } else {
            out.push_str("\nNFR gate: PASSED.\n");
        }
        out
    }
}

/// 把一组采样与基线对比。
///
/// 返回每个**有基线**的采样的 [`Regression`]，以及**无基线**的采样列表。
/// 顺序稳定（按输入顺序），便于确定性测试与稳定 diff。
pub fn compare(samples: &[Sample], baselines: &BaselineSet) -> RegressionReport {
    let mut regressions = Vec::new();
    let mut without_baseline = Vec::new();
    for sample in samples {
        match baselines.find(sample) {
            Some(baseline) => regressions.push(classify(sample, baseline)),
            None => without_baseline.push(sample.clone()),
        }
    }
    RegressionReport {
        regressions,
        without_baseline,
    }
}

/// 计算单项退化并据阈值分类。
fn classify(sample: &Sample, baseline: &Baseline) -> Regression {
    let delta_percent = degrade_percent(sample.kind, baseline.value, sample.value);
    let verdict = if delta_percent <= 0.0 {
        Verdict::Pass
    } else if delta_percent > baseline.threshold_percent {
        Verdict::Failed
    } else {
        Verdict::WithinThreshold
    };
    Regression {
        kind: sample.kind,
        platform: sample.platform.clone(),
        scenario: sample.scenario.clone(),
        baseline: baseline.value,
        current: sample.value,
        delta_percent,
        threshold_percent: baseline.threshold_percent,
        verdict,
    }
}

/// 计算退化百分比：正值=退化，负值=改善。
///
/// lower-is-better（冷启动/RSS）：`current > baseline` 即退化。
/// higher-is-better（帧率/吞吐）：`current < baseline` 即退化。
/// 基线为零时退化为「当前是否也趋于零」，避免除零。
fn degrade_percent(kind: MetricKind, baseline: f64, current: f64) -> f64 {
    if baseline == 0.0 {
        return if current == 0.0 { 0.0 } else { f64::INFINITY };
    }
    let ratio = (current - baseline) / baseline * 100.0;
    if kind.lower_is_better() {
        ratio
    } else {
        -ratio
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_is_better_increase_is_regression() {
        // 冷启动 800ms → 1000ms = +25% 退化，阈值 15% → Failed
        let sample = Sample::new(MetricKind::ColdStartMs, "windows", "first_frame", 1000.0);
        let baseline = Baseline::new(
            MetricKind::ColdStartMs,
            "windows",
            "first_frame",
            800.0,
            15.0,
        );
        let r = classify(&sample, &baseline);
        assert_eq!(r.delta_percent, 25.0);
        assert!(matches!(r.verdict, Verdict::Failed));
    }

    #[test]
    fn higher_is_better_decrease_is_regression() {
        // 帧率 60 → 50 fps：退化 = -(50-60)/60*100 = +16.7%，阈值 10% → Failed
        let sample = Sample::new(MetricKind::Fps, "windows", "steady_scroll", 50.0);
        let baseline = Baseline::new(MetricKind::Fps, "windows", "steady_scroll", 60.0, 10.0);
        let r = classify(&sample, &baseline);
        assert!((r.delta_percent - 16.6667).abs() < 0.01);
        assert!(matches!(r.verdict, Verdict::Failed));
    }
}
