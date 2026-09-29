//! 侧栏、Composer 面板的显隐与尺寸拖拽，以及标题栏移动。

use super::*;

impl WorkspaceView {
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.model.sidebar_visible = !self.model.sidebar_visible;
        cx.notify();
    }
    /// 收起 / 展开底部 Agent（Composer）栏。由状态栏图标按钮与 `ToggleComposer`
    /// 快捷键共用；收起时把焦点交还工作区，避免把键盘焦点留在已隐藏的输入框里。
    pub(super) fn set_composer_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model.composer_visible = visible;
        self.persist_workspace();
        if !visible {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }
    pub(super) fn terminal_area_hidden(&self) -> bool {
        if self.active_is_sftp_browser() {
            return false;
        }
        self.model
            .active_tab()
            .is_some_and(|tab| tab.kind == TabKind::Terminal && tab.terminal_hidden)
    }
    pub(super) fn toggle_terminal_area(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_is_sftp_browser() {
            return;
        }
        let Some(tab) = self.model.active_tab_mut() else {
            return;
        };
        if tab.kind != TabKind::Terminal {
            return;
        }
        tab.terminal_hidden = !tab.terminal_hidden;
        self.pane_context_menu = None;
        self.pane_resizing = None;
        if self.terminal_area_hidden() {
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_active_pane(window, cx);
        }
        self.persist_workspace();
        cx.notify();
    }
    /// 底部/右侧停靠切换（Composer 面板内按钮触发）。切换后同步面板渲染并立即持久化。
    pub(super) fn toggle_composer_dock(&mut self, cx: &mut Context<Self>) {
        self.model.composer_dock = self.model.composer_dock.toggled();
        self.sync_composer_layout(cx);
        self.persist_workspace();
    }
    /// 把停靠位置与底部停靠高度同步进 Composer 自身，驱动其根容器尺寸。
    fn sync_composer_layout(&mut self, cx: &mut Context<Self>) {
        let (dock, height) = (self.model.composer_dock, self.model.composer_height);
        self.composer.update(cx, |composer, cx| {
            composer.set_layout(dock, height, cx);
        });
        cx.notify();
    }
    pub(super) fn start_composer_resize(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer_resizing = true;
        self.explorer_context_menu = None;
        // 双击手柄恢复默认尺寸，与 sidebar 手柄行为一致。
        if event.click_count >= 2 {
            match self.model.composer_dock {
                ComposerDock::Bottom => self.model.composer_height = DEFAULT_COMPOSER_HEIGHT,
                ComposerDock::Right => self.model.composer_dock_width = DEFAULT_COMPOSER_DOCK_WIDTH,
            }
            self.sync_composer_layout(cx);
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn start_sidebar_resize(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_resizing = true;
        self.explorer_context_menu = None;
        if event.click_count >= 2 {
            self.model.sidebar_width = DEFAULT_SIDEBAR_WIDTH;
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn resize_panels(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_resizing {
            self.model.sidebar_width =
                f32::from(event.position.x).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
            cx.notify();
        }
        if self.composer_resizing {
            let viewport = window.viewport_size();
            match self.model.composer_dock {
                ComposerDock::Bottom => {
                    // 光标落在手柄顶缘：面板总占用 = 手柄 + composer_height，
                    // 两者都要计入，否则面板底边会超出光标一个手柄厚度。
                    let height = f32::from(viewport.height)
                        - STATUS_BAR_HEIGHT
                        - COMPOSER_RESIZE_HANDLE_SIZE
                        - f32::from(event.position.y);
                    self.model.composer_height =
                        height.clamp(MIN_COMPOSER_HEIGHT, MAX_COMPOSER_HEIGHT);
                }
                ComposerDock::Right => {
                    let width = f32::from(viewport.width)
                        - COMPOSER_RESIZE_HANDLE_SIZE
                        - f32::from(event.position.x);
                    self.model.composer_dock_width =
                        width.clamp(MIN_COMPOSER_DOCK_WIDTH, MAX_COMPOSER_DOCK_WIDTH);
                }
            }
            self.sync_composer_layout(cx);
        }
        if let Some(resize) = self.pane_resizing.clone() {
            let delta = match resize.direction {
                SplitDirection::Right => f32::from(event.position.x - resize.start_position.x),
                SplitDirection::Down => f32::from(event.position.y - resize.start_position.y),
            };
            let ratio = (resize.start_ratio + delta / resize.extent).clamp(0.1, 0.9);
            if let Some(tab) = self.model.active_tab_mut() {
                let _ = tab.layout.resize_split(&resize.path, ratio);
                tab.state_generation = tab.state_generation.saturating_add(1);
            }
            cx.notify();
        }
    }
    pub(super) fn start_pane_resize(
        &mut self,
        path: Vec<usize>,
        direction: SplitDirection,
        ratio: f32,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let viewport = window.viewport_size();
        let extent = match direction {
            SplitDirection::Right => f32::from(viewport.width),
            SplitDirection::Down => f32::from(viewport.height),
        }
        .max(1.0);
        self.pane_resizing = Some(PaneResizeState {
            path,
            direction,
            start_position: event.position,
            start_ratio: ratio,
            extent,
        });
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn stop_panel_resizes(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_resizing || self.composer_resizing || self.pane_resizing.is_some() {
            self.sidebar_resizing = false;
            self.composer_resizing = false;
            self.pane_resizing = None;
            self.persist_workspace();
            cx.notify();
        }
    }
    /// Linux/BSD 标题栏拖拽的第一步：按下时 armed，真正的窗口移动在
    /// [`Self::move_titlebar`] 的首个移动事件里交给合成器
    /// （`start_window_move`），避免单纯点击也触发移动；双击按桌面约定
    /// 最大化/还原（Windows 上由 HTCAPTION 原生处理，不进这里）。
    pub(super) fn start_titlebar_move(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            self.titlebar_move_armed = false;
            window.zoom_window();
        } else {
            self.titlebar_move_armed = true;
        }
    }
    /// [`Self::start_titlebar_move`] 的 armed 状态只在左键确实按住时生效：
    /// 若按下后指针移出窗口再松开，armed 会残留到指针回来，`pressed_button`
    /// 校验能拦住这种无按键的假拖拽。
    pub(super) fn move_titlebar(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if self.titlebar_move_armed && event.pressed_button == Some(MouseButton::Left) {
            self.titlebar_move_armed = false;
            window.start_window_move();
        }
    }
    pub(super) fn stop_titlebar_move(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.titlebar_move_armed = false;
    }
}
