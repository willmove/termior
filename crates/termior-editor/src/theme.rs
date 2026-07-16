//! Editor themes, independent from the application palette (FR-EDIT-07).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorTheme {
    pub id: String,
    pub name: String,
    pub background: String,
    pub foreground: String,
    pub comment: String,
    pub keyword: String,
    pub string: String,
    pub number: String,
    pub function: String,
    pub type_name: String,
    pub selection: String,
    pub cursor: String,
}

pub fn builtin_editor_themes() -> Vec<EditorTheme> {
    [
        (
            "atom-one", "Atom One", "#282c34", "#abb2bf", "#5c6370", "#c678dd", "#98c379",
            "#d19a66", "#61afef", "#e5c07b", "#3e4451", "#528bff",
        ),
        (
            "aura", "Aura", "#15141b", "#edecee", "#6d6d6d", "#a277ff", "#61ffca", "#ffca85",
            "#82e2ff", "#f694ff", "#29263c", "#a277ff",
        ),
        (
            "copilot", "Copilot", "#1e1e1e", "#d4d4d4", "#6a9955", "#c586c0", "#ce9178", "#b5cea8",
            "#dcdcaa", "#4ec9b0", "#264f78", "#aeafad",
        ),
        (
            "github-dark",
            "GitHub Dark",
            "#0d1117",
            "#c9d1d9",
            "#8b949e",
            "#ff7b72",
            "#a5d6ff",
            "#79c0ff",
            "#d2a8ff",
            "#ffa657",
            "#1f6feb",
            "#58a6ff",
        ),
        (
            "github-light",
            "GitHub Light",
            "#ffffff",
            "#24292f",
            "#6e7781",
            "#cf222e",
            "#0a3069",
            "#0550ae",
            "#8250df",
            "#953800",
            "#b6d7ff",
            "#0969da",
        ),
        (
            "gruvbox-dark",
            "Gruvbox Dark",
            "#282828",
            "#ebdbb2",
            "#928374",
            "#fb4934",
            "#b8bb26",
            "#d79921",
            "#83a598",
            "#fabd2f",
            "#504945",
            "#fe8019",
        ),
        (
            "nord", "Nord", "#2e3440", "#d8dee9", "#616e88", "#81a1c1", "#a3be8c", "#b48ead",
            "#88c0d0", "#ebcb8b", "#434c5e", "#88c0d0",
        ),
        (
            "tokyo-night",
            "Tokyo Night",
            "#1a1b26",
            "#c0caf5",
            "#565f89",
            "#bb9af7",
            "#9ece6a",
            "#ff9e64",
            "#7aa2f7",
            "#2ac3de",
            "#33467c",
            "#c0caf5",
        ),
        (
            "xcode-dark",
            "Xcode Dark",
            "#292a30",
            "#ffffff",
            "#7f8c98",
            "#fc5fa3",
            "#fc6a5d",
            "#d0bf69",
            "#67b7a4",
            "#5dd8ff",
            "#515b70",
            "#ffffff",
        ),
        (
            "xcode-light",
            "Xcode Light",
            "#ffffff",
            "#000000",
            "#5d6c79",
            "#ad3da4",
            "#d12f1b",
            "#272ad8",
            "#3e8087",
            "#0f68a0",
            "#b3d7ff",
            "#000000",
        ),
    ]
    .into_iter()
    .map(
        |(
            id,
            name,
            background,
            foreground,
            comment,
            keyword,
            string,
            number,
            function,
            type_name,
            selection,
            cursor,
        )| EditorTheme {
            id: id.into(),
            name: name.into(),
            background: background.into(),
            foreground: foreground.into(),
            comment: comment.into(),
            keyword: keyword.into(),
            string: string.into(),
            number: number.into(),
            function: function.into(),
            type_name: type_name.into(),
            selection: selection.into(),
            cursor: cursor.into(),
        },
    )
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ships_ten_independent_themes() {
        let themes = builtin_editor_themes();
        assert_eq!(themes.len(), 10);
        assert!(themes.iter().any(|t| t.id == "github-light"));
        assert!(themes.iter().any(|t| t.id == "tokyo-night"));
    }
}
