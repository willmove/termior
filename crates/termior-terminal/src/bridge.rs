//! PTY ↔ 消费方桥接（FR-TERM-01/03）。
//!
//! [`TerminalBridge`] 持有 [`PtySession`]，在独立线程里循环读取 PTY 输出，
//! 通过 futures channel 把原始字节投递给消费方（GPUI 主线程）。
//! reader 线程只做 IO + 字节搬运，不触碰 alacritty `Term`（`Term` 非 `Send`）。

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event as TerminalEvent, EventListener};
use futures::channel::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender};
use futures::SinkExt;

use crate::pty::{PtySession, PtySessionConfig, SpawnError};
use termior_preview::LocalhostDetector;
use termior_terminal_core::{shell_integration::ShellKind, OscEvent, OscStreamFilter};

/// 从 PTY reader 线程流向消费方的一批数据。
#[derive(Debug)]
pub struct PtyData {
    /// PTY output with Termior OSC 7/133/777 removed, ready for the VTE parser.
    pub bytes: Vec<u8>,
    pub events: Vec<OscEvent>,
    pub localhost_urls: Vec<String>,
}

/// Bridges terminal-emulator events back to the PTY and application UI.
///
/// Protocol replies (`PtyWrite`) are written synchronously so device-status and cursor-position
/// queries cannot deadlock a shell while the UI event loop is waiting for more PTY output. Other
/// events are forwarded to the UI for clipboard, title, bell, color, and size handling.
#[derive(Clone)]
pub struct TerminalEventProxy {
    writer: WriterHandle,
    event_tx: UnboundedSender<TerminalEvent>,
}

impl TerminalEventProxy {
    pub fn new(writer: WriterHandle) -> (Self, UnboundedReceiver<TerminalEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded();
        (Self { writer, event_tx }, event_rx)
    }
}

impl EventListener for TerminalEventProxy {
    fn send_event(&self, event: TerminalEvent) {
        match event {
            TerminalEvent::PtyWrite(text) => {
                if let Err(error) = self.writer.write_all(text.as_bytes()) {
                    log::warn!("terminal protocol reply failed: {error}");
                }
            }
            event => {
                let _ = self.event_tx.unbounded_send(event);
            }
        }
    }
}

/// PTY 会话 + reader 线程 + 输出 channel 的统一句柄。
///
/// 典型用法：
/// ```ignore
/// let mut bridge = TerminalBridge::spawn(&config)?;
/// let writer = bridge.writer();
/// let mut rx = bridge.take_output();   // 主线程消费
/// // 主线程循环：while let Some(data) = rx.next().await { vte_parser.advance(&mut term, &data.bytes); }
/// ```
pub struct TerminalBridge {
    session: PtySession,
    output_rx: Option<Receiver<PtyData>>,
    /// 持有 reader 线程句柄，drop 时自然分离（reader 线程在 PTY EOF 后退出）。
    _reader: thread::JoinHandle<()>,
    /// 子进程退出监听线程句柄（退出时关闭输出 channel，drop 时自然分离）。
    _watcher: thread::JoinHandle<()>,
}

impl TerminalBridge {
    /// spawn PTY 会话并启动 reader 线程；返回的 bridge 持有输出 channel 接收端。
    pub fn spawn(config: &PtySessionConfig) -> Result<Self, SpawnError> {
        let mut session = PtySession::spawn(config)?;
        let mut reader = session.take_reader()?;
        // Bound queued output so a fast producer (for example `cat` on a large file) applies
        // backpressure instead of growing application memory without limit.
        let (output_tx, output_rx) = mpsc::channel::<PtyData>(64);
        let mut exit_signaler = output_tx.clone();

        let handle = thread::Builder::new()
            .name("termior-pty-reader".into())
            .spawn(move || {
                run_reader(&mut reader, output_tx);
            })
            .map_err(|e| SpawnError::Open(format!("reader thread: {e}")))?;

        // Windows 上 ConPTY 在 shell 退出后不会关闭输出管道——reader 线程永远等不到
        // EOF，输出流不会结束。用独立线程阻塞 wait 子进程，退出即关闭输出 channel，
        // 让消费方流正常走到结束（上层据此关闭窗格）。Unix 上 reader 会先到 EOF，
        // 此处的 close_channel 是幂等的冗余保障，顺带回收僵尸进程。
        let mut child = session
            .take_child()
            .ok_or_else(|| SpawnError::Spawn("spawned session has no child".into()))?;
        let watcher = thread::Builder::new()
            .name("termior-pty-child-watcher".into())
            .spawn(move || {
                match child.wait() {
                    Ok(status) => log::info!("PTY child exited: {status:?}"),
                    Err(error) => log::warn!("PTY child wait failed: {error}"),
                }
                exit_signaler.close_channel();
            })
            .map_err(|e| SpawnError::Open(format!("child watcher thread: {e}")))?;

        Ok(Self {
            session,
            output_rx: Some(output_rx),
            _reader: handle,
            _watcher: watcher,
        })
    }

