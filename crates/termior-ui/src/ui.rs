//! 共享 UI 层的入口。
//!
//! 实现已迁到 `termior-ui-kit`，这里只做再导出，让应用内的 `crate::ui::*` 调用
//! 点保持不变。新增控件请加在 ui-kit，不要回流到这里。

pub use termior_ui_kit::{
    button, icon, icon_button,
    theme::{
        alpha, border, color, focus_ring, hover_wash, muted, on_color, palette, selected_wash,
        set_palette, ActiveTheme,
    },
    ButtonKind,
};
