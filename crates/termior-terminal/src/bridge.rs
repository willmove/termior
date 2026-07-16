//! PTY ↔ 消费方桥接（FR-TERM-01/03）。
//!
//! [`TerminalBridge`] 持有 [`PtySession`]，在独立线程里循环读取 PTY 输出，
//! 通过 futures channel 把原始字节投递给消费方（GPUI 主线程）。
//! reader 线程只做 IO + 字节搬运，不触碰 alacritty `Term`（`Term` 非 `Send`）。

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::pty::{PtySession, PtySessionConfig, SpawnError};

/// 从 PTY reader 线程流向消费方的一批数据。
#[derive(Debug)]
pub struct PtyData {
    /// PTY 原始输出字节（含 ANSI/OSC 序列），消费方喂给 vte::Parser 与 OscParser。
    pub bytes: Vec<u8>,
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
    output_rx: Option<UnboundedReceiver<PtyData>>,
    /// 持有 reader 线程句柄，drop 时自然分离（reader 线程在 PTY EOF 后退出）。
    _reader: thread::JoinHandle<()>,
}

impl TerminalBridge {
    /// spawn PTY 会话并启动 reader 线程；返回的 bridge 持有输出 channel 接收端。
    pub fn spawn(config: &PtySessionConfig) -> Result<Self, SpawnError> {
        let session = PtySession::spawn(config)?;
        let mut reader = session.take_reader()?;
        let (output_tx, output_rx) = mpsc::unbounded::<PtyData>();

        let handle = thread::Builder::new()
            .name("termior-pty-reader".into())
            .spawn(move || {
                run_reader(&mut reader, output_tx);
            })
            .map_err(|e| SpawnError::Open(format!("reader thread: {e}")))?;

        Ok(Self {
            session,
            output_rx: Some(output_rx),
            _reader: handle,
        })
    }

    /// 取输出 channel 的接收端（消费方在主线程 `next().await`）。
    /// 只能调用一次；重复调用返回 None。
    pub fn take_output(&mut self) -> Option<UnboundedReceiver<PtyData>> {
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
}

/// reader 线程主循环：循环 read，每批投递到 channel；PTY EOF 或 channel 关闭时退出。
fn run_reader(reader: &mut Box<dyn Read + Send>, tx: UnboundedSender<PtyData>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF：子进程关闭了输出
            Ok(n) => {
                let bytes = buf[..n].to_vec();
                // channel 关闭（消费方 drop）时退出循环。
                if tx.unbounded_send(PtyData { bytes }).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                continue;
            }
            Err(_e) => break, // 其它错误：标记会话死亡，reader 退出
        }
    }
    // flush() 返回 Future（异步），同步上下文无法 await；直接关闭 channel，
    // UnboundedSender drop 时未投递数据随 channel 一并清理。
    tx.close_channel();
}

/// 线程安全的 PTY 写句柄：键盘转发用（可 clone，内部 Arc<Mutex>）。
#[derive(Clone)]
pub struct WriterHandle {
    inner: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
}

impl WriterHandle {
    /// 写入字节（如键盘编码后的 ANSI 序列）。
    pub fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.inner.lock().unwrap().write_all(bytes)
    }
}
