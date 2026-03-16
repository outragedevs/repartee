use std::sync::Arc;

/// Events emitted by shell PTY reader threads.
#[derive(Debug)]
pub enum ShellEvent {
    /// Raw bytes read from the PTY master fd.
    Output { id: Arc<str>, bytes: Vec<u8> },
    /// The shell process has exited.
    Exited { id: Arc<str>, status: Option<u32> },
}
