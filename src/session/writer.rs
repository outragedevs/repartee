use std::io::{self, Write};
use tokio::sync::mpsc;

use super::protocol::MainMessage;

/// A `Write` implementation that buffers bytes and sends them as
/// `MainMessage::Output` chunks through an mpsc channel on `flush()`.
///
/// A spawned tokio task drains the receiver and writes framed messages
/// to the actual `UnixStream`.
pub struct SocketWriter {
    buffer: Vec<u8>,
    tx: mpsc::Sender<MainMessage>,
}

impl SocketWriter {
    pub fn new(tx: mpsc::Sender<MainMessage>) -> Self {
        Self {
            buffer: Vec::with_capacity(8192),
            tx,
        }
    }
}

impl Write for SocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let data = std::mem::replace(&mut self.buffer, Vec::with_capacity(8192));
            self.tx.try_send(MainMessage::Output(data)).map_err(|e| {
                io::Error::new(io::ErrorKind::BrokenPipe, format!("socket output channel error: {e}"))
            })?;
        }
        Ok(())
    }
}
