pub mod buffer_list;
pub mod chat_view;
pub mod input;
pub mod layout;
pub mod message_line;
pub mod nick_list;
pub mod status_line;
pub mod styled_text;
pub mod topic_bar;

use std::io;
use color_eyre::eyre::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Set up the terminal for TUI mode.
pub fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to normal mode.
pub fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Truncate a string to fit `max_len` visible chars, appending `+` if truncated.
pub fn truncate_with_plus(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if s.chars().count() <= max_len {
        return s.to_string();
    }
    if max_len <= 1 {
        return "+".to_string();
    }
    // Use char_indices for correct UTF-8 truncation
    let end = s
        .char_indices()
        .nth(max_len - 1)
        .map_or(s.len(), |(i, _)| i);
    format!("{}+", &s[..end])
}

/// Count visible text length from parsed format spans (ignoring color codes).
pub fn visible_len(spans: &[crate::theme::StyledSpan]) -> usize {
    spans.iter().map(|s| s.text.chars().count()).sum()
}

/// Install a panic hook that restores the terminal before printing the panic.
pub fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(panic_info);
    }));
}
