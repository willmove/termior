use super::helpers::requested_skill_names;
use super::input::ComposerInputHandler;
use super::orchestrator::ExternalPendingApproval;
use super::*;
use gpui::TestAppContext;
use termior_agent_host::PERMISSIONS_APPROVAL_METHOD;

fn draw(vcx: &mut gpui::VisualTestContext) {
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

/// 真实布局下拖选跨越用户消息与 Agent 回复的多个段落，复制得到原文。
#[test]
fn dragging_over_messages_selects_and_copies_text() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, _| {
        // 面板足够高，内容不触发贴底滚动，第一条消息完整可见。
        view.panel_height = 600.0;
        view.history.push(Message::user("请解释一下"));
        view.history.push(Message::assistant(
            "First paragraph\n\nSecond **bold** line",
        ));
    });
    draw(vcx);
    let segments = composer.update(vcx, |view, _| view.selectable_texts.segment_bounds());
    let keys = segments.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            SegmentKey { group: 0, seq: 0 },
            SegmentKey { group: 1, seq: 0 },
            SegmentKey { group: 1, seq: 1 },
        ]
    );
    let start = segments[0].1.origin + gpui::point(px(1.0), segments[0].1.size.height / 2.0);
    let last = segments[2].1;
    let end = gpui::point(last.right() + px(40.0), last.bottom() + px(4.0));

    vcx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
    draw(vcx);
    vcx.simulate_mouse_move(end, MouseButton::Left, gpui::Modifiers::none());

    vcx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::none());
    composer.update(vcx, |view, _| {
        assert!(!view.selecting_text, "mouse up ends the drag")
    });
    vcx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    let copied = vcx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("selection copied");
    assert_eq!(
        copied,
        "请解释一下\n\nFirst paragraph\n\nSecond **bold** line"
    );

    // 点在消息区内、文字之外的空白处清除选区。
    let blank = gpui::point(start.x, last.bottom() + px(40.0));
    vcx.simulate_mouse_down(blank, MouseButton::Left, gpui::Modifiers::none());
    composer.update(vcx, |view, _| assert!(view.text_selection.is_none()));
}

/// 完整选中的 Markdown 段复制源码：标记、链接、列表标记与代码围栏保留，
/// 段间空行分隔；部分选取的段仍按渲染文本精确截取。
#[test]
fn selecting_markdown_blocks_copies_markdown_source() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    let message = concat!(
        "修复 `run()` 里的 **竞态**，见 [笔记](https://example.com/a)。\n\n",
        "- 先停任务\n",
        "- 再清理缓存\n\n",
        "```rust\nfn main() {}\n```"
    );
    composer.update(vcx, |view, _| {
        view.panel_height = 600.0;
        view.history.push(Message::assistant(message));
    });
    draw(vcx);
    let segments = composer.update(vcx, |view, _| view.selectable_texts.segment_bounds());
    // 段落、两个列表项、代码块各一段。
    assert_eq!(segments.len(), 4, "paragraph + two items + code block");
    let first = segments[0].1;
    let last = segments[3].1;
    // 按下必须命中段内（hit 而非 nearest）才会开始选取；拖动与抬起可用
    // 段外坐标，nearest 把端点吸附到首/末段边界，得到全选。
    let start = first.origin + gpui::point(px(1.0), first.size.height / 2.0);
    let end = gpui::point(last.right() + px(4.0), last.bottom() + px(4.0));
    vcx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
    draw(vcx);
    vcx.simulate_mouse_move(end, MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::none());
    vcx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    let copied = vcx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("selection copied");
    assert_eq!(
        copied,
        concat!(
            "修复 `run()` 里的 **竞态**，见 [笔记](https://example.com/a)。\n\n",
            "- 先停任务\n\n",
            "- 再清理缓存\n\n",
            "```rust\nfn main() {}\n```"
        )
    );
}

fn segment_keys(
    composer: &gpui::Entity<ComposerView>,
    vcx: &mut gpui::VisualTestContext,
) -> Vec<SegmentKey> {
    composer.update(vcx, |view, _| {
        view.selectable_texts
            .segment_bounds()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    })
}

