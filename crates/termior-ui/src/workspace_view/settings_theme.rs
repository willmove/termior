//! 主题色板应用、设置窗口与行内补全配置。

use super::helpers::map_appearance;
use super::*;

/// 从设置构造行内补全 [`InlineCompleter`]（与聊天 profile 同规则；密钥只读钥匙串，INV-5）。
/// 复用于 `WorkspaceView::new`（此时 `self` 尚未建成）与 `build_completer`。
pub(super) fn build_completer_from_settings(
    settings: &Settings,
) -> Option<std::sync::Arc<InlineCompleter>> {
    if !settings.autocomplete_enabled {
        return None;
    }
    let profile = settings
        .models
        .active_completion_profile
        .as_deref()
        .and_then(|id| {
            settings
                .models
                .profiles
                .iter()
                .find(|profile| profile.id == id)
        })
        .filter(|profile| profile.enabled)?;
    let api_key = KeyringSecretStore::new()
        .get(&format!("provider:{}", profile.id))
        .ok()
        .flatten();
    if !profile.local && api_key.is_none() {
        return None;
    }
    let config = ProviderConfig::from_settings(profile, api_key).ok()?;
    let provider = HttpProvider::new(config).ok()?;
    Some(std::sync::Arc::new(InlineCompleter::new(
        Box::new(provider),
        profile.model.clone(),
    )))
}

