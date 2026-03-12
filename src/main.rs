mod app;
mod commands;
mod config;
mod constants;
mod image_preview;
mod irc;
mod scripting;
mod session;
mod state;
mod storage;
mod theme;
mod ui;

use color_eyre::eyre::Result;
use tracing_subscriber::EnvFilter;

fn setup_logging() -> Result<()> {
    if std::env::var("RUST_LOG").is_ok() {
        let log_dir = constants::home_dir();
        std::fs::create_dir_all(&log_dir)?;
        let log_file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(log_dir.join("repartee.log"))?;
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_writer(log_file)
            .with_ansi(false)
            .init();
    }
    Ok(())
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
        setup_logging()?;
        let target_pid = args.get(2).and_then(|s| s.parse::<u32>().ok());
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(session::shim::run_shim(target_pid, false));
    }

    // Handle -d / --detach: start headless (no fork, no terminal).
    if args.iter().any(|a| a == "--detach" || a == "-d") {
        color_eyre::install()?;
        setup_logging()?;
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
                eprintln!("Attach with: repartee a");
                let result = app.run().await;
                app::App::remove_own_socket();
                result
            });
    }

    // --- Normal start: fork before tokio. ---
    // Child becomes the headless backend (IRC, state, socket listener).
    // Parent becomes the shim (bridges terminal ↔ socket).
    // On detach, the parent/shim exits → shell gets prompt back.

    // Fork BEFORE any tokio runtime or threads exist.
    let fork_result = unsafe { libc::fork() };

    match fork_result {
        -1 => {
            // Fork failed — fall back to direct mode (no detach support).
            color_eyre::install()?;
            setup_logging()?;
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
            setup_logging()?;
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
            setup_logging()?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let sock_path = session::socket_path(child_pid);

                    // Show splash animation — the daemon socket typically
                    // appears during this time (splash takes ~1.5-2.5s).
                    session::shim::run_splash(Some(&sock_path)).await?;

                    // Ensure the socket is ready after the splash.
                    for _ in 0..100 {
                        if sock_path.exists() {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            break;
                        }
                        if !session::is_pid_alive(child_pid) {
                            return Err(color_eyre::eyre::eyre!(
                                "Backend process exited unexpectedly. Check ~/.repartee/repartee.log"
                            ));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                    session::shim::run_shim(Some(child_pid), false).await
                })
        }
    }
}
