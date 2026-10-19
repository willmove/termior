use super::*;
use gpui::{px, MouseButton, TestAppContext};
use termior_agent_host::{
    mcp_servers_path, McpLoginStatus, McpRemoteServer, McpServerCatalog,
    MCP_AUTH_DETAIL_REFRESH_REJECTED,
};

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

#[gpui::test]
fn mcp_page_shows_login_status_and_logout_clears_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let catalog = McpServerCatalog {
        servers: vec![
            McpRemoteServer {
                id: "docs".into(),
                url: "https://mcp.example/mcp".into(),
                oauth_key_ref: Some("mcp-oauth:docs".into()),
                client_id: Some("termior-public".into()),
                scopes: vec!["mcp".into()],
                auth_status: McpLoginStatus::LoggedIn,
                auth_detail: String::new(),
            },
            McpRemoteServer {
                id: "wiki".into(),
                url: "https://wiki.example/mcp".into(),
                oauth_key_ref: Some("mcp-oauth:wiki".into()),
                client_id: Some("termior-public".into()),
                scopes: vec!["mcp".into()],
                auth_status: McpLoginStatus::ReLoginRequired,
                auth_detail: MCP_AUTH_DETAIL_REFRESH_REJECTED.into(),
            },
        ],
    };
    catalog.save(&mcp_servers_path(root), &[]).unwrap();

    cx.update(crate::updater::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        SettingsView::new(
            Settings::default(),
            None,
            Some(root.to_path_buf()),
            None,
            None,
            cx,
        )
    });
    vcx.simulate_resize(gpui::size(px(880.), px(900.)));
    view.update(vcx, |view, cx| view.set_page(SettingsPage::Mcp, cx));
    draw(vcx);

    assert!(vcx
        .debug_bounds("docs · https://mcp.example/mcp · Signed in")
        .is_some());
    assert!(vcx
        .debug_bounds("wiki · https://wiki.example/mcp · Sign-in required")
        .is_some());
    assert!(vcx
        .debug_bounds("The authorization server rejected the refresh token. Sign in again.")
        .is_some());
    assert!(vcx.debug_bounds("mcp-sign-in-wiki").is_some());
    let logout = vcx.debug_bounds("mcp-logout-docs").expect("logout action");
    vcx.simulate_mouse_down(
        logout.center(),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    draw(vcx);

    assert!(vcx
        .debug_bounds("docs · https://mcp.example/mcp · Signed out")
        .is_some());
    let saved = McpServerCatalog::load(&mcp_servers_path(root)).unwrap();
    assert_eq!(saved.servers[0].auth_status, McpLoginStatus::LoggedOut);
    let text = std::fs::read_to_string(mcp_servers_path(root)).unwrap();
    assert!(!text.contains("access_token"));
    assert!(!text.contains("refresh_token"));
    assert!(!text.contains("termior-test-access"));
}
