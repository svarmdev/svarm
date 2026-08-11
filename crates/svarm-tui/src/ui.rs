use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use svarm_agent::vt100::Screen;

use crate::{
    app::{App, MenuItem, Mode, NewAgentField, NewAgentPage, SessionChooser},
    input::MANAGEMENT_KEYBINDINGS,
    screen::TerminalScreen,
    theme::Theme,
};
use svarm_agent::{AgentKind, SessionStatus};

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
pub const SIDEBAR_WIDTH: u16 = 25;
const MENU_HEIGHT: u16 = MenuItem::ALL.len() as u16 + 2;
const MENU_WIDTH: u16 = 46;

#[derive(Clone, Copy)]
pub(crate) struct UiModel<'a> {
    pub app: &'a App,
    pub screen: Option<&'a Screen>,
    pub theme: Theme,
}

pub(crate) fn render(frame: &mut Frame<'_>, model: UiModel<'_>) {
    let app = model.app;
    let theme = model.theme;
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.page()), area);

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Svarm needs at least {MIN_WIDTH}x{MIN_HEIGHT}\ncurrent terminal: {}x{}",
                area.width, area.height
            ))
            .centered()
            .style(text(theme)),
            area,
        );
        return;
    }

    if app.sidebar_visible() {
        render_sidebar(frame, app, sidebar_area(area), theme);
    }
    render_terminal(
        frame,
        model.screen,
        app.mode(),
        terminal_area(area, app.sidebar_visible()),
        theme,
    );

    match app.mode() {
        Mode::NewAgent(NewAgentPage::Form) => render_new_agent_form(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Workspaces) => render_workspace_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Agents) => render_agent_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::NativeBrowser) => render_native_browser(frame, app, theme),
        Mode::ConfirmClose => {
            render_confirmation(frame, theme, "Close agent?", "Close this agent?")
        }
        Mode::ConfirmQuit => render_stop_confirmation(frame, app, theme),
        Mode::Keybinds => render_keybinds(frame, theme),
        Mode::Settings => render_settings(frame, app, theme),
        _ => {}
    }
}

pub(crate) fn render_session_chooser(
    frame: &mut Frame<'_>,
    chooser: &SessionChooser,
    now_ms: u64,
    theme: Theme,
) {
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.page()), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Svarm needs at least {MIN_WIDTH}x{MIN_HEIGHT}\ncurrent terminal: {}x{}",
                area.width, area.height
            ))
            .centered()
            .style(text(theme)),
            area,
        );
        return;
    }

    let block = Block::bordered()
        .title(Span::styled(
            " Open Svarm session ",
            accent(theme).add_modifier(Modifier::BOLD),
        ))
        .border_style(border(theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let footer_height = 2;
    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_height),
    );
    let visible_rows = usize::from(list_area.height);
    let start = chooser.viewport_start(visible_rows);
    let end = (start + visible_rows).min(chooser.row_count());
    let rows = (start..end).map(|index| {
        let selected = index == chooser.selected();
        let line = if let Some(session) = chooser.sessions().get(index) {
            session_row(session, now_ms, list_area.width.saturating_sub(3), theme)
        } else {
            Line::from(vec![
                Span::styled("+ ", success(theme)),
                Span::styled(
                    "Start new session",
                    text(theme).add_modifier(Modifier::BOLD),
                ),
            ])
        };
        let marker = Span::styled(if selected { "▌ " } else { "  " }, accent(theme));
        let mut spans = vec![marker];
        spans.extend(line.spans);
        ListItem::new(Line::from(spans)).style(if selected {
            theme.selected()
        } else {
            text(theme)
        })
    });
    frame.render_widget(List::new(rows), list_area);

    let mut footer = "[Enter] open  [j/k] select  [Esc] cancel".to_owned();
    if chooser.allow_new() {
        footer.push_str("  [n] new");
    }
    frame.render_widget(
        Paragraph::new(footer).style(theme.muted()),
        Rect::new(
            inner.x + 1,
            inner.bottom().saturating_sub(1),
            inner.width - 1,
            1,
        ),
    );
}