    /// 取输出 channel 的接收端（消费方在主线程 `next().await`）。
    /// 只能调用一次；重复调用返回 None。
    pub fn take_output(&mut self) -> Option<Receiver<PtyData>> {
        self.output_rx.take()
    }

    /// resize PTY + 网格（消费方同步 resize Term 后调用）。
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), SpawnError> {
        self.session.resize(rows, cols)
    }

    /// kill 子进程（关闭 tab）。
    pub fn kill(&mut self) -> Result<(), SpawnError> {
        self.session.kill()
    }

    /// 线程安全的 PTY 写句柄（键盘转发用）。
    pub fn writer(&self) -> WriterHandle {
        WriterHandle {
            inner: self.session.writer(),
        }
    }

    /// 本会话实际使用的 shell 类型（spawn 时解析）。
    pub fn shell_kind(&self) -> ShellKind {
        self.session.shell_kind()
    }

    /// 是否为 WSL 会话（影响 cd 注入等命令的路径形式）。
    pub fn is_wsl(&self) -> bool {
        self.session.is_wsl()
    }
}

impl Drop for TerminalBridge {
    fn drop(&mut self) {
        // Closing a terminal tab must terminate its shell. On Windows the session's Job Object
        // then tears down the complete child tree; on Unix this also releases the PTY master.
        let _ = self.session.kill();
    }
}

/// reader 线程主循环：循环 read，每批投递到 channel；PTY EOF 或 channel 关闭时退出。
fn run_reader(reader: &mut Box<dyn Read + Send>, mut tx: Sender<PtyData>) {
    let mut buf = [0u8; 8192];
    let mut osc_filter = OscStreamFilter::new();
    let mut url_detector = LocalhostDetector::default();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF：子进程关闭了输出
            Ok(n) => {
                let filtered = osc_filter.feed(&buf[..n]);
                let localhost_urls = url_detector
                    .feed(&filtered.visible)
                    .into_iter()
                    .map(|url| url.to_string())
                    .collect();
                // channel 关闭（消费方 drop）时退出循环。
                if futures::executor::block_on(tx.send(PtyData {
                    bytes: filtered.visible,
                    events: filtered.events,
                    localhost_urls,
                }))
                .is_err()
                {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(_e) => break, // 其它错误：标记会话死亡，reader 退出
        }
    }
    // Reader owns the final sender; closing it makes the UI stream terminate after queued data.
    tx.close_channel();
}

/// 线程安全的 PTY 写句柄：键盘转发用（可 clone，内部 Arc<Mutex>）。
#[derive(Clone)]
pub struct WriterHandle {
    inner: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
}

impl WriterHandle {
    /// 写入字节（如键盘编码后的 ANSI 序列、CPR 回复）并 flush。
    pub fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self.inner.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn pty_write_events_are_replied_to_immediately() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let writer = WriterHandle {
            inner: Arc::new(Mutex::new(Box::new(SharedWriter(bytes.clone())))),
        };
        let (proxy, mut events) = TerminalEventProxy::new(writer);

        proxy.send_event(TerminalEvent::PtyWrite("\u{1b}[1;1R".into()));

        assert_eq!(&*bytes.lock().unwrap(), b"\x1b[1;1R");
        assert!(matches!(
            events.try_recv(),
            Err(futures::channel::mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn ui_events_are_forwarded() {
        let writer = WriterHandle {
            inner: Arc::new(Mutex::new(Box::new(std::io::sink()))),
        };
        let (proxy, mut events) = TerminalEventProxy::new(writer);

        proxy.send_event(TerminalEvent::Bell);

        assert!(matches!(events.try_recv(), Ok(TerminalEvent::Bell)));
    }
}
