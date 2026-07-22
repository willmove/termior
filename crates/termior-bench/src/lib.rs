//! `termior-bench` — NFR 基准 harness、基线存储与回归门禁（spec §8 非功能需求）。
//!
//! 本 crate 提供：
//! - [`Sample`] / [`MetricKind`]：单次基准测量的结果模型（spec NFR-01~04）。
//! - [`Baseline`] / [`BaselineSet`]：固化基线（`docs/baselines/nfr-baselines.json`）。
//! - [`compare`]：把当前测量与基线对比，超阈值即判定回归（CI fail 依据）。
//! - [`RegressionReport`]：人/机可读的回归报告（JSON + 文本）。
//!
//! **设计原则**：对比与门禁逻辑是纯函数、平台无关、可在 headless CI 确定性运行；
//! 实际采样（启动耗时 / 帧率 / 吞吐）由平台相关的 harness 二进制与
//! criterion bench 产出 [`Sample`] 后喂入 [`compare`]。
//!
//! 阈值语义见 [`Baseline::threshold_percent`]。

#![forbid(unsafe_code)]

mod baselines;
mod compare;
mod metric;
mod payload;
/// PTY/VTE 解析的共享 harness（bench 与 `nfr-pty` 复用）。
pub mod vte_harness;

pub use baselines::{Baseline, BaselineSet};
pub use compare::{compare, Regression, RegressionReport, Verdict};
pub use metric::{MetricKind, Sample};
pub use payload::{
    parse_harness_report, HarnessReport, NfrPayload, PtyThroughputPayload, RunPayload,
};
pub use vte_harness::{new_term, synthetic_pty_output};

#[cfg(test)]
mod tests;
