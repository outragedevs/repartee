mod app;
mod commands;
mod config;
mod constants;
mod dcc;
mod e2e;
mod fs_secure;
mod image_preview;
mod irc;
mod nick_color;
mod scripting;
mod session;
mod shell;
mod spellcheck;
mod state;
mod storage;
mod theme;
mod ui;
mod web;

// Swap glibc ptmalloc2 for jemalloc on Linux. glibc fragments its arena under
// bursty allocation patterns in long-running processes — we observed 3 GB RSS
// growth on Debian before the v0.8.4 chat_view render-budget fix, and even
// post-fix the baseline working set drifts upward over weeks of uptime.
// jemalloc returns memory to the OS more aggressively and is already the
// default system allocator on FreeBSD, so this brings Linux in line with BSD.
// macOS keeps libsystem_malloc — no #[cfg] coverage here means the dep is not
// even pulled into the build graph on non-Linux targets. See
// docs/superpowers/specs/2026-04-10-v084-oom-fix-design.md for rationale.
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use color_eyre::eyre::Result;
use tracing_subscriber::EnvFilter;

fn log_path() -> std::path::PathBuf {
    constants::home_dir().join(format!("{}.log", constants::APP_NAME))
}

fn setup_logging() {
    let log_dir = constants::home_dir();
    if std::fs::create_dir_all(&log_dir).is_err() {
        // Without a writable home dir there's nowhere to log; subscribers
        // never get installed, but startup must still continue.
        return;
    }
    let Ok(log_file) = std::fs::File::options()
        .create(true)
        .append(true)
        .open(log_path())
    else {
        return;
    };
    // Default to WARN so the log file always carries enough breadcrumbs to
    // diagnose silent post-fork crashes ("No session found for PID X")
    // without forcing the user to remember `RUST_LOG=info` first. Users can
    // still raise/lower the level via `RUST_LOG`.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(log_file)
        .with_ansi(false)
        .init();
}