impl WorkspaceView {
    /// 复用 FR-PROV 的 Provider 与密钥配置，构造一个行内补全 [`InlineCompleter`]。
    ///
    /// 遵循与 Composer 聊天 profile 相同的规则：profile 必须启用；非本地 profile 必须在
    /// OS 钥匙串中存有 key（密钥永不落盘，INV-5）。配置缺失或校验失败时返回 `None`，
    /// 编辑器侧静默不补全（FR-EDIT-05「请求失败/超时静默降级」）。
    fn build_completer(&self) -> Option<std::sync::Arc<InlineCompleter>> {
        build_completer_from_settings(&self.settings)
    }
    /// 当前补全配置：(completer, enabled)。enabled 为 false 或 completer 不可用时
    /// 返回 `(None, false)`，确保编辑器侧无论如何都不会发起请求。
    pub(super) fn completion_config(&self) -> (Option<std::sync::Arc<InlineCompleter>>, bool) {
        if !self.settings.autocomplete_enabled {
            return (None, false);
        }
        match self.build_completer() {
            Some(completer) => (Some(completer), true),
            None => (None, false),
        }
    }
    fn apply_theme_preferences(&mut self, settings: &Settings, cx: &mut Context<Self>) {
        self.settings.theme_id = settings.theme_id.clone();
        self.settings.light_theme_id = settings.light_theme_id.clone();
        self.settings.dark_theme_id = settings.dark_theme_id.clone();
        self.settings.editor_theme_id = settings.editor_theme_id.clone();
        self.settings.appearance = settings.appearance;
        // 预览期间把设置里尚未入库的自定义主题并入列表。
        if let Some(library) = self.data_dir.as_ref().and_then(|dir| {
            termior_store::DataFiles::new(dir)
                .themes::<ThemeLibrary>()
                .load()
                .ok()
        }) {
            self.themes = library.all();
        }
        self.sync_theme_index();
        self.recompute_palette(cx);
        let (completer, completion_enabled) = self.completion_config();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor.set_preferences(
                            &self.settings.editor_theme_id,
                            self.settings.vim_mode,
                            completer.clone(),
                            completion_enabled,
                        )
                    });
                }
            }
        }
        cx.notify();
    }
    pub(super) fn sync_theme_index(&mut self) {
        let active_id = active_theme_id(
            map_appearance(self.settings.appearance),
            &self.settings.theme_id,
            &self.settings.light_theme_id,
            &self.settings.dark_theme_id,
            self.system_is_dark,
        );
        self.theme_index = self
            .themes
            .iter()
            .position(|theme| theme.id == active_id)
            .unwrap_or(0);
    }
    pub(super) fn recompute_palette(&mut self, cx: &mut Context<Self>) {
        self.palette = resolve_active_palette(
            &self.themes,
            map_appearance(self.settings.appearance),
            &self.settings.theme_id,
            &self.settings.light_theme_id,
            &self.settings.dark_theme_id,
            self.system_is_dark,
        );
        self.propagate_palette(cx);
    }
    /// 主题变更后同步全局色板并推送到所有已打开的终端(终端 16 色随主题走)。
    fn propagate_palette(&mut self, cx: &mut Context<Self>) {
        ui::set_palette(cx, self.palette.clone());
        let palette = self.palette.clone();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Terminal(terminal) = pane {
                    terminal.update(cx, |terminal, cx| terminal.set_palette(palette.clone(), cx));
                }
            }
        }
    }
    pub(super) fn open_settings(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.open_settings_window(cx);
    }
    pub(super) fn open_settings_window(
        &self,
        cx: &mut Context<Self>,
    ) -> gpui::WindowHandle<SettingsView> {
        let settings = self.settings.clone();
        let migration_error = self.migration_error.clone();
        let data_dir = self.data_dir.clone();
        let workspace = cx.entity().downgrade();
        let ssh_workspace = workspace.clone();
        let main_window = self.main_window;
        let preview_workspace = workspace.clone();
        let on_save = Box::new(move |settings: &Settings, app: &mut gpui::App| {
            if let Some(workspace) = workspace.upgrade() {
                workspace.update(app, |workspace, cx| {
                    workspace.apply_settings(settings.clone(), cx);
                    cx.notify();
                });
            }
        });
        let on_theme_preview = Box::new(move |settings: &Settings, app: &mut gpui::App| {
            if let Some(workspace) = preview_workspace.upgrade() {
                workspace.update(app, |workspace, cx| {
                    workspace.apply_theme_preferences(settings, cx);
                });
            }
        });
        let bounds = Bounds::centered(None, size(px(880.0), px(560.0)), cx);
        cx.open_window(
            app_identity::window_options(WindowBounds::Windowed(bounds)),
            |window, cx| {
                // 接管设置窗口的关闭流程。默认情况下点击标题栏关闭按钮会走
                // Win32 `DefWindowProcW(WM_CLOSE)` → `DestroyWindow`,随后 GPUI 在
                // `WindowsWindow::drop` 里又对同一 HWND 调一次 `DestroyWindow`,
                // 在已销毁的句柄上触发 `无效的窗口句柄`。
                //
                // 这里返回 `false` 阻止 Win32 自行销毁窗口,与 Zed 自身
                // (`zed/src/zed.rs`) 关闭子窗口的做法一致:改由 GPUI 的 `remove_window`
                // 走它自己的清理路径——`WindowsWindow::drop` 唯一一次 `DestroyWindow`,
                // 避免对死句柄的二次操作。
                //
                // 必须在回调里**同步**调用 `remove_window`:该回调运行在 GPUI 的
                // `update_window` 内部,返回后 `update_window_id` 的 `trail` 会立即
                // 检查 `window.removed` 并在同一帧移除窗口。
                //
                // 注意:仅靠这一步仍可能打出 `window not found`——`WindowsWindow::drop`
                // 会异步 `DestroyWindow`,在 HWND 销毁前 `WM_ACTIVATE`/`WM_PAINT` 仍可能
                // 触发 `handle.update().log_err()`。该竞态需在 GPUI Windows 侧于 Drop
                // 时先清空 platform callbacks 才能根治(见 vendor/gpui_windows)。
                let view = cx.new(|cx| {
                    SettingsView::new(
                        settings,
                        migration_error,
                        data_dir,
                        Some(on_save),
                        Some(on_theme_preview),
                        cx,
                    )
                });
                cx.subscribe(
                    &view,
                    move |_, _: &crate::settings_view::OpenSshManager, app| {
                        if let Some(main_window) = main_window {
                            let _ = main_window.update(app, |_, window, cx| {
                                let _ = ssh_workspace.update(cx, |workspace, cx| {
                                    workspace.open_ssh_manager(window, cx)
                                });
                            });
                        }
                    },
                )
                .detach();
                let flush_view = view.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    if let Some(view) = flush_view.upgrade() {
                        view.update(cx, |view, cx| view.flush_save(cx));
                    }
                    window.remove_window();
                    false
                });
                view
            },
        )
        .expect("open settings window")
    }
    fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        crate::updater::set_enabled(settings.automatic_updates, cx);
        // 设置保存回调（含语言切换）：同步 i18n 运行时，notify 触发整窗重渲染。
        // 测试二进制跳过，与 `WorkspaceView::new` 的门控保持一致。
        #[cfg(not(test))]
        termior_i18n::init(settings.language.as_deref());
        self.settings = settings;
        if let Some(library) = self.data_dir.as_ref().and_then(|dir| {
            termior_store::DataFiles::new(dir)
                .themes::<ThemeLibrary>()
                .load()
                .ok()
        }) {
            self.themes = library.all();
        }
        self.notification_router
            .set_enabled(self.settings.agent_notifications);
        self.sync_theme_index();
        self.recompute_palette(cx);
        self.schedule_explorer_scan(self.explorer_requested_root.clone(), cx);
        let (completer, completion_enabled) = self.completion_config();
        for tab in &self.tabs {
            for pane in tab.panes.values() {
                if let PaneContent::Editor(editor) = pane {
                    editor.update(cx, |editor, _| {
                        editor.set_preferences(
                            &self.settings.editor_theme_id,
                            self.settings.vim_mode,
                            completer.clone(),
                            completion_enabled,
                        )
                    });
                } else if let PaneContent::Terminal(terminal) = pane {
                    terminal.update(cx, |terminal, cx| {
                        terminal.set_settings(&self.settings.terminal, &self.settings.keymap, cx)
                    });
                }
            }
        }
        self.composer.update(cx, |composer, cx| {
            composer.configure(
                &self.settings,
                &self.model.root,
                self.workspace_auth.clone(),
                self.data_dir.clone(),
                cx,
            );
        });
    }
}
