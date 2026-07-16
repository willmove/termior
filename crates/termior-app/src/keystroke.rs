//! 键盘按键 → PTY 字节序列映射（FR-TERM-01 键盘输入）。
//!
//! 把 GPUI `Keystroke` 编码成终端期望的字节流，遵循 xterm 语义：
//! - 可打印字符：直接发 utf-8 字节（优先用 `key_char`，回退 `key`）。
//! - 功能键：Enter/Backspace/Tab/Esc/方向键/F1-F12 → 对应控制字符或 CSI 序列。
//! - Ctrl+字母：Ctrl+A..Z → 0x01..0x1a。
//!
//! 修饰键（Alt/Meta）前缀 `\x1b`；Shift 主要改变 `key_char`（由 GPUI 给出）。

use gpui::Keystroke;

/// 把一次按键编码成要写入 PTY 的字节。返回空 Vec 表示该键不产生输入
/// （如纯修饰键按下、未识别的组合）。
pub fn keystroke_to_pty_bytes(ks: &Keystroke) -> Vec<u8> {
    let m = &ks.modifiers;

    // Ctrl + 字母 → 控制字符 0x01..0x1a（仅 control，无 alt/shift/platform/function）
    if m.control && !m.alt && !m.platform && !m.function {
        let key = ks.key.to_lowercase();
        if let Some(c) = key.chars().next() {
            if c.is_ascii_lowercase() {
                let code = (c as u8) - b'a' + 1;
                let mut bytes = vec![code];
                if m.shift {
                    // Ctrl+Shift+字母 不常用，仍按 control code，保持一致
                }
                if m.alt {
                    bytes.insert(0, 0x1b);
                }
                return bytes;
            }
        }
    }

    // 功能键（无 control 修饰，或只带 shift/alt）
    if let Some(bytes) = map_function_key(ks) {
        return bytes;
    }

    // 可打印字符：优先 key_char（GPUI 已处理 shift/死键等），回退 key。
    if let Some(ch) = &ks.key_char {
        if !ch.is_empty() {
            let mut bytes = ch.as_bytes().to_vec();
            if m.alt {
                bytes.insert(0, 0x1b); // Alt 前缀 ESC
            }
            return bytes;
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

/// 功能键映射：返回 None 表示不是功能键。
fn map_function_key(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    // 功能键只在无 control 修饰时生效（Ctrl+方向键等走 application 模式，M1 暂不处理）。
    if m.control {
        return None;
    }
    let alt_prefix = |mut v: Vec<u8>| -> Vec<u8> {
        if m.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let shift = m.shift;
    match ks.key.as_str() {
        "enter" | "return" => Some(alt_prefix(vec![b'\r'])),
        "backspace" => Some(alt_prefix(vec![0x7f])),
        "tab" => Some(alt_prefix(if shift {
            vec![0x1b, b'[', b'Z']
        } else {
            vec![b'\t']
        })),
        "escape" => Some(vec![0x1b]),
        "space" if ks.key_char.is_none() => Some(alt_prefix(vec![b' '])),
        "left" => Some(alt_prefix(csi_arrow(b'D', shift))),
        "right" => Some(alt_prefix(csi_arrow(b'C', shift))),
        "up" => Some(alt_prefix(csi_arrow(b'A', shift))),
        "down" => Some(alt_prefix(csi_arrow(b'B', shift))),
        "home" => Some(alt_prefix(vec![0x1b, b'[', b'H'])),
        "end" => Some(alt_prefix(vec![0x1b, b'[', b'F'])),
        "delete" => Some(alt_prefix(vec![0x1b, b'[', b'3', b'~'])),
        "pageup" => Some(alt_prefix(vec![0x1b, b'[', b'5', b'~'])),
        "pagedown" => Some(alt_prefix(vec![0x1b, b'[', b'6', b'~'])),
        "insert" => Some(alt_prefix(vec![0x1b, b'[', b'2', b'~'])),
        _ => None,
    }
}

/// 方向键 CSI 序列。Shift+方向键走 xterm 的 "modifyOtherKeys" 精确序列。
fn csi_arrow(letter: u8, shift: bool) -> Vec<u8> {
    if shift {
        vec![0x1b, b'[', b'1', b';', b'2', letter]
    } else {
        vec![0x1b, b'[', letter]
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
        assert_eq!(keystroke_to_pty_bytes(&ks("a", Some("a"), m)), b"a");
        assert_eq!(keystroke_to_pty_bytes(&ks("A", Some("A"), m)), b"A");
        assert_eq!(keystroke_to_pty_bytes(&ks("1", Some("1"), m)), b"1");
    }

    #[test]
    fn enter_backspace_tab_escape() {
        let m = Modifiers::none();
        assert_eq!(keystroke_to_pty_bytes(&ks("enter", None, m)), b"\r");
        assert_eq!(keystroke_to_pty_bytes(&ks("backspace", None, m)), b"\x7f");
        assert_eq!(keystroke_to_pty_bytes(&ks("tab", None, m)), b"\t");
        assert_eq!(keystroke_to_pty_bytes(&ks("escape", None, m)), b"\x1b");
    }

    #[test]
    fn arrow_keys() {
        let m = Modifiers::none();
        assert_eq!(keystroke_to_pty_bytes(&ks("up", None, m)), b"\x1b[A");
        assert_eq!(keystroke_to_pty_bytes(&ks("down", None, m)), b"\x1b[B");
        assert_eq!(keystroke_to_pty_bytes(&ks("right", None, m)), b"\x1b[C");
        assert_eq!(keystroke_to_pty_bytes(&ks("left", None, m)), b"\x1b[D");
    }

    #[test]
    fn ctrl_letters() {
        let m = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(keystroke_to_pty_bytes(&ks("c", None, m)), b"\x03");
        assert_eq!(keystroke_to_pty_bytes(&ks("a", None, m)), b"\x01");
        assert_eq!(keystroke_to_pty_bytes(&ks("z", None, m)), b"\x1a");
    }

    #[test]
    fn alt_prefix() {
        let m = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert_eq!(keystroke_to_pty_bytes(&ks("b", Some("b"), m)), b"\x1bb");
    }

    #[test]
    fn shift_tab() {
        let m = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(keystroke_to_pty_bytes(&ks("tab", None, m)), b"\x1b[Z");
    }

    #[test]
    fn unicode_printable() {
        let m = Modifiers::none();
        // CJK 字符走 key_char 的 utf-8
        assert_eq!(
            keystroke_to_pty_bytes(&ks("中", Some("中"), m)),
            "中".as_bytes()
        );
    }
}
