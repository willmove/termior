//! 默认键位表（附录A）+ Win/Linux Ctrl 映射（FR-SET-03）。
//!
//! P0 提供固定默认键位（FR-SET-02 重绑定为 P1）。macOS 使用 Cmd，Win/Linux 映射为 Ctrl。

use serde::{Deserialize, Serialize};

/// 当前平台（决定修饰键映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Mac,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// 一个用户可触发的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyAction {
    NewTerminalTab,
    NewPrivateTerminal,
    NewEditorTab,
    NewPreviewTab,
    ClosePaneOrTab,
    GotoTab1,
    CycleTabs,
    CycleTabsReverse,
    SplitRight,
    SplitDown,
    FocusPanePrev,
    FocusPaneNext,
    InlineSearch,
    ToggleSidebar,
    FocusExplorer,
    FileFinder,
    SourceControlPanel,
    ToggleComposer,
    AskAiAboutSelection,
    CommitStaged,
    OpenSettings,
    Undo,
    Redo,
}

/// 一个按键绑定（逻辑描述，不含平台修饰差异）。
///
/// 注意：`key` 用 `&'static str` 因为默认键位表是编译期常量。
/// FR-SET-02（P1，可重绑定）将引入基于 `String` 的用户键位存储。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyBinding {
    /// 是否以「主修饰键」（Mac=Cmd / Win/Linux=Ctrl）触发。
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: &'static str,
}

impl KeyBinding {
    /// 用主修饰键 + key 构造（FR-SET-03 最常见形态）。
    pub const fn primary(key: &'static str) -> Self {
        Self {
            primary: true,
            shift: false,
            alt: false,
            key,
        }
    }
    pub const fn primary_shift(key: &'static str) -> Self {
        Self {
            primary: true,
            shift: true,
            alt: false,
            key,
        }
    }
    pub const fn ctrl_only(key: &'static str) -> Self {
        Self {
            primary: false,
            shift: false,
            alt: false,
            key,
        }
    }
    pub const fn ctrl_shift(key: &'static str) -> Self {
        Self {
            primary: false,
            shift: true,
            alt: false,
            key,
        }
    }

    /// 渲染为平台显示串（FR-SET-03）。Mac 用 `⌘`，Win/Linux 用 `Ctrl`。
    ///
    /// - `primary==true`：Mac 显示 `⌘`，Win/Linux 显示 `Ctrl`（绝大多数动作）。
    /// - `primary==false`：表示字面 Ctrl（如 `Ctrl+Tab` 循环 tab，跨平台一致）。
    pub fn display(&self, platform: Platform) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.primary {
            parts.push(match platform {
                Platform::Mac => "⌘",
                _ => "Ctrl",
            });
        } else {
            // 非主修饰键绑定按字面 Ctrl 渲染（Ctrl+Tab / Ctrl+Shift+Tab）。
            parts.push("Ctrl");
        }
        if self.shift {
            parts.push(match platform {
                Platform::Mac => "⇧",
                _ => "Shift",
            });
        }
        if self.alt {
            parts.push(match platform {
                Platform::Mac => "⌥",
                _ => "Alt",
            });
        }
        parts.push(self.key);
        let sep = if matches!(platform, Platform::Mac) {
            ""
        } else {
            "+"
        };
        parts.join(sep)
    }
}

/// 一条 keymap 条目：动作 → 绑定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeymapEntry {
    pub action: KeyAction,
    pub binding: KeyBinding,
}

