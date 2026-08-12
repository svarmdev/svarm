use serde::{Deserialize, Serialize};
use terminal_colorsaurus::ColorPalette;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalPalette {
    foreground: [u16; 3],
    background: [u16; 3],
}

impl TerminalPalette {
    pub fn detect() -> Option<Self> {
        terminal_colorsaurus::color_palette(Default::default())
            .ok()
            .map(Self::from)
    }

    fn response(self, query: ColorQuery) -> String {
        let (slot, [red, green, blue]) = match query {
            ColorQuery::Foreground => (10, self.foreground),
            ColorQuery::Background => (11, self.background),
        };
        format!("\x1b]{slot};rgb:{red:04x}/{green:04x}/{blue:04x}\x1b\\")
    }
}

impl From<ColorPalette> for TerminalPalette {
    fn from(palette: ColorPalette) -> Self {
        Self {
            foreground: [
                palette.foreground.r,
                palette.foreground.g,
                palette.foreground.b,
            ],
            background: [
                palette.background.r,
                palette.background.g,
                palette.background.b,
            ],
        }
    }
}

/// The cursor an agent asked the terminal to draw, via `CSI Ps SP q` (DECSCUSR).
///
/// The emulator does not model this, and Svarm cannot infer it from the screen, so the sequence is
/// recognized as it passes through and carried alongside the frame instead. [`Self::Default`] is
/// the agent deferring to the terminal, which is what makes the user's own shape and blink setting
/// apply when no agent has expressed a preference.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    #[default]
    Default,
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

impl CursorStyle {
    fn from_parameter(parameter: u16) -> Self {
        match parameter {
            1 => Self::BlinkingBlock,
            2 => Self::SteadyBlock,
            3 => Self::BlinkingUnderline,
            4 => Self::SteadyUnderline,
            5 => Self::BlinkingBar,
            6 => Self::SteadyBar,
            _ => Self::Default,
        }
    }
}

/// A question an agent asked the terminal that only Svarm can answer, because Svarm *is* the
/// terminal the agent is talking to. Left unanswered, the agent waits out its own timeout — on
/// every query — which is why these are worth recognizing rather than ignoring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceQuery {
    /// `CSI 5 n` — is the terminal healthy?
    Status,
    /// `CSI 6 n` — where is the cursor?
    CursorPosition,
    /// `CSI ? 6 n` — where is the cursor, with a page number?
    ExtendedCursorPosition,
    /// `CSI c` — what kind of terminal is this?
    PrimaryAttributes,
}

impl DeviceQuery {
    /// The answer, given where the emulated cursor currently sits (zero-based, as the screen
    /// reports it; the wire form is one-based).
    pub(crate) fn response(self, cursor: (u16, u16)) -> String {
        let (row, column) = (cursor.0 + 1, cursor.1 + 1);
        match self {
            Self::Status => "\x1b[0n".into(),
            Self::CursorPosition => format!("\x1b[{row};{column}R"),
            Self::ExtendedCursorPosition => format!("\x1b[?{row};{column};1R"),
            // A VT100 with an advanced video option: the capability set the emulator actually
            // provides. Claiming more would invite sequences Svarm would then drop.
            Self::PrimaryAttributes => "\x1b[?1;2c".into(),
        }
    }
}

