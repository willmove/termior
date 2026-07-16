//! Reader-thread OSC interception. Recognized Termior OSC sequences are emitted as structured
//! events and removed before bytes reach the VTE grid.

use crate::osc::{OscEvent, OscParser};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FilteredOutput {
    pub visible: Vec<u8>,
    pub events: Vec<OscEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Osc,
    OscEscape,
}

#[derive(Debug)]
pub struct OscStreamFilter {
    state: State,
    sequence: Vec<u8>,
    parser: OscParser,
}

impl Default for OscStreamFilter {
    fn default() -> Self {
        Self {
            state: State::Ground,
            sequence: Vec::new(),
            parser: OscParser::new(),
        }
    }
}

impl OscStreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, input: &[u8]) -> FilteredOutput {
        let mut output = FilteredOutput::default();
        for &byte in input {
            match self.state {
                State::Ground if byte == 0x1b => self.state = State::Escape,
                State::Ground => output.visible.push(byte),
                State::Escape if byte == b']' => {
                    self.sequence.clear();
                    self.sequence.extend_from_slice(b"\x1b]");
                    self.state = State::Osc;
                }
                State::Escape => {
                    output.visible.push(0x1b);
                    output.visible.push(byte);
                    self.state = State::Ground;
                }
                State::Osc if byte == 0x07 => {
                    self.sequence.push(byte);
                    self.finish_sequence(&mut output);
                }
                State::Osc if byte == 0x1b => {
                    self.sequence.push(byte);
                    self.state = State::OscEscape;
                }
                State::Osc => {
                    self.sequence.push(byte);
                    if self.sequence.len() > 64 * 1024 {
                        output.visible.append(&mut self.sequence);
                        self.state = State::Ground;
                    }
                }
                State::OscEscape if byte == b'\\' => {
                    self.sequence.push(byte);
                    self.finish_sequence(&mut output);
                }
                State::OscEscape => {
                    self.sequence.push(byte);
                    self.state = State::Osc;
                }
            }
        }
        output
    }

    fn finish_sequence(&mut self, output: &mut FilteredOutput) {
        let events = self.parser.feed(&self.sequence);
        if events.is_empty() {
            output.visible.append(&mut self.sequence);
        } else {
            output.events.extend(events);
            self.sequence.clear();
        }
        self.state = State::Ground;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::{AgentState, PromptMark};

    #[test]
    fn strips_recognized_osc_but_preserves_surrounding_bytes() {
        let mut filter = OscStreamFilter::new();
        let out = filter.feed(b"before\x1b]7;file:///work\x07after");
        assert_eq!(out.visible, b"beforeafter");
        assert!(matches!(out.events.as_slice(), [OscEvent::Cwd { path, .. }] if path == "/work"));
    }

    #[test]
    fn preserves_unrelated_title_osc_for_vte() {
        let mut filter = OscStreamFilter::new();
        let input = b"\x1b]0;title\x07";
        assert_eq!(filter.feed(input).visible, input);
    }

    #[test]
    fn handles_split_sequence_and_st_terminator() {
        let mut filter = OscStreamFilter::new();
        assert_eq!(filter.feed(b"x\x1b]133;").visible, b"x");
        let out = filter.feed(b"A\x1b\\y");
        assert_eq!(out.visible, b"y");
        assert_eq!(out.events, vec![OscEvent::Prompt(PromptMark::PromptStart)]);
    }

    #[test]
    fn explicit_agent_event_only() {
        let mut filter = OscStreamFilter::new();
        let out = filter.feed(b"noise\x1b]777;notify;Termior;working\x07");
        assert_eq!(out.visible, b"noise");
        assert_eq!(out.events, vec![OscEvent::AgentEvent(AgentState::Working)]);
    }
}
