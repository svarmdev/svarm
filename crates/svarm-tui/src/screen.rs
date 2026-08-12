//! Draws an emulated terminal screen into the frame.
//!
//! Svarm renders this itself rather than through a widget library because the translation from
//! terminal cell to buffer cell is where an agent's colors are either preserved or quietly lost.
//! Two rules matter and neither is the obvious default:
//!
//! * A cell that asked for the terminal's default foreground or background must stay
//!   [`Color::Reset`], so the pane keeps the same base colors the agent was told about when it
//!   queried the terminal, and matches how the agent would look outside Svarm.
//! * The sixteen palette entries must be written back as the ANSI codes an agent would have used,
//!   not as indexed colors. Terminals apply their own rules to `SGR 31` — most notably brightening
//!   it when bold is set — but render `SGR 38;5;1` at its literal palette entry, so routing
//!   everything through the indexed form turns bold red into dark red across a whole agent
//!   interface. Emitting the ANSI form leaves that decision where it belongs, with the terminal.

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use svarm_agent::terminal_model::{TerminalCell, TerminalColor, TerminalSnapshot};

pub(crate) struct TerminalScreen<'a> {
    screen: &'a TerminalSnapshot,
}

impl<'a> TerminalScreen<'a> {
    pub const fn new(screen: &'a TerminalSnapshot) -> Self {
        Self { screen }
    }

    /// Where the host terminal should place its own cursor, in frame coordinates, or `None` when
    /// the agent has hidden it or it falls outside the pane.
    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        if !self.screen.state.cursor.visible {
            return None;
        }
        let row = self.screen.state.cursor.position.row;
        let column = self.screen.state.cursor.position.column;
        (row < area.height && column < area.width)
            .then(|| Position::new(area.x + column, area.y + row))
    }
}

impl Widget for TerminalScreen<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for row in 0..area.height {
            for column in 0..area.width {
                let Some(cell) = self.screen.cell(row, column) else {
                    continue;
                };
                let buffer_cell = &mut buffer[(area.x + column, area.y + row)];
                cell.contents.with_str(|contents| {
                    buffer_cell.set_symbol(if contents.is_empty() { " " } else { contents });
                });
                buffer_cell.set_style(style(cell));
            }
        }
    }
}

fn style(cell: &TerminalCell) -> Style {
    let mut modifiers = Modifier::empty();
    for (active, modifier) in [
        (cell.attributes.bold, Modifier::BOLD),
        (cell.attributes.dim, Modifier::DIM),
        (cell.attributes.italic, Modifier::ITALIC),
        (cell.attributes.underline, Modifier::UNDERLINED),
        (cell.attributes.inverse, Modifier::REVERSED),
        (cell.attributes.blink, Modifier::SLOW_BLINK),
        (cell.attributes.hidden, Modifier::HIDDEN),
        (cell.attributes.strikethrough, Modifier::CROSSED_OUT),
    ] {
        if active {
            modifiers |= modifier;
        }
    }
    Style::reset()
        .fg(color(cell.foreground))
        .bg(color(cell.background))
        .add_modifier(modifiers)
}

fn color(color: TerminalColor) -> Color {
    match color {
        TerminalColor::Default => Color::Reset,
        TerminalColor::Indexed(index) => ansi(index),
        TerminalColor::Rgb([red, green, blue]) => Color::Rgb(red, green, blue),
    }
}

/// Ratatui's named colors are written as `SGR 30-37` and `SGR 90-97`; anything above the first
/// sixteen entries has only the indexed form.
fn ansi(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        index => Color::Indexed(index),
    }
}

#[cfg(test)]
mod tests {
    use svarm_agent::terminal_model::{TerminalAttributes, TerminalPosition, TerminalSize};

    use super::*;

