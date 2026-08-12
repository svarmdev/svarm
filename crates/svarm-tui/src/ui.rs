use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use svarm_agent::terminal_model::TerminalSnapshot;

use crate::{
    app::{AgentDisplayStatus, App, MenuItem, Mode, NewAgentField, NewAgentPage, SessionChooser},
    input::MANAGEMENT_KEYBINDINGS,
    screen::TerminalScreen,
    theme::Theme,
};
use svarm_agent::{AgentKind, SessionStatus, TerminalProcessSnapshot};

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
const COMPACT_MODAL_WIDTH: u16 = 64;
const COMPACT_MODAL_HEIGHT: u16 = 12;
const STANDARD_MODAL_WIDTH: u16 = 72;
const STANDARD_MODAL_HEIGHT: u16 = 18;
pub const SIDEBAR_WIDTH: u16 = 28;
const AGENT_CARD_HEIGHT: u16 = 3;
const MENU_HEIGHT: u16 = MenuItem::ALL.len() as u16 + 2;
const MENU_WIDTH: u16 = 46;

#[derive(Clone, Copy)]
enum ModalSize {
    Compact,
    Standard,
    Large,
}

impl ModalSize {
    fn area(self, terminal: Rect) -> Rect {
        match self {
            Self::Compact => centered_rect(COMPACT_MODAL_WIDTH, COMPACT_MODAL_HEIGHT, terminal),
            Self::Standard => centered_rect(STANDARD_MODAL_WIDTH, STANDARD_MODAL_HEIGHT, terminal),
            Self::Large => centered_rect(
                terminal.width.saturating_sub(4).min(100),
                terminal.height.saturating_sub(2).min(30),
                terminal,
            ),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UiModel<'a> {
    pub app: &'a App,
    pub screen: Option<&'a TerminalSnapshot>,
    pub scrolled: bool,
    pub embedded: Option<&'a TerminalProcessSnapshot>,
    pub theme: Theme,
    pub colors_enabled: bool,
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
        render_sidebar(frame, app, sidebar_area(area), theme, model.colors_enabled);
    }
    render_terminal(
        frame,
        model.screen,
        model.scrolled,
        app.mode(),
        terminal_area(area, app.sidebar_visible()),
        theme,
    );

    match app.mode() {
        Mode::NewAgent(NewAgentPage::Form) => render_new_agent_form(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Workspaces) => render_workspace_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::Agents) => render_agent_choices(frame, app, theme),
        Mode::NewAgent(NewAgentPage::NativeBrowser) => render_native_browser(frame, app, theme),
        Mode::NewAgent(NewAgentPage::EmbeddedBrowser) | Mode::ToolPrefix => {
            render_embedded_browser(frame, model.embedded, theme)
        }
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

pub fn new_agent_button_area(area: Rect, sidebar_visible: bool) -> Option<Rect> {
    let menu = menu_button_area(area, sidebar_visible)?;
    (menu.y > area.y).then_some(Rect::new(menu.x, menu.y - 1, menu.width, 1))
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

pub fn agent_item_at(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    if !app.sidebar_visible() {
        return None;
    }
    let agents = agent_list_area(app, sidebar_area(area));
    if column < agents.x || column >= agents.right() || row < agents.y || row >= agents.bottom() {
        return None;
    }
    let slot = usize::from((row - agents.y) / AGENT_CARD_HEIGHT);
    if slot >= usize::from(agents.height / AGENT_CARD_HEIGHT) {
        return None;
    }
    let index = agent_list_start(app, agents) + slot;
    (index < app.agents().len()).then_some(index)
}

fn menu_popup_area(button: Rect) -> Rect {
    Rect::new(
        button.x,
        button.y.saturating_sub(MENU_HEIGHT),
        MENU_WIDTH.max(button.width),
        MENU_HEIGHT,
    )
}

fn agent_list_area(app: &App, sidebar: Rect) -> Rect {
    let inner = Block::new()
        .borders(Borders::TOP | Borders::RIGHT)
        .inner(sidebar);
    let popup_height = if app.mode() == Mode::Menu {
        MENU_HEIGHT
    } else {
        0
    };
    let reserved = 2 + popup_height + u16::from(app.notice().is_some());
    Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(reserved),
    )
}

fn agent_list_start(app: &App, area: Rect) -> usize {
    let visible = usize::from((area.height / AGENT_CARD_HEIGHT).max(1));
    let max = app.agents().len().saturating_sub(visible);
    app.sidebar_scroll()
        .unwrap_or_else(|| app.selected_index().saturating_sub(visible - 1))
        .min(max)
}

pub fn agent_list_page_size(app: &App, area: Rect) -> usize {
    usize::from(agent_list_area(app, sidebar_area(area)).height / AGENT_CARD_HEIGHT).max(1)
}

fn render_sidebar(
    frame: &mut Frame<'_>,
    app: &App,
    area: Rect,
    theme: Theme,
    colors_enabled: bool,
) {
    let title = Span::styled(" svarm ", accent(theme).add_modifier(Modifier::BOLD));
    let block = Block::new()
        .title(title)
        .borders(Borders::TOP | Borders::RIGHT)
        .border_style(border(theme))
        .style(Style::default().bg(Color::Reset));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let new_button =
        new_agent_button_area(area, true).expect("visible sidebar has a new-agent button");
    let button = menu_button_area(area, true).expect("visible sidebar has a menu button");
    let agents_area = agent_list_area(app, area);

    let cards = app
        .agents()
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let status = agent.display_status();
            let (circle, status_style) = status_display(status, colors_enabled);
            let content_width = usize::from(agents_area.width.saturating_sub(2));
            let number = format!("{} · ", index + 1);
            let title = end_truncate(
                agent.conversation_title().unwrap_or("Unnamed conversation"),
                usize::from(agents_area.width).saturating_sub(3 + number.chars().count()),
            );
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(
                        if index == app.selected_index() {
                            "▌"
                        } else {
                            " "
                        },
                        accent(theme),
                    ),
                    Span::styled(format!("{circle} "), status_style),
                    Span::styled(number, theme.muted()),
                    Span::styled(title, text(theme).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(agent.kind().label(), text(theme)),
                ]),
            ];
            if let Some(git) = agent.git() {
                let worktree = git
                    .worktree
                    .file_name()
                    .unwrap_or(git.worktree.as_os_str())
                    .to_string_lossy();
                let value =
                    paired_truncate(&worktree, &git.branch, content_width.saturating_sub(2));
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(value, accent(theme)),
                ]));
            } else {
                lines.push(Line::default());
            }
            ListItem::new(lines).style(if index == app.selected_index() {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default().with_offset(agent_list_start(app, agents_area));
    frame.render_stateful_widget(List::new(cards), agents_area, &mut state);

    if let Some(notice) = app.notice() {
        let y = agents_area.bottom();
        frame.render_widget(
            Paragraph::new(format!(" ! {notice}")).style(warning(theme)),
            Rect::new(inner.x, y, inner.width, 1),
        );
    }

    let new_button_style = text(theme);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " + New agent",
                new_button_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled("   ^B n", new_button_style),
        ]))
        .style(new_button_style),
        new_button,
    );

    if app.mode() == Mode::Menu {
        render_menu(frame, app, menu_popup_area(button), theme);
    }

    let button_style = if app.mode() == Mode::Menu {
        theme.selected()
    } else {
        text(theme)
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

fn paired_truncate(left: &str, right: &str, width: usize) -> String {
    const SEPARATOR: &str = " · ";
    if width <= SEPARATOR.chars().count() {
        return end_truncate(left, width);
    }
    let available = width - SEPARATOR.chars().count();
    let left_width = available / 2;
    let right_width = available - left_width;
    format!(
        "{}{}{}",
        end_truncate(left, left_width),
        SEPARATOR,
        end_truncate(right, right_width)
    )
}

fn status_display(status: AgentDisplayStatus, colors_enabled: bool) -> (&'static str, Style) {
    match status {
        AgentDisplayStatus::Unknown | AgentDisplayStatus::Idle => {
            ("●", Style::default().add_modifier(Modifier::DIM))
        }
        AgentDisplayStatus::Working => ("●", status_color(Color::Yellow, colors_enabled)),
        AgentDisplayStatus::Done => ("●", status_color(Color::Green, colors_enabled)),
        AgentDisplayStatus::NeedsYou | AgentDisplayStatus::Failed => {
            ("●", status_color(Color::Red, colors_enabled))
        }
    }
}

fn status_color(color: Color, colors_enabled: bool) -> Style {
    Style::default().fg(if colors_enabled { color } else { Color::Reset })
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
    screen: Option<&TerminalSnapshot>,
    scrolled: bool,
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
        && !scrolled
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
                end_truncate(&value, 42),
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
        ModalSize::Compact,
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
                "  [j/k] move  [Enter/Space] select  [Esc] cancel",
                theme.muted(),
            )),
        ],
    );
}

