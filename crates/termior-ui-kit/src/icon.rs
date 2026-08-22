//! 图标资源与渲染。
//!
//! 图标是编译期内嵌的 SVG（Lucide 子集，ISC 许可，见 `assets/icons/ui/LICENSE.lucide`），
//! 经 GPUI 的 `svg()` 元素以单色遮罩绘制，颜色跟随 `text_color` —— 所以图标天然
//! 随主题走，不需要为每套主题各准备一份资源。
//!
//! 不使用 Unicode 字形：字形由系统字体决定，跨平台不一致、线宽不统一、光学中心
//! 对不齐，且在高 DPI 下会糊。

use gpui::{prelude::*, px, svg, Rgba, SharedString, Svg};
use std::borrow::Cow;

/// 声明图标集：变体名 → `assets/icons/ui/<file>.svg`。
///
/// 文件在编译期读入，写错文件名会编译失败而不是运行时静默丢图标。
macro_rules! icon_set {
    ($($variant:ident => $file:literal),+ $(,)?) => {
        /// 界面图标。新增图标 = 在 [`icon_set!`] 里加一行 + 放一个 SVG 文件。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($variant),+
        }

        impl Icon {
            /// 交给 GPUI `svg().path(..)` 的资源路径，由 [`IconAssets`] 解析。
            pub fn path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("icons/ui/", $file, ".svg")),+
                }
            }

            /// 全部图标，供测试遍历。
            pub const ALL: &'static [Icon] = &[$(Self::$variant),+];
        }

        const EMBEDDED: &[(&str, &[u8])] = &[
            $((
                concat!("icons/ui/", $file, ".svg"),
                include_bytes!(concat!("../../../assets/icons/ui/", $file, ".svg")),
            )),+
        ];
    };
}

icon_set! {
    Files => "files",
    GitBranch => "git-branch",
    GitCommit => "git-commit-vertical",
    Close => "x",
    Plus => "plus",
    Settings => "settings",
    Bell => "bell",
    WindowMinimize => "minus",
    WindowMaximize => "square",
    WindowRestore => "copy",
    Folder => "folder",
    FolderOpen => "folder-open",
    File => "file",
    FileCode => "file-code",
    FileText => "file-text",
    FileImage => "file-image",
    FileCog => "file-cog",
    Braces => "braces",
    Search => "search",
    Refresh => "refresh-cw",
    ChevronRight => "chevron-right",
    ChevronDown => "chevron-down",
    Terminal => "terminal",
    Inbox => "inbox",
    PanelLeft => "layout-panel-left",
}

/// 内嵌图标的 GPUI 资源源。在 `Application::with_assets` 上安装一次即可。
pub struct IconAssets;

impl gpui::AssetSource for IconAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(EMBEDDED
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(EMBEDDED
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// 一个按给定边长与颜色绘制的图标。
///
/// 尺寸取自 [`crate::tokens::icon_size`]，调用方传入而非就地写数字。
pub fn icon(icon: Icon, size: f32, color: Rgba) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource as _;

    #[test]
    fn every_icon_resolves_to_embedded_bytes() {
        for &variant in Icon::ALL {
            let bytes = IconAssets
                .load(variant.path())
                .expect("asset lookup succeeds")
                .unwrap_or_else(|| panic!("{variant:?} has no embedded bytes"));
            assert!(
                bytes.starts_with(b"<svg"),
                "{variant:?} does not look like an SVG"
            );
        }
    }

    #[test]
    fn unknown_paths_resolve_to_none() {
        assert!(IconAssets.load("icons/ui/nope.svg").unwrap().is_none());
    }

    #[test]
    fn listing_the_icon_directory_returns_every_icon() {
        let listed = IconAssets.list("icons/ui/").unwrap();
        assert_eq!(listed.len(), Icon::ALL.len());
    }
}
