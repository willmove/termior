use gpui::{WindowBounds, WindowOptions};

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
}
