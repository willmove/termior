//! SSH 端口转发（隧道）运行时：连接前预检本地端口，连接后观测每条隧道的状态，
//! 并在状态栏以徽标 + 弹层展示（FR-SSH-10）。
//!
//! 观测不触碰隧道本身：本地监听用 bind 探测（connect 会让 OpenSSH 向远端开通道），
//! 远端监听无法在本机探测，只能依据 OpenSSH 在会话输出里的失败诊断。
//! 同一配置的转发只由一个存活 SSH 会话承载，分栏/重复标签不重复请求，避免端口冲突。
use super::*;
use gpui::{Anchor, ClipboardItem};
use termior_ssh::forward::{self, Forward, ForwardKind, Notice, Probe};

/// 只在有隧道处于"连接中"或刚请求远端监听时轮询；空闲时不唤醒、不重绘。
const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// 远端监听失败诊断通常紧随认证；超过该时长不再为它保持轮询。
const REMOTE_NOTICE_WINDOW: Duration = Duration::from_secs(120);
/// 同会话已有本地监听成功后，仍空闲的监听再观察这么多轮才判失败（OpenSSH 依次绑定）。
const SETTLE_TICKS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TunnelState {
    /// 已请求、等待 OpenSSH 完成认证并开始监听。
    Connecting,
    /// 本地监听已建立。
    Active,
    /// 远端监听已请求；本机无法探测，失败时由 OpenSSH 诊断改为 Failed。
    Requested,
    Failed(String),
    /// 承载会话已结束。
    Closed,
}

pub(super) struct TunnelEntry {
    pub(super) forward: Forward,
    pub(super) state: TunnelState,
    idle_ticks: u8,
}

pub(super) struct TunnelSession {
    /// 承载这些转发的 SSH 进程所在的 pane。
    pub(super) owner: (TabId, PaneId),
    pub(super) profile_name: String,
    pub(super) host: String,
    pub(super) entries: Vec<TunnelEntry>,
    started: Instant,
}

/// 连接前预检：本地端口已被占用/不可监听的转发不交给 OpenSSH（避免终端刷警告），
/// 直接标为失败并给出原因；其余照常请求。运行在后台线程（可能解析绑定地址）。
pub(super) fn preflight(forwards: &[Forward]) -> (Vec<Forward>, Vec<TunnelEntry>) {
    let mut requested = Vec::new();
    let entries = forwards
        .iter()
        .map(|forward| {
            let state = if !forward.kind.listens_locally() {
                TunnelState::Requested
            } else {
                match forward::probe_local(forward) {
                    Probe::Free => TunnelState::Connecting,
                    Probe::InUse => TunnelState::Failed(t!("tunnel.reason.in_use").to_string()),
                    Probe::Denied => TunnelState::Failed(t!("tunnel.reason.denied").to_string()),
                    Probe::Unavailable(error) => TunnelState::Failed(
                        tf!("tunnel.reason.unavailable", "error" => error).to_string(),
                    ),
                }
            };
            if !matches!(state, TunnelState::Failed(_)) {
                requested.push(forward.clone());
            }
            TunnelEntry {
                forward: forward.clone(),
                state,
                idle_ticks: 0,
            }
        })
        .collect();
    (requested, entries)
}

/// 状态栏汇总：决定徽标颜色与文案。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct TunnelSummary {
    pub(super) total: usize,
    pub(super) up: usize,
    pub(super) connecting: usize,
    pub(super) failed: usize,
    pub(super) closed: usize,
}

impl TunnelSession {
    pub(super) fn summary(&self) -> TunnelSummary {
        let mut summary = TunnelSummary {
            total: self.entries.len(),
            ..TunnelSummary::default()
        };
        for entry in &self.entries {
            match entry.state {
                TunnelState::Active | TunnelState::Requested => summary.up += 1,
                TunnelState::Connecting => summary.connecting += 1,
                TunnelState::Failed(_) => summary.failed += 1,
                TunnelState::Closed => summary.closed += 1,
            }
        }
        summary
    }