/// 默认键位表（附录A）。
pub fn default_keymap() -> Vec<KeymapEntry> {
    use KeyAction::*;
    vec![
        KeymapEntry {
            action: NewTerminalTab,
            binding: KeyBinding::primary("T"),
        },
        KeymapEntry {
            action: NewPrivateTerminal,
            binding: KeyBinding::primary("R"),
        },
        KeymapEntry {
            action: NewEditorTab,
            binding: KeyBinding::primary("E"),
        },
        KeymapEntry {
            action: NewPreviewTab,
            binding: KeyBinding::primary("P"),
        },
        KeymapEntry {
            action: ClosePaneOrTab,
            binding: KeyBinding::primary("W"),
        },
        KeymapEntry {
            action: GotoTab1,
            binding: KeyBinding::primary("1"),
        },
        KeymapEntry {
            action: CycleTabs,
            binding: KeyBinding::ctrl_only("Tab"),
        },
        KeymapEntry {
            action: CycleTabsReverse,
            binding: KeyBinding::ctrl_shift("Tab"),
        },
        KeymapEntry {
            action: SplitRight,
            binding: KeyBinding::primary("D"),
        },
        KeymapEntry {
            action: SplitDown,
            binding: KeyBinding::primary_shift("D"),
        },
        KeymapEntry {
            action: FocusPanePrev,
            binding: KeyBinding::primary("["),
        },
        KeymapEntry {
            action: FocusPaneNext,
            binding: KeyBinding::primary("]"),
        },
        KeymapEntry {
            action: InlineSearch,
            binding: KeyBinding::primary("F"),
        },
        KeymapEntry {
            action: ToggleSidebar,
            binding: KeyBinding::primary("B"),
        },
        KeymapEntry {
            action: FocusExplorer,
            binding: KeyBinding::primary_shift("E"),
        },
        KeymapEntry {
            action: FileFinder,
            binding: KeyBinding::primary_shift("F"),
        },
        KeymapEntry {
            action: SourceControlPanel,
            binding: KeyBinding::primary("G"),
        },
        KeymapEntry {
            action: ToggleComposer,
            binding: KeyBinding::primary("I"),
        },
        KeymapEntry {
            action: AskAiAboutSelection,
            binding: KeyBinding::primary("L"),
        },
        KeymapEntry {
            action: CommitStaged,
            binding: KeyBinding {
                primary: true,
                shift: true,
                alt: false,
                key: "Enter",
            },
        },
        KeymapEntry {
            action: OpenSettings,
            binding: KeyBinding::primary(","),
        },
        KeymapEntry {
            action: Undo,
            binding: KeyBinding::primary("Z"),
        },
        KeymapEntry {
            action: Redo,
            binding: KeyBinding::primary("Y"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_covers_core_actions() {
        let km = default_keymap();
        let actions: Vec<_> = km.iter().map(|e| e.action).collect();
        for expected in [
            KeyAction::NewTerminalTab,
            KeyAction::NewEditorTab,
            KeyAction::ClosePaneOrTab,
            KeyAction::SplitRight,
            KeyAction::InlineSearch,
            KeyAction::ToggleSidebar,
            KeyAction::FileFinder,
            KeyAction::ToggleComposer,
            KeyAction::OpenSettings,
            KeyAction::Undo,
            KeyAction::Redo,
        ] {
            assert!(actions.contains(&expected), "missing {expected:?}");
        }
    }

    #[test]
    fn primary_binding_displays_cmd_on_mac_ctrl_on_win() {
        let b = KeyBinding::primary("T");
        assert_eq!(b.display(Platform::Mac), "⌘T");
        assert_eq!(b.display(Platform::Windows), "Ctrl+T");
        assert_eq!(b.display(Platform::Linux), "Ctrl+T");
    }

    #[test]
    fn primary_shift_binding() {
        let b = KeyBinding::primary_shift("D");
        assert_eq!(b.display(Platform::Mac), "⌘⇧D");
        assert_eq!(b.display(Platform::Windows), "Ctrl+Shift+D");
    }

    #[test]
    fn ctrl_tab_binding_no_primary() {
        // Ctrl+Tab 是跨平台字面 Ctrl 绑定（即使在 Mac 上也是 Ctrl 而非 Cmd）
        let b = KeyBinding::ctrl_only("Tab");
        assert_eq!(b.display(Platform::Mac), "CtrlTab");
        assert_eq!(b.display(Platform::Windows), "Ctrl+Tab");
        assert_eq!(b.display(Platform::Linux), "Ctrl+Tab");
    }

    #[test]
    fn settings_key_uses_comma() {
        let km = default_keymap();
        let entry = km
            .iter()
            .find(|e| e.action == KeyAction::OpenSettings)
            .unwrap();
        assert_eq!(entry.binding.key, ",");
    }

    #[test]
    fn platform_current_matches_cfg() {
        let p = Platform::current();
        if cfg!(target_os = "windows") {
            assert_eq!(p, Platform::Windows);
        } else if cfg!(target_os = "macos") {
            assert_eq!(p, Platform::Mac);
        } else {
            assert_eq!(p, Platform::Linux);
        }
    }
}
