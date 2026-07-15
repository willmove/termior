//! `termior-explorer-core` — 文件浏览器纯逻辑核心（FR-EXPL-04/05）。
//!
//! - [`fuzzy`]：模糊文件查找（nucleo 排序，`Cmd+Shift+F`）。
//! - [`search`]：glob 过滤 + `.gitignore` 纯逻辑判定。
//!
//! 真实文件遍历用 `ignore` crate（FR-EXPL-03），但本环境不接 fs；这里提供「给定候选集 +
//! 查询/glob/ignore 规则」的纯函数，供上层（GPUI 视图 + 后台索引线程）调用。

#![forbid(unsafe_code)]

pub mod fuzzy;
pub mod search;

pub use fuzzy::{fuzzy_match, FuzzyHit};
pub use search::{glob_matches, is_ignored, IgnoreRule};
