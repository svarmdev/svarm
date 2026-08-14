use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use svarm_agent::{Result, protocol::SessionSummary};

use crate::{
    app::{SessionChooser, StartupChoice},
    terminal::{TerminalSession, colors_enabled},
    theme::ThemeName,
    ui,
};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn choose_session(sessions: Vec<SessionSummary>, allow_new: bool) -> Result<StartupChoice> {
    let mut chooser = SessionChooser::new(sessions, allow_new);
    let mut terminal = TerminalSession::open()?;
    let mut pointer = None;
    loop {
        terminal.terminal().draw(|frame| {
            ui::render_session_chooser(
                frame,
                &chooser,
                unix_time_ms(),
                ThemeName::Dark.theme(colors_enabled()),
                pointer,
            )
        })?;
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Down | KeyCode::Char('j') => chooser.select_next(),
                KeyCode::Up | KeyCode::Char('k') => chooser.select_previous(),
                KeyCode::Enter => {
                    if let Some(choice) = chooser.confirm() {
                        return Ok(choice);
                    }
                }
                KeyCode::Char('n') => {
                    if let Some(choice) = chooser.select_new() {
                        return Ok(choice);
                    }
                }
                KeyCode::Esc => return Ok(chooser.cancel()),
                _ => {}
            },
            Event::Mouse(mouse) => {
                pointer = Some((mouse.column, mouse.row));
                if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                    continue;
                }
                let area = terminal.terminal().size()?.into();
                match ui::session_chooser_click(&chooser, area, mouse.column, mouse.row) {
                    Some(ui::SessionChooserClick::Choose(index)) => {
                        chooser.select(index);
                        if let Some(choice) = chooser.confirm() {
                            return Ok(choice);
                        }
                    }
                    Some(ui::SessionChooserClick::Next) => chooser.select_next(),
                    Some(ui::SessionChooserClick::Previous) => chooser.select_previous(),
                    Some(ui::SessionChooserClick::Open) => {
                        if let Some(choice) = chooser.confirm() {
                            return Ok(choice);
                        }
                    }
                    Some(ui::SessionChooserClick::Cancel) => return Ok(chooser.cancel()),
                    Some(ui::SessionChooserClick::New) => {
                        if let Some(choice) = chooser.select_new() {
                            return Ok(choice);
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
