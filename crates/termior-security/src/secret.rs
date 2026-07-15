//! 密钥不落盘校验（FR-SEC-06 / INV-5）。
//!
//! API Key 只存在于 OS 钥匙串与调用瞬间的内存中；任何持久化文件、日志、panic 报告
//! 不得出现 key 明文。
//!
//! 本模块提供序列化前的「明文泄漏扫描」：对一段待落盘的文本（通常是序列化后的 JSON）
//! 扫描是否包含形似 API key 的明文，命中则拒绝落盘并返回 [`SecretLeak`]。

/// 检测到的密钥泄漏。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretLeak {
    #[error("potential api key leak detected: prefix={prefix}")]
    ApiKey { prefix: String },
}

/// 扫描待落盘文本是否包含形似密钥的明文。
///
/// 识别的形态：
/// - `sk-...`（OpenAI 风格）
/// - `sk-ant-...`（Anthropic 风格）
/// - `xai-...`、`AIza...`（Google）、`gho_`/`ghp_`（GitHub token）
/// - 形如 `"api_key": "..."` / `"apiKey": "..."` / `"Authorization": "Bearer ..."` 的键值对（值非空）
///
/// 返回第一个命中（足以拒绝落盘）。
pub fn assert_no_secret_fields(text: &str) -> Result<(), SecretLeak> {
    for tok in text.split_whitespace() {
        if looks_like_api_key(tok) {
            return Err(SecretLeak::ApiKey {
                prefix: tok.chars().take(8).collect(),
            });
        }
    }
    // 键值对形态：跨整段文本（非按空白切分）
    for (needle, _) in [
        ("\"api_key\"", "api_key"),
        ("\"apiKey\"", "apiKey"),
        ("\"apikey\"", "apikey"),
        ("\"API_KEY\"", "API_KEY"),
        ("\"secret_key\"", "secret_key"),
        ("\"access_token\"", "access_token"),
    ] {
        if let Some(idx) = text.find(needle) {
            // 取该 key 之后第一个引号字符串作为值
            if let Some(val) = extract_next_string(&text[idx + needle.len()..]) {
                if !val.is_empty() && val != "null" {
                    return Err(SecretLeak::ApiKey {
                        prefix: val.chars().take(8).collect(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// 是否形似已知 Provider 的 API key 前缀。
fn looks_like_api_key(tok: &str) -> bool {
    // 去掉首尾的引号/逗号/冒号等 JSON 标点
    let t = tok.trim_matches(|c: char| matches!(c, '"' | ',' | ':' | ' ' | '\n' | '\r' | '\t'));
    let prefixes = [
        "sk-ant-",
        "sk-",
        "xai-",
        "AIza",
        "gho_",
        "ghp_",
        "ghu_",
        "ghs_",
        "ghr_",
    ];
    prefixes.iter().any(|p| t.starts_with(p) && t.len() > p.len() + 8)
}

/// 从 `s` 开头寻找下一个 `"..."` 字符串字面量，返回其内容（不含引号）。
fn extract_next_string(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    // 跳到第一个未转义的 `"`
    while i < bytes.len() && bytes[i] != b'"' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    i += 1; // 跳过开引号
    let start = i;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return Some(s[start..i].to_string());
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_passes() {
        assert!(assert_no_secret_fields(r#"{"theme":"nord"}"#).is_ok());
        assert!(assert_no_secret_fields("just some normal text").is_ok());
    }

    #[test]
    fn openai_key_detected() {
        let e = assert_no_secret_fields("sk-1234567890abcdef").unwrap_err();
        assert!(matches!(e, SecretLeak::ApiKey { .. }));
    }

    #[test]
    fn anthropic_key_detected() {
        assert!(assert_no_secret_fields("sk-ant-api03-xxxxxxxxxxxx").is_err());
    }

    #[test]
    fn google_xai_github_keys_detected() {
        assert!(assert_no_secret_fields("AIzaSyAAAAAAAAAAAAAAAAAAAAAA").is_err());
        assert!(assert_no_secret_fields("xai-1234567890abcdef").is_err());
        assert!(assert_no_secret_fields("ghp_1234567890abcdef").is_err());
    }

    #[test]
    fn api_key_keyvalue_detected() {
        let json = r#"{"theme":"nord","api_key":"sk-abcdefghij"}"#;
        assert!(assert_no_secret_fields(json).is_err());
    }

    #[test]
    fn api_key_null_passes() {
        // 显式 null 不算泄漏（占位）
        let json = r#"{"api_key":null}"#;
        assert!(assert_no_secret_fields(json).is_ok());
    }

    #[test]
    fn short_token_not_flagged() {
        // `sk-abc` 太短，不算 key（避免误伤普通 `sk-` 前缀词）
        assert!(assert_no_secret_fields("sk-abc").is_ok());
    }

    #[test]
    fn bearer_token_in_settings_rejected() {
        let json = r#"{"Authorization":"Bearer sk-1234567890abcdef"}"#;
        // Bearer 后跟 sk- 前缀会被裸 token 扫描命中
        assert!(assert_no_secret_fields(json).is_err());
    }
}
