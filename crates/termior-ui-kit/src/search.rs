use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchOptions {
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchOverlay {
    pub visible: bool,
    pub query: String,
    pub options: SearchOptions,
    pub current: usize,
    pub total: usize,
}

impl SearchOverlay {
    pub fn open(&mut self) {
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn set_results(&mut self, total: usize) {
        self.total = total;
        self.current = self.current.min(total.saturating_sub(1));
    }

    pub fn next(&mut self, backwards: bool) {
        if self.total == 0 {
            self.current = 0;
        } else if backwards {
            self.current = (self.current + self.total - 1) % self.total;
        } else {
            self.current = (self.current + 1) % self.total;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_navigation_wraps() {
        let mut overlay = SearchOverlay::default();
        overlay.set_results(3);
        overlay.next(true);
        assert_eq!(overlay.current, 2);
        overlay.next(false);
        assert_eq!(overlay.current, 0);
    }
}
