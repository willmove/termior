//! Termior 的国际化运行时（FR：UI 多语言支持）。
//!
//! 设计要点（详见 `docs/adr/0007-i18n-embedded-json-tables.md`）：
//!
//! - **编译期内嵌**：每个语言的翻译表是一份 `locales/<id>.json`，经
//!   `include_str!` 打进二进制。桌面发行不需要携带外部翻译文件，
//!   也不引入 fluent/gettext 及其 ICU 依赖面。
//! - **零分配热路径**：表值解析后以 `Arc<str>` 存储，[`text`] 返回
//!   `Arc<str>` 的引用计数克隆；GPUI 侧 `SharedString: From<Arc<str>>`，
//!   逐帧取文案不产生堆分配。
//! - **回退链**：当前语言 → 英文 → 键名本身。缺翻译不 panic、不空白，
//!   并由奇偶校验测试保证各语言键集与英文完全一致。
//! - **复数**：本仓库支持的语言中只有英文/西班牙文/德文区分 one(n==1)/other，
//!   中文与日韩文无复数形态。键名约定 `key.one` / `key.other`，
//!   由 [`plural`] 按 CLDR 简化规则选择。
//!
//! 本 crate 不依赖 GPUI，`termior-ui-kit`（`--no-default-features`）与独立
//! askpass 进程均可直接使用。切换语言是进程级全局状态，UI 层切换后需触发
//! 重渲染让新文案生效。

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

/// 单个语言的元数据：规范 id、英文名与母语名（设置页下拉用，母语名不翻译）。
pub struct LocaleInfo {
    pub id: &'static str,
    pub english_name: &'static str,
    pub native_name: &'static str,
}

/// 全部内置语言。顺序即设置页展示顺序，索引 0 是默认语言（英文）。
pub const LOCALES: &[LocaleInfo] = &[
    LocaleInfo {
        id: "en",
        english_name: "English",
        native_name: "English",
    },
    LocaleInfo {
        id: "zh-CN",
        english_name: "Chinese (Simplified)",
        native_name: "简体中文",
    },
    LocaleInfo {
        id: "zh-TW",
        english_name: "Chinese (Traditional)",
        native_name: "繁體中文",
    },
    LocaleInfo {
        id: "ja",
        english_name: "Japanese",
        native_name: "日本語",
    },
    LocaleInfo {
        id: "ko",
        english_name: "Korean",
        native_name: "한국어",
    },
    LocaleInfo {
        id: "es",
        english_name: "Spanish",
        native_name: "Español",
    },
    LocaleInfo {
        id: "de",
        english_name: "German",
        native_name: "Deutsch",
    },
];

const DEFAULT_LOCALE: &str = "en";

/// 当前语言在 [`LOCALES`] 中的下标。AtomicU8 足够：语言数远小于 256。
static CURRENT: AtomicU8 = AtomicU8::new(0);

const RAW_TABLES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("zh-CN", include_str!("../locales/zh-CN.json")),
    ("zh-TW", include_str!("../locales/zh-TW.json")),
    ("ja", include_str!("../locales/ja.json")),
    ("ko", include_str!("../locales/ko.json")),
    ("es", include_str!("../locales/es.json")),
    ("de", include_str!("../locales/de.json")),
];

struct Tables {
    by_locale: HashMap<&'static str, HashMap<String, Arc<str>>>,
}

static TABLES: OnceLock<Tables> = OnceLock::new();

fn parse_locale_table(id: &str, raw: &str) -> HashMap<String, Arc<str>> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .unwrap_or_else(|error| panic!("termior-i18n: locale {id} is not valid JSON: {error}"));
    let object = value
        .as_object()
        .unwrap_or_else(|| panic!("termior-i18n: locale {id} must be a flat key/value object"));
    object
        .iter()
        .map(|(key, value)| {
            let text = value.as_str().unwrap_or_else(|| {
                panic!("termior-i18n: locale {id} key {key} must map to a string")
            });
            (key.clone(), Arc::from(text))
        })
        .collect()
}

fn tables() -> &'static Tables {
    TABLES.get_or_init(|| Tables {
        by_locale: RAW_TABLES
            .iter()
            .map(|(id, raw)| (*id, parse_locale_table(id, raw)))
            .collect(),
    })
}

/// 设置页下拉所需的全部语言。
pub fn available_locales() -> &'static [LocaleInfo] {
    LOCALES
}

