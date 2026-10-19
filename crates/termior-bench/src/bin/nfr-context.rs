//! `nfr-context` — Agent 上下文延迟（仓库地图）一次性测量。
//!
//! 在临时目录生成多语言合成仓库，测调用线程取相关切片的 p99（后台全量建索引与增量刷新
//! 期间）以及增量刷新耗时，打印一行 `NfrPayload::ContextLatency` JSON 给 `nfr-gate`。
//! 默认 20000 个文件（仓库地图的文件上限）、3 轮取中位数；共享 runner 上单轮被别的进程抢走 CPU 时，中位数滤掉它。
//!
//! 用法：`nfr-context [--files N] [--rounds N] [--scratch DIR]`

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use termior_bench::context_harness::{measure_round, ContextHarnessConfig};
use termior_bench::{ContextLatencyPayload, NfrPayload};

struct Args {
    files: usize,
    rounds: usize,
    scratch: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut parsed = Args {
        files: ContextHarnessConfig::default().files,
        rounds: 3,
        scratch: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--files" => {
                parsed.files = value()?
                    .parse()
                    .map_err(|error| format!("--files: {error}"))?
            }
            "--rounds" => {
                parsed.rounds = value()?
                    .parse()
                    .map_err(|error| format!("--rounds: {error}"))?
            }
            "--scratch" => parsed.scratch = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if parsed.files == 0 || parsed.rounds == 0 {
        return Err("--files and --rounds must be positive".into());
    }
    Ok(parsed)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nfr-context: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let scratch = args.scratch.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("termior-nfr-context-{}", std::process::id()))
    });
    let config = ContextHarnessConfig {
        files: args.files,
        ..ContextHarnessConfig::default()
    };
    let mut rounds = Vec::with_capacity(args.rounds);
    let mut outcome = Ok(());
    for round in 0..args.rounds {
        match measure_round(&scratch.join(format!("round-{round}")), config) {
            Ok(payload) => {
                eprintln!(
                    "nfr-context: round {round}: slice p99 {:.2} ms, incremental {:.1} ms, full index {:.1} ms",
                    payload.slice_p99_ms.unwrap_or_default(),
                    payload.incremental_ms.unwrap_or_default(),
                    payload.full_index_ms.unwrap_or_default()
                );
                rounds.push(payload);
            }
            Err(error) => {
                outcome = Err(format!("round {round}: {error}"));
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    outcome?;
    let payload = ContextLatencyPayload::median_of(rounds);
    let json = serde_json::to_string(&NfrPayload::ContextLatency(payload))
        .map_err(|error| format!("serialize payload: {error}"))?;
    println!("{json}");
    Ok(())
}
