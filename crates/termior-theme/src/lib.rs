//! `termior-theme` — 中央主题引擎（FR-THEME-01/02，P0）。
//!
//! 一份 Rust 主题 token 结构（语义色板）同时驱动 UI 组件、终端 16+ 色调色板、diff
//! 颜色、通知样式（FR-THEME-01）。浅色/深色/跟随系统三态独立于色板选择。
//!
//! 内置 10 套应用主题，并支持自定义主题 JSON 导入/导出。
//!
//! 切换时上层（GPUI Entity）广播重绘；本模块只提供主题数据与解析，不依赖 GPUI。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// 外观模式。`FollowSystem` 表示由运行时解析为 Light 或 Dark。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
    FollowSystem,
}

/// 语义色板 token（FR-THEME-01）。一个主题同时定义浅/深两套，按外观解析。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeTokens {
    pub light: Palette,
    pub dark: Palette,
}

/// 解析后的可用色板（不含三态，已按外观选择）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPalette {
    pub background: Color,
    pub foreground: Color,
    /// 三级 surface（最底/卡片/悬浮）。
    pub surface: [Color; 3],
    pub accent: Color,
    /// 四级状态色：info / success / warning / danger。
    pub status: [Color; 4],
    /// diff 颜色：added(+)/removed(-)/context。
    pub diff: [Color; 3],
    /// 终端 16 色调色板：black/red/green/yellow/blue/magenta/cyan/white × normal/bright。
    pub terminal: TerminalPalette,
}

/// 终端 16 色。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalPalette {
    pub black: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub white: Color,
    pub bright_black: Color,
    pub bright_red: Color,
    pub bright_green: Color,
    pub bright_yellow: Color,
    pub bright_blue: Color,
    pub bright_magenta: Color,
    pub bright_cyan: Color,
    pub bright_white: Color,
}

impl TerminalPalette {
    /// 16 色完备性：返回所有颜色，便于断言无遗漏。
    pub fn all(&self) -> [Color; 16] {
        [
            self.black,
            self.red,
            self.green,
            self.yellow,
            self.blue,
            self.magenta,
            self.cyan,
            self.white,
            self.bright_black,
            self.bright_red,
            self.bright_green,
            self.bright_yellow,
            self.bright_blue,
            self.bright_magenta,
            self.bright_cyan,
            self.bright_white,
        ]
    }
}

/// RGB 颜色（每通道 0-255）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
    /// 仅供 default/nord 用到的别名。
    pub const fn hex3(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as u8,
            g: ((rgb >> 8) & 0xff) as u8,
            b: (rgb & 0xff) as u8,
        }
    }
}

/// 一个已命名的应用主题（含浅/深 token）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub tokens: ThemeTokens,
}

impl Theme {
    /// 按 [`Appearance`] 解析出 [`ResolvedPalette`]（FR-THEME-01）。
    /// `FollowSystem` 在此用 `system_is_dark` 解析为具体值。
    pub fn resolve(&self, appearance: Appearance, system_is_dark: bool) -> ResolvedPalette {
        let palette = match appearance {
            Appearance::Light => &self.tokens.light,
            Appearance::Dark => &self.tokens.dark,
            Appearance::FollowSystem => {
                if system_is_dark {
                    &self.tokens.dark
                } else {
                    &self.tokens.light
                }
            }
        };
        palette.clone()
    }
}