fn render_workspace_choices(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(state) = app.new_agent() else {
        return;
    };
    let visible = 7;
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
                    let name = end_truncate(
                        &choice
                            .path
                            .file_name()
                            .unwrap_or(choice.path.as_os_str())
                            .to_string_lossy(),
                        14,
                    );
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
                        Span::styled(format!("{name:<14}"), text(theme)),
                        Span::styled(
                            end_truncate(&choice.path.display().to_string(), 32),
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
    render_dialog(
        frame,
        theme,
        " Choose workspace ",
        ModalSize::Compact,
        lines,
    );
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
    render_dialog(frame, theme, " Choose agent ", ModalSize::Compact, lines);
}

fn render_native_browser(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(browser) = app.native_browser() else {
        return;
    };
    let area = ModalSize::Large.area(frame.area());
    let visible = usize::from(area.height.saturating_sub(6));
    let content_width = usize::from(area.width.saturating_sub(8));
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
                Span::styled(end_truncate(&label, content_width), text(theme)),
            ])
        });
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "  {}",
                end_truncate(&browser.current_path.display().to_string(), content_width,)
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
            format!("  {}", end_truncate(error, content_width)),
            warning(theme),
        )));
    } else {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "  [Enter/l] open/use  [h] parent  [j/k] move  [Esc] cancel",
        theme.muted(),
    )));
    render_dialog(frame, theme, " Select workspace ", ModalSize::Large, lines);
}

