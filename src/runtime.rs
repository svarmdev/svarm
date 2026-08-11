use std::{io, path::PathBuf, time::Duration};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use crate::{
    AgentKind, Mode,
    app::{App, pty_size},
    input::{encode_key, encode_paste},
    session::Result,
    ui,
};

type SvarmTerminal = Terminal<CrosstermBackend<io::Stdout>>;

pub fn run(kind: AgentKind, cwd: PathBuf) -> Result<()> {
    let cwd = cwd.canonicalize().map_err(|error| {
        format!(
            "could not open workspace {}: {error}",
            cwd.to_string_lossy()
        )
    })?;
    let mut terminal = TerminalSession::open()?;
    let area = terminal.terminal.size()?;
    let child_area = ui::terminal_area(area.into(), true);
    let mut app = App::new(kind, cwd, pty_size(child_area.height, child_area.width))?;
    let mut last_stamp = Vec::new();
    let mut dirty = true;

    while !app.quit {
        dirty |= app.poll();
        dirty |= last_stamp != app.output_stamp();
        if dirty {
            app.mark_selected_seen();
            terminal.terminal.draw(|frame| ui::render(frame, &app))?;
            last_stamp = app.output_stamp();
            dirty = false;
        }

        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        dirty = true;
        match event::read()? {
            Event::Key(key) => {
                let resize = handle_key(&mut app, key)?;
                if resize {
                    resize_agents(&mut app, terminal.terminal.size()?.into())?;
                }
            }
            Event::Paste(text) if app.mode == Mode::Terminal => {
                if let Some(agent) = app.current() {
                    let bracketed = agent.session.parser().screen().bracketed_paste();
                    agent.session.send(&encode_paste(&text, bracketed))?;
                }
            }
            Event::Resize(width, height) => {
                resize_agents(&mut app, Rect::new(0, 0, width, height))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<bool> {
    if key.kind == KeyEventKind::Release {
        return Ok(false);
    }

    match app.mode {
        Mode::Terminal if is_prefix(key) => app.mode = Mode::Prefix,
        Mode::Terminal => {
            if let Some(bytes) = encode_key(key)
                && let Some(agent) = app.current()
                && let Err(error) = agent.session.send(&bytes)
            {
                app.notice = Some(error.to_string());
            }
        }
        Mode::Prefix => return handle_prefix(app, key),
        Mode::ChooseAgent => match key.code {
            KeyCode::Char('c') => spawn(app, AgentKind::Codex),
            KeyCode::Char('a') => spawn(app, AgentKind::Claude),
            KeyCode::Esc => app.mode = Mode::Terminal,
            _ => {}
        },
        Mode::ConfirmClose => match key.code {
            KeyCode::Char('y') => app.close_selected()?,
            KeyCode::Char('n') | KeyCode::Esc => app.mode = Mode::Terminal,
            _ => {}
        },
        Mode::ConfirmQuit => match key.code {
            KeyCode::Char('y') => app.quit = true,
            KeyCode::Char('n') | KeyCode::Esc => app.mode = Mode::Terminal,
            _ => {}
        },
        Mode::Help => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                app.mode = Mode::Terminal;
            }
        }
    }
    Ok(false)
}

fn handle_prefix(app: &mut App, key: KeyEvent) -> Result<bool> {
    let mut resize = false;
    match key.code {
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(agent) = app.current() {
                agent.session.send(&[0x02])?;
            }
            app.mode = Mode::Terminal;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            app.select_next();
            app.mode = Mode::Terminal;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.select_previous();
            app.mode = Mode::Terminal;
        }
        KeyCode::Char('n') => app.mode = Mode::ChooseAgent,
        KeyCode::Char('x') if !app.agents.is_empty() => app.mode = Mode::ConfirmClose,
        KeyCode::Char('q') => app.mode = Mode::ConfirmQuit,
        KeyCode::Char('b') => {
            app.sidebar_visible = !app.sidebar_visible;
            app.mode = Mode::Terminal;
            resize = true;
        }
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(digit @ '1'..='9') => {
            app.select(digit as usize - '1' as usize);
            app.mode = Mode::Terminal;
        }
        KeyCode::Esc => app.mode = Mode::Terminal,
        _ => {
            app.notice = Some("unknown Svarm command; Ctrl+B ? shows help".into());
            app.mode = Mode::Terminal;
        }
    }
    Ok(resize)
}

fn spawn(app: &mut App, kind: AgentKind) {
    if let Err(error) = app.spawn(kind) {
        app.notice = Some(error.to_string());
        app.mode = Mode::Terminal;
    }
}

fn resize_agents(app: &mut App, area: Rect) -> Result<()> {
    let child = ui::terminal_area(area, app.sidebar_visible);
    app.resize(child.height, child.width)
}

fn is_prefix(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('b') && key.modifiers == KeyModifiers::CONTROL
}

struct TerminalSession {
    terminal: SvarmTerminal,
}

impl TerminalSession {
    fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_b_is_the_only_management_prefix() {
        assert!(is_prefix(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_prefix(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_prefix(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::ALT
        )));
    }
}