/// 内置主题注册表（FR-THEME-02 完整十套）。
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        default_theme(),
        nord_theme(),
        styled_theme(
            "tide",
            "Tide",
            StyledColors::new(
                0x101820, 0x4fd1c5, 0xff6b6b, 0x68d391, 0xf6c453, 0x9f7aea, 0x63b3ed,
            ),
        ),
        styled_theme(
            "catppuccin",
            "Catppuccin",
            StyledColors::new(
                0x1e1e2e, 0xcba6f7, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0xf5c2e7, 0x89dceb,
            ),
        ),
        styled_theme(
            "tokyo-night",
            "Tokyo Night",
            StyledColors::new(
                0x1a1b26, 0x7aa2f7, 0xf7768e, 0x9ece6a, 0xe0af68, 0xbb9af7, 0x7dcfff,
            ),
        ),
        styled_theme(
            "caffeine",
            "Caffeine",
            StyledColors::new(
                0x201a17, 0xd08c60, 0xe05d44, 0xa3b18a, 0xd4a373, 0xb5838d, 0x84a59d,
            ),
        ),
        styled_theme(
            "claude",
            "Claude",
            StyledColors::new(
                0x26201d, 0xd97757, 0xc15f3c, 0x7f9b76, 0xe0a458, 0xac7b9b, 0x6f9e99,
            ),
        ),
        styled_theme(
            "gruvbox",
            "Gruvbox",
            StyledColors::new(
                0x282828, 0xd79921, 0xcc241d, 0x98971a, 0xd79921, 0xb16286, 0x689d6a,
            ),
        ),
        styled_theme(
            "sage",
            "Sage",
            StyledColors::new(
                0x17201b, 0x8fb996, 0xd47766, 0x8fb996, 0xd8b26e, 0xb19ac7, 0x78aaa1,
            ),
        ),
        styled_theme(
            "rose-pine",
            "Rose Pine",
            StyledColors::new(
                0x191724, 0xebbcba, 0xeb6f92, 0x9ccfd8, 0xf6c177, 0xc4a7e7, 0x9ccfd8,
            ),
        ),
    ]
}

pub fn find_theme(id: &str) -> Option<Theme> {
    builtin_themes().into_iter().find(|t| t.id == id)
}

/// `Termior-default` 主题。
pub fn default_theme() -> Theme {
    Theme {
        id: "default".into(),
        name: "Termior Default".into(),
        tokens: ThemeTokens {
            light: base_light(),
            dark: base_dark(),
        },
    }
}

/// `nord` 主题（基于 Nord 色板，自定义命名）。
pub fn nord_theme() -> Theme {
    // Nord 调色板
    let n = |rgb: u32| Color::hex3(rgb);
    Theme {
        id: "nord".into(),
        name: "Nord".into(),
        tokens: ThemeTokens {
            light: Palette {
                background: n(0xeceff4),
                foreground: n(0x2e3440),
                surface: [n(0xe5e9f0), n(0xd8dee9), n(0xeceff4)],
                accent: n(0x5e81ac),
                status: [n(0x5e81ac), n(0xa3be8c), n(0xebcb8b), n(0xbf616a)],
                diff: [n(0xa3be8c), n(0xbf616a), n(0x4c566a)],
                terminal: TerminalPalette {
                    black: n(0x2e3440),
                    red: n(0xbf616a),
                    green: n(0xa3be8c),
                    yellow: n(0xebcb8b),
                    blue: n(0x5e81ac),
                    magenta: n(0xb48ead),
                    cyan: n(0x88c0d0),
                    white: n(0xe5e9f0),
                    bright_black: n(0x4c566a),
                    bright_red: n(0xbf616a),
                    bright_green: n(0xa3be8c),
                    bright_yellow: n(0xebcb8b),
                    bright_blue: n(0x81a1c1),
                    bright_magenta: n(0xb48ead),
                    bright_cyan: n(0x8fbcbb),
                    bright_white: n(0xeceff4),
                },
            },
            dark: Palette {
                background: n(0x2e3440),
                foreground: n(0xd8dee9),
                surface: [n(0x2e3440), n(0x3b4252), n(0x434c5e)],
                accent: n(0x88c0d0),
                status: [n(0x81a1c1), n(0xa3be8c), n(0xebcb8b), n(0xbf616a)],
                diff: [n(0xa3be8c), n(0xbf616a), n(0x4c566a)],
                terminal: TerminalPalette {
                    black: n(0x3b4252),
                    red: n(0xbf616a),
                    green: n(0xa3be8c),
                    yellow: n(0xebcb8b),
                    blue: n(0x81a1c1),
                    magenta: n(0xb48ead),
                    cyan: n(0x88c0d0),
                    white: n(0xe5e9f0),
                    bright_black: n(0x4c566a),
                    bright_red: n(0xbf616a),
                    bright_green: n(0xa3be8c),
                    bright_yellow: n(0xebcb8b),
                    bright_blue: n(0x81a1c1),
                    bright_magenta: n(0xb48ead),
                    bright_cyan: n(0x8fbcbb),
                    bright_white: n(0xeceff4),
                },
            },
        },
    }
}

