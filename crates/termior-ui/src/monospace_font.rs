//! 等宽字体解析。
//!
//! GPUI 查字体是「family 名精确匹配」（`gpui_wgpu::cosmic_text_system::load_family`
//! 用 `face.families.iter().any(|f| *name == f.0)` 过滤 fontdb）。匹配不到时它不报错，
//! 而是静默走 `TextSystem::fallback_font_stack` —— 那条链上除了 Zed 自带的 `.ZedMono`
//! 全是比例字体（Helvetica / Adwaita Sans / Cantarell / Noto Sans / …），而 Termior
//! 没有内嵌字体，所以一旦配置的字体没装，终端就会拿到一个比例字体。
//!
//! 后果不只是「不好看」：终端把 `'M'` 的 advance 当网格 cell 宽，文本却按各字形自身
//! advance 排版。比例字体里 `'M'` 差不多是小写字母的两倍宽，于是每个变色片段都要从
//! 网格列重新起画，视觉上就是「空格忽宽忽窄」。
//!
//! 因此字体名在用之前必须校验两件事：系统里确实有这个 family，并且它确实等宽。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use gpui::{px, Font, FontFeatures, FontStyle, FontWeight, SharedString, TextSystem};

/// 候选等宽字体，按优先级排列；用户配置的字体会被插到最前面。
///
/// Nerd Font 变体的 family 名和原字体不同（"JetBrains Mono" vs "JetBrainsMono Nerd
/// Font"），精确匹配下必须分别列出 —— 这正是本次 bug 的直接来源。
const CANDIDATES: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font",
    "JetBrainsMono NF",
    "Fira Code",
    "FiraCode Nerd Font Mono",
    "FiraCode Nerd Font",
    "Cascadia Code",
    "Cascadia Mono",
    "SF Mono",
    "Menlo",
    "Monaco",
    "Consolas",
    "Source Code Pro",
    "Hack",
    "Ubuntu Mono",
    // 发行版/系统自带兜底
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Adwaita Mono",
    "Courier New",
];

/// 判定等宽用的字号；只比较相对宽度，取值不敏感。
const PROBE_FONT_SIZE: f32 = 16.0;

/// 宽窄差异超过这个像素数就认为不是等宽。
const ADVANCE_EPSILON: f32 = 0.01;

fn cache() -> &'static Mutex<HashMap<String, SharedString>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SharedString>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

static DEFAULT_FAMILY: OnceLock<SharedString> = OnceLock::new();

/// 启动时调用一次，解析出全局等宽字体，供拿不到 `App` / `Window` 的渲染辅助函数使用。
pub fn init_default(text_system: &TextSystem) {
    let _ = DEFAULT_FAMILY.set(resolve(text_system, "monospace"));
}

/// 全局等宽字体名。`init_default` 之前返回 `"monospace"`，即修复前的行为。
pub fn default_family() -> SharedString {
    DEFAULT_FAMILY
        .get()
        .cloned()
        .unwrap_or_else(|| SharedString::new_static("monospace"))
}

/// 把 `configured` 解析成一个系统里确实存在、且确实等宽的 family 名。
///
/// 结果按 `configured` 缓存，因此可以在每帧 render 里直接调用。
pub fn resolve(text_system: &TextSystem, configured: &str) -> SharedString {
    if let Some(hit) = cache()
        .lock()
        .expect("monospace font cache poisoned")
        .get(configured)
    {
        return hit.clone();
    }
    let resolved = resolve_uncached(text_system, configured);
    cache()
        .lock()
        .expect("monospace font cache poisoned")
        .insert(configured.to_owned(), resolved.clone());
    resolved
}

fn resolve_uncached(text_system: &TextSystem, configured: &str) -> SharedString {
    let available: HashSet<String> = text_system.all_font_names().into_iter().collect();
    let picked = pick_family(configured, &available, |name| {
        is_monospace(text_system, name)
    });
    if picked.as_ref() != configured {
        log::warn!(
            "字体 `{configured}` 未安装或非等宽，改用 `{picked}`；\
             可在 Settings → Terminal → Font Family 指定其它字体"
        );
    }
    picked
}

