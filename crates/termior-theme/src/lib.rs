//! `termior-theme` — 中央主题引擎（FR-THEME-01/02，P0）。
//!
//! 一份 Rust 主题 token 结构（语义色板）同时驱动 UI 组件、终端 16+ 色调色板、diff
//! 颜色、通知样式（FR-THEME-01）。每个主题声明原生外观归属；`FollowSystem` 在设置层
//! 通过浅/深主题配对实现，不再对同一主题做算法浅色推导。
//!
//! 内置主题含独立的浅色配对候选（default-light / nord-light），并支持自定义主题
//! JSON 导入/导出。
//!
//! 切换时上层（GPUI Entity）广播重绘；本模块只提供主题数据与解析，不依赖 GPUI。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// 外观模式（设置层）。`FollowSystem` 表示在浅/深主题配对之间切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
    FollowSystem,
}

/// 主题的原生外观归属（非三态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeAppearance {
    Light,
    Dark,
}

impl NativeAppearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn to_appearance(self) -> Appearance {
        match self {
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
        }
    }
}

/// 主题色板 token：每个主题只持有一套原生色板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeTokens {
    pub palette: Palette,
}

/// 解析后的可用色板。
///
/// 表面层级用语义字段，不再用下标数组——同一语义不得被无关用途复用。
/// 旧的 `surface: [a, b, c]` JSON 仍可导入，见 [`ResolvedPalette`] 的反序列化。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedPalette {
    pub background: Color,
    pub foreground: Color,
    /// 窗口 chrome：标题栏、状态栏。
    pub chrome: Color,
    /// 侧栏与活动栏底色。
    pub panel: Color,
    /// 面板内抬起面：输入框、卡片、列表选中外的次级底。
    pub elevated: Color,
    /// 浮层：菜单、Tooltip、toast。
    pub overlay: Color,
    pub accent: Color,
    /// 四级状态色：info / success / warning / danger。
    pub status: [Color; 4],
    /// diff 颜色：added(+)/removed(-)/context。
    pub diff: [Color; 3],
    /// 终端 16 色调色板：black/red/green/yellow/blue/magenta/cyan/white × normal/bright。
    pub terminal: TerminalPalette,
}

/// 反序列化中间态：同时接受新语义字段与旧 `surface` 数组。
#[derive(Debug, Deserialize)]
struct PaletteSerde {
    background: Color,
    foreground: Color,
    #[serde(default)]
    chrome: Option<Color>,
    #[serde(default)]
    panel: Option<Color>,
    #[serde(default)]
    elevated: Option<Color>,
    #[serde(default)]
    overlay: Option<Color>,
    /// 旧格式：`[panel, elevated, chrome]`。
    #[serde(default)]
    surface: Option<[Color; 3]>,
    accent: Color,
    status: [Color; 4],
    diff: [Color; 3],
    terminal: TerminalPalette,
}

impl From<PaletteSerde> for ResolvedPalette {
    fn from(value: PaletteSerde) -> Self {
        let (chrome, panel, elevated, overlay) = match (
            value.chrome,
            value.panel,
            value.elevated,
            value.overlay,
            value.surface,
        ) {
            (Some(chrome), Some(panel), Some(elevated), Some(overlay), _) => {
                (chrome, panel, elevated, overlay)
            }
            (Some(chrome), Some(panel), Some(elevated), None, _) => {
                (chrome, panel, elevated, chrome)
            }
            (_, _, _, _, Some([panel, elevated, chrome])) => (chrome, panel, elevated, chrome),
            _ => {
                // 缺字段时退回 background 派生，保证导入不崩；调用方应尽快写出新格式。
                let fallback = value.background;
                (
                    value.chrome.unwrap_or(fallback),
                    value.panel.unwrap_or(fallback),
                    value.elevated.unwrap_or(fallback),
                    value.overlay.unwrap_or(fallback),
                )
            }
        };
        Self {
            background: value.background,
            foreground: value.foreground,
            chrome,
            panel,
            elevated,
            overlay,
            accent: value.accent,
            status: value.status,
            diff: value.diff,
            terminal: value.terminal,
        }
    }
}

impl<'de> Deserialize<'de> for ResolvedPalette {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        PaletteSerde::deserialize(deserializer).map(Into::into)
    }
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

/// 相对亮度（sRGB 近似，0–1）。
pub fn luminance(c: Color) -> f32 {
    (0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32) / 255.0
}

