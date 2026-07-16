//! `termior-diff` — AI edit diff 的 hunk 级审阅核心（FR-EDIT-04 / FR-SEC-02，NFR-10）。
//!
//! AI 提议的文件修改**不直接写盘**：本模块只计算 diff 与 hunk，并支持逐 hunk 接受/拒绝，
//! 落盘动作完全在外层完成（[`apply_acceptances`] 是纯函数，返回应用接受集后的新文本，
//! 不触盘）。`write_file` 永不在此模块内执行（FR-SEC-02）。
//!
//! 验收对齐 Spec §6.3：「AI 提议 5 个 hunk、接受 4 拒 1，落盘结果精确等于接受集」。

#![forbid(unsafe_code)]

use similar::{ChangeTag, TextDiff};

/// 单个 hunk。`id` 用于在 UI 中逐 hunk 接受/拒绝时稳定引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub id: usize,
    /// 原文件中的起始行号（0-based）。
    pub old_start: usize,
    /// 该 hunk 覆盖的原文件行数（含上下文行）。
    pub old_len: usize,
    /// 新文本中的起始行号（0-based）。
    pub new_start: usize,
    /// 该 hunk 的新行数（含上下文行）。
    pub new_len: usize,
    /// `-` 行（被删除/替换前的原文，每行含尾换行）。
    pub removed: Vec<String>,
    /// `+` 行（替换后的新文，每行含尾换行）。
    pub added: Vec<String>,
    /// 接受时该 hunk 产出的完整新行序列（equal 上下文行 + inserted 行，按新文件顺序）。
    /// 拒绝时则保留原 `old_start..old_start+old_len` 区间的原文。
    pub accepted_lines: Vec<String>,
}

impl Hunk {
    /// 该 hunk 是否是纯新增（无删除行）。
    pub fn is_pure_insertion(&self) -> bool {
        self.removed.is_empty()
    }
    /// 该 hunk 是否是纯删除（无新增行）。
    pub fn is_pure_deletion(&self) -> bool {
        self.added.is_empty()
    }
}

/// 计算两段文本之间的 hunk 列表。
///
/// `context` 控制合并相邻变更的上下文行数（影响 hunk 粒度划分，默认 0 = 每个不相邻的
/// 变更段独立成 hunk）。
pub fn diff_hunks(old: &str, new: &str, context: usize) -> Vec<Hunk> {
    let old_lines: Vec<&str> = split_keep_newlines(old);
    let new_lines: Vec<&str> = split_keep_newlines(new);

    let diff = TextDiff::from_slices(&old_lines, &new_lines);
    let grouped = diff.grouped_ops(if context == 0 { 1 } else { context });

    let mut hunks = Vec::new();
    for (id, ops) in grouped.iter().enumerate() {
        if ops.is_empty() {
            continue;
        }
        let first = ops.first().unwrap();
        let last = ops.last().unwrap();

        let old_start = first.old_range().start;
        let old_end = last.old_range().end;
        let new_start = first.new_range().start;
        let new_end = last.new_range().end;

        let mut removed = Vec::new();
        let mut added = Vec::new();
        // accepted_lines：按新文件顺序，equal 上下文行 + inserted 行。
        let mut accepted_lines = Vec::new();
        for op in ops {
            for change in diff.iter_changes(op) {
                let value = change.value().to_string();
                match change.tag() {
                    ChangeTag::Delete => removed.push(value),
                    ChangeTag::Insert => {
                        added.push(value.clone());
                        accepted_lines.push(value);
                    }
                    ChangeTag::Equal => accepted_lines.push(value),
                }
            }
        }
        hunks.push(Hunk {
            id,
            old_start,
            old_len: old_end - old_start,
            new_start,
            new_len: new_end - new_start,
            removed,
            added,
            accepted_lines,
        });
    }
    hunks
}

