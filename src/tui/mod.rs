//! Terminal UI renderer using ratatui.

pub mod log_layer;
pub mod state;
mod ui;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use state::AppState;
use std::{
    io::stdout,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// Restores the terminal to its original state when dropped.
pub struct TuiHandle;

impl Drop for TuiHandle {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

/// Enters alternate-screen raw mode and installs a panic hook that restores the terminal.
pub fn enter() -> Result<TuiHandle> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    Ok(TuiHandle)
}

/// Render loop, redrawing at ~10 fps.
pub async fn run(state: Arc<Mutex<AppState>>, token: CancellationToken) -> Result<()> {
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    // TODO review locking concerns
    loop {
        // Snapshot state under the lock, then render without holding it.
        let snapshot = state.lock().unwrap().clone();
        terminal.draw(|f| ui::render(f, &snapshot))?;

        if token.is_cancelled() {
            break;
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // Poll for keyboard events.
                while event::poll(Duration::ZERO)? {
                    if let Event::Key(key) = event::read()? {
                        let ctrl_c = key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL);
                        if ctrl_c {
                            token.cancel();
                            return Ok(());
                        }
                    }
                }
            }
            _ = token.cancelled() => break,
        }
    }

    Ok(())
}
