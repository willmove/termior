//! 纯逻辑测试：基线查找、回归判定方向、payload 展开、JSON 双向兼容。

use crate::baselines::{Baseline, BaselineSet};
use crate::compare::{compare, Verdict};
use crate::metric::{MetricKind, Sample};
use crate::payload::{HarnessReport, NfrPayload, PtyThroughputPayload, RunPayload};

fn set(baselines: &[Baseline]) -> BaselineSet {
    BaselineSet {
        notes: "test".into(),
        baselines: baselines.to_vec(),
    }
}

#[test]
fn baseline_find_matches_kind_platform_scenario() {
    let b = Baseline::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        800.0,
        15.0,
    );
    let s = set(&[b]);
    let hit = Sample::new(MetricKind::ColdStartMs, "windows", "first_frame", 900.0);
    let miss_kind = Sample::new(MetricKind::RssMiB, "windows", "first_frame", 100.0);
    let miss_plat = Sample::new(MetricKind::ColdStartMs, "linux", "first_frame", 900.0);
    let miss_scen = Sample::new(MetricKind::ColdStartMs, "windows", "empty", 900.0);
    assert!(s.find(&hit).is_some());
    assert!(s.find(&miss_kind).is_none());
    assert!(s.find(&miss_plat).is_none());
    assert!(s.find(&miss_scen).is_none());
}

#[test]
fn cold_start_within_threshold_passes() {
    // 800 → 900 = +12.5%，阈值 15% → WithinThreshold，不算回归
    let baselines = set(&[Baseline::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        800.0,
        15.0,
    )]);
    let samples = [Sample::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        900.0,
    )];
    let report = compare(&samples, &baselines);
    assert!(!report.has_regression());
    assert!(matches!(
        report.regressions[0].verdict,
        Verdict::WithinThreshold
    ));
}

#[test]
fn rss_improvement_is_pass_with_negative_delta() {
    // 150 → 120 MiB = -20%（改善），lower-is-better → Pass
    let baselines = set(&[Baseline::new(
        MetricKind::RssMiB,
        "macos",
        "typical",
        150.0,
        10.0,
    )]);
    let samples = [Sample::new(MetricKind::RssMiB, "macos", "typical", 120.0)];
    let report = compare(&samples, &baselines);
    let r = &report.regressions[0];
    assert!((r.delta_percent - (-20.0)).abs() < 0.001);
    assert!(matches!(r.verdict, Verdict::Pass));
    assert!(!report.has_regression());
}

#[test]
fn fps_drop_over_threshold_fails() {
    // 60 → 48 fps = +20% 退化，阈值 15% → Failed
    let baselines = set(&[Baseline::new(
        MetricKind::Fps,
        "linux",
        "steady_scroll",
        60.0,
        15.0,
    )]);
    let samples = [Sample::new(MetricKind::Fps, "linux", "steady_scroll", 48.0)];
    let report = compare(&samples, &baselines);
    assert!(report.has_regression());
    assert!(report.regressions[0].is_regression());
}

#[test]
fn sample_without_baseline_is_recorded_not_regression() {
    let baselines = set(&[]);
    let samples = [Sample::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        800.0,
    )];
    let report = compare(&samples, &baselines);
    assert!(!report.has_regression());
    assert_eq!(report.without_baseline.len(), 1);
}

#[test]
fn compare_handles_mixed_pass_and_fail() {
    let baselines = set(&[
        Baseline::new(
            MetricKind::ColdStartMs,
            "windows",
            "first_frame",
            800.0,
            15.0,
        ),
        Baseline::new(MetricKind::Fps, "windows", "steady_scroll", 60.0, 15.0),
    ]);
    let samples = [
        Sample::new(MetricKind::ColdStartMs, "windows", "first_frame", 810.0), // pass
        Sample::new(MetricKind::Fps, "windows", "steady_scroll", 30.0),        // fail (-50%)
    ];
    let report = compare(&samples, &baselines);
    assert!(report.has_regression());
    assert_eq!(report.regressions.len(), 2);
    assert!(matches!(
        report.regressions[0].verdict,
        Verdict::WithinThreshold
    ));
    assert!(matches!(report.regressions[1].verdict, Verdict::Failed));
}

