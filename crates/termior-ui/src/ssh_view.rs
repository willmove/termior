//! User-owned OpenSSH connection manager and isolated authentication prompt.
use crate::ui::{self, ButtonKind};
use gpui::{
    canvas, div, prelude::*, px, App, Bounds, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, Pixels, Point, Role, SharedString,
    UTF16Selection, Window,
};
use std::{cell::RefCell, ops::Range, path::PathBuf, rc::Rc};

use termior_ssh::{Authentication, Connection, Profile, Profiles, SessionKind};
use termior_ui_kit::tokens::{font_size, height, space};
use zeroize::Zeroize;

#[derive(Clone)]
struct InputLayout {
    line: gpui::ShapedLine,
    bounds: Bounds<Pixels>,
    offset: Pixels,
}

/// 输入行文字度量：字号来自 token，行高与光标高配套，
/// 让文字在 REGULAR（28px）高的输入框里垂直居中。
const INPUT_LINE_HEIGHT: f32 = 20.0;
const INPUT_CARET_HEIGHT: f32 = 16.0;

/// Tab 键序 = 表单视觉顺序：名称 → 分组/标签 → 主机/端口 → 用户/私钥 →
/// 密码/口令 →（高级：跳板机、远程路径）→ 备注。高级折叠时跳过其中两项。
const FIELD_TAB_ORDER: [usize; 12] = [0, 9, 10, 1, 3, 2, 4, 7, 8, 5, 6, 11];

pub struct ProfilesChanged;
impl gpui::EventEmitter<ProfilesChanged> for SshView {}

type ConnectCallback = Box<dyn Fn(Connection, &mut App)>;

// Read the latest settings, never overwrite a manager edit with the prompt's
// snapshot. The JSON stores only the opt-in flag; the secret goes to the OS vault.
fn save_prompt_preference(
    dir: Option<&std::path::Path>,
    profile: &Profile,
    kind: termior_ssh::credentials::Kind,
    secret: &str,
    remember: bool,
) -> Result<(), String> {
    use termior_ssh::credentials;
    if secret.is_empty() || secret.len() > 1023 || secret.contains(['\0', '\r', '\n']) {
        return Err(t!("ssh.error.secret_invalid").to_string());
    }
    let Some(dir) = dir else {
        return if remember {
            Err(t!("ssh.error.no_app_data_remember").to_string())
        } else {
            Ok(())
        };
    };
    let mut profiles = Profiles::load(dir).map_err(|e| e.to_string())?;
    let Some(saved) = profiles
        .connections
        .iter_mut()
        .find(|saved| saved.name == profile.name && credentials::same_target(saved, profile))
    else {
        return if remember {
            Err(t!("ssh.error.profile_changed").to_string())
        } else {
            Ok(())
        };
    };
    let was_remembered = saved.use_saved_credentials;
    if !remember && !was_remembered {
        return Ok(()); // Session-only login needs neither a vault nor a settings write.
    }
    saved.use_saved_credentials = remember;
    if remember {
        credentials::save(profile, kind, secret)?;
    }
    profiles.save(dir).map_err(|e| e.to_string())?;
    if !remember
        && was_remembered
        && !profiles
            .connections
            .iter()
            .any(|saved| saved.use_saved_credentials && credentials::same_target(saved, profile))
    {
        credentials::delete_all(profile)?;
    }
    Ok(())
}

pub struct SshView {
    profiles: Profiles,
    dir: Option<PathBuf>,
    selected: Option<usize>,
    /// 表单缓冲区，下标即字段：
    /// `[0 name, 1 host, 2 user, 3 port, 4 identity, 5 jump_host,
    ///   6 sftp_remote_path, 7 password, 8 passphrase, 9 group,
    ///   10 tags, 11 notes]`（7、8 是密钥，输入后 zeroize）。
    values: [String; 12],
    remember: bool,
    auth_prompt: Option<String>,
    shared_auth: Option<termior_ssh::auth::Challenge>,
    scroll: gpui::ScrollHandle,
    input_layouts: Rc<RefCell<Vec<Option<InputLayout>>>>,
    dragging_scroll: bool,
    /// “高级选项”（跳板机 / SFTP 远程路径 / 传输选项）默认折叠。
    advanced_open: bool,
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
        if let Some(challenge) = self.shared_auth.take() {
            let _ = challenge.reply.send(termior_ssh::auth::Answer::Cancel);
        }
    }
}

