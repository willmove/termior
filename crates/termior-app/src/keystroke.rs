//! 键盘按键 → PTY 字节序列映射（FR-TERM-01 键盘输入）。
//!
//! 把 GPUI `Keystroke` 编码成终端期望的字节流，遵循 xterm 语义：
//! - 可打印字符：直接发 utf-8 字节（优先用 `key_char`，回退 `key`）。
//! - 功能键：Enter/Backspace/Tab/Esc/方向键/F1-F12 → 对应控制字符或 CSI 序列。
//! - Ctrl+字母：Ctrl+A..Z → 0x01..0x1a。
//!
//! 修饰键（Alt/Meta）前缀 `\x1b`；Shift 主要改变 `key_char`（由 GPUI 给出）。

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// 把一次按键编码成要写入 PTY 的字节。返回空 Vec 表示该键不产生输入
/// （如纯修饰键按下、未识别的组合）。
pub fn keystroke_to_pty_bytes(ks: &Keystroke, mode: TermMode) -> Vec<u8> {
    let m = &ks.modifiers;

    if m.control && !m.platform && !m.function {
        if let Some(code) = control_character(&ks.key) {
            let mut bytes = vec![code];
            if m.alt {
                bytes.insert(0, 0x1b);
            }
            return bytes;
        }
    }

    if let Some(bytes) = map_function_key(ks, mode) {
        return bytes;
    }

    // 可打印字符：优先 key_char（GPUI 已处理 shift/死键等），回退 key。
    if !m.control && !m.platform {
        if let Some(ch) = &ks.key_char {
            if !ch.is_empty() {
                let mut bytes = ch.as_bytes().to_vec();
                if m.alt {
                    bytes.insert(0, 0x1b); // Alt 前缀 ESC
                }
                return bytes;
            }
        }
    }

    // 兜底：单字符 key（如某些布局下 key_char 缺失）
    if !m.control && !m.alt && !m.platform && !m.function {
        if let Some(c) = ks.key.chars().next() {
            if c.is_ascii() && !c.is_control() {
                return vec![c as u8];
            }
        }
    }

    Vec::new()
}

pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let text = text.replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

