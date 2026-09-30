//! NFR 测量模式（`TERMIOR_NFR_MEASURE=1`，由 `termior-bench` 的 `nfr-run` 驱动）的共享小工具。
//!
//! 协议行统一以 `TERMIOR_NFR_` 开头打印到 stdout，harness 逐行解析；见 docs/nfr-baselines.md。

use std::io::Write as _;

/// 最近秩法（nearest-rank）分位数；`q` 取 0.0..=1.0。空输入返回 `None`。
pub fn percentile(values: &[f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (q.clamp(0.0, 1.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

/// 打印一行协议输出并立即 flush（stdout 是管道时默认块缓冲）。
pub fn emit(line: &str) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(&[], 0.99), None);
        assert_eq!(percentile(&[5.0], 0.99), Some(5.0));
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&values, 0.99), Some(99.0));
        assert_eq!(percentile(&values, 0.5), Some(50.0));
        assert_eq!(percentile(&[3.0, 1.0, 2.0], 1.0), Some(3.0));
    }
}