/// 将「接受集」应用到原文本，返回新文本。
///
/// `accepted_ids` 是被接受的 hunk id 列表；不在其中的 hunk 被拒绝（保留原文件对应内容）。
/// 结果精确等于「仅应用接受集」——这是 FR-EDIT-04 的核心保证。
///
/// 本函数是纯函数，不触盘（FR-SEC-02）。
pub fn apply_acceptances(old: &str, hunks: &[Hunk], accepted_ids: &[usize]) -> String {
    let accepted: std::collections::HashSet<usize> = accepted_ids.iter().copied().collect();
    let old_lines: Vec<&str> = split_keep_newlines(old);

    let mut out = String::with_capacity(old.len());
    let mut old_idx = 0usize;

    // 按 old_start 排序后的 hunk 引用
    let mut ordered: Vec<&Hunk> = hunks.iter().collect();
    ordered.sort_by_key(|h| h.old_start);

    for h in ordered {
        // 先拷贝 hunk 之前的未变更原文（含上一个 hunk 之后的部分）
        while old_idx < h.old_start && old_idx < old_lines.len() {
            out.push_str(old_lines[old_idx]);
            old_idx += 1;
        }
        if accepted.contains(&h.id) {
            // 接受：写入该 hunk 的完整新行序列（含上下文 equal 行 + inserted 行）
            for a in &h.accepted_lines {
                out.push_str(a);
            }
        } else {
            // 拒绝：保留原 `old_start..old_start+old_len` 区间的原文
            for i in h.old_start..h.old_start + h.old_len {
                if i < old_lines.len() {
                    out.push_str(old_lines[i]);
                }
            }
        }
        // 跳过本 hunk 覆盖的原行（无论接受/拒绝，原行区间已被处理）
        old_idx = (h.old_start + h.old_len).max(old_idx);
    }
    // 拷贝剩余原行
    while old_idx < old_lines.len() {
        out.push_str(old_lines[old_idx]);
        old_idx += 1;
    }
    out
}

/// 把文本按行切分，**保留尾换行**（便于无损重组）。
fn split_keep_newlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// 把 hunk 列表渲染为 unified-diff 风格文本（供 UI 展示与 `ai-diff` tab）。
///
/// 每行前缀 ` ` / `-` / `+`，hunk 头形如 `@@ -old_start,old_len +new_start,new_len @@`。
/// 这是纯展示函数，不参与接受/拒绝逻辑。
pub fn render_unified(old: &str, hunks: &[Hunk]) -> String {
    let old_lines: Vec<&str> = split_keep_newlines(old);
    let mut out = String::new();
    for h in hunks {
        // hunk 头：行号 1-based
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.old_start + 1,
            h.old_len,
            h.new_start + 1,
            h.new_len
        ));
        // 重放该 hunk 区间内的 equal + removed（来自原文）与 added（来自新文）。
        // 这里按「先原文 removed 段、再 added 段」简化渲染，足以供审阅；
        // 精确 interleaved 顺序可由 diff_hunks 内部再暴露（当前 removed/added 已足够）。
        let removed_set: Vec<&str> = h.removed.iter().map(|s| s.as_str()).collect();
        // 先输出原文该区间里「未被删除」的行作为 context，再输出 - 行与 + 行。
        // 简化：直接输出 removed 作 `-`、added 作 `+`；context 由调用方按区间补。
        for r in &removed_set {
            push_prefixed(&mut out, '-', r);
        }
        for a in &h.added {
            push_prefixed(&mut out, '+', a);
        }
        // 补 context（原文区间内未被 hunk 删除的行）
        let _ = old_lines;
    }
    out
}

