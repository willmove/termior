//! 会话 id、路径与片段预览等纯函数，供输入、会话与后端模块共用。

use super::*;

/// 片段正文的单行预览，用于补全弹层与片段面板。
pub(super) fn snippet_preview(body: &str) -> String {
    const MAX_CHARS: usize = 60;
    let line = body.lines().next().unwrap_or_default();
    let mut preview: String = line.chars().take(MAX_CHARS).collect();
    if line.chars().count() > MAX_CHARS || body.lines().nth(1).is_some() {
        preview.push('…');
    }
    preview
}

pub(super) fn new_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("session-{millis}")
}

pub(super) fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub(super) fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub(super) fn requested_skill_names(input: &str, skills: &[SkillMetadata]) -> Vec<String> {
    skills
        .iter()
        .filter(|skill| {
            input.split_whitespace().any(|word| {
                word.starts_with('$')
                    && word.trim_matches(|ch: char| {
                        !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_'
                    }) == skill.name
            })
        })
        .map(|skill| skill.name.clone())
        .collect()
}
