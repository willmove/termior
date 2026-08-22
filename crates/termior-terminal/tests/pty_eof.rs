//! 回归测试：shell 退出（如用户键入 exit）后，PTY 输出流必须随之结束，
//! 上层 UI 依赖该流结束信号关闭窗格。
//!
//! Windows 上 ConPTY 在子进程退出后不会关闭输出管道（reader 永远等不到 EOF），
//! 该信号由 `TerminalBridge` 的子进程监听线程提供。

use std::time::{Duration, Instant};

use alacritty_terminal::{
    term::{test::TermSize, Config as TermConfig, Term},
    vte::ansi::{Processor as VteProcessor, StdSyncHandler},
};
use futures::StreamExt;
use termior_terminal::{PtySessionConfig, TerminalBridge, TerminalEventProxy};

/// 与 pty_roundtrip 相同：Windows 上等首个提示符（PowerShell 启动期会丢弃过早
/// 送达的输入）；Unix 上任何输出即视为就绪。
fn shell_is_ready(output: &str) -> bool {
    #[cfg(windows)]
    {
        output.contains('>')
    }
    #[cfg(not(windows))]
    {
        !output.is_empty()
    }
}

fn assert_exit_ends_stream(config: PtySessionConfig) {
    let mut bridge = TerminalBridge::spawn(&config).unwrap();
    let writer = bridge.writer();
    let mut rx = bridge.take_output().expect("output channel");

    // shell 启动期会发终端查询序列并等待回复，不经模拟器回复 PowerShell 会一直停在
    // 启动阶段。与生产路径一致：字节喂给 alacritty Term，其 event proxy 同步回复。
    let (event_proxy, _events) = TerminalEventProxy::new(writer.clone());
    let mut term = Term::new(TermConfig::default(), &TermSize::new(80, 24), event_proxy);
    let mut processor = VteProcessor::<StdSyncHandler>::default();

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut acc = String::new();
    while Instant::now() < deadline && !shell_is_ready(&acc) {
        match rx.try_recv() {
            Ok(data) => {
                acc.push_str(&String::from_utf8_lossy(&data.bytes));
                processor.advance(&mut term, &data.bytes);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    assert!(
        shell_is_ready(&acc),
        "shell did not become interactive: {acc:?}"
    );

    writer
        .write_all(b"exit\r\n")
        .expect("write exit to the shell");

    // 继续消费（同样喂模拟器，回复后续查询）直到流结束。
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        futures::executor::block_on(async {
            while let Some(data) = rx.next().await {
                processor.advance(&mut term, &data.bytes);
            }
        });
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("PTY output stream must end shortly after the shell exits");
}

#[test]
fn shell_exit_ends_the_output_stream() {
    assert_exit_ends_stream(PtySessionConfig::default());
}

/// 生产路径实际使用 shell integration（pwsh 以 -NoExit -File 启动），
/// 该配置下 exit 同样必须能终止会话并结束输出流。
#[test]
fn integrated_shell_exit_ends_the_output_stream() {
    let config = PtySessionConfig {
        shell_integration: true,
        ..Default::default()
    };
    assert_exit_ends_stream(config);
}
