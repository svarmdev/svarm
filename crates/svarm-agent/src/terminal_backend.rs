//! Runtime adapter for the current terminal emulator.
//!
//! `vt100` types stop here. Everything leaving this module is owned by Svarm.

use std::collections::BTreeMap;

use vt100::{MouseProtocolEncoding, MouseProtocolMode, Parser};

use crate::{
    CursorStyle,
    terminal_model::{
        MouseEncoding, MouseProtocol, TerminalAttributes, TerminalBackend, TerminalBells,
        TerminalCell, TerminalClipboardRequest, TerminalColor, TerminalCursor, TerminalModes,
        TerminalPosition, TerminalProgress, TerminalProgressState, TerminalScrollback,
        TerminalSize, TerminalSnapshot, TerminalState,
    },
};

#[derive(Default)]
struct BellCallbacks {
    audible: u64,
    visual: u64,
}

impl vt100::Callbacks for BellCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.audible = self.audible.saturating_add(1);
    }

    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.visual = self.visual.saturating_add(1);
    }
}

pub(crate) struct Vt100Backend {
    parser: Parser<BellCallbacks>,
}

#[cfg(test)]
pub(crate) fn create(size: TerminalSize, scrollback: usize) -> Box<dyn TerminalBackend> {
    Box::new(Vt100Backend::new(size, scrollback))
}

pub(crate) fn create_with_scrollback_bytes(
    size: TerminalSize,
    scrollback_max_bytes: usize,
) -> Box<dyn TerminalBackend> {
    Box::new(Vt100Backend::new_with_scrollback_bytes(
        size,
        scrollback_max_bytes,
    ))
}

impl Vt100Backend {
    #[cfg(test)]
    pub(crate) fn new(size: TerminalSize, scrollback: usize) -> Self {
        Self {
            parser: Parser::new_with_callbacks(
                size.rows.max(1),
                size.cols.max(1),
                scrollback,
                BellCallbacks::default(),
            ),
        }
    }

    pub(crate) fn new_with_scrollback_bytes(
        size: TerminalSize,
        scrollback_max_bytes: usize,
    ) -> Self {
        Self {
            parser: Parser::new_with_callbacks_and_scrollback_bytes(
                size.rows.max(1),
                size.cols.max(1),
                scrollback_max_bytes,
                BellCallbacks::default(),
            ),
        }
    }

    fn semantic_snapshot(
        &self,
        cursor_style: CursorStyle,
        modes: TerminalModes,
    ) -> TerminalSnapshot {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let size = TerminalSize::new(rows, cols);
        let mut hyperlinks = Vec::new();
        let mut hyperlink_ids = BTreeMap::new();
        let cells = (0..rows)
            .flat_map(|row| (0..cols).map(move |column| (row, column)))
            .map(|(row, column)| {
                let Some(cell) = screen.cell(row, column) else {
                    return TerminalCell::default();
                };
                let hyperlink = screen.hyperlink_uri(cell.hyperlink_id()).map(|uri| {
                    *hyperlink_ids.entry(uri.to_owned()).or_insert_with(|| {
                        hyperlinks.push(uri.to_owned());
                        hyperlinks.len() as u32
                    })
                });
                TerminalCell {
                    contents: cell.contents().into(),
                    foreground: color(cell.fgcolor()),
                    background: color(cell.bgcolor()),
                    attributes: TerminalAttributes {
                        bold: cell.bold(),
                        dim: cell.dim(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                        inverse: cell.inverse(),
                        blink: cell.blink(),
                        hidden: cell.hidden(),
                        strikethrough: cell.strikethrough(),
                    },
                    wide: cell.is_wide(),
                    wide_continuation: cell.is_wide_continuation(),
                    hyperlink,
                }
            })
            .collect();
        let (cursor_row, cursor_column) = screen.cursor_position();
        let callbacks = self.parser.callbacks();
        TerminalSnapshot {
            state: TerminalState {
                size,
                cursor: TerminalCursor {
                    position: TerminalPosition {
                        row: cursor_row,
                        column: cursor_column,
                    },
                    visible: !screen.hide_cursor(),
                    style: cursor_style,
                },
                alternate_screen: screen.alternate_screen(),
                scrollback: TerminalScrollback {
                    position: screen.scrollback(),
                    retained_rows: screen.scrollback_filled(),
                },
                modes,
                title: screen.title().to_owned(),
                working_directory: screen.path().map(str::to_owned),
                hyperlinks,
                progress: screen.progress().map(|(state, value)| TerminalProgress {
                    state: match state {
                        1 => TerminalProgressState::Normal,
                        2 => TerminalProgressState::Error,
                        3 => TerminalProgressState::Indeterminate,
                        4 => TerminalProgressState::Warning,
                        _ => TerminalProgressState::Hidden,
                    },
                    value,
                }),
                clipboard_request: screen.clipboard().map(|(selector, payload)| {
                    TerminalClipboardRequest {
                        selector: selector.to_vec(),
                        base64_payload: payload.to_vec(),
                    }
                }),
                bells: TerminalBells {
                    audible: callbacks.audible,
                    visual: callbacks.visual,
                },
                shell_command: screen.shell_command().map(str::to_owned),
            },
            cells,
            wrapped_rows: (0..rows).map(|row| screen.row_wrapped(row)).collect(),
        }
    }
}

impl TerminalBackend for Vt100Backend {
    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn resize(&mut self, size: TerminalSize) {
        self.parser
            .screen_mut()
            .set_size(size.rows.max(1), size.cols.max(1));
    }

