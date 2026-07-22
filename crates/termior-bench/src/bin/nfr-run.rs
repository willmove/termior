//! `nfr-run` — 进程级 NFR harness（spec NFR-01 冷启动 / NFR-04 RSS / NFR-03 帧率）。
//!
//! 启动 release `termior`（设 `TERMIOR_NFR_MEASURE=1`），在后台：
//! - 计时进程启动 → 应用打印 `TERMIOR_NFR_FIRST_FRAME` 行的间隔 = 冷启动。
//! - 用 sysinfo 轮询子进程 RSS，取稳态（窗口存活期间）最大物理内存。
//! - 应用退出前打印 `TERMIOR_NFR_FPS=NN` 行 = 稳态采样帧率。
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
    /// 子进程存活时间（秒），到点发 kill；留足帧率采样窗口。
    dwell_secs: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut binary: Option<PathBuf> = None;
    let mut workspace: Option<PathBuf> = None;
    let mut dwell_secs: u64 = 6;
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
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        binary: binary.ok_or("--binary is required")?,
        workspace,
        dwell_secs,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("nfr-run: {e}");
            eprintln!(
                "usage: nfr-run --binary <termior(.exe)> [--workspace <dir>] [--dwell-secs 6]"
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
        return Err(format!("binary not found: {}", args.binary.display()));
    }

    let launch = Instant::now();
    let mut cmd = Command::new(&args.binary);
    cmd.env("TERMIOR_NFR_MEASURE", "1");
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
        let mut first_frame_ms: Option<f64> = None;
        let mut fps: Option<f64> = None;
        use std::io::{BufRead, BufReader};
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if first_frame_ms.is_none() && line.contains("TERMIOR_NFR_FIRST_FRAME") {
                first_frame_ms = Some(launch.elapsed().as_secs_f64() * 1000.0);
            }
            if let Some(rest) = line.strip_prefix("TERMIOR_NFR_FPS=") {
                if let Ok(v) = rest.trim().parse::<f64>() {
                    fps = Some(v);
                }
            }
        }
        (first_frame_ms, fps)
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
        // 进程提前退出也 OK，继续等 reader 收尾。
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    // dwell 到点结束子进程。
    let _ = child.kill();
    let _ = child.wait();

    let (first_frame_ms, fps) = reader.join().map_err(|_| "stdout reader panicked")?;

    // 拿不到首帧标记 = 冷启动测量无效（应用可能没正常开窗），不报告该指标，
    // 由门禁侧判为「无基线/缺项」而非用 launch→join 的粗略值蒙混（ticket L9 语义）。
    let cold_start_ms = first_frame_ms;
    let rss_mib = if peak_rss_bytes > 0 {
        Some(peak_rss_bytes as f64 / (1024.0 * 1024.0))
    } else {
        None
    };

    Ok(RunPayload {
        cold_start_ms,
        rss_mib,
        fps,
    })
}
