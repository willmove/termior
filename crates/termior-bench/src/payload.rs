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

/// 一次进程级 harness 运行的载荷（冷启动 / RSS / 帧率 / 帧耗时 / 键入回显）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunPayload {
    /// 冷启动耗时（ms）。首帧可交互到进程启动。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_start_ms: Option<f64>,
    /// 常驻 RSS（MiB）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_mib: Option<f64>,
    /// 稳态帧率（fps），采样期间每帧强制整窗重绘。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    /// 强制重绘下的帧间隔 p99（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_p99_ms: Option<f64>,
    /// 键入回显延迟 p99（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_p99_ms: Option<f64>,
    /// 采样期间窗口是否一直处于前台。`false` 时 GPUI 把帧间隔限到 ~33ms，
    /// 帧率上限约 30fps——判读帧率前先看这一项。仅供判读，不进门禁。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_active: Option<bool>,
    /// 冷启动分阶段时间戳（相对 main 入口，ms）。仅供定位，不进门禁。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phases_ms: std::collections::BTreeMap<String, f64>,
    /// 首帧到帧率采样开始的强制重绘预热时长（ms）。仅供判读。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warmup_ms: Option<f64>,
    /// 逐个 1s 采样窗口的帧率；`fps` 是它们的中位数。仅供判读。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fps_windows: Vec<f64>,
    /// 多次启动时每次的原始测量；顶层各指标是它们的中位数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<RunPayload>,
}

impl RunPayload {
    /// 把多次启动的测量合并成一份：每个指标取各次的中位数（偶数次取中间两值均值），
    /// 原始测量放进 `runs`。单次偶发的慢启动（runner 上的后台扫描、软件光栅化抢 CPU）
    /// 被滤掉；真实回归会同时拉低每一次，中位数照样越线。
    pub fn median_of(runs: Vec<RunPayload>) -> RunPayload {
        let pick = |field: fn(&RunPayload) -> Option<f64>| median(runs.iter().filter_map(field));
        let mut phase_names: Vec<&String> = runs.iter().flat_map(|r| r.phases_ms.keys()).collect();
        phase_names.sort();
        phase_names.dedup();
        let phases_ms = phase_names
            .into_iter()
            .filter_map(|name| {
                median(runs.iter().filter_map(|r| r.phases_ms.get(name).copied()))
                    .map(|ms| (name.clone(), ms))
            })
            .collect();
        let active: Vec<bool> = runs.iter().filter_map(|r| r.window_active).collect();
        RunPayload {
            cold_start_ms: pick(|r| r.cold_start_ms),
            rss_mib: pick(|r| r.rss_mib),
            fps: pick(|r| r.fps),
            frame_p99_ms: pick(|r| r.frame_p99_ms),
            echo_p99_ms: pick(|r| r.echo_p99_ms),
            window_active: (!active.is_empty()).then(|| active.iter().all(|a| *a)),
            phases_ms,
            warmup_ms: pick(|r| r.warmup_ms),
            fps_windows: Vec::new(),
            runs,
        }
    }

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
        if let Some(v) = self.frame_p99_ms {
            out.push(Sample::new(MetricKind::FrameP99Ms, platform, scenario, v));
        }
        if let Some(v) = self.echo_p99_ms {
            out.push(Sample::new(MetricKind::EchoP99Ms, platform, scenario, v));
        }
        out
    }
}

/// `nfr-context` 的载荷：仓库地图在合成仓库上的上下文延迟。
///
/// 只有 `slice_p99_ms` 与 `incremental_ms` 进门禁，其余字段供判读。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextLatencyPayload {
    /// 合成仓库的文件数。
    pub files: u64,
    /// 增量阶段改动、改名、删除、新增的文件总数。
    #[serde(default)]
    pub changed_files: u64,
    /// 后台建索引/刷新期间，调用线程取切片的 p99（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice_p99_ms: Option<f64>,
    /// 增量 sweep 从请求到落定的墙钟时间（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice_p50_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice_max_ms: Option<f64>,
    /// 首次全量建索引的墙钟时间（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_index_ms: Option<f64>,
    /// 在调用线程上启动服务（校验根、起线程）的耗时（ms）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_ms: Option<f64>,
    /// 计入分位数的切片次数。
    #[serde(default)]
    pub slices: u64,
    /// 多轮测量的原始值；顶层各指标是它们的中位数。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<ContextLatencyPayload>,
}

