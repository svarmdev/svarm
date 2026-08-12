use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, path::PathBuf, sync::mpsc};

use crossterm::event::{
    Event as HostEvent, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use svarm_agent::{
    AgentKind, Result, SessionStatus, TerminalPalette, TerminalProcessSnapshot,
    input::{encode_key, encode_mouse, encode_paste},
    protocol::{
        Event as ServerEvent, InputModifiers, KeyCode as AgentKeyCode, KeyInput as AgentKeyInput,
    },
};

use crate::{
    agents::{
        ClientEvent, InitialAgentRequest, InitialSession, RemoteAgents, RemoteUpdate, WheelRouting,
    },
    app::{
        App, BrowserAction, ExitIntent, MenuItem, Mode, NewAgentField, NewAgentPage,
        WorkspaceChoice,
    },
    input::{ManagementCommand, is_management_prefix, key_input, management_command, mouse_input},
    settings::{Settings, SettingsStore},
    terminal::{TerminalSession, colors_enabled},
    ui::{self, UiModel},
    workspace::{DirectoryLoader, YaziLaunchError, YaziPicker, YaziResult},
};

const EVENT_QUEUE: usize = 1_024;

pub fn run(
    initial_agent: InitialAgentRequest,
    invocation_directory: Option<PathBuf>,
    runtime_directory: PathBuf,
    socket_path: PathBuf,
    target: InitialSession,
) -> Result<()> {
    let palette = TerminalPalette::detect();
    let colors_enabled = colors_enabled();
    let settings = SettingsStore::discover();
    let (mut settings_value, settings_notice) = settings.load();
    let (width, height) = crossterm::terminal::size()?;
    let child_area = ui::terminal_area(Rect::new(0, 0, width, height), true);
    let (events_tx, events) = mpsc::sync_channel(EVENT_QUEUE);
    let embedded_pending = std::sync::Arc::new(AtomicBool::new(false));
    let mut browser = BrowserRuntime {
        loader: DirectoryLoader::new(events_tx.clone()),
        generation: 0,
        invocation_directory,
        runtime_directory,
        palette,
        yazi: None,
        embedded_pending: embedded_pending.clone(),
        embedded_notify: {
            let events = events_tx.clone();
            let pending = embedded_pending;
            std::sync::Arc::new(move || {
                if !pending.swap(true, Ordering::AcqRel)
                    && events.try_send(ClientEvent::EmbeddedToolChanged).is_err()
                {
                    pending.store(false, Ordering::Release);
                }
            })
        },
    };
    let (mut agents, snapshot) = RemoteAgents::connect(
        &socket_path,
        target,
        child_area.height.max(1),
        child_area.width.max(1),
        palette,
        events_tx.clone(),
    )?;
    let mut app = App::hydrate(snapshot, settings_value.theme, settings_notice);
    let explicit_kind = initial_agent.kind;
    let explicit_workspace = initial_agent.workspace;
    let default_kind = explicit_kind.or(settings_value.last_agent);
    let default_workspace = explicit_workspace
        .clone()
        .or_else(|| remembered_workspace(&settings_value));
    if explicit_kind.is_some()
        && let (Some(kind), Some(launch_directory)) = (default_kind, default_workspace.clone())
    {
        agents.spawn(kind, launch_directory.clone(), &events)?;
        settings_value.record_successful_launch(launch_directory, kind);
        if let Err(error) = settings.save(&settings_value) {
            app.set_notice(error);
        }
    } else if explicit_kind.is_some() || explicit_workspace.is_some() {
        app.open_new_agent(
            default_workspace,
            default_kind,
            workspace_choices(&settings_value, explicit_workspace.as_ref()),
        );
    }

    let mut terminal = TerminalSession::open()?;
    terminal.spawn_input(move |event| events_tx.send(ClientEvent::Host(event)).is_ok());
    let mut dirty = true;
    let mut connection_failure = None;
    while app.exit_intent() == ExitIntent::None && connection_failure.is_none() {
        if dirty {
            if let Some((id, generation)) = app.mark_selected_seen() {
                agents.mark_seen(id, generation)?;
            }
            let selected = app.selected_agent_id();
            let embedded = browser.snapshot();
            let model = UiModel {
                app: &app,
                screen: selected.and_then(|id| agents.screen(id)),
                scrolled: selected.is_some_and(|id| agents.is_scrolled(id)),
                embedded: embedded.as_ref(),
                theme: app.theme().theme(colors_enabled),
                colors_enabled,
            };
            let cursor_style = embedded.as_ref().map_or_else(
                || {
                    selected
                        .and_then(|id| agents.cursor_style(id))
                        .unwrap_or_default()
                },
                |snapshot| snapshot.terminal.state.cursor.style,
            );
            terminal.set_cursor_style(cursor_style)?;
            terminal.terminal().draw(|frame| ui::render(frame, model))?;
            dirty = false;
        }

        // Sleep until something actually happens, then absorb everything else already queued so a
        // burst of agent output costs one redraw instead of one redraw per frame.
        let Ok(first) = events.recv() else { break };
        for event in std::iter::once(first).chain(events.try_iter()) {
            match event {
                ClientEvent::Remote(incoming) => {
                    for update in agents.apply(incoming) {
                        dirty |= apply_remote_update(&mut app, &mut connection_failure, update);
                    }
                }
                ClientEvent::Host(host) => {
                    let host_area = terminal.terminal().size()?.into();
                    let resources = InteractionResources {
                        settings_store: &settings,
                        settings: &mut settings_value,
                        events: &events,
                        browser: &mut browser,
                        host_area,
                    };
                    dirty |=
                        handle_host_event(&mut app, &mut agents, resources, &mut terminal, host)?;
                }
                ClientEvent::DirectoryLoaded(result) => {
                    dirty |=
                        app.apply_directory_load(result.generation, result.path, result.result);
                }
                ClientEvent::EmbeddedToolChanged => {
                    browser.embedded_pending.store(false, Ordering::Release);
                    browser.poll_yazi(&mut app);
                    dirty = true;
                }
            }
        }
    }

    drop(terminal);
    if let Some(error) = connection_failure {
        return Err(connection_failure_message(&error, agents.session_id().0).into());
    }
    match app.exit_intent() {
        ExitIntent::Detach => agents.detach(&events)?,
        ExitIntent::StopSession => {
            let summary = agents.stop(&events)?;
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

fn handle_host_event(
    app: &mut App,
    agents: &mut RemoteAgents,
    mut resources: InteractionResources<'_>,
    terminal: &mut TerminalSession,
    event: HostEvent,
) -> Result<bool> {
    let mut dirty = false;
    match event {
        HostEvent::Key(key) => {
            let (resize, redraw) = handle_key(app, agents, &mut resources, key)?;
            dirty |= redraw;
            if resize {
                resize_agents(agents, app, terminal.terminal().size()?.into())?;
            }
        }
        HostEvent::Paste(text) if app.mode() == Mode::Terminal => {
            if let Some(id) = app.selected_agent_id() {
                dirty |= agents.show_live(id);
                agents.paste(id, text)?;
            }
        }
        HostEvent::Paste(text) if app.mode() == Mode::NewAgent(NewAgentPage::EmbeddedBrowser) => {
            if let Err(error) = resources.browser.paste(&text) {
                app.set_notice(format!("could not paste into Yazi: {error}"));
            }
        }
        HostEvent::Resize(width, height) => {
            dirty = true;
            let area = Rect::new(0, 0, width, height);
            resize_agents(agents, app, area)?;
            if let Err(error) = resources.browser.resize(area) {
                app.set_notice(format!("could not resize Yazi: {error}"));
                resources.browser.force_close(app);
            }
        }
        HostEvent::Mouse(mouse) => {
            let area = terminal.terminal().size()?.into();
            let (resize, redraw) = handle_mouse(app, agents, &mut resources, mouse, area)?;
            dirty |= redraw;
            if resize {
                resize_agents(agents, app, terminal.terminal().size()?.into())?;
            }
        }
        _ => {}
    }
    Ok(dirty)
}

fn apply_remote_update(
    app: &mut App,
    connection_failure: &mut Option<String>,
    update: RemoteUpdate,
) -> bool {
    let mut dirty = false;
    match update {
        RemoteUpdate::Event(event) => match *event {
            ServerEvent::AgentAdded { agent, .. } => {
                app.add_remote_agent(agent);
                dirty = true;
            }
            ServerEvent::AgentChanged { agent, .. } => {
                dirty |= app.update_remote_agent(agent);
            }
            ServerEvent::AgentRemoved { agent_id, .. } => {
                app.remove_agent(agent_id);
                dirty = true;
            }
            ServerEvent::AgentArchived {
                agent_id,
                conversation,
                ..
            } => {
                app.archive_remote_agent(agent_id, conversation);
                dirty = true;
            }
            ServerEvent::ArchivedResumed {
                conversation_id,
                agent,
                ..
            } => {
                app.resume_remote_agent(&conversation_id, *agent);
                dirty = true;
            }
            ServerEvent::ConversationSwitched {
                agent,
                archived,
                reactivated_id,
                ..
            } => {
                app.apply_conversation_switch(*agent, archived, reactivated_id.as_deref());
                dirty = true;
            }
            ServerEvent::SessionNotice(notice) => {
                app.set_notice(notice.message);
                dirty = true;
            }
            ServerEvent::LeaseRevoked { reason } => {
                *connection_failure = Some(reason);
            }
            ServerEvent::ServerStopping => {
                *connection_failure = Some("Svarm server is stopping".into());
            }
            ServerEvent::SvarmSessionSnapshot(_) | ServerEvent::SvarmSessionChanged(_) => {
                dirty = true;
            }
            ServerEvent::TerminalFull(_)
            | ServerEvent::TerminalDiff(_)
            | ServerEvent::TerminalViewport(_) => {
                unreachable!("terminal frames are applied by adapter")
            }
        },
        RemoteUpdate::TerminalChanged => dirty = true,
        RemoteUpdate::Error(error) => {
            app.set_notice(error);
            dirty = true;
        }
        RemoteUpdate::Disconnected(error) => *connection_failure = Some(error),
    }
    dirty
}

fn handle_key(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
    key: KeyEvent,
) -> Result<(bool, bool)> {
    if key.kind == KeyEventKind::Release {
        return Ok((false, false));
    }

    let mut redraw = true;
    match app.mode() {
        Mode::Terminal if is_management_prefix(key) => app.set_mode(Mode::Prefix),
        Mode::Terminal => {
            redraw = app
                .selected_agent_id()
                .is_some_and(|id| agents.show_live(id));
            if let Some(event) = key_input(key)
                && let Some(id) = app.selected_agent_id()
            {
                agents.key(id, event)?;
            }
        }
        Mode::Prefix => {
            return handle_management_command(
                app,
                agents,
                resources.settings,
                resources.host_area,
                management_command(key),
            );
        }
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) if is_management_prefix(key) => {
            app.set_mode(Mode::ToolPrefix);
        }
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) => {
            if let Err(error) = resources.browser.send_key(key) {
                app.set_notice(format!("could not send input to Yazi: {error}"));
            }
        }
        Mode::ToolPrefix => {
            if is_management_prefix(key) {
                if let Err(error) = resources.browser.send_literal_prefix() {
                    app.set_notice(format!("could not send input to Yazi: {error}"));
                }
                app.open_embedded_browser();
            } else if key.code == KeyCode::Char('x') {
                resources.browser.force_close(app);
            } else {
                app.open_embedded_browser();
            }
        }
        Mode::NewAgent(page) => {
            handle_new_agent_key(app, agents, resources, page, key);
        }
        Mode::ConfirmClose => match key.code {
            KeyCode::Char('y') => close_selected(app, agents)?,
            KeyCode::Char('n') | KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::ConfirmArchive => match key.code {
            KeyCode::Char('y') => archive_selected(app, agents)?,
            KeyCode::Char('n') | KeyCode::Esc => app.cancel_confirmation(),
            _ => {}
        },
        Mode::ArchiveUnavailable => match key.code {
            KeyCode::Enter | KeyCode::Esc => app.set_mode(Mode::Terminal),
            _ => {}
        },
        Mode::ConfirmResume => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.cycle_pending_archive(1),
            KeyCode::Char('k') | KeyCode::Up => app.cycle_pending_archive(-1),
            KeyCode::Char('y') => resume_selected_archive(app, agents)?,
            KeyCode::Char('n') | KeyCode::Esc => app.cancel_confirmation(),
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
            KeyCode::Char('h') | KeyCode::Left => {
                save_theme(app, resources.settings_store, resources.settings, -1)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                save_theme(app, resources.settings_store, resources.settings, 1)
            }
            KeyCode::Esc | KeyCode::Char('q') => app.set_mode(Mode::Menu),
            _ => {}
        },
    }
    Ok((false, redraw))
}

fn handle_management_command(
    app: &mut App,
    agents: &mut RemoteAgents,
    settings: &Settings,
    area: Rect,
    command: ManagementCommand,
) -> Result<(bool, bool)> {
    let mut resize = false;
    match command {
        ManagementCommand::LiteralPrefix => {
            if let Some(id) = app.selected_agent_id() {
                agents.show_live(id);
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
        ManagementCommand::ScrollTerminalUp | ManagementCommand::ScrollTerminalDown => {
            if let Some(id) = app.selected_agent_id() {
                let page = isize::try_from(ui::terminal_area(area, app.sidebar_visible()).height)
                    .unwrap_or(isize::MAX);
                let rows = if command == ManagementCommand::ScrollTerminalUp {
                    page
                } else {
                    -page
                };
                agents.scroll(id, rows)?;
            }
            app.set_mode(Mode::Terminal);
        }
        ManagementCommand::ChooseAgent => app.open_new_agent(
            remembered_workspace(settings),
            settings.last_agent,
            workspace_choices(settings, None),
        ),
        ManagementCommand::CloseAgent if app.selected_agent_id().is_some() => {
            app.set_mode(Mode::ConfirmClose);
        }
        ManagementCommand::ArchiveAgent => {
            if app.request_archive_selected() {
                archive_selected(app, agents)?;
            }
        }
        ManagementCommand::ResumeArchived => {
            if !app.request_resume_archived(0) {
                app.set_mode(Mode::Terminal);
            }
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
            if app.select_sidebar_index(index) {
                sync_selection(app, agents)?;
                app.set_mode(Mode::Terminal);
            }
        }
        ManagementCommand::Cancel => app.set_mode(Mode::Terminal),
        ManagementCommand::Unknown | ManagementCommand::CloseAgent => {
            app.set_mode(Mode::Terminal);
        }
    }
    Ok((resize, true))
}

fn handle_mouse(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
    mouse: MouseEvent,
    area: Rect,
) -> Result<(bool, bool)> {
    if app.mode() == Mode::Terminal && handle_sidebar_wheel(app, mouse, area) {
        return Ok((false, true));
    }

    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(action) = ui::click_action(app, area, mouse.column, mouse.row)
    {
        return apply_click_action(app, agents, resources, area, action);
    }

    if app.mode() == Mode::NewAgent(NewAgentPage::EmbeddedBrowser) {
        if let Err(error) = resources.browser.mouse(mouse, area) {
            app.set_notice(format!("could not send mouse input to Yazi: {error}"));
            return Ok((false, true));
        }
        return Ok((false, false));
    }

    if app.mode() != Mode::Terminal {
        return Ok((false, false));
    }
    let child_area = ui::terminal_area(area, app.sidebar_visible());
    if !contains(child_area, mouse.column, mouse.row) {
        return Ok((false, false));
    }
    let Some(id) = app.selected_agent_id() else {
        return Ok((false, false));
    };
    if let Some(steps) = wheel_steps(mouse.kind) {
        match agents.wheel_routing(id) {
            WheelRouting::ChildMouse => {}
            WheelRouting::AlternateScreen => {
                agents.show_live(id);
                agents.key(id, alternate_scroll_input(steps))?;
                return Ok((false, true));
            }
            WheelRouting::Scrollback => {
                agents.scroll(id, -steps * 3)?;
                return Ok((false, true));
            }
        }
    }
    if agents.wheel_routing(id) != WheelRouting::ChildMouse {
        return Ok((false, false));
    }
    let redraw = agents.show_live(id);
    let translated = MouseEvent {
        column: mouse.column - child_area.x,
        row: mouse.row - child_area.y,
        ..mouse
    };
    agents.mouse(id, mouse_input(translated))?;
    Ok((false, redraw))
}

fn apply_click_action(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
    area: Rect,
    action: ui::ClickAction,
) -> Result<(bool, bool)> {
    match action {
        ui::ClickAction::Management(command) => {
            handle_management_command(app, agents, resources.settings, area, command)
        }
        ui::ClickAction::ToggleMenu => {
            app.set_mode(if app.mode() == Mode::Menu {
                Mode::Terminal
            } else {
                Mode::Menu
            });
            Ok((false, true))
        }
        ui::ClickAction::SidebarItem(index) => {
            if app.select_sidebar_index(index) {
                sync_selection(app, agents)?;
            }
            Ok((false, true))
        }
        ui::ClickAction::MenuItem(item) => {
            app.select_menu_item(item);
            app.open_selected_menu_item();
            Ok((false, true))
        }
        ui::ClickAction::Next => {
            if app.mode() == Mode::ConfirmResume {
                app.cycle_pending_archive(1);
            } else {
                app.move_new_agent_selection(1);
            }
            Ok((false, true))
        }
        ui::ClickAction::Previous => {
            if app.mode() == Mode::ConfirmResume {
                app.cycle_pending_archive(-1);
            } else {
                app.move_new_agent_selection(-1);
            }
            Ok((false, true))
        }
        ui::ClickAction::Confirm => {
            match app.mode() {
                Mode::NewAgent(NewAgentPage::Form) => {
                    activate_new_agent_field(app, agents, resources)
                }
                Mode::NewAgent(NewAgentPage::Workspaces) => app.confirm_workspace(),
                Mode::NewAgent(NewAgentPage::Agents) => app.confirm_agent(),
                Mode::NewAgent(NewAgentPage::NativeBrowser) => {
                    activate_native_browser(app, resources.browser)
                }
                Mode::ConfirmClose => close_selected(app, agents)?,
                Mode::ConfirmArchive => archive_selected(app, agents)?,
                Mode::ConfirmResume => resume_selected_archive(app, agents)?,
                Mode::ConfirmQuit => app.request_stop(),
                _ => {}
            }
            Ok((false, true))
        }
        ui::ClickAction::Cancel => {
            match app.mode() {
                Mode::NewAgent(NewAgentPage::Form) => app.cancel_new_agent(),
                Mode::NewAgent(NewAgentPage::Workspaces | NewAgentPage::Agents) => {
                    app.back_to_new_agent_form()
                }
                Mode::NewAgent(NewAgentPage::NativeBrowser) => app.close_native_browser(),
                Mode::ConfirmArchive | Mode::ConfirmResume => app.cancel_confirmation(),
                Mode::ArchiveUnavailable => app.set_mode(Mode::Terminal),
                Mode::ConfirmClose | Mode::ConfirmQuit => app.set_mode(Mode::Terminal),
                Mode::Keybinds | Mode::Settings => app.set_mode(Mode::Menu),
                _ => {}
            }
            Ok((false, true))
        }
        ui::ClickAction::NewAgentField(field) => {
            app.select_new_agent_field(field);
            activate_new_agent_field(app, agents, resources);
            Ok((false, true))
        }
        ui::ClickAction::Workspace(index) => {
            app.select_workspace(index);
            app.confirm_workspace();
            Ok((false, true))
        }
        ui::ClickAction::BrowseWorkspaces => {
            resources
                .browser
                .open(app, resources.settings, resources.host_area);
            Ok((false, true))
        }
        ui::ClickAction::AgentKind(index) => {
            app.select_agent_kind(index);
            app.confirm_agent();
            Ok((false, true))
        }
        ui::ClickAction::NativeBrowserItem(index) => {
            app.set_native_browser_position(index);
            activate_native_browser(app, resources.browser);
            Ok((false, true))
        }
        ui::ClickAction::NativeBrowserParent => {
            open_native_browser_parent(app, resources.browser);
            Ok((false, true))
        }
        ui::ClickAction::ThemePrevious => {
            save_theme(app, resources.settings_store, resources.settings, -1);
            Ok((false, true))
        }
        ui::ClickAction::ThemeNext => {
            save_theme(app, resources.settings_store, resources.settings, 1);
            Ok((false, true))
        }
        ui::ClickAction::EmbeddedAccept | ui::ClickAction::EmbeddedCancel => {
            let code = if action == ui::ClickAction::EmbeddedAccept {
                'q'
            } else {
                'Q'
            };
            if let Err(error) = resources.browser.send_key(KeyEvent::new(
                KeyCode::Char(code),
                crossterm::event::KeyModifiers::NONE,
            )) {
                app.set_notice(format!("could not send input to Yazi: {error}"));
            }
            Ok((false, true))
        }
        ui::ClickAction::EmbeddedForceClose => {
            resources.browser.force_close(app);
            Ok((false, true))
        }
    }
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

fn archive_selected(app: &mut App, agents: &mut RemoteAgents) -> Result<()> {
    let Some(id) = app.selected_agent_id() else {
        app.cancel_confirmation();
        return Ok(());
    };
    agents.archive(id)?;
    app.set_mode(Mode::Terminal);
    Ok(())
}

fn resume_selected_archive(app: &mut App, agents: &mut RemoteAgents) -> Result<()> {
    let Some(id) = app.pending_resume().map(str::to_owned) else {
        app.cancel_confirmation();
        return Ok(());
    };
    agents.resume_archived(id)?;
    app.cancel_confirmation();
    Ok(())
}

fn submit_new_agent(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
) {
    let Some((kind, launch_directory)) = app.new_agent_submission() else {
        return;
    };
    match agents.spawn(kind, launch_directory.clone(), resources.events) {
        Ok(()) => {
            resources
                .settings
                .record_successful_launch(launch_directory, kind);
            match resources.settings_store.save(resources.settings) {
                Ok(()) => app.clear_notice(),
                Err(error) => app.set_notice(error),
            }
            app.finish_new_agent();
        }
        Err(error) => app.set_notice(error.to_string()),
    }
}

fn activate_new_agent_field(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
) {
    match app.new_agent().map(|state| state.draft.selected_field) {
        Some(NewAgentField::Workspace) => app.open_workspace_choices(),
        Some(NewAgentField::Agent) => app.open_agent_choices(),
        Some(NewAgentField::Start) => submit_new_agent(app, agents, resources),
        None => {}
    }
}

fn handle_new_agent_key(
    app: &mut App,
    agents: &mut RemoteAgents,
    resources: &mut InteractionResources<'_>,
    page: NewAgentPage,
    key: KeyEvent,
) {
    match page {
        NewAgentPage::Form => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.move_new_agent_selection(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_new_agent_selection(-1),
            KeyCode::Enter | KeyCode::Char(' ') => activate_new_agent_field(app, agents, resources),
            KeyCode::Esc => app.cancel_new_agent(),
            _ => {}
        },
        NewAgentPage::Workspaces => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.move_new_agent_selection(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_new_agent_selection(-1),
            KeyCode::Enter => app.confirm_workspace(),
            KeyCode::Char('b') => {
                resources
                    .browser
                    .open(app, resources.settings, resources.host_area)
            }
            KeyCode::Esc => app.back_to_new_agent_form(),
            _ => {}
        },
        NewAgentPage::Agents => match key.code {
            KeyCode::Char('j') | KeyCode::Down => app.move_new_agent_selection(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_new_agent_selection(-1),
            KeyCode::Char('c') => app.set_agent_choice(AgentKind::Codex),
            KeyCode::Char('a') => app.set_agent_choice(AgentKind::Claude),
            KeyCode::Enter => app.confirm_agent(),
            KeyCode::Esc => app.back_to_new_agent_form(),
            _ => {}
        },
        NewAgentPage::NativeBrowser => handle_native_browser_key(app, resources.browser, key),
        NewAgentPage::EmbeddedBrowser => {}
    }
}

struct InteractionResources<'a> {
    settings_store: &'a SettingsStore,
    settings: &'a mut Settings,
    events: &'a mpsc::Receiver<ClientEvent>,
    browser: &'a mut BrowserRuntime,
    host_area: Rect,
}

fn handle_native_browser_key(app: &mut App, browser: &mut BrowserRuntime, key: KeyEvent) {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => app.move_new_agent_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_new_agent_selection(-1),
        KeyCode::Home => app.set_native_browser_position(0),
        KeyCode::End => {
            let end = app.native_browser().map_or(0, |state| state.entries.len());
            app.set_native_browser_position(end);
        }
        KeyCode::PageUp => app.move_new_agent_selection(-8),
        KeyCode::PageDown => app.move_new_agent_selection(8),
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
            open_native_browser_parent(app, browser)
        }
        KeyCode::Enter | KeyCode::Char('l') => activate_native_browser(app, browser),
        KeyCode::Esc => app.close_native_browser(),
        _ => {}
    }
}

fn open_native_browser_parent(app: &mut App, browser: &mut BrowserRuntime) {
    if let Some(parent) = app
        .native_browser()
        .and_then(|state| state.current_path.parent())
        .map(PathBuf::from)
    {
        browser.load(app, parent);
    }
}

fn activate_native_browser(app: &mut App, browser: &mut BrowserRuntime) {
    match app.native_browser_action() {
        Some(BrowserAction::Select(path)) => match path.canonicalize() {
            Ok(path) if path.is_dir() => app.choose_browsed_workspace(path),
            Ok(_) => app.set_notice("selected workspace is not a directory"),
            Err(error) => app.set_notice(format!("could not select workspace: {error}")),
        },
        Some(BrowserAction::Load(path)) => browser.load(app, path),
        None => {}
    }
}

struct BrowserRuntime {
    loader: DirectoryLoader,
    generation: u64,
    invocation_directory: Option<PathBuf>,
    runtime_directory: PathBuf,
    palette: Option<TerminalPalette>,
    yazi: Option<YaziPicker>,
    embedded_pending: std::sync::Arc<AtomicBool>,
    embedded_notify: std::sync::Arc<dyn Fn() + Send + Sync>,
}

impl BrowserRuntime {
    fn open(&mut self, app: &mut App, settings: &Settings, area: Rect) {
        let start = app
            .new_agent()
            .and_then(|state| state.draft.workspace.clone())
            .filter(|path| path.is_dir())
            .or_else(|| {
                settings
                    .workspaces
                    .iter()
                    .find(|path| path.is_dir())
                    .cloned()
            })
            .or_else(|| {
                self.invocation_directory
                    .clone()
                    .filter(|path| path.is_dir())
            })
            .or_else(|| {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|path| path.is_dir())
            })
            .unwrap_or_else(|| PathBuf::from("/"));
        let content = ui::embedded_terminal_area(area);
        let size = svarm_agent::PtySize {
            rows: content.height.max(1),
            cols: content.width.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        match YaziPicker::spawn(
            &start,
            &self.runtime_directory,
            size,
            self.palette,
            self.embedded_notify.clone(),
        ) {
            Ok(yazi) => {
                self.yazi = Some(yazi);
                app.open_embedded_browser();
            }
            Err(YaziLaunchError::NotFound) => self.open_native(app, start),
            Err(YaziLaunchError::Failed(error)) => app.set_notice(error),
        }
    }

    fn open_native(&mut self, app: &mut App, start: PathBuf) {
        self.generation = self.generation.saturating_add(1);
        app.open_native_browser(start.clone(), self.generation);
        if let Err(error) = self.loader.load(self.generation, start) {
            app.set_notice(error);
        }
    }

    fn load(&mut self, app: &mut App, path: PathBuf) {
        self.generation = self.generation.saturating_add(1);
        app.begin_directory_load(path.clone(), self.generation);
        if let Err(error) = self.loader.load(self.generation, path) {
            app.set_notice(error);
        }
    }

    fn snapshot(&self) -> Option<TerminalProcessSnapshot> {
        self.yazi.as_ref().map(YaziPicker::snapshot)
    }

    fn send_key(&self, key: KeyEvent) -> std::result::Result<(), String> {
        let Some(yazi) = &self.yazi else {
            return Ok(());
        };
        let snapshot = yazi.snapshot();
        if let Some(input) = key_input(key)
            && let Some(bytes) = encode_key(&input, snapshot.modes)
        {
            yazi.send(&bytes)?;
        }
        Ok(())
    }

    fn send_literal_prefix(&self) -> std::result::Result<(), String> {
        if let Some(yazi) = &self.yazi {
            yazi.send(&[0x02])?;
        }
        Ok(())
    }

    fn paste(&self, text: &str) -> std::result::Result<(), String> {
        if let Some(yazi) = &self.yazi {
            let bytes = encode_paste(text, yazi.snapshot().modes);
            yazi.send(&bytes)?;
        }
        Ok(())
    }

    fn mouse(&self, mouse: MouseEvent, area: Rect) -> std::result::Result<(), String> {
        let Some(yazi) = &self.yazi else {
            return Ok(());
        };
        let content = ui::embedded_terminal_area(area);
        if !contains(content, mouse.column, mouse.row) {
            return Ok(());
        }
        let translated = MouseEvent {
            column: mouse.column - content.x,
            row: mouse.row - content.y,
            ..mouse
        };
        if let Some(bytes) = encode_mouse(&mouse_input(translated), yazi.snapshot().modes) {
            yazi.send(&bytes)?;
        }
        Ok(())
    }

    fn resize(&self, area: Rect) -> std::result::Result<(), String> {
        if let Some(yazi) = &self.yazi {
            let content = ui::embedded_terminal_area(area);
            yazi.resize(content.height.max(1), content.width.max(1))?;
        }
        Ok(())
    }

    fn poll_yazi(&mut self, app: &mut App) {
        let Some(yazi) = &mut self.yazi else {
            return;
        };
        let snapshot = yazi.snapshot();
        let finished = if snapshot.read_error.is_some() {
            let _ = yazi.stop();
            true
        } else {
            match yazi.poll() {
                Ok(SessionStatus::Running) => {
                    if snapshot.output_closed {
                        let notify = self.embedded_notify.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            notify();
                        });
                    }
                    false
                }
                Ok(SessionStatus::Exited) => true,
                Err(error) => {
                    app.set_notice(format!("could not poll Yazi: {error}"));
                    true
                }
            }
        };
        if !finished {
            return;
        }
        let yazi = self.yazi.take().expect("Yazi was present while polled");
        match yazi.finish() {
            YaziResult::Selected(path) => app.choose_browsed_workspace(path),
            YaziResult::Cancelled => app.close_embedded_browser(),
            YaziResult::Failed(error) => {
                app.set_notice(error);
                app.close_embedded_browser();
            }
        }
    }

    fn force_close(&mut self, app: &mut App) {
        if let Some(yazi) = &mut self.yazi
            && let Err(error) = yazi.stop()
        {
            app.set_notice(format!("could not close Yazi: {error}"));
        }
        self.yazi = None;
        app.close_embedded_browser();
    }
}

fn workspace_choices(settings: &Settings, extra: Option<&PathBuf>) -> Vec<WorkspaceChoice> {
    let mut choices = settings
        .workspaces
        .iter()
        .map(|path| WorkspaceChoice {
            path: path.clone(),
            available: path.is_dir(),
        })
        .collect::<Vec<_>>();
    if let Some(path) = extra
        && !choices.iter().any(|choice| choice.path == *path)
    {
        choices.insert(
            0,
            WorkspaceChoice {
                path: path.clone(),
                available: path.is_dir(),
            },
        );
    }
    choices
}

fn remembered_workspace(settings: &Settings) -> Option<PathBuf> {
    settings
        .workspaces
        .first()
        .filter(|path| path.is_dir())
        .cloned()
}

fn sync_selection(app: &App, agents: &mut RemoteAgents) -> Result<()> {
    if let Some(id) = app.selected_agent_id() {
        agents.select(id)?;
    }
    Ok(())
}

fn save_theme(
    app: &mut App,
    settings_store: &SettingsStore,
    settings: &mut Settings,
    delta: isize,
) {
    let theme = app.cycle_theme(delta);
    settings.theme = theme;
    match settings_store.save(settings) {
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

const fn wheel_steps(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(-1),
        MouseEventKind::ScrollDown => Some(1),
        _ => None,
    }
}

fn alternate_scroll_input(steps: isize) -> AgentKeyInput {
    AgentKeyInput {
        code: if steps < 0 {
            AgentKeyCode::Up
        } else {
            AgentKeyCode::Down
        },
        modifiers: InputModifiers::default(),
    }
}

fn handle_sidebar_wheel(app: &mut App, mouse: MouseEvent, area: Rect) -> bool {
    let Some(steps) = wheel_steps(mouse.kind) else {
        return false;
    };
    if !app.sidebar_visible() || !contains(ui::sidebar_area(area), mouse.column, mouse.row) {
        return false;
    }
    app.scroll_sidebar(steps, ui::agent_list_page_size(app, area));
    true
}

fn connection_failure_message(reason: &str, session_id: u64) -> String {
    let reason = reason.replace(['\r', '\n'], " ");
    let mut characters = reason.chars();
    let mut first_line = characters.by_ref().take(79).collect::<String>();
    if characters.next().is_some() {
        first_line.push('…');
    }
    format!(
        "{first_line}\nAgents may still be running.\nReattach: svarm --attach --session {session_id}"
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{KeyModifiers, MouseEventKind};
    use svarm_agent::{AgentId, AgentKind, SessionSnapshot};

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
    fn touchpad_and_wheel_events_map_to_sidebar_and_alternate_screen_scrolling() {
        assert_eq!(wheel_steps(MouseEventKind::ScrollUp), Some(-1));
        assert_eq!(wheel_steps(MouseEventKind::ScrollDown), Some(1));
        assert_eq!(wheel_steps(MouseEventKind::Moved), None);
        assert_eq!(alternate_scroll_input(-1).code, AgentKeyCode::Up);
        assert_eq!(alternate_scroll_input(1).code, AgentKeyCode::Down);
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        for id in 1..=8 {
            app.add_agent(SessionSnapshot {
                id: AgentId::new(id),
                kind: AgentKind::Codex,
                launch_directory: PathBuf::from("/tmp/workspace"),
                status: SessionStatus::Running,
                output_generation: 0,
                read_error: None,
                exit: None,
                conversation_id: None,
            });
        }
        let area = Rect::new(0, 0, 80, 24);
        assert!(handle_sidebar_wheel(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 5,
                modifiers: KeyModifiers::NONE,
            },
            area,
        ));
        assert_eq!(app.sidebar_scroll(), Some(3));
    }

    #[test]
    fn connection_notices_fit_at_80_columns_and_include_reattach_command() {
        let message = connection_failure_message(&"connection lost ".repeat(20), 42);
        assert!(message.lines().all(|line| line.chars().count() <= 80));
        assert!(message.contains("Reattach: svarm --attach --session 42"));

        let revoked = connection_failure_message(
            "another client explicitly took over this Svarm session",
            42,
        );
        assert!(revoked.lines().all(|line| line.chars().count() <= 80));
        assert!(revoked.starts_with("another client explicitly took over"));
    }

    #[test]
    fn a_missing_most_recent_workspace_does_not_silently_select_an_older_one() {
        let settings = Settings {
            workspaces: vec![
                PathBuf::from("/svarm-test-definitely-missing"),
                std::env::current_dir().unwrap(),
            ],
            ..Settings::default()
        };

        assert_eq!(remembered_workspace(&settings), None);
    }
}
