//! Composer `/` 命令面板（FR-AGENT-05）：不离开 Composer 执行应用内动作。
//!
//! 本模块只负责命令目录、触发判定与排序，不依赖 GPUI；执行由 UI 层按
//! [`SlashAction`] 分派——Composer 自身动作就地处理，应用级动作复用快捷键
//! 系统的 [`KeyAction`]，与键位走同一条执行路径。

use termior_store::KeyAction;

/// 只作用于 Composer 自身状态的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerCommand {
    NewSession,
    ModeAuto,
    ModePlan,
    ModeYolo,
    CycleAgent,
    Stop,
    AttachFile,
    ToggleDock,
    ContextInspector,
    Skills,
    Memory,
    Recovery,
    Checkpoints,
    Automations,
    /// `/snippet <handle> <text>`：保存 `#handle` 片段（FR-AGENT-04）。
    SaveSnippet,
    Snippets,
    /// 应用内 TODO 面板（FR-SESS-04）。
    Todos,
}

/// 一条命令被选中后要执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashAction {
    Composer(ComposerCommand),
    App(KeyAction),
}

/// 命令目录中的一项。`description_key` 为 i18n 键，由 UI 层翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub action: SlashAction,
    pub description_key: &'static str,
    /// 需要参数的命令：面板选中后只补全为 `/name `，由用户继续输入参数再提交。
    pub takes_args: bool,
}

const fn composer(
    name: &'static str,
    command: ComposerCommand,
    description_key: &'static str,
) -> SlashCommand {
    SlashCommand {
        name,
        action: SlashAction::Composer(command),
        description_key,
        takes_args: false,
    }
}

const fn composer_with_args(
    name: &'static str,
    command: ComposerCommand,
    description_key: &'static str,
) -> SlashCommand {
    SlashCommand {
        takes_args: true,
        ..composer(name, command, description_key)
    }
}

const fn app(name: &'static str, action: KeyAction, description_key: &'static str) -> SlashCommand {
    SlashCommand {
        name,
        action: SlashAction::App(action),
        description_key,
        takes_args: false,
    }
}

/// 命令目录；空查询时按此顺序展示（Composer 动作在前）。
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    composer("new", ComposerCommand::NewSession, "slash.new"),
    composer("auto", ComposerCommand::ModeAuto, "slash.auto"),
    composer("plan", ComposerCommand::ModePlan, "slash.plan"),
    composer("yolo", ComposerCommand::ModeYolo, "slash.yolo"),
    composer("agent", ComposerCommand::CycleAgent, "slash.agent"),
    composer("stop", ComposerCommand::Stop, "slash.stop"),
    composer("attach", ComposerCommand::AttachFile, "slash.attach"),
    composer("dock", ComposerCommand::ToggleDock, "slash.dock"),
    composer(
        "context",
        ComposerCommand::ContextInspector,
        "slash.context",
    ),
    composer("skills", ComposerCommand::Skills, "slash.skills"),
    composer("memory", ComposerCommand::Memory, "slash.memory"),
    composer("recovery", ComposerCommand::Recovery, "slash.recovery"),
    composer(
        "checkpoints",
        ComposerCommand::Checkpoints,
        "slash.checkpoints",
    ),
    composer(
        "automations",
        ComposerCommand::Automations,
        "slash.automations",
    ),
    composer_with_args("snippet", ComposerCommand::SaveSnippet, "slash.snippet"),
    composer("snippets", ComposerCommand::Snippets, "slash.snippets"),
    composer("todos", ComposerCommand::Todos, "slash.todos"),
    app("terminal", KeyAction::NewTerminalTab, "slash.terminal"),
    app(
        "private-terminal",
        KeyAction::NewPrivateTerminal,
        "slash.private_terminal",
    ),
    app("editor", KeyAction::NewEditorTab, "slash.editor"),
    app("preview", KeyAction::NewPreviewTab, "slash.preview"),
    app("split-right", KeyAction::SplitRight, "slash.split_right"),
    app("split-down", KeyAction::SplitDown, "slash.split_down"),
    app("close", KeyAction::ClosePaneOrTab, "slash.close"),
    app("sidebar", KeyAction::ToggleSidebar, "slash.sidebar"),
    app("explorer", KeyAction::FocusExplorer, "slash.explorer"),
    app("find", KeyAction::FileFinder, "slash.find"),
    app("search", KeyAction::InlineSearch, "slash.search"),
    app("git", KeyAction::SourceControlPanel, "slash.git"),
    app("commit", KeyAction::CommitStaged, "slash.commit"),
    app("settings", KeyAction::OpenSettings, "slash.settings"),
    app("hide", KeyAction::ToggleComposer, "slash.hide"),
];

