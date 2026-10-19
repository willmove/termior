//! 纯逻辑测试：基线查找、回归判定方向、payload 展开、JSON 双向兼容。

use crate::baselines::{Baseline, BaselineSet};
use crate::compare::{compare, Verdict};
use crate::metric::{MetricKind, Sample};
use crate::payload::{
    ContextLatencyPayload, HarnessReport, NfrPayload, PtyThroughputPayload, RunPayload,
};

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
        frame_p99_ms: Some(18.0),
        echo_p99_ms: Some(12.0),
        window_active: Some(true),
        phases_ms: [("fonts".to_owned(), 40.0)].into_iter().collect(),
        warmup_ms: Some(2100.0),
        fps_windows: vec![58.0, 59.0, 60.0],
        runs: Vec::new(),
    };
    let samples = run.to_samples("windows", "typical");
    assert_eq!(samples.len(), 5);
    assert!(samples.iter().any(|s| s.kind == MetricKind::FrameP99Ms));
    assert!(samples.iter().any(|s| s.kind == MetricKind::EchoP99Ms));
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
                ..Default::default()
            }),
        ],
    };
    let json = serde_json::to_string(&report).unwrap();
    let parsed = crate::payload::parse_harness_report(&json).unwrap();
    let samples = parsed.to_samples();
    // 1 pty + 2 run (cold_start + fps; rss None dropped)
    assert_eq!(samples.len(), 3);
}

fn run(cold_start_ms: f64, fps: f64, window_active: bool) -> RunPayload {
    RunPayload {
        cold_start_ms: Some(cold_start_ms),
        rss_mib: Some(55.0),
        fps: Some(fps),
        window_active: Some(window_active),
        phases_ms: [("fonts".to_owned(), cold_start_ms / 2.0)]
            .into_iter()
            .collect(),
        fps_windows: vec![fps; 5],
        ..Default::default()
    }
}

#[test]
fn median_of_runs_drops_a_single_slow_launch() {
    // Windows hosted runner 上实际出现过的形态：一次启动被启动期卡顿拖到 16fps。
    let merged = RunPayload::median_of(vec![
        run(180.0, 16.0, true),
        run(130.0, 27.0, true),
        run(140.0, 25.0, true),
    ]);
    assert_eq!(merged.fps, Some(25.0));
    assert_eq!(merged.cold_start_ms, Some(140.0));
    assert_eq!(merged.rss_mib, Some(55.0));
    assert_eq!(merged.phases_ms.get("fonts"), Some(&70.0));
    assert_eq!(merged.window_active, Some(true));
    assert_eq!(merged.runs.len(), 3);
    assert!(merged.fps_windows.is_empty());

    let baselines = set(&[Baseline::new(
        MetricKind::Fps,
        "windows",
        "typical",
        27.0,
        40.0,
    )]);
    let report = compare(&merged.to_samples("windows", "typical"), &baselines);
    assert!(!report.has_regression());
}

#[test]
fn median_of_runs_still_fails_a_consistent_regression() {
    let merged = RunPayload::median_of(vec![
        run(150.0, 12.0, true),
        run(150.0, 14.0, true),
        run(150.0, 30.0, true),
    ]);
    assert_eq!(merged.fps, Some(14.0));
    let baselines = set(&[Baseline::new(
        MetricKind::Fps,
        "windows",
        "typical",
        27.0,
        40.0,
    )]);
    let report = compare(&merged.to_samples("windows", "typical"), &baselines);
    assert!(report.has_regression());
}

#[test]
fn median_of_runs_handles_even_counts_and_missing_values() {
    let mut no_fps = run(200.0, 0.0, false);
    no_fps.fps = None;
    let merged = RunPayload::median_of(vec![run(100.0, 20.0, true), no_fps]);
    assert_eq!(merged.cold_start_ms, Some(150.0));
    assert_eq!(merged.fps, Some(20.0));
    assert_eq!(merged.window_active, Some(false));
    assert_eq!(merged.echo_p99_ms, None);
    assert_eq!(RunPayload::median_of(Vec::new()), RunPayload::default());
}

