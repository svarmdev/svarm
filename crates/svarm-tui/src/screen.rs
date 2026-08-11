//! Draws an agent's emulated screen into the frame.
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
use svarm_agent::vt100::{Cell, Color as TerminalColor, Screen};

pub(crate) struct AgentScreen<'a> {
    screen: &'a Screen,
}

impl<'a> AgentScreen<'a> {
    pub const fn new(screen: &'a Screen) -> Self {
        Self { screen }
    }

    /// Where the host terminal should place its own cursor, in frame coordinates, or `None` when
    /// the agent has hidden it or it falls outside the pane.
    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        if self.screen.hide_cursor() {
            return None;
        }
        let (row, column) = self.screen.cursor_position();
        (row < area.height && column < area.width)
            .then(|| Position::new(area.x + column, area.y + row))
    }
}

impl Widget for AgentScreen<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for row in 0..area.height {
            for column in 0..area.width {
                let Some(cell) = self.screen.cell(row, column) else {
                    continue;
                };
                let buffer_cell = &mut buffer[(area.x + column, area.y + row)];
                buffer_cell.set_symbol(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
                buffer_cell.set_style(style(cell));
            }
        }
    }
}

fn style(cell: &Cell) -> Style {
    let mut modifiers = Modifier::empty();
    for (active, modifier) in [
        (cell.bold(), Modifier::BOLD),
        (cell.dim(), Modifier::DIM),
        (cell.italic(), Modifier::ITALIC),
        (cell.underline(), Modifier::UNDERLINED),
        (cell.inverse(), Modifier::REVERSED),
        (cell.blink(), Modifier::SLOW_BLINK),
        (cell.hidden(), Modifier::HIDDEN),
        (cell.strikethrough(), Modifier::CROSSED_OUT),
    ] {
        if active {
            modifiers |= modifier;
        }
    }
    Style::reset()
        .fg(color(cell.fgcolor()))
        .bg(color(cell.bgcolor()))
        .add_modifier(modifiers)
}

fn color(color: TerminalColor) -> Color {
    match color {
        TerminalColor::Default => Color::Reset,
        TerminalColor::Idx(index) => ansi(index),
        TerminalColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
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
    use svarm_agent::vt100::Parser;

    use super::*;

    fn render(output: &[u8]) -> Buffer {
        let mut parser = Parser::new(2, 12, 0);
        parser.process(output);
        let area = Rect::new(0, 0, 12, 2);
        let mut buffer = Buffer::empty(area);
        AgentScreen::new(parser.screen()).render(area, &mut buffer);
        buffer
    }

    #[test]
    fn ansi_colors_keep_their_named_form_so_the_terminal_can_apply_its_own_rules() {
        // Written back as `SGR 1` and `SGR 31`, which is what the agent sent and what a terminal
        // brightens; an indexed color would instead be pinned to its literal palette entry.
        let buffer = render(b"\x1b[1;31mred");
        assert_eq!(buffer[(0, 0)].fg, Color::Red);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));

        let buffer = render(b"\x1b[91mbright");
        assert_eq!(buffer[(0, 0)].fg, Color::LightRed);

        let buffer = render(b"\x1b[41mon-red");
        assert_eq!(buffer[(0, 0)].bg, Color::Red);
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
    }

    #[test]
    fn palette_and_true_color_requests_are_passed_through_unchanged() {
        let buffer = render(b"\x1b[38;5;200mpink");
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(200));

        let buffer = render(b"\x1b[38;2;10;20;30mrgb");
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(10, 20, 30));

        let buffer = render(b"\x1b[48;2;1;2;3mrgb");
        assert_eq!(buffer[(0, 0)].bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn default_colors_defer_to_the_host_terminal() {
        let buffer = render(b"plain");
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }

    #[test]
    fn attributes_beyond_the_common_four_survive_rendering() {
        for (output, modifier) in [
            (&b"\x1b[9mgone"[..], Modifier::CROSSED_OUT),
            (&b"\x1b[5mblink"[..], Modifier::SLOW_BLINK),
            (&b"\x1b[8mhidden"[..], Modifier::HIDDEN),
            (&b"\x1b[3mitalic"[..], Modifier::ITALIC),
            (&b"\x1b[2mdim"[..], Modifier::DIM),
            (&b"\x1b[4munder"[..], Modifier::UNDERLINED),
            (&b"\x1b[7minv"[..], Modifier::REVERSED),
        ] {
            let buffer = render(output);
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
        let mut parser = Parser::new(2, 12, 0);
        parser.process(b"ab");
        AgentScreen::new(parser.screen()).render(Rect::new(0, 0, 12, 2), &mut buffer);

        assert_eq!(buffer[(4, 0)].symbol(), " ");
    }

    #[test]
    fn the_cursor_reports_its_place_in_frame_coordinates_and_hides_on_request() {
        let mut parser = Parser::new(2, 12, 0);
        parser.process(b"ab");
        let area = Rect::new(3, 1, 12, 2);
        assert_eq!(
            AgentScreen::new(parser.screen()).cursor_position(area),
            Some(Position::new(5, 1))
        );

        parser.process(b"\x1b[?25l");
        assert_eq!(
            AgentScreen::new(parser.screen()).cursor_position(area),
            None
        );
    }
}
