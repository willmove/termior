//! 面向用户的界面文案。
//!
//! 所有按钮标签、空状态说明、Tooltip 文案集中在此，方便日后接入 i18n。
//! 业务逻辑错误信息（panic、日志）不在此列。
//!
//! 本模块无 GPUI 依赖，`--no-default-features` 下也可编译。

/// 活动栏。
pub mod activity {
    pub const EXPLORER: &str = "File Explorer";
    pub const SOURCE_CONTROL: &str = "Source Control";
    pub const GIT_HISTORY: &str = "Git History";
}

/// 窗口与标签。
pub mod chrome {
    pub const NEW_TERMINAL_TAB: &str = "New terminal tab";
    pub const CLOSE_TAB: &str = "Close tab";
    pub const SETTINGS: &str = "Settings";
    pub const AGENT_ACTIVITY: &str = "Agent activity";
    pub const MINIMIZE: &str = "Minimize";
    pub const MAXIMIZE: &str = "Maximize";
    pub const RESTORE: &str = "Restore";
    pub const CLOSE_WINDOW: &str = "Close";
}

/// 空状态。
pub mod empty {
    pub const NO_TABS: &str = "No tabs open";
    pub const NO_TABS_DETAIL: &str = "Press Ctrl/Cmd+T to open a terminal";
    pub const NO_VISIBLE_FILES: &str = "This workspace has no visible files";
    pub const NO_PROVIDER_PROFILES: &str = "No provider profiles";
    pub const NO_CUSTOM_AGENTS: &str = "No custom agents yet";
    pub const STARTING_TERMINAL: &str = "Starting terminal…";
    pub const STARTING_SPLIT_TERMINAL: &str = "Starting split terminal…";
    pub const SPLIT_VIEW: &str = "Split view";
    pub const PANE_UNAVAILABLE: &str = "Pane unavailable";
    pub const MARKDOWN_SOURCE_UNAVAILABLE: &str = "Markdown preview source is unavailable";
    pub const NO_GIT_HISTORY: &str = "No commits to show yet";
    pub const GIT_HISTORY_SIDEBAR: &str = "History is available in the sidebar";
    pub const DIFF_REOPEN_HINT: &str = "Reopen the source item to refresh this diff";
    pub const TERMINAL_FAILED: &str = "Could not start the terminal";
}

/// 文件浏览器操作（完整词，不用 +F / +D 这类缩写）。
pub mod explorer {
    pub const FIND: &str = "Find file";
    pub const SEARCH: &str = "Search in files";
    pub const NEW_FILE: &str = "New file";
    pub const NEW_DIRECTORY: &str = "New folder";
    pub const REFRESH: &str = "Refresh";
    pub const ROOT_INVALID: &str = "Workspace folder is missing or is not a directory";

    pub fn skipped_summary(count: usize) -> String {
        match count {
            0 => String::new(),
            1 => "1 item was skipped".into(),
            n => format!("{n} items were skipped"),
        }
    }
}

/// 源码管理面板。
pub mod git {
    pub const STAGE_ALL: &str = "Stage all";
    pub const COMMIT: &str = "Commit";
    pub const FETCH: &str = "Fetch";
    pub const PULL: &str = "Pull";
    pub const PUSH: &str = "Push";
    pub const NEW_BRANCH: &str = "New branch";
    pub const SWITCH_BRANCH: &str = "Switch branch";
    pub const OPEN_FULL_HISTORY: &str = "Open full history";
}

/// 通用操作。
pub mod action {
    pub const SEND: &str = "Send";
    pub const OPEN_IN_BROWSER: &str = "Open in browser";
    pub const SAVE: &str = "Save";
}