#[test]
fn run_payload_without_new_fields_still_parses() {
    let json = r#"{"kind":"Run","cold_start_ms":182.9,"rss_mib":54.1,"fps":16.0,"frame_p99_ms":202.6,"echo_p99_ms":223.4,"window_active":true,"phases_ms":{"fonts":64.8}}"#;
    let NfrPayload::Run(parsed) = serde_json::from_str::<NfrPayload>(json).unwrap() else {
        panic!("expected a run payload");
    };
    assert_eq!(parsed.fps, Some(16.0));
    assert!(parsed.runs.is_empty());
    assert!(parsed.fps_windows.is_empty());
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
        (MetricKind::ContextLatencyP99Ms, "context_latency_p99_ms"),
        (MetricKind::RepoMapIncrementalMs, "repo_map_incremental_ms"),
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

fn context_round(slice_p99_ms: f64, incremental_ms: f64) -> ContextLatencyPayload {
    ContextLatencyPayload {
        files: 5_000,
        changed_files: 80,
        slice_p99_ms: Some(slice_p99_ms),
        incremental_ms: Some(incremental_ms),
        slices: 10,
        ..ContextLatencyPayload::default()
    }
}

#[test]
fn context_payload_expands_to_gated_samples_only() {
    let report = HarnessReport {
        platform: "linux".into(),
        payloads: vec![NfrPayload::ContextLatency(ContextLatencyPayload {
            full_index_ms: Some(900.0),
            spawn_ms: Some(0.2),
            ..context_round(4.0, 120.0)
        })],
    };
    let samples = report.to_samples();
    assert_eq!(
        samples,
        vec![
            Sample::new(
                MetricKind::ContextLatencyP99Ms,
                "linux",
                "repo_map_slice",
                4.0
            ),
            Sample::new(
                MetricKind::RepoMapIncrementalMs,
                "linux",
                "repo_map_refresh",
                120.0
            ),
        ]
    );
    assert!(MetricKind::ContextLatencyP99Ms.lower_is_better());
    assert_eq!(MetricKind::RepoMapIncrementalMs.unit(), "ms");
}

#[test]
fn context_median_drops_one_stalled_round_but_fails_a_real_regression() {
    let baselines = set(&[Baseline::new(
        MetricKind::ContextLatencyP99Ms,
        "linux",
        "repo_map_slice",
        10.0,
        50.0,
    )]);
    let merged = ContextLatencyPayload::median_of(vec![
        context_round(8.0, 100.0),
        context_round(90.0, 900.0),
        context_round(9.0, 110.0),
    ]);
    assert_eq!(merged.slice_p99_ms, Some(9.0));
    assert_eq!(merged.incremental_ms, Some(110.0));
    assert_eq!(merged.slices, 30);
    assert_eq!(merged.runs.len(), 3);
    assert!(!compare(&merged.to_samples("linux"), &baselines).has_regression());

    let slow = ContextLatencyPayload::median_of(vec![
        context_round(30.0, 100.0),
        context_round(31.0, 100.0),
        context_round(29.0, 100.0),
    ]);
    assert!(compare(&slow.to_samples("linux"), &baselines).has_regression());
}

#[test]
fn context_payload_json_roundtrips_through_the_gate_format() {
    let payload =
        NfrPayload::ContextLatency(ContextLatencyPayload::median_of(vec![context_round(
            3.0, 50.0,
        )]));
    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.starts_with(r#"{"kind":"ContextLatency""#), "{json}");
    assert_eq!(serde_json::from_str::<NfrPayload>(&json).unwrap(), payload);
    let minimal = r#"{"kind":"ContextLatency","files":10}"#;
    let NfrPayload::ContextLatency(parsed) = serde_json::from_str(minimal).unwrap() else {
        panic!("expected a context payload");
    };
    assert!(parsed.to_samples("any").is_empty());
}

#[test]
fn percentile_uses_nearest_rank() {
    use crate::context_harness::percentile;
    assert_eq!(percentile(&[], 0.99), None);
    let values: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(percentile(&values, 0.99), Some(99.0));
    assert_eq!(percentile(&values, 0.5), Some(50.0));
    assert_eq!(percentile(&values, 1.0), Some(100.0));
    assert_eq!(percentile(&[7.0], 0.99), Some(7.0));
}

#[test]
fn context_round_indexes_churns_and_verifies_a_small_repo() {
    use crate::context_harness::{measure_round, ContextHarnessConfig};
    let scratch = tempfile::tempdir().unwrap();
    let payload = measure_round(
        scratch.path(),
        ContextHarnessConfig {
            files: 600,
            timeout: std::time::Duration::from_secs(120),
        },
    )
    .unwrap();
    assert_eq!(payload.files, 600);
    assert!(payload.changed_files >= 6, "{payload:?}");
    assert!(payload.slices >= 50, "{payload:?}");
    assert!(payload.slice_p99_ms.is_some_and(|ms| ms >= 0.0));
    assert!(payload.incremental_ms.is_some_and(|ms| ms > 0.0));
    assert_eq!(payload.to_samples("any").len(), 2);
}

#[test]
fn churn_touches_every_category() {
    use crate::context_harness::{churn, generate_repo, relative_path};
    let scratch = tempfile::tempdir().unwrap();
    generate_repo(scratch.path(), 1_000).unwrap();
    assert!(scratch.path().join("target/debug/generated_0.rs").is_file());
    let plan = churn(scratch.path(), 1_000).unwrap();
    assert_eq!(plan.modified.len(), 10);
    assert_eq!(plan.renamed.len(), 2);
    assert_eq!(plan.deleted.len(), 2);
    assert_eq!(plan.added.len(), 2);
    assert_eq!(plan.total(), 16);
    for (index, to) in &plan.renamed {
        assert!(!scratch.path().join(relative_path(*index)).exists());
        assert!(scratch.path().join(to).is_file());
    }
    for index in &plan.deleted {
        assert!(!scratch.path().join(relative_path(*index)).exists());
    }
    for index in &plan.added {
        assert!(scratch.path().join(relative_path(*index)).is_file());
    }
}
