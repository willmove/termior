//! Remote MCP server sign-in status. Tokens stay in the system keychain.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use termior_agent_host::{
    login_with_browser, logout_mcp_server, mcp_servers_path, system_clock, McpLoginStatus,
    McpOAuthBinding, McpServerCatalog, PkceLoginOptions, MCP_AUTH_DETAIL_REFRESH_REJECTED,
    MCP_AUTH_DETAIL_REVOKE_FAILED,
};

impl SettingsView {
    pub(super) fn reload_mcp_servers(&mut self) {
        let Some(dir) = self.data_dir.clone() else {
            self.mcp_servers = McpServerCatalog::default();
            self.mcp_load_error = None;
            return;
        };
        match McpServerCatalog::load(&mcp_servers_path(&dir)) {
            Ok(catalog) => {
                self.mcp_servers = catalog;
                self.mcp_load_error = None;
            }
            Err(error) => self.mcp_load_error = Some(error.to_string()),
        }
    }

    fn mcp_binding(&self, server_id: &str) -> Option<McpOAuthBinding> {
        let dir = self.data_dir.clone()?;
        Some(McpOAuthBinding {
            server_id: server_id.to_owned(),
            store: Arc::new(KeyringSecretStore::new()),
            catalog_path: mcp_servers_path(&dir),
            now: system_clock(),
            diagnostic_log: Some(dir.join("mcp.log")),
        })
    }

    pub(super) fn sign_in_mcp(&mut self, server_id: String, cx: &mut Context<Self>) {
        let Some(server) = self
            .mcp_servers
            .servers
            .iter()
            .find(|server| server.id == server_id)
            .cloned()
        else {
            return;
        };
        let Some(binding) = self.mcp_binding(&server_id) else {
            self.status = t!("settings.mcp.storage_unavailable").to_string();
            cx.notify();
            return;
        };
        self.status = tf!("settings.mcp.sign_in_waiting", "id" => server_id.clone()).to_string();
        cx.notify();
        let endpoint = server.url;
        let options = PkceLoginOptions {
            binding,
            client_id: server.client_id,
            scopes: server.scopes,
            timeout: Duration::from_secs(180),
        };
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    login_with_browser(&endpoint, &options, &|url| {
                        termior_platform::open_external(url).map_err(|error| error.to_string())
                    })
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.reload_mcp_servers();
                view.status = match result {
                    Ok(()) => tf!("settings.mcp.signed_in", "id" => server_id).to_string(),
                    Err(error) => tf!(
                        "settings.mcp.sign_in_failed",
                        "id" => server_id,
                        "error" => error.to_string()
                    )
                    .to_string(),
                };
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn logout_mcp(&mut self, server_id: String, cx: &mut Context<Self>) {
        let Some(binding) = self.mcp_binding(&server_id) else {
            self.status = t!("settings.mcp.storage_unavailable").to_string();
            cx.notify();
            return;
        };
        let task = cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { logout_mcp_server(&binding) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.reload_mcp_servers();
                view.status = match result {
                    Ok(report) if report.remote_revoked => {
                        tf!("settings.mcp.signed_out", "id" => server_id).to_string()
                    }
                    Ok(_) => t!("settings.mcp.detail_revoke_failed").to_string(),
                    Err(error) => tf!(
                        "settings.mcp.logout_failed",
                        "id" => server_id,
                        "error" => error.to_string()
                    )
                    .to_string(),
                };
                cx.notify();
            });
        });
        self.track_task(task);
    }

    pub(super) fn mcp_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = self.palette.clone();
        let load_error = self.mcp_load_error.clone();
        let servers = self.mcp_servers.servers.clone();
        let empty = servers.is_empty();
        let rows: Vec<_> = servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let status = match server.auth_status {
                    McpLoginStatus::LoggedIn => t!("settings.mcp.status_logged_in"),
                    McpLoginStatus::LoggedOut => t!("settings.mcp.status_logged_out"),
                    McpLoginStatus::ReLoginRequired => t!("settings.mcp.status_relogin"),
                };
                let summary = tf!(
                    "settings.mcp.row",
                    "id" => server.id.clone(),
                    "url" => server.url.clone(),
                    "status" => status.to_string()
                );
                let detail = match server.auth_detail.as_str() {
                    MCP_AUTH_DETAIL_REFRESH_REJECTED => {
                        Some(t!("settings.mcp.detail_refresh_rejected").to_string())
                    }
                    MCP_AUTH_DETAIL_REVOKE_FAILED => {
                        Some(t!("settings.mcp.detail_revoke_failed").to_string())
                    }
                    _ => None,
                };
                let painted = summary.to_string();
                let server_id = server.id.clone();
                let show_sign_in = server.auth_status != McpLoginStatus::LoggedIn;
                let show_logout = server.auth_status != McpLoginStatus::LoggedOut;
                div()
                    .id(SharedString::from(format!("mcp-server-{index}")))
                    .debug_selector({
                        let painted = painted.clone();
                        move || painted.clone()
                    })
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .bg(crate::ui::alpha(palette.foreground, 0.04))
                    .aria_label(summary.clone())
                    .child(div().text_sm().child(summary))
                    .when_some(detail, |row, detail| {
                        let painted = detail.clone();
                        row.child(
                            div()
                                .text_xs()
                                .text_color(crate::ui::muted(&palette))
                                .debug_selector(move || painted.clone())
                                .child(detail),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .when(show_sign_in, |actions| {
                                let server_id = server_id.clone();
                                actions.child(
                                    Self::button(
                                        t!("settings.mcp.sign_in"),
                                        SharedString::from(format!("mcp-sign-in-{server_id}")),
                                        &palette,
                                    )
                                    .debug_selector({
                                        let server_id = server_id.clone();
                                        move || format!("mcp-sign-in-{server_id}")
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.sign_in_mcp(server_id.clone(), cx);
                                        }),
                                    ),
                                )
                            })
                            .when(show_logout, |actions| {
                                let server_id = server_id.clone();
                                actions.child(
                                    Self::button(
                                        t!("settings.mcp.log_out"),
                                        SharedString::from(format!("mcp-logout-{server_id}")),
                                        &palette,
                                    )
                                    .debug_selector({
                                        let server_id = server_id.clone();
                                        move || format!("mcp-logout-{server_id}")
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.logout_mcp(server_id.clone(), cx);
                                        }),
                                    ),
                                )
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_6()
            .when_some(load_error, |page, error| {
                page.child(
                    div()
                        .text_xs()
                        .text_color(crate::ui::color(palette.status[3]))
                        .child(tf!("settings.mcp.load_failed", "error" => error)),
                )
            })
            .child(
                self.section(
                    t!("settings.mcp.title"),
                    t!("settings.mcp.description"),
                    [div()
                        .id("mcp-server-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(empty, |list| {
                            list.child(
                                div()
                                    .id("mcp-servers-empty")
                                    .debug_selector(|| "mcp-servers-empty".into())
                                    .text_sm()
                                    .text_color(crate::ui::muted(&palette))
                                    .child(t!("settings.mcp.empty")),
                            )
                        })
                        .children(rows)
                        .into_any_element()],
                ),
            )
            .into_any_element()
    }
}
