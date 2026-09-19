use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc};

use super::protocol::MainMessage;

pub const MAX_SOCKET_OUTPUT_QUEUE_BYTES: usize = 64 * 1024 * 1024;
pub const SOCKET_OUTPUT_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct OutputBuffer {
    bytes: Vec<u8>,
    frame: bool,
    failed: Option<io::ErrorKind>,
}

#[derive(Clone)]
pub struct SocketOutput {
    buffer: Arc<Mutex<OutputBuffer>>,
    tx: mpsc::UnboundedSender<MainMessage>,
    queued_bytes: Arc<AtomicUsize>,
    max_queued_bytes: usize,
    pub drained: Arc<Notify>,
    graphics_disabled: Arc<AtomicBool>,
}

impl SocketOutput {
    pub fn graphics_enabled(&self) -> bool {
        !self.graphics_disabled.load(Ordering::Relaxed)
    }

    pub fn disable_graphics(&self) {
        self.graphics_disabled.store(true, Ordering::Relaxed);
    }

    pub fn begin_frame(&self) -> bool {
        if !self.tx.is_closed() && self.queued_bytes.load(Ordering::Acquire) != 0 {
            return false;
        }
        self.buffer.lock().unwrap().frame = true;
        true
    }

    pub fn finish_frame(&self) -> io::Result<()> {
        let mut buffer = self.buffer.lock().unwrap();
        buffer.frame = false;
        let result = self.flush_buffer(&mut buffer);
        drop(buffer);
        result
    }

    pub fn cancel_frame(&self) {
        *self.buffer.lock().unwrap() = OutputBuffer::default();
    }

    fn flush_buffer(&self, buffer: &mut OutputBuffer) -> io::Result<()> {
        if let Some(kind) = buffer.failed {
            return Err(io::Error::new(kind, "terminal frame could not be buffered"));
        }
        if buffer.frame || buffer.bytes.is_empty() {
            return Ok(());
        }
        let len = buffer.bytes.len();
        self.queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(len)
                    .filter(|next| *next <= self.max_queued_bytes)
            })
            .map_err(|current| queue_full_error(current, len, self.max_queued_bytes))?;
        let data = std::mem::take(&mut buffer.bytes);
        self.tx.send(MainMessage::Output(data)).map_err(|_| {
            self.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            io::Error::new(io::ErrorKind::BrokenPipe, "socket output channel closed")
        })
    }
}

pub struct SocketWriter {
    output: SocketOutput,
}

impl SocketWriter {
    pub fn new(
        tx: mpsc::UnboundedSender<MainMessage>,
        queued_bytes: Arc<AtomicUsize>,
        max_queued_bytes: usize,
    ) -> Self {
        Self {
            output: SocketOutput {
                buffer: Arc::new(Mutex::new(OutputBuffer::default())),
                tx,
                queued_bytes,
                max_queued_bytes,
                drained: Arc::new(Notify::new()),
                graphics_disabled: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    pub fn output(&self) -> SocketOutput {
        self.output.clone()
    }
}

impl Write for SocketWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut buffer = self.output.buffer.lock().unwrap();
        if self.output.tx.is_closed() {
            buffer.failed = Some(io::ErrorKind::BrokenPipe);
        }
        if let Some(kind) = buffer.failed {
            return Err(io::Error::new(kind, "terminal frame could not be buffered"));
        }
        let current = self.output.queued_bytes.load(Ordering::Acquire);
        let additional = buffer.bytes.len().saturating_add(bytes.len());
        if additional > self.output.max_queued_bytes {
            buffer.failed = Some(io::ErrorKind::WouldBlock);
            return Err(queue_full_error(
                current,
                additional,
                self.output.max_queued_bytes,
            ));
        }
        buffer.bytes.extend_from_slice(bytes);
        drop(buffer);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output
            .flush_buffer(&mut self.output.buffer.lock().unwrap())
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
    fn control_output_can_wait_behind_a_full_frame() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, Arc::clone(&queued), 10);
        writer.write_all(b"0123456789").unwrap();
        writer.flush().unwrap();
        writer.write_all(b"resize").unwrap();
        assert_eq!(writer.flush().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(!writer.output().begin_frame());
        rx.try_recv().unwrap();
        queued.store(0, Ordering::Release);
        assert!(writer.output().begin_frame());
        writer.write_all(b"next").unwrap();
        writer.output().finish_frame().unwrap();
        assert!(matches!(rx.try_recv().unwrap(), MainMessage::Output(bytes) if bytes == b"resizenext"));
    }

    #[test]
    fn closed_receiver_is_not_mistaken_for_backpressure() {
        let (tx, rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(8));
        let mut writer = SocketWriter::new(tx, queued, 10);
        let output = writer.output();
        drop(rx);
        assert!(output.begin_frame());
        assert_eq!(
            writer.write_all(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn oversized_frame_is_discarded_without_partial_output() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, Arc::clone(&queued), 10);
        let output = writer.output();
        assert!(output.begin_frame());
        writer.write_all(b"prefix").unwrap();
        writer.flush().unwrap();
        assert_eq!(
            writer.write_all(b"oversized").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(output.finish_frame().is_err());
        assert!(rx.try_recv().is_err());
        assert_eq!(queued.load(Ordering::Relaxed), 0);
        output.cancel_frame();
        assert!(output.begin_frame());
        writer.write_all(b"recovered").unwrap();
        output.finish_frame().unwrap();
        assert!(
            matches!(rx.try_recv().unwrap(), MainMessage::Output(bytes) if bytes == b"recovered")
        );
    }

    #[test]
    fn slow_receiver_defers_the_next_frame_until_output_drains() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, Arc::clone(&queued), 10);
        let output = writer.output();
        assert!(output.begin_frame());
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();
        assert!(rx.try_recv().is_err());
        output.finish_frame().unwrap();
        for _ in 0..10 {
            assert!(!output.begin_frame());
        }
        let MainMessage::Output(bytes) = rx.try_recv().unwrap() else {
            panic!("missing output")
        };
        assert_eq!(bytes, b"first");
        assert!(!output.begin_frame());
        queued.fetch_sub(bytes.len(), Ordering::AcqRel);
        assert!(output.begin_frame());
        writer.write_all(b"second").unwrap();
        output.finish_frame().unwrap();
        assert!(matches!(rx.try_recv().unwrap(), MainMessage::Output(bytes) if bytes == b"second"));
    }
}