/// Agent 的命令在会话里显示为折叠块：默认只有命令与状态，展开后输出可选取复制。
#[test]
fn tool_calls_render_as_collapsible_blocks() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, _| {
        view.panel_height = 600.0;
        view.history.push(Message::user("跑一下测试"));
        view.history.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![termior_ai::ToolCall {
                id: "call-1".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"cargo test"}"#.into(),
            }],
            tool_result: None,
        });
        view.history.push(Message {
            role: Role::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_result: Some(termior_ai::ToolResult::success(
                "call-1",
                "session_id=s1 state=Exited exit=0 timed_out=false cursor=2\ntest result: ok",
            )),
        });
    });
    draw(vcx);
    assert_eq!(
        segment_keys(&composer, vcx),
        vec![SegmentKey { group: 0, seq: 0 }],
        "collapsed block shows no output text"
    );

    composer.update(vcx, |view, cx| view.toggle_tool_block("call-1", cx));
    draw(vcx);
    let keys = segment_keys(&composer, vcx);
    assert_eq!(keys.len(), 2, "expanded block adds the output segment");
    assert_eq!(keys[1].group, 1, "output belongs to the issuing message");
}

/// 运行中实时推送的定稿 assistant 消息替换流式占位，工具结果随即追加。
#[test]
fn live_messages_replace_the_streaming_draft() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, _| {
        view.history.push(Message::user("hi"));
        view.append_text_delta("Let me ".into());
        view.append_text_delta("check".into());
        let mut assistant = Message::assistant("Let me check");
        assistant.tool_calls.push(termior_ai::ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
        });
        view.apply_live_message(Message::user("hi"));
        view.apply_live_message(assistant);
        view.apply_live_message(Message {
            role: Role::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_result: Some(termior_ai::ToolResult::success("call-1", "fn main() {}")),
        });
        view.append_text_delta("Done".into());
        let roles = view
            .history
            .iter()
            .map(|message| message.role)
            .collect::<Vec<_>>();
        assert_eq!(
            roles,
            vec![Role::User, Role::Assistant, Role::Tool, Role::Assistant]
        );
        assert_eq!(view.history[1].tool_calls.len(), 1);
        assert_eq!(view.history[3].content, "Done");
    });
}

/// 用真实 ComposerView 渲染，防止输入行结构改动把文本压成逐字换行（竖排）。
/// CJK 输入 + 光标在开头是用户踩过的状态；断言输入文本保持单行高度。
#[test]
fn composer_input_text_stays_on_one_line() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    let input = "你现在是什么模型为什么这样显示输入内容测试";
    composer.update(vcx, |view, _| {
        view.draft.input = input.into();
        view.cursor = 0;
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let text = vcx
        .debug_bounds("composer-input-text")
        .expect("input text rendered");
    assert!(
        text.size.height < px(30.0),
        "composer input wrapped to multiple lines: {text:?}"
    );
    assert!(
        text.size.width > px(150.0),
        "composer input collapsed: {text:?}"
    );
}

/// IME 候选窗锚点必须落在输入行文本上，且随光标前缀（如 CJK）右移。
#[test]
fn ime_anchor_tracks_composer_input_cursor() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, _| {
        view.draft.input = "你好 world".into();
        view.cursor = "你好".chars().count();
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let text = vcx
        .debug_bounds("composer-input-text")
        .expect("input text rendered");
    vcx.update(|window, cx| {
        let mut handler = ComposerInputHandler {
            view: composer.downgrade(),
        };
        let bounds = handler
            .bounds_for_range(0..0, window, cx)
            .expect("bounds_for_range must report caret bounds after paint");
        assert_eq!(bounds.origin.y, text.origin.y);
        assert!(
            bounds.origin.x > text.origin.x,
            "CJK prefix must shift anchor right: {bounds:?} vs {text:?}"
        );
        assert!(bounds.size.height > px(0.0));
    });
}

/// Ctrl+V 把剪贴板纯文本插入输入框光标处（含多行），不产生附件。
#[test]
fn ctrl_v_pastes_clipboard_text_at_cursor() {
    let mut cx = TestAppContext::single();
    cx.write_to_clipboard(ClipboardItem::new_string(" the bug\nsee log".to_string()));
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, _| {
        view.draft.input = "fix".into();
        view.cursor = "fix".chars().count();
    });
    press(
        &composer,
        vcx,
        if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        },
    );
    composer.update(vcx, |view, _| {
        assert_eq!(view.draft.input, "fix the bug\nsee log");
        assert_eq!(view.cursor, "fix the bug\nsee log".chars().count());
        assert!(view.draft.attachments.is_empty());
    });
}

fn type_text(composer: &gpui::Entity<ComposerView>, vcx: &mut gpui::VisualTestContext, text: &str) {
    vcx.update(|window, cx| {
        let mut handler = ComposerInputHandler {
            view: composer.downgrade(),
        };
        handler.replace_text_in_range(None, text, window, cx);
    });
}

