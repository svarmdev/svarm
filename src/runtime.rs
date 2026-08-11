use std::{io, path::PathBuf, time::Duration};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};

use crate::{
    AgentKind, Mode,
    app::{App, pty_size},
    input::{encode_key, encode_mouse, encode_paste},
    session::{Result, TerminalPalette},
    ui,
};

type SvarmTerminal = Terminal<CrosstermBackend<io::Stdout>>;
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);

pub fn run(kind: AgentKind, cwd: PathBuf) -> Result<()> {
    let cwd = cwd.canonicalize().map_err(|error| {
        format!(
            "could not open workspace {}: {error}",
            cwd.to_string_lossy()
        )
    })?;
    let palette = TerminalPalette::detect();
    let mut terminal = TerminalSession::open()?;
    let area = terminal.terminal.size()?;
    let child_area = ui::terminal_area(area.into(), true);
    let mut app = App::new(
        kind,
        cwd,
        pty_size(child_area.height, child_area.width),
        palette,
    )?;
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

        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                let (resize, redraw) = handle_key(&mut app, key)?;
                dirty |= redraw;
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
                dirty = true;
                resize_agents(&mut app, Rect::new(0, 0, width, height))?;
            }
            Event::Mouse(mouse) => {
                dirty |= handle_mouse(&mut app, mouse, terminal.terminal.size()?.into())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) -> Result<(bool, bool)> {
    if key.kind == KeyEventKind::Release {
        return Ok((false, false));
    }

    let mut redraw = true;
    match app.mode {
        Mode::Terminal if is_prefix(key) => app.mode = Mode::Prefix,
        Mode::Terminal => {
            redraw = false;
            if let Some(bytes) = encode_key(key)
                && let Some(agent) = app.current()
                && let Err(error) = agent.session.send(&bytes)
            {
                app.notice = Some(error.to_string());
                redraw = true;
            }
        }
        Mode::Prefix => return handle_prefix(app, key).map(|resize| (resize, true)),
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
        Mode::Menu => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.select_next_menu_item(),
            KeyCode::Char('k') | KeyCode::Up => app.select_previous_menu_item(),
            KeyCode::Enter => open_menu_item(app),
            KeyCode::Char('1') => {
                app.menu_selected = 0;
                open_menu_item(app);
            }
            KeyCode::Char('2') => {
                app.menu_selected = 1;
                open_menu_item(app);
            }
            KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Terminal,
            _ => {}
        },
        Mode::Keybinds => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                app.mode = Mode::Menu;
            }
        }
        Mode::Settings => match key.code {
            KeyCode::Char('h') | KeyCode::Left => app.theme.cycle(-1),
            KeyCode::Char('l') | KeyCode::Right => app.theme.cycle(1),
            KeyCode::Esc | KeyCode::Char('q') => app.mode = Mode::Menu,
            _ => {}
        },
    }
    Ok((false, redraw))
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
        KeyCode::Char('m') => {
            if !app.sidebar_visible {
                app.sidebar_visible = true;
                resize = true;
            }
            app.mode = Mode::Menu;
        }
        KeyCode::Char('?') => app.mode = Mode::Keybinds,
        KeyCode::Char(digit @ '1'..='9') => {
            app.select(digit as usize - '1' as usize);
            app.mode = Mode::Terminal;
        }
        KeyCode::Esc => app.mode = Mode::Terminal,
        _ => {
            app.notice = Some("unknown Svarm command; Ctrl+B m opens menu".into());
            app.mode = Mode::Terminal;
        }
    }
    Ok(resize)
}

fn open_menu_item(app: &mut App) {
    app.mode = if app.menu_selected == 0 {
        Mode::Keybinds
    } else {
        Mode::Settings
    };
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, area: Rect) -> Result<bool> {
    if matches!(
        app.mode,
        Mode::Terminal | Mode::Menu | Mode::Keybinds | Mode::Settings
    ) && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
    {
        if ui::menu_button_area(area, app.sidebar_visible)
            .is_some_and(|button| contains(button, mouse.column, mouse.row))
        {
            app.mode = if app.mode == Mode::Menu {
                Mode::Terminal
            } else {
                Mode::Menu
            };
            return Ok(true);
        }

        if app.mode == Mode::Menu
            && let Some(index) = ui::menu_item_at(area, mouse.column, mouse.row)
        {
            app.menu_selected = index;
            open_menu_item(app);
            return Ok(true);
        }
    }

    if app.mode != Mode::Terminal {
        return Ok(false);
    }
    let child_area = ui::terminal_area(area, app.sidebar_visible);
    if !contains(child_area, mouse.column, mouse.row) {
        return Ok(false);
    }
    let Some(agent) = app.current() else {
        return Ok(false);
    };
    let (mode, encoding) = {
        let parser = agent.session.parser();
        (
            parser.screen().mouse_protocol_mode(),
            parser.screen().mouse_protocol_encoding(),
        )
    };
    let translated = MouseEvent {
        column: mouse.column - child_area.x,
        row: mouse.row - child_area.y,
        ..mouse
    };
    if let Some(bytes) = encode_mouse(translated, mode, encoding) {
        agent.session.send(&bytes)?;
    }
    Ok(false)
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
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
