use super::edit::SettingsInputHandler;
use super::*;
use gpui::TestAppContext;

fn view_with_draft(cx: &mut TestAppContext, draft: &str) -> gpui::Entity<SettingsView> {
    cx.new(|cx| {
        let mut view = SettingsView::new(Settings::default(), None, None, None, None, cx);
        view.edit_field = Some(EditField::AddModel);
        view.draft = draft.to_owned();
        view.cursor = draft.chars().count();
        view
    })
}

#[test]
fn model_list_adds_sets_agent_default_and_removes() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, " claude-custom ");
    view.update(&mut cx, |view, cx| {
        assert_eq!(view.profile().unwrap().id, "anthropic");
        view.commit_edit();
        assert!(view
            .profile()
            .unwrap()
            .model_choices()
            .iter()
            .any(|m| m == "claude-custom"));

        view.set_agent_default_model("claude-sonnet-5-5".into(), cx);
        let profile = view.profile().unwrap();
        assert_eq!(profile.model, "claude-sonnet-5-5");
        assert!(profile.enabled);
        assert_eq!(
            view.settings.models.active_chat_profile.as_deref(),
            Some("anthropic")
        );
        // 设默认不打乱列表顺序。
        assert_eq!(profile.model_choices()[0], "claude-opus-5-5");

        view.remove_model("claude-sonnet-5-5", cx);
        let profile = view.profile().unwrap();
        assert!(!profile
            .model_choices()
            .iter()
            .any(|m| m == "claude-sonnet-5-5"));
        assert_eq!(profile.model, profile.model_choices()[0]);
    });
}

#[test]
fn paste_replaces_selection() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "abcdef");
    view.update(&mut cx, |view, cx| {
        view.cursor = 2;
        view.anchor = Some(4);
        view.insert_text("XY", cx);
        assert_eq!(view.draft, "abXYef");
        assert_eq!(view.cursor, 4);
        assert_eq!(view.anchor, None);
    });
}

#[test]
fn paste_inserts_at_cursor_without_selection() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "abc");
    view.update(&mut cx, |view, cx| {
        view.cursor = 1;
        view.insert_text("XY", cx);
        assert_eq!(view.draft, "aXYbc");
        assert_eq!(view.cursor, 3);
        assert_eq!(view.anchor, None);
    });
}

#[test]
fn paste_strips_line_breaks_for_single_line_fields() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "");
    view.update(&mut cx, |view, cx| {
        view.insert_text("sk-\r\n123\n", cx);
        assert_eq!(view.draft, "sk-123");
    });
}

#[test]
fn collapsed_anchor_does_not_resurrect_selection_after_paste() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "ab");
    view.update(&mut cx, |view, cx| {
        // Shift+Left 再 Shift+Right 会留下 anchor == cursor 的折叠态。
        view.anchor = Some(1);
        view.cursor = 1;
        view.insert_text("X", cx);
        assert_eq!(view.draft, "aXb");
        assert_eq!(view.selection(), None);
    });
}

#[test]
fn delete_selection_collapses_cursor_to_start() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "abcdef");
    view.update(&mut cx, |view, _cx| {
        view.cursor = 2;
        view.anchor = Some(5);
        assert_eq!(view.selection(), Some(2..5));
        assert!(view.delete_selection());
        assert_eq!(view.draft, "abf");
        assert_eq!(view.cursor, 2);
        assert_eq!(view.anchor, None);
    });
}

#[test]
fn select_all_spans_whole_draft() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "gpt-4o");
    view.update(&mut cx, |view, _cx| {
        view.anchor = Some(0);
        assert_eq!(view.selection(), Some(0..6));
    });
}

#[test]
fn double_click_word_range_covers_urls_and_symbols() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = view_with_draft(&mut cx, "https://example.com/a b");
    view.update(&mut cx, |view, _cx| {
        // 点击 URL 内部：整段 URL 算一个词。
        assert_eq!(view.word_range_at(5), 0..21);
        // 点击 URL 与后续单词之间的空格边界：向左扩展到 URL 结尾。
        assert_eq!(view.word_range_at(21), 0..21);
        // 点击最后一个单词：只选中它。
        assert_eq!(view.word_range_at(22), 22..23);
    });
}

/// Composer 只选用已启用的 profile；设默认时必须连带启用，
/// 否则「设了默认聊天模型但 Agent 面板仍不可用」会静默失效。
#[test]
fn make_active_chat_enables_profile() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, cx| {
        assert!(!view.settings.models.profiles[0].enabled);
        view.make_active_chat(cx);
        assert!(view.settings.models.profiles[0].enabled);
        assert_eq!(
            view.settings.models.active_chat_profile.as_deref(),
            Some("anthropic")
        );
    });
}