fn render_embedded_browser(
    frame: &mut Frame<'_>,
    snapshot: Option<&TerminalProcessSnapshot>,
    theme: Theme,
) {
    let area = embedded_modal_area(frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::bordered()
            .title(" Select workspace · Yazi ")
            .border_style(accent(theme))
            .style(theme.surface()),
        area,
    );
    let content = embedded_terminal_area(frame.area());
    if let Some(snapshot) = snapshot {
        let terminal = TerminalScreen::new(&snapshot.terminal);
        if let Some(position) = terminal.cursor_position(content) {
            frame.set_cursor_position(position);
        }
        frame.render_widget(terminal, content);
    }
    frame.render_widget(
        Paragraph::new("[q] use current directory  [Q] cancel  [Ctrl+B x] force close")
            .style(theme.muted()),
        Rect::new(
            area.x.saturating_add(1),
            area.bottom().saturating_sub(2),
            area.width.saturating_sub(2),
            1,
        ),
    );
}

fn embedded_modal_area(area: Rect) -> Rect {
    ModalSize::Large.area(area)
}

pub(crate) fn embedded_terminal_area(area: Rect) -> Rect {
    let modal = embedded_modal_area(area);
    Rect::new(
        modal.x.saturating_add(1),
        modal.y.saturating_add(1),
        modal.width.saturating_sub(2),
        modal.height.saturating_sub(4),
    )
}

