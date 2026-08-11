use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use tui_term::widget::{Cursor, PseudoTerminal};

use crate::{Mode, app::App, session::SessionStatus};

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;
pub const SIDEBAR_WIDTH: u16 = 25;

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let theme = Theme::detect();
    let area = frame.area();
    frame.render_widget(Block::new().style(theme.page), area);

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new(format!(
                "Svarm needs at least {MIN_WIDTH}x{MIN_HEIGHT}\ncurrent terminal: {}x{}",
                area.width, area.height
            ))
            .centered()
            .style(theme.text),
            area,
        );
        return;
    }

    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let terminal = terminal_area(area, app.sidebar_visible);
    if app.sidebar_visible {
        let [sidebar, _] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)]).areas(body);
        render_sidebar(frame, app, sidebar, theme);
    }
    render_terminal(frame, app, terminal, theme);
    render_footer(frame, app, footer, theme);

    if app.mode == Mode::Help {
        render_help(frame, theme);
    }
}

pub fn terminal_area(area: Rect, sidebar_visible: bool) -> Rect {
    let body_height = area.height.saturating_sub(1);
    let sidebar_width = if sidebar_visible {
        SIDEBAR_WIDTH.min(area.width.saturating_sub(1))
    } else {
        0
    };
    Rect::new(
        area.x.saturating_add(sidebar_width),
        area.y,
        area.width.saturating_sub(sidebar_width),
        body_height,
    )
}

fn render_sidebar(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let title = Line::from(vec![
        Span::styled(" svarm", theme.accent.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {} ", app.workspace_name()), theme.muted),
    ]);
    let block = Block::new()
        .title(title)
        .borders(Borders::RIGHT)
        .border_style(theme.border);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = app
        .agents
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let selected = index == app.selected;
            let status = agent.session.status();
            let marker = match (status, agent.has_unseen_output()) {
                (SessionStatus::Exited, _) => "×",
                (_, true) => "!",
                _ => "●",
            };
            let status_style = match (status, agent.has_unseen_output()) {
                (SessionStatus::Exited, _) => theme.muted,
                (_, true) => theme.warning,
                _ => theme.success,
            };
            let mut line = Line::from(vec![
                Span::styled(if selected { " ▌ " } else { "   " }, theme.accent),
                Span::styled(format!("{} ", index + 1), theme.muted),
                Span::styled(agent.session.kind.label(), theme.text),
                Span::raw(" "),
                Span::styled(marker, status_style),
            ]);
            if selected {
                line = line.style(theme.selection);
            }
            ListItem::new(line)
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(rows), inner);
}

fn render_terminal(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let Some(agent) = app.current() else {
        frame.render_widget(
            Paragraph::new("No agents open. Press Ctrl+B, then n to start one.")
                .centered()
                .style(theme.muted),
            area,
        );
        return;
    };
    let parser = agent.session.parser();
    let terminal = PseudoTerminal::new(parser.screen())
        .cursor(Cursor::default().visibility(app.mode == Mode::Terminal));
    frame.render_widget(terminal, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let line = match app.mode {
        Mode::Terminal => match &app.notice {
            Some(notice) => Line::from(Span::styled(format!(" {notice}"), theme.warning)),
            None => Line::from(vec![
                Span::styled(" ctrl+b", theme.accent.add_modifier(Modifier::BOLD)),
                Span::styled(" commands", theme.muted),
            ]),
        },
        Mode::Prefix => shortcuts(
            theme,
            &[
                ("j/k", "switch"),
                ("n", "new"),
                ("x", "close"),
                ("b", "sidebar"),
                ("q", "quit"),
                ("?", "help"),
            ],
        ),
        Mode::ChooseAgent => shortcuts(
            theme,
            &[("c", "Codex"), ("a", "Claude Code"), ("esc", "cancel")],
        ),
        Mode::ConfirmClose => Line::from(vec![
            Span::styled(" Close this agent? ", theme.warning),
            Span::styled("[y] yes", theme.text),
            Span::styled("  [esc] cancel", theme.muted),
        ]),
        Mode::ConfirmQuit => Line::from(vec![
            Span::styled(" Stop all agents and quit? ", theme.warning),
            Span::styled("[y] yes", theme.text),
            Span::styled("  [esc] cancel", theme.muted),
        ]),
        Mode::Help => shortcuts(theme, &[("esc", "close help")]),
    };
    frame.render_widget(Paragraph::new(line).style(theme.footer), area);
}

fn shortcuts(theme: Theme, pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, (key, action)) in pairs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", theme.muted));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            theme.accent.add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {action}"), theme.muted));
    }
    Line::from(spans)
}

fn render_help(frame: &mut Frame<'_>, theme: Theme) {
    let area = centered_rect(58, 15, frame.area());
    frame.render_widget(Clear, area);
    let help = Paragraph::new(vec![
        Line::from(Span::styled(
            "Svarm commands",
            theme.accent.add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Ctrl+B, j/k or arrows   previous/next agent"),
        Line::from("Ctrl+B, 1..9            select agent"),
        Line::from("Ctrl+B, n               start Codex or Claude Code"),
        Line::from("Ctrl+B, x               close the selected agent"),
        Line::from("Ctrl+B, b               toggle the sidebar"),
        Line::from("Ctrl+B, Ctrl+B          send Ctrl+B to the agent"),
        Line::from("Ctrl+B, q               stop all agents and quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Outside command mode, every key goes to the native agent TUI.",
            theme.muted,
        )),
    ])
    .block(Block::bordered().title(" Help ").border_style(theme.accent))
    .wrap(Wrap { trim: false })
    .style(theme.text);
    frame.render_widget(help, area);
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

#[derive(Clone, Copy)]
struct Theme {
    page: Style,
    text: Style,
    muted: Style,
    accent: Style,
    success: Style,
    warning: Style,
    border: Style,
    selection: Style,
    footer: Style,
}

impl Theme {
    fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Self {
                page: Style::default(),
                text: Style::default(),
                muted: Style::default().add_modifier(Modifier::DIM),
                accent: Style::default().add_modifier(Modifier::BOLD),
                success: Style::default(),
                warning: Style::default().add_modifier(Modifier::BOLD),
                border: Style::default().add_modifier(Modifier::DIM),
                selection: Style::default().add_modifier(Modifier::REVERSED),
                footer: Style::default().add_modifier(Modifier::REVERSED),
            };
        }
        Self {
            page: Style::default(),
            text: Style::default().fg(Color::White),
            muted: Style::default().fg(Color::DarkGray),
            accent: Style::default().fg(Color::Cyan),
            success: Style::default().fg(Color::Green),
            warning: Style::default().fg(Color::Yellow),
            border: Style::default().fg(Color::DarkGray),
            selection: Style::default().add_modifier(Modifier::REVERSED),
            footer: Style::default().bg(Color::DarkGray).fg(Color::White),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_area_reserves_sidebar_and_footer() {
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), true),
            Rect::new(SIDEBAR_WIDTH, 0, 120 - SIDEBAR_WIDTH, 39)
        );
        assert_eq!(
            terminal_area(Rect::new(0, 0, 120, 40), false),
            Rect::new(0, 0, 120, 39)
        );
    }

    #[test]
    fn centered_rect_clamps_to_the_terminal() {
        assert_eq!(
            centered_rect(100, 100, Rect::new(2, 3, 20, 10)),
            Rect::new(2, 3, 20, 10)
        );
    }
}