/// 把任意 BCP 47 风格标签规范化为内置语言 id。
///
/// 已知变体归并：`zh-Hans`/`zh-SG`/`zh-MY` → `zh-CN`；`zh-Hant`/`zh-HK`/
/// `zh-MO` → `zh-TW`；无 script/region 的裸 `zh` 按简体处理；英语/日语/
/// 韩语/西语/德语忽略地区后缀（`de-AT` → `de`）。未知语言返回 `None`。
pub fn canonicalize(tag: &str) -> Option<&'static str> {
    let normalized = tag.trim().to_ascii_lowercase();
    let mut language = None;
    let mut script = None;
    let mut region = None;
    for part in normalized.split(['-', '_']) {
        if part.is_empty() {
            continue;
        }
        if part.len() == 4 {
            script = Some(part);
        } else if part.len() == 2 {
            if language.is_none() {
                language = Some(part);
            } else {
                region = Some(part);
            }
        }
    }
    match language? {
        "zh" => {
            if script == Some("hant") || ["tw", "hk", "mo"].contains(&region.unwrap_or("")) {
                Some("zh-TW")
            } else {
                Some("zh-CN")
            }
        }
        "en" => Some("en"),
        "ja" => Some("ja"),
        "ko" => Some("ko"),
        "es" => Some("es"),
        "de" => Some("de"),
        _ => None,
    }
}

/// 当前语言 id（恒为内置 id，不会是任意用户输入）。
pub fn current_locale() -> &'static str {
    LOCALES[CURRENT.load(Ordering::Relaxed) as usize].id
}

/// 切换当前语言。接受任意可 [`canonicalize`] 的标签；未知标签返回 `false`
/// 且保持原语言不变。
pub fn set_locale(tag: &str) -> bool {
    let Some(id) = canonicalize(tag) else {
        return false;
    };
    let Some(index) = LOCALES.iter().position(|locale| locale.id == id) else {
        return false;
    };
    CURRENT.store(index as u8, Ordering::Relaxed);
    true
}

/// 系统语言检测（用于「跟随系统」）。检测失败返回 `None`。
pub fn system_locale() -> Option<&'static str> {
    sys_locale::get_locale().as_deref().and_then(canonicalize)
}

/// 应用启动时的语言初始化：显式设置优先，其次系统语言，最后英文。
/// 返回实际生效的语言 id。
pub fn init(preferred: Option<&str>) -> &'static str {
    let id = preferred
        .and_then(canonicalize)
        .or_else(system_locale)
        .unwrap_or(DEFAULT_LOCALE);
    set_locale(id);
    current_locale()
}

/// 按当前语言取键值；回退英文；再缺则原样返回键名（开发期可见漏网键）。
pub fn text(key: &str) -> Arc<str> {
    text_in(current_locale(), key)
}

/// 按指定语言取键值，回退链同 [`text`]。
pub fn text_in(locale: &str, key: &str) -> Arc<str> {
    resolve(tables(), locale, key)
}

/// 回退链实现：目标语言 → 英文 → 键名本身。拆成自由函数便于用合成表测试。
fn resolve(tables: &Tables, locale: &str, key: &str) -> Arc<str> {
    let lookup = |id: &str| {
        tables
            .by_locale
            .get(id)
            .and_then(|table| table.get(key))
            .cloned()
    };
    let locale = canonicalize(locale).unwrap_or(DEFAULT_LOCALE);
    lookup(locale)
        .or_else(|| lookup(DEFAULT_LOCALE))
        .unwrap_or_else(|| Arc::from(key))
}

/// 取键值并做 `{name}` 占位符替换，返回拼好的字符串。
///
/// `{{` / `}}` 是字面量大括号转义；未知占位符原样保留，方便发现漏传参数。
pub fn format_key(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    interpolate(&text(key), args)
}

/// 复数键取值：按语言与数量在 `key.one` / `key.other` 之间选择，
/// 并把 `{count}` 替换为数量。
///
/// CLDR 简化规则——本仓库语言表中仅 en/es/de 区分 one（n == 1）与 other，
/// zh-CN/zh-TW/ja/ko 无复数形态恒取 other。新增语言若需更多类别
/// （如俄语的 one/few/many），须在此扩充规则。
pub fn plural(count: usize, key: &str) -> String {
    let category = plural_category(current_locale(), count);
    let suffix = match category {
        PluralCategory::One => "one",
        PluralCategory::Other => "other",
    };
    let full_key = format!("{key}.{suffix}");
    interpolate(&text(&full_key), &[("count", &count)])
}

