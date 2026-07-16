//! 模糊文件查找（FR-EXPL-04：`Cmd+Shift+F`，nucleo 排序）。
//!
//! 输入候选路径列表 + 查询串，返回按匹配分数降序排序的结果。Enter 打开、Esc 关闭由上层处理。

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// 一次模糊匹配命中。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyHit {
    pub path: String,
    pub score: u32,
}

/// 对候选列表做模糊匹配，返回按分数降序的结果（仅含命中项）。
///
/// 路径匹配使用 `Config::DEFAULT.match_paths()`，对路径分隔符与文件名首字母给予加分。
pub fn fuzzy_match<'a, I>(query: &str, candidates: I) -> Vec<FuzzyHit>
where
    I: IntoIterator<Item = &'a str>,
{
    let candidates: Vec<&str> = candidates.into_iter().collect();
    if query.trim().is_empty() {
        // 空查询：返回全部，分数 0（上层可决定是否展示）
        return candidates
            .into_iter()
            .map(|c| FuzzyHit {
                path: c.to_string(),
                score: 0,
            })
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut buf: Vec<char> = Vec::new();
    let mut hits: Vec<FuzzyHit> = candidates
        .iter()
        .filter_map(|c| {
            let haystack = Utf32Str::new(c, &mut buf);
            let score = pattern.score(haystack, &mut matcher)?;
            Some(FuzzyHit {
                path: c.to_string(),
                score,
            })
        })
        .collect();

    // 稳定排序：分数降序；同分保持原顺序（稳定）。
    hits.sort_by_key(|h| std::cmp::Reverse(h.score));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all() {
        let cands = ["a.rs", "b.rs", "c.md"];
        let hits = fuzzy_match("", cands.iter().copied());
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|h| h.score == 0));
    }

    #[test]
    fn matches_subsequence() {
        let cands = ["src/main.rs", "src/lib.rs", "README.md"];
        let hits = fuzzy_match("main", cands.iter().copied());
        assert!(hits.iter().any(|h| h.path == "src/main.rs"));
        // 不含 main 的不出现在结果中
        assert!(hits.iter().all(|h| h.path != "README.md"));
    }

    #[test]
    fn results_sorted_by_score_desc() {
        let cands = ["main.rs", "src/main.rs", "xmain"];
        let hits = fuzzy_match("main", cands.iter().copied());
        // 分数应单调不增
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score, "{:?}", w);
        }
    }

    #[test]
    fn no_match_returns_empty() {
        let cands = ["foo.rs", "bar.rs"];
        let hits = fuzzy_match("zzzzzzz", cands.iter().copied());
        assert!(hits.is_empty());
    }

    #[test]
    fn case_insensitive() {
        let cands = ["Main.rs", "MAIN.RS", "main.rs"];
        let hits = fuzzy_match("main", cands.iter().copied());
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn path_separator_bonus() {
        // nucleo 的 match_paths 配置会对路径分隔符后的首字母加分。
        // 此处仅断言两者都命中、且结果按分数降序（具体相对顺序由评分器决定）。
        let cands = ["src/foo.rs", "foo.rs"];
        let hits = fuzzy_match("foo", cands.iter().copied());
        assert_eq!(hits.len(), 2);
        for w in hits.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[test]
    fn large_candidate_set_no_panic() {
        // 对齐 FR-EXPL 验收：模糊查找首屏 < 100ms（此处验证正确性与无 panic）
        let cands: Vec<String> = (0..10_000).map(|i| format!("file_{i}.rs")).collect();
        let refs: Vec<&str> = cands.iter().map(|s| s.as_str()).collect();
        let hits = fuzzy_match("file_5", refs);
        assert!(!hits.is_empty());
        // 第一条分数最高
        assert!(hits[0].score >= hits[hits.len() - 1].score);
    }
}