fn session_row(
    session: &svarm_agent::protocol::SessionSummary,
    now_ms: u64,
    _width: u16,
    theme: Theme,
) -> Line<'static> {
    let state = if session.attachment.is_some() {
        "attached"
    } else {
        "detached"
    };
    let age = format_age(now_ms.saturating_sub(session.last_user_activity_ms));
    Line::from(vec![
        Span::styled(format!("{}  ", session.id.0), accent(theme)),
        Span::styled(
            format!(
                "{}  {}/{} running  {}  ",
                state, session.running_agents, session.total_agents, age
            ),
            if session.attachment.is_some() {
                warning(theme)
            } else {
                theme.muted()
            },
        ),
    ])
}

fn format_age(age_ms: u64) -> String {
    let seconds = age_ms / 1_000;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

pub fn terminal_area(area: Rect, sidebar_visible: bool) -> Rect {
    let sidebar_width = if sidebar_visible {
        SIDEBAR_WIDTH.min(area.width.saturating_sub(1))
    } else {
        0
    };
    Rect::new(
        area.x.saturating_add(sidebar_width),
        area.y,
        area.width.saturating_sub(sidebar_width),
        area.height,
    )
}

pub fn sidebar_area(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        SIDEBAR_WIDTH.min(area.width.saturating_sub(1)),
        area.height,
    )
}

pub fn menu_button_area(area: Rect, sidebar_visible: bool) -> Option<Rect> {
    if !sidebar_visible || area.height == 0 {
        return None;
    }
    let sidebar = sidebar_area(area);
    Some(Rect::new(
        sidebar.x,
        sidebar.bottom().saturating_sub(1),
        sidebar.width.saturating_sub(1),
        1,
    ))
}

pub fn menu_item_at(area: Rect, column: u16, row: u16) -> Option<MenuItem> {
    let button = menu_button_area(area, true)?;
    let popup = menu_popup_area(button);
    if column <= popup.x || column >= popup.right().saturating_sub(1) {
        return None;
    }
    let index = usize::from(row.checked_sub(popup.y.saturating_add(1))?);
    MenuItem::ALL.get(index).copied()
}

fn menu_popup_area(button: Rect) -> Rect {
    Rect::new(
        button.x,
        button.y.saturating_sub(MENU_HEIGHT),
        MENU_WIDTH.max(button.width),
        MENU_HEIGHT,
    )
}

fn render_sidebar(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let title = Span::styled(" svarm ", accent(theme).add_modifier(Modifier::BOLD));
    let block = Block::new()
        .title(title)
        .borders(Borders::TOP | Borders::RIGHT)
        .border_style(border(theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let button = menu_button_area(area, true).expect("visible sidebar has a menu button");
    let popup_height = if app.mode() == Mode::Menu {
        MENU_HEIGHT
    } else {
        0
    };
    let notice_height = u16::from(app.notice().is_some());
    let agents_height = inner
        .height
        .saturating_sub(1 + popup_height + notice_height);
    let agents_area = Rect::new(inner.x, inner.y, inner.width, agents_height);

    let rows = app
        .agents()
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let selected = index == app.selected_index();
            let status = agent.status();
            let marker = match (status, agent.has_unseen_output()) {
                (SessionStatus::Exited, _) => "×",
                (_, true) => "!",
                _ => "●",
            };
            let status_style = match (status, agent.has_unseen_output()) {
                (SessionStatus::Exited, _) => theme.muted(),
                (_, true) => warning(theme),
                _ => success(theme),
            };
            let fixed_width = 3
                + (index + 1).to_string().chars().count()
                + 1
                + agent.kind().label().chars().count()
                + 3
                + 1
                + marker.chars().count();
            let workspace = end_truncate(
                &agent.workspace_name(),
                usize::from(agents_area.width).saturating_sub(fixed_width),
            );
            let mut line = Line::from(vec![
                Span::styled(if selected { " ▌ " } else { "   " }, accent(theme)),
                Span::styled(format!("{} ", index + 1), theme.muted()),
                Span::styled(agent.kind().label(), text(theme)),
                Span::styled(" · ", theme.muted()),
                Span::styled(workspace, theme.muted()),
                Span::raw(" "),
                Span::styled(marker, status_style),
            ]);
            if selected {
                line = line.style(theme.selected());
            }
            ListItem::new(line)
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(rows), agents_area);

    if let Some(notice) = app.notice() {
        let y = agents_area.bottom();
        frame.render_widget(
            Paragraph::new(format!(" ! {notice}")).style(warning(theme)),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    if app.mode() == Mode::Menu {
        render_menu(frame, app, menu_popup_area(button), theme);
    }

    let button_style = if app.mode() == Mode::Menu {
        theme.selected()
    } else {
        theme.surface()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ≡ Menu", button_style.add_modifier(Modifier::BOLD)),
            Span::styled("   ^B m", button_style),
        ]))
        .style(button_style),
        button,
    );
}

