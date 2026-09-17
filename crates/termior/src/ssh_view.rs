//! User-owned OpenSSH connection manager and isolated authentication prompt.
use crate::ui::{self, ButtonKind};
use gpui::{
    canvas, div, prelude::*, px, App, Bounds, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, Role, SharedString,
    UTF16Selection, Window,
};
use std::{cell::RefCell, ops::Range, path::PathBuf, rc::Rc};

#[derive(Clone)]
struct InputLayout {
    line: gpui::ShapedLine,
    bounds: Bounds<Pixels>,
    offset: Pixels,
}
use termior_ssh::{Authentication, Connection, Profile, Profiles, SessionKind};
use zeroize::Zeroize;

pub struct ProfilesChanged;
impl gpui::EventEmitter<ProfilesChanged> for SshView {}

type ConnectCallback = Box<dyn Fn(Connection, &mut App)>;
pub struct SshView {
    profiles: Profiles,
    dir: Option<PathBuf>,
    selected: Option<usize>,
    values: [String; 9],
    remember: bool,
    auth_prompt: Option<String>,
    scroll: gpui::ScrollHandle,
    input_layouts: Rc<RefCell<Vec<Option<InputLayout>>>>,
    dragging_scroll: bool,
    recursive: bool,
    resume: bool,
    pending_transfer: Option<Connection>,
    auth: Authentication,
    field: usize,
    cursor: usize,
    marked: Option<Range<usize>>,
    select_all: bool,
    focus: FocusHandle,
    status: String,
    load_failed: bool,
    confirm_delete: bool,
    connect: ConnectCallback,
}

impl Drop for SshView {
    fn drop(&mut self) {
        self.clear_secrets();
    }
}

