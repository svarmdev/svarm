use std::io;

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use svarm_agent::Result;

pub(crate) type SvarmTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) struct TerminalSession {
    terminal: SvarmTerminal,
}

impl TerminalSession {
    pub fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        let mut session = Self { terminal };
        session.terminal.clear()?;
        Ok(session)
    }

    pub fn terminal(&mut self) -> &mut SvarmTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

pub(crate) fn colors_enabled() -> bool {
    !std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}