fn end_truncate(value: &str, width: usize) -> String {
    let mut characters = value.chars();
    let mut truncated = characters.by_ref().take(width).collect::<String>();
    if characters.next().is_some() && width > 0 {
        truncated.pop();
        truncated.push('…');
    }
    truncated
}

fn render_menu(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(" Menu ")
        .border_style(accent(theme))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows = MenuItem::ALL.into_iter().map(|item| {
        let marker = if item == app.menu_selected() {
            " ▌ "
        } else {
            "   "
        };
        let style = if item == app.menu_selected() {
            theme.selected()
        } else {
            text(theme)
        };
        ListItem::new(Line::from(vec![
            Span::styled(marker, accent(theme)),
            Span::styled(item.label(), style),
        ]))
        .style(style)
    });
    frame.render_widget(List::new(rows), inner);
}

fn render_terminal(
    frame: &mut Frame<'_>,
    screen: Option<&Screen>,
    mode: Mode,
    area: Rect,
    theme: Theme,
) {
    let Some(screen) = screen else {
        frame.render_widget(
            Paragraph::new("No agents open. Press Ctrl+B, then n to start one.")
                .centered()
                .style(theme.muted()),
            area,
        );
        return;
    };
    // The cursor is the host terminal's own, placed below, so that it keeps the shape, color and
    // blink the user configured. Painting one into the buffer can only produce a static block.
    let pane = TerminalScreen::new(screen);
    if mode == Mode::Terminal
        && let Some(position) = pane.cursor_position(area)
    {
        frame.set_cursor_position(position);
    }
    frame.render_widget(pane, area);
}

fn render_new_agent_form(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let workspace = state.draft.workspace.as_ref().map_or_else(
        || "<choose workspace>".into(),
        |path| path.display().to_string(),
    );
    let agent = state.draft.agent.map_or("<choose agent>", AgentKind::label);
    let complete = state.draft.workspace.is_some() && state.draft.agent.is_some();
    let row = |field, label: &str, value: String| {
        let selected = state.draft.selected_field == field;
        Line::from(vec![
            Span::styled(if selected { " > " } else { "   " }, accent(theme)),
            Span::styled(format!("{label:<12}"), text(theme)),
            Span::styled(
                end_truncate(&value, 50),
                if selected {
                    theme.selected()
                } else {
                    theme.muted()
                },
            ),
        ])
    };
    render_dialog(
        frame,
        theme,
        " New agent ",
        72,
        9,
        vec![
            Line::from(""),
            row(NewAgentField::Workspace, "Workspace", workspace),
            row(NewAgentField::Agent, "Agent", agent.into()),
            Line::from(vec![
                Span::styled(
                    if state.draft.selected_field == NewAgentField::Start {
                        " > "
                    } else {
                        "   "
                    },
                    accent(theme),
                ),
                Span::styled(
                    if complete {
                        "Start agent"
                    } else {
                        "Start agent (disabled)"
                    },
                    if complete { text(theme) } else { theme.muted() },
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  [j/k] move  [Enter] select  [w] workspace  [a] agent  [Esc] cancel",
                theme.muted(),
            )),
        ],
    );
}

fn render_workspace_choices(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let visible = 8;
    let start = state.selected_workspace.saturating_sub(visible - 1);
    let mut lines = vec![Line::from("")];
    if state.workspaces.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No saved workspaces. Press b to browse.",
            theme.muted(),
        )));
    } else {
        lines.extend(
            state
                .workspaces
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(index, choice)| {
                    let name = choice
                        .path
                        .file_name()
                        .unwrap_or(choice.path.as_os_str())
                        .to_string_lossy();
                    let missing = if choice.available { "" } else { "  missing" };
                    Line::from(vec![
                        Span::styled(
                            if index == state.selected_workspace {
                                " > "
                            } else {
                                "   "
                            },
                            accent(theme),
                        ),
                        Span::styled(format!("{name:<16}"), text(theme)),
                        Span::styled(
                            end_truncate(&choice.path.display().to_string(), 45),
                            theme.muted(),
                        ),
                        Span::styled(missing, warning(theme)),
                    ])
                }),
        );
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "  [Enter] use  [b] browse  [j/k] move  [Esc] back",
            theme.muted(),
        )),
    ]);
    render_dialog(frame, theme, " Choose workspace ", 76, 13, lines);
}

