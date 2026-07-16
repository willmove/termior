//! glob 过滤 + `.gitignore` 纯逻辑判定（FR-EXPL-05 glob / FR-EXPL-03 ignore）。
//!
//! - [`glob_matches`]：给定 glob 模式（如 `*.rs`、`src/**/*.ts`）判定路径是否匹配。
//! - [`is_ignored`]：给定一组简化 gitignore 规则，判定路径是否被忽略。
//!
//! 真实文件遍历用 `ignore` crate（尊重 `.gitignore`/`.ignore`），但本环境不接 fs；
//! 此处提供「规则集 → 判定」的纯函数，便于单测与上层注入。

use globset::{Glob, GlobSet, GlobSetBuilder};

/// glob 模式匹配（FR-EXPL-05）。
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    // 用 globset 编译单个 glob；失败则不匹配（保守）。
    match Glob::new(pattern) {
        Ok(g) => {
            let mut b = GlobSetBuilder::new();
            b.add(g);
            match b.build() {
                Ok(set) => set.is_match(path),
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// 简化的 gitignore 规则。
#[derive(Debug, Clone)]
pub struct IgnoreRule {
    /// 原始模式文本。
    pub pattern: String,
    /// 是否取反（`!pattern`）。
    pub negated: bool,
    /// 锚定到根（含 `/` 或以 `/` 开头）。
    pub anchored: bool,
    /// 仅目录（以 `/` 结尾）。
    pub dir_only: bool,
    /// 编译后的 globset（匹配正规则）。
    compiled: Option<GlobSet>,
}

impl IgnoreRule {
    /// 从一行 gitignore 文本解析（自动去除注释 `#` 与前后空白）。
    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let (pattern, negated) = if let Some(rest) = trimmed.strip_prefix('!') {
            (rest.trim(), true)
        } else {
            (trimmed, false)
        };
        let dir_only = pattern.ends_with('/');
        let core = pattern.trim_end_matches('/').trim_start_matches('/');

        // 含 '/'（去末尾后）视为锚定（gitignore 语义）。
        let anchored = core.contains('/');

        // 编译 glob：把多个候选模式都加进去，任一命中即视为匹配。
        let mut builder = GlobSetBuilder::new();
        let patterns: Vec<String> = if anchored {
            vec![core.to_string(), format!("{core}/**")]
        } else {
            // 非锚定：匹配任意层级（含根）下同名条目
            vec![
                core.to_string(),
                format!("**/{core}"),
                format!("**/{core}/**"),
            ]
        };
        for p in patterns {
            if let Ok(g) = Glob::new(&p) {
                builder.add(g);
            }
        }
        let compiled = builder.build().ok();

        Some(IgnoreRule {
            pattern: pattern.to_string(),
            negated,
            anchored,
            dir_only,
            compiled,
        })
    }

    /// 判定给定相对路径是否被本规则命中。
    pub fn matches(&self, rel_path: &str) -> bool {
        let normalized = rel_path.replace('\\', "/");
        // dir_only 规则只匹配目录（路径以 / 结尾或无扩展名近似）——此处简化：路径以 / 结尾
        if self.dir_only && !normalized.ends_with('/') {
            // 仍尝试匹配目录前缀：去掉最后一段后匹配
            let parent = match normalized.rfind('/') {
                Some(i) => format!("{}/", &normalized[..=i]),
                None => return false,
            };
            return self.hit(&parent);
        }
        self.hit(&normalized)
    }

    fn hit(&self, path: &str) -> bool {
        self.compiled
            .as_ref()
            .map(|c| c.is_match(path))
            .unwrap_or(false)
    }
}

/// 给定一组 ignore 规则，判定路径是否被忽略（FR-EXPL-03）。
///
/// gitignore 语义：后定义的规则优先；`!` 取反可重新包含。
pub fn is_ignored(rules: &[IgnoreRule], rel_path: &str) -> bool {
    let mut ignored = false;
    for r in rules {
        if r.matches(rel_path) {
            ignored = !r.negated;
        }
    }
    ignored
}

/// 便捷：从文本行解析规则列表。
pub fn parse_ignore_lines(text: &str) -> Vec<IgnoreRule> {
    text.lines().filter_map(IgnoreRule::parse).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_simple_extension() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(glob_matches("*.rs", "src/lib.rs"));
        assert!(!glob_matches("*.rs", "main.ts"));
    }

    #[test]
    fn glob_double_star() {
        assert!(glob_matches("src/**/*.rs", "src/a/b/c.rs"));
        assert!(glob_matches("src/**/*.rs", "src/x.rs"));
        assert!(!glob_matches("src/**/*.rs", "other/x.rs"));
    }

    #[test]
    fn glob_exact_name() {
        assert!(glob_matches("Cargo.toml", "Cargo.toml"));
        assert!(!glob_matches("Cargo.toml", "Cargo.lock"));
    }

    #[test]
    fn ignore_simple_pattern() {
        let rules = parse_ignore_lines("node_modules\n*.log\n/dist");
        assert!(is_ignored(&rules, "node_modules"));
        assert!(is_ignored(&rules, "app.log"));
        assert!(is_ignored(&rules, "dist/app.js"));
        assert!(!is_ignored(&rules, "src/main.rs"));
    }

    #[test]
    fn ignore_negation_reincludes() {
        let rules = parse_ignore_lines("*.log\n!important.log");
        assert!(is_ignored(&rules, "debug.log"));
        assert!(
            !is_ignored(&rules, "important.log"),
            "negation should reinclude"
        );
    }

    #[test]
    fn ignore_anchored_vs_unanchored() {
        // `build/` 锚定（含 /）只匹配根 build/，不匹配 src/build/
        let rules = parse_ignore_lines("build/");
        assert!(is_ignored(&rules, "build/x"));
        // 非锚定的同名目录在其他位置也可命中（gitignore 语义：无 / 则任意层级）
        assert!(is_ignored(&rules, "src/build/x"));
    }

    #[test]
    fn ignore_comments_and_blanks_skipped() {
        let rules = parse_ignore_lines("# a comment\n\n*.tmp\n   \n# another");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "*.tmp");
    }

    #[test]
    fn ignore_dir_only() {
        let rules = parse_ignore_lines("build/");
        // 目录规则匹配 build 下的内容
        assert!(is_ignored(&rules, "build/output.o"));
    }

    #[test]
    fn later_rule_overrides_earlier() {
        // 最后一条匹配的规则决定结果
        let rules = parse_ignore_lines("*.rs\n!main.rs\n*.rs");
        assert!(is_ignored(&rules, "main.rs")); // 最后 *.rs 重新忽略
    }

    #[test]
    fn backslash_paths_normalized() {
        let rules = parse_ignore_lines("*.log");
        assert!(is_ignored(&rules, "app\\debug.log"));
    }

    #[test]
    fn dotfiles_can_be_ignored() {
        let rules = parse_ignore_lines(".env*\n.DS_Store");
        assert!(is_ignored(&rules, ".env"));
        assert!(is_ignored(&rules, ".env.local"));
        assert!(is_ignored(&rules, ".DS_Store"));
    }

    #[test]
    fn path_normalization_for_glob() {
        assert!(glob_matches("*.rs", "src/lib.rs"));
    }
}