/// 按背景亮度推断原生外观。
pub fn infer_native_appearance(palette: &Palette) -> NativeAppearance {
    if luminance(palette.background) >= 0.5 {
        NativeAppearance::Light
    } else {
        NativeAppearance::Dark
    }
}

/// 一个已命名的应用主题（单套原生色板 + 外观归属）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub native_appearance: NativeAppearance,
    pub tokens: ThemeTokens,
}

#[derive(Debug, Deserialize)]
struct ThemeSerde {
    id: String,
    name: String,
    #[serde(default)]
    native_appearance: Option<NativeAppearance>,
    tokens: ThemeTokensSerde,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ThemeTokensSerde {
    /// 新格式：`{ "palette": { ... } }`
    Single { palette: Palette },
    /// 旧格式：同时含 light/dark；按暗色底板取原生侧。
    Dual { light: Palette, dark: Palette },
}

impl From<ThemeSerde> for Theme {
    fn from(value: ThemeSerde) -> Self {
        let (palette, inferred) = match value.tokens {
            ThemeTokensSerde::Single { palette } => {
                let inferred = infer_native_appearance(&palette);
                (palette, inferred)
            }
            ThemeTokensSerde::Dual { light, dark } => {
                // 旧双色板：优先保留暗色侧（历史主题多为深色原生），若暗色其实偏亮则用浅色。
                if luminance(dark.background) < 0.5 {
                    (dark, NativeAppearance::Dark)
                } else {
                    (light, NativeAppearance::Light)
                }
            }
        };
        Self {
            id: value.id,
            name: value.name,
            native_appearance: value.native_appearance.unwrap_or(inferred),
            tokens: ThemeTokens { palette },
        }
    }
}

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ThemeSerde::deserialize(deserializer).map(Into::into)
    }
}

impl Theme {
    /// 主题的原生色板。
    pub fn palette(&self) -> &ResolvedPalette {
        &self.tokens.palette
    }

    /// 兼容旧调用：忽略 appearance，始终返回原生色板。
    pub fn resolve(&self, _appearance: Appearance, _system_is_dark: bool) -> ResolvedPalette {
        self.palette().clone()
    }
}

/// 按设置解析当前应使用的主题 id。
pub fn active_theme_id<'a>(
    appearance: Appearance,
    theme_id: &'a str,
    light_theme_id: &'a str,
    dark_theme_id: &'a str,
    system_is_dark: bool,
) -> &'a str {
    match appearance {
        Appearance::FollowSystem => {
            if system_is_dark {
                dark_theme_id
            } else {
                light_theme_id
            }
        }
        Appearance::Light | Appearance::Dark => theme_id,
    }
}

/// 在主题列表中解析当前色板。
pub fn resolve_active_palette(
    themes: &[Theme],
    appearance: Appearance,
    theme_id: &str,
    light_theme_id: &str,
    dark_theme_id: &str,
    system_is_dark: bool,
) -> ResolvedPalette {
    let id = active_theme_id(
        appearance,
        theme_id,
        light_theme_id,
        dark_theme_id,
        system_is_dark,
    );
    themes
        .iter()
        .find(|theme| theme.id == id)
        .map(|theme| theme.palette().clone())
        .unwrap_or_else(|| default_theme().palette().clone())
}

/// 按原生外观过滤主题（FollowSystem 配对候选）。
pub fn themes_for_native_appearance(themes: &[Theme], appearance: NativeAppearance) -> Vec<&Theme> {
    themes
        .iter()
        .filter(|theme| theme.native_appearance == appearance)
        .collect()
}

/// 内置主题注册表：8 套深色风格主题 + default/nord 的浅深各一。
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        default_theme(),
        default_light_theme(),
        nord_theme(),
        nord_light_theme(),
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

fn theme_with(id: &str, name: &str, native: NativeAppearance, palette: Palette) -> Theme {
    Theme {
        id: id.into(),
        name: name.into(),
        native_appearance: native,
        tokens: ThemeTokens { palette },
    }
}

/// `Termior Default`（深色原生）。
pub fn default_theme() -> Theme {
    theme_with(
        "default",
        "Termior Default",
        NativeAppearance::Dark,
        base_dark(),
    )
}

/// `Termior Default Light`（浅色配对候选）。
pub fn default_light_theme() -> Theme {
    theme_with(
        "default-light",
        "Termior Default Light",
        NativeAppearance::Light,
        base_light(),
    )
}