#[derive(Clone, Copy)]
struct StyledColors {
    background: u32,
    accent: u32,
    red: u32,
    green: u32,
    yellow: u32,
    magenta: u32,
    cyan: u32,
}

impl StyledColors {
    const fn new(
        background: u32,
        accent: u32,
        red: u32,
        green: u32,
        yellow: u32,
        magenta: u32,
        cyan: u32,
    ) -> Self {
        Self {
            background,
            accent,
            red,
            green,
            yellow,
            magenta,
            cyan,
        }
    }
}

fn styled_theme(id: &str, name: &str, colors: StyledColors) -> Theme {
    let white = Color::rgb(255, 255, 255);
    let black = Color::rgb(18, 18, 20);
    let background = Color::hex3(colors.background);
    let accent = Color::hex3(colors.accent);
    let red = Color::hex3(colors.red);
    let green = Color::hex3(colors.green);
    let yellow = Color::hex3(colors.yellow);
    let magenta = Color::hex3(colors.magenta);
    let cyan = Color::hex3(colors.cyan);

    // Keep the visual identity in both appearance modes. Previously the light
    // variants copied `base_light()` wholesale and changed only the accent,
    // so most themes rendered identical application chrome.
    let light_foreground = blend(background, black, 0.10);
    let light_accent = blend(accent, black, 0.18);
    let light = Palette {
        background: blend(background, white, 0.94),
        foreground: light_foreground,
        surface: [
            blend(background, white, 0.89),
            blend(background, white, 0.975),
            blend(background, white, 0.84),
        ],
        accent: light_accent,
        status: [
            light_accent,
            blend(green, black, 0.14),
            blend(yellow, black, 0.16),
            blend(red, black, 0.12),
        ],
        diff: [
            blend(green, white, 0.70),
            blend(red, white, 0.72),
            blend(background, white, 0.76),
        ],
        terminal: TerminalPalette {
            black: light_foreground,
            red: blend(red, black, 0.12),
            green: blend(green, black, 0.16),
            yellow: blend(yellow, black, 0.18),
            blue: light_accent,
            magenta: blend(magenta, black, 0.12),
            cyan: blend(cyan, black, 0.16),
            white: blend(background, white, 0.88),
            bright_black: blend(background, white, 0.42),
            bright_red: blend(red, white, 0.14),
            bright_green: blend(green, white, 0.10),
            bright_yellow: blend(yellow, white, 0.10),
            bright_blue: blend(accent, white, 0.10),
            bright_magenta: blend(magenta, white, 0.12),
            bright_cyan: blend(cyan, white, 0.10),
            bright_white: blend(background, white, 0.98),
        },
    };

    let dark_foreground = blend(background, white, 0.90);
    let dark = Palette {
        background,
        foreground: dark_foreground,
        surface: [
            background,
            blend(background, white, 0.08),
            blend(background, white, 0.14),
        ],
        accent,
        status: [accent, green, yellow, red],
        diff: [
            blend(green, background, 0.22),
            blend(red, background, 0.22),
            blend(dark_foreground, background, 0.58),
        ],
        terminal: TerminalPalette {
            black: blend(background, white, 0.10),
            red,
            green,
            yellow,
            blue: accent,
            magenta,
            cyan,
            white: blend(background, white, 0.84),
            bright_black: blend(background, white, 0.30),
            bright_red: blend(red, white, 0.18),
            bright_green: blend(green, white, 0.18),
            bright_yellow: blend(yellow, white, 0.16),
            bright_blue: blend(accent, white, 0.20),
            bright_magenta: blend(magenta, white, 0.18),
            bright_cyan: blend(cyan, white, 0.18),
            bright_white: blend(background, white, 0.96),
        },
    };
    Theme {
        id: id.into(),
        name: name.into(),
        tokens: ThemeTokens { light, dark },
    }
}

