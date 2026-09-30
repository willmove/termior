//! `nfr-run` — 进程级 NFR harness（spec NFR-01 冷启动 / NFR-04 RSS / NFR-03 帧率）。
//!
//! 启动 release `termior`（设 `TERMIOR_NFR_MEASURE=1`），在后台：
//! - 计时进程启动 → 应用打印 `TERMIOR_NFR_FIRST_FRAME` 行的间隔 = 冷启动。
//! - 用 sysinfo 轮询子进程 RSS，取稳态（窗口存活期间）最大物理内存。
//! - 应用退出前打印 `TERMIOR_NFR_FPS=NN` / `TERMIOR_NFR_FRAME_P99_MS=NN.N` = 强制重绘下的帧率与帧间隔 p99。
//! - `--echo-samples N`（默认 40，0 关闭）时应用接着跑键入回显探针，打印 `TERMIOR_NFR_ECHO_P99_MS`。
//! - `TERMIOR_NFR_PHASE` 冷启动分阶段时间戳原样收进 payload 并转发到 stderr，便于 CI 日志定位。
//!
//! 输出一行 `NfrPayload::Run` JSON。
//!
//! **平台范围**：需真实显示环境（GPUI 窗口）。CI 仅在 windows/macos（带显示）
//! 的 desktop job 启用；ubuntu（headless）只跑 `nfr-pty` + criterion bench。
//! 冷启动绝对值依赖硬件，故 CI 门禁用「相对基线回归」，而非 spec 绝对目标
//! （spec 绝对目标作为发布验收参考，记录在 docs/nfr-baselines.md）。

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use sysinfo::{Pid, ProcessesToUpdate, System};
use termior_bench::{NfrPayload, RunPayload};

struct Args {
    binary: PathBuf,
    workspace: Option<PathBuf>,
    /// 子进程最长存活时间（秒），到点发 kill；应用测完会自行退出，通常提前结束。
    dwell_secs: u64,
    /// 键入回显探针样本数（偶数，`x`/Backspace 交替）；0 = 不测。
    echo_samples: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut binary: Option<PathBuf> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut dwell_secs: u64 = 6;
    let mut echo_samples: usize = 40;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--binary" => {
                binary = Some(PathBuf::from(args.next().ok_or("--binary needs a value")?))
            }
            "--workspace" => {
                workspace = Some(PathBuf::from(
                    args.next().ok_or("--workspace needs a value")?,
                ))
            }
            "--dwell-secs" => {
                dwell_secs = args
                    .next()
                    .ok_or("--dwell-secs needs a value")?
                    .parse()
                    .map_err(|e: std::num::ParseIntError| format!("--dwell-secs: {e}"))?
            }
            "--echo-samples" => {
                echo_samples = args
                    .next()
                    .ok_or("--echo-samples needs a value")?
                    .parse()
                    .map_err(|e: std::num::ParseIntError| format!("--echo-samples: {e}"))?
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        binary: binary.ok_or("--binary is required")?,
        workspace,
        dwell_secs,
        echo_samples,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("nfr-run: {e}");
            eprintln!(
                "usage: nfr-run --binary <termior(.exe)> [--workspace <dir>] [--dwell-secs 6] [--echo-samples 40]"
            );
            std::process::exit(2);
        }
    };

    match run(&args) {
        Ok(payload) => {
            let nfr = NfrPayload::Run(payload);
            println!(
                "{}",
                serde_json::to_string(&nfr).expect("serialize run payload")
            );
        }
        Err(e) => {
            eprintln!("nfr-run: {e}");
            std::process::exit(3);
        }
    }
}

fn run(args: &Args) -> Result<RunPayload, String> {
    if !args.binary.exists() {
        #[cfg(windows)]
        if args.binary.extension().is_none() {
            let with_exe = args.binary.with_extension("exe");
            if with_exe.exists() {
                return run_with_binary(args, with_exe);
            }
        }
        return Err(format!("binary not found: {}", args.binary.display()));
    }
    run_with_binary(args, args.binary.clone())
}