fn control_character(key: &str) -> Option<u8> {
    let key = key.to_ascii_lowercase();
    if key.chars().count() != 1 {
        return None;
    }
    let character = key.chars().next()?;
    match character {
        'a'..='z' => Some(character as u8 - b'a' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

/// 功能键映射：根据 DECCKM/DECPAM 与 xterm 修饰键规则编码。
fn map_function_key(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let legacy_alt_prefix = |mut bytes: Vec<u8>| -> Vec<u8> {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    let modifier = modifier_parameter(ks);
    let app_cursor = mode.contains(TermMode::APP_CURSOR) && modifier == 1;
    match ks.key.as_str() {
        "enter" | "return" => Some(legacy_alt_prefix(vec![b'\r'])),
        "backspace" => Some(legacy_alt_prefix(vec![0x7f])),
        "tab" => Some(if modifier == 1 {
            vec![b'\t']
        } else if m.shift && !m.alt && !m.control {
            b"\x1b[Z".to_vec()
        } else {
            format!("\x1b[1;{modifier}Z").into_bytes()
        }),
        "escape" => Some(vec![0x1b]),
        "space" if ks.key_char.is_none() => Some(legacy_alt_prefix(vec![b' '])),
        "left" => Some(cursor_key('D', modifier, app_cursor)),
        "right" => Some(cursor_key('C', modifier, app_cursor)),
        "up" => Some(cursor_key('A', modifier, app_cursor)),
        "down" => Some(cursor_key('B', modifier, app_cursor)),
        "home" => Some(cursor_key('H', modifier, app_cursor)),
        "end" => Some(cursor_key('F', modifier, app_cursor)),
        "insert" => Some(tilde_key(2, modifier)),
        "delete" => Some(tilde_key(3, modifier)),
        "pageup" => Some(tilde_key(5, modifier)),
        "pagedown" => Some(tilde_key(6, modifier)),
        "f1" => Some(function_key('P', None, modifier)),
        "f2" => Some(function_key('Q', None, modifier)),
        "f3" => Some(function_key('R', None, modifier)),
        "f4" => Some(function_key('S', None, modifier)),
        "f5" => Some(function_key('~', Some(15), modifier)),
        "f6" => Some(function_key('~', Some(17), modifier)),
        "f7" => Some(function_key('~', Some(18), modifier)),
        "f8" => Some(function_key('~', Some(19), modifier)),
        "f9" => Some(function_key('~', Some(20), modifier)),
        "f10" => Some(function_key('~', Some(21), modifier)),
        "f11" => Some(function_key('~', Some(23), modifier)),
        "f12" => Some(function_key('~', Some(24), modifier)),
        "numpad0" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOp".to_vec()),
        "numpad1" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOq".to_vec()),
        "numpad2" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOr".to_vec()),
        "numpad3" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOs".to_vec()),
        "numpad4" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOt".to_vec()),
        "numpad5" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOu".to_vec()),
        "numpad6" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOv".to_vec()),
        "numpad7" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOw".to_vec()),
        "numpad8" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOx".to_vec()),
        "numpad9" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOy".to_vec()),
        "numpaddecimal" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOn".to_vec()),
        "numpadenter" if mode.contains(TermMode::APP_KEYPAD) => Some(b"\x1bOM".to_vec()),
        _ => None,
    }
}

fn modifier_parameter(ks: &Keystroke) -> u8 {
    1 + u8::from(ks.modifiers.shift)
        + 2 * u8::from(ks.modifiers.alt)
        + 4 * u8::from(ks.modifiers.control)
}

fn cursor_key(final_character: char, modifier: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        format!("\x1bO{final_character}").into_bytes()
    } else if modifier == 1 {
        format!("\x1b[{final_character}").into_bytes()
    } else {
        format!("\x1b[1;{modifier}{final_character}").into_bytes()
    }
}

fn tilde_key(number: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn function_key(final_character: char, tilde_number: Option<u8>, modifier: u8) -> Vec<u8> {
    match (tilde_number, modifier) {
        (None, 1) => format!("\x1bO{final_character}").into_bytes(),
        (None, _) => format!("\x1b[1;{modifier}{final_character}").into_bytes(),
        (Some(number), 1) => format!("\x1b[{number}~").into_bytes(),
        (Some(number), _) => format!("\x1b[{number};{modifier}~").into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, key_char: Option<&str>, mods: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: mods,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn printable_chars() {
        let m = Modifiers::none();
        assert_eq!(
            keystroke_to_pty_bytes(&ks("a", Some("a"), m), TermMode::default()),
            b"a"
        );
        assert_eq!(
            keystroke_to_pty_bytes(&ks("A", Some("A"), m), TermMode::default()),
            b"A"
        );
        assert_eq!(
            keystroke_to_pty_bytes(&ks("1", Some("1"), m), TermMode::default()),
            b"1"
        );
    }

    #[test]
    fn enter_backspace_tab_escape() {
        let m = Modifiers::none();
        let mode = TermMode::default();
        assert_eq!(keystroke_to_pty_bytes(&ks("enter", None, m), mode), b"\r");
        assert_eq!(
            keystroke_to_pty_bytes(&ks("backspace", None, m), mode),
            b"\x7f"
        );
        assert_eq!(keystroke_to_pty_bytes(&ks("tab", None, m), mode), b"\t");
        assert_eq!(
            keystroke_to_pty_bytes(&ks("escape", None, m), mode),
            b"\x1b"
        );
    }

    #[test]
    fn arrow_keys() {
        let m = Modifiers::none();
        let mode = TermMode::default();
        assert_eq!(keystroke_to_pty_bytes(&ks("up", None, m), mode), b"\x1b[A");
        assert_eq!(
            keystroke_to_pty_bytes(&ks("down", None, m), mode),
            b"\x1b[B"
        );
        assert_eq!(
            keystroke_to_pty_bytes(&ks("right", None, m), mode),
            b"\x1b[C"
        );
        assert_eq!(
            keystroke_to_pty_bytes(&ks("left", None, m), mode),
            b"\x1b[D"
        );
    }

    #[test]
    fn ctrl_letters() {
        let m = Modifiers {
            control: true,
            ..Default::default()
        };
        let mode = TermMode::default();
        assert_eq!(keystroke_to_pty_bytes(&ks("c", None, m), mode), b"\x03");
        assert_eq!(keystroke_to_pty_bytes(&ks("a", None, m), mode), b"\x01");
        assert_eq!(keystroke_to_pty_bytes(&ks("z", None, m), mode), b"\x1a");
    }

    #[test]
    fn alt_prefix() {
        let m = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            keystroke_to_pty_bytes(&ks("b", Some("b"), m), TermMode::default()),
            b"\x1bb"
        );
    }

    #[test]
    fn shift_tab() {
        let m = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            keystroke_to_pty_bytes(&ks("tab", None, m), TermMode::default()),
            b"\x1b[Z"
        );
    }

    #[test]
    fn unicode_printable() {
        let m = Modifiers::none();
        // CJK 字符走 key_char 的 utf-8
        assert_eq!(
            keystroke_to_pty_bytes(&ks("中", Some("中"), m), TermMode::default()),
            "中".as_bytes()
        );
    }

    #[test]
    fn application_cursor_and_modified_keys() {
        let app_cursor = TermMode::default() | TermMode::APP_CURSOR;
        assert_eq!(
            keystroke_to_pty_bytes(&ks("up", None, Modifiers::none()), app_cursor),
            b"\x1bOA"
        );
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(
            keystroke_to_pty_bytes(&ks("up", None, ctrl), app_cursor),
            b"\x1b[1;5A"
        );
        assert_eq!(
            keystroke_to_pty_bytes(&ks("f5", None, ctrl), app_cursor),
            b"\x1b[15;5~"
        );
    }

    #[test]
    fn bracketed_and_plain_paste() {
        assert_eq!(encode_paste("a\nb", TermMode::default()), b"a\rb");
        assert_eq!(
            encode_paste("a\x1bb", TermMode::default() | TermMode::BRACKETED_PASTE),
            b"\x1b[200~ab\x1b[201~"
        );
    }
}