impl SshView {
    /// Keep an already-open manager from writing a stale list after sidebar deletion.
    /// Preserve unrelated form edits, but discard the form for a removed profile.
    pub fn refresh_saved_profiles(&mut self, cx: &mut Context<Self>) {
        let selected_name = self
            .selected
            .and_then(|i| self.profiles.connections.get(i))
            .map(|p| p.name.clone());
        let Some(dir) = &self.dir else {
            return;
        };
        match Profiles::load(dir) {
            Ok(profiles) => {
                self.selected = selected_name
                    .as_ref()
                    .and_then(|name| profiles.connections.iter().position(|p| &p.name == name));
                self.profiles = profiles;
                if selected_name.is_some() && self.selected.is_none() {
                    self.clear_secrets();
                    self.values = Default::default();
                    self.cursor = 0;
                    self.marked = None;
                    self.select_all = false;
                    self.pending_transfer = None;
                    self.confirm_delete = false;
                    self.status = t!("ssh.status.session_deleted").to_string();
                }
            }
            Err(error) => {
                self.load_failed = true;
                self.status = error.to_string();
            }
        }
        cx.notify();
    }
    /// Open the requested saved connection, including when reusing the manager window.
    pub fn edit_saved(&mut self, name: &str, rename: bool, cx: &mut Context<Self>) {
        if let Some(dir) = &self.dir {
            match Profiles::load(dir) {
                Ok(profiles) => self.profiles = profiles,
                Err(error) => {
                    self.status = error.to_string();
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(index) = self
            .profiles
            .connections
            .iter()
            .position(|p| p.name == name)
        {
            self.field = if rename { 0 } else { 1 };
            self.select(index);
            self.select_all = rename;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            self.status = if rename {
                t!("ssh.status.rename_hint")
            } else {
                t!("ssh.status.edit_hint")
            }
            .to_string();
        }
        cx.notify();
    }
    pub fn refresh_credential_preferences(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(latest) = Profiles::load(dir) else {
            return;
        };
        for (index, profile) in self.profiles.connections.iter_mut().enumerate() {
            if let Some(saved) = latest.connections.iter().find(|p| {
                p.name == profile.name && termior_ssh::credentials::same_target(p, profile)
            }) {
                // Preserve an explicitly edited, not-yet-saved checkbox.
                if self.selected == Some(index) && self.remember == profile.use_saved_credentials {
                    self.remember = saved.use_saved_credentials;
                }
                profile.use_saved_credentials = saved.use_saved_credentials;
            }
        }
        cx.notify();
    }
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
                tf!("ssh.error.load_failed", "error" => error).to_string(),
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
            shared_auth: None,
            scroll: gpui::ScrollHandle::new(),
            input_layouts: Rc::new(RefCell::new(vec![None; 12])),
            dragging_scroll: false,
            advanced_open: false,
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
    pub fn new_shared_prompt(
        challenge: termior_ssh::auth::Challenge,
        dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new(dir, Box::new(|_, _| {}), cx);
        view.auth_prompt = Some(
            tf!(
                "ssh.prompt.shared_title",
                "name" => challenge.profile.name,
                "host" => challenge.profile.host,
                "prompt" => challenge.prompt
            )
            .to_string(),
        );
        view.remember = challenge.remember;
        view.status = challenge.error.clone().unwrap_or_default();
        view.shared_auth = Some(challenge);
        view.field = 7;
        view
    }
    fn cancel_prompt(&mut self, window: &mut Window) {
        if let Some(challenge) = self.shared_auth.take() {
            let _ = challenge.reply.send(termior_ssh::auth::Answer::Cancel);
            self.clear_secrets();
            window.remove_window();
        } else {
            std::process::exit(1);
        }
    }
    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use termior_ssh::auth::{Answer, ChallengeKind};
        if let Some(challenge) = self.shared_auth.as_ref() {
            if challenge
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
            {
                self.cancel_prompt(window);
                return;
            }
            if let Some(kind) = challenge.kind.credential() {
                if let Err(error) = save_prompt_preference(
                    self.dir.as_deref(),
                    &challenge.profile,
                    kind,
                    &self.values[7],
                    self.remember,
                ) {
                    self.status = error;
                    cx.notify();
                    return;
                }
                cx.emit(ProfilesChanged);
            }
            let answer = match challenge.kind {
                ChallengeKind::Confirm => Answer::Confirm(true),
                ChallengeKind::Info => Answer::Acknowledge,
                _ => Answer::Secret {
                    value: zeroize::Zeroizing::new(std::mem::take(&mut self.values[7])),
                    remember: self.remember,
                },
            };
            let challenge = self.shared_auth.take().unwrap();
            let _ = challenge.reply.send(answer);
            self.clear_secrets();
            window.remove_window();
            return;
        }
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        let ok = writeln!(out, "{}", self.values[7])
            .and_then(|_| out.flush())
            .is_ok();
        std::process::exit(if ok { 0 } else { 1 });
    }
    fn next_field(&self, backward: bool) -> usize {
        if self.auth_prompt.is_some() {
            return 7;
        }
        let reachable: Vec<usize> = FIELD_TAB_ORDER
            .iter()
            .copied()
            .filter(|i| self.advanced_open || !matches!(i, 5 | 6))
            .collect();
        let n = reachable.len();
        let at = reachable.iter().position(|i| *i == self.field).unwrap_or(0);
        reachable[(at + if backward { n - 1 } else { 1 }) % n]
    }
    fn profile(&self) -> Result<Profile, String> {
        let port = if self.values[3].trim().is_empty() {
            None
        } else {
            Some(
                self.values[3]
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| t!("ssh.error.port_range").to_string())?,
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
            group: self.values[9].trim().into(),
            tags: parse_tags(&self.values[10]),
            notes: self.values[11].trim().into(),
            sftp_remote_path: self.values[6].trim().into(),
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
    /// 左栏连接按钮；`index` 是 connections 的真实下标，分组渲染不改变它。
    fn profile_button(
        &self,
        index: usize,
        p: &termior_theme::ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        ui::button(
            ("profile", index),
            SharedString::from(self.profiles.connections[index].name.clone()),
            if self.selected == Some(index) {
                ButtonKind::Primary
            } else {
                ButtonKind::Ghost
            },
            p,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select(index);
            cx.notify();
        }))
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
            p.sftp_remote_path.clone(),
            String::new(),
            String::new(),
            p.group.clone(),
            p.tags.join(", "),
            p.notes.clone(),
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
            return Err(t!("ssh.error.write_blocked").to_string());
        }
        let dir = self
            .dir
            .as_ref()
            .ok_or_else(|| t!("ssh.error.no_app_data").to_string())?;
        profiles.save(dir).map_err(|e| e.to_string())?;
        self.profiles = profiles;
        Ok(())
    }
    fn save(&mut self) -> Result<(), String> {
        if self.load_failed {
            return Err(t!("ssh.error.write_blocked").to_string());
        }
        if self.dir.is_none() {
            return Err(t!("ssh.error.no_app_data").to_string());
        }
        let profile = self.profile()?;
        let previous = self
            .selected
            .and_then(|i| self.profiles.connections.get(i))
            .cloned();
        if !self.remember && (!self.values[7].is_empty() || !self.values[8].is_empty()) {
            return Err(t!("ssh.error.remember_required").to_string());
        }
        let mut updated = self.profiles.clone();
        let index = self.selected.unwrap_or(updated.connections.len());
        if index == updated.connections.len() {
            updated.connections.push(profile.clone());
        } else {
            updated.connections[index] = profile.clone();
        }
        // 表单里可以直接输入新分组名；保存前确保它进入分组列表。
        updated.upsert_group(&profile.group);
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
                termior_ssh::credentials::delete_all(&previous)
                    .map_err(|_| t!("ssh.error.stale_credentials").to_string())?;
            }
        }
        Ok(())
    }
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.auth_prompt.is_some() && key.key == "escape" {
            self.cancel_prompt(window);
            return;
        }
        if self.auth_prompt.is_some() && key.key == "enter" {
            self.submit_prompt(window, cx);
            return;
        }
        if key.key == "tab" {
            self.field = self.next_field(key.modifiers.shift);
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
                let transfer = connection.transfer.as_ref().unwrap();
                this.status = tf!(
                    "ssh.transfer.pending",
                    "action" => if upload { t!("ssh.transfer.upload") } else { t!("ssh.transfer.download") },
                    "name" => connection.profile.name,
                    "host" => connection.profile.host,
                    "local" => transfer.local_path,
                    "arrow" => if upload { "→" } else { "←" },
                    "remote" => transfer.remote_path,
                    "warning" => if resume {
                        t!("ssh.transfer.resume_warning")
                    } else {
                        t!("ssh.transfer.overwrite_warning")
                    }
                )
                .to_string();
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

/// 表单里的标签输入以逗号（含全角）分隔；保存前解析并去掉空项。
fn parse_tags(input: &str) -> Vec<String> {
    input
        .split([',', '，'])
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 把字段单元格按行组装成竖排表单段；行内从左到右，单元格 flex_1 平分宽度。
fn field_rows(rows: &[&[usize]], cells: &mut [Option<gpui::Div>]) -> gpui::Div {
    let mut grid = div().flex().flex_col().gap(px(space::SM));
    for row in rows {
        if !row.iter().any(|i| cells[*i].is_some()) {
            continue;
        }
        let mut fields = div().flex().items_start().gap(px(space::SM));
        for &i in *row {
            if let Some(cell) = cells[i].take() {
                fields = fields.child(cell);
            }
        }
        grid = grid.child(fields);
    }
    grid
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
            .gap(px(space::SM))
            .w(px(180.))
            .flex_shrink_0()
            .when(narrow, |d| d.w_full().flex_row().flex_wrap());
        list = list.child(
            ui::button("new", t!("ssh.new_connection"), ButtonKind::Subtle, &p).on_click(
                cx.listener(|this, _, _, cx| {
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
                }),
            ),
        );
        // 未分组连接在前；分组名作为静态小标签行（含空分组），管理窗口不做折叠。
        let partition =
            termior_ssh::group_connections(&self.profiles.connections, &self.profiles.groups);
        for index in &partition.ungrouped {
            list = list.child(self.profile_button(*index, &p, cx));
        }
        for (group, members) in &partition.groups {
            list = list.child(
                div()
                    .pl(px(space::XS))
                    .pt(px(space::XS))
                    .text_size(px(font_size::MICRO))
                    .text_color(ui::muted(&p))
                    .child(SharedString::from(group.clone())),
            );
            for index in members {
                list = list.child(self.profile_button(*index, &p, cx));
            }
        }
        let mut form = div()
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .flex_1()
            .min_w_0()
            .h_auto()
            .flex_shrink_0()
            .when(self.auth_prompt.is_some(), |form| form.flex_none());
        // 单元格先按字段下标构建，行布局由 field_rows 按 visual order 组装。
        let mut cells: Vec<Option<gpui::Div>> = (0..12).map(|_| None).collect();
        for (i, label) in [
            t!("ssh.field.name"),
            t!("ssh.field.host"),
            t!("ssh.field.user"),
            t!("ssh.field.port"),
            t!("ssh.field.identity"),
            t!("ssh.field.jump"),
            t!("ssh.field.remote_path"),
            t!("ssh.field.password"),
            t!("ssh.field.passphrase"),
            t!("ssh.field.group"),
            t!("ssh.field.tags"),
            t!("ssh.field.notes"),
        ]
        .into_iter()
        .enumerate()
        {
            if self.auth_prompt.is_some() && i != 7 {
                continue;
            }
            let display = if matches!(i, 7 | 8) {
                "•".repeat(self.values[i].encode_utf16().count())
            } else {
                self.values[i].clone()
            };
            let caret = if self.field != i {
                0
            } else if matches!(i, 7 | 8) {
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
            cells[i] = Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(space::XS))
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(font_size::BODY)).flex_shrink_0().child(
                        if self.auth_prompt.is_some() {
                            t!("ssh.field.auth_response")
                        } else {
                            label.clone()
                        },
                    ))
                    .child(
                        termior_ui_kit::input_field(&p, self.field == i)
                            .relative()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .id(("field", i))
                            .role(Role::TextInput)
                            .aria_label(label)
                            .h(px(height::REGULAR))
                            .flex_shrink_0()
                            .py_0()
                            .px(px(space::MD))
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
                                            px(font_size::BODY),
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
                                            + gpui::point(
                                                -offset,
                                                (bounds.size.height - px(INPUT_LINE_HEIGHT)) / 2.,
                                            );
                                        if selected {
                                            window.paint_quad(gpui::fill(
                                                Bounds::new(
                                                    origin,
                                                    gpui::size(line.width(), px(INPUT_LINE_HEIGHT)),
                                                ),
                                                selection,
                                            ));
                                        }
                                        let _ = line.paint(
                                            origin,
                                            px(INPUT_LINE_HEIGHT),
                                            gpui::TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                        if active {
                                            window.paint_quad(gpui::fill(
                                                Bounds::new(
                                                    origin
                                                        + gpui::point(
                                                            caret_x,
                                                            px((INPUT_LINE_HEIGHT
                                                                - INPUT_CARET_HEIGHT)
                                                                / 2.),
                                                        ),
                                                    gpui::size(px(2.), px(INPUT_CARET_HEIGHT)),
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
                                cx.listener(
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        this.field = i;
                                        this.cursor = this.input_layouts.borrow()[i]
                                            .as_ref()
                                            .map(|layout| {
                                                let byte = layout.line.closest_index_for_x(
                                                    event.position.x - layout.bounds.left()
                                                        + layout.offset,
                                                );
                                                if matches!(i, 7 | 8) {
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
                                    },
                                ),
                            ),
                    ),
            );
        }
        // 基本区：名称独占一行；分组+标签、主机+端口、用户+私钥、密码+口令两列。
        let grid = field_rows(&[&[0], &[9, 10], &[1, 3], &[2, 4], &[7, 8]], &mut cells);
        form = form.child(grid);
        if let Some(prompt) = &self.auth_prompt {
            let kind = self.shared_auth.as_ref().map(|challenge| challenge.kind);
            let has_secret = !matches!(
                kind,
                Some(
                    termior_ssh::auth::ChallengeKind::Confirm
                        | termior_ssh::auth::ChallengeKind::Info
                )
            );
            let can_remember = kind.is_some_and(|kind| kind.credential().is_some());
            return div()
                .id("ssh-auth-prompt")
                .size_full()
                .overflow_y_scroll()
                .p(px(space::LG))
                .bg(ui::color(p.background))
                .text_color(ui::color(p.foreground))
                .flex()
                .flex_col()
                .gap(px(space::MD))
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::key))
                .child(
                    div()
                        .text_size(px(font_size::BODY))
                        .child(SharedString::from(prompt.clone())),
                )
                .when(has_secret, |element| element.child(form))
                .when(can_remember, |element| {
                    element
                        .child(
                            ui::button(
                                "remember-auth",
                                if self.remember {
                                    t!("ssh.prompt.remember.on")
                                } else {
                                    t!("ssh.prompt.remember.off")
                                },
                                ButtonKind::Subtle,
                                &p,
                            )
                            .debug_selector(|| "remember-auth".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remember = !this.remember;
                                cx.notify();
                            })),
                        )
                        .child(
                            div()
                                .text_size(px(font_size::MICRO))
                                .text_color(ui::muted(&p))
                                .child(t!("ssh.prompt.remember_hint")),
                        )
                })
                .when(!self.status.is_empty(), |element| {
                    element.child(
                        div()
                            .text_size(px(font_size::BODY))
                            .child(SharedString::from(self.status.clone())),
                    )
                })
                .child(
                    ui::button("submit", t!("ssh.prompt.confirm"), ButtonKind::Primary, &p)
                        .debug_selector(|| "submit".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_prompt(window, cx)),
                        ),
                )
                .child(
                    ui::button("cancel", t!("ssh.prompt.cancel"), ButtonKind::Ghost, &p)
                        .on_click(cx.listener(|this, _, window, _| this.cancel_prompt(window))),
                )
                .into_any_element();
        }
        let mut auth = div().flex().flex_wrap().gap(px(space::XS));
        for (i, (value, label)) in [
            (Authentication::Auto, t!("ssh.auth.auto")),
            (Authentication::Password, t!("ssh.auth.password")),
            (Authentication::Key, t!("ssh.auth.key")),
            (Authentication::Agent, t!("ssh.auth.agent")),
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
        form = form
            .child(auth)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(space::XS))
                    .child(
                        ui::button(
                            "remember",
                            if self.remember {
                                t!("ssh.remember.on")
                            } else {
                                t!("ssh.remember.off")
                            },
                            ButtonKind::Subtle,
                            &p,
                        )
                        .whitespace_normal()
                        .justify_start()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.remember = !this.remember;
                            cx.notify();
                        })),
                    )
                    .child(
                        ui::button(
                            "forget",
                            t!("ssh.forget_credentials"),
                            ButtonKind::Ghost,
                            &p,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.status = this
                                .profile()
                                .and_then(|p| {
                                    termior_ssh::credentials::delete(
                                        &p,
                                        termior_ssh::credentials::Kind::Password,
                                    )?;
                                    termior_ssh::credentials::delete(
                                        &p,
                                        termior_ssh::credentials::Kind::Passphrase,
                                    )
                                })
                                .map(|_| {
                                    this.clear_secrets();
                                    this.cursor = this.cursor.min(this.values[this.field].len());
                                    t!("ssh.status.credentials_cleared").to_string()
                                })
                                .unwrap_or_else(|e| e);
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .text_size(px(font_size::MICRO))
                    .text_color(ui::muted(&p))
                    .child(t!("ssh.credentials_hint")),
            );
        let transfers = div()
            .flex()
            .flex_wrap()
            .gap(px(space::XS))
            .child(
                ui::button(
                    "recursive",
                    if self.recursive {
                        t!("ssh.transfer.recursive.on")
                    } else {
                        t!("ssh.transfer.recursive.off")
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
                        t!("ssh.transfer.resume.on")
                    } else {
                        t!("ssh.transfer.resume.off")
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
                ui::button(
                    "upload",
                    t!("ssh.transfer.upload_button"),
                    ButtonKind::Subtle,
                    &p,
                )
                .on_click(cx.listener(|this, _, _, cx| this.choose_transfer(true, cx))),
            )
            .child(
                ui::button(
                    "download",
                    t!("ssh.transfer.download_button"),
                    ButtonKind::Subtle,
                    &p,
                )
                .on_click(cx.listener(|this, _, _, cx| this.choose_transfer(false, cx))),
            );
        let confirmation = self.pending_transfer.is_some().then(|| {
            ui::button(
                "start-transfer",
                t!("ssh.transfer.confirm"),
                ButtonKind::Primary,
                &p,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(connection) = this.pending_transfer.take() {
                    (this.connect)(connection, cx);
                    this.status = t!("ssh.status.transfer_started").to_string();
                }
                cx.notify();
            }))
        });
        // 高级选项：跳板机、SFTP 远程路径与传输工具默认折叠，保持基本表单紧凑。
        let advanced = div()
            .flex()
            .flex_col()
            .gap(px(space::SM))
            .child(
                ui::button(
                    "advanced-toggle",
                    if self.advanced_open {
                        t!("ssh.advanced.expanded")
                    } else {
                        t!("ssh.advanced.collapsed")
                    },
                    ButtonKind::Subtle,
                    &p,
                )
                .debug_selector(|| "ssh-advanced-toggle".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.advanced_open = !this.advanced_open;
                    cx.notify();
                })),
            )
            .when(self.advanced_open, |section| {
                section
                    .child(field_rows(&[&[5], &[6]], &mut cells))
                    .child(transfers)
                    .children(confirmation)
                    .child(
                        div()
                            .text_size(px(font_size::MICRO))
                            .text_color(ui::muted(&p))
                            .child(t!("ssh.sftp_hint")),
                    )
            });
        // 备注收尾；状态行与动作按钮固定在表单最下方。
        form = form
            .child(advanced)
            .child(field_rows(&[&[11]], &mut cells))
            .when(!self.status.is_empty(), |element| {
                element.child(
                    div()
                        .text_size(px(font_size::BODY))
                        .child(SharedString::from(self.status.clone())),
                )
            });
        let mut actions = div().flex().flex_wrap().gap(px(space::XS)).child(
            ui::button("save", t!("action.save"), ButtonKind::Subtle, &p)
                .debug_selector(|| "ssh-save".into())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.status = this
                        .save()
                        .map(|_| t!("ssh.status.saved").to_string())
                        .unwrap_or_else(|e| e);
                    cx.emit(ProfilesChanged);
                    cx.notify();
                })),
        );
        for (i, (kind, label)) in [
            (SessionKind::Shell, t!("ssh.connect_ssh")),
            (SessionKind::Sftp, t!("ssh.open_sftp")),
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
                                this.status = t!("ssh.status.connected").to_string();
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
                    t!("ssh.delete_confirm")
                } else {
                    t!("ssh.delete")
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
                                this.status = t!("ssh.status.deleted").to_string();
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
        form = form.child(actions);
        let scrollbar = termior_ui_kit::scrollbar("ssh-scrollbar", &self.scroll, &p)
            .debug_selector(|| "ssh-scrollbar".into())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.dragging_scroll = true;
                    termior_ui_kit::scroll_to_pointer(&this.scroll, event.position.y);
                    cx.notify();
                }),
            );
        div()
            .id("ssh-manager")
            .role(Role::Group)
            .aria_label(t!("ssh.manager.aria"))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                if this.dragging_scroll && e.pressed_button == Some(MouseButton::Left) {
                    termior_ui_kit::scroll_to_pointer(&this.scroll, e.position.y);
                    cx.notify();
                } else {
                    this.dragging_scroll = false;
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging_scroll = false),
            )
            .size_full()
            .p(px(space::LG))
            .bg(ui::color(p.background))
            .text_color(ui::color(p.foreground))
            .flex()
            .flex_col()
            .gap(px(space::MD))
            .child(
                div()
                    .text_size(px(font_size::HEADING))
                    .flex_shrink_0()
                    .child(t!("ssh.manager.title")),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(space::XS))
                    .child(
                        div()
                            .id("ssh-scroll")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(space::LG))
                                    .when(narrow, |d| d.flex_col())
                                    .child(list.flex_shrink_0())
                                    .child(form.when(narrow, |d| d.w_full())),
                            ),
                    )
                    .child(scrollbar),
            )
            .into_any_element()
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
        if matches!(self.field, 7 | 8) {
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
        let caret = if matches!(self.field, 7 | 8) {
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
                layout.bounds.top()
                    + (layout.bounds.size.height - px(INPUT_LINE_HEIGHT)) / 2.
                    + px((INPUT_LINE_HEIGHT - INPUT_CARET_HEIGHT) / 2.),
            ),
            gpui::size(px(2.), px(INPUT_CARET_HEIGHT)),
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
    fn shared_prompt_offers_remember_only_for_identified_credentials() {
        use termior_ssh::auth::{Answer, Challenge, ChallengeKind};
        for kind in [
            ChallengeKind::Password,
            ChallengeKind::Other,
            ChallengeKind::Confirm,
        ] {
            let (reply, answers) = std::sync::mpsc::sync_channel(1);
            let prompt = Challenge {
                profile: Profile {
                    name: "fixture".into(),
                    host: "host.example".into(),
                    ..Default::default()
                },
                prompt: "Authentication".into(),
                kind,
                remember: false,
                error: None,
                reply,
                cancelled: Default::default(),
            };
            let mut cx = TestAppContext::single();
            let (view, vcx) =
                cx.add_window_view(|_, cx| SshView::new_shared_prompt(prompt, None, cx));
            vcx.simulate_resize(gpui::size(px(620.), px(440.)));
            vcx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            assert_eq!(
                vcx.debug_bounds("remember-auth").is_some(),
                kind == ChallengeKind::Password
            );
            if kind == ChallengeKind::Password {
                let button = vcx.debug_bounds("remember-auth").unwrap();
                vcx.simulate_click(button.center(), gpui::Modifiers::default());
                view.update(vcx, |v, _| assert!(v.remember));
                vcx.simulate_click(button.center(), gpui::Modifiers::default());
            }
            view.update(vcx, |v, _| v.values[7] = "test-secret".into());
            let submit = vcx.debug_bounds("submit").unwrap();
            vcx.simulate_click(submit.center(), gpui::Modifiers::default());
            match answers
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
            {
                Answer::Secret { value, remember } => {
                    assert_eq!(&*value, "test-secret");
                    assert!(!remember);
                }
                Answer::Confirm(true) => assert_eq!(kind, ChallengeKind::Confirm),
                _ => panic!("unexpected authentication answer"),
            }
        }
    }

    #[test]
    #[ignore = "writes an isolated test entry to the system credential vault"]
    fn first_prompt_remember_persists_only_opt_in_and_can_be_cleared() {
        use termior_ssh::credentials::{self, Kind};
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            name: "prompt-test".into(),
            host: "fixture.invalid".into(),
            known_hosts_file: dir.path().join("trust").display().to_string(),
            ..Default::default()
        };
        let mut profiles = Profiles::default();
        profiles.connections.push(profile.clone());
        profiles.save(dir.path()).unwrap();
        struct Cleanup(Profile);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = credentials::delete_all(&self.0);
            }
        }
        let _cleanup = Cleanup(profile.clone());
        save_prompt_preference(
            Some(dir.path()),
            &profile,
            Kind::Password,
            "unique-fixture-secret",
            true,
        )
        .unwrap();
        assert!(Profiles::load(dir.path()).unwrap().connections[0].use_saved_credentials);
        assert_eq!(
            &**credentials::load(&profile, Kind::Password)
                .unwrap()
                .as_ref()
                .unwrap(),
            "unique-fixture-secret"
        );
        let json = std::fs::read_to_string(dir.path().join("Termior-ssh.json")).unwrap();
        assert!(!json.contains("unique-fixture-secret"));
        save_prompt_preference(
            Some(dir.path()),
            &profile,
            Kind::Password,
            "session-only",
            false,
        )
        .unwrap();
        assert!(!Profiles::load(dir.path()).unwrap().connections[0].use_saved_credentials);
        assert!(credentials::load(&profile, Kind::Password)
            .unwrap()
            .is_none());
    }

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
        for (width, height) in [(480., 320.), (860., 380.)] {
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
                // REGULAR 输入框去掉边框后的完整内容高度。
                assert!(
                    last.bounds.size.height >= px(termior_ui_kit::tokens::height::REGULAR - 2.)
                );
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
    #[test]
    fn sidebar_delete_refresh_keeps_other_edits_and_never_restores_deleted_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let first = Profile {
            name: "first".into(),
            host: "first.test".into(),
            ..Profile::default()
        };
        let second = Profile {
            name: "second".into(),
            host: "second.test".into(),
            ..Profile::default()
        };
        Profiles {
            connections: vec![first, second.clone()],
            ..Profiles::default()
        }
        .save(dir.path())
        .unwrap();
        let mut cx = TestAppContext::single();
        let view =
            cx.new(|cx| SshView::new(Some(dir.path().to_path_buf()), Box::new(|_, _| {}), cx));
        view.update(&mut cx, |view, cx| {
            view.edit_saved("second", true, cx);
            assert_eq!(view.selected, Some(1));
            assert_eq!(view.field, 0);
            assert!(view.select_all);
            view.values[0] = "unsaved rename".into();
            Profiles {
                connections: vec![second],
                ..Profiles::default()
            }
            .save(dir.path())
            .unwrap();
            view.refresh_saved_profiles(cx);
            assert_eq!(view.selected, Some(0));
            assert_eq!(view.values[0], "unsaved rename");
            view.save().unwrap();
            let saved = Profiles::load(dir.path()).unwrap();
            assert_eq!(saved.connections.len(), 1);
            assert_eq!(saved.connections[0].name, "unsaved rename");
            Profiles::default().save(dir.path()).unwrap();
            view.refresh_saved_profiles(cx);
            assert!(view.selected.is_none());
            assert!(view.values.iter().all(String::is_empty));
        });
    }
    #[test]
    fn group_field_roundtrips_and_upserts_new_groups() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let view =
            cx.new(|cx| SshView::new(Some(dir.path().to_path_buf()), Box::new(|_, _| {}), cx));
        view.update(&mut cx, |view, _| {
            view.values[0] = "server".into();
            view.values[1] = "example.test".into();
            view.values[9] = "  Prod ".into();
            view.save().unwrap();
            let saved = Profiles::load(dir.path()).unwrap();
            assert_eq!(saved.groups, vec!["Prod".to_owned()]);
            assert_eq!(saved.connections[0].group, "Prod");
            view.select(0);
            assert_eq!(view.values[9], "Prod");
            // 清空分组字段保存 → 归入未分组；分组本身保留。
            view.values[9].clear();
            view.save().unwrap();
            let saved = Profiles::load(dir.path()).unwrap();
            assert_eq!(saved.connections[0].group, "");
            assert_eq!(saved.groups, vec!["Prod".to_owned()], "空分组持久保留");
        });
    }
    #[test]
    fn tags_notes_and_remote_path_roundtrip_through_the_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut cx = TestAppContext::single();
        let view =
            cx.new(|cx| SshView::new(Some(dir.path().to_path_buf()), Box::new(|_, _| {}), cx));
        view.update(&mut cx, |view, _| {
            view.values[0] = "server".into();
            view.values[1] = "example.test".into();
            view.values[6] = "/srv/data".into();
            view.values[10] = " prod， 数据库 , ".into();
            view.values[11] = " 临时备注 ".into();
            view.save().unwrap();
            let saved = Profiles::load(dir.path()).unwrap();
            assert_eq!(
                saved.connections[0].tags,
                vec!["prod".to_owned(), "数据库".to_owned()]
            );
            assert_eq!(saved.connections[0].notes, "临时备注");
            assert_eq!(saved.connections[0].sftp_remote_path, "/srv/data");
            view.select(0);
            assert_eq!(view.values[10], "prod, 数据库");
            assert_eq!(view.values[11], "临时备注");
            assert_eq!(view.values[6], "/srv/data");
        });
    }
    #[test]
    fn advanced_section_hides_jump_host_and_remote_path_until_opened() {
        let mut cx = TestAppContext::single();
        let (view, vcx) = cx.add_window_view(|_, cx| SshView::new(None, Box::new(|_, _| {}), cx));
        vcx.simulate_resize(gpui::size(px(860.), px(620.)));
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        view.update(vcx, |v, _| {
            assert!(!v.advanced_open);
            assert!(v.input_layouts.borrow()[5].is_none(), "跳板机默认折叠");
            assert!(
                v.input_layouts.borrow()[6].is_none(),
                "SFTP 远程路径默认折叠"
            );
        });
        let toggle = vcx
            .debug_bounds("ssh-advanced-toggle")
            .expect("高级选项开关");
        vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        view.update(vcx, |v, _| {
            assert!(v.advanced_open);
            assert!(v.input_layouts.borrow()[5].is_some());
            assert!(v.input_layouts.borrow()[6].is_some());
        });
    }
}
