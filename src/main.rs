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

    // --- Normal start: single process with real terminal ---
    color_eyre::install()?;
    setup_logging()?;
    ui::install_panic_hook();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut app = app::App::new()?;
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            app.cached_term_cols = cols;
            app.cached_term_rows = rows;
        }
        app.terminal = Some(ui::setup_terminal()?);
        let result = app.run().await;
        // Only restore if we still own a terminal (None after detach+quit).
        if let Some(ref mut terminal) = app.terminal
            && !app.is_socket_attached
        {
            let _ = ui::restore_terminal(terminal);
        }
        result
    })
}
