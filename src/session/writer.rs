use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

#[derive(Default)]
struct DrainTiming {
    queued_at: Option<Instant>,
    last: Option<Duration>,
    terminal_acks: bool,
    sent: u64,
    acked: u64,
    /// End offset (in `sent` bytes) and queue time of each unacknowledged flush.
    in_flight: VecDeque<(u64, Instant)>,
}

/// Measures how long queued output takes to reach the terminal, so callers can
/// pace optional redraws to what the link actually delivers. With shim
/// acknowledgements every flush is timed until the shim has written its last
/// byte, so the measurement keeps updating under a sustained backlog; without
/// them it falls back to the local socket accepting the output.
#[derive(Clone, Default)]
pub struct DrainClock(Arc<Mutex<DrainTiming>>);

impl DrainClock {
    pub fn enable_terminal_acks(&self) {
        self.0.lock().unwrap().terminal_acks = true;
    }

    fn start(&self, bytes: usize) {
        let mut timing = self.0.lock().unwrap();
        let now = Instant::now();
        if timing.terminal_acks {
            timing.sent = timing
                .sent
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
            let end = timing.sent;
            timing.in_flight.push_back((end, now));
        } else {
            timing.queued_at.get_or_insert(now);
        }
    }

    pub fn socket_drained(&self) {
        let mut timing = self.0.lock().unwrap();
        if !timing.terminal_acks
            && let Some(queued_at) = timing.queued_at.take()
        {
            timing.last = Some(queued_at.elapsed());
        }
    }

    pub fn terminal_written(&self, bytes: u64) {
        let mut timing = self.0.lock().unwrap();
        timing.acked = timing.acked.saturating_add(bytes).min(timing.sent);
        while let Some(&(end, queued_at)) = timing.in_flight.front() {
            if end > timing.acked {
                break;
            }
            timing.in_flight.pop_front();
            timing.last = Some(queued_at.elapsed());
        }
        drop(timing);
    }

    pub fn last_drain(&self) -> Option<Duration> {
        self.0.lock().unwrap().last
    }

    /// Output is still on its way to the terminal.
    pub fn backlogged(&self) -> bool {
        let timing = self.0.lock().unwrap();
        if timing.terminal_acks {
            timing.acked < timing.sent
        } else {
            timing.queued_at.is_some()
        }
    }
}

#[derive(Clone)]
pub struct SocketOutput {
    buffer: Arc<Mutex<OutputBuffer>>,
    tx: mpsc::UnboundedSender<MainMessage>,
    queued_bytes: Arc<AtomicUsize>,
    max_queued_bytes: usize,
    pub drained: Arc<Notify>,
    pub drain_clock: DrainClock,
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
        // The loop `fetch_update` runs, written out: Rust 1.99 deprecates it in
        // favour of `try_update`, which is not yet stable on the 1.91 floor.
        let mut current = self.queued_bytes.load(Ordering::Acquire);
        loop {
            let next = current
                .checked_add(len)
                .filter(|next| *next <= self.max_queued_bytes)
                .ok_or_else(|| queue_full_error(current, len, self.max_queued_bytes))?;
            match self.queued_bytes.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.drain_clock.start(len);
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
                drain_clock: DrainClock::default(),
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
    fn drain_clock_measures_from_first_queued_output_until_drained() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, queued, 1024);
        let clock = writer.output().drain_clock;
        assert_eq!(clock.last_drain(), None);
        clock.socket_drained();
        assert_eq!(clock.last_drain(), None);
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        writer.write_all(b"second").unwrap();
        writer.flush().unwrap();
        clock.socket_drained();
        let first = clock.last_drain().unwrap();
        assert!(first >= Duration::from_millis(20));
        writer.write_all(b"third").unwrap();
        writer.flush().unwrap();
        clock.socket_drained();
        assert!(clock.last_drain().unwrap() < first);
    }

    #[test]
    fn terminal_acks_replace_the_socket_drain_measurement() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, queued, 1024);
        let clock = writer.output().drain_clock;
        clock.enable_terminal_acks();
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();
        writer.write_all(b"second").unwrap();
        writer.flush().unwrap();
        clock.socket_drained();
        assert_eq!(clock.last_drain(), None);
        assert!(clock.backlogged());
        std::thread::sleep(Duration::from_millis(20));
        clock.terminal_written(3);
        assert_eq!(clock.last_drain(), None);
        clock.terminal_written(2);
        assert!(clock.last_drain().unwrap() >= Duration::from_millis(20));
        assert!(clock.backlogged());
        clock.terminal_written(6);
        assert!(!clock.backlogged());
    }

    #[test]
    fn sustained_terminal_backlog_keeps_updating_the_measurement() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let mut writer = SocketWriter::new(tx, queued, 1024);
        let clock = writer.output().drain_clock;
        clock.enable_terminal_acks();
        writer.write_all(b"frame").unwrap();
        writer.flush().unwrap();
        let mut previous = Duration::ZERO;
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(10));
            writer.write_all(b"frame").unwrap();
            writer.flush().unwrap();
            clock.terminal_written(5);
            let drain = clock.last_drain().unwrap();
            assert!(drain >= Duration::from_millis(10));
            assert_ne!(drain, previous);
            previous = drain;
            assert!(clock.backlogged());
        }
    }

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
