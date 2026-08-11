use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use tui_term::{
    vt100::Screen,
    widget::{Cursor, PseudoTerminal},
};

use crate::{
    app::{App, MenuItem, Mode},
    input::MANAGEMENT_KEYBINDINGS,
    theme::Theme,
};
use svarm_agent::SessionStatus;

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
pub const SIDEBAR_WIDTH: u16 = 25;
const MENU_HEIGHT: u16 = MenuItem::ALL.len() as u16 + 2;

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
        Mode::ChooseAgent => render_choose_agent(frame, theme),
        Mode::ConfirmClose => {
            render_confirmation(frame, theme, "Close agent?", "Close this agent?")
        }
        Mode::ConfirmQuit => {
            render_confirmation(frame, theme, "Quit Svarm?", "Stop all agents and quit?")
        }
        Mode::Keybinds => render_keybinds(frame, theme),
        Mode::Settings => render_settings(frame, app, theme),
        _ => {}
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
        button.width,
        MENU_HEIGHT,
    )
}

fn render_sidebar(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let title = Line::from(vec![
        Span::styled(" svarm", accent(theme).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {} ", app.workspace_name()), theme.muted()),
    ]);
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
            let mut line = Line::from(vec![
                Span::styled(if selected { " ▌ " } else { "   " }, accent(theme)),
                Span::styled(format!("{} ", index + 1), theme.muted()),
                Span::styled(agent.kind().label(), text(theme)),
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
    let terminal =
        PseudoTerminal::new(screen).cursor(Cursor::default().visibility(mode == Mode::Terminal));
    frame.render_widget(terminal, area);
}

fn render_choose_agent(frame: &mut Frame<'_>, theme: Theme) {
    render_dialog(
        frame,
        theme,
        " New agent ",
        42,
        7,
        vec![
            Line::from(""),
            Line::from("  [c] Codex"),
            Line::from("  [a] Claude Code"),
            Line::from(""),
            Line::from(Span::styled("  Esc cancels", theme.muted())),
        ],
    );
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
        58,
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
    use ratatui::{Terminal, backend::TestBackend};

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
        assert_eq!(menu_item_at(area, 30, 36), None);
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
}
