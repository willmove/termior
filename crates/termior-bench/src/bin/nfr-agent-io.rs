//! `nfr-agent-io` — Agent 任务日志写路径的一次性测量。
//!
//! 测三件事：一次流式回合的非关键事件批量落盘、关键事件的逐条 flush、以及重启时
//! 的日志重放。打印一行 `NfrPayload::AgentIo` JSON 给 `nfr-gate`。
//!
//! 用法：`nfr-agent-io [--burst N] [--rounds N] [--scratch DIR]`

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::path::PathBuf;

use termior_bench::agent_io_harness::{measure_round, round_root, AgentIoConfig};
use termior_bench::{AgentIoPayload, NfrPayload};

struct Args {
    burst: usize,
    rounds: usize,
    scratch: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut parsed = Args {
        burst: AgentIoConfig::default().burst_events,
        rounds: 3,
        scratch: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--burst" => {
                parsed.burst = value()?
                    .parse()
                    .map_err(|error| format!("--burst: {error}"))?
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
    if parsed.burst == 0 || parsed.rounds == 0 {
        return Err("--burst and --rounds must be positive".into());
    }
    Ok(parsed)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nfr-agent-io: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let scratch = args.scratch.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("termior-nfr-agent-io-{}", std::process::id()))
    });
    let config = AgentIoConfig {
        burst_events: args.burst,
        ..AgentIoConfig::default()
    };
    let mut rounds = Vec::with_capacity(args.rounds);
    let mut outcome = Ok(());
    for round in 0..args.rounds {
        match measure_round(&round_root(&scratch, round), config) {
            Ok(payload) => {
                eprintln!(
                    "nfr-agent-io: round {round}: burst {:.2} ms ({} events), critical {:.2} ms, replay {:.2} ms, journal {} bytes",
                    payload.burst_total_ms,
                    payload.burst_events,
                    payload.critical_total_ms,
                    payload.replay_ms,
                    payload.journal_bytes
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
    let payload = AgentIoPayload::median_of(rounds);
    let json = serde_json::to_string(&NfrPayload::AgentIo(payload))
        .map_err(|error| format!("serialize payload: {error}"))?;
    println!("{json}");
    Ok(())
}
