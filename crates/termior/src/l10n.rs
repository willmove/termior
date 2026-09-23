//! 视图层本地化宏（底层运行时见 `termior-i18n` crate）。
//!
//! 三个宏都返回 [`gpui::SharedString`]，可直接传给 `.child(...)` /
//! `.label(...)` 等接受 `impl Into<SharedString>` 的 API；由 `Arc<str>`
//! 引用计数升级而来，逐帧取值不产生堆分配。
//!
//! - [`t!`]：静态文案，键见 `crates/termior-i18n/locales/en.json`。
//! - [`tf!`]：带 `{name}` 占位符的文案。
//! - [`tn!`]：复数文案，按数量在 `key.one` / `key.other` 间选择。
//!
//! 用法示例：
//!
//! ```ignore
//! t!("chrome.settings")
//! tf!("ssh.error.connect", "host" => host, "error" => message)
//! tn!(count, "explorer.skipped")
//! ```
//!
//! 键名约定按来源文件分命名空间（`chrome.` / `settings.` / `ssh.` / …），
//! 与 `locales/*.json` 的分组注释一一对应；缺键时回退英文、再缺则显示键名
//! 本身（奇偶校验测试保证各语言键集一致）。

/// 取当前语言的静态文案。
#[macro_export]
macro_rules! t {
    ($key:expr) => {
        gpui::SharedString::from(::termior_i18n::text($key))
    };
}

/// 取当前语言的文案并替换 `{name}` 占位符（`{{` / `}}` 为字面量大括号）。
#[macro_export]
macro_rules! tf {
    ($key:expr, $($name:literal => $value:expr),+ $(,)?) => {
        gpui::SharedString::from(::termior_i18n::format_key(
            $key,
            &[$(($name, &$value)),+],
        ))
    };
}

/// 取当前语言的复数文案：按数量选择 `key.one` / `key.other` 并替换 `{count}`。
#[macro_export]
macro_rules! tn {
    ($count:expr, $key:expr) => {
        gpui::SharedString::from(::termior_i18n::plural($count, $key))
    };
}
