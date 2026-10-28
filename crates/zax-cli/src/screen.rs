//! Raw mode and the alternate screen, released however the process leaves.

use std::io::stdout;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;

pub struct Screen {
    pub terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Screen {
    pub fn enter() -> Result<Self> {
        enable_raw_mode().context("could not put the terminal into raw mode")?;
        // `EnableFocusChange` is what makes `Event::FocusGained` arrive, which is
        // how a pane refreshes on focus. tmux forwards it because our conf sets
        // `focus-events on`.
        // `EnableBracketedPaste` delivers a paste as one `Event::Paste`, so a
        // pasted newline is not mistaken for `enter`.
        // `EnableMouseCapture` delivers clicks and the wheel as `Event::Mouse`.
        // It takes plain drag-to-select from the terminal; most terminals give
        // it back with shift (option in iTerm2) held.
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableFocusChange,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout()))?,
        })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange,
            LeaveAlternateScreen
        );
    }
}
