use std::{io, thread};

use crossterm::{
    clipboard::CopyToClipboard,
    cursor::{SetCursorStyle, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use svarm_agent::{CursorStyle, Result};

pub(crate) type SvarmTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub(crate) struct TerminalSession {
    terminal: SvarmTerminal,
    cursor_style: CursorStyle,
    keyboard_enhanced: bool,
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
        // Without this the host terminal reports Shift+Enter as a plain Enter, and no amount of
        // care further down can recover the difference. Only disambiguation is requested: key
        // release and repeat events would multiply the input Svarm forwards without adding
        // anything an agent asked for.
        let keyboard_enhanced = supports_keyboard_enhancement().unwrap_or(false)
            && execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .is_ok();
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        let mut session = Self {
            terminal,
            cursor_style: CursorStyle::default(),
            keyboard_enhanced,
        };
        session.terminal.clear()?;
        Ok(session)
    }

    pub fn terminal(&mut self) -> &mut SvarmTerminal {
        &mut self.terminal
    }

    /// Reads host input on its own thread, so the interface can block on one channel carrying both
    /// keystrokes and server frames instead of waking up to check whether input arrived.
    ///
    /// Started after the terminal is configured, so the first read already sees raw mode, the
    /// bracketed paste and mouse modes, and any keyboard enhancement this session enabled.
    pub fn spawn_input(&self, mut deliver: impl FnMut(Event) -> bool + Send + 'static) {
        thread::spawn(move || {
            while let Ok(event) = event::read() {
                if !deliver(event) {
                    break;
                }
            }
        });
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

    /// Writes through the terminal protocol so copying also works over SSH and through
    /// multiplexers that pass OSC 52 to the outer terminal.
    pub fn copy_to_clipboard(&mut self, text: &str) -> Result<()> {
        execute!(
            self.terminal.backend_mut(),
            CopyToClipboard::to_clipboard_from(text)
        )?;
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
        if self.keyboard_enhanced {
            let _ = execute!(self.terminal.backend_mut(), PopKeyboardEnhancementFlags);
        }
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

#[cfg(test)]
mod tests {
    use crossterm::{Command, clipboard::CopyToClipboard};

    #[test]
    fn clipboard_command_uses_osc_52_clipboard_destination() {
        let mut output = String::new();
        CopyToClipboard::to_clipboard_from("copy me")
            .write_ansi(&mut output)
            .unwrap();
        assert!(output.starts_with("\x1b]52;c;"));
        assert!(output.contains("Y29weSBtZQ=="));
    }
}