    fn cursor_position(&self) -> TerminalPosition {
        let (row, column) = self.parser.screen().cursor_position();
        TerminalPosition { row, column }
    }

    fn modes(&self, keyboard_disambiguate: bool, mouse_alternate_scroll: bool) -> TerminalModes {
        let screen = self.parser.screen();
        TerminalModes {
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_alternate_scroll,
            keyboard_disambiguate,
            mouse_protocol: match screen.mouse_protocol_mode() {
                MouseProtocolMode::None => MouseProtocol::None,
                MouseProtocolMode::Press => MouseProtocol::Press,
                MouseProtocolMode::PressRelease => MouseProtocol::PressRelease,
                MouseProtocolMode::ButtonMotion => MouseProtocol::ButtonMotion,
                MouseProtocolMode::AnyMotion => MouseProtocol::AnyMotion,
            },
            mouse_encoding: match screen.mouse_protocol_encoding() {
                MouseProtocolEncoding::Default => MouseEncoding::Default,
                MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
        }
    }

    fn snapshot(&self, cursor_style: CursorStyle, modes: TerminalModes) -> TerminalSnapshot {
        self.semantic_snapshot(cursor_style, modes)
    }

    fn viewport(
        &mut self,
        scrollback: usize,
        cursor_style: CursorStyle,
        modes: TerminalModes,
    ) -> TerminalSnapshot {
        let original = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(scrollback);
        let snapshot = self.semantic_snapshot(cursor_style, modes);
        self.parser.screen_mut().set_scrollback(original);
        snapshot
    }
}

fn color(color: vt100::Color) -> TerminalColor {
    match color {
        vt100::Color::Default => TerminalColor::Default,
        vt100::Color::Idx(index) => TerminalColor::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => TerminalColor::Rgb([red, green, blue]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_preserves_visible_terminal_semantics() {
        let mut backend = Vt100Backend::new(TerminalSize::new(2, 12), 4);
        backend.process(
            b"\x1b]2;agent title\x07\x1b]7;file://host/tmp/a%20b\x07\
              \x1b[?1049h\x1b]8;;https://example.com\x07\x1b[1;3;38;2;1;2;3m\xE7\x95\x8C\
              \x1b]8;;\x07\x1b[?25l",
        );
        let snapshot = backend.snapshot(CursorStyle::SteadyBar, backend.modes(false, false));

        assert_eq!(snapshot.state.title, "agent title");
        assert_eq!(
            snapshot.state.working_directory.as_deref(),
            Some("/tmp/a b")
        );
        assert!(snapshot.state.alternate_screen);
        assert!(!snapshot.state.cursor.visible);
        assert_eq!(snapshot.state.cursor.style, CursorStyle::SteadyBar);
        assert_eq!(snapshot.cells[0].contents, "界");
        assert!(snapshot.cells[0].wide);
        assert_eq!(snapshot.cells[0].foreground, TerminalColor::Rgb([1, 2, 3]));
        assert!(snapshot.cells[0].attributes.bold);
        assert!(snapshot.cells[0].attributes.italic);
        assert_eq!(snapshot.cells[0].hyperlink, Some(1));
        assert_eq!(snapshot.state.hyperlinks, ["https://example.com"]);
    }

    #[test]
    fn adapter_handles_split_input_and_terminal_modes() {
        let mut backend = Vt100Backend::new(TerminalSize::new(2, 8), 0);
        for chunk in [
            &b"\x1b[?20"[..],
            &b"04h\x1b[?1h\x1b[?1000"[..],
            &b"h\x1b[?1006hpartial"[..],
        ] {
            backend.process(chunk);
        }
        let modes = backend.modes(true, true);
        let snapshot = backend.snapshot(CursorStyle::default(), modes);

        assert!(modes.bracketed_paste);
        assert!(modes.application_cursor);
        assert_eq!(modes.mouse_protocol, MouseProtocol::PressRelease);
        assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
        assert!(modes.keyboard_disambiguate);
        assert!(modes.mouse_alternate_scroll);
        assert!(snapshot.contents().contains("partial"));
    }

    #[test]
    fn adapter_reports_bells_and_osc_metadata() {
        let mut backend = Vt100Backend::new(TerminalSize::new(1, 8), 0);
        backend.process(b"\x07\x1bg\x1b]9;4;2;75\x07\x1b]52;c;Zm9v\x07");
        let snapshot = backend.snapshot(CursorStyle::default(), TerminalModes::default());

        assert_eq!(snapshot.state.bells.audible, 1);
        assert_eq!(snapshot.state.bells.visual, 1);
        assert_eq!(
            snapshot.state.progress,
            Some(TerminalProgress {
                state: TerminalProgressState::Error,
                value: 75,
            })
        );
        assert_eq!(
            snapshot.state.clipboard_request,
            Some(TerminalClipboardRequest {
                selector: b"c".to_vec(),
                base64_payload: b"Zm9v".to_vec(),
            })
        );
    }

    fn fill_history(backend: &mut Vt100Backend, lines: usize) {
        for _ in 0..lines {
            backend.process(b"x\r\n");
        }
    }

    #[test]
    fn byte_budget_retains_more_narrow_rows() {
        let mut narrow = Vt100Backend::new_with_scrollback_bytes(TerminalSize::new(2, 20), 100_000);
        let mut wide = Vt100Backend::new_with_scrollback_bytes(TerminalSize::new(2, 80), 100_000);
        fill_history(&mut narrow, 1_000);
        fill_history(&mut wide, 1_000);

        assert!(
            narrow.parser.screen().scrollback_filled() > wide.parser.screen().scrollback_filled()
        );
        assert!(narrow.parser.screen().scrollback_storage_bytes() <= 100_000);
        assert!(wide.parser.screen().scrollback_storage_bytes() <= 100_000);
    }

    #[test]
    fn active_rows_survive_a_smaller_budget() {
        let mut backend = Vt100Backend::new_with_scrollback_bytes(TerminalSize::new(2, 80), 1);
        fill_history(&mut backend, 20);

        assert_eq!(backend.parser.screen().size(), (2, 80));
        assert_eq!(backend.parser.screen().scrollback_filled(), 0);
        assert!(backend.parser.screen().scrollback_storage_bytes() > 1);
    }

    #[test]
    fn resize_charges_mixed_width_history() {
        let mut backend = Vt100Backend::new_with_scrollback_bytes(TerminalSize::new(2, 20), 20_000);
        fill_history(&mut backend, 10);
        backend.parser.screen_mut().set_scrollback(usize::MAX);
        backend.resize(TerminalSize::new(2, 80));
        fill_history(&mut backend, 1);

        let screen = backend.parser.screen();
        assert!(screen.scrollback_storage_bytes() <= 20_000);
        assert!(screen.scrollback() <= screen.scrollback_filled());
        assert!(screen.scrollback_filled() > 1);
    }

    #[test]
    fn hyperlink_metadata_is_bounded() {
        let mut backend =
            Vt100Backend::new_with_scrollback_bytes(TerminalSize::new(2, 12), 100_000);
        let suffix = "x".repeat(3_900);
        for index in 0..300 {
            backend.process(
                format!("\x1b]8;;https://example.com/{index}/{suffix}\x07x\x1b]8;;\x07").as_bytes(),
            );
        }

        assert!(backend.parser.screen().hyperlink_storage_bytes() <= 1_000_000);
    }

    #[test]
    fn default_budget_serves_narrow_history_beyond_ten_thousand_rows() {
        let mut backend = Vt100Backend::new_with_scrollback_bytes(
            TerminalSize::new(2, 40),
            crate::session::SCROLLBACK_BYTES,
        );
        fill_history(&mut backend, 11_000);

        assert!(backend.parser.screen().scrollback_filled() > 10_000);
        let viewport = backend.viewport(10_500, CursorStyle::default(), TerminalModes::default());
        assert_eq!(viewport.state.scrollback.position, 10_500);
    }
}