/// 复数类别。
pub enum PluralCategory {
    One,
    Other,
}

fn plural_category(locale: &str, count: usize) -> PluralCategory {
    match locale {
        "en" | "es" | "de" if count == 1 => PluralCategory::One,
        _ => PluralCategory::Other,
    }
}

fn interpolate(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + 16);
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => {
                out.push('{');
                index += 2;
            }
            b'}' if bytes.get(index + 1) == Some(&b'}') => {
                out.push('}');
                index += 2;
            }
            b'{' => match template[index + 1..].find('}') {
                Some(offset) => {
                    let end = index + 1 + offset;
                    let name = &template[index + 1..end];
                    if let Some((_, value)) = args.iter().find(|(arg, _)| *arg == name) {
                        let _ = write!(out, "{value}");
                    } else {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                    index = end + 1;
                }
                None => {
                    out.push_str(&template[index..]);
                    break;
                }
            },
            b'}' => {
                out.push('}');
                index += 1;
            }
            _ => {
                let next = template[index..]
                    .find(['{', '}'])
                    .map_or(bytes.len(), |offset| index + offset);
                out.push_str(&template[index..next]);
                index = next;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `set_locale` 改的是进程级全局状态；加锁避免测试间竞争。
    static LOCALE_GUARD: Mutex<()> = Mutex::new(());

    fn with_locale<T>(locale: &str, body: impl FnOnce() -> T) -> T {
        let _guard = LOCALE_GUARD.lock().unwrap();
        let previous = current_locale();
        assert!(set_locale(locale));
        let result = body();
        set_locale(previous);
        result
    }

    #[test]
    fn canonicalize_known_variants() {
        assert_eq!(canonicalize("en"), Some("en"));
        assert_eq!(canonicalize("en-US"), Some("en"));
        assert_eq!(canonicalize("EN_us"), Some("en"));
        assert_eq!(canonicalize("zh"), Some("zh-CN"));
        assert_eq!(canonicalize("zh-CN"), Some("zh-CN"));
        assert_eq!(canonicalize("zh-SG"), Some("zh-CN"));
        assert_eq!(canonicalize("zh-Hans"), Some("zh-CN"));
        assert_eq!(canonicalize("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(canonicalize("zh_TW"), Some("zh-TW"));
        assert_eq!(canonicalize("zh-TW"), Some("zh-TW"));
        assert_eq!(canonicalize("zh-Hant"), Some("zh-TW"));
        assert_eq!(canonicalize("zh-HK"), Some("zh-TW"));
        assert_eq!(canonicalize("ja-JP"), Some("ja"));
        assert_eq!(canonicalize("ko-KR"), Some("ko"));
        assert_eq!(canonicalize("es-419"), Some("es"));
        assert_eq!(canonicalize("fr"), None);
        assert_eq!(canonicalize("  de-AT "), Some("de"));
        assert_eq!(canonicalize(""), None);
    }

    #[test]
    fn set_locale_rejects_unknown_tags() {
        let _guard = LOCALE_GUARD.lock().unwrap();
        let previous = current_locale();
        assert!(!set_locale("klingon"));
        assert_eq!(current_locale(), previous);
    }

    #[test]
    fn text_falls_back_to_english_then_key() {
        with_locale("ja", || {
            // 各语言表齐全时应返回该语言自身的译文。
            assert_eq!(&*text("action.save"), "保存");
        });
        with_locale("en", || {
            assert_eq!(&*text("definitely.not.a.key"), "definitely.not.a.key");
        });
    }

    #[test]
    fn resolve_falls_back_through_the_chain() {
        // 合成表直接驱动回退链：目标语言缺键时取英文，英文也缺时返回键名。
        let mut by_locale = HashMap::new();
        let mut ja = HashMap::new();
        ja.insert("only.ja".to_owned(), Arc::from("日本語"));
        by_locale.insert("ja", ja);
        let mut en = HashMap::new();
        en.insert("only.en".to_owned(), Arc::from("English"));
        by_locale.insert("en", en);
        let tables = Tables { by_locale };

        assert_eq!(&*resolve(&tables, "ja", "only.ja"), "日本語");
        assert_eq!(&*resolve(&tables, "ja", "only.en"), "English");
        assert_eq!(&*resolve(&tables, "ja", "nowhere"), "nowhere");
        // 无法识别的语言标签回退英文。
        assert_eq!(&*resolve(&tables, "xx", "only.en"), "English");
    }

    #[test]
    fn text_in_reads_named_locale() {
        assert_eq!(&*text_in("en", "action.send"), "Send");
        assert_eq!(
            text_in("zh-Hant", "action.send"),
            text_in("zh-TW", "action.send")
        );
    }

    #[test]
    fn interpolate_substitutes_named_args() {
        let args: &[(&str, &dyn std::fmt::Display)] = &[("count", &3), ("name", &"docs")];
        assert_eq!(
            interpolate("{count} items in {name}", args),
            "3 items in docs"
        );
        // 未知占位符原样保留。
        assert_eq!(interpolate("{count} of {total}", args), "3 of {total}");
        // 大括号转义与孤立大括号。
        assert_eq!(interpolate("{{count}}", args), "{count}");
        assert_eq!(interpolate("a}b", args), "a}b");
        assert_eq!(interpolate("no placeholders", args), "no placeholders");
        assert_eq!(interpolate("", args), "");
        assert_eq!(interpolate("{", args), "{");
        assert_eq!(interpolate("trailing {", args), "trailing {");
        assert_eq!(interpolate("{}", args), "{}");
    }

    #[test]
    fn plural_picks_category_and_formats_count() {
        with_locale("en", || {
            assert_eq!(plural(1, "explorer.skipped"), "1 item was skipped");
            assert_eq!(plural(0, "explorer.skipped"), "0 items were skipped");
            assert_eq!(plural(5, "explorer.skipped"), "5 items were skipped");
        });
        with_locale("zh-CN", || {
            assert_eq!(plural(1, "explorer.skipped"), "跳过了 1 项");
        });
    }

    #[test]
    fn plural_category_follows_language_rules() {
        use PluralCategory::{One, Other};
        assert!(matches!(plural_category("en", 1), One));
        assert!(matches!(plural_category("en", 2), Other));
        assert!(matches!(plural_category("en", 0), Other));
        assert!(matches!(plural_category("es", 1), One));
        assert!(matches!(plural_category("de", 1), One));
        assert!(matches!(plural_category("ja", 1), Other));
        assert!(matches!(plural_category("ko", 1), Other));
        assert!(matches!(plural_category("zh-CN", 1), Other));
        assert!(matches!(plural_category("zh-TW", 1), Other));
    }

    #[test]
    fn format_key_interpolates() {
        with_locale("en", || {
            let count = 12;
            assert_eq!(
                format_key("explorer.skipped.other", &[("count", &count)]),
                "12 items were skipped"
            );
        });
    }

    #[test]
    fn init_prefers_explicit_setting_then_system() {
        let _guard = LOCALE_GUARD.lock().unwrap();
        let previous = current_locale();
        assert_eq!(init(Some("zh-Hant")), "zh-TW");
        // 显式设置未知语言时回退系统语言；系统语言也未知时回退英文。
        assert_eq!(init(None), system_locale().unwrap_or("en"));
        set_locale(previous);
    }

    #[test]
    fn locale_tables_have_identical_key_sets() {
        let tables = tables();
        let base = &tables.by_locale[DEFAULT_LOCALE];
        assert!(!base.is_empty(), "en.json must not be empty");
        for (id, table) in &tables.by_locale {
            for key in base.keys() {
                assert!(
                    table.contains_key(key),
                    "locale {id} is missing key {key} (present in en)"
                );
            }
            for key in table.keys() {
                assert!(
                    base.contains_key(key),
                    "locale {id} has extra key {key} (absent from en)"
                );
            }
        }
    }

    #[test]
    fn locale_tables_have_no_empty_values() {
        for (id, table) in &tables().by_locale {
            for (key, value) in table {
                assert!(
                    !value.trim().is_empty(),
                    "locale {id} key {key} must not be empty"
                );
            }
        }
    }

    #[test]
    fn raw_tables_cover_every_registered_locale() {
        for locale in LOCALES {
            assert!(
                RAW_TABLES.iter().any(|(id, _)| *id == locale.id),
                "locale {} has no embedded table",
                locale.id
            );
        }
    }
}
