use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

use super::protocol::MainMessage;

pub const MAX_SOCKET_OUTPUT_QUEUE_BYTES: usize = 16 * 1024 * 1024;

/// A `Write` implementation that buffers bytes and sends them as
/// `MainMessage::Output` chunks through an mpsc channel on `flush()`.
///
/// A spawned tokio task drains the receiver and writes framed messages
/// to the actual `UnixStream`.
pub struct SocketWriter {
    buffer: Vec<u8>,
    tx: mpsc::UnboundedSender<MainMessage>,
    queued_bytes: Arc<AtomicUsize>,
    max_queued_bytes: usize,
}

impl SocketWriter {
    pub fn new(
        tx: mpsc::UnboundedSender<MainMessage>,
        queued_bytes: Arc<AtomicUsize>,
        max_queued_bytes: usize,
    ) -> Self {
        Self {
            buffer: Vec::with_capacity(8192),
            tx,
            queued_bytes,
            max_queued_bytes,
        }
    }

    fn reserve_queued_bytes(&self, len: usize) -> io::Result<()> {
        let mut current = self.queued_bytes.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(len) else {
                return Err(queue_full_error(current, len, self.max_queued_bytes));
            };
            if next > self.max_queued_bytes {
                return Err(queue_full_error(current, len, self.max_queued_bytes));
            }
            match self.queued_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }
}

impl Write for SocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let pending = self
            .queued_bytes
            .load(Ordering::Relaxed)
            .saturating_add(self.buffer.len())
            .saturating_add(buf.len());
        if pending > self.max_queued_bytes {
            return Err(queue_full_error(
                self.queued_bytes.load(Ordering::Relaxed),
                self.buffer.len().saturating_add(buf.len()),
                self.max_queued_bytes,
            ));
        }
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let data = std::mem::replace(&mut self.buffer, Vec::with_capacity(8192));
            let data_len = data.len();
            self.reserve_queued_bytes(data_len)?;
            self.tx.send(MainMessage::Output(data)).map_err(|e| {
                self.queued_bytes.fetch_sub(data_len, Ordering::AcqRel);
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("socket output channel error: {e}"),
                )
            })?;
        }
        Ok(())
    }
}

fn queue_full_error(current: usize, additional: usize, max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "socket output queue full: queued={current} additional={additional} max={max} bytes"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_tracks_queued_output_bytes() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, Arc::clone(&queued), 1024);

        writer.write_all(b"hello").unwrap();
        writer.flush().unwrap();

        assert_eq!(queued.load(Ordering::Relaxed), 5);
        assert!(matches!(rx.try_recv().unwrap(), MainMessage::Output(bytes) if bytes == b"hello"));
    }

    #[test]
    fn write_rejects_output_above_queue_limit() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(8));
        let mut writer = SocketWriter::new(tx, queued, 10);

        let err = writer.write_all(b"abc").unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }
}
