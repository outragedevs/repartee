use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use crossterm::event;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::interval;

use super::protocol::{self, MainMessage, ShimMessage};
use super::{list_sessions, socket_path};

/// Exit reason from the shim relay loop.
enum RelayExit {
    /// Main process shut down — shim should exit.
    Quit,
    /// Daemon confirmed detach — shim should restore terminal and exit.
    Detached,
    /// Write to the socket failed.
    WriteError(String),
    /// Input channel from the terminal reader closed.
    InputClosed,
    /// Socket read channel closed (daemon disconnected).
    ConnectionLost,
}

/// Messages from the output writer thread to the relay loop, in socket order:
/// each one is sent only after all preceding terminal output was written.
enum Control {
    /// Bytes written to the terminal, to acknowledge upstream.
    Written(u64),
    Detached,
    Quit,
}

/// Run the shim process that bridges a local terminal to a detached repartee instance.
///
/// If `show_splash` is true, runs the splash screen animation while waiting
/// for the daemon socket to become ready (initial launch from fork).
pub async fn run_shim(target_pid: Option<u32>, show_splash: bool) -> Result<()> {
    let (pid, sock_path) = resolve_session(target_pid);

    // If this is an initial launch, show splash while waiting for the daemon.
    if show_splash {
        run_splash(Some(&sock_path)).await?;
    }

    // Connect to the Unix socket.
    let stream = UnixStream::connect(&sock_path)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("Failed to connect to session PID {pid}: {e}"))?;

    run_connected_shim(pid, stream).await
}

pub async fn run_connected_shim(pid: u32, stream: UnixStream) -> Result<()> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let read_half = tokio::io::BufReader::new(read_half);

    // Capture terminal environment BEFORE enabling raw mode.
    let term_env = protocol::TerminalEnv::capture();

    // Enable raw mode on the shim's terminal.
    crossterm::terminal::enable_raw_mode()?;

    // Send initial terminal environment (includes dimensions + env vars).
    protocol::write_message(&mut write_half, &term_env).await?;

    // Input channel: blocking reader and dimension sampling → upstream messages.
    let (input_tx, mut input_rx) = mpsc::channel::<ShimMessage>(1024);
    let input_stop = Arc::new(AtomicBool::new(false));

    spawn_input_reader(input_tx, Arc::clone(&input_stop), (term_env.cols, term_env.rows));

    // Downstream channel: spawn a task that reads MainMessages from the socket
    // and forwards them through an mpsc channel. This avoids the cancellation-
    // safety bug where `select!` could cancel a `read_exact` mid-read on large
    // messages (e.g. Kitty image frames), desynchronizing the byte stream.
    // Terminal writes happen on a dedicated thread: a stalled terminal (e.g. a
    // congested SSH link) then backs up the socket instead of the input relay.
    let (downstream_tx, downstream_rx) = mpsc::channel::<MainMessage>(2);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<Control>();
    spawn_output_writer(
        std::io::stdout(),
        downstream_rx,
        control_tx,
        Arc::clone(&input_stop),
    );
    tokio::spawn(async move {
        let mut reader = read_half;
        loop {
            match protocol::read_message::<_, MainMessage>(&mut reader).await {
                Ok(msg) => {
                    if downstream_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("shim downstream read error: {e}");
                    break;
                }
            }
        }
    });

    // Main select loop: upstream (input → socket) and downstream (channel → stdout).
    tracing::info!("shim relay loop starting");
    let exit_reason = run_relay_loop(&mut input_rx, &mut write_half, &mut control_rx).await;
    tracing::info!("shim relay loop exited");

    // Cleanup.
    input_stop.store(true, Ordering::Relaxed);
    let _ = crossterm::terminal::disable_raw_mode();
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show
    );

    match exit_reason {
        RelayExit::Quit => eprintln!("Session ended."),
        RelayExit::Detached => eprintln!("Detached from repartee (PID {pid})."),
        RelayExit::WriteError(e) => {
            eprintln!("Disconnected from repartee (PID {pid}): write error: {e}");
        }
        RelayExit::InputClosed => eprintln!("Disconnected from repartee (PID {pid}): input closed"),
        RelayExit::ConnectionLost => {
            eprintln!("Disconnected from repartee (PID {pid}): connection lost");
        }
    }

    Ok(())
}