#[test]
fn slash_palette_runs_composer_commands_and_clears_input() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    type_text(&composer, vcx, "/pl");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    composer.update(vcx, |view, cx| {
        assert_eq!(view.slash_suggestions[0].name, "plan");
        assert!(!view.is_compact(), "palette must expand the composer");
        view.run_slash_command(view.slash_suggestions[0], cx);
        assert_eq!(view.mode, Mode::Plan);
        assert!(view.draft.input.is_empty());
        assert!(view.slash_suggestions.is_empty());
    });
}

#[test]
fn slash_palette_ignores_paths_inside_messages() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    type_text(&composer, vcx, "look at /usr/bin");
    composer.update(vcx, |view, _| assert!(view.slash_suggestions.is_empty()));
}

#[test]
fn slash_palette_forwards_app_actions_to_the_workspace() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&composer, move |_, action: &ComposerAppAction, _| {
            sink.lock().unwrap().push(action.0);
        })
        .detach();
    });
    composer.update(vcx, |view, cx| {
        let command = slash::find_command("split-right").expect("catalogued");
        view.run_slash_command(command, cx);
    });
    assert_eq!(*received.lock().unwrap(), vec![KeyAction::SplitRight]);
}

fn press(composer: &gpui::Entity<ComposerView>, vcx: &mut gpui::VisualTestContext, key: &str) {
    composer.update_in(vcx, |view, window, cx| {
        let event = KeyDownEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
            is_held: false,
            prefer_character_input: false,
        };
        view.handle_key_down(&event, window, cx);
    });
}

#[test]
fn snippet_command_saves_and_hash_completion_inserts_handle() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    type_text(&composer, vcx, "/snippet review Please review\n  carefully");
    press(&composer, vcx, "enter");
    composer.update(vcx, |view, _| {
        assert!(view.draft.input.is_empty(), "command must not be sent");
        assert!(view.history.is_empty());
        assert_eq!(view.snippets.snippets[0].handle, "review");
    });

    type_text(&composer, vcx, "do #rev");
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    composer.update(vcx, |view, _| {
        assert_eq!(view.snippet_suggestions[0].handle, "review");
        assert!(!view.is_compact());
    });
    press(&composer, vcx, "tab");
    composer.update(vcx, |view, _| {
        assert_eq!(view.draft.input, "do #review ");
        assert_eq!(view.cursor, view.draft.input.chars().count());
        assert!(view.snippet_suggestions.is_empty());
    });
}

#[test]
fn invalid_snippet_command_keeps_input_for_correction() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    type_text(&composer, vcx, "/snippet onlyhandle");
    press(&composer, vcx, "enter");
    composer.update(vcx, |view, _| {
        assert_eq!(view.draft.input, "/snippet onlyhandle");
        assert!(view.snippets.snippets.is_empty());
    });
}

#[test]
fn selecting_an_argument_command_only_completes_its_name() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    type_text(&composer, vcx, "/snippet");
    press(&composer, vcx, "enter");
    composer.update(vcx, |view, _| {
        assert_eq!(view.draft.input, "/snippet ");
        assert!(view.slash_suggestions.is_empty());
    });
}

#[test]
fn open_panels_keep_an_empty_composer_expanded() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        assert!(view.is_compact());
        view.invoke_slash_command(slash::find_command("snippets").unwrap(), "", cx);
        assert!(view.center_open(ComposerCenter::Snippets));
        assert!(!view.is_compact());
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

#[test]
fn opening_a_panel_closes_the_previous_one() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        view.invoke_slash_command(slash::find_command("snippets").unwrap(), "", cx);
        view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
        assert!(view.center_open(ComposerCenter::Todos));
        assert!(!view.center_open(ComposerCenter::Snippets));
        view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
        assert!(!view.any_center_open());
    });
}