fn render_confirmation(frame: &mut Frame<'_>, theme: Theme, title: &str, prompt: &str) {
    render_dialog(
        frame,
        theme,
        title,
        ModalSize::Standard,
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
        ModalSize::Standard,
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
    render_dialog(frame, theme, " Keybinds ", ModalSize::Standard, lines);
}

fn render_settings(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    render_dialog(
        frame,
        theme,
        " Settings ",
        ModalSize::Standard,
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
    size: ModalSize,
    lines: Vec<Line<'static>>,
) {
    let area = size.area(frame.area());
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
        AgentActivity, AgentSnapshot, AttachmentSummary, ConnectionId, GitContext, SessionId,
        SessionRevision, SessionSummary, SvarmSessionSnapshot, TerminalSequence,
    };
    use svarm_agent::terminal_model::{TerminalPosition, TerminalSize};

    use super::*;

    fn terminal_snapshot(rows: u16, cols: u16, text: &str) -> TerminalSnapshot {
        let mut snapshot = TerminalSnapshot::blank(TerminalSize::new(rows, cols));
        for (column, character) in text.chars().enumerate() {
            snapshot.cell_mut(0, column as u16).unwrap().contents = character.to_string();
        }
        snapshot.state.cursor.position = TerminalPosition {
            row: 0,
            column: text.chars().count() as u16,
        };
        snapshot
    }

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
        assert_eq!(
            new_agent_button_area(area, true),
            Some(Rect::new(0, 38, 27, 1))
        );
        assert_eq!(menu_button_area(area, true), Some(Rect::new(0, 39, 27, 1)));
        assert_eq!(menu_item_at(area, 2, 36), Some(MenuItem::Keybinds));
        assert_eq!(menu_item_at(area, 2, 37), Some(MenuItem::Settings));
        assert_eq!(menu_item_at(area, 50, 36), None);
    }

    #[test]
    fn sidebar_renders_new_agent_button_above_menu() {
        let app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Monochrome,
            false,
            None,
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
                    },
                );
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("+ New agent"));
        assert!(rendered.contains("≡ Menu"));
    }

    #[test]
    fn centered_rect_clamps_to_the_terminal() {
        assert_eq!(
            centered_rect(100, 100, Rect::new(2, 3, 20, 10)),
            Rect::new(2, 3, 20, 10)
        );
    }

    #[test]
    fn modal_tiers_have_canonical_areas() {
        let terminal = Rect::new(0, 0, 120, 40);
        assert_eq!(ModalSize::Compact.area(terminal), Rect::new(28, 14, 64, 12));
        assert_eq!(
            ModalSize::Standard.area(terminal),
            Rect::new(24, 11, 72, 18)
        );
        assert_eq!(ModalSize::Large.area(terminal), Rect::new(10, 5, 100, 30));
        assert_eq!(
            embedded_modal_area(terminal),
            ModalSize::Large.area(terminal)
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
                            scrolled: false,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
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
                            scrolled: false,
                            embedded: None,
                            theme: app.theme().theme(true),
                            colors_enabled: true,
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
            completed_generation: 0,
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: None,
            activity: AgentActivity::Unknown,
            recognition: None,
            git: None,
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
    fn agent_cards_show_titles_status_markers_and_optional_git_context() {
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
            exit: (status == SessionStatus::Exited).then_some(svarm_agent::ProcessExit {
                code: 1,
                signal: None,
                success: false,
            }),
            output_generation,
            seen_generation,
            completed_generation: if id == 2 { output_generation } else { 0 },
            terminal_sequence: TerminalSequence(0),
            read_error: None,
            conversation_title: (id != 1).then(|| format!("Conversation {id}")),
            activity: AgentActivity::Idle,
            recognition: None,
            git: (id == 2).then_some(GitContext {
                branch: "feature/sidebar".into(),
                worktree: "/tmp/project-eight".into(),
            }),
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
        assert!(rendered.contains("● 1 · Unnamed conversation"));
        assert!(rendered.contains("● 2 · Conversation 2"));
        assert!(!rendered.contains("failed"));
        assert!(!rendered.contains("done"));
        assert!(!rendered.contains("/tmp/project-eight"));
        assert!(rendered.contains("project-e… · feature/s…"));

        let theme = crate::theme::ThemeName::CatppuccinMocha.theme(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: None,
                        theme,
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();
        let circle_colors = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .map(|cell| cell.fg)
            .collect::<Vec<_>>();
        assert_eq!(circle_colors, vec![Color::Red, Color::Green]);
        for row in 0..24 {
            for column in 0..SIDEBAR_WIDTH {
                assert_eq!(
                    terminal.backend().buffer()[(column, row)].bg,
                    Color::Reset,
                    "sidebar background changed at ({column}, {row})"
                );
            }
        }
    }

    #[test]
    fn status_markers_share_one_glyph_and_keep_fixed_colors_across_themes() {
        assert_eq!(
            status_display(AgentDisplayStatus::Unknown, true),
            status_display(AgentDisplayStatus::Idle, true)
        );
        for status in [
            AgentDisplayStatus::Unknown,
            AgentDisplayStatus::Idle,
            AgentDisplayStatus::Working,
            AgentDisplayStatus::Done,
            AgentDisplayStatus::NeedsYou,
            AgentDisplayStatus::Failed,
        ] {
            assert_eq!(status_display(status, true).0, "●");
        }
        assert_eq!(
            status_display(AgentDisplayStatus::Working, true).1.fg,
            Some(Color::Yellow)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::Done, true).1.fg,
            Some(Color::Green)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::NeedsYou, true).1.fg,
            Some(Color::Red)
        );
        assert_eq!(
            status_display(AgentDisplayStatus::Working, false).1.fg,
            Some(Color::Reset)
        );
    }

    #[test]
    fn multiline_agent_list_scrolls_to_keep_the_selected_card_visible_at_80x24() {
        let agents = (1..=8)
            .map(|id| AgentSnapshot {
                id: svarm_agent::AgentId::new(id),
                kind: svarm_agent::AgentKind::Claude,
                launch_directory: PathBuf::from(format!("/tmp/project-{id}")),
                status: SessionStatus::Running,
                exit: None,
                output_generation: 1,
                seen_generation: 1,
                completed_generation: 0,
                terminal_sequence: TerminalSequence(0),
                read_error: None,
                conversation_title: Some(format!("Conversation {id}")),
                activity: AgentActivity::Idle,
                recognition: None,
                git: None,
            })
            .collect::<Vec<_>>();
        let mut app = App::hydrate(
            SvarmSessionSnapshot {
                summary: SessionSummary {
                    id: SessionId(9),
                    running_agents: agents.len(),
                    total_agents: agents.len(),
                    attachment: None,
                    last_user_activity_ms: 1,
                    revision: SessionRevision(1),
                },
                selected_agent_id: Some(svarm_agent::AgentId::new(8)),
                rows: 24,
                cols: 80,
                agents,
            },
            crate::theme::ThemeName::Monochrome,
            None,
        );

        let rendered = render_app_text(&app);
        assert!(rendered.contains("8 · Conversation 8"));
        assert!(rendered.contains("Claude Code"));
        assert!(!rendered.contains("8 · Claude Code"));
        assert!(!rendered.contains("idle"));

        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(agent_item_at(&app, area, 2, 1), Some(1));
        assert_eq!(agent_item_at(&app, area, 2, 19), Some(7));
        assert_eq!(agent_item_at(&app, area, 2, 22), None);
        assert_eq!(agent_item_at(&app, area, 40, 19), None);

        app.scroll_sidebar(-1, agent_list_page_size(&app, area));
        assert_eq!(agent_item_at(&app, area, 2, 1), Some(0));
        let rendered = render_app_text(&app);
        assert!(rendered.contains("1 · Conversation 1"));
        assert!(!rendered.contains("8 · Conversation 8"));
    }

    #[test]
    fn the_agent_cursor_is_the_host_terminal_cursor_not_a_painted_cell() {
        let screen = terminal_snapshot(24, 55, "prompt> ");
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
                        screen: Some(&screen),
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
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
    fn scrollback_does_not_overlay_a_position_label() {
        let screen = terminal_snapshot(24, 80, "terminal output");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| {
                render_terminal(
                    frame,
                    Some(&screen),
                    true,
                    Mode::Terminal,
                    frame.area(),
                    crate::theme::ThemeName::Dark.theme(true),
                );
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("terminal output"));
        assert!(!rendered.contains("scroll 12/40"));
    }

    #[test]
    fn a_hidden_agent_cursor_leaves_the_host_cursor_hidden() {
        let mut screen = terminal_snapshot(24, 55, "working");
        screen.state.cursor.visible = false;
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
                        screen: Some(&screen),
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();

        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn embedded_terminal_uses_prepared_screen_area_and_cursor_at_80x24() {
        let mut terminal = terminal_snapshot(18, 74, "yazi> ");
        terminal.state.cursor.style = svarm_agent::CursorStyle::SteadyBar;
        let snapshot = TerminalProcessSnapshot {
            terminal,
            status: SessionStatus::Running,
            exit: None,
            read_error: None,
            generation: 1,
            modes: svarm_agent::protocol::TerminalModes::default(),
            output_closed: false,
        };
        let mut app = App::new(
            "workspace".into(),
            crate::theme::ThemeName::Dark,
            true,
            None,
        );
        app.open_embedded_browser();
        let area = Rect::new(0, 0, 80, 24);
        let content = embedded_terminal_area(area);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| {
                render(
                    frame,
                    UiModel {
                        app: &app,
                        screen: None,
                        scrolled: false,
                        embedded: Some(&snapshot),
                        theme: app.theme().theme(true),
                        colors_enabled: true,
                    },
                )
            })
            .unwrap();

        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            Position::new(content.x + 6, content.y)
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Select workspace · Yazi"));
        assert!(rendered.contains("force close"));
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
                        scrolled: false,
                        embedded: None,
                        theme: app.theme().theme(false),
                        colors_enabled: false,
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
