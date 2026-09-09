use gpui::{point, px, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions};
use termior_ui_kit::{titlebar::MACOS_TRAFFIC_LIGHT_INSET, tokens::height};

pub(crate) const APP_ID: &str = termior_store::paths::BUNDLE_ID;

/// Applies the same desktop identity to every native Termior window.
pub(crate) fn window_options(bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(bounds),
        app_id: Some(APP_ID.to_owned()),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        icon: linux_icon(),
        ..Default::default()
    }
}

/// The main window draws its own titlebar so tabs and the window controls can share
/// one row. Secondary windows (settings) keep the system titlebar — they have no tab
/// strip to merge into it, so a custom one would only cost a row of chrome.
pub(crate) fn main_window_options(bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Termior".into()),
            appears_transparent: true,
            // Vertically centre the traffic lights in our titlebar row; the horizontal
            // offset matches `MACOS_TRAFFIC_LIGHT_INSET`, which reserves the space.
            traffic_light_position: Some(point(px(12.0), px((height::TITLE_BAR - 16.0) / 2.0))),
        }),
        window_decorations: Some(WindowDecorations::Client),
        // Linux CSD：窗口四周的 resize 环带/阴影是透明像素，透出合成器桌面
        // （见 workspace_view::render_window_frame）。无合成器时 gpui 回退
        // Server 装饰，内容满幅绘制，透明背景无副作用。其他平台保持不透明。
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        window_background: gpui::WindowBackgroundAppearance::Transparent,
        ..window_options(bounds)
    }
}

/// Left inset the titlebar content needs so it clears the macOS traffic lights.
pub(crate) const fn titlebar_leading_inset() -> f32 {
    if cfg!(target_os = "macos") {
        MACOS_TRAFFIC_LIGHT_INSET
    } else {
        0.0
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn linux_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    use std::sync::{Arc, OnceLock};

    static ICON: OnceLock<Option<Arc<image::RgbaImage>>> = OnceLock::new();
    ICON.get_or_init(|| {
        image::load_from_memory(include_bytes!("../../../assets/icons/png/256x256.png"))
            .ok()
            .map(|image| Arc::new(image.into_rgba8()))
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_identity_uses_the_persistent_storage_bundle_id() {
        assert_eq!(APP_ID, "app.termior.Termior");
    }

    #[test]
    fn only_the_main_window_hides_the_system_titlebar() {
        let bounds = WindowBounds::Windowed(gpui::Bounds::default());
        let main = main_window_options(bounds);
        assert!(main.titlebar.unwrap().appears_transparent);

        let bounds = WindowBounds::Windowed(gpui::Bounds::default());
        let secondary = window_options(bounds);
        assert!(!secondary.titlebar.unwrap().appears_transparent);
    }
}