    fn apply_notice(&mut self, notice: Notice) -> bool {
        let (locally, port, reason) = match notice {
            Notice::LocalListenFailed(port) => (true, port, t!("tunnel.reason.ssh_local")),
            Notice::RemoteListenFailed(port) => (false, port, t!("tunnel.reason.ssh_remote")),
        };
        let mut changed = false;
        for entry in &mut self.entries {
            if entry.forward.kind.listens_locally() == locally
                && entry.forward.listen_port == port
                && matches!(
                    entry.state,
                    TunnelState::Connecting | TunnelState::Active | TunnelState::Requested
                )
            {
                entry.state = TunnelState::Failed(reason.to_string());
                changed = true;
            }
        }
        changed
    }

    fn close(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            if !matches!(entry.state, TunnelState::Failed(_) | TunnelState::Closed) {
                entry.state = TunnelState::Closed;
                changed = true;
            }
        }
        changed
    }

    fn needs_polling(&self) -> bool {
        self.entries.iter().any(|entry| match entry.state {
            TunnelState::Connecting => true,
            TunnelState::Requested => self.started.elapsed() < REMOTE_NOTICE_WINDOW,
            _ => false,
        })
    }
}

/// 承载 pane 的存活情况：`None` 表示标签已关闭。
enum OwnerStatus {
    Gone,
    Exited,
    Alive(Vec<Notice>),
}

impl WorkspaceView {
    /// spawn 前调用：决定这次 SSH 会话是否承载该配置的转发。
    /// 返回需要预检的转发；不承载时清空传给 OpenSSH 的转发列表。
    pub(super) fn plan_tunnels(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        remote: Option<&mut termior_ssh::Connection>,
        cx: &App,
    ) -> Option<Vec<Forward>> {
        let remote = remote?;
        if remote.kind != termior_ssh::SessionKind::Shell
            || remote.transfer.is_some()
            || remote.profile.forwards.is_empty()
        {
            remote.profile.forwards.clear();
            return None;
        }
        // 分栏或重复标签已有存活会话承载同一配置时，本会话不再请求，避免端口冲突。
        let carried_elsewhere = self.tunnels.values().any(|session| {
            session.profile_name == remote.profile.name
                && session.owner != (tab_id, pane_id)
                && matches!(
                    self.tunnel_owner_status(session.owner, cx),
                    OwnerStatus::Alive(_)
                )
        });
        if carried_elsewhere {
            remote.profile.forwards.clear();
            return None;
        }
        Some(std::mem::take(&mut remote.profile.forwards))
    }

