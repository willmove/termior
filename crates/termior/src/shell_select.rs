//! Shell 选择的共享逻辑：设置页 Terminal 区下拉与新建终端选择器共用的
//! 条目标注、设置映射与 spawn 覆盖转换。
//!
//! 机器可读的探测结果（[`termior_terminal::DiscoveredShell`]）在
//! `termior-terminal`；这里只做 UI 侧的包装：本地化名称、选中态推导、
//! 写回 [`termior_store::Settings`]。

use gpui::SharedString;
use termior_store::{Settings, ShellDetection};
use termior_terminal::{shell_kind_for_program, DiscoveredShell, ShellKind};

/// 下拉/选择器中的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOption {
    /// 系统默认：`shell_detection = auto`（按平台探测）。
    Default,
    /// 探测到的本机 shell。
    Native { program: String },
    /// WSL 发行版（Windows）。
    Wsl { distribution: String },
    /// 手动路径（仅设置页下拉；路径本体由 Manual 路径输入行编辑）。
    Manual,
}

impl From<&DiscoveredShell> for ShellOption {
    fn from(shell: &DiscoveredShell) -> Self {
        match shell {
            DiscoveredShell::Native { program } => ShellOption::Native {
                program: program.clone(),
            },
            DiscoveredShell::Wsl { distribution } => ShellOption::Wsl {
                distribution: distribution.clone(),
            },
        }
    }
}

impl ShellOption {
    /// 转成 [`crate::workspace_view`] spawn 用的覆盖值；`None` 表示用设置默认。
    pub fn spawn_override(&self) -> Option<DiscoveredShell> {
        match self {
            ShellOption::Default | ShellOption::Manual => None,
            ShellOption::Native { program } => Some(DiscoveredShell::Native {
                program: program.clone(),
            }),
            ShellOption::Wsl { distribution } => Some(DiscoveredShell::Wsl {
                distribution: distribution.clone(),
            }),
        }
    }

    /// 菜单/下拉条目的本地化名称。
    pub fn label(&self) -> SharedString {
        match self {
            ShellOption::Default => t!("settings.terminal.shell.auto"),
            ShellOption::Native { program } => native_shell_label(program),
            ShellOption::Wsl { distribution } => {
                tf!("shell.name.wsl", "name" => distribution.clone())
            }
            ShellOption::Manual => t!("settings.terminal.shell.manual"),
        }
    }
}

/// 本机 shell 的展示名：按类型映射本地化键；路径含 `git` 的 bash 标注为
/// Git Bash；未知程序退化到文件名本身（如 nu、elvish）。
pub fn native_shell_label(program: &str) -> SharedString {
    match shell_kind_for_program(program) {
        Some(ShellKind::Pwsh) => t!("shell.name.pwsh"),
        Some(ShellKind::PowerShell) => t!("shell.name.powershell"),
        Some(ShellKind::Cmd) => t!("shell.name.cmd"),
        Some(ShellKind::Bash) => {
            let lower = program.to_ascii_lowercase();
            if lower.contains(r"\git\") || lower.contains("/git/") {
                t!("shell.name.git_bash")
            } else {
                t!("shell.name.bash")
            }
        }
        Some(ShellKind::Zsh) => t!("shell.name.zsh"),
        Some(ShellKind::Fish) => t!("shell.name.fish"),
        None => SharedString::from(
            program
                .rsplit(['\\', '/'])
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(program),
        ),
    }
}

/// 当前设置对应的下拉选中项。`discovered` 用于区分「探测到的 shell」与
/// 「手动路径」：Manual 路径与某个探测结果一致（忽略大小写）时按 Native 勾选。
pub fn selected_option(settings: &Settings, discovered: &[DiscoveredShell]) -> ShellOption {
    if let Some(distribution) = settings.wsl_distribution.clone() {
        return ShellOption::Wsl { distribution };
    }
    match &settings.terminal.shell_detection {
        ShellDetection::Auto => ShellOption::Default,
        ShellDetection::Manual { path } => {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                if let Some(shell) = discovered
                    .iter()
                    .find(|shell| matches_discovered(trimmed, shell))
                {
                    return ShellOption::from(shell);
                }
            }
            ShellOption::Manual
        }
    }
}

/// 路径与探测条目是否指向同一程序（大小写不敏感的字符串等值；探测结果
/// 存的就是解析后的绝对路径）。
fn matches_discovered(path: &str, shell: &DiscoveredShell) -> bool {
    match shell {
        DiscoveredShell::Native { program } => program.eq_ignore_ascii_case(path),
        DiscoveredShell::Wsl { .. } => false,
    }
}

/// 把选择写回设置。Native 覆盖 `shell_detection` 并清空 `wsl_distribution`；
/// WSL 反之（PTY 在 WSL 模式下默认 Bash，`shell_detection` 复位 auto）。
pub fn apply_option(option: &ShellOption, settings: &mut Settings) {
    match option {
        ShellOption::Default => {
            settings.terminal.shell_detection = ShellDetection::Auto;
            settings.wsl_distribution = None;
        }
        ShellOption::Native { program } => {
            settings.terminal.shell_detection = ShellDetection::Manual {
                path: program.clone(),
            };
            settings.wsl_distribution = None;
        }
        ShellOption::Wsl { distribution } => {
            settings.terminal.shell_detection = ShellDetection::Auto;
            settings.wsl_distribution = Some(distribution.clone());
        }
        ShellOption::Manual => {
            // 保留已有的手动路径；从其他选项切过来时给空路径，等输入行填写。
            if !matches!(
                settings.terminal.shell_detection,
                ShellDetection::Manual { .. }
            ) {
                settings.terminal.shell_detection = ShellDetection::Manual {
                    path: String::new(),
                };
            }
            settings.wsl_distribution = None;
        }
    }
}