impl ContextLatencyPayload {
    /// 每个指标取各轮中位数，原始轮次放进 `runs`。
    pub fn median_of(runs: Vec<ContextLatencyPayload>) -> ContextLatencyPayload {
        let pick = |field: fn(&ContextLatencyPayload) -> Option<f64>| {
            median(runs.iter().filter_map(field))
        };
        ContextLatencyPayload {
            files: runs.iter().map(|r| r.files).max().unwrap_or(0),
            changed_files: runs.iter().map(|r| r.changed_files).max().unwrap_or(0),
            slice_p99_ms: pick(|r| r.slice_p99_ms),
            incremental_ms: pick(|r| r.incremental_ms),
            slice_p50_ms: pick(|r| r.slice_p50_ms),
            slice_max_ms: pick(|r| r.slice_max_ms),
            full_index_ms: pick(|r| r.full_index_ms),
            spawn_ms: pick(|r| r.spawn_ms),
            slices: runs.iter().map(|r| r.slices).sum(),
            runs,
        }
    }

    pub fn to_samples(&self, platform: &str) -> Vec<Sample> {
        let mut out = Vec::new();
        if let Some(v) = self.slice_p99_ms {
            out.push(Sample::new(
                MetricKind::ContextLatencyP99Ms,
                platform,
                "repo_map_slice",
                v,
            ));
        }
        if let Some(v) = self.incremental_ms {
            out.push(Sample::new(
                MetricKind::RepoMapIncrementalMs,
                platform,
                "repo_map_refresh",
                v,
            ));
        }
        out
    }
}

/// harness 二进制最终打印的顶层 JSON。CI 脚本据此分流到 PTY bench、进程级 run 或上下文延迟。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NfrPayload {
    PtyThroughput(PtyThroughputPayload),
    Run(RunPayload),
    ContextLatency(ContextLatencyPayload),
    AgentIo(AgentIoPayload),
    Residual(ResidualPayload),
}

/// `nfr-residual`'s payload: what a long automation run leaves behind.
///
/// The gated metric is `bytes_per_cycle`: the residual per cycle has to stay flat, or
/// a long run grows without bound.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResidualPayload {
    /// Automation cycles driven in the round.
    pub cycles: u64,
    /// Cycles that ran to completion.
    pub completed_cycles: u64,
    /// Wall clock for the round (ms).
    #[serde(default)]
    pub elapsed_ms: f64,
    /// Data directory size before the round.
    #[serde(default)]
    pub disk_before_bytes: u64,
    /// Data directory size after the round.
    #[serde(default)]
    pub disk_after_bytes: u64,
    /// How much the round grew the data directory.
    #[serde(default)]
    pub disk_growth_bytes: u64,
    /// Data directory size after the retention pass.
    #[serde(default)]
    pub disk_after_cleanup_bytes: u64,
    /// How much the data directory grew *after* the retention pass ran.
    ///
    /// This is the marginal cost of one more cycle in steady state: what a long run
    /// would keep paying per cycle. The raw growth above is dominated by the fixed
    /// per-task records a first run writes, which is not a leak.
    #[serde(default)]
    pub residual_growth_bytes: u64,
    /// How much the retention pass gave back.
    #[serde(default)]
    pub reclaimed_bytes: u64,
    /// Journal records before the round.
    #[serde(default)]
    pub journal_records_before: u64,
    /// Journal records after the round.
    #[serde(default)]
    pub journal_records_after: u64,
    /// Journal records after the retention pass.
    #[serde(default)]
    pub journal_records_after_cleanup: u64,
    /// Runs still queued when the round ended.
    #[serde(default)]
    pub residual_queued_runs: u64,
    /// Multi-round raw values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<ResidualPayload>,
}

impl ResidualPayload {
    /// The residual per cycle: the slope a long run would follow.
    pub fn bytes_per_cycle(&self) -> f64 {
        if self.cycles == 0 {
            return 0.0;
        }
        self.residual_growth_bytes as f64 / self.cycles as f64
    }

    /// The raw growth per cycle, before the retention pass. Kept for the report, but
    /// not the gated metric: it measures the fixed per-task records, not a leak.
    pub fn raw_bytes_per_cycle(&self) -> f64 {
        if self.cycles == 0 {
            return 0.0;
        }
        self.disk_growth_bytes as f64 / self.cycles as f64
    }