/// `nord`（深色原生）。
pub fn nord_theme() -> Theme {
    let n = |rgb: u32| Color::hex3(rgb);
    theme_with(
        "nord",
        "Nord",
        NativeAppearance::Dark,
        Palette {
            background: n(0x2e3440),
            foreground: n(0xd8dee9),
            chrome: n(0x3b4252),
            panel: n(0x242933),
            elevated: n(0x3b4252),
            overlay: n(0x434c5e),
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
    )
}

/// `nord-light`（浅色配对候选）。
pub fn nord_light_theme() -> Theme {
    let n = |rgb: u32| Color::hex3(rgb);
    theme_with(
        "nord-light",
        "Nord Light",
        NativeAppearance::Light,
        Palette {
            background: n(0xeceff4),
            foreground: n(0x2e3440),
            chrome: n(0xd8dee9),
            panel: n(0xe0e4ec),
            elevated: n(0xf5f7fb),
            overlay: n(0xffffff),
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
    )
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

/// 深色原生主题：只生成暗色色板，不再算法推导浅色变体。
fn styled_theme(id: &str, name: &str, colors: StyledColors) -> Theme {
    let white = Color::rgb(255, 255, 255);
    let background = Color::hex3(colors.background);
    let accent = Color::hex3(colors.accent);
    let red = Color::hex3(colors.red);
    let green = Color::hex3(colors.green);
    let yellow = Color::hex3(colors.yellow);
    let magenta = Color::hex3(colors.magenta);
    let cyan = Color::hex3(colors.cyan);

    let dark_foreground = blend(background, white, 0.90);
    let pure_black = Color::rgb(0, 0, 0);
    let palette = Palette {
        background,
        foreground: dark_foreground,
        chrome: blend(background, white, 0.14),
        // 用纯黑压暗，避免与接近黑的主题底色混合后几乎不变。
        panel: blend(background, pure_black, 0.35),
        elevated: blend(background, white, 0.08),
        overlay: blend(background, white, 0.20),
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
    theme_with(id, name, NativeAppearance::Dark, palette)
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
        chrome: c(0xeaeef2),
        panel: c(0xf6f8fa),
        elevated: c(0xffffff),
        overlay: c(0xffffff),
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
        chrome: c(0x1c2128),
        panel: c(0x010409),
        elevated: c(0x161b22),
        overlay: c(0x21262d),
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
    fn builtin_themes_cover_light_and_dark_natives() {
        let t = builtin_themes();
        assert!(t.len() >= 10, "FR-THEME-02: ships the full theme set");
        let ids: Vec<&str> = t.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains(&"default"));
        assert!(ids.contains(&"default-light"));
        assert!(ids.contains(&"nord"));
        assert!(ids.contains(&"nord-light"));
        assert!(ids.contains(&"tokyo-night"));
        assert!(t
            .iter()
            .any(|x| x.native_appearance == NativeAppearance::Light));
        assert!(t
            .iter()
            .any(|x| x.native_appearance == NativeAppearance::Dark));
    }

    #[test]
    fn every_builtin_has_native_appearance_label() {
        for theme in builtin_themes() {
            let inferred = infer_native_appearance(theme.palette());
            assert_eq!(
                theme.native_appearance, inferred,
                "{} native_appearance should match palette luminance",
                theme.id
            );
        }
    }

    #[test]
    fn builtin_themes_have_distinct_chrome_palettes() {
        let themes = builtin_themes();
        for (index, theme) in themes.iter().enumerate() {
            let palette = theme.palette();
            for other in themes.iter().skip(index + 1) {
                let other_palette = other.palette();
                assert_ne!(
                    (
                        palette.background,
                        palette.foreground,
                        palette.chrome,
                        palette.panel
                    ),
                    (
                        other_palette.background,
                        other_palette.foreground,
                        other_palette.chrome,
                        other_palette.panel
                    ),
                    "{} and {} render the same app chrome",
                    theme.id,
                    other.id
                );
            }
        }
    }

    /// chrome / panel 相对 content background 必须可分辨——浅色糊成一片的病灶。
    #[test]
    fn chrome_and_panel_contrast_against_background() {
        for theme in builtin_themes() {
            let p = theme.palette();
            let bg = luminance(p.background);
            let chrome_delta = (luminance(p.chrome) - bg).abs();
            let panel_delta = (luminance(p.panel) - bg).abs();
            assert!(
                chrome_delta >= 0.04,
                "{}: chrome vs background delta={chrome_delta:.3}",
                theme.id
            );
            assert!(
                panel_delta >= 0.025,
                "{}: panel vs background delta={panel_delta:.3}",
                theme.id
            );
        }
    }

    #[test]
    fn old_surface_array_format_still_imports() {
        let json = r#"{
            "id": "legacy-theme",
            "name": "Legacy",
            "tokens": {
                "light": {
                    "background": {"r":255,"g":255,"b":255},
                    "foreground": {"r":0,"g":0,"b":0},
                    "surface": [
                        {"r":240,"g":240,"b":240},
                        {"r":250,"g":250,"b":250},
                        {"r":230,"g":230,"b":230}
                    ],
                    "accent": {"r":0,"g":100,"b":200},
                    "status": [
                        {"r":0,"g":100,"b":200},
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":160,"b":0},
                        {"r":200,"g":0,"b":0}
                    ],
                    "diff": [
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":0,"b":0},
                        {"r":100,"g":100,"b":100}
                    ],
                    "terminal": {
                        "black":{"r":0,"g":0,"b":0},"red":{"r":1,"g":0,"b":0},
                        "green":{"r":0,"g":1,"b":0},"yellow":{"r":1,"g":1,"b":0},
                        "blue":{"r":0,"g":0,"b":1},"magenta":{"r":1,"g":0,"b":1},
                        "cyan":{"r":0,"g":1,"b":1},"white":{"r":1,"g":1,"b":1},
                        "bright_black":{"r":2,"g":2,"b":2},"bright_red":{"r":3,"g":0,"b":0},
                        "bright_green":{"r":0,"g":3,"b":0},"bright_yellow":{"r":3,"g":3,"b":0},
                        "bright_blue":{"r":0,"g":0,"b":3},"bright_magenta":{"r":3,"g":0,"b":3},
                        "bright_cyan":{"r":0,"g":3,"b":3},"bright_white":{"r":4,"g":4,"b":4}
                    }
                },
                "dark": {
                    "background": {"r":10,"g":10,"b":10},
                    "foreground": {"r":240,"g":240,"b":240},
                    "surface": [
                        {"r":20,"g":20,"b":20},
                        {"r":30,"g":30,"b":30},
                        {"r":40,"g":40,"b":40}
                    ],
                    "accent": {"r":0,"g":100,"b":200},
                    "status": [
                        {"r":0,"g":100,"b":200},
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":160,"b":0},
                        {"r":200,"g":0,"b":0}
                    ],
                    "diff": [
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":0,"b":0},
                        {"r":100,"g":100,"b":100}
                    ],
                    "terminal": {
                        "black":{"r":0,"g":0,"b":0},"red":{"r":1,"g":0,"b":0},
                        "green":{"r":0,"g":1,"b":0},"yellow":{"r":1,"g":1,"b":0},
                        "blue":{"r":0,"g":0,"b":1},"magenta":{"r":1,"g":0,"b":1},
                        "cyan":{"r":0,"g":1,"b":1},"white":{"r":1,"g":1,"b":1},
                        "bright_black":{"r":2,"g":2,"b":2},"bright_red":{"r":3,"g":0,"b":0},
                        "bright_green":{"r":0,"g":3,"b":0},"bright_yellow":{"r":3,"g":3,"b":0},
                        "bright_blue":{"r":0,"g":0,"b":3},"bright_magenta":{"r":3,"g":0,"b":3},
                        "bright_cyan":{"r":0,"g":3,"b":3},"bright_white":{"r":4,"g":4,"b":4}
                    }
                }
            }
        }"#;
        let theme = from_json(json).unwrap();
        assert_eq!(theme.native_appearance, NativeAppearance::Dark);
        assert_eq!(theme.tokens.palette.panel, Color::rgb(20, 20, 20));
        assert_eq!(theme.tokens.palette.chrome, Color::rgb(40, 40, 40));
        assert_eq!(theme.tokens.palette.overlay, Color::rgb(40, 40, 40));
    }

    #[test]
    fn import_infers_native_appearance_from_luminance() {
        let json = r#"{
            "id": "bright-custom",
            "name": "Bright",
            "tokens": {
                "palette": {
                    "background": {"r":250,"g":250,"b":250},
                    "foreground": {"r":20,"g":20,"b":20},
                    "chrome": {"r":230,"g":230,"b":230},
                    "panel": {"r":240,"g":240,"b":240},
                    "elevated": {"r":255,"g":255,"b":255},
                    "overlay": {"r":255,"g":255,"b":255},
                    "accent": {"r":0,"g":100,"b":200},
                    "status": [
                        {"r":0,"g":100,"b":200},
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":160,"b":0},
                        {"r":200,"g":0,"b":0}
                    ],
                    "diff": [
                        {"r":0,"g":160,"b":0},
                        {"r":200,"g":0,"b":0},
                        {"r":100,"g":100,"b":100}
                    ],
                    "terminal": {
                        "black":{"r":0,"g":0,"b":0},"red":{"r":1,"g":0,"b":0},
                        "green":{"r":0,"g":1,"b":0},"yellow":{"r":1,"g":1,"b":0},
                        "blue":{"r":0,"g":0,"b":1},"magenta":{"r":1,"g":0,"b":1},
                        "cyan":{"r":0,"g":1,"b":1},"white":{"r":1,"g":1,"b":1},
                        "bright_black":{"r":2,"g":2,"b":2},"bright_red":{"r":3,"g":0,"b":0},
                        "bright_green":{"r":0,"g":3,"b":0},"bright_yellow":{"r":3,"g":3,"b":0},
                        "bright_blue":{"r":0,"g":0,"b":3},"bright_magenta":{"r":3,"g":0,"b":3},
                        "bright_cyan":{"r":0,"g":3,"b":3},"bright_white":{"r":4,"g":4,"b":4}
                    }
                }
            }
        }"#;
        let theme = from_json(json).unwrap();
        assert_eq!(theme.native_appearance, NativeAppearance::Light);
        let exported = to_json(&theme).unwrap();
        assert!(exported.contains("\"native_appearance\": \"light\""));
        assert!(exported.contains("\"palette\""));
        assert!(!exported.contains("\"light\":"));
    }

    #[test]
    fn find_theme_works() {
        assert!(find_theme("nord").is_some());
        assert!(find_theme("default-light").is_some());
        assert!(find_theme("nope").is_none());
    }

    #[test]
    fn resolve_active_palette_follows_pairing() {
        let themes = builtin_themes();
        let light = resolve_active_palette(
            &themes,
            Appearance::FollowSystem,
            "tokyo-night",
            "default-light",
            "tokyo-night",
            false,
        );
        let dark = resolve_active_palette(
            &themes,
            Appearance::FollowSystem,
            "tokyo-night",
            "default-light",
            "tokyo-night",
            true,
        );
        assert_eq!(light.background, default_light_theme().palette().background);
        assert_eq!(
            dark.background,
            find_theme("tokyo-night").unwrap().palette().background
        );
    }

    #[test]
    fn selecting_theme_uses_native_palette() {
        let tokyo = find_theme("tokyo-night").unwrap();
        assert_eq!(tokyo.native_appearance, NativeAppearance::Dark);
        assert!(luminance(tokyo.palette().background) < 0.5);
    }

    #[test]
    fn terminal_palette_complete() {
        for t in builtin_themes() {
            let pal = t.palette().terminal.all();
            assert_eq!(pal.len(), 16);
            for c in pal {
                assert!((c.r, c.g, c.b) != (0, 0, 0), "all-black color in {}", t.id);
            }
        }
    }

    #[test]
    fn diff_palette_has_three_colors() {
        for t in builtin_themes() {
            assert_eq!(t.palette().diff.len(), 3);
        }
    }

    #[test]
    fn status_palette_has_four_colors() {
        for t in builtin_themes() {
            assert_eq!(t.palette().status.len(), 4);
        }
    }

    #[test]
    fn theme_json_roundtrip() {
        let t = nord_theme();
        let json = to_json(&t).unwrap();
        let back = from_json(&json).unwrap();
        assert_eq!(back.id, t.id);
        assert_eq!(back.native_appearance, NativeAppearance::Dark);
        assert_eq!(back.tokens.palette.background, t.tokens.palette.background);
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

    #[test]
    fn themes_for_native_filters_pairing_candidates() {
        let themes = builtin_themes();
        let light = themes_for_native_appearance(&themes, NativeAppearance::Light);
        let dark = themes_for_native_appearance(&themes, NativeAppearance::Dark);
        assert!(light
            .iter()
            .all(|t| t.native_appearance == NativeAppearance::Light));
        assert!(dark
            .iter()
            .all(|t| t.native_appearance == NativeAppearance::Dark));
        assert!(light.iter().any(|t| t.id == "default-light"));
        assert!(dark.iter().any(|t| t.id == "tokyo-night"));
        assert!(!light.iter().any(|t| t.id == "tokyo-night"));
    }
}
