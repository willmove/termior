//! Agent 状态聚合、系统/应用内通知与 Toast。

use super::helpers::agent_status_message;
use super::*;

impl WorkspaceView {
    pub(super) fn queue_agent_update(
        &mut self,
        id: &str,
        title: &str,
        tab_id: Option<u64>,
        target: NotificationTarget,
        status: AgentStatus,
        cx: &mut Context<Self>,
    ) {
        self.pending_agent_updates.insert(
            id.to_owned(),
            PendingAgentUpdate {
                indicator: AgentIndicator {
                    id: id.to_owned(),
                    title: title.to_owned(),
                    status,
                    tab_id,
                },
                notification: Notification {
                    title: title.to_owned(),
                    body: agent_status_message(status).to_string(),
                    target,
                    status,
                },
            },
        );
        cx.notify();
    }
    pub(super) fn remove_terminal_agent(&mut self, tab_id: TabId, pane_id: PaneId) {
        let id = format!("terminal-agent-{}-{}", tab_id.0, pane_id.0);
        self.pending_agent_updates.remove(&id);
        self.notification_router.remove_agent(&id);
    }
    pub(super) fn process_agent_updates(&mut self, window: &Window, cx: &mut Context<Self>) {
        let updates = std::mem::take(&mut self.pending_agent_updates);
        let context = NotificationContext {
            window_focused: window.is_window_active(),
            active_tab: self.model.active.map(|id| id.0),
            composer_visible: self.model.composer_visible,
        };
        for update in updates.into_values() {
            if !self.notification_router.update_agent(update.indicator) {
                continue;
            }
            match self
                .notification_router
                .route(&update.notification, context)
            {
                NotificationDecision::Suppress => {}
                NotificationDecision::InAppToast => self.enqueue_toast(update.notification, cx),
                NotificationDecision::System => {
                    let notification = update.notification;
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(error) = NativeNotifier.notify(&notification) {
                                log::warn!("system notification failed: {error}");
                            }
                        })
                        .detach();
                }
            }
        }
    }
    pub(super) fn enqueue_toast(&mut self, notification: Notification, cx: &mut Context<Self>) {
        const MAX_TOASTS: usize = 4;
        if self.toasts.len() >= MAX_TOASTS {
            self.toasts.remove(0);
        }
        let id = self.next_toast_id;
        self.next_toast_id = self.next_toast_id.saturating_add(1);
        self.toasts.push(InAppToast { id, notification });
        let timer = cx.background_executor().timer(Duration::from_secs(5));
        cx.spawn(async move |workspace, cx| {
            timer.await;
            let _ = workspace.update(cx, |workspace, cx| {
                workspace.toasts.retain(|toast| toast.id != id);
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn toggle_bell(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.bell_open = !self.bell_open;
        cx.notify();
    }
    pub(super) fn open_agent_surface(&mut self, tab_id: Option<u64>, cx: &mut Context<Self>) {
        let target = tab_id.map_or(NotificationTarget::Composer, NotificationTarget::Tab);
        self.open_notification_target(target, cx);
        self.bell_open = false;
        cx.notify();
    }
    pub(super) fn open_notification_target(
        &mut self,
        target: NotificationTarget,
        cx: &mut Context<Self>,
    ) {
        match target {
            NotificationTarget::Tab(tab_id) => {
                let id = TabId(tab_id);
                if self.model.tabs.iter().any(|tab| tab.id == id) {
                    self.activate_runtime(id, cx);
                }
            }
            NotificationTarget::Composer => self.model.composer_visible = true,
            NotificationTarget::Global => {}
        }
        cx.notify();
    }
}
