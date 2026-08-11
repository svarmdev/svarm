use std::{path::PathBuf, time::Duration};

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use svarm_agent::{AgentKind, AgentManager, Result, TerminalPalette, pty_size};

use crate::{
    app::{App, MenuItem, Mode},
    input::{
        ManagementCommand, encode_key, encode_mouse, encode_paste, is_management_prefix,
        management_command,
    },
    settings::SettingsStore,
    terminal::{TerminalSession, colors_enabled},
    ui::{self, UiModel},
};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);

pub fn run(kind: Option<AgentKind>, cwd: PathBuf) -> Result<()> {
    let cwd = cwd.canonicalize().map_err(|error| {
        format!(
            "could not open workspace {}: {error}",
            cwd.to_string_lossy()
        )
    })?;
    let workspace_name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| cwd.to_str().unwrap_or("workspace"))
        .to_owned();
    let palette = TerminalPalette::detect();
    let colors_enabled = colors_enabled();
    let settings = SettingsStore::discover();
    let (theme, settings_notice) = settings.load();

    let mut terminal = TerminalSession::open()?;
    let area = terminal.terminal().size()?;
    let child_area = ui::terminal_area(area.into(), true);
    let mut agents = AgentManager::new(cwd, pty_size(child_area.height, child_area.width), palette);
    let mut app = App::new(workspace_name, theme, kind.is_none(), settings_notice);
    if let Some(kind) = kind {
        app.add_agent(agents.spawn(kind)?);
    }

    let mut dirty = true;
    while !app.quit_requested() {
        for snapshot in agents.poll() {
            match snapshot {
                Ok(snapshot) => dirty |= app.update_agent(snapshot),
                Err(error) => {
                    app.set_notice(error.to_string());
                    dirty = true;
                }
            }
        }

        if dirty {
            app.mark_selected_seen();
            let terminal_snapshot = app
                .selected_agent_id()
                .and_then(|id| agents.terminal_snapshot(id));
            let model = UiModel {
                app: &app,
                screen: terminal_snapshot.as_ref().map(|terminal| terminal.screen()),
                theme: app.theme().theme(colors_enabled),
            };
            terminal.terminal().draw(|frame| ui::render(frame, model))?;
            dirty = false;
        }

        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                let (resize, redraw) = handle_key(&mut app, &mut agents, &settings, key)?;
                dirty |= redraw;
                if resize {
                    resize_agents(&mut agents, &app, terminal.terminal().size()?.into())?;
                }
            }
            Event::Paste(text) if app.mode() == Mode::Terminal => {
                if let Some(id) = app.selected_agent_id() {
                    let bracketed = agents
                        .terminal_snapshot(id)
                        .is_some_and(|terminal| terminal.screen().bracketed_paste());
                    agents.send(id, &encode_paste(&text, bracketed))?;
                }
            }
            Event::Resize(width, height) => {
                dirty = true;
                resize_agents(&mut agents, &app, Rect::new(0, 0, width, height))?;
            }
            Event::Mouse(mouse) => {
                dirty |=
                    handle_mouse(&mut app, &agents, mouse, terminal.terminal().size()?.into())?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle_key(
    app: &mut App,
    agents: &mut AgentManager,
    settings: &SettingsStore,
    key: KeyEvent,
) -> Result<(bool, bool)> {
    if key.kind == KeyEventKind::Release {
        return Ok((false, false));
    }

    let mut redraw = true;
    match app.mode() {
        Mode::Terminal if is_management_prefix(key) => app.set_mode(Mode::Prefix),
        Mode::Terminal => {
            redraw = false;
            if let Some(bytes) = encode_key(key)
                && let Some(id) = app.selected_agent_id()
                && let Err(error) = agents.send(id, &bytes)
            {
                app.set_notice(error.to_string());
                redraw = true;
            }
        }
        Mode::Prefix => return handle_management_command(app, agents, management_command(key)),
        Mode::ChooseAgent => match key.code {
            KeyCode::Char('c') => spawn(app, agents, AgentKind::Codex),
            KeyCode::Char('a') => spawn(app, agents, AgentKind::Claude),
            KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::ConfirmClose => match key.code {
            KeyCode::Char('y') => close_selected(app, agents)?,
            KeyCode::Char('n') | KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::ConfirmQuit => match key.code {
            KeyCode::Char('y') => app.request_quit(),
            KeyCode::Char('n') | KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::Menu => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.select_next_menu_item(),
            KeyCode::Char('k') | KeyCode::Up => app.select_previous_menu_item(),
            KeyCode::Enter => app.open_selected_menu_item(),
            KeyCode::Char(digit @ '1'..='9') => {
                if let Some(item) = MenuItem::ALL.get(digit as usize - '1' as usize) {
                    app.select_menu_item(*item);
                    app.open_selected_menu_item();
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::Keybinds => {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                app.set_mode(Mode::Menu);
            }
        }
        Mode::Settings => match key.code {
            KeyCode::Char('h') | KeyCode::Left => save_theme(app, settings, -1),
            KeyCode::Char('l') | KeyCode::Right => save_theme(app, settings, 1),
            KeyCode::Esc | KeyCode::Char('q') => app.set_mode(Mode::Menu),
            _ => {}
        },
    }
    Ok((false, redraw))
}

fn handle_management_command(
    app: &mut App,
    agents: &AgentManager,
    command: ManagementCommand,
) -> Result<(bool, bool)> {
    let mut resize = false;
    match command {
        ManagementCommand::LiteralPrefix => {
            if let Some(id) = app.selected_agent_id() {
                agents.send(id, &[0x02])?;
            }
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::NextAgent => {
            app.select_next();
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::PreviousAgent => {
            app.select_previous();
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::ChooseAgent => app.set_mode(Mode::ChooseAgent),
        ManagementCommand::CloseAgent if app.selected_agent_id().is_some() => {
            app.set_mode(Mode::ConfirmClose);
        }
        ManagementCommand::ConfirmQuit => app.set_mode(Mode::ConfirmQuit),
        ManagementCommand::ToggleSidebar => {
            app.toggle_sidebar();
            app.set_mode(Mode::Terminal);
            resize = true;
        }
        ManagementCommand::OpenMenu => {
            if !app.sidebar_visible() {
                app.show_sidebar();
                resize = true;
            }
            app.set_mode(Mode::Menu);
        }
        ManagementCommand::OpenKeybinds => app.set_mode(Mode::Keybinds),
        ManagementCommand::SelectAgent(index) => {
            app.select(index);
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::Cancel => app.set_mode(Mode::Terminal),
        ManagementCommand::Unknown | ManagementCommand::CloseAgent => {
            app.set_notice("unknown Svarm command; Ctrl+B m opens menu");
            app.set_mode(Mode::Terminal);
        }
    }
    Ok((resize, true))
}

fn handle_mouse(
    app: &mut App,
    agents: &AgentManager,
    mouse: MouseEvent,
    area: Rect,
) -> Result<bool> {
    if matches!(
        app.mode(),
        Mode::Terminal | Mode::Menu | Mode::Keybinds | Mode::Settings
    ) && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
    {
        if ui::menu_button_area(area, app.sidebar_visible())
            .is_some_and(|button| contains(button, mouse.column, mouse.row))
        {
            app.set_mode(if app.mode() == Mode::Menu {
                Mode::Terminal
            } else {
                Mode::Menu
            });
            return Ok(true);
        }

        if app.mode() == Mode::Menu
            && let Some(item) = ui::menu_item_at(area, mouse.column, mouse.row)
        {
            app.select_menu_item(item);
            app.open_selected_menu_item();
            return Ok(true);
        }
    }

    if app.mode() != Mode::Terminal {
        return Ok(false);
    }
    let child_area = ui::terminal_area(area, app.sidebar_visible());
    if !contains(child_area, mouse.column, mouse.row) {
        return Ok(false);
    }
    let Some(id) = app.selected_agent_id() else {
        return Ok(false);
    };
    let Some(terminal) = agents.terminal_snapshot(id) else {
        return Ok(false);
    };
    let mode = terminal.screen().mouse_protocol_mode();
    let encoding = terminal.screen().mouse_protocol_encoding();

    let translated = MouseEvent {
        column: mouse.column - child_area.x,
        row: mouse.row - child_area.y,
        ..mouse
    };
    if let Some(bytes) = encode_mouse(translated, mode, encoding) {
        agents.send(id, &bytes)?;
    }
    Ok(false)
}

fn close_selected(app: &mut App, agents: &mut AgentManager) -> Result<()> {
    let Some(id) = app.selected_agent_id() else {
        app.set_mode(Mode::Terminal);
        return Ok(());
    };
    agents.close(id)?;
    app.remove_agent(id);
    Ok(())
}

fn spawn(app: &mut App, agents: &mut AgentManager, kind: AgentKind) {
    match agents.spawn(kind) {
        Ok(snapshot) => {
            app.add_agent(snapshot);
            app.clear_notice();
        }
        Err(error) => {
            app.set_notice(error.to_string());
            app.set_mode(Mode::Terminal);
        }
    }
}

fn save_theme(app: &mut App, settings: &SettingsStore, delta: isize) {
    let theme = app.cycle_theme(delta);
    match settings.save_theme(theme) {
        Ok(()) => app.clear_notice(),
        Err(error) => app.set_notice(error),
    }
}

fn resize_agents(agents: &mut AgentManager, app: &App, area: Rect) -> Result<()> {
    let child = ui::terminal_area(area, app.sidebar_visible());
    agents.resize(child.height, child.width)
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    #[test]
    fn ctrl_b_is_the_only_management_prefix() {
        assert!(is_management_prefix(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_management_prefix(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_management_prefix(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::ALT
        )));
    }
}
