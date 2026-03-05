mod app;
mod commands;
mod config;
mod constants;
mod image_preview;
mod irc;
mod scripting;
mod state;
mod theme;
mod ui;

use color_eyre::eyre::Result;

fn main() -> Result<()> {
    color_eyre::install()?;
    println!("{} v{}", constants::APP_NAME, constants::APP_VERSION);
    Ok(())
}