fn render_agent_choices(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let mut lines = vec![Line::from("")];
    lines.extend(AgentKind::ALL.iter().enumerate().map(|(index, kind)| {
        Line::from(vec![
            Span::styled(
                if index == state.selected_agent {
                    " > "
                } else {
                    "   "
                },
                accent(theme),
            ),
            Span::styled(kind.label(), text(theme)),
        ])
    }));
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "  [j/k] move  [Enter] use  [c/a] choose  [Esc] back",
            theme.muted(),
        )),
    ]);
    render_dialog(frame, theme, " Choose agent ", 44, 8, lines);
}

fn render_native_browser(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(browser) = app.native_browser() else {
        return;
    };
    let visible = 8;
    let start = browser.selected.saturating_sub(visible - 1);
    let rows = std::iter::once((0, "Use this directory".into()))
        .chain(
            browser
                .entries
                .iter()
                .enumerate()
                .map(|(index, choice)| (index + 1, format!("{}/", choice.label))),
        )
        .skip(start)
        .take(visible)
        .map(|(index, label)| {
            Line::from(vec![
                Span::styled(
                    if index == browser.selected {
                        " > "
                    } else {
                        "   "
                    },
                    accent(theme),
                ),
                Span::styled(end_truncate(&label, 66), text(theme)),
            ])
        });
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "  {}",
                end_truncate(&browser.current_path.display().to_string(), 68)
            ),
            theme.muted(),
        )),
        Line::from(""),
    ];
    lines.extend(rows);
    if browser.loading {
        lines.push(Line::from(Span::styled("  Loading…", theme.muted())));
    } else if let Some(error) = &browser.error {
        lines.push(Line::from(Span::styled(
            format!("  {}", end_truncate(error, 68)),
            warning(theme),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "  [Enter/l] open/use  [h] parent  [j/k] move  [Esc] cancel",
        theme.muted(),
    )));
    render_dialog(frame, theme, " Select workspace ", 76, 16, lines);
}

fn render_confirmation(frame: &mut Frame<'_>, theme: Theme, title: &str, prompt: &str) {
    render_dialog(
        frame,
        theme,
        title,
        46,
        6,
        vec![
            Line::from(""),
            Line::from(Span::styled(format!("  {prompt}"), warning(theme))),
            Line::from(""),
            Line::from("  [y] Yes    [Esc] Cancel"),
        ],
    );
}

fn render_stop_confirmation(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let session = app
        .session_id()
        .map_or_else(|| "local".into(), |id| id.0.to_string());
    let running = app
        .agents()
        .iter()
        .filter(|agent| agent.status() == SessionStatus::Running)
        .count();
    render_dialog(
        frame,
        theme,
        " Stop Svarm session? ",
        64,
        8,
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Session {session} · {running} running agents"),
                warning(theme),
            )),
            Line::from("  This terminates every agent in the session."),
            Line::from(""),
            Line::from("  [y] Stop session    [Esc] Cancel"),
        ],
    );
}

fn render_keybinds(frame: &mut Frame<'_>, theme: Theme) {
    let mut lines = vec![Line::from("")];
    lines.extend(
        MANAGEMENT_KEYBINDINGS
            .iter()
            .map(|binding| Line::from(format!("  {:<27} {}", binding.keys, binding.action))),
    );
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "  Otherwise, every key and supported mouse event goes",
            theme.muted(),
        )),
        Line::from(Span::styled("  to the native agent TUI.", theme.muted())),
        Line::from(""),
        Line::from(Span::styled("  Esc closes", theme.muted())),
    ]);
    render_dialog(
        frame,
        theme,
        " Keybinds ",
        76,
        MANAGEMENT_KEYBINDINGS.len() as u16 + 8,
        lines,
    );
}

