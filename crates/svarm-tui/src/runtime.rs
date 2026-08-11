use std::{path::PathBuf, time::Duration};

use crossterm::event::{
    self, Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::Rect;
use svarm_agent::{AgentKind, Result, TerminalPalette, protocol::Event as ServerEvent};

use crate::{
    agents::{InitialSession, RemoteAgents, RemoteUpdate},
    app::{App, ExitIntent, MenuItem, Mode},
    input::{ManagementCommand, is_management_prefix, key_input, management_command, mouse_input},
    settings::SettingsStore,
    terminal::{TerminalSession, colors_enabled},
    ui::{self, UiModel},
};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(16);

pub fn run(kind: Option<AgentKind>, socket_path: PathBuf, target: InitialSession) -> Result<()> {
    let target = canonicalize_target(target)?;
    let palette = TerminalPalette::detect();
    let colors_enabled = colors_enabled();
    let settings = SettingsStore::discover();
    let (theme, settings_notice) = settings.load();
    let (width, height) = crossterm::terminal::size()?;
    let child_area = ui::terminal_area(Rect::new(0, 0, width, height), true);
    let (mut agents, snapshot) = RemoteAgents::connect(
        &socket_path,
        target,
        child_area.height.max(1),
        child_area.width.max(1),
        palette,
    )?;
    let mut app = App::hydrate(snapshot, theme, settings_notice);
    if let Some(kind) = kind {
        agents.spawn(kind)?;
    }

    let mut terminal = TerminalSession::open()?;
    let mut dirty = true;
    let mut connection_failure = None;
    while app.exit_intent() == ExitIntent::None {
        dirty |= apply_remote_updates(&mut app, &mut agents, &mut connection_failure);
        if connection_failure.is_some() {
            break;
        }

        if dirty {
            let screen = app.selected_agent_id().and_then(|id| agents.screen(id));
            let model = UiModel {
                app: &app,
                screen,
                theme: app.theme().theme(colors_enabled),
            };
            terminal.terminal().draw(|frame| ui::render(frame, model))?;
            if let Some((id, generation)) = app.mark_selected_seen() {
                agents.mark_seen(id, generation)?;
            }
            dirty = false;
        }

        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            HostEvent::Key(key) => {
                let (resize, redraw) = handle_key(&mut app, &mut agents, &settings, key)?;
                dirty |= redraw;
                if resize {
                    resize_agents(&mut agents, &app, terminal.terminal().size()?.into())?;
                }
            }
            HostEvent::Paste(text) if app.mode() == Mode::Terminal => {
                if let Some(id) = app.selected_agent_id() {
                    agents.paste(id, text)?;
                }
            }
            HostEvent::Resize(width, height) => {
                dirty = true;
                resize_agents(&mut agents, &app, Rect::new(0, 0, width, height))?;
            }
            HostEvent::Mouse(mouse) => {
                dirty |= handle_mouse(
                    &mut app,
                    &mut agents,
                    mouse,
                    terminal.terminal().size()?.into(),
                )?;
            }
            _ => {}
        }
    }

    drop(terminal);
    if let Some(error) = connection_failure {
        return Err(connection_failure_message(&error, agents.session_id().0).into());
    }
    match app.exit_intent() {
        ExitIntent::Detach => agents.detach()?,
        ExitIntent::StopSession => {
            let summary = agents.stop()?;
            if summary.cleanup_errors > 0 {
                return Err(format!(
                    "Svarm session stopped with {} agent cleanup errors",
                    summary.cleanup_errors
                )
                .into());
            }
        }
        ExitIntent::None => {}
    }
    Ok(())
}

fn canonicalize_target(target: InitialSession) -> Result<InitialSession> {
    match target {
        InitialSession::Create(path) => {
            let canonical = path.canonicalize().map_err(|error| {
                format!(
                    "could not open workspace {}: {error}",
                    path.to_string_lossy()
                )
            })?;
            Ok(InitialSession::Create(canonical))
        }
        target => Ok(target),
    }
}

fn apply_remote_updates(
    app: &mut App,
    agents: &mut RemoteAgents,
    connection_failure: &mut Option<String>,
) -> bool {
    let mut dirty = false;
    for update in agents.drain() {
        match update {
            RemoteUpdate::Event(ServerEvent::AgentAdded { agent, .. }) => {
                app.add_remote_agent(agent);
                dirty = true;
            }
            RemoteUpdate::Event(ServerEvent::AgentChanged { agent, .. }) => {
                dirty |= app.update_remote_agent(agent);
            }
            RemoteUpdate::Event(ServerEvent::AgentRemoved { agent_id, .. }) => {
                app.remove_agent(agent_id);
                dirty = true;
            }
            RemoteUpdate::Event(ServerEvent::SessionNotice(notice)) => {
                app.set_notice(notice.message);
                dirty = true;
            }
            RemoteUpdate::Event(ServerEvent::LeaseRevoked { reason }) => {
                *connection_failure = Some(reason);
            }
            RemoteUpdate::Event(ServerEvent::ServerStopping) => {
                *connection_failure = Some("Svarm server is stopping".into());
            }
            RemoteUpdate::Event(
                ServerEvent::SvarmSessionSnapshot(_) | ServerEvent::SvarmSessionChanged(_),
            ) => {
                dirty = true;
            }
            RemoteUpdate::Event(ServerEvent::TerminalFull(_) | ServerEvent::TerminalDiff(_)) => {
                unreachable!("terminal frames are applied by adapter")
            }
            RemoteUpdate::TerminalChanged => dirty = true,
            RemoteUpdate::Error(error) => {
                app.set_notice(error);
                dirty = true;
            }
            RemoteUpdate::Disconnected(error) => *connection_failure = Some(error),
        }
    }
    dirty
}