/// 下拉按钮上显示的当前值：手动路径直接显示路径，其余显示条目名。
pub fn selection_value(option: &ShellOption, settings: &Settings) -> SharedString {
    match option {
        ShellOption::Manual => match &settings.terminal.shell_detection {
            ShellDetection::Manual { path } if !path.trim().is_empty() => {
                SharedString::from(path.clone())
            }
            _ => t!("settings.terminal.shell.manual"),
        },
        other => other.label(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(detection: ShellDetection, wsl: Option<&str>) -> Settings {
        let mut settings = termior_store::default_settings();
        settings.terminal.shell_detection = detection;
        settings.wsl_distribution = wsl.map(str::to_string);
        settings
    }

    #[test]
    fn selected_option_prefers_wsl_then_detection() {
        let discovered = vec![DiscoveredShell::Native {
            program: r"C:\Program Files\Git\bin\bash.exe".into(),
        }];
        assert_eq!(
            selected_option(
                &settings_with(ShellDetection::Auto, Some("Ubuntu")),
                &discovered
            ),
            ShellOption::Wsl {
                distribution: "Ubuntu".into()
            }
        );
        assert_eq!(
            selected_option(&settings_with(ShellDetection::Auto, None), &discovered),
            ShellOption::Default
        );
        // 路径与探测结果一致（忽略大小写）→ Native 勾选。
        assert_eq!(
            selected_option(
                &settings_with(
                    ShellDetection::Manual {
                        path: r"c:\program files\git\bin\bash.exe".into()
                    },
                    None
                ),
                &discovered
            ),
            ShellOption::Native {
                program: r"C:\Program Files\Git\bin\bash.exe".into()
            }
        );
        // 任意其他路径 → Manual。
        assert_eq!(
            selected_option(
                &settings_with(
                    ShellDetection::Manual {
                        path: r"C:\tools\nu.exe".into()
                    },
                    None
                ),
                &discovered
            ),
            ShellOption::Manual
        );
    }

    #[test]
    fn apply_option_round_trips_native_and_wsl() {
        let mut settings = termior_store::default_settings();
        apply_option(
            &ShellOption::Native {
                program: r"C:\Program Files\Git\bin\bash.exe".into(),
            },
            &mut settings,
        );
        assert_eq!(
            settings.terminal.shell_detection,
            ShellDetection::Manual {
                path: r"C:\Program Files\Git\bin\bash.exe".into()
            }
        );
        assert_eq!(settings.wsl_distribution, None);

        apply_option(
            &ShellOption::Wsl {
                distribution: "Debian".into(),
            },
            &mut settings,
        );
        assert_eq!(settings.terminal.shell_detection, ShellDetection::Auto);
        assert_eq!(settings.wsl_distribution.as_deref(), Some("Debian"));

        apply_option(&ShellOption::Default, &mut settings);
        assert_eq!(settings.terminal.shell_detection, ShellDetection::Auto);
        assert_eq!(settings.wsl_distribution, None);
    }

    #[test]
    fn apply_option_manual_keeps_existing_path() {
        let mut settings = termior_store::default_settings();
        apply_option(&ShellOption::Manual, &mut settings);
        assert_eq!(
            settings.terminal.shell_detection,
            ShellDetection::Manual {
                path: String::new()
            }
        );
        settings.terminal.shell_detection = ShellDetection::Manual {
            path: r"C:\sh\nu.exe".into(),
        };
        apply_option(&ShellOption::Manual, &mut settings);
        assert_eq!(
            settings.terminal.shell_detection,
            ShellDetection::Manual {
                path: r"C:\sh\nu.exe".into()
            }
        );
    }

    #[test]
    fn spawn_override_maps_only_concrete_shells() {
        assert_eq!(ShellOption::Default.spawn_override(), None);
        assert_eq!(ShellOption::Manual.spawn_override(), None);
        assert_eq!(
            ShellOption::Native {
                program: "pwsh".into()
            }
            .spawn_override(),
            Some(DiscoveredShell::Native {
                program: "pwsh".into()
            })
        );
        assert_eq!(
            ShellOption::Wsl {
                distribution: "Ubuntu".into()
            }
            .spawn_override(),
            Some(DiscoveredShell::Wsl {
                distribution: "Ubuntu".into()
            })
        );
    }

    #[test]
    fn native_label_marks_git_bash() {
        assert_eq!(
            native_shell_label(r"C:\Program Files\Git\bin\bash.exe"),
            t!("shell.name.git_bash")
        );
        assert_eq!(native_shell_label(r"C:\cygwin\bin\bash.exe"), t!("shell.name.bash"));
        // 未知程序退化到文件名。
        assert_eq!(native_shell_label(r"C:\tools\nu.exe"), "nu.exe");
    }
}