fn render_settings(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    render_dialog(
        frame,
        theme,
        " Settings ",
        54,
        8,
        vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Theme", text(theme).add_modifier(Modifier::BOLD)),
                Span::styled("              ‹  ", theme.muted()),
                Span::styled(
                    app.theme().label(),
                    accent(theme).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ›", theme.muted()),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Left/right changes and saves the theme.",
                theme.muted(),
            )),
            Line::from(""),
            Line::from(Span::styled("  Esc returns to menu", theme.muted())),
        ],
    );
}

fn render_dialog(
    frame: &mut Frame<'_>,
    theme: Theme,
    title: &str,
    width: u16,
    height: u16,
    lines: Vec<Line<'static>>,
) {
    let area = centered_rect(width, height, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::bordered()
                    .title(Span::styled(
                        title.to_owned(),
                        accent(theme).add_modifier(Modifier::BOLD),
                    ))
                    .border_style(border(theme)),
            )
            .wrap(Wrap { trim: false })
            .style(theme.surface()),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn text(theme: Theme) -> Style {
    Style::default().fg(theme.text)
}

fn accent(theme: Theme) -> Style {
    Style::default().fg(theme.accent)
}

fn success(theme: Theme) -> Style {
    Style::default().fg(theme.ok)
}

fn warning(theme: Theme) -> Style {
    Style::default().fg(theme.warn)
}

fn border(theme: Theme) -> Style {
    Style::default().fg(theme.border)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::{Terminal, backend::TestBackend, layout::Position};
    use svarm_agent::protocol::{
        AgentSnapshot, AttachmentSummary, ConnectionId, SessionId, SessionRevision, SessionSummary,
        SvarmSessionSnapshot, TerminalSequence,
    };

    use super::*;

    #[test]
    fn terminal_area_only_reserves_the_sidebar() {
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), true),
            Rect::new(SIDEBAR_WIDTH, 0, 120 - SIDEBAR_WIDTH, 40)
        );
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), false),
            Rect::new(0, 0, 120, 40)
        );
    }

    #[test]
    fn menu_hit_areas_stay_at_the_bottom_of_the_sidebar() {
        let area = Rect::new(0, 0, 120, 40);
        assert_eq!(menu_button_area(area, true), Some(Rect::new(0, 39, 24, 1)));
        assert_eq!(menu_item_at(area, 2, 36), Some(MenuItem::Keybinds));
        assert_eq!(menu_item_at(area, 2, 37), Some(MenuItem::Settings));
        assert_eq!(menu_item_at(area, 50, 36), None);
    }

    #[test]
    fn centered_rect_clamps_to_the_terminal() {
        assert_eq!(
            centered_rect(100, 100, Rect::new(2, 3, 20, 10)),
            Rect::new(2, 3, 20, 10)
        );
    }

    #[test]
    fn prepared_ui_model_renders_at_supported_sizes() {
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );

        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        UiModel {
                            app: &app,
                            screen: None,
                            theme: app.theme().theme(true),
                        },
                    );
                })
                .unwrap();
        }
    }

    #[test]
    fn native_browser_renders_loading_listing_and_error_states_at_supported_sizes() {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        app.open_new_agent(None, None, Vec::new());
        app.open_native_browser(PathBuf::from("/tmp/a very long workspace path"), 1);
        app.apply_directory_load(
            1,
            PathBuf::from("/tmp/a very long workspace path"),
            Ok(vec![crate::app::DirectoryChoice {
                path: PathBuf::from("/tmp/a very long workspace path/child"),
                label: "child".into(),
            }]),
        );

        for (width, height) in [(80, 24), (120, 40), (200, 60)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        UiModel {
                            app: &app,
                            screen: None,
                            theme: app.theme().theme(true),
                        },
                    );
                })
                .unwrap();
        }
        assert!(render_app_text(&app).contains("Use this directory"));
    }

    #[test]
    fn session_chooser_preserves_id_state_and_footer_at_80x24() {
        let chooser = SessionChooser::new(
            vec![SessionSummary {
                id: SessionId(42),
                running_agents: 1,
                total_agents: 2,
                attachment: Some(AttachmentSummary {
                    connection_id: ConnectionId(1),
                    process_id: Some(7),
                    attached_at_ms: 1,
                    last_activity_ms: 1,
                }),
                last_user_activity_ms: 1,
                revision: SessionRevision(1),
            }],
            true,
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_session_chooser(
                    frame,
                    &chooser,
                    2_001,
                    crate::theme::ThemeName::Monochrome.theme(false),
                )
            })
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("Open Svarm session"));
        assert!(rendered.contains("42"));
        assert!(rendered.contains("attached"));
        assert!(rendered.contains("[Enter] open  [j/k] select  [Esc] cancel  [n] new"));
    }

    #[test]
    fn keybinds_and_menu_use_canonical_detach_and_stop_copy() {
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            false,
            None,
        );
        app.set_mode(Mode::Keybinds);
        let keybinds = render_app_text(&app);
        assert!(keybinds.contains("detach — agents keep running"));
        assert!(keybinds.contains("stop session — terminates all agents"));

        app.set_mode(Mode::Menu);
        let menu = render_app_text(&app);
        assert!(menu.contains("Detach — agents keep running"));
        assert!(menu.contains("Stop session — terminates all agents"));
    }

    #[test]
    fn stop_confirmation_names_target_and_running_agents_at_80x24() {
        let summary = SessionSummary {
            id: SessionId(7),
            running_agents: 1,
            total_agents: 1,
            attachment: None,
            last_user_activity_ms: 1,
            revision: SessionRevision(1),
        };
        let agent = AgentSnapshot {
            id: svarm_agent::AgentId::new(1),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/project-seven"),
            status: SessionStatus::Running,
            exit: None,
            output_generation: 0,
            seen_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            recognition: None,
        };
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary,
                selected_agent_id: Some(agent.id),
                rows: 24,
                cols: 80,
                agents: vec![agent],
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        app.set_mode(Mode::ConfirmQuit);
        let rendered = render_app_text(&app);
        assert!(rendered.contains("Session 7 · 1 running agents"));
        assert!(!rendered.contains("/tmp/project-seven"));
        assert!(rendered.contains("terminates every agent"));
    }

    #[test]
    fn reattached_exit_and_unseen_states_have_monochrome_symbols() {
        let summary = SessionSummary {
            id: SessionId(8),
            running_agents: 1,
            total_agents: 2,
            attachment: None,
            last_user_activity_ms: 1,
            revision: SessionRevision(1),
        };
        let agent = |id, status, output_generation, seen_generation| AgentSnapshot {
            id: svarm_agent::AgentId::new(id),
            kind: svarm_agent::AgentKind::Codex,
            launch_directory: PathBuf::from("/tmp/project-eight"),
            status,
            exit: None,
            output_generation,
            seen_generation,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            recognition: None,
        };
        let exited = agent(1, SessionStatus::Exited, 1, 1);
        let unseen = agent(2, SessionStatus::Running, 2, 1);
        let app = App::hydrate(
            SvarmSessionSnapshot {
                summary,
                selected_agent_id: Some(unseen.id),
                rows: 24,
                cols: 80,
                agents: vec![exited, unseen],
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );
        let rendered = render_app_text(&app);
        assert!(rendered.contains('×'));
        assert!(rendered.contains('!'));
        assert!(rendered.contains("project"));
    }

    #[test]
    fn the_agent_cursor_is_the_host_terminal_cursor_not_a_painted_cell() {
        let mut parser = svarm_agent::vt100::Parser::new(24, 55, 0);
        parser.process(b"prompt> ");
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.set_mode(Mode::Terminal);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: Some(parser.screen()),
                        theme: app.theme().theme(true),
                    },
                )
            })
            .unwrap();

        // Column 8 of the pane, which starts after the sidebar.
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(SIDEBAR_WIDTH + 8, 0)
        );
        let cursor_cell = &terminal.backend().buffer()[(SIDEBAR_WIDTH + 8, 0)];
        assert!(
            !cursor_cell.modifier.contains(Modifier::REVERSED) && cursor_cell.symbol() != "█",
            "the pane must not paint a cursor over the terminal's own"
        );
    }

    #[test]
    fn a_hidden_agent_cursor_leaves_the_host_cursor_hidden() {
        let mut parser = svarm_agent::vt100::Parser::new(24, 55, 0);
        parser.process(b"working\x1b[?25l");
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.set_mode(Mode::Terminal);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: Some(parser.screen()),
                        theme: app.theme().theme(true),
                    },
                )
            })
            .unwrap();

        assert!(!terminal.backend().cursor_visible());
    }

    fn render_app_text(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app,
                        screen: None,
                        theme: app.theme().theme(false),
                    },
                )
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}
