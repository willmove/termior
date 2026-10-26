//! `nfr-residual` — 长时间自动化运行留下的资源残留。
//!
//! 驱动多轮自动化周期（真实调度器、真实存储、真实任务日志），然后测量数据目录
//! 的增长、任务日志条数与仍然排队的运行数。打印一行 `NfrPayload::Residual` JSON
//! 给 `nfr-gate`。
//!
//! 用法：`nfr-residual [--cycles N] [--rounds N] [--scratch DIR]`

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use termior_bench::residual_harness::{measure_round, soak_root, ResidualConfig};
use termior_bench::{NfrPayload, ResidualPayload};

struct Args {
    cycles: usize,
    rounds: usize,
    scratch: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut parsed = Args {
        cycles: ResidualConfig::default().cycles,
        rounds: 3,
        scratch: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--cycles" => {
                parsed.cycles = value()?
                    .parse()
                    .map_err(|error| format!("--cycles: {error}"))?
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
    if parsed.cycles == 0 || parsed.rounds == 0 {
        return Err("--cycles and --rounds must be positive".into());
    }
    Ok(parsed)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nfr-residual: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let scratch = args.scratch.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("termior-nfr-residual-{}", std::process::id()))
    });
    let config = ResidualConfig {
        cycles: args.cycles,
        ..ResidualConfig::default()
    };
    let mut rounds = Vec::with_capacity(args.rounds);
    let mut outcome = Ok(());
    // Every round shares one data directory: a long automation run keeps its state
    // in one place, and the rounds then measure the marginal cost of their own
    // cycles.
    let root = soak_root(&scratch);
    for round in 0..args.rounds {
        match measure_round(&root, config) {
            Ok(payload) => {
                eprintln!(
                    "nfr-residual: round {round}: {} cycles, {:.1} B/cycle residual, {:.1} B/cycle raw, {} journal records, {} queued, {:.1} ms",
                    payload.cycles,
                    payload.bytes_per_cycle(),
                    payload.raw_bytes_per_cycle(),
                    payload.journal_records_after,
                    payload.residual_queued_runs,
                    payload.elapsed_ms
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
    // The first round is a warm-up: the store is still filling up towards its cap,
    // so its marginal cost is not the steady-state cost a long run pays. The gated
    // value is the median of the rounds after it.
    let steady: Vec<ResidualPayload> = rounds.iter().skip(1).cloned().collect();
    let mut payload = ResidualPayload::median_of(if steady.is_empty() {
        rounds.clone()
    } else {
        steady
    });
    // Every round is kept in the report.
    payload.runs = rounds;
    let json = serde_json::to_string(&NfrPayload::Residual(payload))
        .map_err(|error| format!("serialize payload: {error}"))?;
    println!("{json}");
    Ok(())
}
