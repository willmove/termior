//! Inline completion state (FR-EDIT-05). Network requests live in `termior-ai`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionState {
    Disabled,
    Idle,
    Waiting { revision: u64 },
    Showing { revision: u64, ghost_text: String },
}

#[derive(Debug, Clone)]
pub struct CompletionController {
    state: CompletionState,
}

impl CompletionController {
    pub fn new(enabled: bool) -> Self {
        Self {
            state: if enabled {
                CompletionState::Idle
            } else {
                CompletionState::Disabled
            },
        }
    }

    pub fn state(&self) -> &CompletionState {
        &self.state
    }

    /// 当前正在显示的 ghost text（未显示时为 `None`），供渲染层读取。
    pub fn ghost_text(&self) -> Option<&str> {
        match &self.state {
            CompletionState::Showing { ghost_text, .. } => Some(ghost_text),
            _ => None,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.state = if enabled {
            CompletionState::Idle
        } else {
            CompletionState::Disabled
        };
    }

    pub fn request(&mut self, revision: u64) -> bool {
        if matches!(self.state, CompletionState::Disabled) {
            return false;
        }
        self.state = CompletionState::Waiting { revision };
        true
    }

    pub fn receive(&mut self, revision: u64, text: impl Into<String>) -> bool {
        if !matches!(self.state, CompletionState::Waiting { revision: r } if r == revision) {
            return false;
        }
        let text = text.into();
        self.state = if text.is_empty() {
            CompletionState::Idle
        } else {
            CompletionState::Showing {
                revision,
                ghost_text: text,
            }
        };
        true
    }

    pub fn typed(&mut self) {
        self.reset_to_idle();
    }

    /// Drop any showing ghost text and cancel a pending request (Esc / IME 预编辑)。
    pub fn dismiss(&mut self) {
        self.reset_to_idle();
    }

    /// 取消在途请求（连续输入时调用）：等待中的请求即使稍后返回也会被 `receive` 拒绝。
    pub fn cancel(&mut self) {
        self.reset_to_idle();
    }

    /// 把状态机回到 Idle（Disabled 保持不变）。`dismiss`/`cancel`/`typed` 共用同一语义。
    fn reset_to_idle(&mut self) {
        if !matches!(self.state, CompletionState::Disabled) {
            self.state = CompletionState::Idle;
        }
    }

    pub fn accept(&mut self, current_revision: u64) -> Option<String> {
        let accepted = match &self.state {
            CompletionState::Showing {
                revision,
                ghost_text,
            } if *revision == current_revision => Some(ghost_text.clone()),
            _ => None,
        };
        self.reset_to_idle();
        accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_completion_is_rejected() {
        let mut c = CompletionController::new(true);
        assert!(c.request(3));
        assert!(!c.receive(2, "stale"));
        assert!(c.receive(3, "ghost"));
        assert_eq!(c.accept(4), None);
    }

    #[test]
    fn tab_accepts_matching_revision() {
        let mut c = CompletionController::new(true);
        c.request(7);
        c.receive(7, "world");
        assert_eq!(c.accept(7).as_deref(), Some("world"));
        assert_eq!(c.state(), &CompletionState::Idle);
    }

    #[test]
    fn ghost_text_visible_only_when_showing() {
        let mut c = CompletionController::new(true);
        assert!(c.ghost_text().is_none());
        c.request(1);
        assert!(c.ghost_text().is_none());
        c.receive(1, "abc");
        assert_eq!(c.ghost_text(), Some("abc"));
    }

    #[test]
    fn dismiss_clears_showing_text() {
        let mut c = CompletionController::new(true);
        c.request(2);
        c.receive(2, "ghost");
        c.dismiss();
        assert_eq!(c.state(), &CompletionState::Idle);
        assert!(c.ghost_text().is_none());
    }

    #[test]
    fn cancel_after_request_makes_receive_a_noop() {
        let mut c = CompletionController::new(true);
        c.request(5);
        c.cancel();
        assert_eq!(c.state(), &CompletionState::Idle);
        // 取消后到达的过时响应被忽略。
        assert!(!c.receive(5, "late"));
        assert!(c.ghost_text().is_none());
    }

    #[test]
    fn disabled_controller_ignores_requests() {
        let mut c = CompletionController::new(false);
        assert!(!c.request(1));
        c.receive(1, "x");
        assert!(c.ghost_text().is_none());
    }
}
