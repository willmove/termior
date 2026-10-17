use super::*;
use gpui::{px, MouseButton, TestAppContext};

const SECRET: &str = "sk-testsecretvalue123456";

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

#[gpui::test]
fn hooks_page_lists_hooks_and_redacts_recent_runs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let catalog = termior_hooks::HookCatalog {
        hooks: vec![
            termior_hooks::HookConfig {
                id: "lint".into(),
                point: termior_hooks::HookPoint::PreTool,
                program: "echo".into(),
                args: vec![SECRET.into()],
                enabled: true,
                fail_closed: true,
                source: "project".into(),
                timeout_ms: 1500,
            },
            termior_hooks::HookConfig {
                id: "format".into(),
                point: termior_hooks::HookPoint::PostTool,
                program: "printf".into(),
                args: vec!["ok".into()],
                enabled: true,
                fail_closed: true,
                source: "user".into(),
                timeout_ms: 5000,
            },
        ],
    };
    std::fs::write(
        termior_hooks::lifecycle_hooks_path(root),
        serde_json::to_string_pretty(&catalog).unwrap(),
    )
    .unwrap();
    let runs = vec![termior_hooks::HookRunRecord {
        hook_id: "lint".into(),
        task_id: "task".into(),
        point: termior_hooks::HookPoint::PreTool,
        decision: "replace".into(),
        diagnostic: format!("path {SECRET}"),
        duration_ms: 42,
        modified_arguments: true,
    }];
    std::fs::write(
        termior_hooks::hook_runs_path(root),
        serde_json::to_string_pretty(&runs).unwrap(),
    )
    .unwrap();

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
    vcx.simulate_resize(gpui::size(px(880.), px(1400.)));
    view.update(vcx, |view, cx| view.set_page(SettingsPage::Hooks, cx));
    draw(vcx);

    let first = "lint · project · pre-tool · echo [REDACTED] · 1500 ms";
    let second = "format · user · post-tool · printf ok · 5000 ms";
    let run = "pre-tool · 42 ms · replace: path [REDACTED] · arguments modified";
    assert!(
        vcx.debug_bounds(first).is_some(),
        "first hook row should render source, trigger, redacted command, and timeout"
    );
    assert!(
        vcx.debug_bounds(second).is_some(),
        "second hook row should render source, trigger, command, and timeout"
    );
    assert!(
        vcx.debug_bounds(run).is_some(),
        "recent run should render trigger, duration, redacted result, and argument change"
    );
    let leaked_hook: &'static str =
        Box::leak(format!("lint · project · pre-tool · echo {SECRET} · 1500 ms").into_boxed_str());
    let leaked_run: &'static str = Box::leak(
        format!("pre-tool · 42 ms · replace: path {SECRET} · arguments modified").into_boxed_str(),
    );
    assert!(vcx.debug_bounds(leaked_hook).is_none());
    assert!(vcx.debug_bounds(leaked_run).is_none());
}

#[gpui::test]
fn disabling_a_hook_persists_and_the_runner_skips_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let catalog = termior_hooks::HookCatalog {
        hooks: vec![termior_hooks::HookConfig {
            id: "skip".into(),
            point: termior_hooks::HookPoint::PreTool,
            program: "definitely-not-a-real-hook-binary".into(),
            args: Vec::new(),
            enabled: true,
            fail_closed: true,
            source: "user".into(),
            timeout_ms: 5_000,
        }],
    };
    catalog
        .save(&termior_hooks::lifecycle_hooks_path(root))
        .unwrap();

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
    view.update(vcx, |view, cx| view.set_page(SettingsPage::Hooks, cx));
    draw(vcx);
    let toggle = vcx
        .debug_bounds("lifecycle-hook-toggle-0")
        .expect("disable control");
    vcx.simulate_mouse_down(
        toggle.center(),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    let loaded =
        termior_hooks::HookCatalog::load(&termior_hooks::lifecycle_hooks_path(root)).unwrap();
    assert!(!loaded.hooks[0].enabled);
    let pipeline = termior_hooks::HookPipeline::new(loaded.hooks);
    let event = termior_hooks::HookEvent {
        schema_version: termior_hooks::HOOK_SCHEMA_VERSION,
        point: termior_hooks::HookPoint::PreTool,
        task_id: "task".into(),
        tool: Some("read_file".into()),
        arguments: Some(serde_json::json!({"path": "a.txt"})),
    };
    let outcome = pipeline
        .apply_pre_tool(&event, |_| panic!("disabled hook must not run"))
        .unwrap();
    assert!(outcome.records.is_empty());
    assert!(!outcome.replaced);
}
