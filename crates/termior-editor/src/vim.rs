//! Vim interaction state independent from GPUI key dispatch (FR-EDIT-06).

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
    CommandLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VimCommand {
    Move { motion: Motion, count: usize },
    DeleteMotion { motion: Motion, count: usize },
    ChangeMotion { motion: Motion, count: usize },
    YankMotion { motion: Motion, count: usize },
    PasteAfter { count: usize },
    Undo,
    Redo,
    EnterMode(VimMode),
    SetMark(char),
    JumpMark(char),
    Execute(String),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct VimEngine {
    mode: VimMode,
    pending: String,
    count: usize,
    command_line: String,
    registers: HashMap<char, String>,
    marks: HashMap<char, usize>,
}

impl Default for VimEngine {
    fn default() -> Self {
        Self {
            mode: VimMode::Normal,
            pending: String::new(),
            count: 0,
            command_line: String::new(),
            registers: HashMap::new(),
            marks: HashMap::new(),
        }
    }
}

impl VimEngine {
    pub fn mode(&self) -> VimMode {
        self.mode
    }

    pub fn command_line(&self) -> &str {
        &self.command_line
    }

    pub fn feed(&mut self, key: &str) -> Option<VimCommand> {
        if self.mode == VimMode::Insert {
            if key == "esc" {
                self.mode = VimMode::Normal;
                return Some(VimCommand::EnterMode(VimMode::Normal));
            }
            return None;
        }
        if self.mode == VimMode::CommandLine {
            return self.feed_command_line(key);
        }
        if key == "esc" {
            self.pending.clear();
            self.count = 0;
            self.mode = VimMode::Normal;
            return Some(VimCommand::Cancel);
        }
        if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() {
            let digit = key.parse::<usize>().ok()?;
            // A leading zero with no pending operator is the `0` motion. After an operator it is
            // a valid count digit (`d0` is parsed as the motion below because count is still zero).
            if digit != 0 || self.count != 0 {
                self.count = self.count.saturating_mul(10).saturating_add(digit);
                return None;
            }
        }
        if self.pending.is_empty() {
            match key {
                "i" => return Some(self.enter(VimMode::Insert)),
                "v" => return Some(self.enter(VimMode::Visual)),
                "V" => return Some(self.enter(VimMode::VisualLine)),
                "ctrl-v" => return Some(self.enter(VimMode::VisualBlock)),
                ":" => {
                    self.command_line.clear();
                    return Some(self.enter(VimMode::CommandLine));
                }
                "u" => return Some(VimCommand::Undo),
                "ctrl-r" => return Some(VimCommand::Redo),
                "p" => {
                    return Some(VimCommand::PasteAfter {
                        count: self.take_count(),
                    })
                }
                "d" | "c" | "y" | "m" | "'" => {
                    self.pending.push_str(key);
                    return None;
                }
                _ => {
                    if let Some(motion) = motion_for(key) {
                        return Some(VimCommand::Move {
                            motion,
                            count: self.take_count(),
                        });
                    }
                }
            }
        } else {
            let operator = std::mem::take(&mut self.pending);
            if operator == "m" || operator == "'" {
                self.count = 0;
                let mark = key.chars().next()?;
                return Some(if operator == "m" {
                    VimCommand::SetMark(mark)
                } else {
                    VimCommand::JumpMark(mark)
                });
            }
            if operator == key && matches!(operator.as_str(), "d" | "c" | "y") {
                let motion = Motion::Down;
                let count = self.take_count();
                return Some(match operator.as_str() {
                    "d" => VimCommand::DeleteMotion { motion, count },
                    "c" => VimCommand::ChangeMotion { motion, count },
                    _ => VimCommand::YankMotion { motion, count },
                });
            }
            if let Some(motion) = motion_for(key) {
                let count = self.take_count();
                return Some(match operator.as_str() {
                    "d" => VimCommand::DeleteMotion { motion, count },
                    "c" => VimCommand::ChangeMotion { motion, count },
                    "y" => VimCommand::YankMotion { motion, count },
                    _ => return None,
                });
            }
        }
        self.pending.clear();
        self.count = 0;
        None
    }

    pub fn set_register(&mut self, name: char, value: impl Into<String>) {
        self.registers.insert(name, value.into());
    }

    pub fn register(&self, name: char) -> Option<&str> {
        self.registers.get(&name).map(String::as_str)
    }

    pub fn set_mark_position(&mut self, name: char, char_index: usize) {
        self.marks.insert(name, char_index);
    }

    pub fn mark_position(&self, name: char) -> Option<usize> {
        self.marks.get(&name).copied()
    }

    fn enter(&mut self, mode: VimMode) -> VimCommand {
        self.mode = mode;
        self.count = 0;
        VimCommand::EnterMode(mode)
    }

    fn take_count(&mut self) -> usize {
        let count = self.count.max(1);
        self.count = 0;
        count
    }

    fn feed_command_line(&mut self, key: &str) -> Option<VimCommand> {
        match key {
            "esc" => {
                self.mode = VimMode::Normal;
                Some(VimCommand::Cancel)
            }
            "enter" => {
                self.mode = VimMode::Normal;
                Some(VimCommand::Execute(std::mem::take(&mut self.command_line)))
            }
            "backspace" => {
                self.command_line.pop();
                None
            }
            text => {
                self.command_line.push_str(text);
                None
            }
        }
    }
}

fn motion_for(key: &str) -> Option<Motion> {
    Some(match key {
        "h" => Motion::Left,
        "l" => Motion::Right,
        "k" => Motion::Up,
        "j" => Motion::Down,
        "w" => Motion::WordForward,
        "b" => Motion::WordBackward,
        "e" => Motion::WordEnd,
        "0" => Motion::LineStart,
        "$" => Motion::LineEnd,
        "gg" => Motion::DocumentStart,
        "G" => Motion::DocumentEnd,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ciw_and_d2j() {
        let mut vim = VimEngine::default();
        assert_eq!(vim.feed("c"), None);
        assert_eq!(
            vim.feed("w"),
            Some(VimCommand::ChangeMotion {
                motion: Motion::WordForward,
                count: 1
            })
        );
        assert_eq!(vim.feed("d"), None);
        assert_eq!(vim.feed("2"), None);
        assert_eq!(
            vim.feed("j"),
            Some(VimCommand::DeleteMotion {
                motion: Motion::Down,
                count: 2
            })
        );
    }

    #[test]
    fn visual_block_and_command_line() {
        let mut vim = VimEngine::default();
        assert_eq!(
            vim.feed("ctrl-v"),
            Some(VimCommand::EnterMode(VimMode::VisualBlock))
        );
        vim.feed("esc");
        vim.feed(":");
        vim.feed("w");
        assert_eq!(vim.feed("enter"), Some(VimCommand::Execute("w".into())));
    }

    #[test]
    fn registers_and_marks_persist() {
        let mut vim = VimEngine::default();
        vim.set_register('a', "copied");
        vim.set_mark_position('q', 42);
        assert_eq!(vim.register('a'), Some("copied"));
        assert_eq!(vim.mark_position('q'), Some(42));
    }
}
