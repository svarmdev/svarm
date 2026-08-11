use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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
    loop {
        terminal.terminal().draw(|frame| {
            ui::render_session_chooser(
                frame,
                &chooser,
                unix_time_ms(),
                ThemeName::Dark.theme(colors_enabled()),
            )
        })?;
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
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
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