fn handle_key(
    app: &mut App,
    agents: &mut RemoteAgents,
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
            if let Some(event) = key_input(key)
                && let Some(id) = app.selected_agent_id()
            {
                agents.key(id, event)?;
            }
        }
        Mode::Prefix => return handle_management_command(app, agents, management_command(key)),
        Mode::ChooseAgent => match key.code {
            KeyCode::Char('c') => spawn(app, agents, AgentKind::Codex)?,
            KeyCode::Char('a') => spawn(app, agents, AgentKind::Claude)?,
            KeyCode::Esc if app.selected_agent_id().is_some() => app.set_mode(Mode::Terminal),
            KeyCode::Esc => app.request_detach(),
            _ => {}
        },
        Mode::ConfirmClose => match key.code {
            KeyCode::Char('y') => close_selected(app, agents)?,
            KeyCode::Char('n') | KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::ConfirmQuit => match key.code {
            KeyCode::Char('y') => app.request_stop(),
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
    agents: &mut RemoteAgents,
    command: ManagementCommand,
) -> Result<(bool, bool)> {
    let mut resize = false;
    match command {
        ManagementCommand::LiteralPrefix => {
            if let Some(id) = app.selected_agent_id() {
                agents.literal(id, vec![0x02])?;
            }
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::NextAgent => {
            app.select_next();
            sync_selection(app, agents)?;
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::PreviousAgent => {
            app.select_previous();
            sync_selection(app, agents)?;
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::ChooseAgent => app.set_mode(Mode::ChooseAgent),
        ManagementCommand::CloseAgent if app.selected_agent_id().is_some() => {
            app.set_mode(Mode::ConfirmClose);
        }
        ManagementCommand::ConfirmQuit => app.set_mode(Mode::ConfirmQuit),
        ManagementCommand::Detach => app.request_detach(),
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
            sync_selection(app, agents)?;
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
    agents: &mut RemoteAgents,
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
    let translated = MouseEvent {
        column: mouse.column - child_area.x,
        row: mouse.row - child_area.y,
        ..mouse
    };
    agents.mouse(id, mouse_input(translated))?;
    Ok(false)
}

fn close_selected(app: &mut App, agents: &mut RemoteAgents) -> Result<()> {
    let Some(id) = app.selected_agent_id() else {
        app.set_mode(Mode::Terminal);
        return Ok(());
    };
    agents.close(id)?;
    app.set_mode(Mode::Terminal);
    Ok(())
}

fn spawn(app: &mut App, agents: &mut RemoteAgents, kind: AgentKind) -> Result<()> {
    agents.spawn(kind)?;
    app.clear_notice();
    app.set_mode(Mode::Terminal);
    Ok(())
}

fn sync_selection(app: &App, agents: &mut RemoteAgents) -> Result<()> {
    if let Some(id) = app.selected_agent_id() {
        agents.select(id)?;
    }
    Ok(())
}

fn save_theme(app: &mut App, settings: &SettingsStore, delta: isize) {
    let theme = app.cycle_theme(delta);
    match settings.save_theme(theme) {
        Ok(()) => app.clear_notice(),
        Err(error) => app.set_notice(error),
    }
}

fn resize_agents(agents: &mut RemoteAgents, app: &App, area: Rect) -> Result<()> {
    let child = ui::terminal_area(area, app.sidebar_visible());
    agents.resize(child.height.max(1), child.width.max(1))
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn connection_failure_message(reason: &str, session_id: u64) -> String {
    let reason = reason.replace(['\r', '\n'], " ");
    let mut characters = reason.chars();
    let mut first_line = characters.by_ref().take(79).collect::<String>();
    if characters.next().is_some() {
        first_line.push('…');
    }
    format!(
        "{first_line}\nAgents may still be running.\nReattach: svarm --attach --workspace {session_id}"
    )
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

    #[test]
    fn connection_notices_fit_at_80_columns_and_include_reattach_command() {
        let message = connection_failure_message(&"connection lost ".repeat(20), 42);
        assert!(message.lines().all(|line| line.chars().count() <= 80));
        assert!(message.contains("Reattach: svarm --attach --workspace 42"));

        let revoked = connection_failure_message(
            "another client explicitly took over this Svarm session",
            42,
        );
        assert!(revoked.lines().all(|line| line.chars().count() <= 80));
        assert!(revoked.starts_with("another client explicitly took over"));
    }
}