fn blend(a: Color, b: Color, amount: f32) -> Color {
    let channel =
        |left: u8, right: u8| (left as f32 * (1.0 - amount) + right as f32 * amount).round() as u8;
    Color::rgb(channel(a.r, b.r), channel(a.g, b.g), channel(a.b, b.b))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeLibrary {
    #[serde(default)]
    pub custom: Vec<Theme>,
}

impl ThemeLibrary {
    pub fn all(&self) -> Vec<Theme> {
        let mut themes = builtin_themes();
        themes.extend(self.custom.clone());
        themes
    }

    pub fn save_custom(&mut self, theme: Theme) -> Result<(), ThemeError> {
        if theme.id.trim().is_empty() {
            return Err(ThemeError::EmptyId);
        }
        if builtin_themes()
            .iter()
            .any(|builtin| builtin.id == theme.id)
        {
            return Err(ThemeError::BuiltinId(theme.id));
        }
        if let Some(existing) = self.custom.iter_mut().find(|item| item.id == theme.id) {
            *existing = theme;
        } else {
            self.custom.push(theme);
        }
        Ok(())
    }

    pub fn based_on(&self, source_id: &str, id: &str, name: &str) -> Option<Theme> {
        let mut theme = self.all().into_iter().find(|theme| theme.id == source_id)?;
        theme.id = id.to_owned();
        theme.name = name.to_owned();
        Some(theme)
    }

    pub fn export(theme: &Theme) -> Result<String, ThemeError> {
        Ok(serde_json::to_string_pretty(theme)?)
    }

    pub fn import(&mut self, json: &str) -> Result<Theme, ThemeError> {
        let theme: Theme = serde_json::from_str(json)?;
        self.save_custom(theme.clone())?;
        Ok(theme)
    }
}

/// Palette 别名（语义色板）。
pub type Palette = ResolvedPalette;

fn base_light() -> Palette {
    let c = |rgb: u32| Color::hex3(rgb);
    Palette {
        background: c(0xffffff),
        foreground: c(0x1f2328),
        surface: [c(0xf6f8fa), c(0xffffff), c(0xeaeef2)],
        accent: c(0x0969da),
        status: [c(0x0969da), c(0x1a7f37), c(0x9a6700), c(0xcf222e)],
        diff: [c(0x1a7f37), c(0xcf222e), c(0x6e7781)],
        terminal: TerminalPalette {
            black: c(0x24292f),
            red: c(0xcf222e),
            green: c(0x1a7f37),
            yellow: c(0x9a6700),
            blue: c(0x0969da),
            magenta: c(0x8250df),
            cyan: c(0x1b7c83),
            white: c(0x6e7781),
            bright_black: c(0x57606a),
            bright_red: c(0xa40e26),
            bright_green: c(0x2da44e),
            bright_yellow: c(0xbf8700),
            bright_blue: c(0x218bff),
            bright_magenta: c(0xa475f9),
            bright_cyan: c(0x3192aa),
            bright_white: c(0x8c959f),
        },
    }
}

fn base_dark() -> Palette {
    let c = |rgb: u32| Color::hex3(rgb);
    Palette {
        background: c(0x0d1117),
        foreground: c(0xe6edf3),
        surface: [c(0x010409), c(0x0d1117), c(0x161b22)],
        accent: c(0x2f81f7),
        status: [c(0x2f81f7), c(0x3fb950), c(0xd29922), c(0xf85149)],
        diff: [c(0x3fb950), c(0xf85149), c(0x8b949e)],
        terminal: TerminalPalette {
            black: c(0x484f58),
            red: c(0xff7b72),
            green: c(0x3fb950),
            yellow: c(0xd29922),
            blue: c(0x58a6ff),
            magenta: c(0xbc8cff),
            cyan: c(0x39c5cf),
            white: c(0xb1bac4),
            bright_black: c(0x6e7681),
            bright_red: c(0xffa198),
            bright_green: c(0x56d364),
            bright_yellow: c(0xe3b341),
            bright_blue: c(0x79c0ff),
            bright_magenta: c(0xd2a8ff),
            bright_cyan: c(0x56d4dd),
            bright_white: c(0xf0f6fc),
        },
    }
}

/// 加载/保存自定义主题（FR-THEME-04 的持久化格式基础；P1 完整 UI）。
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme not found: {0}")]
    NotFound(String),
    #[error("invalid theme json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("theme id is empty")]
    EmptyId,
    #[error("cannot replace a built-in theme: {0}")]
    BuiltinId(String),
}

/// 把主题序列化为 JSON（用于 `Termior-custom-themes.json`）。
pub fn to_json(theme: &Theme) -> Result<String, ThemeError> {
    Ok(serde_json::to_string_pretty(theme)?)
}