/// 选字体的纯逻辑：优先用户配置，其次候选表，都要求「已安装且等宽」。
///
/// 全不满足时退而使用「已安装但不等宽」的候选，再不行才原样返回 `configured`
/// 交给 GPUI 自己回退。抽成纯函数是为了不依赖真实字体系统也能测。
fn pick_family(
    configured: &str,
    available: &HashSet<String>,
    is_monospace: impl Fn(&str) -> bool,
) -> SharedString {
    let mut installed_but_not_monospace: Option<&str> = None;

    for name in std::iter::once(configured).chain(CANDIDATES.iter().copied()) {
        if !available.contains(name) {
            continue;
        }
        if is_monospace(name) {
            return SharedString::from(name.to_owned());
        }
        installed_but_not_monospace.get_or_insert(name);
    }

    SharedString::from(installed_but_not_monospace.unwrap_or(configured).to_owned())
}

/// 用几个宽窄差异最大的字符判定等宽。
///
/// 注意 `resolve_font` 在 family 不存在时会静默回退，所以调用方必须先确认 family
/// 在 `all_font_names()` 里，否则这里量到的是回退字体的宽度。
fn is_monospace(text_system: &TextSystem, family: &str) -> bool {
    let font = Font {
        family: SharedString::from(family.to_owned()),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let font_id = text_system.resolve_font(&font);
    let size = px(PROBE_FONT_SIZE);
    let mut advances = ['M', 'i', 'W', '.'].into_iter().map(|ch| {
        text_system
            .advance(font_id, size, ch)
            .map(|s| s.width.as_f32())
    });

    let Some(Ok(first)) = advances.next() else {
        return false;
    };
    if first <= 0.0 {
        return false;
    }
    advances.all(|advance| matches!(advance, Ok(w) if (w - first).abs() < ADVANCE_EPSILON))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn configured_family_wins_when_installed_and_monospace() {
        let available = set(&["Fira Code", "JetBrainsMono Nerd Font"]);
        let picked = pick_family("Fira Code", &available, |_| true);
        assert_eq!(picked.as_ref(), "Fira Code");
    }

    #[test]
    fn falls_back_to_candidate_when_configured_family_is_missing() {
        // 复现本次 bug：装的是 Nerd Font 变体，配置里写的是不带空格差异的原名。
        let available = set(&["JetBrainsMono Nerd Font", "Adwaita Sans"]);
        let picked = pick_family("JetBrains Mono", &available, |_| true);
        assert_eq!(picked.as_ref(), "JetBrainsMono Nerd Font");
    }

    #[test]
    fn installed_but_proportional_family_is_rejected() {
        let available = set(&["Comic Sans MS", "DejaVu Sans Mono"]);
        let picked = pick_family("Comic Sans MS", &available, |name| name.contains("Mono"));
        assert_eq!(picked.as_ref(), "DejaVu Sans Mono");
    }

    #[test]
    fn candidate_order_is_respected() {
        let available = set(&["Courier New", "Fira Code", "Noto Sans Mono"]);
        let picked = pick_family("missing", &available, |_| true);
        assert_eq!(picked.as_ref(), "Fira Code");
    }

    #[test]
    fn prefers_installed_proportional_family_over_a_name_that_resolves_to_nothing() {
        let available = set(&["Adwaita Mono"]);
        let picked = pick_family("missing", &available, |_| false);
        assert_eq!(picked.as_ref(), "Adwaita Mono");
    }

    #[test]
    fn keeps_configured_family_when_nothing_is_available() {
        let picked = pick_family("JetBrains Mono", &set(&[]), |_| true);
        assert_eq!(picked.as_ref(), "JetBrains Mono");
    }
}