#[test]
fn select_profile_switches_index_and_clamps() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, cx| {
        view.select_profile(7, cx); // DeepSeek
        assert_eq!(view.profile().map(|p| p.id.as_str()), Some("deepseek"));
        assert_eq!(view.select_menu, None);
        view.select_profile(usize::MAX, cx); // 越界时保持原索引
        assert_eq!(view.profile().map(|p| p.id.as_str()), Some("deepseek"));
    });
}

/// IME 候选窗锚点必须落在编辑字段文本上，且随光标前缀（如 CJK）右移。
#[test]
fn credential_drafts_stay_with_their_provider() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, cx| {
        view.edit_field = Some(EditField::ApiKey);
        view.draft = "test-only-provider-a".into();
        view.select_profile(1, cx);
        assert!(view.pending_api_key.is_none());
        view.pending_api_key = Some("test-only-provider-b".into());
        view.select_profile(0, cx);
        assert_eq!(
            view.pending_api_key.as_deref(),
            Some("test-only-provider-a")
        );
        view.select_profile(1, cx);
        assert_eq!(
            view.pending_api_key.as_deref(),
            Some("test-only-provider-b")
        );
        assert!(!serde_json::to_string(&view.settings)
            .unwrap()
            .contains("test-only"));
    });
}

#[test]
fn creating_and_selecting_agents_commits_to_the_original_agent() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, cx| {
        view.new_agent(cx);
        view.edit_field = Some(EditField::AgentName);
        view.draft = "Reviewer".into();
        view.new_agent(cx);
        assert_eq!(view.agents.agents[0].name, "Reviewer");
        view.edit_field = Some(EditField::AgentName);
        view.draft = "Writer".into();
        view.select_agent(0, cx);
        assert_eq!(view.agents.agents[1].name, "Writer");
        assert_eq!(view.agent().unwrap().name, "Reviewer");
    });
}

#[test]
fn tool_presets_and_toggles_preserve_permission_boundaries() {
    use termior_security::gating::{ToolId, ToolLevel};
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, cx| {
        view.new_agent(cx);
        view.set_agent_tools(true, cx);
        let tools = &view.agent().unwrap().tools;
        assert!(!tools.is_empty());
        assert!(tools
            .iter()
            .all(|name| ToolId::from_name(name).unwrap().level() == ToolLevel::Auto));
        view.toggle_agent_tool("write_file", cx);
        assert!(view
            .agent()
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "write_file"));
        view.toggle_agent_tool("write_file", cx);
        assert!(!view
            .agent()
            .unwrap()
            .tools
            .iter()
            .any(|t| t == "write_file"));
        view.set_agent_tools(false, cx);
        assert!(view.agent().unwrap().tools.is_empty());
    });
}

/// 探针只在编辑中的字段渲染，所以切到 Models 页并把 Model 字段置为编辑态。
#[test]
fn ime_anchor_tracks_edit_field_prefix() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut view = SettingsView::new(Settings::default(), None, None, None, None, cx);
        view.page = SettingsPage::Models;
        view.edit_field = Some(EditField::AddModel);
        view.draft = "你好model".into();
        view.cursor = "你好".chars().count();
        view
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    vcx.update(|window, cx| {
        let mut handler = SettingsInputHandler {
            view: view.downgrade(),
        };
        let bounds = handler
            .bounds_for_range(0..0, window, cx)
            .expect("bounds_for_range must report caret bounds while editing");
        let edit = view
            .read(cx)
            .edit_bounds
            .expect("edit bounds recorded by probe");
        assert_eq!(bounds.origin.y, edit.origin.y);
        assert!(
            bounds.origin.x > edit.origin.x,
            "CJK prefix must shift anchor right: {bounds:?} vs {edit:?}"
        );
        assert!(bounds.size.height > px(0.0));
    });
}

#[test]
fn content_pane_shows_a_scrollbar_when_the_page_overflows() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        SettingsView::new(Settings::default(), None, None, None, None, cx)
    });
    // 偏矮的窗格让配置页必然放不下；滚动条必须出现在内容区右缘。
    vcx.simulate_resize(gpui::size(px(880.), px(360.)));
    for page in [
        SettingsPage::General,
        SettingsPage::Models,
        SettingsPage::Themes,
        SettingsPage::Shortcuts,
        SettingsPage::Agents,
    ] {
        view.update(vcx, |v, cx| v.set_page(page, cx));
        vcx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let bar = vcx
            .debug_bounds("settings-scrollbar")
            .expect("scrollbar column");
        assert!(
            bar.size.height > px(100.),
            "{page:?} scrollbar spans the pane"
        );
        assert!(
            bar.right() <= px(880.) && bar.right() > px(800.),
            "{page:?} scrollbar sits at the right edge: {bar:?}"
        );
        view.update(vcx, |v, _| {
            assert!(
                v.scroll.max_offset().y > px(0.),
                "{page:?} overflows and needs the scrollbar"
            );
        });
    }
}

