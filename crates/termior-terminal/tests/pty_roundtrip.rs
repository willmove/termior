//! PTY 回环测试：验证 PTY spawn + reader 线程 + writer 通路（FR-TERM-01 验收）。
//!
//! Windows ConPTY/Shell 可能先发终端查询序列并等待回复，因此测试把输出送进真实
//! `alacritty_terminal::Term`，同时验证 PTY、协议回复与交互式 Shell 的完整往返。

use std::time::{Duration, Instant};

use alacritty_terminal::{
    term::{test::TermSize, Config as TermConfig, Term},
    vte::ansi::{Processor as VteProcessor, StdSyncHandler},
};
use termior_terminal::{
    PtyData, PtySessionConfig, TerminalBridge, TerminalEventProxy, WriterHandle,
};

fn collect_via_emulator_until<F: Fn(&str) -> bool>(
    rx: &mut futures::channel::mpsc::Receiver<PtyData>,
    writer: WriterHandle,
    pred: F,
    timeout: Duration,
) -> String {
    let (event_proxy, _events) = TerminalEventProxy::new(writer);
    let mut term = Term::new(TermConfig::default(), &TermSize::new(80, 24), event_proxy);
    let mut processor = VteProcessor::<StdSyncHandler>::default();
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(data) => {
                acc.push_str(&String::from_utf8_lossy(&data.bytes));
                processor.advance(&mut term, &data.bytes);
                if pred(&acc) {
                    return acc;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    acc
}

#[test]
fn spawn_produces_output() {
    // Windows 用 cmd，Unix 用 bash——都应能 spawn 并产生初始输出。
    let mut config = PtySessionConfig::default();
    #[cfg(windows)]
    {
        config.shell = Some(termior_terminal_core::ShellKind::Cmd);
    }
    #[cfg(unix)]
    {
        config.shell = Some(termior_terminal_core::ShellKind::Bash);
    }

    let mut bridge = TerminalBridge::spawn(&config).expect("PTY spawn");
    let writer = bridge.writer();
    let mut rx = bridge.take_output().expect("output channel");

    // PTY 一旦 spawn，shell/ConPTY 至少应产生一些字节（提示符、CPR 查询等）；
    // 这些字节必须经过模拟器，以便 CPR/设备查询得到即时回复。
    let initial = collect_via_emulator_until(
        &mut rx,
        writer.clone(),
        |s| !s.is_empty(),
        Duration::from_secs(3),
    );
    assert!(
        !initial.is_empty(),
        "PTY produced no output within 3s — reader/writer 通路异常"
    );

    // writer 写入后，命令应被回显并真正执行。
    let wres = writer.write_all(b"echo termior_pty_alive\r\n");
    assert!(wres.is_ok(), "writer write_all failed: {wres:?}");

    let after = collect_via_emulator_until(
        &mut rx,
        writer,
        |output| output.matches("termior_pty_alive").count() >= 2,
        Duration::from_secs(3),
    );
    assert!(
        after.matches("termior_pty_alive").count() >= 2,
        "shell did not execute and echo the marker: {after:?}"
    );

    let _ = bridge.kill();
}

#[test]
fn default_shell_roundtrip_handles_terminal_protocol_queries() {
    let mut bridge = TerminalBridge::spawn(&PtySessionConfig::default()).expect("default PTY");
    let writer = bridge.writer();
    let mut rx = bridge.take_output().expect("output channel");

    let initial = collect_via_emulator_until(
        &mut rx,
        writer.clone(),
        |output| !output.is_empty(),
        Duration::from_secs(5),
    );
    assert!(
        !initial.is_empty(),
        "default shell produced no terminal data"
    );

    writer
        .write_all(b"echo termior_default_shell_alive\r\n")
        .expect("write marker command");
    let output = collect_via_emulator_until(
        &mut rx,
        writer,
        |output| output.matches("termior_default_shell_alive").count() >= 2,
        Duration::from_secs(5),
    );
    assert!(
        output.matches("termior_default_shell_alive").count() >= 2,
        "default shell did not stay interactive and execute the marker: {output:?}"
    );

    let _ = bridge.kill();
}

#[test]
fn shell_integration_roundtrip_stays_interactive() {
    let config = PtySessionConfig {
        shell_integration: true,
        ..Default::default()
    };
    let mut bridge = TerminalBridge::spawn(&config).expect("integrated default PTY");
    let writer = bridge.writer();
    let mut rx = bridge.take_output().expect("output channel");

    let initial = collect_via_emulator_until(
        &mut rx,
        writer.clone(),
        |output| !output.is_empty(),
        Duration::from_secs(5),
    );
    assert!(
        !initial.is_empty(),
        "shell integration produced no terminal data"
    );

    writer
        .write_all(b"echo termior_integrated_shell_alive\r\n")
        .expect("write integration marker command");
    let output = collect_via_emulator_until(
        &mut rx,
        writer,
        |output| output.matches("termior_integrated_shell_alive").count() >= 2,
        Duration::from_secs(5),
    );
    assert!(
        output.matches("termior_integrated_shell_alive").count() >= 2,
        "integrated shell did not remain interactive: {output:?}"
    );

    let _ = bridge.kill();
}

#[test]
fn spawn_with_explicit_size() {
    let config = PtySessionConfig {
        rows: 30,
        cols: 120,
        ..Default::default()
    };
    let bridge = TerminalBridge::spawn(&config).expect("PTY spawn");
    // resize 应不报错。
    let r = bridge.resize(40, 160);
    assert!(r.is_ok(), "resize failed: {r:?}");
    drop(bridge);
}