#[test]
fn a_tall_panel_is_capped_and_leaves_room_for_the_conversation() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    let panel_height = composer.update(vcx, |view, cx| {
        for index in 0..40 {
            view.todos
                .lock()
                .unwrap()
                .add(format!("todo-{index}"), "a long backlog item");
        }
        view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
        view.panel_height
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    let dock = vcx
        .debug_bounds("composer-center-dock")
        .expect("panel dock rendered");
    let messages = vcx
        .debug_bounds("composer-messages")
        .expect("conversation rendered");
    assert!(
        dock.size.height <= px(panel_height * CENTER_DOCK_MAX_FRACTION + 1.0),
        "panel dock not capped: {dock:?}"
    );
    assert!(
        dock.size.height >= px(panel_height * CENTER_DOCK_MAX_FRACTION - 1.0),
        "overflowing panel should fill its cap: {dock:?}"
    );
    assert!(
        messages.size.height > px(0.0),
        "conversation squeezed out: {messages:?}"
    );
}

#[test]
fn todo_panel_shows_agent_items_and_user_edits_update_the_shared_store() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        view.todos.lock().unwrap().add("todo-1", "write tests");
        view.invoke_slash_command(slash::find_command("todos").unwrap(), "", cx);
        assert!(view.center_open(ComposerCenter::Todos));
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    composer.update(vcx, |view, cx| {
        view.update_todos(cx, |store| store.set_done("todo-1", true));
        assert_eq!(view.todos.lock().unwrap().open_count(), 0);
        view.update_todos(cx, |store| store.remove("todo-1"));
        assert!(view.todos.lock().unwrap().items.is_empty());
    });
}

#[test]
fn new_session_command_starts_an_empty_active_session() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        view.sessions.create("old");
        view.history.push(Message::user("hello"));
        view.new_session(cx);
        assert!(view.history.is_empty());
        assert_eq!(view.sessions.sessions.len(), 2);
        assert_ne!(view.sessions.active_id.as_deref(), Some("old"));
        assert_eq!(view.sessions.get("old").unwrap().messages.len(), 1);

        view.orchestrator.busy = true;
        view.new_session(cx);
        assert_eq!(
            view.sessions.sessions.len(),
            2,
            "busy composer keeps its session"
        );
    });
}

#[test]
fn session_model_choice_overrides_the_default_and_resets_for_new_sessions() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        let mut models = ModelSettings::default();
        for profile in &mut models.profiles {
            profile.enabled = profile.id == "anthropic";
        }
        models.active_chat_profile = Some("anthropic".into());
        view.model_settings = models;
        view.sessions.create("s1");
        assert_eq!(view.model_chip_label().as_ref(), "claude-opus-5-5");

        let choice = SessionModel {
            profile_id: "anthropic".into(),
            model: "claude-haiku-5-5".into(),
        };
        view.choose_model(Some(choice.clone()), cx);
        assert_eq!(view.model_chip_label().as_ref(), "claude-haiku-5-5");
        assert_eq!(
            view.sessions.get("s1").unwrap().model.as_ref(),
            Some(&choice),
            "the choice is remembered with the session"
        );

        // 选到未启用的服务商不生效，回落到 Agent 默认模型。
        view.choose_model(
            Some(SessionModel {
                profile_id: "openai".into(),
                model: "gpt-6.1-sol".into(),
            }),
            cx,
        );
        assert_eq!(view.model_chip_label().as_ref(), "claude-opus-5-5");

        view.model_choice = Some(choice);
        view.new_session(cx);
        assert!(view.model_choice.is_none());

        view.model_menu_open = true;
        view.model_menu_ready = vec![true; view.model_settings.profiles.len()];
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

#[test]
fn skills_activate_only_through_an_explicit_dollar_mention() {
    let skills = vec![SkillMetadata {
        name: "review-code".into(),
        description: "Review code".into(),
        path: PathBuf::from(".agents/skills/review-code/SKILL.md"),
        allowed_tools: Default::default(),
        body_loaded: false,
    }];

    assert!(requested_skill_names("please review-code", &skills).is_empty());
    assert_eq!(
        requested_skill_names("please use $review-code, now", &skills),
        vec!["review-code"]
    );
}