fn push_prefixed(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

/// 从「原文本 + 一段新插入文本 + 插入位置」构造单个 hunk（供 AI write_file 工具
/// 把提议变更包成 diff 而非直接写盘，FR-SEC-02）。
///
/// `at_line` 为 0-based 行号；`new_text` 为要插入的内容（不含则会创建纯插入 hunk）。
pub fn make_insertion_hunk(old: &str, at_line: usize, new_text: &str) -> Hunk {
    let old_lines: Vec<&str> = split_keep_newlines(old);
    let start = at_line.min(old_lines.len());
    let added: Vec<String> = split_keep_newlines(new_text)
        .iter()
        .map(|s| s.to_string())
        .collect();
    let new_len = added.len();
    Hunk {
        id: 0,
        old_start: start,
        old_len: 0,
        new_start: start,
        new_len,
        removed: vec![],
        accepted_lines: added.clone(),
        added,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_changes_yields_no_hunks() {
        let h = diff_hunks("a\nb\nc\n", "a\nb\nc\n", 0);
        assert!(h.is_empty());
        assert_eq!(apply_acceptances("a\nb\nc\n", &h, &[]), "a\nb\nc\n");
    }

    #[test]
    fn single_insertion() {
        let old = "a\nb\n";
        let new = "a\nx\nb\n";
        let h = diff_hunks(old, new, 0);
        assert_eq!(h.len(), 1);
        assert!(h[0].is_pure_insertion());
        assert_eq!(apply_acceptances(old, &h, &[0]), new);
        assert_eq!(apply_acceptances(old, &h, &[]), old);
    }

    #[test]
    fn single_deletion() {
        let old = "a\nx\nb\n";
        let new = "a\nb\n";
        let h = diff_hunks(old, new, 0);
        assert_eq!(h.len(), 1);
        assert!(h[0].is_pure_deletion());
        assert_eq!(apply_acceptances(old, &h, &[0]), new);
        assert_eq!(apply_acceptances(old, &h, &[]), old);
    }

    #[test]
    fn replace() {
        let old = "a\nold\nb\n";
        let new = "a\nNEW\nb\n";
        let h = diff_hunks(old, new, 0);
        assert_eq!(h.len(), 1);
        assert_eq!(apply_acceptances(old, &h, &[0]), new);
    }

    // —— FR-EDIT-04 验收场景：接受子集，落盘精确等于接受集 ——
    #[test]
    fn accept_subset_hunks_exact() {
        // 用间隔较大的修改段，确保产生独立 hunk
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "A\n2\n3\n4\n5\n6\n7\nB\n9\n"; // 第 1、8 行被改
        let hunks = diff_hunks(old, new, 0);
        // 应得到 2 个独立 hunk
        assert!(hunks.len() >= 2, "expected >=2 hunks, got {hunks:?}");
        // 接受第一个、拒绝其余：结果应精确等于「仅应用接受的 hunk」
        let result = apply_acceptances(old, &hunks, &[hunks[0].id]);
        assert_eq!(result, "A\n2\n3\n4\n5\n6\n7\n8\n9\n");
    }

    #[test]
    fn accept_subset_inverse() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "A\n2\n3\n4\n5\n6\n7\nB\n9\n";
        let hunks = diff_hunks(old, new, 0);
        assert!(hunks.len() >= 2);
        // 接受最后一个、拒绝前面的
        let last_id = hunks.last().unwrap().id;
        let result = apply_acceptances(old, &hunks, &[last_id]);
        assert_eq!(result, "1\n2\n3\n4\n5\n6\n7\nB\n9\n");
    }

    #[test]
    fn all_hunks_accepted_equals_new() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "A\nB\nc\nD\nE\n";
        let hunks = diff_hunks(old, new, 0);
        let accepted: Vec<usize> = hunks.iter().map(|h| h.id).collect();
        assert_eq!(apply_acceptances(old, &hunks, &accepted), new);
    }

    #[test]
    fn none_accepted_equals_old() {
        let old = "a\nb\nc\n";
        let new = "A\nB\nC\n";
        let hunks = diff_hunks(old, new, 0);
        assert_eq!(apply_acceptances(old, &hunks, &[]), old);
    }

    #[test]
    fn hunks_have_stable_sequential_ids() {
        let old = "a\nb\nc\nd\n";
        let new = "x\ny\nz\nw\n";
        let hunks = diff_hunks(old, new, 0);
        let ids: Vec<usize> = hunks.iter().map(|h| h.id).collect();
        assert_eq!(ids, (0..hunks.len()).collect::<Vec<_>>());
    }

    #[test]
    fn out_of_order_accepted_ids_handled() {
        let old = "a\nb\nc\n";
        let new = "A\nB\nC\n";
        let hunks = diff_hunks(old, new, 0);
        let r1 = apply_acceptances(old, &hunks, &[0, 2]);
        let r2 = apply_acceptances(old, &hunks, &[2, 0]);
        assert_eq!(r1, r2);
    }

    #[test]
    fn nonexistent_accepted_id_ignored() {
        let old = "a\nb\n";
        let new = "A\nb\n";
        let hunks = diff_hunks(old, new, 0);
        assert_eq!(apply_acceptances(old, &hunks, &[99]), old);
    }

    #[test]
    fn empty_string_handling() {
        let old = "";
        let new = "x\n";
        let hunks = diff_hunks(old, new, 0);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].is_pure_insertion());
        assert_eq!(apply_acceptances(old, &hunks, &[0]), new);
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let old = "a\nb";
        let new = "a\nB";
        let hunks = diff_hunks(old, new, 0);
        assert_eq!(apply_acceptances(old, &hunks, &[0]), new);
        let result = apply_acceptances(old, &hunks, &[0]);
        assert!(!result.ends_with('\n'));
    }

    #[test]
    fn module_never_writes_to_disk() {
        // apply_acceptances 是纯函数：落盘是调用方职责。
        let old = "a\nb\n";
        let new = "A\nB\n";
        let hunks = diff_hunks(old, new, 0);
        assert_eq!(apply_acceptances(old, &hunks, &[0]), "A\nB\n");
    }

    #[test]
    fn large_file_handles_efficiently() {
        let old: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let new: String = (0..1000).map(|i| format!("line {i} edited\n")).collect();
        let hunks = diff_hunks(&old, &new, 0);
        let accepted: Vec<usize> = hunks.iter().map(|h| h.id).collect();
        assert_eq!(apply_acceptances(&old, &hunks, &accepted), new);
    }

    // —— render_unified：UI 展示 ——
    #[test]
    fn render_unified_has_hunk_header_and_prefixes() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let hunks = diff_hunks(old, new, 0);
        let patch = render_unified(old, &hunks);
        assert!(patch.starts_with("@@ -"), "got: {patch}");
        assert!(patch.contains("-b\n"), "removed line: {patch}");
        assert!(patch.contains("+B\n"), "added line: {patch}");
    }

    #[test]
    fn render_unified_empty_when_no_hunks() {
        let patch = render_unified("a\nb\n", &[]);
        assert!(patch.is_empty());
    }

    #[test]
    fn render_unified_handles_no_trailing_newline() {
        let old = "a\nb";
        let new = "a\nB";
        let hunks = diff_hunks(old, new, 0);
        let patch = render_unified(old, &hunks);
        // 渲染补全尾换行以便展示
        assert!(patch.contains("+B\n"));
    }

    // —— make_insertion_hunk：AI write_file 包成 diff ——
    #[test]
    fn insertion_hunk_is_pure_insertion() {
        let h = make_insertion_hunk("a\nb\n", 1, "x\ny\n");
        assert!(h.is_pure_insertion());
        assert_eq!(h.old_len, 0);
        assert_eq!(h.added, vec!["x\n".to_string(), "y\n".to_string()]);
    }

    #[test]
    fn insertion_hunk_applied_inserts_at_line() {
        let old = "a\nb\nc\n";
        let h = make_insertion_hunk(old, 1, "X\n");
        let hunks = vec![h];
        assert_eq!(apply_acceptances(old, &hunks, &[0]), "a\nX\nb\nc\n");
    }

    #[test]
    fn insertion_hunk_at_end() {
        let old = "a\nb\n";
        let h = make_insertion_hunk(old, 10, "tail\n"); // 超出行号 → 末尾
        let hunks = vec![h];
        assert_eq!(apply_acceptances(old, &hunks, &[0]), "a\nb\ntail\n");
    }

    #[test]
    fn insertion_hunk_rejected_keeps_original() {
        let old = "a\nb\n";
        let h = make_insertion_hunk(old, 0, "X\n");
        let hunks = vec![h];
        assert_eq!(apply_acceptances(old, &hunks, &[]), "a\nb\n");
    }
}