impl SshView {
    fn clear_secrets(&mut self) {
        self.values[7].zeroize();
        self.values[8].zeroize();
    }
    pub fn new(dir: Option<PathBuf>, connect: ConnectCallback, cx: &mut Context<Self>) -> Self {
        let loaded = dir.as_ref().map(|dir| Profiles::load(dir)).transpose();
        let (profiles, status, load_failed) = match loaded {
            Ok(value) => (value.unwrap_or_default(), String::new(), false),
            Err(error) => (
                Profiles::default(),
                format!("无法读取连接配置：{error}。修复文件后重新打开，避免覆盖原配置。"),
                true,
            ),
        };
        Self {
            profiles,
            dir,
            selected: None,
            values: Default::default(),
            remember: false,
            auth_prompt: None,
            scroll: gpui::ScrollHandle::new(),
            input_layouts: Rc::new(RefCell::new(vec![None; 9])),
            dragging_scroll: false,
            auth: Authentication::Auto,
            field: 0,
            cursor: 0,
            marked: None,
            select_all: false,
            focus: cx.focus_handle(),
            status,
            load_failed,
            confirm_delete: false,
            connect,
            recursive: false,
            resume: false,
            pending_transfer: None,
        }
    }
    pub fn new_prompt(prompt: String, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(None, Box::new(|_, _| {}), cx);
        view.auth_prompt = Some(prompt);
        view.field = 7;
        view
    }
    fn submit_prompt(&self) -> ! {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let ok = writeln!(out, "{}", self.values[7])
            .and_then(|_| out.flush())
            .is_ok();
        std::process::exit(if ok { 0 } else { 1 });
    }
    fn scroll_pointer(&self, y: gpui::Pixels) {
        let bounds = self.scroll.bounds();
        let height = f32::from(bounds.size.height).max(1.0);
        let max = f32::from(self.scroll.max_offset().y);
        let thumb = (height * height / (height + max)).max(24.0).min(height);
        let ratio = ((f32::from(y - bounds.top()) - thumb / 2.0) / (height - thumb).max(1.0))
            .clamp(0.0, 1.0);
        self.scroll
            .set_offset(gpui::point(px(0.), px(-max * ratio)));
    }
    fn profile(&self) -> Result<Profile, String> {
        let port = if self.values[3].trim().is_empty() {
            None
        } else {
            Some(
                self.values[3]
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "端口应为 1–65535".to_owned())?,
            )
        };
        let profile = Profile {
            use_saved_credentials: self.remember,
            name: self.values[0].trim().into(),
            host: self.values[1].trim().into(),
            user: self.values[2].trim().into(),
            port,
            identity_file: self.values[4].clone(),
            jump_host: self.values[5].trim().into(),
            authentication: self.auth,
            ..self
                .selected
                .and_then(|i| self.profiles.connections.get(i))
                .cloned()
                .unwrap_or_default()
        };
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }
    fn select(&mut self, index: usize) {
        self.clear_secrets();
        let p = &self.profiles.connections[index];
        self.values = [
            p.name.clone(),
            p.host.clone(),
            p.user.clone(),
            p.port.map(|p| p.to_string()).unwrap_or_default(),
            p.identity_file.clone(),
            p.jump_host.clone(),
            String::new(),
            String::new(),
            String::new(),
        ];
        self.pending_transfer = None;
        self.auth = p.authentication;
        self.remember = p.use_saved_credentials;
        self.selected = Some(index);
        self.marked = None;
        self.select_all = false;
        self.confirm_delete = false;
        self.cursor = self.values[self.field].len();
    }
    fn persist(&mut self, profiles: Profiles) -> Result<(), String> {
        if self.load_failed {
            return Err("配置读取失败，已禁止覆盖".into());
        }
        let dir = self.dir.as_ref().ok_or("应用数据目录不可用")?;
        profiles.save(dir).map_err(|e| e.to_string())?;
        self.profiles = profiles;
        Ok(())
    }
    fn save(&mut self) -> Result<(), String> {
        if self.load_failed {
            return Err("配置读取失败，已禁止覆盖".into());
        }
        if self.dir.is_none() {
            return Err("应用数据目录不可用".into());
        }
        let profile = self.profile()?;
        let previous = self
            .selected
            .and_then(|i| self.profiles.connections.get(i))
            .cloned();
        if !self.remember && (!self.values[7].is_empty() || !self.values[8].is_empty()) {
            return Err("请勾选系统凭据库保存，或清空密码/口令后在连接终端输入".into());
        }
        let mut updated = self.profiles.clone();
        let index = self.selected.unwrap_or(updated.connections.len());
        if index == updated.connections.len() {
            updated.connections.push(profile);
        } else {
            updated.connections[index] = profile;
        }
        updated.validate().map_err(|e| e.to_string())?;
        if self.remember {
            use termior_ssh::credentials::{self, Kind};
            let p = &updated.connections[index];
            if !self.values[7].is_empty() {
                credentials::save(p, Kind::Password, &self.values[7])?;
            }
            if !self.values[8].is_empty() {
                credentials::save(p, Kind::Passphrase, &self.values[8])?;
            }
        }
        self.persist(updated)?;
        self.clear_secrets();
        self.cursor = self.cursor.min(self.values[self.field].len());
        self.selected = Some(index);
        if let Some(previous) = previous {
            if previous.use_saved_credentials
                && !self.profiles.connections.iter().any(|p| {
                    p.use_saved_credentials && termior_ssh::credentials::same_target(p, &previous)
                })
            {
                termior_ssh::credentials::delete_all(&previous).map_err(|_| {
                    "配置已保存，但旧凭据删除失败；请在系统凭据管理器中清理 Termior-ssh 条目"
                        .to_owned()
                })?;
            }
        }
        Ok(())
    }
    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.auth_prompt.is_some() && key.key == "escape" {
            std::process::exit(1);
        }
        if self.auth_prompt.is_some() && key.key == "enter" {
            self.submit_prompt();
        }
        if key.key == "tab" {
            self.field = if self.auth_prompt.is_some() {
                7
            } else {
                (self.field + if key.modifiers.shift { 8 } else { 1 }) % 9
            };
            self.marked = None;
            self.select_all = false;
            self.cursor = self.values[self.field].len();
        } else if (key.modifiers.control || key.modifiers.platform) && key.key == "a" {
            self.select_all = true;
        } else if (key.modifiers.control || key.modifiers.platform) && key.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.replace(None, &text);
            }
        } else if matches!(key.key.as_str(), "left" | "right" | "home" | "end") {
            let text = &self.values[self.field];
            self.cursor = match key.key.as_str() {
                "left" => text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0),
                "right" => text[self.cursor..]
                    .chars()
                    .next()
                    .map(|c| self.cursor + c.len_utf8())
                    .unwrap_or(text.len()),
                "home" => 0,
                _ => text.len(),
            };
            self.select_all = false;
            self.marked = None;
        } else if key.key == "backspace" || key.key == "delete" {
            self.pending_transfer = None;
            if self.select_all {
                self.values[self.field].clear();
                self.select_all = false;
                self.cursor = 0;
            } else if key.key == "delete" {
                if self.cursor < self.values[self.field].len() {
                    self.values[self.field].remove(self.cursor);
                }
            } else {
                let start = self.values[self.field][..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.values[self.field].replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            self.marked = None;
        } else {
            return;
        }
        if key.key == "tab" {
            if let Some(layout) = &self.input_layouts.borrow()[self.field] {
                let viewport = self.scroll.bounds();
                let adjustment = if layout.bounds.bottom() > viewport.bottom() {
                    viewport.bottom() - layout.bounds.bottom() - px(8.)
                } else if layout.bounds.top() < viewport.top() {
                    viewport.top() - layout.bounds.top() + px(8.)
                } else {
                    px(0.)
                };
                self.scroll
                    .set_offset(self.scroll.offset() + gpui::point(px(0.), adjustment));
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
    fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        self.pending_transfer = None;
        let value = &mut self.values[self.field];
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| {
            if self.select_all {
                0..value.encode_utf16().count()
            } else {
                let n = value[..self.cursor].encode_utf16().count();
                n..n
            }
        });
        let start = byte_index(value, range.start);
        let end = byte_index(value, range.end);
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        value.replace_range(start..end, &text);
        self.cursor = start + text.len();
        self.marked = None;
        self.select_all = false;
    }

    fn choose_transfer(&mut self, upload: bool, cx: &mut Context<Self>) {
        if let Err(error) = self.save() {
            self.status = error;
            cx.notify();
            return;
        }
        cx.emit(ProfilesChanged);
        let profile = match self.profile() {
            Ok(p) => p,
            Err(e) => {
                self.status = e;
                cx.notify();
                return;
            }
        };
        let remote_path = self.values[6].clone();
        if let Err(error) = termior_ssh::quote_sftp_path(&remote_path) {
            self.status = error.to_string();
            cx.notify();
            return;
        }
        let recursive = self.recursive;
        let resume = self.resume;
        cx.spawn(async move |this, cx| {
            let dialog = rfd::AsyncFileDialog::new();
            let selected = if recursive {
                dialog.pick_folder().await
            } else if upload {
                dialog.pick_file().await
            } else {
                dialog.save_file().await
            };
            let Some(selected) = selected else {
                return;
            };
            let connection = Connection {
                profile,
                kind: SessionKind::Sftp,
                transfer: Some(termior_ssh::Transfer {
                    upload,
                    recursive,
                    resume,
                    remote_path,
                    local_path: selected.path().to_string_lossy().into_owned(),
                }),
            };
            let _ = this.update(cx, |this, cx| {
                let t = connection.transfer.as_ref().unwrap();
                this.status = format!(
                    "待确认 {}（{} @ {}）：{} {} {}。{}",
                    if upload { "上传" } else { "下载" },
                    connection.profile.name,
                    connection.profile.host,
                    t.local_path,
                    if upload { "→" } else { "←" },
                    t.remote_path,
                    if resume {
                        "续传要求已有内容与源文件一致，否则文件可能损坏。"
                    } else {
                        "目标同名文件将被覆盖。"
                    }
                );
                this.pending_transfer = Some(connection);
                cx.notify();
            });
        })
        .detach();
    }
}
fn byte_index(text: &str, offset: usize) -> usize {
    let mut n = 0;
    for (i, c) in text.char_indices() {
        if n >= offset {
            return i;
        }
        n += c.len_utf16();
    }
    text.len()
}
impl Focusable for SshView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for SshView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let narrow = window.viewport_size().width < px(650.);
        let focus = self.focus.clone();
        let entity = cx.entity();
        let mut list = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(190.))
            .flex_shrink_0()
            .when(narrow, |d| d.w_full().flex_row().flex_wrap());
        list = list.child(
            ui::button("new", "＋ 新建连接", ButtonKind::Subtle, &p).on_click(cx.listener(
                |this, _, _, cx| {
                    this.selected = None;
                    this.clear_secrets();
                    this.values = Default::default();
                    this.cursor = 0;
                    this.select_all = false;
                    this.pending_transfer = None;
                    this.auth = Authentication::Auto;
                    this.remember = false;
                    this.confirm_delete = false;
                    this.status.clear();
                    this.marked = None;
                    cx.emit(ProfilesChanged);
                    cx.notify();
                },
            )),
        );
        for (i, profile) in self.profiles.connections.iter().enumerate() {
            list = list.child(
                ui::button(
                    ("profile", i),
                    SharedString::from(profile.name.clone()),
                    if self.selected == Some(i) {
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Ghost
                    },
                    &p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(i);
                    cx.notify();
                })),
            );
        }
        let mut form = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_w_0()
            .h_auto()
            .flex_shrink_0();
        for (i, label) in [
            "连接名称",
            "主机 / IP / SSH config 别名",
            "用户名（空白沿用 SSH config）",
            "端口（空白沿用 SSH config / 22）",
            "私钥路径（可选，支持加密私钥）",
            "跳板机（可选：user@host:port，逗号分隔多级）",
            "SFTP 远程路径（上传目标 / 下载源）",
            "登录密码（留空保留已保存密码）",
            "私钥口令（留空保留已保存口令）",
        ]
        .into_iter()
        .enumerate()
        {
            if self.auth_prompt.is_some() && i != 7 {
                continue;
            }
            let display = if i >= 7 {
                "•".repeat(self.values[i].encode_utf16().count())
            } else {
                self.values[i].clone()
            };
            let caret = if self.field != i {
                0
            } else if i >= 7 {
                "•".len()
                    * self.values[i][..self.cursor.min(self.values[i].len())]
                        .encode_utf16()
                        .count()
            } else {
                self.cursor.min(self.values[i].len())
            };
            let layouts = self.input_layouts.clone();
            let active = self.field == i;
            let selected = active && self.select_all;
            let foreground = ui::color(p.foreground);
            let accent = ui::color(p.accent);
            let selection = ui::selected_wash(&p);
            form = form
                .child(
                    div()
                        .text_sm()
                        .flex_shrink_0()
                        .child(if self.auth_prompt.is_some() {
                            "认证响应"
                        } else {
                            label
                        }),
                )
                .child(
                    termior_ui_kit::input_field(&p, self.field == i)
                        .relative()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .id(("field", i))
                        .role(Role::TextInput)
                        .aria_label(label)
                        .h(px(38.))
                        .flex_shrink_0()
                        .py_0()
                        .px_2()
                        .flex()
                        .items_center()
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, window, cx| {
                                    let run = gpui::TextRun {
                                        len: display.len(),
                                        font: window.text_style().font(),
                                        color: foreground.into(),
                                        background_color: None,
                                        underline: None,
                                        strikethrough: None,
                                    };
                                    let line = window.text_system().shape_line(
                                        display.into(),
                                        px(14.),
                                        &[run],
                                        None,
                                    );
                                    let caret_x = line.x_for_index(caret);
                                    let offset = if active {
                                        (caret_x - bounds.size.width + px(4.)).max(px(0.))
                                    } else {
                                        px(0.)
                                    };
                                    let origin = bounds.origin
                                        + gpui::point(-offset, (bounds.size.height - px(22.)) / 2.);
                                    if selected {
                                        window.paint_quad(gpui::fill(
                                            Bounds::new(origin, gpui::size(line.width(), px(22.))),
                                            selection,
                                        ));
                                    }
                                    let _ = line.paint(
                                        origin,
                                        px(22.),
                                        gpui::TextAlign::Left,
                                        None,
                                        window,
                                        cx,
                                    );
                                    if active {
                                        window.paint_quad(gpui::fill(
                                            Bounds::new(
                                                origin + gpui::point(caret_x, px(1.)),
                                                gpui::size(px(2.), px(20.)),
                                            ),
                                            accent,
                                        ));
                                    }
                                    layouts.borrow_mut()[i] = Some(InputLayout {
                                        line,
                                        bounds,
                                        offset,
                                    });
                                },
                            )
                            .w_full()
                            .h_full(),
                        )
                        .when(self.field == i, |field| {
                            let focus = focus.clone();
                            let entity = entity.clone();
                            field.child(
                                canvas(
                                    |bounds, _, _| bounds,
                                    move |bounds, _, window, cx| {
                                        window.handle_input(
                                            &focus,
                                            ElementInputHandler::new(bounds, entity.clone()),
                                            cx,
                                        );
                                    },
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                this.field = i;
                                this.cursor = this.input_layouts.borrow()[i]
                                    .as_ref()
                                    .map(|layout| {
                                        let byte = layout.line.closest_index_for_x(
                                            event.position.x - layout.bounds.left() + layout.offset,
                                        );
                                        if i >= 7 {
                                            byte_index(&this.values[i], byte / "•".len())
                                        } else {
                                            byte
                                        }
                                    })
                                    .unwrap_or(this.values[i].len());
                                this.select_all = false;
                                this.marked = None;
                                window.focus(&this.focus, cx);
                                cx.notify();
                            }),
                        ),
                );
        }
        if let Some(prompt) = &self.auth_prompt {
            return div()
                .size_full()
                .p_4()
                .bg(ui::color(p.background))
                .text_color(ui::color(p.foreground))
                .flex()
                .flex_col()
                .gap_3()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::key))
                .child(div().text_sm().child(SharedString::from(prompt.clone())))
                .child(form)
                .child(
                    ui::button("submit", "确认", ButtonKind::Primary, &p)
                        .on_click(cx.listener(|this, _, _, _| this.submit_prompt())),
                )
                .child(
                    ui::button("cancel", "取消", ButtonKind::Ghost, &p)
                        .on_click(|_, _, _| std::process::exit(1)),
                )
                .into_any_element();
        }
        let mut auth = div().flex().flex_wrap().gap_1();
        for (i, (value, label)) in [
            (Authentication::Auto, "自动"),
            (Authentication::Password, "密码 / MFA"),
            (Authentication::Key, "密钥"),
            (Authentication::Agent, "SSH Agent"),
        ]
        .into_iter()
        .enumerate()
        {
            auth = auth.child(
                ui::button(
                    ("auth", i),
                    label,
                    if value == self.auth {
                        ButtonKind::Primary
                    } else {
                        ButtonKind::Subtle
                    },
                    &p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.auth = value;
                    this.pending_transfer = None;
                    cx.notify();
                })),
            );
        }
        form = form.child(auth).child(
            ui::button("remember", if self.remember { "☑ 使用系统凭据库保存密码和私钥口令" } else { "☐ 使用系统凭据库保存密码和私钥口令" }, ButtonKind::Subtle, &p).whitespace_normal().justify_start()
                .on_click(cx.listener(|this, _, _, cx| { this.remember = !this.remember; cx.notify(); }))
        ).child(
            ui::button("forget", "清除该连接的已保存凭据", ButtonKind::Ghost, &p).on_click(cx.listener(|this, _, _, cx| {
                this.status = this.profile().and_then(|p| {
                    termior_ssh::credentials::delete(&p, termior_ssh::credentials::Kind::Password)?;
                    termior_ssh::credentials::delete(&p, termior_ssh::credentials::Kind::Passphrase)
                }).map(|_| { this.clear_secrets(); this.cursor = this.cursor.min(this.values[this.field].len()); "已清除系统凭据".into() }).unwrap_or_else(|e| e);
                cx.notify();
            }))
        ).child(
            div()
                .text_xs()
                .child("密码和私钥口令仅保存到系统凭据库；私钥继续使用原文件或 SSH Agent。首次指纹及 MFA 仍需确认。"),
        );
        let mut actions = div().flex().flex_wrap().gap_2().child(
            ui::button("save", "保存", ButtonKind::Subtle, &p)
                .debug_selector(|| "ssh-save".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.status = this.save().map(|_| "已保存".into()).unwrap_or_else(|e| e);
                    cx.emit(ProfilesChanged);
                    cx.notify();
                })),
        );
        for (i, (kind, label)) in [
            (SessionKind::Shell, "连接 SSH"),
            (SessionKind::Sftp, "打开 SFTP"),
        ]
        .into_iter()
        .enumerate()
        {
            actions = actions.child(
                ui::button(("connect", i), label, ButtonKind::Primary, &p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Err(error) = this.save() {
                            this.status = error;
                            cx.notify();
                            return;
                        }
                        cx.emit(ProfilesChanged);
                        match this.profile() {
                            Ok(profile) => {
                                (this.connect)(
                                    Connection {
                                        profile,
                                        kind,
                                        transfer: None,
                                    },
                                    cx,
                                );
                                this.status =
                                    "连接已在主窗口的新标签打开；请在那里完成认证。".into();
                            }
                            Err(e) => this.status = e,
                        }
                        cx.notify();
                    },
                )),
            );
        }
        actions = actions.child(
            ui::button(
                "delete",
                if self.confirm_delete {
                    "确认删除"
                } else {
                    "删除"
                },
                ButtonKind::Ghost,
                &p,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(index) = this.selected {
                    if this.confirm_delete {
                        let saved = &this.profiles.connections[index];
                        if saved.use_saved_credentials
                            && !this.profiles.connections.iter().enumerate().any(|(i, p)| {
                                i != index
                                    && p.use_saved_credentials
                                    && termior_ssh::credentials::same_target(p, saved)
                            })
                        {
                            let result = termior_ssh::credentials::delete(
                                saved,
                                termior_ssh::credentials::Kind::Password,
                            )
                            .and_then(|_| {
                                termior_ssh::credentials::delete(
                                    saved,
                                    termior_ssh::credentials::Kind::Passphrase,
                                )
                            });
                            if let Err(error) = result {
                                this.status = error;
                                cx.notify();
                                return;
                            }
                        }
                        let mut updated = this.profiles.clone();
                        updated.connections.remove(index);
                        match this.persist(updated) {
                            Ok(()) => {
                                this.selected = None;
                                this.clear_secrets();
                                this.values = Default::default();
                                this.remember = false;
                                cx.emit(ProfilesChanged);
                                this.cursor = 0;
                                this.select_all = false;
                                this.pending_transfer = None;
                                this.status = "已删除配置，活动连接不受影响".into();
                            }
                            Err(e) => this.status = e,
                        }
                        this.confirm_delete = false;
                    } else {
                        this.confirm_delete = true;
                    }
                }
                cx.notify();
            })),
        );
        let transfers = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                ui::button(
                    "recursive",
                    if self.recursive {
                        "☑ 整个目录"
                    } else {
                        "☐ 整个目录"
                    },
                    ButtonKind::Subtle,
                    &p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.recursive = !this.recursive;
                    this.pending_transfer = None;
                    cx.notify();
                })),
            )
            .child(
                ui::button(
                    "resume",
                    if self.resume {
                        "☑ 断点续传"
                    } else {
                        "☐ 断点续传"
                    },
                    ButtonKind::Subtle,
                    &p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.resume = !this.resume;
                    this.pending_transfer = None;
                    cx.notify();
                })),
            )
            .child(
                ui::button("upload", "选择上传…", ButtonKind::Subtle, &p)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_transfer(true, cx))),
            )
            .child(
                ui::button("download", "下载到…", ButtonKind::Subtle, &p)
                    .on_click(cx.listener(|this, _, _, cx| this.choose_transfer(false, cx))),
            );
        let confirmation = self.pending_transfer.is_some().then(|| {
            ui::button(
                "start-transfer",
                "确认路径并开始传输",
                ButtonKind::Primary,
                &p,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(connection) = this.pending_transfer.take() {
                    (this.connect)(connection, cx);
                    this.status = "传输已在主窗口打开，可查看进度、错误或断开取消。".into();
                }
                cx.notify();
            }))
        });
        let scroll = self.scroll.clone();
        let thumb = ui::alpha(p.foreground, 0.5);
        let scrollbar = div()
            .id("ssh-scrollbar")
            .debug_selector(|| "ssh-scrollbar".into())
            .w(px(12.))
            .h_full()
            .flex_shrink_0()
            .cursor_pointer()
            .bg(ui::alpha(p.foreground, 0.08))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.dragging_scroll = true;
                    this.scroll_pointer(event.position.y);
                    cx.notify();
                }),
            )
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |bounds, _, window, _| {
                        let h = f32::from(bounds.size.height);
                        let max = f32::from(scroll.max_offset().y);
                        if h > 0.0 {
                            let th = (h * h / (h + max)).max(24.0).min(h);
                            let top = if max > 0.0 {
                                (-f32::from(scroll.offset().y) / max).clamp(0., 1.) * (h - th)
                            } else {
                                0.
                            };
                            window.paint_quad(gpui::fill(
                                gpui::Bounds::new(
                                    bounds.origin + gpui::point(px(2.), px(top)),
                                    gpui::size(px(8.), px(th)),
                                ),
                                thumb,
                            ));
                        }
                    },
                )
                .size_full(),
            );
        div().id("ssh-manager").role(Role::Group).aria_label("SSH 连接管理").track_focus(&self.focus).on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| { if this.dragging_scroll && e.pressed_button == Some(MouseButton::Left) { this.scroll_pointer(e.position.y); cx.notify(); } else { this.dragging_scroll = false; } }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging_scroll = false))
            .size_full().p_4().bg(ui::color(p.background)).text_color(ui::color(p.foreground)).flex().flex_col().gap_3()
            .child(div().text_lg().flex_shrink_0().child("SSH / SFTP 连接管理"))
            .child(div().flex().flex_1().min_h_0().gap_2()
                .child(div().id("ssh-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().track_scroll(&self.scroll)
                    .child(div().flex().items_start().gap_4().when(narrow, |d| d.flex_col()).child(list.flex_shrink_0()).child(form.when(narrow, |d| d.w_full()).child(actions).child(transfers)
                        .child(div().text_sm().child(SharedString::from(self.status.clone())))
                        .children(confirmation)
                        .child(div().text_xs().child("SFTP：ls / cd 浏览；put / get 上传下载；Ctrl+C 取消；help 查看命令。")))))
                .child(scrollbar)).into_any_element()
    }
}
impl EntityInputHandler for SshView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.field >= 7 {
            return None;
        }
        let value = &self.values[self.field];
        *actual = Some(range.clone());
        Some(value[byte_index(value, range.start)..byte_index(value, range.end)].into())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let n = self.values[self.field].encode_utf16().count();
        let cursor = self.values[self.field][..self.cursor]
            .encode_utf16()
            .count();
        Some(UTF16Selection {
            range: if self.select_all {
                0..n
            } else {
                cursor..cursor
            },
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.clone()
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace(range, text);
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = range
            .as_ref()
            .or(self.marked.as_ref())
            .map(|r| r.start)
            .unwrap_or_else(|| {
                if self.select_all {
                    0
                } else {
                    self.values[self.field][..self.cursor]
                        .encode_utf16()
                        .count()
                }
            });
        self.replace(range, text);
        self.marked = Some(start..start + text.encode_utf16().count());
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layouts = self.input_layouts.borrow();
        let layout = layouts[self.field].as_ref()?;
        let caret = if self.field >= 7 {
            self.values[self.field][..self.cursor]
                .encode_utf16()
                .count()
                * "•".len()
        } else {
            self.cursor
        };
        Some(Bounds::new(
            gpui::point(
                layout.bounds.left() + layout.line.x_for_index(caret) - layout.offset,
                layout.bounds.top() + (layout.bounds.size.height - px(22.)) / 2. + px(1.),
            ),
            gpui::size(px(2.), px(20.)),
        ))
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.values[self.field].encode_utf16().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn save_button_persists_and_notifies_the_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let (view, vcx) = cx.add_window_view(|_, cx| {
            SshView::new(Some(dir.path().into()), Box::new(|_, _| {}), cx)
        });
        let changed = Rc::new(std::cell::Cell::new(false));
        let observed = changed.clone();
        let _subscription = vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, _: &ProfilesChanged, _| observed.set(true))
        });
        view.update(vcx, |v, _| {
            v.values[0] = "saved".into();
            v.values[1] = "host.example".into();
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        view.update(vcx, |v, _| {
            v.scroll
                .set_offset(gpui::point(px(0.), -v.scroll.max_offset().y))
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let button = vcx.debug_bounds("ssh-save").unwrap();
        vcx.simulate_click(button.center(), gpui::Modifiers::default());
        assert_eq!(Profiles::load(dir.path()).unwrap().connections.len(), 1);
        assert!(
            changed.get(),
            "saved profiles must refresh the sidebar immediately"
        );
    }

    #[test]
    fn small_window_scroll_reaches_credentials_and_keeps_inputs_full_height() {
        let mut cx = TestAppContext::single();
        let (view, vcx) = cx.add_window_view(|_, cx| SshView::new(None, Box::new(|_, _| {}), cx));
        for (width, height) in [(480., 320.), (860., 450.)] {
            vcx.simulate_resize(gpui::size(px(width), px(height)));
            vcx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            let scrollbar = vcx
                .debug_bounds("ssh-scrollbar")
                .expect("visible scrollbar");
            assert!(scrollbar.size.height > px(100.));
            assert!(scrollbar.right() <= px(width));
            view.update(vcx, |v, _| {
                assert!(
                    v.scroll.max_offset().y > px(100.),
                    "form must scroll instead of compressing"
                );
                v.scroll
                    .set_offset(gpui::point(px(0.), -v.scroll.max_offset().y));
            });
            vcx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            view.update(vcx, |v, _| {
                let layouts = v.input_layouts.borrow();
                let last = layouts[8].as_ref().expect("password input painted");
                assert!(last.bounds.size.height >= px(30.));
                assert!(last.bounds.bottom() <= v.scroll.bounds().bottom());
                assert!(last.bounds.right() <= v.scroll.bounds().right());
            });
        }
    }

    #[test]
    fn long_input_caret_is_visible_and_secret_text_is_not_exposed_to_ime() {
        let mut cx = TestAppContext::single();
        let (view, vcx) = cx.add_window_view(|_, cx| SshView::new(None, Box::new(|_, _| {}), cx));
        vcx.simulate_resize(gpui::size(px(480.), px(320.)));
        view.update(vcx, |v, _| {
            v.values[0] = "中文 very long hostname ".repeat(20);
            v.cursor = v.values[0].len();
        });
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        view.update(vcx, |v, _| {
            let layouts = v.input_layouts.borrow();
            let layout = layouts[0].as_ref().unwrap();
            let x = layout.line.x_for_index(v.cursor) - layout.offset;
            assert!(layout.offset > px(0.));
            assert!(x >= px(0.) && x <= layout.bounds.size.width - px(2.));
        });
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.field = 7;
                v.values[7] = "private-value".into();
                v.cursor = v.values[7].len();
                assert!(v.text_for_range(0..3, &mut None, window, cx).is_none());
            })
        });
    }

    #[test]
    fn unicode_editing_preserves_cursor_and_ime_replacement() {
        let mut cx = TestAppContext::single();
        let view = cx.new(|cx| SshView::new(None, Box::new(|_, _| {}), cx));
        view.update(&mut cx, |view, _| {
            view.replace(None, "测试😀host");
            view.cursor = "测试".len();
            view.replace(None, "远程");
            assert_eq!(view.values[0], "测试远程😀host");
            view.marked = Some(2..4);
            view.replace(None, "生产");
            assert_eq!(view.values[0], "测试生产😀host");
            assert_eq!(view.cursor, "测试生产".len());
            view.select_all = true;
            view.replace(None, "新\n连接");
            assert_eq!(view.values[0], "新连接");
            assert_eq!(view.cursor, "新连接".len());
        });
    }

    #[test]
    fn editing_profile_preserves_advanced_settings() {
        let mut cx = TestAppContext::single();
        let view = cx.new(|cx| SshView::new(None, Box::new(|_, _| {}), cx));
        view.update(&mut cx, |view, _| {
            view.profiles.connections.push(Profile {
                name: "server".into(),
                host: "example.test".into(),
                connect_timeout_secs: 42,
                keepalive_secs: 60,
                known_hosts_file: "/trust/hosts".into(),
                ..Profile::default()
            });
            view.select(0);
            view.values[0] = "renamed".into();
            let updated = view.profile().unwrap();
            assert_eq!(updated.connect_timeout_secs, 42);
            assert_eq!(updated.known_hosts_file, "/trust/hosts");
            assert_eq!(updated.name, "renamed");
        });
    }
}