#[test]
fn agent_menu_separates_backend_kinds_and_launches_cli_agents_in_a_terminal() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    let launched = Arc::new(Mutex::new(Vec::new()));
    let sink = launched.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&composer, move |_, launch: &ComposerLaunchCliAgent, _| {
            sink.lock().unwrap().push(launch.0.program);
        })
        .detach();
    });
    composer.update(vcx, |view, cx| {
        view.toggle_agent_menu(cx);
        assert!(view.agent_menu_open);
        assert_eq!(
            view.cli_agent_available.len(),
            termior_agent_host::CLI_AGENTS.len()
        );
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
    composer.update(vcx, |view, cx| {
        view.choose_agent(agent_menu::AgentMenuChoice::Codex, cx);
        assert_eq!(
            view.orchestrator.backend_choice,
            AgentBackendChoice::CodexAppServer
        );
        assert!(!view.agent_menu_open);
        view.choose_agent(agent_menu::AgentMenuChoice::BuiltIn(None), cx);
        assert_eq!(
            view.orchestrator.backend_choice,
            AgentBackendChoice::BuiltIn
        );
        // 普通 CLI Agent 不改变 Composer 后端，只请求在终端里启动。
        view.choose_agent(agent_menu::AgentMenuChoice::Cli(0), cx);
        assert_eq!(
            view.orchestrator.backend_choice,
            AgentBackendChoice::BuiltIn
        );
    });
    assert_eq!(*launched.lock().unwrap(), vec!["claude"]);
}

#[test]
fn terminal_action_without_a_model_keeps_the_attachment_and_drops_the_project_pin() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        view.run_terminal_action(
            TerminalAgentAction::FixAndRerun,
            "$ cargo test (exit 101)".into(),
            "error[E0425]".into(),
            Some(termior_ai::CommandProvenance {
                id: "cmd-1".into(),
                cwd: "/project".into(),
                command: Some("cargo test".into()),
                exit_code: Some(101),
            }),
            PathBuf::from("/project"),
            cx,
        );
        assert_eq!(view.draft.attachments.len(), 1);
        assert!(view.draft.input.contains("cargo test"));
        assert!(view.needs_model_setup);
        assert_eq!(view.orchestrator.task_project_override, None);
        assert!(view.orchestrator.active_task.is_none());
    });
}

#[test]
fn task_details_panel_renders_for_the_last_task_and_hand_back_needs_an_agent() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    composer.update(vcx, |view, cx| {
        view.last_task = Some(TaskRef {
            task_id: "task-details-without-commands".into(),
            project_dir: PathBuf::from("/project"),
            backend_id: "native".into(),
            environment_id: "direct".into(),
        });
        view.open_center = Some(ComposerCenter::TaskDetails);
        assert!(view.any_center_open());
        assert!(view.task_digests().is_empty());
        view.hand_back_terminal(&termior_terminal::CommandSessionId("cmd-x".into()), cx);
        assert_eq!(
            view.status,
            t!("composer.task.no_agent_to_hand_back").to_string()
        );
    });
    vcx.update(|window, cx| {
        window.refresh();
        let _ = window.draw(cx);
    });
}

fn sample_permission_profile() -> serde_json::Value {
    serde_json::json!({
        "network": {"enabled": true},
        "fileSystem": {"write": ["/proj", "/shared"]}
    })
}

fn show_permission_approval(
    composer: &gpui::Entity<ComposerView>,
    vcx: &mut gpui::VisualTestContext,
    scope: &str,
) {
    let scope = scope.to_string();
    composer.update(vcx, |view, _| {
        let raw = serde_json::json!({
            "threadId": "th-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "reason": "needs access",
            "scope": scope,
            "permissions": sample_permission_profile(),
        });
        view.panel_height = 600.0;
        view.orchestrator.external_pending_approval = Some(ExternalPendingApproval::from_codex(
            "7".into(),
            "item-1".into(),
            PERMISSIONS_APPROVAL_METHOD.into(),
            &raw,
        ));
    });
    draw(vcx);
}

fn click_id(vcx: &mut gpui::VisualTestContext, id: &'static str) {
    let bounds = vcx
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("missing element {id}"));
    vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
}

fn click_permission(vcx: &mut gpui::VisualTestContext, index: usize) {
    match index {
        0 => click_id(vcx, "permission-choice-0"),
        1 => click_id(vcx, "permission-choice-1"),
        2 => click_id(vcx, "permission-choice-2"),
        other => panic!("unexpected permission choice {other}"),
    }
}

/// 三项权限默认全选；批准回传完整 profile，scope 固定为 turn。
#[test]
fn permission_card_approve_all_sends_every_checked_permission() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    show_permission_approval(&composer, vcx, "turn");
    assert!(vcx.debug_bounds("permission-scope-turn").is_some());
    assert!(vcx.debug_bounds("permission-choice-0").is_some());
    assert!(vcx.debug_bounds("permission-choice-1").is_some());
    assert!(vcx.debug_bounds("permission-choice-2").is_some());
    let expected_ids = termior_agent_host::permission_entries(&sample_permission_profile())
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    composer.update(vcx, |view, _| {
        let pending = view
            .orchestrator
            .external_pending_approval
            .as_ref()
            .unwrap();
        assert!(pending.permissions.iter().all(|choice| choice.checked));
        assert_eq!(
            pending
                .permissions
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            expected_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_eq!(
            pending
                .permissions
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fileSystem.write[0]  /proj",
                "fileSystem.write[1]  /shared",
                "network  {\"enabled\":true}",
            ]
        );
    });
    click_id(vcx, "approve-tool");
    composer.update(vcx, |view, _| {
        let reply = view.orchestrator.last_external_approval.as_ref().unwrap();
        assert!(reply.approved);
        assert_eq!(
            reply.granted_permission_ids.as_deref(),
            Some(expected_ids.as_slice())
        );
        assert_eq!(
            termior_agent_host::permissions_approval_result(
                &sample_permission_profile(),
                true,
                reply.granted_permission_ids.as_deref(),
            ),
            serde_json::json!({
                "permissions": sample_permission_profile(),
                "scope": "turn"
            })
        );
    });
}

