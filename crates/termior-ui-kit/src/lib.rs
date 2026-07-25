//! Termior 的界面基础层：布局/搜索契约，以及（在 `gpui-kit` feature 下）设计
//! token、主题映射与共享控件。
//!
//! 视图层不直接书写间距、圆角、字号与颜色字面量——那些只从这里取。

#![forbid(unsafe_code)]

mod layout;
mod search;
pub mod text;
pub mod tokens;

pub use layout::{LayoutError, LayoutNode, PaneExtent, PaneId, PaneLayout, SplitDirection};
pub use search::{SearchOptions, SearchOverlay};

#[cfg(feature = "gpui-kit")]
mod controls;
#[cfg(feature = "gpui-kit")]
mod empty_state;
#[cfg(feature = "gpui-kit")]
mod icon;
#[cfg(feature = "gpui-kit")]
mod input;
#[cfg(feature = "gpui-kit")]
mod list;
#[cfg(feature = "gpui-kit")]
mod menu;
#[cfg(feature = "gpui-kit")]
pub mod theme;
#[cfg(feature = "gpui-kit")]
pub mod titlebar;

#[cfg(feature = "gpui-kit")]
pub use controls::{button, icon_button, ButtonKind, Tooltip};
#[cfg(feature = "gpui-kit")]
pub use empty_state::{empty_hint, empty_state, empty_state_message};
#[cfg(feature = "gpui-kit")]
pub use icon::{icon, Icon, IconAssets};
#[cfg(feature = "gpui-kit")]
pub use input::input_field;
#[cfg(feature = "gpui-kit")]
pub use list::{list_row, list_row_meta};
#[cfg(feature = "gpui-kit")]
pub use menu::{menu_hint, menu_item, menu_panel, menu_separator};