/// A change to the keyboard protocol, from the "kitty keyboard" extension.
///
/// This matters because the legacy encoding has no way to say `Shift+Enter`: it collapses to the
/// same carriage return as a plain `Enter`. An agent that wants to tell them apart enables this
/// protocol, and Svarm has to notice, because Svarm is what encodes the user's keystrokes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyboardProtocol {
    /// `CSI > flags u` — enter a new mode, keeping the previous one to return to.
    Push(u8),
    /// `CSI < count u` — return to a previous mode.
    Pop(u16),
    /// `CSI = flags ; mode u` — change the current mode in place.
    Set(u8, u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Recognized {
    CursorStyle(CursorStyle),
    Query(DeviceQuery),
    KeyboardQuery,
    Keyboard(KeyboardProtocol),
    AlternateScroll(bool),
}

/// Tracks the keyboard mode an agent has asked for, including the stack the protocol maintains.
#[derive(Debug, Default)]
pub struct KeyboardState {
    stack: Vec<u8>,
    flags: u8,
}

impl KeyboardState {
    /// `CSI ... u` reporting is requested by bit 0, "disambiguate escape codes". Without it the
    /// agent expects the legacy encoding and must keep getting it.
    const DISAMBIGUATE: u8 = 0b1;

    pub(crate) fn apply(&mut self, change: KeyboardProtocol) {
        match change {
            KeyboardProtocol::Push(flags) => {
                // Bounded so a misbehaving agent cannot grow this without limit.
                if self.stack.len() < 32 {
                    self.stack.push(self.flags);
                }
                self.flags = flags;
            }
            KeyboardProtocol::Pop(count) => {
                for _ in 0..count.max(1) {
                    self.flags = self.stack.pop().unwrap_or_default();
                }
            }
            // Mode 1 sets, 2 adds, 3 clears; anything else is not a request Svarm understands.
            KeyboardProtocol::Set(flags, mode) => match mode {
                1 => self.flags = flags,
                2 => self.flags |= flags,
                3 => self.flags &= !flags,
                _ => {}
            },
        }
    }

    pub const fn disambiguates(&self) -> bool {
        self.flags & Self::DISAMBIGUATE != 0
    }

    pub const fn flags(&self) -> u8 {
        self.flags
    }
}

/// Recognizes the `CSI` sequences Svarm has to act on itself, because the emulator models neither
/// the cursor's shape nor the queries an agent addresses to its terminal.
///
/// Reads arrive in arbitrary chunks, so this tracks position within a candidate sequence rather
/// than buffering bytes, and reports each match with the offset just past it. Callers that answer
/// a query need that offset: the reply must describe the screen as of the query, not as of
/// whatever the agent wrote later in the same read.
#[derive(Default)]
pub(crate) struct ControlDetector {
    scan: Scan,
}

#[derive(Clone, Copy, Default)]
enum Scan {
    #[default]
    Idle,
    Escape,
    /// Inside `CSI`: the private prefix if one was given, and the parameters seen so far.
    Csi {
        prefix: Option<u8>,
        first: u16,
        second: Option<u16>,
    },
    /// A space intermediate has been seen, so `q` would end a cursor-style request.
    Space(u16),
}

impl Scan {
    const fn csi(prefix: Option<u8>) -> Self {
        Self::Csi {
            prefix,
            first: 0,
            second: None,
        }
    }
}

impl ControlDetector {
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Vec<(usize, Recognized)> {
        let mut recognized = Vec::new();
        for (offset, byte) in bytes.iter().enumerate() {
            let mut report = |item| recognized.push((offset + 1, item));
            self.scan = match (self.scan, byte) {
                (_, 0x1b) => Scan::Escape,
                (Scan::Escape, b'[') => Scan::csi(None),
                (
                    Scan::Csi {
                        prefix: None,
                        first: 0,
                        second: None,
                    },
                    b'?' | b'>' | b'<' | b'=',
                ) => Scan::csi(Some(*byte)),
                (
                    Scan::Csi {
                        prefix,
                        first,
                        second,
                    },
                    b'0'..=b'9',
                ) => {
                    let digit = u16::from(byte - b'0');
                    match second {
                        Some(second) => Scan::Csi {
                            prefix,
                            first,
                            second: Some(second.saturating_mul(10) + digit),
                        },
                        None => Scan::Csi {
                            prefix,
                            first: first.saturating_mul(10) + digit,
                            second: None,
                        },
                    }
                }
                (
                    Scan::Csi {
                        prefix,
                        first,
                        second: None,
                    },
                    b';',
                ) => Scan::Csi {
                    prefix,
                    first,
                    second: Some(0),
                },
                (
                    Scan::Csi {
                        prefix: None,
                        first,
                        second: None,
                    },
                    b' ',
                ) => Scan::Space(first),
                (Scan::Space(parameter), b'q') => {
                    report(Recognized::CursorStyle(CursorStyle::from_parameter(
                        parameter,
                    )));
                    Scan::Idle
                }
                (
                    Scan::Csi {
                        prefix,
                        first,
                        second: None,
                    },
                    b'n',
                ) => {
                    let query = match (prefix, first) {
                        (None, 5) => Some(DeviceQuery::Status),
                        (None, 6) => Some(DeviceQuery::CursorPosition),
                        (Some(b'?'), 6) => Some(DeviceQuery::ExtendedCursorPosition),
                        _ => None,
                    };
                    if let Some(query) = query {
                        report(Recognized::Query(query));
                    }
                    Scan::Idle
                }
                (
                    Scan::Csi {
                        prefix: None,
                        first: 0,
                        second: None,
                    },
                    b'c',
                ) => {
                    report(Recognized::Query(DeviceQuery::PrimaryAttributes));
                    Scan::Idle
                }
                (
                    Scan::Csi {
                        prefix: Some(b'?'),
                        first: 0,
                        second: None,
                    },
                    b'u',
                ) => {
                    report(Recognized::KeyboardQuery);
                    Scan::Idle
                }
                (
                    Scan::Csi {
                        prefix: Some(prefix),
                        first,
                        second,
                    },
                    b'u',
                ) => {
                    let change = match prefix {
                        b'>' => Some(KeyboardProtocol::Push(first as u8)),
                        b'<' => Some(KeyboardProtocol::Pop(first)),
                        // A missing mode means 1, per the protocol.
                        b'=' => Some(KeyboardProtocol::Set(
                            first as u8,
                            second.unwrap_or(1).max(1) as u8,
                        )),
                        _ => None,
                    };
                    if let Some(change) = change {
                        report(Recognized::Keyboard(change));
                    }
                    Scan::Idle
                }
                (
                    Scan::Csi {
                        prefix: Some(b'?'),
                        first: 1007,
                        second: None,
                    },
                    enabled @ (b'h' | b'l'),
                ) => {
                    report(Recognized::AlternateScroll(*enabled == b'h'));
                    Scan::Idle
                }
                _ => Scan::Idle,
            };
        }
        recognized
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorQuery {
    Foreground,
    Background,
}

#[derive(Default)]
pub(crate) struct ColorQueryDetector {
    pending: Vec<u8>,
}

impl ColorQueryDetector {
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Vec<ColorQuery> {
        const QUERIES: [(&[u8], ColorQuery); 4] = [
            (b"\x1b]10;?\x1b\\", ColorQuery::Foreground),
            (b"\x1b]11;?\x1b\\", ColorQuery::Background),
            (b"\x1b]10;?\x07", ColorQuery::Foreground),
            (b"\x1b]11;?\x07", ColorQuery::Background),
        ];

        self.pending.extend_from_slice(bytes);
        let mut detected = Vec::new();
        while let Some((position, pattern, query)) = QUERIES
            .iter()
            .filter_map(|(pattern, query)| {
                self.pending
                    .windows(pattern.len())
                    .position(|window| window == *pattern)
                    .map(|position| (position, *pattern, *query))
            })
            .min_by_key(|(position, _, _)| *position)
        {
            detected.push(query);
            self.pending.drain(..position + pattern.len());
        }

        const MAX_PARTIAL_QUERY_LEN: usize = 7;
        if self.pending.len() > MAX_PARTIAL_QUERY_LEN {
            self.pending
                .drain(..self.pending.len() - MAX_PARTIAL_QUERY_LEN);
        }
        detected
    }
}

pub(crate) fn color_query_responses(
    detector: &mut ColorQueryDetector,
    palette: Option<TerminalPalette>,
    bytes: &[u8],
) -> Vec<String> {
    let Some(palette) = palette else {
        detector.process(bytes);
        return Vec::new();
    };
    detector
        .process(bytes)
        .into_iter()
        .map(|query| palette.response(query))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_formats_osc_color_responses() {
        let palette = TerminalPalette {
            foreground: [0xaaaa, 0xbbbb, 0xcccc],
            background: [0x1111, 0x2222, 0x3333],
        };

        assert_eq!(
            palette.response(ColorQuery::Foreground),
            "\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\"
        );
        assert_eq!(
            palette.response(ColorQuery::Background),
            "\x1b]11;rgb:1111/2222/3333\x1b\\"
        );
    }

    #[test]
    fn detector_reports_exact_queries_split_across_reads() {
        let mut detector = ColorQueryDetector::default();

        assert!(detector.process(b"before\x1b]10;?\x1b").is_empty());
        assert_eq!(
            detector.process(b"\\middle\x1b]11;?\x07after"),
            [ColorQuery::Foreground, ColorQuery::Background]
        );
    }

    fn styles(detector: &mut ControlDetector, bytes: &[u8]) -> Vec<CursorStyle> {
        detector
            .process(bytes)
            .into_iter()
            .filter_map(|(_, recognized)| match recognized {
                Recognized::CursorStyle(style) => Some(style),
                _ => None,
            })
            .collect()
    }

    fn queries(detector: &mut ControlDetector, bytes: &[u8]) -> Vec<DeviceQuery> {
        detector
            .process(bytes)
            .into_iter()
            .filter_map(|(_, recognized)| match recognized {
                Recognized::Query(query) => Some(query),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cursor_styles_are_recognized_across_reads_and_default_when_unset() {
        let mut detector = ControlDetector::default();

        assert!(styles(&mut detector, b"text without a request").is_empty());
        assert_eq!(
            styles(&mut detector, b"\x1b[5 q"),
            [CursorStyle::BlinkingBar]
        );
        assert!(
            styles(&mut detector, b"\x1b[2").is_empty(),
            "sequence is incomplete"
        );
        assert_eq!(styles(&mut detector, b" q"), [CursorStyle::SteadyBlock]);
        assert_eq!(
            styles(&mut detector, b"\x1b[0 q"),
            [CursorStyle::Default],
            "zero returns the cursor to the terminal's own preference"
        );
    }

    #[test]
    fn cursor_detection_reports_every_request_and_ignores_lookalikes() {
        let mut detector = ControlDetector::default();

        assert_eq!(
            styles(&mut detector, b"\x1b[1 q\x1b[38;5;9mred\x1b[4 q"),
            [CursorStyle::BlinkingBlock, CursorStyle::SteadyUnderline]
        );
        assert_eq!(
            styles(&mut detector, b"\x1b[?25h\x1b[q\x1b[ q"),
            [CursorStyle::Default],
            "an empty parameter is DECSCUSR 0, while other sequences are not cursor requests"
        );
        assert!(styles(&mut detector, b"\x1b[2J\x1b[Hq").is_empty());
    }

    #[test]
    fn device_queries_are_recognized_and_answered_from_the_emulated_screen() {
        let mut detector = ControlDetector::default();

        assert_eq!(
            queries(&mut detector, b"\x1b[6n"),
            [DeviceQuery::CursorPosition]
        );
        assert_eq!(
            queries(&mut detector, b"\x1b[?6n"),
            [DeviceQuery::ExtendedCursorPosition]
        );
        assert_eq!(queries(&mut detector, b"\x1b[5n"), [DeviceQuery::Status]);
        assert_eq!(
            queries(&mut detector, b"\x1b[c\x1b[0c"),
            [
                DeviceQuery::PrimaryAttributes,
                DeviceQuery::PrimaryAttributes
            ]
        );
        assert!(
            queries(&mut detector, b"\x1b[>c\x1b[3n\x1b[2J").is_empty(),
            "only the queries Svarm can answer from its own state are claimed"
        );

        // Reports are one-based, so a cursor at the origin is row 1, column 1.
        assert_eq!(DeviceQuery::CursorPosition.response((0, 0)), "\x1b[1;1R");
        assert_eq!(DeviceQuery::CursorPosition.response((4, 9)), "\x1b[5;10R");
        assert_eq!(
            DeviceQuery::ExtendedCursorPosition.response((4, 9)),
            "\x1b[?5;10;1R"
        );
        assert_eq!(DeviceQuery::Status.response((0, 0)), "\x1b[0n");
        assert_eq!(
            DeviceQuery::PrimaryAttributes.response((0, 0)),
            "\x1b[?1;2c"
        );
    }

    #[test]
    fn keyboard_protocol_requests_are_tracked_through_push_set_and_pop() {
        let mut detector = ControlDetector::default();
        let mut keyboard = KeyboardState::default();
        assert!(!keyboard.disambiguates(), "legacy encoding until asked");

        let changes = |detector: &mut ControlDetector, bytes: &[u8]| {
            detector
                .process(bytes)
                .into_iter()
                .filter_map(|(_, recognized)| match recognized {
                    Recognized::Keyboard(change) => Some(change),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            changes(&mut detector, b"\x1b[>1u"),
            [KeyboardProtocol::Push(1)]
        );
        keyboard.apply(KeyboardProtocol::Push(1));
        assert!(keyboard.disambiguates());

        assert_eq!(
            changes(&mut detector, b"\x1b[=1;3u"),
            [KeyboardProtocol::Set(1, 3)],
            "mode 3 clears the named flags"
        );
        keyboard.apply(KeyboardProtocol::Set(1, 3));
        assert!(!keyboard.disambiguates());

        keyboard.apply(KeyboardProtocol::Set(1, 2));
        assert!(keyboard.disambiguates(), "mode 2 adds the named flags");

        assert_eq!(
            changes(&mut detector, b"\x1b[<1u"),
            [KeyboardProtocol::Pop(1)]
        );
        keyboard.apply(KeyboardProtocol::Pop(1));
        assert!(
            !keyboard.disambiguates(),
            "popping restores what was pushed over"
        );
    }

    #[test]
    fn popping_past_the_bottom_of_the_stack_returns_to_the_legacy_encoding() {
        let mut keyboard = KeyboardState::default();
        keyboard.apply(KeyboardProtocol::Push(1));
        keyboard.apply(KeyboardProtocol::Pop(9));

        assert!(!keyboard.disambiguates());
    }

    #[test]
    fn recognized_offsets_point_just_past_the_sequence() {
        let mut detector = ControlDetector::default();
        let output = b"ab\x1b[6ncd";

        assert_eq!(
            detector.process(output),
            [(6, Recognized::Query(DeviceQuery::CursorPosition))]
        );
        assert_eq!(&output[..6], b"ab\x1b[6n");
    }

    #[test]
    fn alternate_scroll_mode_is_recognized_across_reads() {
        let mut detector = ControlDetector::default();

        assert!(detector.process(b"\x1b[?100").is_empty());
        assert_eq!(
            detector.process(b"7h\x1b[?1007l"),
            [
                (2, Recognized::AlternateScroll(true)),
                (10, Recognized::AlternateScroll(false)),
            ]
        );
    }

    #[test]
    fn kitty_keyboard_query_is_recognized_across_reads() {
        let mut detector = ControlDetector::default();

        assert!(detector.process(b"\x1b[?").is_empty());
        assert_eq!(detector.process(b"u"), [(1, Recognized::KeyboardQuery)]);
    }

    #[test]
    fn detector_does_not_infer_queries_from_partial_evidence() {
        let mut detector = ColorQueryDetector::default();

        assert!(detector.process(b"\x1b]10;?").is_empty());
        assert!(detector.process(b"not-a-terminator").is_empty());
    }
}