/// Resolve which session to connect to (given PID or auto-detect).
fn resolve_session(target_pid: Option<u32>) -> (u32, std::path::PathBuf) {
    if let Some(pid) = target_pid {
        let path = socket_path(pid);
        if !path.exists() {
            eprintln!("No session found for PID {pid}");
            std::process::exit(1);
        }
        return (pid, path);
    }

    let sessions = list_sessions();
    match sessions.len() {
        0 => {
            eprintln!("No detached sessions found.");
            std::process::exit(1);
        }
        1 => sessions.into_iter().next().unwrap(),
        _ => {
            eprintln!("Multiple sessions found. Specify a PID:");
            for (pid, _) in &sessions {
                eprintln!("  repartee a {pid}");
            }
            std::process::exit(1);
        }
    }
}

/// Run the splash screen animation in the shim's terminal.
///
/// Shows the progressive logo reveal while waiting for the daemon socket to
/// appear. If the socket is ready before the animation finishes, the splash
/// holds briefly then returns. Any keypress dismisses immediately.
///
/// `sock_path` is optional — when provided, the hold phase exits early once
/// the socket file appears (daemon is ready).
pub async fn run_splash(sock_path: Option<&std::path::Path>) -> Result<()> {
    use crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
    use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
    use ratatui::prelude::*;

    const LINE_DELAY_MS: u64 = 50;
    const HOLD_MS: u64 = 2500;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let total_lines = include_str!("../../logo.txt").lines().count();
    let mut visible = 0;
    let mut line_tick = interval(Duration::from_millis(LINE_DELAY_MS));
    let mut dismissed = false;

    // Phase 1: progressive reveal.
    while visible < total_lines && !dismissed {
        terminal.draw(|frame| crate::ui::splash::render(frame, visible))?;

        tokio::select! {
            _ = line_tick.tick() => {
                visible += 1;
            }
            ev = tokio::task::spawn_blocking(|| {
                if event::poll(std::time::Duration::from_millis(1)).unwrap_or(false) {
                    event::read().ok()
                } else {
                    None
                }
            }) => {
                if let Ok(Some(crossterm::event::Event::Key(_))) = ev {
                    dismissed = true;
                }
            }
        }
    }

    // Phase 2: hold fully revealed logo (wait for socket + timeout).
    if !dismissed {
        terminal.draw(|frame| crate::ui::splash::render(frame, total_lines))?;
        let hold_start = Instant::now();
        while hold_start.elapsed() < Duration::from_millis(HOLD_MS) && !dismissed {
            let remaining = Duration::from_millis(HOLD_MS).saturating_sub(hold_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            // Also check if socket is ready — if so, we can finish early.
            if sock_path.is_some_and(std::path::Path::exists)
                && hold_start.elapsed() >= Duration::from_millis(500)
            {
                break;
            }
            if let Ok(Some(crossterm::event::Event::Key(_))) =
                tokio::task::spawn_blocking(move || {
                    if event::poll(remaining.min(Duration::from_millis(100))).unwrap_or(false) {
                        event::read().ok()
                    } else {
                        None
                    }
                })
                .await
            {
                dismissed = true;
            }
        }
    }

    // Restore terminal — the relay loop will re-enter alt screen via the daemon's
    // setup_socket_terminal().
    let _ = crossterm::terminal::disable_raw_mode();
    let mut stdout = std::io::stdout();
    let _ = crossterm::execute!(
        stdout,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show
    );

    Ok(())
}

/// Spawn a blocking task that reads crossterm events and sends them upstream.
///
/// Intercepts `Ctrl+\` and `Ctrl+Z` at the shim level, converting them to
/// `ShimMessage::Detach` instead of forwarding the raw key event. This keeps
/// detach as a protocol-level concept — the daemon never sees the keystroke.
fn spawn_input_reader(tx: mpsc::Sender<ShimMessage>, stop: Arc<AtomicBool>, size: (u16, u16)) {
    std::thread::spawn(move || {
        let mut last_size = size;
        while !stop.load(Ordering::Relaxed) {
            let event = if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                match event::read() {
                    Ok(event) => Some(event),
                    Err(_) => break,
                }
            } else {
                None
            };
            if !forward_terminal_sample(&tx, &mut last_size, event, crossterm::terminal::size().ok()) {
                break;
            }
        }
    });
}

fn forward_terminal_sample(
    tx: &mpsc::Sender<ShimMessage>,
    last_size: &mut (u16, u16),
    event: Option<crossterm::event::Event>,
    size: Option<(u16, u16)>,
) -> bool {
    let mut size = size;
    let resize_event = matches!(&event, Some(crossterm::event::Event::Resize(..)));
    if let Some(event) = event {
        if is_detach_key(&event) {
            return tx.blocking_send(ShimMessage::Detach).is_ok();
        }
        if let crossterm::event::Event::Resize(cols, rows) = event {
            size = size.or(Some((cols, rows)));
        } else if tx.blocking_send(ShimMessage::TermEvent(event)).is_err() {
            return false;
        }
    }
    if let Some((cols, rows)) = size
        && cols > 0 && rows > 0 && (resize_event || *last_size != (cols, rows))
    {
        if tx.blocking_send(ShimMessage::Resize { cols, rows }).is_err() {
            return false;
        }
        *last_size = (cols, rows);
    }
    true
}

