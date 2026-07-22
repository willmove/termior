//! PTY/VTE 吞吐基准（spec NFR-02：VTE 解析吞吐达 alacritty_terminal 同量级）。
//!
//! 复刻应用真实消费路径：PTY reader 过滤后的字节流 → `vte::Processor::advance`
//! 喂入 alacritty `Term` 网格（解析逻辑在 `vte_harness` 共享）。这把「cat 大文件
//! 期间渲染能否跟上」拆成可纯函数量化的部分——VTE 解析 + 网格写入速率，排除平台
//! GPU/显示噪声。该 bench 在 headless CI（ubuntu）即可跑，是 NFR 回归门禁主干项。
//! 一次性取值（喂基线）由 `src/bin/nfr-pty.rs` 完成，复用同一 `vte_harness`。

use std::hint::black_box;

use alacritty_terminal::vte::ansi::{Processor as VteProcessor, StdSyncHandler};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use termior_bench::vte_harness::{new_term, synthetic_pty_output};

fn bench_throughput(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("pty/vte_throughput");
    for size_mib in [1, 5] {
        let bytes = synthetic_pty_output(size_mib * 1024 * 1024);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{size_mib}mib")),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    let mut term = new_term();
                    let mut processor = VteProcessor::<StdSyncHandler>::default();
                    processor.advance(black_box(&mut term), black_box(bytes));
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_throughput);
criterion_main!(benches);