fn run_with_binary(args: &Args, binary: PathBuf) -> Result<RunPayload, String> {
    // 隔离应用数据目录：每次都从全新状态启动，正是 spec NFR-04 的「空载（1 终端 tab）」
    // 场景；也避免恢复开发者本机的 tab/设置，让本地与 CI 测的是同一场景，且不改动
    // 真实的 workspace/session 文件。
    let data_dir = std::env::temp_dir().join(format!("termior-nfr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("create data dir: {e}"))?;
    let result = run_isolated(args, binary, &data_dir);
    let _ = std::fs::remove_dir_all(&data_dir);
    result
}

fn run_isolated(
    args: &Args,
    binary: PathBuf,
    data_dir: &std::path::Path,
) -> Result<RunPayload, String> {
    let launch = Instant::now();
    let mut cmd = Command::new(&binary);
    cmd.env("TERMIOR_NFR_MEASURE", "1");
    cmd.env("TERMIOR_DATA_DIR", data_dir);
    if args.echo_samples > 0 {
        cmd.env("TERMIOR_NFR_ECHO_SAMPLES", args.echo_samples.to_string());
    }
    if let Some(ws) = &args.workspace {
        cmd.arg(ws);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let pid_u32 = child.id();
    let pid = Pid::from_u32(pid_u32);

    // 读 stdout：找 first-frame 标记行（冷启动终点）与 FPS 行。
    // 冷启动 = 从 `launch`（harness 决定启动，≈ 进程 spawn 起点）到应用打印首帧标记，
    // 故把 `launch` 移入 reader 线程做计时基准（ticket L9：从进程启动到首帧可交互）。
    let stdout = child.stdout.take().ok_or("no stdout capture")?;
    let reader = std::thread::spawn(move || {
        let mut report = RunPayload::default();
        use std::io::{BufRead, BufReader};
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if report.cold_start_ms.is_none() && line.contains("TERMIOR_NFR_FIRST_FRAME") {
                report.cold_start_ms = Some(launch.elapsed().as_secs_f64() * 1000.0);
            }
            parse_protocol_line(&line, &mut report);
        }
        report
    });

    // 轮询 RSS：在 dwell 窗口内取最大物理内存作为常驻 RSS（NFR-04）。
    let mut sys = System::new();
    let mut peak_rss_bytes: u64 = 0;
    let poll_deadline = Instant::now() + std::time::Duration::from_secs(args.dwell_secs);
    while Instant::now() < poll_deadline {
        sys.refresh_processes(ProcessesToUpdate::All);
        if let Some(proc_info) = sys.process(pid) {
            if proc_info.memory() > peak_rss_bytes {
                peak_rss_bytes = proc_info.memory();
            }
        }
        // 应用测完自行退出：不再空等到 dwell 截止。
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    // dwell 到点结束子进程。
    let _ = child.kill();
    let _ = child.wait();

    let mut report = reader.join().map_err(|_| "stdout reader panicked")?;

    // 拿不到首帧标记 = 冷启动测量无效（应用可能没正常开窗），不报告该指标，
    // 由门禁侧判为「无基线/缺项」而非用 launch→join 的粗略值蒙混（ticket L9 语义）。
    report.rss_mib = if peak_rss_bytes > 0 {
        Some(peak_rss_bytes as f64 / (1024.0 * 1024.0))
    } else {
        None
    };
    Ok(report)
}

/// 解析应用打印的一行 `TERMIOR_NFR_*` 协议输出（首帧标记由调用方计时，不在此处）。
fn parse_protocol_line(line: &str, report: &mut RunPayload) {
    let number = |rest: &str| {
        rest.split_whitespace()
            .next()
            .and_then(|value| value.parse::<f64>().ok())
    };
    if let Some(rest) = line.strip_prefix("TERMIOR_NFR_FPS=") {
        report.fps = number(rest).or(report.fps);
    } else if let Some(rest) = line.strip_prefix("TERMIOR_NFR_FRAME_P99_MS=") {
        report.frame_p99_ms = number(rest).or(report.frame_p99_ms);
    } else if let Some(rest) = line.strip_prefix("TERMIOR_NFR_ECHO_P99_MS=") {
        eprintln!("nfr-run: {line}");
        report.echo_p99_ms = number(rest).or(report.echo_p99_ms);
    } else if let Some(rest) = line.strip_prefix("TERMIOR_NFR_WINDOW_ACTIVE=") {
        report.window_active = Some(rest.trim() == "1");
        if rest.trim() != "1" {
            eprintln!(
                "nfr-run: window was not foreground during sampling; GPUI caps it to ~30fps"
            );
        }
    } else if let Some(rest) = line.strip_prefix("TERMIOR_NFR_PHASE ") {
        eprintln!("nfr-run: phase {rest}");
        if let Some((name, value)) = rest.split_once('=') {
            if let Some(ms) = number(value) {
                report.phases_ms.insert(name.trim().to_owned(), ms);
            }
        }
    } else if line.starts_with("TERMIOR_NFR_ECHO_UNAVAILABLE") {
        eprintln!("nfr-run: key echo probe could not run (no terminal output)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_lines_fill_run_payload() {
        let mut report = RunPayload::default();
        for line in [
            "TERMIOR_NFR_PHASE fonts=42.5",
            "TERMIOR_NFR_FPS=60",
            "TERMIOR_NFR_FRAME_P99_MS=17.9",
            "TERMIOR_NFR_WINDOW_ACTIVE=0",
            "TERMIOR_NFR_ECHO_P99_MS=8.4 samples=40",
            "unrelated log line",
        ] {
            parse_protocol_line(line, &mut report);
        }
        assert_eq!(report.fps, Some(60.0));
        assert_eq!(report.frame_p99_ms, Some(17.9));
        assert_eq!(report.echo_p99_ms, Some(8.4));
        assert_eq!(report.window_active, Some(false));
        assert_eq!(report.phases_ms.get("fonts"), Some(&42.5));
    }
}