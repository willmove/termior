use crate::ui::{self, ButtonKind};
use gpui::{
    div, prelude::*, px, Context, Entity, MouseButton, MouseDownEvent, Render, SharedString, Window,
};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle, WindowHandle};
use termior_platform::open_external;
use termior_preview::{normalize_preview_url, PreviewBackend, PreviewTab};

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct PreviewParentWindow(RawWindowHandle);

#[cfg(target_os = "windows")]
impl HasWindowHandle for PreviewParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: `initialize` captures the GPUI window's live native handle and uses this
        // wrapper only for the synchronous WebView2 controller creation on the same UI thread.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

pub struct PreviewView {
    state: PreviewTab,
    active: bool,
    initialization_started: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    webview: Option<Entity<gpui_wry::WebView>>,
}

impl PreviewView {
    pub fn new(url: String) -> Self {
        let (state, initialization_started) = match normalize_preview_url(&url) {
            Ok(url) => (PreviewTab::new(url).expect("normalized preview URL"), false),
            Err(error) => {
                let mut state =
                    PreviewTab::new("http://localhost:3000").expect("static preview fallback URL");
                state.fallback(format!("Invalid saved preview URL: {error}"));
                (state, true)
            }
        };
        Self {
            state,
            active: false,
            initialization_started,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            webview: None,
        }
    }

    pub fn initialize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.initialization_started {
            return;
        }
        self.initialization_started = true;

        #[cfg(target_os = "windows")]
        {
            let url = self.state.url.clone();
            // WebView2's synchronous constructor pumps the Win32 message queue. Running it
            // directly in a GPUI entity update re-enters foreground tasks while `App` is
            // mutably borrowed and panics. A foreground task itself holds no `App` borrow;
            // only the short handle extraction and final state update do.
            cx.spawn_in(window, async move |preview, cx| {
                let parent = match cx.update(|window, _| {
                    HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
                }) {
                    Ok(Ok(handle)) => PreviewParentWindow(handle),
                    Ok(Err(error)) => {
                        let _ = preview.update_in(cx, |preview, _, cx| {
                            preview.state.fallback(error.to_string());
                            cx.notify();
                        });
                        return;
                    }
                    Err(error) => {
                        log::warn!("preview parent window unavailable: {error}");
                        return;
                    }
                };
                let result = wry::WebViewBuilder::new()
                    .with_url(&url)
                    .build_as_child(&parent);
                let _ = preview.update_in(cx, move |preview, window, cx| {
                    match result {
                        Ok(raw) => {
                            preview.state.embedded_ready();
                            let webview = cx.new(|cx| gpui_wry::WebView::new(raw, window, cx));
                            if !preview.active {
                                webview.update(cx, |webview, _| webview.hide());
                            }
                            preview.webview = Some(webview);
                        }
                        Err(error) => preview.state.fallback(error.to_string()),
                    }
                    cx.notify();
                });
            })
            .detach();
        }

        #[cfg(target_os = "macos")]
        {
            match wry::WebViewBuilder::new()
                .with_url(&self.state.url)
                .build_as_child(window)
            {
                Ok(raw) => {
                    self.state.embedded_ready();
                    let webview = cx.new(|cx| gpui_wry::WebView::new(raw, window, cx));
                    if !self.active {
                        webview.update(cx, |webview, _| webview.hide());
                    }
                    self.webview = Some(webview);
                }
                Err(error) => self.state.fallback(error.to_string()),
            }
            cx.notify();
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let _ = window;
            self.state
                .fallback("embedded WebView unavailable on this platform build");
            cx.notify();
        }
    }

    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
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

    pub fn backend(&self) -> PreviewBackend {
        self.state.backend
    }

    fn reload(&mut self, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(webview) = &self.webview {
            if let Err(error) = webview.read(cx).raw().reload() {
                self.state.last_error = Some(error.to_string());
                log::warn!("reload preview failed: {error}");
                cx.notify();
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let _ = cx;
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
        let p = ui::palette(cx);
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if let Some(webview) = self.webview.clone() {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .bg(ui::color(p.background))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(36.0))
                        .flex_shrink_0()
                        .px_2()
                        .border_b_1()
                        .border_color(ui::border(&p))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_xs()
                                .text_color(ui::muted(&p))
                                .child(SharedString::from(self.state.url.clone())),
                        )
                        .child(
                            ui::button("reload-preview", "Reload", ButtonKind::Subtle, &p)
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::reload)),
                        )
                        .child(
                            ui::button(
                                "open-preview-browser",
                                "Open in browser",
                                ButtonKind::Subtle,
                                &p,
                            )
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::open_browser)),
                        ),
                )
                .child(div().flex_1().min_h(px(0.0)).child(webview))
                .into_any_element();
        }
        let (message, detail) = match self.state.backend {
            PreviewBackend::Pending => (
                "Starting embedded preview…",
                Some("WebView initialization runs asynchronously.".to_owned()),
            ),
            PreviewBackend::ExternalBrowser => (
                "Embedded preview is unavailable.",
                self.state.last_error.clone(),
            ),
            PreviewBackend::Embedded => (
                "Embedded preview is not mounted.",
                self.state.last_error.clone(),
            ),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .child(message)
            .child(
                div()
                    .text_xs()
                    .text_color(ui::muted(&p))
                    .child(SharedString::from(self.state.url.clone())),
            )
            .children(detail.map(|detail| {
                div()
                    .max_w(px(640.0))
                    .text_xs()
                    .text_color(ui::muted(&p))
                    .child(SharedString::from(detail))
            }))
            .children(
                (self.state.backend == PreviewBackend::ExternalBrowser).then(|| {
                    ui::button(
                        "open-preview-browser",
                        "Open in browser",
                        ButtonKind::Primary,
                        &p,
                    )
                    .px_4()
                    .py_2()
                    .text_sm()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::open_browser))
                }),
            )
            .into_any_element()
    }
}