/// 取消一项后，批准只回传仍勾选的权限，服务端提议的 session 也不会被放大。
#[test]
fn permission_card_partial_approve_sends_only_checked_permissions() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    show_permission_approval(&composer, vcx, "session");
    assert!(vcx.debug_bounds("permission-scope-session").is_some());
    let shared_index = composer.update(vcx, |view, _| {
        view.orchestrator
            .external_pending_approval
            .as_ref()
            .unwrap()
            .permissions
            .iter()
            .position(|choice| choice.id == "fileSystem.write[1]")
            .unwrap()
    });
    click_permission(vcx, shared_index);
    composer.update(vcx, |view, _| {
        let pending = view
            .orchestrator
            .external_pending_approval
            .as_ref()
            .unwrap();
        assert!(!pending.permissions[shared_index].checked);
        assert_eq!(
            pending
                .permissions
                .iter()
                .filter(|choice| choice.checked)
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            vec!["fileSystem.write[0]", "network"]
        );
    });
    draw(vcx);
    click_id(vcx, "approve-tool");
    composer.update(vcx, |view, _| {
        let reply = view.orchestrator.last_external_approval.as_ref().unwrap();
        assert!(reply.approved);
        assert_eq!(
            reply.granted_permission_ids.as_deref(),
            Some(["fileSystem.write[0]".to_string(), "network".to_string(),].as_slice())
        );
        assert_eq!(
            termior_agent_host::permissions_approval_result(
                &sample_permission_profile(),
                true,
                reply.granted_permission_ids.as_deref(),
            ),
            serde_json::json!({
                "permissions": {
                    "network": {"enabled": true},
                    "fileSystem": {"write": ["/proj"]}
                },
                "scope": "turn"
            })
        );
    });
}

/// 拒绝忽略勾选，回传空权限集。
#[test]
fn permission_card_deny_sends_an_empty_permission_set() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    show_permission_approval(&composer, vcx, "session");
    click_id(vcx, "reject-tool");
    composer.update(vcx, |view, _| {
        let reply = view.orchestrator.last_external_approval.as_ref().unwrap();
        assert!(!reply.approved);
        assert_eq!(reply.granted_permission_ids.as_deref(), Some([].as_slice()));
        assert_eq!(
            termior_agent_host::permissions_approval_result(
                &sample_permission_profile(),
                false,
                reply.granted_permission_ids.as_deref(),
            ),
            serde_json::json!({"permissions": {}, "scope": "turn"})
        );
    });
}

/// 未知 scope 仍显示在卡片上，批准后的授权范围仍是 turn。
#[test]
fn permission_card_shows_unknown_scope_and_grants_for_the_turn() {
    let mut cx = TestAppContext::single();
    let (composer, vcx) = cx.add_window_view(|_, cx| ComposerView::new(cx));
    show_permission_approval(&composer, vcx, "something-new");
    assert!(vcx.debug_bounds("permission-scope-something-new").is_some());
    composer.update(vcx, |view, _| {
        assert_eq!(
            view.orchestrator
                .external_pending_approval
                .as_ref()
                .unwrap()
                .scope,
            "something-new"
        );
    });
    click_id(vcx, "approve-tool");
    composer.update(vcx, |view, _| {
        assert!(view.orchestrator.external_pending_approval.is_none());
        let reply = view.orchestrator.last_external_approval.as_ref().unwrap();
        assert!(reply.approved);
        let result = termior_agent_host::permissions_approval_result(
            &sample_permission_profile(),
            reply.approved,
            reply.granted_permission_ids.as_deref(),
        );
        assert_eq!(
            result.get("scope").and_then(serde_json::Value::as_str),
            Some("turn")
        );
    });
}