/// 从 JSON 反序列化主题。
pub fn from_json(json: &str) -> Result<Theme, ThemeError> {
    Ok(serde_json::from_str(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_has_ten() {
        let t = builtin_themes();
        assert_eq!(t.len(), 10, "FR-THEME-02: ships the full theme set");
        let ids: Vec<String> = t.iter().map(|x| x.id.clone()).collect();
        assert!(ids.iter().any(|x| x == "default"));
        assert!(ids.iter().any(|x| x == "nord"));
    }

    #[test]
    fn builtin_themes_have_distinct_chrome_palettes() {
        let themes = builtin_themes();
        for appearance in [Appearance::Light, Appearance::Dark] {
            for (index, theme) in themes.iter().enumerate() {
                let palette = theme.resolve(appearance, false);
                for other in themes.iter().skip(index + 1) {
                    let other_palette = other.resolve(appearance, false);
                    assert_ne!(
                        (palette.background, palette.foreground, palette.surface),
                        (
                            other_palette.background,
                            other_palette.foreground,
                            other_palette.surface
                        ),
                        "{} and {} render the same app chrome in {appearance:?} mode",
                        theme.id,
                        other.id
                    );
                }
            }
        }
    }

    #[test]
    fn find_theme_works() {
        assert!(find_theme("nord").is_some());
        assert!(find_theme("nope").is_none());
    }

    #[test]
    fn resolve_light_vs_dark_differ() {
        let t = default_theme();
        let light = t.resolve(Appearance::Light, true);
        let dark = t.resolve(Appearance::Dark, true);
        assert_ne!(light.background, dark.background);
    }

    #[test]
    fn follow_system_resolves_correctly() {
        let t = default_theme();
        let dark = t.resolve(Appearance::FollowSystem, true);
        let light = t.resolve(Appearance::FollowSystem, false);
        assert_eq!(dark.background, t.tokens.dark.background);
        assert_eq!(light.background, t.tokens.light.background);
    }

    #[test]
    fn terminal_palette_complete() {
        for t in builtin_themes() {
            for pal in [t.tokens.light.terminal.all(), t.tokens.dark.terminal.all()] {
                assert_eq!(pal.len(), 16);
                // 无全黑（避免漏配）
                for c in pal {
                    assert!((c.r, c.g, c.b) != (0, 0, 0), "all-black color in {}", t.id);
                }
            }
        }
    }

    #[test]
    fn diff_palette_has_three_colors() {
        for t in builtin_themes() {
            for pal in [&t.tokens.light, &t.tokens.dark] {
                assert_eq!(pal.diff.len(), 3);
            }
        }
    }

    #[test]
    fn status_palette_has_four_colors() {
        for t in builtin_themes() {
            for pal in [&t.tokens.light, &t.tokens.dark] {
                assert_eq!(pal.status.len(), 4);
            }
        }
    }

    #[test]
    fn theme_json_roundtrip() {
        let t = nord_theme();
        let json = to_json(&t).unwrap();
        let back = from_json(&json).unwrap();
        assert_eq!(back.id, t.id);
        assert_eq!(back.tokens.dark.background, t.tokens.dark.background);
    }

    #[test]
    fn invalid_json_rejected() {
        assert!(from_json("not json").is_err());
    }

    #[test]
    fn color_to_hex() {
        assert_eq!(Color::rgb(255, 0, 128).to_hex(), "#ff0080");
    }

    #[test]
    fn appearance_serializes_snake_case() {
        let json = serde_json::to_string(&Appearance::FollowSystem).unwrap();
        assert_eq!(json, "\"follow_system\"");
        let back: Appearance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Appearance::FollowSystem);
    }

    #[test]
    fn custom_theme_import_export_and_builtin_guard() {
        let library = ThemeLibrary::default();
        let custom = library.based_on("nord", "my-nord", "My Nord").unwrap();
        let json = ThemeLibrary::export(&custom).unwrap();
        let mut target = ThemeLibrary::default();
        let imported = target.import(&json).unwrap();
        assert_eq!(imported.id, "my-nord");
        assert_eq!(target.custom.len(), 1);
        assert!(matches!(
            target.save_custom(default_theme()),
            Err(ThemeError::BuiltinId(_))
        ));
    }
}