    pub(super) fn register_tunnels(
        &mut self,
        tab_id: TabId,
        pane_id: PaneId,
        entries: Vec<TunnelEntry>,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .model
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.remote.as_ref())
            .map(|remote| remote.profile.clone())
        else {
            return;
        };
        self.tunnels.insert(
            tab_id,
            TunnelSession {
                owner: (tab_id, pane_id),
                profile_name: profile.name,
                host: profile.host,
                entries,
                started: Instant::now(),
            },
        );
        self.ensure_tunnel_poll(cx);
        cx.notify();
    }

    fn tunnel_owner_status(&self, owner: (TabId, PaneId), cx: &App) -> OwnerStatus {
        if self.pending_terminals.contains(&owner) {
            return OwnerStatus::Alive(Vec::new());
        }
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == owner.0) else {
            return OwnerStatus::Gone;
        };
        match tab.panes.get(&owner.1) {
            Some(PaneContent::Terminal(terminal)) => {
                let terminal = terminal.read(cx);
                if terminal.has_exited() {
                    OwnerStatus::Exited
                } else {
                    OwnerStatus::Alive(terminal.forward_notices().to_vec())
                }
            }
            Some(_) => OwnerStatus::Exited,
            None => OwnerStatus::Gone,
        }
    }

    /// 同步刷新：处理会话退出、标签关闭与 OpenSSH 诊断。返回需要 bind 探测的条目。
    pub(super) fn refresh_tunnels(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Vec<(TabId, usize, Forward)> {
        let mut jobs = Vec::new();
        let mut changed = false;
        let ids: Vec<TabId> = self.tunnels.keys().copied().collect();
        for id in ids {
            let Some(owner) = self.tunnels.get(&id).map(|session| session.owner) else {
                continue;
            };
            let status = self.tunnel_owner_status(owner, cx);
            let Some(session) = self.tunnels.get_mut(&id) else {
                continue;
            };
            match status {
                OwnerStatus::Gone => {
                    self.tunnels.remove(&id);
                    if self.tunnel_popover.is_some_and(|(tab, _)| tab == id) {
                        self.tunnel_popover = None;
                    }
                    changed = true;
                }
                OwnerStatus::Exited => changed |= session.close(),
                OwnerStatus::Alive(notices) => {
                    for notice in notices {
                        changed |= session.apply_notice(notice);
                    }
                    for (index, entry) in session.entries.iter().enumerate() {
                        if entry.state == TunnelState::Connecting {
                            jobs.push((id, index, entry.forward.clone()));
                        }
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
        jobs
    }

    fn apply_tunnel_probes(
        &mut self,
        results: Vec<(TabId, usize, Forward, Probe)>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        for (id, index, forward, probe) in results {
            let Some(session) = self.tunnels.get_mut(&id) else {
                continue;
            };
            let connected = session
                .entries
                .iter()
                .any(|entry| entry.state == TunnelState::Active);
            let Some(entry) = session
                .entries
                .get_mut(index)
                .filter(|entry| entry.forward == forward && entry.state == TunnelState::Connecting)
            else {
                continue;
            };
            match probe {
                Probe::InUse => {
                    entry.state = TunnelState::Active;
                    changed = true;
                }
                Probe::Free if connected => {
                    entry.idle_ticks = entry.idle_ticks.saturating_add(1);
                    if entry.idle_ticks >= SETTLE_TICKS {
                        entry.state =
                            TunnelState::Failed(t!("tunnel.reason.not_listening").to_string());
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn ensure_tunnel_poll(&mut self, cx: &mut Context<Self>) {
        if self.tunnel_polling {
            return;
        }
        self.tunnel_polling = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL_INTERVAL).await;
            let Ok(Some(jobs)) = this.update(cx, |workspace, cx| {
                let jobs = workspace.refresh_tunnels(cx);
                if workspace.tunnels.values().any(TunnelSession::needs_polling) {
                    Some(jobs)
                } else {
                    workspace.tunnel_polling = false;
                    None
                }
            }) else {
                break;
            };
            if jobs.is_empty() {
                continue;
            }
            let results = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .map(|(id, index, forward)| {
                            let probe = forward::probe_local(&forward);
                            (id, index, forward, probe)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            if this
                .update(cx, |workspace, cx| {
                    workspace.apply_tunnel_probes(results, cx)
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    /// 活动标签对应的隧道会话（键、会话、是否由其他标签承载）。
    pub(super) fn active_tunnel_session(&self, cx: &App) -> Option<(TabId, &TunnelSession, bool)> {
        let tab = self.model.active_tab()?;
        let remote = tab.remote.as_ref()?;
        if remote.kind != termior_ssh::SessionKind::Shell || remote.transfer.is_some() {
            return None;
        }
        let carrier = self.tunnels.iter().find(|(id, session)| {
            **id != tab.id
                && session.profile_name == remote.profile.name
                && matches!(
                    self.tunnel_owner_status(session.owner, cx),
                    OwnerStatus::Alive(_)
                )
        });
        match self.tunnels.get(&tab.id) {
            // 本标签会话已退出、但其他标签仍承载同一配置时，展示承载方。
            Some(session)
                if carrier.is_none()
                    || matches!(
                        self.tunnel_owner_status(session.owner, cx),
                        OwnerStatus::Alive(_)
                    ) =>
            {
                Some((tab.id, session, false))
            }
            _ => carrier.map(|(id, session)| (*id, session, true)),
        }
    }

    fn tab_title(&self, id: TabId) -> String {
        self.model
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .map(|tab| tab.title.clone())
            .unwrap_or_default()
    }

    /// 状态栏徽标：仅在活动标签是带转发的 SSH 会话时出现。
    pub(super) fn render_tunnel_chip(
        &self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (session_tab, session, shared) = self.active_tunnel_session(cx)?;
        let summary = session.summary();
        let (tone, state) = summary_tone(p, &summary);
        let label = if summary.total == 1 {
            let entry = &session.entries[0];
            match entry.forward.target_endpoint() {
                Some(target) => format!("{} → {}", entry.forward.listen_endpoint(), target),
                None => tf!("tunnel.chip.socks", "listen" => entry.forward.listen_endpoint())
                    .to_string(),
            }
        } else {
            tf!("tunnel.chip.count", "up" => summary.up, "total" => summary.total).to_string()
        };
        let tooltip = if shared {
            tf!("tunnel.chip.tooltip_shared", "state" => state, "tab" => self.tab_title(session.owner.0))
                .to_string()
        } else {
            tf!("tunnel.chip.tooltip", "state" => state).to_string()
        };
        let wash = ui::hover_wash(p);
        let open = self.tunnel_popover.is_some();
        let palette = p.clone();
        Some(
            div()
                .id("status-tunnels")
                .debug_selector(|| "status-tunnels".into())
                .role(Role::Button)
                .aria_label(SharedString::from(tooltip.clone()))
                .aria_expanded(open)
                .tooltip(move |_window, cx| Tooltip::view(tooltip.clone(), &palette, cx))
                .flex()
                .items_center()
                .gap(px(tokens::space::XS))
                .h(px(tokens::height::REGULAR))
                .px(px(tokens::space::SM))
                .rounded(px(tokens::radius::MD))
                .cursor_pointer()
                .hover(move |style| style.bg(wash))
                .when(open, |chip| chip.bg(wash))
                .child(ui::icon(Icon::ArrowLeftRight, icon_size::SM, tone))
                .child(div().size(px(6.)).rounded_full().bg(tone))
                .child(
                    div()
                        .max_w(px(280.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(label)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.close_chrome_menus();
                        this.tunnel_popover = if this.tunnel_popover.is_some() {
                            None
                        } else {
                            Some((session_tab, event.position))
                        };
                        this.tunnel_copied = None;
                        // 打开弹层时立即刷新一次，不等下一轮轮询。
                        let _ = this.refresh_tunnels(cx);
                        cx.notify();
                    }),
                )
                .into_any_element(),
        )
    }

    /// 状态栏弹层：逐条列出映射、状态与快捷动作。
    pub(super) fn render_tunnel_popover(
        &self,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (popover_tab, position) = self.tunnel_popover?;
        let (session_tab, session, shared) = self
            .active_tunnel_session(cx)
            .filter(|(id, _, _)| *id == popover_tab)?;
        let summary = session.summary();
        let (tone, state) = summary_tone(p, &summary);
        let owner_exited = matches!(
            self.tunnel_owner_status(session.owner, cx),
            OwnerStatus::Exited
        );
        let muted = ui::muted(p);
        let mut rows = div().flex().flex_col().gap(px(tokens::space::XS));
        for (index, entry) in session.entries.iter().enumerate() {
            rows = rows.child(self.tunnel_row(session_tab, index, entry, &session.host, p, cx));
        }
        let profile_name = session.profile_name.clone();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(tokens::space::MD))
            .px(px(tokens::space::SM))
            .pt(px(tokens::space::XS))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(tokens::font_size::BODY))
                            .text_color(ui::color(p.foreground))
                            .child(tf!("tunnel.popover.title", "name" => session.profile_name.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font_size::MICRO))
                            .text_color(muted)
                            .child(SharedString::from(if shared {
                                tf!("tunnel.popover.shared", "tab" => self.tab_title(session.owner.0))
                                    .to_string()
                            } else {
                                session.host.clone()
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens::space::XS))
                    .text_size(px(tokens::font_size::MICRO))
                    .text_color(tone)
                    .child(div().size(px(6.)).rounded_full().bg(tone))
                    .child(state),
            );
        let has_problem = summary.failed > 0;
        let footer = div()
            .flex()
            .flex_col()
            .gap(px(tokens::space::XS))
            .px(px(tokens::space::SM))
            .pb(px(tokens::space::XS))
            .when(has_problem && !owner_exited, |footer| {
                footer.child(
                    div()
                        .text_size(px(tokens::font_size::MICRO))
                        .text_color(muted)
                        .child(t!("tunnel.popover.failed_hint")),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(tokens::space::XS))
                    .when(owner_exited && !shared, |actions| {
                        actions.child(
                            ui::button(
                                "tunnel-reconnect",
                                t!("tunnel.popover.reconnect"),
                                ButtonKind::Primary,
                                p,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.tunnel_popover = None;
                                    this.reconnect_remote(window, cx);
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .child(
                        ui::button(
                            "tunnel-edit",
                            t!("tunnel.popover.edit"),
                            ButtonKind::Subtle,
                            p,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.tunnel_popover = None;
                                this.open_ssh_manager(window, cx);
                                if let Some(handle) = this.ssh_manager_window {
                                    let name = profile_name.clone();
                                    let _ = handle.update(cx, |view, _, cx| {
                                        view.edit_saved_forwards(&name, cx)
                                    });
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            );
        Some(
            anchored()
                .anchor(Anchor::BottomLeft)
                .position(position - gpui::point(px(12.), px(14.)))
                .snap_to_window_with_margin(px(8.))
                .child(
                    menu_panel(p)
                        .id("tunnel-popover")
                        .debug_selector(|| "tunnel-popover".into())
                        .role(Role::Dialog)
                        .aria_label(
                            tf!("tunnel.popover.title", "name" => session.profile_name.clone()),
                        )
                        .w(px(440.))
                        .max_w(relative(0.9))
                        .flex()
                        .flex_col()
                        .gap(px(tokens::space::SM))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(header)
                        .child(rows)
                        .child(footer)
                        .with_animation(
                            "tunnel-popover-fade-in",
                            Animation::new(UI_FADE_IN).with_easing(ease_in_out),
                            |style, delta| style.opacity(delta),
                        ),
                )
                .into_any_element(),
        )
    }

    fn tunnel_row(
        &self,
        session_tab: TabId,
        index: usize,
        entry: &TunnelEntry,
        host: &str,
        p: &ResolvedPalette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = ui::muted(p);
        let forward = &entry.forward;
        let (kind_label, detail) = match forward.kind {
            ForwardKind::Local => (
                t!("tunnel.kind.local"),
                tf!("tunnel.detail.local", "host" => host),
            ),
            ForwardKind::Remote => (
                t!("tunnel.kind.remote"),
                tf!("tunnel.detail.remote", "host" => host),
            ),
            ForwardKind::Dynamic => (
                t!("tunnel.kind.socks"),
                tf!("tunnel.detail.socks", "host" => host),
            ),
        };
        let mapping = match forward.target_endpoint() {
            Some(target) => format!("{}  →  {}", forward.listen_endpoint(), target),
            None => forward.listen_endpoint(),
        };
        let (tone, state_label) = entry_tone(p, &entry.state);
        let reason = match &entry.state {
            TunnelState::Failed(reason) => Some(reason.clone()),
            _ => None,
        };
        let exposed = forward.exposed_beyond_loopback();
        let usable = entry.state == TunnelState::Active;
        let client = forward.local_client_address();
        let copied = self.tunnel_copied == Some((session_tab, index));
        let badge_bg = ui::alpha(p.accent, 0.14);
        div()
            .id(("tunnel-row", index))
            .flex()
            .items_center()
            .gap(px(tokens::space::SM))
            .px(px(tokens::space::SM))
            .py(px(tokens::space::XS))
            .rounded(px(tokens::radius::SM))
            .bg(ui::alpha(p.foreground, 0.03))
            .child(
                div()
                    .flex_none()
                    .w(px(48.))
                    .py(px(2.))
                    .rounded(px(tokens::radius::SM))
                    .bg(badge_bg)
                    .text_color(ui::color(p.accent))
                    .text_size(px(tokens::font_size::MICRO))
                    .flex()
                    .justify_center()
                    .child(kind_label),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(tokens::font_size::BODY))
                            .text_color(ui::color(p.foreground))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(mapping)),
                    )
                    .child(
                        div()
                            .text_size(px(tokens::font_size::MICRO))
                            .text_color(muted)
                            .child(SharedString::from(match &reason {
                                Some(reason) => reason.clone(),
                                None => detail.to_string(),
                            })),
                    )
                    .when(exposed, |column| {
                        column.child(
                            div()
                                .text_size(px(tokens::font_size::MICRO))
                                .text_color(ui::color(p.status[2]))
                                .child(t!("tunnel.exposed")),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(tokens::space::XS))
                    .text_size(px(tokens::font_size::MICRO))
                    .text_color(tone)
                    .child(div().size(px(6.)).rounded_full().bg(tone))
                    .child(state_label),
            )
            .when_some(client.filter(|_| usable), |row, address| {
                let open_url = format!("http://{address}");
                let copy_address = address.clone();
                row.child(
                    ui::button(
                        ("tunnel-copy", index),
                        if copied {
                            t!("tunnel.copied")
                        } else {
                            t!("tunnel.copy")
                        },
                        ButtonKind::Ghost,
                        p,
                    )
                    .flex_none()
                    .px(px(tokens::space::SM))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_address.clone()));
                            this.tunnel_copied = Some((session_tab, index));
                            cx.notify();
                        }),
                    ),
                )
                .when(forward.kind == ForwardKind::Local, |row| {
                    row.child(
                        ui::button(
                            ("tunnel-open", index),
                            t!("tunnel.open"),
                            ButtonKind::Ghost,
                            p,
                        )
                        .flex_none()
                        .px(px(tokens::space::SM))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.tunnel_popover = None;
                                this.create_preview_url(open_url.clone(), window, cx);
                            }),
                        ),
                    )
                })
            })
    }
}

fn entry_tone(p: &ResolvedPalette, state: &TunnelState) -> (gpui::Rgba, SharedString) {
    match state {
        TunnelState::Connecting => (ui::color(p.status[0]), t!("tunnel.state.connecting")),
        TunnelState::Active => (ui::color(p.status[1]), t!("tunnel.state.active")),
        TunnelState::Requested => (ui::color(p.status[1]), t!("tunnel.state.requested")),
        TunnelState::Failed(_) => (ui::color(p.status[3]), t!("tunnel.state.failed")),
        TunnelState::Closed => (ui::muted(p), t!("tunnel.state.closed")),
    }
}

fn summary_tone(p: &ResolvedPalette, summary: &TunnelSummary) -> (gpui::Rgba, SharedString) {
    if summary.closed + summary.failed == summary.total && summary.closed > 0 {
        (ui::muted(p), t!("tunnel.state.closed"))
    } else if summary.failed > 0 {
        (
            ui::color(p.status[3]),
            tf!("tunnel.summary.failed", "count" => summary.failed),
        )
    } else if summary.connecting > 0 {
        (ui::color(p.status[0]), t!("tunnel.state.connecting"))
    } else {
        (ui::color(p.status[1]), t!("tunnel.summary.all_up"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(states: &[(Forward, TunnelState)]) -> TunnelSession {
        TunnelSession {
            owner: (TabId(1), PaneId(1)),
            profile_name: "dev".into(),
            host: "server".into(),
            entries: states
                .iter()
                .cloned()
                .map(|(forward, state)| TunnelEntry {
                    forward,
                    state,
                    idle_ticks: 0,
                })
                .collect(),
            started: Instant::now(),
        }
    }

    #[test]
    fn preflight_skips_busy_local_ports_and_keeps_remote_requests() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let busy_port = busy.local_addr().unwrap().port();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let free_port = free.local_addr().unwrap().port();
        drop(free);
        let forwards = vec![
            Forward::local(busy_port, "127.0.0.1", 80),
            Forward::local(free_port, "127.0.0.1", 80),
            forward::parse("R 9000:localhost:3000").unwrap(),
        ];
        let (requested, entries) = preflight(&forwards);
        assert_eq!(requested, forwards[1..].to_vec());
        assert!(matches!(entries[0].state, TunnelState::Failed(_)));
        assert_eq!(entries[1].state, TunnelState::Connecting);
        assert_eq!(entries[2].state, TunnelState::Requested);
    }

    #[test]
    fn notices_fail_matching_side_and_close_keeps_failures() {
        let mut s = session(&[
            (Forward::local(8080, "127.0.0.1", 80), TunnelState::Active),
            (
                forward::parse("R 8080:localhost:80").unwrap(),
                TunnelState::Requested,
            ),
        ]);
        assert!(s.apply_notice(Notice::RemoteListenFailed(8080)));
        assert_eq!(s.entries[0].state, TunnelState::Active);
        assert!(matches!(s.entries[1].state, TunnelState::Failed(_)));
        assert!(
            !s.apply_notice(Notice::RemoteListenFailed(8080)),
            "idempotent"
        );
        assert!(s.close());
        assert_eq!(s.entries[0].state, TunnelState::Closed);
        assert!(matches!(s.entries[1].state, TunnelState::Failed(_)));
        assert_eq!(
            s.summary(),
            TunnelSummary {
                total: 2,
                failed: 1,
                closed: 1,
                ..TunnelSummary::default()
            }
        );
        assert!(!s.needs_polling());
    }

    fn ssh_tab(view: &mut WorkspaceView, profile: &termior_ssh::Profile) -> TabId {
        let id = view.model.new_tab(TabKind::Terminal, "SSH · dev", false);
        view.model.active_tab_mut().unwrap().remote = Some(termior_ssh::Connection {
            profile: profile.clone(),
            kind: termior_ssh::SessionKind::Shell,
            transfer: None,
        });
        view.tabs.push(AppTab {
            id,
            panes: super::super::helpers::single_pane(PaneContent::Placeholder(String::new())),
        });
        // 视作 spawn 中的存活会话，避免测试真正启动 OpenSSH。
        view.pending_terminals.insert((id, PaneId(1)));
        id
    }

    #[gpui::test]
    fn status_chip_opens_popover_and_follows_the_carrying_tab(cx: &mut gpui::TestAppContext) {
        use gpui::Modifiers;
        let root = tempfile::tempdir().unwrap();
        let profile = termior_ssh::Profile {
            name: "dev".into(),
            host: "server".into(),
            forwards: forward::parse_list("L 8080:127.0.0.1:8080, R 9000:localhost:3000").unwrap(),
            ..termior_ssh::Profile::default()
        };
        let (view, cx) = cx.add_window_view(|_, cx| {
            crate::updater::init(cx);
            let mut view = WorkspaceView::new(root.path().to_path_buf(), false, cx);
            view.data_dir = None;
            view.model = WorkspaceState::new(root.path().to_path_buf());
            view.model.composer_visible = false;
            view.tabs.clear();
            view
        });
        cx.simulate_resize(size(px(1200.), px(800.)));
        let first = view.update(cx, |v, cx| {
            let id = ssh_tab(v, &profile);
            let entries = profile
                .forwards
                .iter()
                .cloned()
                .zip([TunnelState::Active, TunnelState::Requested])
                .map(|(forward, state)| TunnelEntry {
                    forward,
                    state,
                    idle_ticks: 0,
                })
                .collect();
            v.tunnels.insert(
                id,
                TunnelSession {
                    owner: (id, PaneId(1)),
                    profile_name: "dev".into(),
                    host: "server".into(),
                    entries,
                    started: Instant::now(),
                },
            );
            assert!(v
                .active_tunnel_session(cx)
                .is_some_and(|(_, _, shared)| !shared));
            id
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let chip = cx
            .debug_bounds("status-tunnels")
            .expect("status bar tunnel chip");
        cx.simulate_mouse_down(chip.center(), MouseButton::Left, Modifiers::default());
        view.update(cx, |v, _| {
            assert_eq!(v.tunnel_popover.map(|(id, _)| id), Some(first))
        });
        cx.update(|window, cx| window.draw(cx).clear());
        assert!(cx.debug_bounds("tunnel-popover").is_some());
        cx.simulate_mouse_down(
            gpui::point(px(600.), px(300.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        view.update(cx, |v, _| {
            assert!(v.tunnel_popover.is_none(), "outside click closes")
        });

        // 同一配置的第二个标签不重复请求转发，状态栏展示承载方。
        view.update(cx, |v, cx| {
            let second = ssh_tab(v, &profile);
            let mut remote = v.model.tab(second).unwrap().remote.clone();
            assert!(v
                .plan_tunnels(second, PaneId(1), remote.as_mut(), cx)
                .is_none());
            assert!(remote.unwrap().profile.forwards.is_empty());
            let (carrier, _, shared) = v.active_tunnel_session(cx).unwrap();
            assert_eq!(carrier, first);
            assert!(shared);
            // 承载标签关闭后，转发状态随之移除。
            v.pending_terminals.remove(&(first, PaneId(1)));
            v.tabs.retain(|tab| tab.id != first);
            let _ = v.refresh_tunnels(cx);
            assert!(v.tunnels.is_empty());
            assert!(v.active_tunnel_session(cx).is_none());
            let mut remote = v.model.tab(second).unwrap().remote.clone();
            assert_eq!(
                v.plan_tunnels(second, PaneId(1), remote.as_mut(), cx)
                    .map(|forwards| forwards.len()),
                Some(2),
                "the next connection takes over"
            );
        });
    }
}
