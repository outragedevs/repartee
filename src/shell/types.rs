/// Events emitted by shell PTY reader threads.
#[derive(Debug)]
pub enum ShellEvent {
    /// Raw bytes read from the PTY master fd.
    Output { id: String, bytes: Vec<u8> },
    /// The shell process has exited.
    Exited { id: String, status: Option<u32> },
}
