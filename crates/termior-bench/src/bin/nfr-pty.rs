//! `nfr-pty` — PTY/VTE 吞吐一次性测量（spec NFR-02）。
//!
//! 与 `benches/pty_throughput.rs`（criterion 统计稳态）共用同一解析路径（`vte_harness`），
//! 但这里只跑一次取值，打印一行 `NfrPayload::PtyThroughput` JSON 给 CI 采集。
//! 设计取舍：门禁要的是「单值 vs 基线」，不需要 criterion 的多次采样与预热；
//! 单值采样噪声由阈值（百分比带宽）吸收。

use std::hint::black_box;
use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::vte::ansi::{Processor as VteProcessor, StdSyncHandler};

use termior_bench::vte_harness::{new_term, synthetic_pty_output};
use termior_bench::{NfrPayload, PtyThroughputPayload};

const MEASURE_MIB: usize = 5;
const MEASURE_BYTES: usize = MEASURE_MIB * 1024 * 1024;

fn main() {
    // 预热一次：走通热路径、稳定分配器后再计时。
    let warmup = synthetic_pty_output(MEASURE_BYTES);
    feed_once(&warmup);

    let bytes = synthetic_pty_output(MEASURE_BYTES);
    let start = Instant::now();
    feed_once(black_box(&bytes));
    let elapsed = start.elapsed().as_secs_f64();

    let payload = PtyThroughputPayload {
        bytes: bytes.len() as u64,
        elapsed_secs: elapsed,
    };
    let nfr = NfrPayload::PtyThroughput(payload);
    println!(
        "{}",
        serde_json::to_string(&nfr).expect("serialize payload")
    );
}

fn feed_once(bytes: &[u8]) {
    let mut term = new_term();
    let mut processor = VteProcessor::<StdSyncHandler>::default();
    processor.advance(&mut term, bytes);
    // 防止整个调用被优化掉：读取网格行数作为副作用。
    let _ = term.grid().total_lines();
}
