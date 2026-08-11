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

/// Recognizes `CSI Ps SP q` in an agent's output. Reads arrive in arbitrary chunks, so this keeps
/// only the position within a candidate sequence rather than buffering bytes.
#[derive(Default)]
pub(crate) struct CursorStyleDetector {
    scan: Scan,
}

#[derive(Clone, Copy, Default)]
enum Scan {
    #[default]
    Idle,
    Escape,
    Parameter(u16),
    Intermediate(u16),
}

impl CursorStyleDetector {
    /// Returns the last style requested in `bytes`, if any.
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Option<CursorStyle> {
        let mut style = None;
        for byte in bytes {
            self.scan = match (self.scan, byte) {
                (_, 0x1b) => Scan::Escape,
                (Scan::Escape, b'[') => Scan::Parameter(0),
                (Scan::Parameter(value), b'0'..=b'9') => {
                    Scan::Parameter(value.saturating_mul(10) + u16::from(byte - b'0'))
                }
                (Scan::Parameter(value), b' ') => Scan::Intermediate(value),
                (Scan::Intermediate(value), b'q') => {
                    style = Some(CursorStyle::from_parameter(value));
                    Scan::Idle
                }
                _ => Scan::Idle,
            };
        }
        style
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

    #[test]
    fn cursor_styles_are_recognized_across_reads_and_default_when_unset() {
        let mut detector = CursorStyleDetector::default();

        assert_eq!(detector.process(b"text without a request"), None);
        assert_eq!(
            detector.process(b"\x1b[5 q"),
            Some(CursorStyle::BlinkingBar)
        );
        assert_eq!(detector.process(b"\x1b[2"), None, "sequence is incomplete");
        assert_eq!(detector.process(b" q"), Some(CursorStyle::SteadyBlock));
        assert_eq!(
            detector.process(b"\x1b[0 q"),
            Some(CursorStyle::Default),
            "zero returns the cursor to the terminal's own preference"
        );
    }

    #[test]
    fn cursor_detection_reports_the_last_request_and_ignores_lookalikes() {
        let mut detector = CursorStyleDetector::default();

        assert_eq!(
            detector.process(b"\x1b[1 q\x1b[38;5;9mred\x1b[4 q"),
            Some(CursorStyle::SteadyUnderline),
            "later requests supersede earlier ones within a single read"
        );
        assert_eq!(
            detector.process(b"\x1b[?25h\x1b[6n\x1b[q\x1b[ q"),
            Some(CursorStyle::Default),
            "an empty parameter is DECSCUSR 0, while other sequences are not cursor requests"
        );
        assert_eq!(detector.process(b"\x1b[2J\x1b[Hq"), None);
    }

    #[test]
    fn detector_does_not_infer_queries_from_partial_evidence() {
        let mut detector = ColorQueryDetector::default();

        assert!(detector.process(b"\x1b]10;?").is_empty());
        assert!(detector.process(b"not-a-terminator").is_empty());
    }
}