#[test]
fn agent_retention_fields_commit_within_range() {
    let mut cx = TestAppContext::single();
    cx.update(crate::updater::init);
    let view = cx.new(|cx| SettingsView::new(Settings::default(), None, None, None, None, cx));
    view.update(&mut cx, |view, _| {
        view.edit_field = Some(EditField::RetentionDays);
        view.draft = "14".into();
        view.commit_edit();
        assert_eq!(view.settings.agent_retention.max_age_days, 14);
        view.edit_field = Some(EditField::RetentionCapacity);
        view.draft = "512".into();
        view.commit_edit();
        assert_eq!(
            view.settings.agent_retention.max_disk_bytes,
            512 * 1024 * 1024
        );
        view.edit_field = Some(EditField::RetentionDays);
        view.draft = "0".into();
        view.commit_edit();
        assert_eq!(view.settings.agent_retention.max_age_days, 14);
        assert!(!view.status.is_empty());
    });
}

#[gpui::test]
fn agents_page_shows_disk_usage_and_cleanup_keeps_the_journal(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let journal = root.join("agent-tasks/task/events.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, b"journal-bytes").unwrap();
    let blob = root.join("agent-artifacts/blobs/chunk.txt");
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, vec![b'x'; 2048]).unwrap();
    let context = root.join("agent-artifacts/task/tool-output/body.txt");
    std::fs::create_dir_all(context.parent().unwrap()).unwrap();
    std::fs::write(&context, b"context-body").unwrap();
    std::fs::write(root.join("hooks.json"), b"hook").unwrap();
    std::fs::write(root.join("mcp.log"), b"mcp").unwrap();
    std::fs::write(root.join("Termior-crash.log"), b"boom").unwrap();
    std::fs::write(root.join("Termior-settings.json"), b"{}").unwrap();
    for path in [
        &blob,
        &context,
        &root.join("hooks.json"),
        &root.join("mcp.log"),
        &root.join("Termior-crash.log"),
    ] {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(5))
            .unwrap();
    }
    let journal_bytes = std::fs::read(&journal).unwrap();

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
    view.update(vcx, |view, cx| view.set_page(SettingsPage::Agents, cx));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    view.update(vcx, |view, _| {
        let usage = view.agent_usage.expect("usage measured");
        assert_eq!(usage.journal, journal_bytes.len() as u64);
        assert_eq!(usage.blobs, 2048);
        assert_eq!(usage.context, b"context-body".len() as u64);
        assert_eq!(usage.hook_mcp, (b"hook".len() + b"mcp".len()) as u64);
        assert_eq!(usage.crash, b"boom".len() as u64);
        assert_eq!(view.settings.agent_retention.max_age_days, 30);
    });
    assert!(vcx.debug_bounds("agent-data-usage-total").is_some());
    // 明细行已随设置页简化移除，只保留合计一行。
    for detail in [
        "agent-data-usage-journal",
        "agent-data-usage-blobs",
        "agent-data-usage-hook-mcp",
        "agent-data-usage-crash",
    ] {
        assert!(
            vcx.debug_bounds(detail).is_none(),
            "{detail} should not render"
        );
    }
    let cleanup = vcx
        .debug_bounds("agent-data-cleanup")
        .expect("cleanup action");
    vcx.simulate_mouse_down(
        cleanup.center(),
        MouseButton::Left,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();

    assert_eq!(std::fs::read(&journal).unwrap(), journal_bytes);
    assert!(!blob.exists(), "expired blob is removed");
    assert!(!context.exists(), "expired context is removed");
    assert!(!root.join("hooks.json").exists());
    assert!(!root.join("mcp.log").exists());
    assert!(!root.join("Termior-crash.log").exists());
    assert!(root.join("Termior-settings.json").is_file());
    view.update(vcx, |view, _| {
        let usage = view.agent_usage.expect("usage refreshed");
        assert_eq!(usage.journal, journal_bytes.len() as u64);
        assert_eq!(usage.blobs, 0);
        assert_eq!(usage.context, 0);
        assert!(!view.cleanup_running);
        assert!(
            view.status.contains("Journal") || view.status.contains("journal"),
            "status should report that journals stayed intact: {}",
            view.status
        );
    });
}