/// Reap a child process if it has already exited (non-blocking).
///
/// Returns `Some(human_status)` when the child is gone — covers both
/// normal exit and signal termination — so the parent's pre-attach wait
/// loop can fail fast with the actual reason instead of timing out 5s
/// later with "No session found for PID X". `is_pid_alive` (kill(0))
/// cannot do this on its own: a child that exited but hasn't been
/// `wait`ed for is a zombie and `kill(0)` reports it as alive.
fn try_reap(child_pid: u32) -> Option<String> {
    let Ok(pid) = libc::pid_t::try_from(child_pid) else {
        return Some("invalid PID".into());
    };
    let mut status: libc::c_int = 0;
    // SAFETY: WNOHANG makes waitpid non-blocking; passing a valid pointer
    // to an i32 is sound. Returns 0 if child still running, pid if reaped,
    // -1 on ECHILD/EINTR (treat as "still around" — be conservative).
    let result = unsafe {
        libc::waitpid(
            pid,
            std::ptr::from_mut::<libc::c_int>(&mut status),
            libc::WNOHANG,
        )
    };
    if result != pid {
        return None;
    }
    if libc::WIFEXITED(status) {
        Some(format!("exit code {}", libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Some(format!("killed by signal {}", libc::WTERMSIG(status)))
    } else {
        Some("unknown termination".into())
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Handle --version / -v before any setup (no tokio needed).
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("{} {}", constants::APP_NAME, constants::APP_VERSION);
        return Ok(());
    }

    // Handle attach subcommand: `repartee a [pid]` or `repartee attach [pid]`
    // Runs purely as a shim — no fork needed.
    if args.get(1).map(String::as_str) == Some("a")
        || args.get(1).map(String::as_str) == Some("attach")
    {
        color_eyre::install()?;
        setup_logging();
        let target_pid = args.get(2).and_then(|s| s.parse::<u32>().ok());
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(session::shim::run_shim(target_pid, false));
    }

    // Handle log browser subcommand: `repartee l` or `repartee logs`.
    // Direct mode like `attach` — no fork, no IRC, no socket listener.
    // Pre-fork validation isn't needed here (we never fork) but config
    // parse errors still surface inside `App::new` and reach the user's
    // TTY directly.
    if args.get(1).map(String::as_str) == Some("l")
        || args.get(1).map(String::as_str) == Some("logs")
    {
        color_eyre::install()?;
        setup_logging();
        ui::install_panic_hook();
        constants::ensure_config_dir();
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut app = app::App::new_log_browser()?;
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    app.cached_term_cols = cols;
                    app.cached_term_rows = rows;
                }
                app.terminal = Some(ui::setup_terminal()?);
                let result = app.run().await;
                if let Some(ref mut terminal) = app.terminal {
                    let _ = ui::restore_terminal(terminal);
                }
                result
            });
    }

    // Handle -d / --detach: start headless (no fork, no terminal).
    if args.iter().any(|a| a == "--detach" || a == "-d") {
        color_eyre::install()?;
        setup_logging();
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let mut app = app::App::new()?;
                app.detached = true;
                let pid = std::process::id();
                let sock_path = session::socket_path(pid);
                eprintln!("Starting detached. PID={pid}");
                eprintln!("Socket: {}", sock_path.display());
                eprintln!("Attach with: {} a", constants::APP_NAME);
                let result = app.run().await;
                app::App::remove_own_socket();
                result
            });
    }

    // --- Normal start: fork before tokio. ---
    // Child becomes the headless backend (IRC, state, socket listener).
    // Parent becomes the shim (bridges terminal ↔ socket).
    // On detach, the parent/shim exits → shell gets prompt back.
    //
    // Validate config + theme on the parent's TTY *before* forking. The
    // child runs with stderr redirected to /dev/null, so any `App::new`
    // failure (e.g. a TOML typo like `autoconnect = fals`) would otherwise
    // disappear into the void and surface as a generic "No session found
    // for PID X" 5 seconds later. Failing fast here puts the actual
    // toml-error line/column on the user's screen.
    constants::ensure_config_dir();
    if let Err(e) = config::validate_startup_files(
        &constants::config_path(),
        &constants::theme_dir(),
    ) {
        // `{e:#}` formats the eyre chain without color_eyre's source-location
        // footer — the toml parser already prints line/column inside the
        // message, anything more would just clutter the user's terminal.
        eprintln!("{}: {e:#}", constants::APP_NAME);
        std::process::exit(1);
    }

    // Fork BEFORE any tokio runtime or threads exist.
    let fork_result = unsafe { libc::fork() };

    match fork_result {
        -1 => {
            // Fork failed — fall back to direct mode (no detach support).
            color_eyre::install()?;
            setup_logging();
            ui::install_panic_hook();
            let mut app = app::App::new()?;
            if let Ok((cols, rows)) = crossterm::terminal::size() {
                app.cached_term_cols = cols;
                app.cached_term_rows = rows;
            }
            app.terminal = Some(ui::setup_terminal()?);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let result = rt.block_on(app.run());
            if let Some(ref mut terminal) = app.terminal {
                let _ = ui::restore_terminal(terminal);
            }
            result
        }
        0 => {
            // Child: headless backend process.
            unsafe {
                libc::setsid();
                let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if devnull >= 0 {
                    libc::dup2(devnull, libc::STDIN_FILENO);
                    libc::dup2(devnull, libc::STDOUT_FILENO);
                    libc::dup2(devnull, libc::STDERR_FILENO);
                    libc::close(devnull);
                }
            }
            color_eyre::install()?;
            setup_logging();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let mut app = app::App::new()?;
                    app.detached = true;
                    let result = app.run().await;
                    app::App::remove_own_socket();
                    result
                })
        }
        child_pid => {
            // Parent: terminal shim connecting to the child's socket.
            // The splash screen runs while the daemon starts up in the background.
            let child_pid = u32::try_from(child_pid)
                .map_err(|_| color_eyre::eyre::eyre!("fork returned invalid PID: {child_pid}"))?;
            color_eyre::install()?;
            setup_logging();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let sock_path = session::socket_path(child_pid);

                    // Show splash animation — the daemon socket typically
                    // appears during this time (splash takes ~1.5-2.5s).
                    session::shim::run_splash(Some(&sock_path)).await?;

                    // Wait for the socket OR for the child to die. `waitpid`
                    // (non-blocking) reaps a dead child so we can report its
                    // actual exit status instead of staring 5 s at a zombie
                    // PID that `kill(0)` insists is alive. The log file we
                    // unconditionally maintain in `setup_logging` carries
                    // any backtrace the user needs.
                    let mut socket_ready = false;
                    for _ in 0..100 {
                        if sock_path.exists() {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            socket_ready = true;
                            break;
                        }
                        if let Some(reason) = try_reap(child_pid) {
                            let log_path = log_path();
                            return Err(color_eyre::eyre::eyre!(
                                "Backend exited during startup ({reason}). \
                                 See {} for details.",
                                log_path.display()
                            ));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    if !socket_ready {
                        let log_path = log_path();
                        return Err(color_eyre::eyre::eyre!(
                            "Backend (PID {child_pid}) is alive but never opened its session \
                             socket within 5 s — likely failed to bind {}. \
                             See {} for details.",
                            sock_path.display(),
                            log_path.display()
                        ));
                    }
                    session::shim::run_shim(Some(child_pid), false).await
                })
        }
    }
}
