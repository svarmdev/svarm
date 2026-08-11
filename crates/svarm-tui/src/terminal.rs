use std::io;

use crossterm::{
    cursor::{SetCursorStyle, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use svarm_agent::{CursorStyle, Result};

pub(crate) type SvarmTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) struct TerminalSession {
    terminal: SvarmTerminal,
    cursor_style: CursorStyle,
}

impl TerminalSession {
    pub fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // The cursor is deliberately not hidden here: Ratatui shows and places it for any frame
        // that sets a cursor position, and hides it for any frame that does not.
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        ) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        let mut session = Self {
            terminal,
            cursor_style: CursorStyle::default(),
        };
        session.terminal.clear()?;
        Ok(session)
    }

    pub fn terminal(&mut self) -> &mut SvarmTerminal {
        &mut self.terminal
    }

    /// Asks the host terminal for the cursor the agent requested. `Default` restores the user's
    /// own configured shape and blink, which is what an agent means when it sends DECSCUSR 0 and
    /// what should apply while no agent has asked for anything.
    pub fn set_cursor_style(&mut self, style: CursorStyle) -> Result<()> {
        if self.cursor_style == style {
            return Ok(());
        }
        self.cursor_style = style;
        execute!(self.terminal.backend_mut(), cursor_style(style))?;
        Ok(())
    }
}

const fn cursor_style(style: CursorStyle) -> SetCursorStyle {
    match style {
        CursorStyle::Default => SetCursorStyle::DefaultUserShape,
        CursorStyle::BlinkingBlock => SetCursorStyle::BlinkingBlock,
        CursorStyle::SteadyBlock => SetCursorStyle::SteadyBlock,
        CursorStyle::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
        CursorStyle::SteadyUnderline => SetCursorStyle::SteadyUnderScore,
        CursorStyle::BlinkingBar => SetCursorStyle::BlinkingBar,
        CursorStyle::SteadyBar => SetCursorStyle::SteadyBar,
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            SetCursorStyle::DefaultUserShape,
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