    fn screen(text: &str) -> TerminalSnapshot {
        let mut screen = TerminalSnapshot::blank(TerminalSize::new(2, 12));
        for (column, character) in text.chars().enumerate() {
            screen.cell_mut(0, column as u16).unwrap().contents = character.to_string().into();
        }
        screen.state.cursor.position = TerminalPosition {
            row: 0,
            column: text.chars().count() as u16,
        };
        screen
    }

    fn render(screen: &TerminalSnapshot) -> Buffer {
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::empty(area);
        TerminalScreen::new(screen).render(area, &mut buffer);
        buffer
    }

    #[test]
    fn ansi_colors_keep_their_named_form_so_the_terminal_can_apply_its_own_rules() {
        // Written back as `SGR 1` and `SGR 31`, which is what the agent sent and what a terminal
        // brightens; an indexed color would instead be pinned to its literal palette entry.
        let mut screen = screen("red");
        screen.cells[0].foreground = TerminalColor::Indexed(1);
        screen.cells[0].attributes.bold = true;
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].fg, Color::Red);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));

        screen.cells[0].foreground = TerminalColor::Indexed(9);
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].fg, Color::LightRed);

        screen.cells[0].foreground = TerminalColor::Default;
        screen.cells[0].background = TerminalColor::Indexed(1);
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].bg, Color::Red);
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
    }

    #[test]
    fn palette_and_true_color_requests_are_passed_through_unchanged() {
        let mut screen = screen("pink");
        screen.cells[0].foreground = TerminalColor::Indexed(200);
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(200));

        screen.cells[0].foreground = TerminalColor::Rgb([10, 20, 30]);
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(10, 20, 30));

        screen.cells[0].background = TerminalColor::Rgb([1, 2, 3]);
        let buffer = render(&screen);
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn default_colors_defer_to_the_host_terminal() {
        let buffer = render(&screen("plain"));
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn attributes_beyond_the_common_four_survive_rendering() {
        for (attributes, modifier) in [
            (
                TerminalAttributes {
                    strikethrough: true,
                    ..Default::default()
                },
                Modifier::CROSSED_OUT,
            ),
            (
                TerminalAttributes {
                    blink: true,
                    ..Default::default()
                },
                Modifier::SLOW_BLINK,
            ),
            (
                TerminalAttributes {
                    hidden: true,
                    ..Default::default()
                },
                Modifier::HIDDEN,
            ),
            (
                TerminalAttributes {
                    italic: true,
                    ..Default::default()
                },
                Modifier::ITALIC,
            ),
            (
                TerminalAttributes {
                    dim: true,
                    ..Default::default()
                },
                Modifier::DIM,
            ),
            (
                TerminalAttributes {
                    underline: true,
                    ..Default::default()
                },
                Modifier::UNDERLINED,
            ),
            (
                TerminalAttributes {
                    inverse: true,
                    ..Default::default()
                },
                Modifier::REVERSED,
            ),
        ] {
            let mut screen = screen("test");
            screen.cells[0].attributes = attributes;
            let buffer = render(&screen);
            assert!(
                buffer[(0, 0)].modifier.contains(modifier),
                "{modifier:?} was dropped"
            );
        }
    }

    #[test]
    fn cells_the_agent_never_wrote_do_not_keep_earlier_content() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 2));
        buffer[(4, 0)].set_symbol("x");
        TerminalScreen::new(&screen("ab")).render(Rect::new(0, 0, 12, 2), &mut buffer);

        assert_eq!(buffer[(4, 0)].symbol(), " ");
    }

    #[test]
    fn the_cursor_reports_its_place_in_frame_coordinates_and_hides_on_request() {
        let mut screen = screen("ab");
        let area = Rect::new(3, 1, 12, 2);
        assert_eq!(
            TerminalScreen::new(&screen).cursor_position(area),
            Some(Position::new(5, 1))
        );

        screen.state.cursor.visible = false;
        assert_eq!(TerminalScreen::new(&screen).cursor_position(area), None);
    }
}