    pub fn median_of(runs: Vec<ResidualPayload>) -> ResidualPayload {
        let pick =
            |field: fn(&ResidualPayload) -> f64| median(runs.iter().map(field)).unwrap_or_default();
        ResidualPayload {
            cycles: runs.iter().map(|r| r.cycles).max().unwrap_or(0),
            completed_cycles: runs.iter().map(|r| r.completed_cycles).max().unwrap_or(0),
            elapsed_ms: pick(|r| r.elapsed_ms),
            disk_before_bytes: runs.iter().map(|r| r.disk_before_bytes).min().unwrap_or(0),
            disk_after_bytes: runs.iter().map(|r| r.disk_after_bytes).max().unwrap_or(0),
            disk_growth_bytes: pick(|r| r.disk_growth_bytes as f64) as u64,
            disk_after_cleanup_bytes: pick(|r| r.disk_after_cleanup_bytes as f64) as u64,
            residual_growth_bytes: pick(|r| r.residual_growth_bytes as f64) as u64,
            reclaimed_bytes: pick(|r| r.reclaimed_bytes as f64) as u64,
            journal_records_before: runs
                .iter()
                .map(|r| r.journal_records_before)
                .min()
                .unwrap_or(0),
            journal_records_after: runs
                .iter()
                .map(|r| r.journal_records_after)
                .max()
                .unwrap_or(0),
            journal_records_after_cleanup: runs
                .iter()
                .map(|r| r.journal_records_after_cleanup)
                .max()
                .unwrap_or(0),
            residual_queued_runs: runs
                .iter()
                .map(|r| r.residual_queued_runs)
                .max()
                .unwrap_or(0),
            runs,
        }
    }

    pub fn to_samples(&self, platform: &str) -> Vec<Sample> {
        vec![Sample::new(
            MetricKind::ResidualBytesPerCycle,
            platform,
            "automation_cycle",
            self.bytes_per_cycle(),
        )]
    }
}

/// `nfr-agent-io`'s payload: the task journal write path.
///
/// Only `burst_total_ms`, `critical_total_ms` and `replay_ms` gate; the rest is for
/// reading the numbers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentIoPayload {
    /// Non-critical events in the measured burst.
    pub burst_events: u64,
    /// Critical events written after the burst.
    pub critical_events: u64,
    /// Wall clock for the whole non-critical burst (ms).
    #[serde(default)]
    pub burst_total_ms: f64,
    /// p99 of the non-critical appends (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_p99_ms: Option<f64>,
    /// The slowest non-critical append (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_max_ms: Option<f64>,
    /// Mean cost of one non-critical append (microseconds).
    #[serde(default)]
    pub per_event_us: f64,
    /// Wall clock for the critical events (ms). Every critical event flushes.
    #[serde(default)]
    pub critical_total_ms: f64,
    /// p99 of the critical appends (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_p99_ms: Option<f64>,
    /// Wall clock for replaying the journal (ms).
    #[serde(default)]
    pub replay_ms: f64,
    /// Size of the journal file after the round.
    #[serde(default)]
    pub journal_bytes: u64,
    /// Multi-round raw values; the top-level metrics are their medians.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<AgentIoPayload>,
}

impl AgentIoPayload {
    /// Each metric takes the median across rounds.
    pub fn median_of(runs: Vec<AgentIoPayload>) -> AgentIoPayload {
        let pick =
            |field: fn(&AgentIoPayload) -> f64| median(runs.iter().map(field)).unwrap_or_default();
        AgentIoPayload {
            burst_events: runs.iter().map(|r| r.burst_events).max().unwrap_or(0),
            critical_events: runs.iter().map(|r| r.critical_events).max().unwrap_or(0),
            burst_total_ms: pick(|r| r.burst_total_ms),
            burst_p99_ms: runs
                .iter()
                .filter_map(|r| r.burst_p99_ms)
                .min_by(f64::total_cmp),
            burst_max_ms: runs
                .iter()
                .filter_map(|r| r.burst_max_ms)
                .max_by(f64::total_cmp),
            per_event_us: pick(|r| r.per_event_us),
            critical_total_ms: pick(|r| r.critical_total_ms),
            critical_p99_ms: runs
                .iter()
                .filter_map(|r| r.critical_p99_ms)
                .max_by(f64::total_cmp),
            replay_ms: pick(|r| r.replay_ms),
            journal_bytes: runs.iter().map(|r| r.journal_bytes).max().unwrap_or(0),
            runs,
        }
    }

    pub fn to_samples(&self, platform: &str) -> Vec<Sample> {
        vec![
            Sample::new(
                MetricKind::AgentIoBurstMs,
                platform,
                "journal_burst",
                self.burst_total_ms,
            ),
            Sample::new(
                MetricKind::AgentIoCriticalMs,
                platform,
                "journal_critical",
                self.critical_total_ms,
            ),
            Sample::new(
                MetricKind::AgentIoReplayMs,
                platform,
                "journal_replay",
                self.replay_ms,
            ),
        ]
    }
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
                NfrPayload::ContextLatency(c) => out.extend(c.to_samples(&self.platform)),
                NfrPayload::AgentIo(a) => out.extend(a.to_samples(&self.platform)),
                NfrPayload::Residual(r) => out.extend(r.to_samples(&self.platform)),
            }
        }
        out
    }
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut sorted: Vec<f64> = values.collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    })
}

/// 解析 harness 输出的一行 JSON（容忍前后空白与多行 pretty JSON）。
pub fn parse_harness_report(json: &str) -> Result<HarnessReport, serde_json::Error> {
    serde_json::from_str(json.trim())
}
