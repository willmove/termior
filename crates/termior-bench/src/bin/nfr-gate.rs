//! `nfr-gate` — NFR 回归门禁（CI fail 判据）。
//!
//! 读两份输入：
//! - `--payload`：harness（`nfr-pty` / `nfr-run`）产出的 JSON，可多份合并。
//! - `--baselines`：`docs/baselines/nfr-baselines.json`。
//!
//! 展开为 [`Sample`] 后调用 [`termior_bench::compare`]，打印人类可读摘要，
//! 有回归则以非零码退出（CI 据此 fail）。这是把「感觉流畅」转成「可对比达标证据」
//! 的最后一环（ticket 目标）。

use std::path::PathBuf;
use std::process::ExitCode;

use termior_bench::{compare, BaselineSet, HarnessReport, NfrPayload};

struct Args {
    platform: String,
    payloads: Vec<PathBuf>,
    baselines: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut platform: Option<String> = None;
    let mut payloads: Vec<PathBuf> = Vec::new();
    let mut baselines: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--platform" => platform = Some(args.next().ok_or("--platform needs a value")?),
            "--payload" => {
                payloads.push(PathBuf::from(args.next().ok_or("--payload needs a value")?))
            }
            "--baselines" => {
                baselines = Some(PathBuf::from(
                    args.next().ok_or("--baselines needs a value")?,
                ))
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        platform: platform.ok_or("--platform is required")?,
        payloads,
        baselines: baselines.ok_or("--baselines is required")?,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("nfr-gate: {e}");
            eprintln!("usage: nfr-gate --platform <windows|macos|linux> --payload <json> [--payload <json>...] --baselines <json>");
            return ExitCode::from(2);
        }
    };

    let baseline_json = match std::fs::read_to_string(&args.baselines) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "nfr-gate: cannot read baselines {}: {e}",
                args.baselines.display()
            );
            return ExitCode::from(2);
        }
    };
    let baselines = match BaselineSet::from_json(&baseline_json) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("nfr-gate: invalid baselines json: {e}");
            return ExitCode::from(2);
        }
    };

    // 合并所有 payload 文件为一份 HarnessReport。
    let mut all_payloads: Vec<NfrPayload> = Vec::new();
    for path in &args.payloads {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nfr-gate: cannot read payload {}: {e}", path.display());
                return ExitCode::from(2);
            }
        };
        // payload 文件可能是单条 NfrPayload，也可能是 HarnessReport（带 platform），
        // 也可能是多行（每行一条）。逐行+整体两种方式都试。
        collect_payloads(&raw, &mut all_payloads);
    }
    let report = HarnessReport {
        platform: args.platform.clone(),
        payloads: all_payloads,
    };

    let samples = report.to_samples();
    if samples.is_empty() {
        eprintln!("nfr-gate: no samples parsed from payload(s); nothing to gate.");
        return ExitCode::from(2);
    }

    let result = compare(&samples, &baselines);
    print!("{}", result.summary());
    if result.has_regression() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// 把一段 JSON 文本里的 payload 收集起来，兼容三种形态。
fn collect_payloads(raw: &str, out: &mut Vec<NfrPayload>) {
    // 1. 整体解析为 HarnessReport（带 platform 的完整报告）。
    if let Ok(report) = serde_json::from_str::<HarnessReport>(raw.trim()) {
        out.extend(report.payloads);
        return;
    }
    // 2. 按行解析（每行一条 NfrPayload）。
    let mut got_any = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(p) = serde_json::from_str::<NfrPayload>(line) {
            out.push(p);
            got_any = true;
        }
    }
    if got_any {
        return;
    }
    // 3. 整体解析为单条 NfrPayload。
    if let Ok(p) = serde_json::from_str::<NfrPayload>(raw.trim()) {
        out.push(p);
    }
}