#[test]
fn summary_marks_failed_regression() {
    let baselines = set(&[Baseline::new(
        MetricKind::Fps,
        "windows",
        "steady_scroll",
        60.0,
        15.0,
    )]);
    let samples = [Sample::new(
        MetricKind::Fps,
        "windows",
        "steady_scroll",
        30.0,
    )];
    let report = compare(&samples, &baselines);
    let s = report.summary();
    assert!(s.contains("REGRESSION"));
    assert!(s.contains("FAILED"));
}

#[test]
fn baseline_json_roundtrips() {
    let original = set(&[
        Baseline::new(
            MetricKind::ColdStartMs,
            "windows",
            "first_frame",
            800.0,
            15.0,
        ),
        Baseline::new(MetricKind::RssMiB, "macos", "typical", 150.0, 10.0),
    ]);
    let json = original.to_json_pretty().unwrap();
    let parsed = BaselineSet::from_json(&json).unwrap();
    assert_eq!(original, parsed);
}

#[test]
fn payload_run_expands_to_samples() {
    let run = RunPayload {
        cold_start_ms: Some(800.0),
        rss_mib: Some(140.0),
        fps: Some(59.0),
    };
    let samples = run.to_samples("windows", "typical");
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().any(|s| s.kind == MetricKind::ColdStartMs));
    assert!(samples.iter().any(|s| s.kind == MetricKind::RssMiB));
    assert!(samples.iter().any(|s| s.kind == MetricKind::Fps));
}

#[test]
fn pty_payload_computes_mibps() {
    let p = PtyThroughputPayload {
        bytes: 5 * 1024 * 1024, // 5 MiB
        elapsed_secs: 0.1,
    };
    let s = p.to_sample("linux");
    assert!(
        (s.value - 50.0).abs() < 0.01,
        "expected 50 MiB/s, got {}",
        s.value
    );
    assert_eq!(s.scenario, "vte_parse");
}

#[test]
fn harness_report_parse_and_expand() {
    let report = HarnessReport {
        platform: "windows".into(),
        payloads: vec![
            NfrPayload::PtyThroughput(PtyThroughputPayload {
                bytes: 10 * 1024 * 1024,
                elapsed_secs: 0.2,
            }),
            NfrPayload::Run(RunPayload {
                cold_start_ms: Some(700.0),
                rss_mib: None,
                fps: Some(60.0),
            }),
        ],
    };
    let json = serde_json::to_string(&report).unwrap();
    let parsed = crate::payload::parse_harness_report(&json).unwrap();
    let samples = parsed.to_samples();
    // 1 pty + 2 run (cold_start + fps; rss None dropped)
    assert_eq!(samples.len(), 3);
}

#[test]
fn baseline_zero_is_handled_without_panic() {
    // 基线为 0 时退化判定不除零。
    let baselines = set(&[Baseline::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        0.0,
        15.0,
    )]);
    let samples = [Sample::new(
        MetricKind::ColdStartMs,
        "windows",
        "first_frame",
        0.0,
    )];
    let report = compare(&samples, &baselines);
    assert!(!report.has_regression());
}

#[test]
fn metric_kind_serialization_names_are_stable() {
    // docs/baselines/nfr-baselines.json 依赖这些确切字符串；锁死防 rename 回归。
    let cases = [
        (MetricKind::ColdStartMs, "cold_start_ms"),
        (MetricKind::RssMiB, "rss_mib"),
        (MetricKind::Fps, "fps"),
        (MetricKind::PtyThroughputMiBps, "pty_throughput_mibps"),
    ];
    for (kind, expected) in cases {
        let s = serde_json::to_string(&kind).unwrap();
        assert_eq!(
            s,
            format!("\"{expected}\""),
            "serialize mismatch for {kind:?}"
        );
        let back: MetricKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, kind, "roundtrip mismatch for {kind:?}");
    }
}