/// Check if a crossterm event is a detach chord (`Ctrl+\` or `Ctrl+Z`).
const fn is_detach_key(ev: &crossterm::event::Event) -> bool {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    matches!(
        ev,
        Event::Key(KeyEvent {
            code: KeyCode::Char('\\' | '4' | 'z'),
            modifiers,
            ..
        }) if modifiers.contains(KeyModifiers::CONTROL)
    )
}

/// Write terminal output on a dedicated thread and pass acknowledgements and
/// control messages on in socket order. Stops writing once `stop` is set so
/// nothing lands after the shim has restored the terminal.
fn spawn_output_writer<W>(
    mut out: W,
    mut downstream_rx: mpsc::Receiver<MainMessage>,
    control_tx: mpsc::UnboundedSender<Control>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()>
where
    W: Write + Send + 'static,
{
    std::thread::spawn(move || {
        let mut output_acks = false;
        while let Some(message) = downstream_rx.blocking_recv() {
            let control = match message {
                MainMessage::Output(bytes) => {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = out.write_all(&bytes);
                    let _ = out.flush();
                    if !output_acks {
                        continue;
                    }
                    Control::Written(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                }
                MainMessage::OutputAcks => {
                    output_acks = true;
                    continue;
                }
                MainMessage::Detached => Control::Detached,
                MainMessage::Quit => Control::Quit,
            };
            if control_tx.send(control).is_err() {
                break;
            }
        }
    })
}

/// Relay loop: forward input and output acknowledgements upstream and react to
/// daemon control messages.
///
/// Both sides are mpsc channels, which are cancellation-safe in `select!`.
/// The downstream reader task handles the actual socket read (with non-
/// cancellation-safe `read_exact`) in its own dedicated task, and terminal
/// output is written by `spawn_output_writer`, so this loop never waits on
/// the terminal.
async fn run_relay_loop<W>(
    input_rx: &mut mpsc::Receiver<ShimMessage>,
    write_half: &mut W,
    control_rx: &mut mpsc::UnboundedReceiver<Control>,
) -> RelayExit
where
    W: tokio::io::AsyncWriteExt + Unpin + Send,
{
    loop {
        tokio::select! {
            msg = input_rx.recv() => {
                if let Some(shim_msg) = msg {
                    if let Err(e) = protocol::write_message(write_half, &shim_msg).await {
                        return RelayExit::WriteError(e.to_string());
                    }
                } else {
                    return RelayExit::InputClosed;
                }
            }
            control = control_rx.recv() => {
                match control {
                    Some(Control::Written(bytes)) => {
                        let written = ShimMessage::OutputWritten(bytes);
                        if let Err(e) = protocol::write_message(write_half, &written).await {
                            return RelayExit::WriteError(e.to_string());
                        }
                    }
                    Some(Control::Detached) => return RelayExit::Detached,
                    Some(Control::Quit) => return RelayExit::Quit,
                    None => return RelayExit::ConnectionLost,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    struct GatedWriter {
        gate: std::sync::mpsc::Receiver<()>,
        written: Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Write for GatedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.gate.recv();
            self.written.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn gated_writer() -> (GatedWriter, std::sync::mpsc::Sender<()>, Arc<std::sync::Mutex<Vec<u8>>>) {
        let (open, gate) = std::sync::mpsc::channel();
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        (GatedWriter { gate, written: Arc::clone(&written) }, open, written)
    }

    #[tokio::test]
    async fn input_reaches_the_daemon_while_terminal_output_is_stalled() {
        let (writer, open, written) = gated_writer();
        let (downstream_tx, downstream_rx) = mpsc::channel(2);
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_output_writer(writer, downstream_rx, control_tx, stop);
        downstream_tx.send(MainMessage::OutputAcks).await.unwrap();
        downstream_tx.send(MainMessage::Output(b"frame".to_vec())).await.unwrap();

        let (input_tx, mut input_rx) = mpsc::channel(8);
        let (mut socket, mut daemon) = tokio::io::duplex(4096);
        let relay = tokio::spawn(async move {
            run_relay_loop(&mut input_rx, &mut socket, &mut control_rx).await
        });
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        input_tx.send(ShimMessage::TermEvent(key.clone())).await.unwrap();
        let forwarded = tokio::time::timeout(
            Duration::from_secs(1),
            protocol::read_message::<_, ShimMessage>(&mut daemon),
        )
        .await
        .expect("input must not wait for the stalled terminal")
        .unwrap();
        assert!(matches!(forwarded, ShimMessage::TermEvent(event) if event == key));
        assert!(written.lock().unwrap().is_empty());

        downstream_tx.send(MainMessage::Detached).await.unwrap();
        open.send(()).unwrap();
        let acknowledged = tokio::time::timeout(
            Duration::from_secs(1),
            protocol::read_message::<_, ShimMessage>(&mut daemon),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(acknowledged, ShimMessage::OutputWritten(5)));
        let exit = tokio::time::timeout(Duration::from_secs(1), relay).await.unwrap().unwrap();
        assert!(matches!(exit, RelayExit::Detached));
        assert_eq!(written.lock().unwrap().as_slice(), b"frame");
        drop(downstream_tx);
        handle.join().unwrap();
    }

    #[test]
    fn control_messages_wait_for_preceding_output_and_stop_suppresses_later_output() {
        let (writer, open, written) = gated_writer();
        let (downstream_tx, downstream_rx) = mpsc::channel(4);
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn_output_writer(writer, downstream_rx, control_tx, Arc::clone(&stop));
        downstream_tx.try_send(MainMessage::Output(b"before".to_vec())).unwrap();
        downstream_tx.try_send(MainMessage::Quit).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(control_rx.try_recv().is_err());
        open.send(()).unwrap();
        assert!(matches!(control_rx.blocking_recv(), Some(Control::Quit)));
        assert_eq!(written.lock().unwrap().as_slice(), b"before");
        stop.store(true, Ordering::Relaxed);
        downstream_tx.try_send(MainMessage::Output(b"after".to_vec())).unwrap();
        handle.join().unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"before");
        assert!(control_rx.blocking_recv().is_none());
    }

    #[test]
    fn dimensions_recover_without_a_resize_event_and_ignore_invalid_samples() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut last_size = (80, 24);
        for size in [Some((80, 24)), None, Some((0, 0))] {
            assert!(forward_terminal_sample(&sender, &mut last_size, None, size));
        }
        assert!(receiver.try_recv().is_err());
        assert!(forward_terminal_sample(&sender, &mut last_size, None, Some((120, 40))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 120, rows: 40 }));
        assert!(forward_terminal_sample(&sender, &mut last_size, None, Some((120, 40))));
        assert!(receiver.try_recv().is_err());
        assert!(forward_terminal_sample(&sender, &mut last_size, None, Some((80, 24))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 80, rows: 24 }));
    }

    #[test]
    fn late_kernel_dimensions_replace_stale_signal_dimensions() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut last_size = (80, 24);
        assert!(forward_terminal_sample(&sender, &mut last_size, Some(Event::Resize(80, 24)), Some((80, 24))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 80, rows: 24 }));
        assert!(forward_terminal_sample(&sender, &mut last_size, None, Some((132, 50))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 132, rows: 50 }));
        assert!(forward_terminal_sample(&sender, &mut last_size, Some(Event::Resize(80, 24)), Some((132, 50))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 132, rows: 50 }));
        assert_eq!(last_size, (132, 50));
    }

    #[test]
    fn real_resize_events_preserve_same_grid_metric_refreshes() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut last_size = (80, 24);
        for _ in 0..2 {
            assert!(forward_terminal_sample(&sender, &mut last_size, Some(Event::Resize(80, 24)), Some((80, 24))));
            assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 80, rows: 24 }));
        }
        assert!(forward_terminal_sample(&sender, &mut last_size, None, Some((80, 24))));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn input_resize_and_detach_share_one_ordered_source() {
        let (sender, mut receiver) = mpsc::channel(8);
        let mut last_size = (80, 24);
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(forward_terminal_sample(&sender, &mut last_size, Some(key.clone()), Some((100, 30))));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::TermEvent(event) if event == key));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 100, rows: 30 }));
        assert!(forward_terminal_sample(&sender, &mut last_size, Some(Event::Resize(90, 25)), None));
        assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Resize { cols: 90, rows: 25 }));
        for code in ['z', '\\', '4'] {
            let detach = Event::Key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL));
            assert!(forward_terminal_sample(&sender, &mut last_size, Some(detach), Some((160, 50))));
            assert!(matches!(receiver.try_recv().unwrap(), ShimMessage::Detach));
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(last_size, (90, 25));
    }
}
