//! 尺寸 token —— 界面上一切间距、控件高度、圆角与字号的唯一来源。
//!
//! 全部落在 4px 栅格上。视图层不应再书写这四类尺寸的像素字面量：需要新档位时
//! 在这里加，而不是就地写数字。
//!
//! 终端与编辑器的字号/行高/字间距由用户设置驱动，**不受**本模块约束。

/// 间距（内边距、外边距、gap）。
pub mod space {
    pub const NONE: f32 = 0.0;
    /// 图标与紧邻文字之间。
    pub const HAIR: f32 = 2.0;
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 6.0;
    pub const MD: f32 = 8.0;
    pub const LG: f32 = 12.0;
    pub const XL: f32 = 16.0;
    pub const XXL: f32 = 24.0;
}

/// 控件与容器高度。
pub mod height {
    /// 徽标、状态胶囊。
    pub const TIGHT: f32 = 20.0;
    /// 列表行、状态栏。
    pub const COMPACT: f32 = 24.0;
    /// 图标按钮、紧凑输入框。
    pub const REGULAR: f32 = 28.0;
    /// 常规按钮、活动栏按钮。
    pub const ROOMY: f32 = 32.0;
    /// 标题栏与标签页。窗口控制按钮必须与之等高。
    pub const TITLE_BAR: f32 = 36.0;
}

/// 圆角。三档足够：贴边元素、常规控件、浮层。
pub mod radius {
    pub const SM: f32 = 4.0;
    pub const MD: f32 = 6.0;
    pub const LG: f32 = 8.0;
}

/// 字号。四档，覆盖从状态栏微型文字到区块标题。
pub mod font_size {
    /// 状态栏、徽标、次要计数。
    pub const MICRO: f32 = 11.0;
    /// 界面默认。
    pub const BODY: f32 = 12.0;
    /// 标签页标题、按钮主文案。
    pub const EMPHASIS: f32 = 13.0;
    /// 区块标题、空状态主文案。
    pub const HEADING: f32 = 15.0;
}

/// 图标边长。与 `height` 档位配对使用（如 REGULAR 按钮配 SM 图标）。
pub mod icon_size {
    pub const XS: f32 = 12.0;
    pub const SM: f32 = 16.0;
    pub const MD: f32 = 20.0;
    /// 空状态主图标。
    pub const LG: f32 = 24.0;
}

/// 列表缩进步进（每深入一层）。
pub mod indent {
    pub const STEP: f32 = 12.0;
}

// 档位必须严格递增，否则"往上取一档"就不成立了。编译期拦下。
const _: () = {
    assert!(height::TIGHT < height::COMPACT);
    assert!(height::COMPACT < height::REGULAR);
    assert!(height::REGULAR < height::ROOMY);
    assert!(height::ROOMY < height::TITLE_BAR);
    assert!(font_size::MICRO < font_size::BODY);
    assert!(font_size::BODY < font_size::EMPHASIS);
    assert!(font_size::EMPHASIS < font_size::HEADING);
    assert!(icon_size::XS < icon_size::SM);
    assert!(icon_size::SM < icon_size::MD);
    assert!(icon_size::MD < icon_size::LG);
    assert!(radius::SM < radius::MD);
    assert!(radius::MD < radius::LG);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// token 的价值来自"落在同一栅格上"，一旦有人塞进 13px 这类值就失效了。
    #[test]
    fn every_size_token_sits_on_the_four_pixel_grid() {
        let sizes = [
            space::NONE,
            space::XS,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
            height::TIGHT,
            height::COMPACT,
            height::REGULAR,
            height::ROOMY,
            height::TITLE_BAR,
            radius::SM,
            radius::LG,
            icon_size::XS,
            icon_size::SM,
            icon_size::MD,
            icon_size::LG,
            indent::STEP,
        ];
        for size in sizes {
            assert_eq!(size % 4.0, 0.0, "{size} is off the 4px grid");
        }
        // 半档：只允许 2px 的倍数，用于图标与文字之间这类过小的间隙。
        for size in [space::HAIR, space::SM, radius::MD] {
            assert_eq!(size % 2.0, 0.0, "{size} is not a multiple of 2px");
        }
    }
}
