//! 设置页共用的小函数。

use super::*;

pub(super) fn bytes_to_mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

pub(super) fn on_off(value: bool) -> SharedString {
    if value {
        t!("settings.common.on")
    } else {
        t!("settings.common.off")
    }
}

pub(super) fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}
