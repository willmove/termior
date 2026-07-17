use gpui::{
    div, prelude::*, Context, Entity, MouseButton, MouseDownEvent, Render, SharedString, Window,
};
use termior_platform::open_external;
use termior_preview::PreviewTab;

pub struct PreviewView {
    state: PreviewTab,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    webview: Option<Entity<gpui_wry::WebView>>,
}

impl PreviewView {
    pub fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = PreviewTab::new(url.clone())
            .unwrap_or_else(|_| PreviewTab::new("http://localhost:3000").unwrap());
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let webview = match wry::WebViewBuilder::new()
            .with_url(&state.url)
            .build_as_child(window)
        {
            Ok(raw) => {
                state.embedded_ready();
                Some(cx.new(|cx| gpui_wry::WebView::new(raw, window, cx)))
            }
            Err(error) => {
                state.fallback(error.to_string());
                None
            }
        };
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        state.fallback("embedded WebView unavailable on this platform build");
        Self {
            state,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            webview,
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(webview) = &self.webview {
            webview.update(cx, |webview, _| {
                if active {
                    webview.show();
                } else {
                    webview.hide();
                }
            });
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = (active, cx);
    }

    fn open_browser(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Err(error) = open_external(&self.state.url) {
            log::warn!("open browser failed: {error}");
        }
    }
}

impl Render for PreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(webview) = self.webview.clone() {
            return div().size_full().child(webview).into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child("Embedded preview is unavailable; using the system browser.")
            .child(SharedString::from(self.state.url.clone()))
            .child(
                div()
                    .id("open-preview-browser")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(gpui::rgba(0x4f8fefff))
                    .cursor_pointer()
                    .child("Open in browser")
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::open_browser)),
            )
            .into_any_element()
    }
}
