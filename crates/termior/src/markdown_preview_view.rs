use crate::{editor_view::EditorView, markdown_render, ui};
use gpui::{
    div, prelude::*, px, relative, Context, Entity, FontWeight, Render, SharedString, Window,
};
use std::path::PathBuf;
use termior_preview::MarkdownDocument;

pub struct MarkdownPreviewView {
    path: PathBuf,
    source: Entity<EditorView>,
    source_revision: u64,
    source_text: String,
    document: MarkdownDocument,
}

impl MarkdownPreviewView {
    pub fn new(path: PathBuf, source: Entity<EditorView>, cx: &mut Context<Self>) -> Self {
        let (source_revision, source_text) = {
            let source = source.read(cx);
            (source.revision(), source.text())
        };
        let document = MarkdownDocument::parse(&source_text);
        cx.observe(&source, |preview, source, cx| {
            let source_revision = source.read(cx).revision();
            if source_revision == preview.source_revision {
                return;
            }
            preview.source_revision = source_revision;
            preview.source_text = source.read(cx).text();
            preview.document = MarkdownDocument::parse(&preview.source_text);
            cx.notify();
        })
        .detach();
        Self {
            path,
            source,
            source_revision,
            source_text,
            document,
        }
    }

    pub fn contains_text(&self, expected: &str) -> bool {
        self.document.plain_text().contains(expected)
    }

    pub fn source(&self) -> Entity<EditorView> {
        self.source.clone()
    }

    pub fn set_path_after_rename(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.path = path.clone();
        self.source
            .update(cx, |source, _| source.set_path_after_rename(path));
        cx.notify();
    }
}

impl Render for MarkdownPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let title = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Markdown");
        // 滚动块的元素 id 前缀按视图实体区分，多个预览标签并存时不冲突。
        let id_prefix = format!("preview-md-{}", cx.entity().entity_id().as_non_zero_u64());
        let blocks = markdown_render::render_blocks(
            &self.document,
            markdown_render::Density::PREVIEW,
            &id_prefix,
            &p,
        );

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.0))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(ui::border(&p))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child("Markdown Preview"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(ui::muted(&p))
                            .child(SharedString::from(title.to_owned())),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_full()
                            .bg(ui::selected_wash(&p))
                            .text_xs()
                            .text_color(ui::color(p.accent))
                            .child("Live"),
                    ),
            )
            .child(
                div()
                    .id("markdown-preview-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(
                        div().flex().justify_center().w_full().child(
                            div()
                                .w_full()
                                .max_w(px(880.0))
                                .px_8()
                                .py_6()
                                // 正文基准字号：让列表标记等未显式设定字号的
                                // 子元素与 14px 正文一致，而不是窗口默认字号。
                                .text_sm()
                                .line_height(relative(1.55))
                                .children(blocks),
                        ),
                    ),
            )
    }
}
