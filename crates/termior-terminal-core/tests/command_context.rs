use termior_terminal_core::{OscCommandTracker, OscEvent, OutputCursor, TerminalContextReference};

#[test]
fn osc_command_boundaries_create_structured_records_without_guessing() {
    let mut tracker = OscCommandTracker::new("terminal-1", "/project");
    tracker.observe(
        &OscEvent::CommandStart {
            cmd: "cargo test".into(),
        },
        OutputCursor { sequence: 10 },
    );
    tracker.observe(
        &OscEvent::CommandExit { code: Some(1) },
        OutputCursor { sequence: 28 },
    );

    let record = tracker.records().last().unwrap();
    assert_eq!(record.command.as_deref(), Some("cargo test"));
    assert_eq!(record.exit_code, Some(1));
    assert_eq!(record.output_start.sequence, 10);
    assert_eq!(record.output_end.unwrap().sequence, 28);
}

#[test]
fn selection_without_shell_integration_keeps_unknown_fields_unknown() {
    let reference = TerminalContextReference::selection(
        "terminal-plain",
        "/project",
        OutputCursor { sequence: 4 },
        OutputCursor { sequence: 9 },
        "compiler output",
    );
    assert_eq!(reference.command_session_id, None);
    assert_eq!(reference.command, None);
    assert_eq!(reference.exit_code, None);
    assert_eq!(reference.output_start.sequence, 4);
    assert_eq!(reference.output_end.sequence, 9);
}

fn run(tracker: &mut OscCommandTracker, cmd: &str, output: &[u8], code: i32) {
    tracker.observe(
        &OscEvent::CommandStart { cmd: cmd.into() },
        OutputCursor { sequence: 0 },
    );
    tracker.observe_output(output);
    tracker.observe(
        &OscEvent::CommandExit { code: Some(code) },
        OutputCursor { sequence: 0 },
    );
}

#[test]
fn command_output_is_captured_without_control_sequences() {
    let mut tracker = OscCommandTracker::new("t", "/project");
    tracker.observe_output(b"prompt text is not captured ");
    run(
        &mut tracker,
        "cargo test",
        b"\x1b[32mok\x1b[0m\r\n\x1b]8;;file:///x\x07link\x1b]8;;\x1b\\\r\nprogress 10%\rprogress 100%\n",
        0,
    );
    let record = tracker.records().last().unwrap();
    assert_eq!(
        tracker.output_of(&record.id).as_deref(),
        Some("ok\nlink\nprogress 100%\n")
    );
}

#[test]
fn escape_sequences_split_across_batches_are_still_stripped() {
    let mut tracker = OscCommandTracker::new("t", "/");
    tracker.observe(
        &OscEvent::CommandStart { cmd: "ls".into() },
        OutputCursor { sequence: 0 },
    );
    tracker.observe_output(b"a\x1b[3");
    tracker.observe_output(b"1mb\x1b]0;ti");
    tracker.observe_output(b"tle\x07c");
    let id = tracker.active().unwrap().id.clone();
    assert_eq!(tracker.output_of(&id).as_deref(), Some("abc"));
}

#[test]
fn selections_resolve_to_the_newest_command_that_printed_them() {
    let mut tracker = OscCommandTracker::new("t", "/project");
    run(&mut tracker, "make", b"error: missing  semicolon\n", 2);
    run(&mut tracker, "ls", b"src\ntarget\n", 0);
    let found = tracker.find_by_output("missing semicolon").unwrap();
    assert_eq!(found.command.as_deref(), Some("make"));
    assert_eq!(found.exit_code, Some(2));
    assert_eq!(found.cwd, "/project");
    assert!(tracker.find_by_output("not printed anywhere").is_none());
    assert!(tracker.find_by_output("   ").is_none());
    assert_eq!(
        tracker.last_failed().unwrap().command.as_deref(),
        Some("make")
    );
}

#[test]
fn records_and_captures_are_bounded() {
    let mut tracker = OscCommandTracker::new("t", "/");
    for index in 0..300 {
        run(&mut tracker, &format!("echo {index}"), b"x\n", 0);
    }
    assert_eq!(tracker.records().len(), 256);
    let oldest = tracker.records().first().unwrap().id.clone();
    assert!(tracker.output_of(&oldest).is_some());
    assert!(
        tracker.output_of("t-command-1").is_none(),
        "evicted capture is dropped"
    );

    tracker.observe(
        &OscEvent::CommandStart { cmd: "yes".into() },
        OutputCursor { sequence: 0 },
    );
    tracker.observe_output(&vec![b'y'; 100 * 1024]);
    let id = tracker.active().unwrap().id.clone();
    assert_eq!(tracker.output_of(&id).unwrap().len(), 32 * 1024);
}
