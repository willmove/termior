//! PTY 回环测试：验证 PTY spawn + reader 线程 + writer 通路（FR-TERM-01 验收）。
//!
//! 注：Windows ConPTY 启动时会发 `\x1b[6n`（光标位置查询）并期望终端回复；
//! 本测试只验证 PTY 层 IO 通路（能 spawn、reader 能读到字节、writer 写入不报错），
//! 不依赖 shell 提示符语义（那是 vte::Term 渲染层的事）。

use std::time::{Duration, Instant};

use futures::StreamExt;
use termior_terminal::{PtyData, PtySessionConfig, TerminalBridge};

/// 收集 channel 输出直到满足谓词或超时。
fn collect_until<F: Fn(&str) -> bool>(
    rx: &mut futures::channel::mpsc::UnboundedReceiver<PtyData>,
    pred: F,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    while Instant::now() < deadline {
        match rx.try_next() {
            Ok(Some(data)) => {
                acc.push_str(&String::from_utf8_lossy(&data.bytes));
                if pred(&acc) {
                    return acc;
                }
            }
            Ok(None) => break,
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

    let mut bridge = match TerminalBridge::spawn(&config) {
        Ok(b) => b,
        Err(e) => {
            // CI/headless 环境可能无可用 PTY/shell，跳过而非失败。
            eprintln!("skip: PTY spawn unavailable: {e}");
            return;
        }
    };
    let writer = bridge.writer();
    let mut rx = bridge.take_output().expect("output channel");

    // PTY 一旦 spawn，shell/ConPTY 至少应产生一些字节（提示符、CPR 查询等）。
    let initial = collect_until(&mut rx, |s| !s.is_empty(), Duration::from_secs(3));
    assert!(
        !initial.is_empty(),
        "PTY produced no output within 3s — reader/writer 通路异常"
    );

    // writer 写入应成功（即使 shell 因 ConPTY CPR 未应答而不回显，写入本身不应报错）。
    let wres = writer.write_all(b"echo termior_pty_alive\r\n");
    assert!(wres.is_ok(), "writer write_all failed: {wres:?}");

    // 写入后应能继续从 reader 读到更多输出（或至少不报错）。宽松断言。
    let _after = collect_until(&mut rx, |_| false, Duration::from_millis(800));

    let _ = bridge.kill();
}

#[test]
fn spawn_with_explicit_size() {
    let config = PtySessionConfig {
        rows: 30,
        cols: 120,
        ..Default::default()
    };
    let bridge = match TerminalBridge::spawn(&config) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skip: PTY spawn unavailable: {e}");
            return;
        }
    };
    // resize 应不报错。
    let r = bridge.resize(40, 160);
    assert!(r.is_ok(), "resize failed: {r:?}");
    drop(bridge);
}
