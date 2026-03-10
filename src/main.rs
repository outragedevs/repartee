mod app;
mod commands;
mod config;
mod constants;
mod image_preview;
mod irc;
mod scripting;
mod state;
mod storage;
mod theme;
mod ui;

use color_eyre::eyre::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Debug logging is opt-in: set RUST_LOG env var to enable.
    // e.g. RUST_LOG=info or RUST_LOG=repartee=debug
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

    ui::install_panic_hook();

    let mut app = app::App::new()?;
    let mut terminal = ui::setup_terminal()?;
    let result = app.run(&mut terminal).await;
    ui::restore_terminal(&mut terminal)?;
    result
}
