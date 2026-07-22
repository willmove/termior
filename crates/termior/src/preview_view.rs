use crate::ui::{self, ButtonKind};
use gpui::{
    div, prelude::*, px, Context, MouseButton, MouseDownEvent, Render, SharedString, Window,
};
use termior_platform::open_external;
use termior_preview::{normalize_preview_url, PreviewTab};

/// Web preview view.
///
/// Termior intentionally does **not** embed a WebView (see `docs/adr/0002-remove-embedded-webview.md`).
/// A preview tab only serves the preserved subsystems — localhost detection, URL validation, and
/// the system-browser opener — and renders a placeholder panel whose primary action is
/// "Open in browser". All domain logic (state machine, validation) still lives in `termior-preview`.
pub struct PreviewView {
    state: PreviewTab,
}

impl PreviewView {
    pub fn new(url: String) -> Self {
        let state = match normalize_preview_url(&url) {
            Ok(url) => PreviewTab::new(url).expect("normalized preview URL"),
            Err(error) => {
                let mut state =
                    PreviewTab::new("http://localhost:3000").expect("static preview fallback URL");
                state.fallback(format!("Invalid saved preview URL: {error}"));
                state
            }
        };
        Self { state }
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
        let detail = self.state.last_error.clone().or_else(|| {
            Some(
                "Termior opens web previews in your default browser. Localhost URLs detected in \
                 the terminal and the status-bar pill still route here."
                    .to_owned(),
            )
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .child("Open this preview in your browser")
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
            .child(
                ui::button(
                    "open-preview-browser",
                    "Open in browser",
                    ButtonKind::Primary,
                    &p,
                )
                .px_4()
                .py_2()
                .text_sm()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::open_browser)),
            )
            .into_any_element()
    }
}
