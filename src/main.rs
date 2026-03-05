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
    ui::install_panic_hook();

    let mut app = app::App::new()?;
    let mut terminal = ui::setup_terminal()?;
    let result = app.run(&mut terminal);
    ui::restore_terminal(&mut terminal)?;
    result
}