/// 光标前的输入若整体是一个 `/xxx` 词元，返回 `xxx`（可为空）。
///
/// 只在输入开头触发，且词元内不含空白：`/plan` 触发，`look at /usr/bin`
/// 与 `/plan now` 均不触发，避免把正文里的路径误当命令。
pub fn slash_query(input: &str, cursor: usize) -> Option<&str> {
    let end = input
        .char_indices()
        .nth(cursor)
        .map_or(input.len(), |(byte, _)| byte);
    let query = input[..end].strip_prefix('/')?;
    (!query.chars().any(char::is_whitespace)).then_some(query)
}

/// 按查询筛选并排序命令：名称前缀 > 名称子串 > 子序列；同档保持目录顺序。
pub fn match_commands(query: &str) -> Vec<&'static SlashCommand> {
    let query = query.to_ascii_lowercase();
    let mut hits: Vec<(u8, &'static SlashCommand)> = SLASH_COMMANDS
        .iter()
        .filter_map(|command| rank(&query, command.name).map(|score| (score, command)))
        .collect();
    hits.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    hits.into_iter().map(|(_, command)| command).collect()
}

/// 解析提交时的整条输入 `/name args`。
///
/// 无参命令只在没有参数时命中：`/new` 执行命令，而 `/new approach please`
/// 仍作为普通消息发送。
pub fn parse_invocation(input: &str) -> Option<(&'static SlashCommand, &str)> {
    let rest = input.trim().strip_prefix('/')?;
    let (name, args) = rest
        .split_once(char::is_whitespace)
        .map_or((rest, ""), |(name, args)| (name, args.trim()));
    let command = find_command(name)?;
    (command.takes_args || args.is_empty()).then_some((command, args))
}

/// 精确按名称查找（不区分大小写）。
pub fn find_command(name: &str) -> Option<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))
}

fn rank(query: &str, name: &str) -> Option<u8> {
    if query.is_empty() || name.starts_with(query) {
        return Some(3);
    }
    if name.contains(query) {
        return Some(2);
    }
    let mut remaining = name.chars();
    query
        .chars()
        .all(|wanted| remaining.any(|candidate| candidate == wanted))
        .then_some(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn query_requires_leading_slash_without_whitespace() {
        assert_eq!(slash_query("/", 1), Some(""));
        assert_eq!(slash_query("/pl", 3), Some("pl"));
        assert_eq!(slash_query("/plan now", 9), None);
        assert_eq!(slash_query("look at /usr", 12), None);
        assert_eq!(slash_query(" /plan", 6), None);
        assert_eq!(slash_query("", 0), None);
    }

    #[test]
    fn query_only_considers_text_before_cursor() {
        assert_eq!(slash_query("/plan later", 5), Some("plan"));
        assert_eq!(slash_query("/计划", 2), Some("计"));
        assert_eq!(slash_query("/p", 99), Some("p"));
    }

    #[test]
    fn empty_query_lists_catalogue_in_order() {
        let names: Vec<_> = match_commands("").iter().map(|c| c.name).collect();
        let catalogue: Vec<_> = SLASH_COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(names, catalogue);
    }

    #[test]
    fn prefix_beats_substring_beats_subsequence() {
        let names: Vec<_> = match_commands("s").iter().map(|c| c.name).collect();
        assert_eq!(names[0], "stop");
        let names: Vec<_> = match_commands("term").iter().map(|c| c.name).collect();
        assert_eq!(names, ["terminal", "private-terminal"]);
        let names: Vec<_> = match_commands("sr").iter().map(|c| c.name).collect();
        assert!(names.contains(&"split-right"));
        assert!(match_commands("zzz").is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(match_commands("PLAN")[0].name, "plan");
        assert_eq!(find_command("Plan").map(|c| c.name), Some("plan"));
        assert!(find_command("nope").is_none());
    }

    #[test]
    fn catalogue_names_are_unique_kebab_case() {
        let mut seen = HashSet::new();
        for command in SLASH_COMMANDS {
            assert!(seen.insert(command.name), "duplicate {}", command.name);
            assert!(command
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'));
            assert!(command.description_key.starts_with("slash."));
        }
    }

    #[test]
    fn invocation_parsing_respects_argument_arity() {
        let (command, args) = parse_invocation("/snippet fix  please fix it").unwrap();
        assert_eq!(command.name, "snippet");
        assert_eq!(args, "fix  please fix it");
        assert_eq!(
            parse_invocation(" /new ").map(|(c, a)| (c.name, a)),
            Some(("new", ""))
        );
        assert!(parse_invocation("/new approach please").is_none());
        assert!(parse_invocation("/unknown").is_none());
        assert!(parse_invocation("plain text").is_none());
    }
}
