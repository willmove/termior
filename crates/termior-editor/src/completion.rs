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
        if !matches!(self.state, CompletionState::Disabled) {
            self.state = CompletionState::Idle;
        }
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
}
